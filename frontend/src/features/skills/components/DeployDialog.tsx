import { useEffect, useState } from 'react'
import { useTranslation } from 'react-i18next'

import { FileCode2, Rocket, ShieldCheck, RotateCcw, ArrowRightLeft, Loader2, X } from 'lucide-react'
import { useSkillFields } from '../../../store/skillStore'
import type { SkillDriftFile, SkillItem, SkillSyncPreview } from '../../../types/skill'
import { SOURCES } from '../sources'
import { SkillDiffPane } from './SkillDiffPane'

/** 部署体检对话框：一屏展示每个 Agent 收到的 Skill 副本状态，支持按 Agent 一键修复。
 *  发布/调整装备已改为即时同步，这里定位为手动核对与补救入口。 */
export function DeployDialog({ onClose, onNotice }: {
  onClose: () => void
  onNotice: (message: string) => void
}) {
  const { t } = useTranslation()
  const { skills, previewSkillSync, applySkillSync } = useSkillFields('skills', 'previewSkillSync', 'applySkillSync')
  const [previews, setPreviews] = useState<Record<string, SkillSyncPreview | 'error'>>({})
  const [busyAgent, setBusyAgent] = useState<string | null>(null)
  const [checking, setChecking] = useState(true)
  // 钻取查看某个「待更新」Skill 的本地更改 diff。
  const [drift, setDrift] = useState<{ target: string, skill: SkillItem } | null>(null)

  const refreshAgent = async (agent: string) => {
    try {
      const result = await previewSkillSync(agent)
      setPreviews((prev) => ({ ...prev, [agent]: result }))
    } catch {
      setPreviews((prev) => ({ ...prev, [agent]: 'error' }))
    }
  }

  useEffect(() => {
    let mounted = true
    setChecking(true)
    Promise.all(SOURCES.map(({ id }) => previewSkillSync(id).then(
      (result) => [id, result] as const,
      () => [id, 'error'] as const,
    ))).then((entries) => {
      if (mounted) {
        setPreviews(Object.fromEntries(entries))
        setChecking(false)
      }
    })
    return () => { mounted = false }
  }, [previewSkillSync, skills])

  async function redeploy(agent: string, label: string) {
    setBusyAgent(agent)
    try {
      const result = await applySkillSync(agent, true)
      const created = result.create.length + result.missing.length
      const updated = result.update.length + result.conflict.length
      const deployed = created + updated
      onNotice(t('skills.deployDone', { label, created, updated }))
      if (deployed > 0) await refreshAgent(agent)
    } catch (error) {
      onNotice(t('skills.deployFailed', { label, error: String(error) }))
    } finally {
      setBusyAgent(null)
    }
  }

  return (
    <div className="fixed inset-0 z-40 flex items-center justify-center p-4" role="dialog" aria-modal="true" aria-label={t('skills.deployDialogAria')}>
      <button aria-label={t('skills.dialogCancel')} onClick={onClose} className="absolute inset-0 cursor-default bg-gray-950/40 backdrop-blur-[2px]" />
      <div className="fluent-dialog-panel relative flex max-h-[85vh] w-full max-w-lg flex-col overflow-hidden rounded-2xl border border-gray-200 bg-white shadow-2xl dark:border-gray-700 dark:bg-gray-900">
        {/* header */}
        <div className="flex items-center gap-2 border-b border-gray-100 px-5 py-4 dark:border-gray-800">
          <div className="rounded-lg bg-violet-500/10 p-1.5 text-violet-600 dark:text-violet-400">
            <Rocket size={16} />
          </div>
          <div className="min-w-0">
            <h2 className="text-sm font-semibold text-gray-800 dark:text-gray-100">{t('skills.deployDialogTitle')}</h2>
            <p className="text-[11px] text-gray-500 dark:text-gray-400">{t('skills.deployDialogHint')}</p>
          </div>
          <button onClick={onClose} aria-label={t('skills.dialogCancel')} className="ml-auto shrink-0 rounded-md p-1.5 text-gray-500 transition hover:bg-gray-100 hover:text-gray-600 dark:hover:bg-gray-800 dark:hover:text-gray-300">
            <X size={15} />
          </button>
        </div>

        {/* agent cards */}
        <div className="min-h-0 flex-1 space-y-2.5 overflow-y-auto p-5">
          {checking && (
            <div className="flex items-center justify-center gap-2 py-8 text-xs text-gray-500">
              <Loader2 size={14} className="animate-spin motion-reduce:animate-none" />
              {t('skills.deployChecking')}
            </div>
          )}
          {!checking && SOURCES.map((agent) => {
            const preview = previews[agent.id]
            const equipped = skills.filter((s) => s.status === 'published' && s.assigned_agents.includes(agent.id) && s.source !== agent.id).length
            if (preview === undefined) return null
            if (preview === 'error') {
              return (
                <div key={agent.id} className="flex items-center gap-3 rounded-xl border border-gray-200 p-3 dark:border-gray-700">
                  <span className={`h-2 w-2 shrink-0 rounded-full ${agent.color}`} />
                  <span className="text-xs font-medium text-gray-700 dark:text-gray-200">{agent.label}</span>
                  <span className="ml-auto text-xs text-rose-500">{t('skills.deployReadFailed')}</span>
                </div>
              )
            }
            const pending = preview.create.length + preview.update.length + preview.conflict.length + preview.missing.length
            const clean = pending === 0
            return (
              <div key={agent.id} className="rounded-xl border border-gray-200 p-3 dark:border-gray-700">
                <div className="flex items-center gap-2">
                  <span className={`h-2 w-2 shrink-0 rounded-full ${agent.color}`} />
                  <span className="text-xs font-semibold text-gray-800 dark:text-gray-100">{agent.label}</span>
                  <span className="text-[11px] text-gray-500 dark:text-gray-400">{t('skills.deployEquipped', { count: equipped })}</span>
                  {clean && (
                    <span className="ml-auto inline-flex items-center gap-1 text-[11px] font-medium text-emerald-600 dark:text-emerald-400">
                      <ShieldCheck size={11} /> {t('skills.deployUpToDate')}
                    </span>
                  )}
                </div>
                <div className="mt-2 flex flex-wrap items-center gap-1.5">
                  {preview.create.length > 0 && <DeployBadge label={t('skills.badgeToCreate')} count={preview.create.length} tone="emerald" />}
                  {preview.update.length > 0 && <DeployBadge label={t('skills.badgeToUpdate')} count={preview.update.length} tone="amber" />}
                  {preview.conflict.length > 0 && <DeployBadge label={t('skills.badgeConflict')} count={preview.conflict.length} tone="rose" />}
                  {preview.missing.length > 0 && <DeployBadge label={t('skills.badgeMissing')} count={preview.missing.length} tone="rose" />}
                  {preview.unchanged.length > 0 && <DeployBadge label={t('skills.badgeUnchanged')} count={preview.unchanged.length} tone="gray" />}
                  <button
                    onClick={() => { void redeploy(agent.id, agent.label) }}
                    disabled={busyAgent === agent.id || clean}
                    className="ml-auto inline-flex items-center gap-1.5 rounded-lg bg-violet-600 px-3 py-1.5 text-xs font-medium text-white transition hover:bg-violet-500 active:scale-[0.98] disabled:opacity-40"
                  >
                    {busyAgent === agent.id ? <Loader2 size={12} className="animate-spin motion-reduce:animate-none" /> : <Rocket size={12} />}
                    {t('skills.deployAction')}
                  </button>
                </div>
                {preview.update.length > 0 && (
                  <div className="mt-1.5 space-y-0.5 border-t border-gray-100 pt-1.5 dark:border-gray-800">
                    {preview.update.map(skill => (
                      <button
                        key={`${skill.source}:${skill.name}`}
                        onClick={() => setDrift({ target: agent.id, skill })}
                        className="flex w-full items-center gap-1.5 rounded-md px-2 py-1 text-left text-xs text-gray-600 transition hover:bg-gray-50 dark:text-gray-300 dark:hover:bg-gray-800"
                      >
                        <FileCode2 size={11} className="shrink-0 text-amber-500" />
                        <span className="truncate">{skill.name}</span>
                        <span className="ml-auto shrink-0 text-[10px] font-medium text-violet-600 dark:text-violet-400">{t('skills.driftView')} →</span>
                      </button>
                    ))}
                  </div>
                )}
              </div>
            )
          })}
        </div>
      </div>
      {drift && (
        <DriftDialog
          target={drift.target}
          skill={drift.skill}
          onClose={() => setDrift(null)}
          onDone={() => { setDrift(null); void refreshAgent(drift.target) }}
          onNotice={onNotice}
        />
      )}
    </div>
  )
}

function DeployBadge({ label, count, tone }: { label: string; count: number; tone: 'emerald' | 'amber' | 'rose' | 'gray' }) {
  const tones: Record<string, string> = {
    emerald: 'bg-emerald-500/10 text-emerald-700 dark:text-emerald-400',
    amber:   'bg-amber-500/10 text-amber-700 dark:text-amber-400',
    rose:    'bg-rose-500/10 text-rose-700 dark:text-rose-400',
    gray:    'bg-gray-100 text-gray-500 dark:bg-gray-800 dark:text-gray-400',
  }
  return (
    <span className={`inline-flex items-center gap-1 rounded-full px-2 py-0.5 text-[11px] font-medium ${tones[tone]}`}>
      {label} <span className="font-mono font-semibold tabular-nums">{count}</span>
    </span>
  )
}

/** 单个「待更新」Skill 的本地更改对比：文件级列表 + 行级 diff，
 *  操作为「用共享库覆盖」（丢弃本地改动）或「采纳本地版本」（回写共享库
 *  并同步到其他已装备 Agent）。 */
function DriftDialog({ target, skill, onClose, onDone, onNotice }: {
  target: string
  skill: SkillItem
  onClose: () => void
  onDone: () => void
  onNotice: (message: string) => void
}) {
  const { t } = useTranslation()
  const { skillDriftDetail, adoptLocalSkill, applySkillSyncOne } = useSkillFields('skillDriftDetail', 'adoptLocalSkill', 'applySkillSyncOne')
  const [files, setFiles] = useState<SkillDriftFile[] | null>(null)
  const [selected, setSelected] = useState(0)
  const [error, setError] = useState<string | null>(null)
  const [busy, setBusy] = useState(false)

  useEffect(() => {
    let mounted = true
    skillDriftDetail(target, skill.source, skill.name)
      .then((result) => { if (mounted) { setFiles(result); setSelected(0) } })
      .catch((e) => { if (mounted) setError(String(e)) })
    return () => { mounted = false }
  }, [target, skill.source, skill.name, skillDriftDetail])

  const agentLabel = SOURCES.find(s => s.id === target)?.label ?? target

  async function overwriteWithShared() {
    if (!window.confirm(t('skills.driftOverwriteConfirm', { name: skill.name, agent: agentLabel }))) return
    setBusy(true)
    try {
      await applySkillSyncOne(target, skill.source, skill.name)
      onDone()
    } catch (e) {
      setError(String(e))
      setBusy(false)
    }
  }

  async function adoptLocal() {
    if (!window.confirm(t('skills.driftAdoptConfirm', { name: skill.name, agent: agentLabel }))) return
    setBusy(true)
    try {
      const result = await adoptLocalSkill(target, skill.source, skill.name)
      onNotice(t('skills.driftAdoptDone', {
        name: skill.name,
        synced: result.synced.length,
        skipped: result.skipped.length,
      }))
      onDone()
    } catch (e) {
      setError(String(e))
      setBusy(false)
    }
  }

  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center p-4" role="dialog" aria-modal="true" aria-label={t('skills.driftTitle')}>
      <button aria-label={t('skills.dialogCancel')} onClick={onClose} className="absolute inset-0 cursor-default bg-gray-950/50 backdrop-blur-[2px]" />
      <div className="fluent-dialog-panel relative flex max-h-[85vh] w-full max-w-2xl flex-col overflow-hidden rounded-2xl border border-gray-200 bg-white shadow-2xl dark:border-gray-700 dark:bg-gray-900">
        <div className="flex items-center gap-2 border-b border-gray-100 px-5 py-4 dark:border-gray-800">
          <div className="rounded-lg bg-amber-500/10 p-1.5 text-amber-600 dark:text-amber-400">
            <FileCode2 size={16} />
          </div>
          <div className="min-w-0">
            <h2 className="truncate text-sm font-semibold text-gray-800 dark:text-gray-100">{skill.name}</h2>
            <p className="text-[11px] text-gray-500 dark:text-gray-400">{t('skills.driftSubtitle', { agent: agentLabel })}</p>
          </div>
          <button onClick={onClose} aria-label={t('skills.dialogCancel')} className="ml-auto shrink-0 rounded-md p-1.5 text-gray-500 transition hover:bg-gray-100 hover:text-gray-600 dark:hover:bg-gray-800 dark:hover:text-gray-300">
            <X size={15} />
          </button>
        </div>

        <div className="min-h-0 flex-1 overflow-y-auto p-5">
          {error && <p className="text-xs text-rose-500">{t('skills.driftLoadFailed', { error })}</p>}
          {!files && !error && (
            <div className="flex items-center justify-center gap-2 py-8 text-xs text-gray-500">
              <Loader2 size={14} className="animate-spin motion-reduce:animate-none" />
              {t('skills.deployChecking')}
            </div>
          )}
          {files && files.length === 0 && (
            <p className="py-8 text-center text-xs text-gray-500">{t('skills.driftEmpty')}</p>
          )}
          {files && files.length > 0 && (
            <SkillDiffPane files={files} selected={selected} onSelect={setSelected} />
          )}
        </div>

        <div className="border-t border-gray-100 px-5 py-3 dark:border-gray-800">
          <p className="mb-2 text-[11px] leading-4 text-gray-500 dark:text-gray-400">{t('skills.driftHint', { agent: agentLabel })}</p>
          <div className="flex flex-wrap items-center gap-2">
            <button
              onClick={() => { void overwriteWithShared() }}
              disabled={busy || !files}
              className="inline-flex items-center gap-1.5 rounded-lg border border-gray-300 px-3 py-1.5 text-xs font-medium text-gray-700 transition hover:bg-gray-50 disabled:opacity-40 dark:border-gray-600 dark:text-gray-200 dark:hover:bg-gray-800"
            >
              <RotateCcw size={13} />
              {t('skills.driftOverwrite')}
            </button>
            <button
              onClick={() => { void adoptLocal() }}
              disabled={busy || !files}
              className="inline-flex items-center gap-1.5 rounded-lg bg-violet-600 px-3 py-1.5 text-xs font-medium text-white transition hover:bg-violet-500 disabled:opacity-40"
            >
              {busy ? <Loader2 size={12} className="animate-spin motion-reduce:animate-none" /> : <ArrowRightLeft size={12} />}
              {t('skills.driftAdopt')}
            </button>
          </div>
        </div>
      </div>
    </div>
  )
}
