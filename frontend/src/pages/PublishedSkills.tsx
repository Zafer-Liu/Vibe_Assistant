import { memo, useEffect, useMemo, useState } from 'react'
import { useTranslation } from 'react-i18next'
import {
  ArrowLeft, ArrowRightLeft, ChevronDown, ChevronUp, FileCode2,
  FolderSync, Globe, Loader2, RotateCcw, Scale, Search, ShieldCheck, Sparkles, Trash2, X,
} from 'lucide-react'
import { useSkillFields } from '../store/skillStore'
import type { SkillDriftAgent, SkillDriftFile, SkillItem } from '../types/skill'
import { PublishDialog } from '../features/skills/components/PublishDialog'
import { SkillDiffPane } from '../features/skills/components/SkillDiffPane'
import { SOURCES } from '../features/skills/sources'

const sourceMeta = (id: string) => SOURCES.find(s => s.id === id)

export const PublishedSkills = memo(function PublishedSkills({ onBack, onGoSync }: { onBack: () => void; onGoSync: () => void }) {
  const { t } = useTranslation()
  const { skills, skillCacheReady, loadSkills, readSkill, setSkillStatus, publishSkill, deleteSkill, applySkillSync, publishedDrift } = useSkillFields('skills', 'skillCacheReady', 'loadSkills', 'readSkill', 'setSkillStatus', 'publishSkill', 'deleteSkill', 'applySkillSync', 'publishedDrift')
  const [query, setQuery] = useState('')
  const [agentFilter, setAgentFilter] = useState<'all' | string>('all')
  const [loading, setLoading] = useState(false)
  const [notice, setNotice] = useState<string | null>(null)
  const [booting, setBooting] = useState(() => !skillCacheReady)
  // 展开查看内容：key 为 `${source}:${name}`
  const [expandedKey, setExpandedKey] = useState<string | null>(null)
  const [expandedContent, setExpandedContent] = useState<string | null>(null)
  // 调整装备对话框
  const [adjusting, setAdjusting] = useState<SkillItem | null>(null)
  const [adjustTargets, setAdjustTargets] = useState<Set<string>>(new Set())
  // 各已发布 Skill 的 Agent 漂移状态（版本仲裁数据），key 为 `${source}:${name}`
  const [driftMap, setDriftMap] = useState<Record<string, SkillDriftAgent[]>>({})
  const [arbitrating, setArbitrating] = useState<SkillItem | null>(null)

  useEffect(() => {
    let mounted = true
    void loadSkills().catch(() => setNotice(t('skills.readFailed'))).finally(() => {
      if (mounted) setBooting(false)
    })
    return () => { mounted = false }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [loadSkills])

  useEffect(() => {
    if (!notice) return
    const timer = setTimeout(() => setNotice(null), 5000)
    return () => clearTimeout(timer)
  }, [notice])

  // 漂移总览跟随技能列表变化刷新：任何装载/采纳/覆盖之后 skills 都会重载，随之重新浮现本地修改。
  useEffect(() => {
    let mounted = true
    publishedDrift()
      .then((entries) => {
        if (!mounted) return
        const map: Record<string, SkillDriftAgent[]> = {}
        for (const entry of entries) map[`${entry.source}:${entry.name}`] = entry.agents
        setDriftMap(map)
      })
      .catch(() => { /* 漂移总览失败不阻塞页面，仅无徽标 */ })
    return () => { mounted = false }
  }, [skills, publishedDrift])

  // ── Derived data ──

  const published = useMemo(() => skills.filter((s) => s.status === 'published'), [skills])

  const visible = useMemo(() => published.filter((skill) => {
    const agentMatches = agentFilter === 'all' || skill.assigned_agents.includes(agentFilter)
    const q = query.trim().toLowerCase()
    return agentMatches && (!q || `${skill.name} ${skill.description} ${skill.source}`.toLowerCase().includes(q))
  }), [published, agentFilter, query])

  const countsByAgent = useMemo(() => {
    const map: Record<string, number> = {}
    for (const s of published) for (const a of s.assigned_agents) map[a] = (map[a] || 0) + 1
    return map
  }, [published])

  // ── Actions ──

  async function toggleContent(skill: SkillItem) {
    const key = `${skill.source}:${skill.name}`
    if (expandedKey === key) {
      setExpandedKey(null)
      setExpandedContent(null)
      return
    }
    setLoading(true)
    try {
      const doc = await readSkill(skill.source, skill.name)
      setExpandedKey(key)
      setExpandedContent(doc.content)
    } catch (error) {
      setNotice(t('skills.readSkillFailed', { error: String(error) }))
    } finally {
      setLoading(false)
    }
  }

  function openAdjust(skill: SkillItem) {
    setAdjusting(skill)
    setAdjustTargets(new Set(skill.assigned_agents))
  }

  async function confirmAdjust() {
    if (!adjusting) return
    setLoading(true)
    try {
      await publishSkill(adjusting.source, adjusting.name, [...adjustTargets])
      await loadSkills()
      // 调整装备即推送：直接同步到所有勾选的 Agent（新增 + 覆盖更新）。
      const parts: string[] = []
      for (const agent of [...adjustTargets].filter(a => a !== adjusting.source)) {
        try {
          const result = await applySkillSync(agent, true)
          parts.push(t('skills.syncAgentSummary', { agent, created: result.create.length + result.missing.length, updated: result.update.length + result.conflict.length }))
        } catch (error) {
          parts.push(t('skills.syncAgentFailed', { agent, error: String(error) }))
        }
      }
      setNotice(parts.length ? t('skills.adjustDone', { summary: parts.join(t('skills.adjustPartsJoiner')) }) : t('skills.adjustDoneNoSync'))
      setAdjusting(null)
    } catch (error) {
      setNotice(t('skills.adjustFailed', { error: String(error) }))
    } finally {
      setLoading(false)
    }
  }

  async function toDraft(skill: SkillItem) {
    setLoading(true)
    try {
      await setSkillStatus(skill.source, skill.name, 'draft')
      await loadSkills()
      setNotice(t('skills.toDraftDone', { name: skill.name }))
    } catch (error) {
      setNotice(t('skills.statusUpdateFailed', { error: String(error) }))
    } finally {
      setLoading(false)
    }
  }

  // 删除前二次确认：整个 Skill 目录（含附属脚本与历史快照）都会从共享库移除。
  const [deleting, setDeleting] = useState<SkillItem | null>(null)

  async function confirmDelete() {
    if (!deleting) return
    setLoading(true)
    try {
      await deleteSkill(deleting.source, deleting.name)
      await loadSkills(true)
      if (expandedKey === `${deleting.source}:${deleting.name}`) {
        setExpandedKey(null)
        setExpandedContent(null)
      }
      setNotice(t('skills.deleteDone', { name: deleting.name }))
      setDeleting(null)
    } catch (error) {
      setNotice(t('skills.deleteFailed', { error: String(error) }))
    } finally {
      setLoading(false)
    }
  }

  // ── Render ──

  if (booting) {
    return (
      <div className="flex h-full min-h-0 items-center justify-center bg-gray-50 px-6 dark:bg-gray-950" role="status" aria-live="polite">
        <div className="flex items-center gap-3 text-gray-800 dark:text-gray-100">
          <span className="rounded-xl bg-violet-500/10 p-2.5 text-violet-600 dark:text-violet-400">
            <Loader2 className="animate-spin motion-reduce:animate-none" size={21} />
          </span>
          <p className="text-sm font-semibold">{t('skills.publishedBooting')}</p>
        </div>
      </div>
    )
  }

  return (
    <div className="flex h-full min-h-0 flex-col bg-gray-50 dark:bg-gray-950">
      {/* ── Header ── */}
      <header className="fluent-page-header fluent-command-header flex items-center gap-4 border-b border-gray-200 bg-white px-6 py-4 dark:border-gray-800 dark:bg-gray-900">
        <button
          onClick={onBack}
          className="rounded-lg border border-gray-300 p-2 text-gray-500 transition hover:bg-gray-50 hover:text-gray-700 dark:border-gray-600 dark:text-gray-400 dark:hover:bg-gray-800 dark:hover:text-gray-200"
          aria-label={t('skills.backToLibrary')}
          title={t('skills.backToLibrary')}
        >
          <ArrowLeft size={15} />
        </button>
        <div className="flex min-w-0 items-center gap-3">
          <div className="rounded-xl bg-emerald-500/10 p-2.5 text-emerald-600 dark:text-emerald-400">
            <ShieldCheck size={22} />
          </div>
          <div className="min-w-0">
            <h1 className="text-base font-semibold text-gray-900 dark:text-gray-100">
              {t('skills.publishedTitle')}
              <span className="ml-2 rounded-full bg-emerald-500/10 px-2 py-0.5 align-middle font-mono text-xs font-semibold text-emerald-700 dark:text-emerald-300">{published.length}</span>
            </h1>
            <p className="mt-0.5 hidden text-xs text-gray-500 dark:text-gray-400 sm:block">{t('skills.publishedSubtitle')}</p>
          </div>
        </div>

        <div className="ml-auto">
          <button
            onClick={onGoSync}
            className="inline-flex items-center gap-2 rounded-lg bg-violet-600 px-4 py-2 text-xs font-medium text-white transition-all hover:bg-violet-500 active:scale-[0.97]"
          >
            <ArrowRightLeft size={14} />
            {t('skills.goSync')}
          </button>
        </div>
      </header>

      {/* ── Toolbar: search + equipped-agent filters ── */}
      <div className="flex flex-wrap items-center gap-3 border-b border-gray-200 bg-white px-6 py-3 dark:border-gray-800 dark:bg-gray-900">
        <label className="relative block w-full sm:w-64">
          <Search size={14} className="pointer-events-none absolute left-2.5 top-2.5 text-gray-500" />
          <input
            value={query}
            onChange={(e) => setQuery(e.target.value)}
            placeholder={t('skills.search')}
            className="w-full rounded-lg border border-gray-300 bg-white py-1.5 pl-8 pr-3 text-xs text-gray-800 outline-none transition focus:border-violet-400 focus:ring-2 focus:ring-violet-400/20 dark:border-gray-600 dark:bg-gray-800 dark:text-gray-100 dark:focus:border-violet-500"
          />
        </label>
        <div className="flex flex-wrap items-center gap-1.5">
          <span className="text-xs text-gray-500 dark:text-gray-400">{t('skills.equippedTo')}:</span>
          <FilterPill active={agentFilter === 'all'} onClick={() => setAgentFilter('all')}>
            {t('skills.all')} <span className="ml-1 font-mono opacity-70">{published.length}</span>
          </FilterPill>
          {SOURCES.map(s => (
            <FilterPill key={s.id} active={agentFilter === s.id} onClick={() => setAgentFilter(s.id)}>
              <span className={`mr-1 inline-block h-1.5 w-1.5 rounded-full ${s.color}`} />
              {s.label}
              <span className="ml-1 font-mono opacity-70">{countsByAgent[s.id] ?? 0}</span>
            </FilterPill>
          ))}
        </div>
      </div>

      {/* ── Content ── */}
      <div className="min-h-0 flex-1 overflow-y-auto">
        {visible.length > 0 ? (
          <div className="mx-auto max-w-4xl space-y-3 p-5">
            <p className="text-xs leading-5 text-gray-500 dark:text-gray-400">{t('skills.syncAfterChange')}</p>
            {visible.map((skill) => {
              const key = `${skill.source}:${skill.name}`
              const expanded = expandedKey === key
              const meta = sourceMeta(skill.source)
              const driftAgents = driftMap[key]
              const modifiedCount = driftAgents?.filter(a => a.state === 'modified').length ?? 0
              const missingCount = driftAgents?.filter(a => a.state === 'missing').length ?? 0
              const needsArbitration = modifiedCount + missingCount > 0
              return (
                <div key={key} className="rounded-2xl border border-gray-200 bg-white p-4 dark:border-gray-700 dark:bg-gray-900">
                  <div className="flex items-start gap-3">
                    <div className="rounded-lg bg-violet-500/10 p-2 text-violet-600 dark:text-violet-400">
                      <FileCode2 size={16} />
                    </div>
                    <div className="min-w-0 flex-1">
                      <div className="flex flex-wrap items-center gap-2">
                        <h2 className="truncate text-sm font-semibold text-gray-900 dark:text-gray-100">{skill.name}</h2>
                        <span className="rounded-full bg-emerald-500/10 px-2 py-0.5 text-xs font-medium text-emerald-700 dark:text-emerald-300">v{skill.version}</span>
                        <span className="inline-flex items-center gap-1 rounded-full bg-gray-100 px-2 py-0.5 text-xs font-medium text-gray-600 dark:bg-gray-800 dark:text-gray-300">
                          <Globe size={10} />
                          {meta?.label ?? skill.source}
                        </span>
                      </div>
                      <p className="mt-1 line-clamp-2 text-xs text-gray-500 dark:text-gray-400">
                        {skill.description || t('skills.noDescription')}
                      </p>
                      <div className="mt-2 flex flex-wrap items-center gap-1.5">
                        {skill.assigned_agents.length > 0 ? (
                          skill.assigned_agents.map((id) => {
                            const agent = sourceMeta(id)
                            return (
                              <span key={id} className="inline-flex items-center gap-1.5 rounded-full border border-violet-500/60 bg-violet-50 px-2 py-0.5 text-xs font-medium text-violet-700 dark:bg-violet-500/15 dark:text-violet-200">
                                <span className={`h-1.5 w-1.5 rounded-full ${agent?.color ?? 'bg-gray-400'}`} />
                                {agent?.label ?? id}
                              </span>
                            )
                          })
                        ) : (
                          <span className="text-xs text-amber-600 dark:text-amber-400">{t('skills.notEquipped')} · {t('skills.notEquippedSyncHint')}</span>
                        )}
                        {skill.files && skill.files.length > 1 && (
                          <span className="inline-flex items-center gap-1 rounded-full bg-gray-100 px-2 py-0.5 text-xs font-medium text-gray-500 dark:bg-gray-800 dark:text-gray-400" title={skill.files.join('\n')}>
                            <FolderSync size={10} />
                            {t('skills.bundledFiles', { count: skill.files.length })}
                          </span>
                        )}
                        {modifiedCount > 0 && (
                          <span className="inline-flex items-center gap-1 rounded-full bg-amber-500/10 px-2 py-0.5 text-xs font-semibold text-amber-700 dark:text-amber-300">
                            <Scale size={10} />
                            {t('skills.arbitrateBadge', { count: modifiedCount })}
                          </span>
                        )}
                        {missingCount > 0 && (
                          <span className="inline-flex items-center gap-1 rounded-full bg-rose-500/10 px-2 py-0.5 text-xs font-semibold text-rose-700 dark:text-rose-300">
                            <ShieldCheck size={10} />
                            {t('skills.arbitrateBadgeMissing', { count: missingCount })}
                          </span>
                        )}
                      </div>
                    </div>
                  </div>

                  <div className="mt-3 flex flex-wrap items-center gap-2 border-t border-gray-100 pt-3 dark:border-gray-800">
                    <button
                      onClick={() => { void toggleContent(skill) }}
                      disabled={loading}
                      className="inline-flex items-center gap-1.5 rounded-md border border-gray-300 px-2.5 py-1.5 text-xs font-medium text-gray-700 hover:bg-gray-50 disabled:opacity-50 dark:border-gray-600 dark:text-gray-200 dark:hover:bg-gray-800"
                    >
                      {expanded ? <ChevronUp size={13} /> : <ChevronDown size={13} />}
                      {expanded ? t('skills.hideContent') : t('skills.viewContent')}
                    </button>
                    <button
                      onClick={() => setArbitrating(skill)}
                      disabled={loading}
                      className={`inline-flex items-center gap-1.5 rounded-md px-2.5 py-1.5 text-xs font-medium transition disabled:opacity-50 ${
                        needsArbitration
                          ? 'bg-amber-500 text-white hover:bg-amber-400'
                          : 'border border-gray-300 text-gray-700 hover:bg-gray-50 dark:border-gray-600 dark:text-gray-200 dark:hover:bg-gray-800'
                      }`}
                    >
                      <Scale size={13} />
                      {t('skills.arbitrateButton')}
                    </button>
                    <button
                      onClick={() => openAdjust(skill)}
                      disabled={loading}
                      className="inline-flex items-center gap-1.5 rounded-md bg-violet-600 px-2.5 py-1.5 text-xs font-medium text-white hover:bg-violet-500 disabled:opacity-50"
                    >
                      <ShieldCheck size={13} />
                      {t('skills.adjustEquip')}
                    </button>
                    <button
                      onClick={() => { void toDraft(skill) }}
                      disabled={loading}
                      className="inline-flex items-center gap-1.5 rounded-md border border-gray-300 px-2.5 py-1.5 text-xs font-medium text-gray-500 hover:bg-gray-50 disabled:opacity-50 dark:border-gray-600 dark:text-gray-400 dark:hover:bg-gray-800"
                    >
                      {t('skills.toDraft')}
                    </button>
                    <button
                      onClick={() => setDeleting(skill)}
                      disabled={loading}
                      className="inline-flex items-center gap-1.5 rounded-md border border-red-200 px-2.5 py-1.5 text-xs font-medium text-red-600 hover:bg-red-50 disabled:opacity-50 dark:border-red-900/60 dark:text-red-400 dark:hover:bg-red-900/20"
                    >
                      <Trash2 size={13} />
                      {t('skills.delete')}
                    </button>
                  </div>

                  {expanded && expandedContent !== null && (
                    <div className="mt-3 space-y-2">
                      <pre className="max-h-96 overflow-auto whitespace-pre-wrap rounded-xl bg-gray-50 p-4 font-mono text-xs leading-6 text-gray-700 dark:bg-gray-800/60 dark:text-gray-200">
                        {expandedContent}
                      </pre>
                      {skill.files && skill.files.length > 0 && (
                        <div className="flex flex-wrap gap-1.5 rounded-xl bg-gray-50 p-3 dark:bg-gray-800/60">
                          <span className="w-full pb-1 text-[10px] font-semibold uppercase tracking-wide text-gray-500 dark:text-gray-400">
                            {t('skills.bundledFiles', { count: skill.files.length })} · {t('skills.bundledFilesHint')}
                          </span>
                          {skill.files.map((file) => (
                            <span key={file} className="inline-flex items-center rounded-md bg-white px-2 py-0.5 font-mono text-[11px] text-gray-600 shadow-sm dark:bg-gray-900 dark:text-gray-300">
                              {file}
                            </span>
                          ))}
                        </div>
                      )}
                    </div>
                  )}
                </div>
              )
            })}
          </div>
        ) : (
          <div className="flex h-full flex-col items-center justify-center px-6 text-center">
            <div className="mx-auto mb-3 flex h-14 w-14 items-center justify-center rounded-2xl bg-amber-100 dark:bg-amber-900/30">
              <Sparkles size={24} className="text-amber-600 dark:text-amber-400" />
            </div>
            <p className="text-sm font-medium text-gray-700 dark:text-gray-200">
              {published.length === 0 ? t('skills.publishedEmptyTitle') : t('skills.emptyTitle')}
            </p>
            <p className="mt-1 max-w-sm text-xs leading-5 text-gray-500 dark:text-gray-400">
              {published.length === 0 ? t('skills.publishedEmptyHint') : t('skills.emptyHint')}
            </p>
            {published.length === 0 && (
              <button onClick={onBack} className="mt-4 text-xs font-medium text-violet-600 hover:text-violet-500 dark:text-violet-400">
                {t('skills.backToLibrary')} →
              </button>
            )}
          </div>
        )}
      </div>

      {/* ── Version arbitration modal ── */}
      {arbitrating && (
        <ArbitrateDialog
          skill={arbitrating}
          agents={driftMap[`${arbitrating.source}:${arbitrating.name}`] ?? []}
          onClose={() => setArbitrating(null)}
          onChanged={() => { void loadSkills(true) }}
          onNotice={setNotice}
        />
      )}

      {/* ── Adjust-assignment modal (reuses the publish dialog) ── */}
      {adjusting && (
        <PublishDialog
          skill={{ item: adjusting, content: '' }}
          targets={adjustTargets}
          setTargets={setAdjustTargets}
          loading={loading}
          onConfirm={() => { void confirmAdjust() }}
          onClose={() => setAdjusting(null)}
        />
      )}

      {/* ── Delete confirmation modal ── */}
      {deleting && (
        <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/50 p-4" role="dialog" aria-modal="true">
          <div className="w-full max-w-sm rounded-2xl border border-gray-200 bg-white p-5 dark:border-gray-700 dark:bg-gray-900">
            <div className="flex items-center gap-3">
              <span className="rounded-xl bg-red-500/10 p-2.5 text-red-600 dark:text-red-400">
                <Trash2 size={18} />
              </span>
              <h3 className="text-sm font-semibold text-gray-900 dark:text-gray-100">{t('skills.deleteDialogTitle')}</h3>
            </div>
            <p className="mt-3 text-xs leading-5 text-gray-600 dark:text-gray-300">
              {t('skills.deleteDialogRemove')} <span className="font-mono font-semibold">{deleting.source}:{deleting.name}</span>{t('skills.deleteDialogDir', { files: deleting.files && deleting.files.length > 1 ? t('skills.deleteDialogFiles', { count: deleting.files.length - 1 }) : '' })}
            </p>
            <div className="mt-4 flex justify-end gap-2">
              <button
                onClick={() => setDeleting(null)}
                disabled={loading}
                className="rounded-md border border-gray-300 px-3 py-1.5 text-xs font-medium text-gray-600 hover:bg-gray-50 disabled:opacity-50 dark:border-gray-600 dark:text-gray-300 dark:hover:bg-gray-800"
              >
                {t('skills.dialogCancel')}
              </button>
              <button
                onClick={() => { void confirmDelete() }}
                disabled={loading}
                className="inline-flex items-center gap-1.5 rounded-md bg-red-600 px-3 py-1.5 text-xs font-medium text-white hover:bg-red-500 disabled:opacity-50"
              >
                <Trash2 size={13} />
                {t('skills.confirmDelete')}
              </button>
            </div>
          </div>
        </div>
      )}

      {/* ── Floating toast ── */}
      {notice && (
        <div className="pointer-events-none fixed bottom-5 left-1/2 z-50 -translate-x-1/2" role="status">
          <div className="pointer-events-auto flex max-w-md items-center gap-2 rounded-xl border border-gray-200 bg-white px-4 py-2.5 text-xs text-gray-700 shadow-lg dark:border-gray-700 dark:bg-gray-900 dark:text-gray-200">
            <span className="min-w-0 flex-1">{notice}</span>
            <button onClick={() => setNotice(null)} aria-label={t('skills.closeNoticeAria')} className="shrink-0 rounded-md p-1 text-gray-500 hover:bg-gray-100 hover:text-gray-600 dark:hover:bg-gray-800 dark:hover:text-gray-300">
              <X size={13} />
            </button>
          </div>
        </div>
      )}
    </div>
  )
})

function FilterPill({ active, onClick, children }: { active: boolean; onClick: () => void; children: React.ReactNode }) {
  return (
    <button
      onClick={onClick}
      className={`inline-flex items-center rounded-full px-2.5 py-1 text-xs font-medium transition-all ${
        active
          ? 'bg-violet-600 text-white shadow-sm shadow-violet-500/25'
          : 'bg-gray-100 text-gray-600 hover:bg-gray-200 dark:bg-gray-800 dark:text-gray-300 dark:hover:bg-gray-700'
      }`}
    >
      {children}
    </button>
  )
}

/** 版本仲裁对话框：列出某个已发布 Skill 在全部已装备 Agent 上的本地状态，
 *  由用户决定哪个版本替换为共享库最新（采纳/入库）、用共享库覆盖哪个 Agent、
 *  或为缺失的 Agent 补齐部署；每次操作后状态即时刷新。 */
function ArbitrateDialog({ skill, agents, onClose, onChanged, onNotice }: {
  skill: SkillItem
  agents: SkillDriftAgent[]
  onClose: () => void
  onChanged: () => void
  onNotice: (message: string) => void
}) {
  const { t } = useTranslation()
  const { skillDriftDetail, adoptLocalSkill, applySkillSyncOne, loadSkills } = useSkillFields('skillDriftDetail', 'adoptLocalSkill', 'applySkillSyncOne', 'loadSkills')
  const [rows, setRows] = useState<SkillDriftAgent[]>(agents)
  const [diffAgent, setDiffAgent] = useState<string | null>(null)
  const [diffFiles, setDiffFiles] = useState<SkillDriftFile[] | null>(null)
  const [diffSelected, setDiffSelected] = useState(0)
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState<string | null>(null)

  async function refresh() {
    await loadSkills(true)
    onChanged()
  }

  async function viewDiff(agent: string) {
    if (diffAgent === agent) {
      setDiffAgent(null)
      setDiffFiles(null)
      return
    }
    setDiffAgent(agent)
    setDiffFiles(null)
    try {
      const files = await skillDriftDetail(agent, skill.source, skill.name)
      setDiffFiles(files)
      setDiffSelected(0)
    } catch (e) {
      setError(String(e))
    }
  }

  async function overwrite(agent: string) {
    const agentLabel = SOURCES.find(s => s.id === agent)?.label ?? agent
    const isMissing = rows.find(row => row.agent === agent)?.state === 'missing'
    if (!isMissing && !window.confirm(t('skills.driftOverwriteConfirm', { name: skill.name, agent: agentLabel }))) return
    setBusy(true)
    try {
      await applySkillSyncOne(agent, skill.source, skill.name)
      onNotice(t(isMissing ? 'skills.arbitrateDeployMissingDone' : 'skills.arbitrateOverwriteDone', { agent: agentLabel, name: skill.name }))
      setRows(rows.map(r => r.agent === agent ? { ...r, state: 'in_sync', changed_files: 0 } : r))
      setDiffAgent(null)
      setDiffFiles(null)
      await refresh()
    } catch (e) {
      setError(String(e))
    } finally {
      setBusy(false)
    }
  }

  async function adopt(agent: string) {
    const agentLabel = SOURCES.find(s => s.id === agent)?.label ?? agent
    if (!window.confirm(t('skills.driftAdoptConfirm', { name: skill.name, agent: agentLabel }))) return
    setBusy(true)
    try {
      const result = await adoptLocalSkill(agent, skill.source, skill.name)
      onNotice(t('skills.driftAdoptDone', {
        name: skill.name,
        synced: result.synced.length,
        skipped: result.skipped.length,
      }))
      // 其他被跳过的 Agent（自己也改过）保持 modified，继续留给用户仲裁。
      const skippedSet = new Set(result.skipped)
      setRows(rows.map(r =>
        skippedSet.has(r.agent) || r.state === 'missing'
          ? r
          : { ...r, state: 'in_sync', changed_files: 0 },
      ))
      setDiffAgent(null)
      setDiffFiles(null)
      await refresh()
    } catch (e) {
      setError(String(e))
    } finally {
      setBusy(false)
    }
  }

  const stateChip = (state: SkillDriftAgent['state'], changed: number) => {
    if (state === 'in_sync') {
      return <span className="rounded-full bg-emerald-500/10 px-2 py-0.5 text-[11px] font-semibold text-emerald-700 dark:text-emerald-300">✓ {t('skills.arbitrateStateInSync')}</span>
    }
    if (state === 'missing') {
      return <span className="rounded-full bg-rose-500/10 px-2 py-0.5 text-[11px] font-semibold text-rose-700 dark:text-rose-300">{t('skills.arbitrateStateMissing')}</span>
    }
    return <span className="rounded-full bg-amber-500/10 px-2 py-0.5 text-[11px] font-semibold text-amber-700 dark:text-amber-300">{t('skills.arbitrateStateModified', { count: changed })}</span>
  }

  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center p-4" role="dialog" aria-modal="true" aria-label={t('skills.arbitrateTitle')}>
      <button aria-label={t('skills.dialogCancel')} onClick={onClose} className="absolute inset-0 cursor-default bg-gray-950/50 backdrop-blur-[2px]" />
      <div className="relative flex max-h-[85vh] w-full max-w-2xl flex-col overflow-hidden rounded-2xl border border-gray-200 bg-white shadow-2xl dark:border-gray-700 dark:bg-gray-900">
        <div className="flex items-center gap-2 border-b border-gray-100 px-5 py-4 dark:border-gray-800">
          <div className="rounded-lg bg-amber-500/10 p-1.5 text-amber-600 dark:text-amber-400">
            <Scale size={16} />
          </div>
          <div className="min-w-0">
            <h2 className="truncate text-sm font-semibold text-gray-800 dark:text-gray-100">
              {t('skills.arbitrateTitle')} · <span className="font-mono">{skill.name}</span>
            </h2>
            <p className="text-[11px] text-gray-500 dark:text-gray-400">{t('skills.arbitrateHint', { version: skill.version })}</p>
          </div>
          <button onClick={onClose} aria-label={t('skills.dialogCancel')} className="ml-auto shrink-0 rounded-md p-1.5 text-gray-500 transition hover:bg-gray-100 hover:text-gray-600 dark:hover:bg-gray-800 dark:hover:text-gray-300">
            <X size={15} />
          </button>
        </div>

        <p className="border-b border-gray-100 px-5 py-2 text-[11px] leading-4 text-gray-500 dark:border-gray-800 dark:text-gray-400">
          {t('skills.arbitrateChoices')}
        </p>

        <div className="min-h-0 flex-1 space-y-3 overflow-y-auto p-5">
          {error && <p className="text-xs text-rose-500">{t('skills.driftLoadFailed', { error })}</p>}
          {rows.length === 0 && (
            <p className="py-6 text-center text-xs text-gray-500">{t('skills.notEquipped')}</p>
          )}
          {rows.map((row) => {
            const agent = SOURCES.find(s => s.id === row.agent)
            return (
              <div key={row.agent} className="rounded-xl border border-gray-200 p-3 dark:border-gray-700">
                <div className="flex flex-wrap items-center gap-2">
                  <span className={`h-2 w-2 rounded-full ${agent?.color ?? 'bg-gray-400'}`} />
                  <span className="text-xs font-semibold text-gray-800 dark:text-gray-100">{agent?.label ?? row.agent}</span>
                  {stateChip(row.state, row.changed_files)}
                  <div className="ml-auto flex flex-wrap items-center gap-1.5">
                    {row.state === 'modified' && (
                      <>
                        <button
                          onClick={() => { void viewDiff(row.agent) }}
                          disabled={busy}
                          className="rounded-md border border-gray-300 px-2 py-1 text-[11px] font-medium text-gray-600 transition hover:bg-gray-50 disabled:opacity-40 dark:border-gray-600 dark:text-gray-300 dark:hover:bg-gray-800"
                        >
                          {diffAgent === row.agent ? t('skills.arbitrateHideDiff') : t('skills.arbitrateViewDiff')}
                        </button>
                        <button
                          onClick={() => { void overwrite(row.agent) }}
                          disabled={busy}
                          className="inline-flex items-center gap-1 rounded-md border border-gray-300 px-2 py-1 text-[11px] font-medium text-gray-600 transition hover:bg-gray-50 disabled:opacity-40 dark:border-gray-600 dark:text-gray-300 dark:hover:bg-gray-800"
                        >
                          <RotateCcw size={11} />
                          {t('skills.driftOverwrite')}
                        </button>
                        <button
                          onClick={() => { void adopt(row.agent) }}
                          disabled={busy}
                          className="inline-flex items-center gap-1 rounded-md bg-violet-600 px-2 py-1 text-[11px] font-medium text-white transition hover:bg-violet-500 disabled:opacity-40"
                        >
                          {busy ? <Loader2 size={11} className="animate-spin motion-reduce:animate-none" /> : <ArrowRightLeft size={11} />}
                          {t('skills.driftAdopt')}
                        </button>
                      </>
                    )}
                    {row.state === 'missing' && (
                      <button
                        onClick={() => { void overwrite(row.agent) }}
                        disabled={busy}
                        className="inline-flex items-center gap-1 rounded-md bg-rose-600 px-2 py-1 text-[11px] font-medium text-white transition hover:bg-rose-500 disabled:opacity-40"
                      >
                        <ShieldCheck size={11} />
                        {t('skills.arbitrateDeployMissing')}
                      </button>
                    )}
                  </div>
                </div>
                {diffAgent === row.agent && (
                  <div className="mt-3 border-t border-gray-100 pt-3 dark:border-gray-800">
                    {diffFiles === null ? (
                      <div className="flex items-center justify-center gap-2 py-4 text-xs text-gray-500">
                        <Loader2 size={14} className="animate-spin motion-reduce:animate-none" />
                        {t('skills.deployChecking')}
                      </div>
                    ) : diffFiles.length === 0 ? (
                      <p className="py-3 text-center text-xs text-gray-500">{t('skills.driftEmpty')}</p>
                    ) : (
                      <SkillDiffPane files={diffFiles} selected={diffSelected} onSelect={setDiffSelected} />
                    )}
                  </div>
                )}
              </div>
            )
          })}
        </div>
      </div>
    </div>
  )
}
