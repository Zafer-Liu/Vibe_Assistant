import { useState } from 'react'
import { useTranslation } from 'react-i18next'
import type { AgentConfig } from '../types/agent'
import { X, Plus, Minus, Loader2, FolderOpen, Sparkles, AlertCircle, Eye, EyeOff, GitBranch, PenLine } from 'lucide-react'
import { invoke } from '@tauri-apps/api/core'
import { open } from '@tauri-apps/plugin-dialog'
import { GithubInstallTab } from './GithubInstallTab'

interface Props {
  initial?: Partial<AgentConfig>
  onSave: (config: Partial<AgentConfig>) => Promise<void>
  onClose: () => void
}

interface ScanResult {
  name: string
  command: string
  args: string[]
  port?: number
  description: string
  project_type: string
}

const PROJECT_TYPE_LABELS: Record<string, string> = {
  python: '🐍 Python',
  node: '🟩 Node.js',
  rust: '🦀 Rust',
  go: '🐹 Go',
  binary: '⚙️ Executable',
  'npm-global': '📦 npm global',
  unknown: '❓ Unknown',
}

type FormTab = 'manual' | 'github'

export function AgentForm({ initial, onSave, onClose }: Props) {
  const { t } = useTranslation()
  const isEdit = !!initial?.id

  // 编辑已有 Agent 时固定在 manual tab
  const [activeTab, setActiveTab] = useState<FormTab>('manual')

  const [name, setName] = useState(initial?.name ?? '')
  const [description, setDescription] = useState(initial?.description ?? '')
  const [command, setCommand] = useState(initial?.command ?? '')
  const [args, setArgs] = useState<string[]>(initial?.args ?? [])
  const [workingDir, setWorkingDir] = useState(initial?.working_dir ?? '')
  const [worktreeRepo, setWorktreeRepo] = useState(initial?.worktree_repo ?? '')
  const [worktreeBranch, setWorktreeBranch] = useState(initial?.worktree_branch ?? '')
  const [port, setPort] = useState(initial?.port?.toString() ?? '')
  const [uiToken, setUiToken] = useState(initial?.ui_token ?? '')
  const [showToken, setShowToken] = useState(false)
  const [autoRestart, setAutoRestart] = useState(initial?.auto_restart ?? false)
  const [envPairs, setEnvPairs] = useState<[string, string][]>(
    Object.entries(initial?.env ?? {})
  )
  const [saving, setSaving] = useState(false)
  const [scanning, setScanning] = useState(false)
  const [error, setError] = useState('')
  const [scanHint, setScanHint] = useState('')
  const [scanStatus, setScanStatus] = useState<'detected' | 'failed' | ''>('')

  /** GitHub tab 完成 clone 后预填充字段，并切换到 manual tab 让用户确认 */
  function handleGithubPrefill(partial: Partial<AgentConfig>) {
    if (partial.name !== undefined) setName(partial.name)
    if (partial.description !== undefined) setDescription(partial.description)
    if (partial.command !== undefined) setCommand(partial.command)
    if (partial.args !== undefined) setArgs(partial.args)
    if (partial.working_dir !== undefined) setWorkingDir(partial.working_dir)
    if (partial.port !== undefined) setPort(partial.port.toString())
    if (partial.env !== undefined) setEnvPairs(Object.entries(partial.env))
    if (partial.auto_restart !== undefined) setAutoRestart(partial.auto_restart)
    setTimeout(() => setActiveTab('manual'), 300)
  }

  async function pickDirectory() {
    try {
      const selected = await open({ directory: true, multiple: false })
      if (!selected || typeof selected !== 'string') return
      setWorkingDir(selected)
      await scanDirectory(selected)
    } catch (e) {
      const msg = String(e)
      if (!msg.includes('cancelled') && msg !== 'null' && msg !== 'undefined') {
        setScanStatus('failed')
        setScanHint(t('agentForm.openFolderFailed', { error: msg }))
      }
    }
  }

  async function scanDirectory(dir: string) {
    if (!dir) return
    setScanning(true)
    setScanHint('')
    setScanStatus('')
    try {
      const result = await invoke<ScanResult>('scan_project_dir', { dir })
      if (!name) setName(result.name)
      if (!description && result.description) setDescription(result.description)
      if (!command && result.command) setCommand(result.command)
      if (args.length === 0 && result.args.length) setArgs(result.args)
      if (!port && result.port) setPort(result.port.toString())

      if (result.project_type !== 'unknown') {
        setScanStatus('detected')
        setScanHint(t('agentForm.detected', { type: PROJECT_TYPE_LABELS[result.project_type] ?? result.project_type }))
      } else {
        setScanStatus('failed')
        setScanHint(t('agentForm.detectFailed'))
      }
    } catch (e) {
      setScanStatus('failed')
      setScanHint(t('agentForm.scanFailed', { error: String(e) }))
    } finally {
      setScanning(false)
    }
  }

  async function handleWorkingDirChange(val: string) {
    setWorkingDir(val)
    if (val.length > 3) {
      await scanDirectory(val)
    }
  }

  async function handleSubmit(e: React.FormEvent) {
    e.preventDefault()
    if (!name.trim()) { setError(t('agentForm.nameRequired')); return }
    if (!command.trim()) { setError(t('agentForm.commandRequired')); return }

    const env: Record<string, string> = {}
    envPairs.forEach(([k, v]) => { if (k.trim()) env[k.trim()] = v })

    setSaving(true)
    setError('')
    try {
      await onSave({
        id: initial?.id,
        name: name.trim(),
        description: description.trim(),
        command: command.trim(),
        args: args.filter(a => a.trim()),
        working_dir: workingDir.trim(),
        env,
        port: port ? Number(port) : undefined,
        ui_token: uiToken.trim() || undefined,
        worktree_repo: worktreeRepo.trim() || undefined,
        worktree_branch: worktreeRepo.trim() ? (worktreeBranch.trim() || undefined) : undefined,
        auto_restart: autoRestart,
      })
      onClose()
    } catch (err) {
      setError(String(err))
      setSaving(false)
    }
  }

  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/40 dark:bg-black/60">
      <div className="w-full max-w-lg rounded-2xl border border-gray-200 bg-white shadow-2xl dark:border-gray-700 dark:bg-gray-900">
        {/* Header */}
        <div className="flex items-center justify-between border-b border-gray-200 px-6 py-4 dark:border-gray-800">
          <h3 className="text-base font-semibold text-gray-900 dark:text-gray-100">
            {isEdit ? t('agentForm.titleEdit') : t('agentForm.titleNew')}
          </h3>
          <button onClick={onClose} className="rounded-md p-1 text-gray-400 hover:bg-gray-100 hover:text-gray-600 dark:hover:bg-gray-800 dark:hover:text-gray-300">
            <X className="h-5 w-5" />
          </button>
        </div>

        {/* Tab bar — 新建时才显示 */}
        {!isEdit && (
          <div className="flex gap-1 border-b border-gray-200 px-6 pt-3 dark:border-gray-800">
            <TabBtn
              active={activeTab === 'manual'}
              onClick={() => setActiveTab('manual')}
              icon={<PenLine className="h-3.5 w-3.5" />}
              label={t('agentForm.tabManual')}
            />
            <TabBtn
              active={activeTab === 'github'}
              onClick={() => setActiveTab('github')}
              icon={<GitBranch className="h-3.5 w-3.5" />}
              label={t('agentForm.tabGithub')}
            />
          </div>
        )}

        {/* GitHub 安装 Tab */}
        {activeTab === 'github' && !isEdit && (
          <div className="max-h-[70vh] overflow-y-auto px-6 py-5">
            <GithubInstallTab onPrefill={handleGithubPrefill} />
          </div>
        )}

        {/* 手动配置 Tab */}
        {(activeTab === 'manual' || isEdit) && (
        <form onSubmit={handleSubmit}>
          <div className="max-h-[65vh] overflow-y-auto px-6 py-5 space-y-4">

            <Field label={t('agentForm.workingDir')}>
              <div className="flex gap-2">
                <input
                  value={workingDir}
                  onChange={e => handleWorkingDirChange(e.target.value)}
                  placeholder="D:/projects/my-agent"
                  className="field-input flex-1 font-mono"
                />
                <button
                  type="button"
                  onClick={pickDirectory}
                  className="flex items-center rounded-lg border border-gray-200 px-3 py-2 text-gray-500 transition-colors hover:bg-gray-100 hover:text-gray-700 dark:border-gray-700 dark:text-gray-400 dark:hover:bg-gray-800 dark:hover:text-gray-200"
                  title={t('agentForm.browseFolder')}
                >
                  <FolderOpen className="h-4 w-4" />
                </button>
              </div>
              {(scanning || scanHint) && (
                <div className={`mt-2 flex items-center gap-1.5 text-xs rounded-lg px-3 py-2 ${
                  scanning
                    ? 'bg-blue-50 text-blue-600 dark:bg-blue-900/30 dark:text-blue-400'
                    : scanStatus === 'detected'
                      ? 'bg-green-50 text-green-700 dark:bg-green-900/30 dark:text-green-400'
                      : 'bg-yellow-50 text-yellow-700 dark:bg-yellow-900/30 dark:text-yellow-400'
                }`}>
                  {scanning
                    ? <><Loader2 className="h-3.5 w-3.5 animate-spin" /> {t('agentForm.scanningProject')}</>
                    : <><Sparkles className="h-3.5 w-3.5" /> {scanHint}</>
                  }
                </div>
              )}
            </Field>

            <Field label={t('agentForm.worktreeRepo')}>
              <input value={worktreeRepo} onChange={e => setWorktreeRepo(e.target.value)}
                placeholder={t('agentForm.worktreeRepoPlaceholder')} className="field-input font-mono" />
              <p className="mt-1 text-xs text-gray-400 dark:text-gray-600">
                {t('agentForm.worktreeRepoHint')}
              </p>
            </Field>

            {worktreeRepo.trim() && (
              <Field label={t('agentForm.worktreeBranch')}>
                <input value={worktreeBranch} onChange={e => setWorktreeBranch(e.target.value)}
                  placeholder={t('agentForm.worktreeBranchPlaceholder')} className="field-input font-mono" />
                <p className="mt-1 text-xs text-gray-400 dark:text-gray-600">
                  {t('agentForm.worktreeBranchHint')}
                </p>
              </Field>
            )}

            <div className="border-t border-gray-200 dark:border-gray-800" />

            <Field label={t('agentForm.name')}>
              <input autoFocus={!!initial?.id} value={name} onChange={e => setName(e.target.value)}
                placeholder={t('agentForm.namePlaceholder')} className="field-input" />
            </Field>

            <Field label={t('agentForm.description')}>
              <input value={description} onChange={e => setDescription(e.target.value)}
                placeholder={t('agentForm.descriptionPlaceholder')} className="field-input" />
            </Field>

            <Field label={t('agentForm.command')}>
              <input value={command} onChange={e => setCommand(e.target.value)}
                placeholder={t('agentForm.commandPlaceholder')} className="field-input font-mono" />
            </Field>

            <Field label={t('agentForm.args')}>
              <div className="space-y-1.5">
                {args.map((arg, i) => (
                  <div key={i} className="flex gap-2">
                    <input value={arg}
                      onChange={e => setArgs(prev => prev.map((a, j) => j === i ? e.target.value : a))}
                      placeholder={t('agentForm.argsPlaceholder')} className="field-input flex-1 font-mono text-xs" />
                    <button type="button"
                      onClick={() => setArgs(prev => prev.filter((_, j) => j !== i))}
                      className="text-gray-400 hover:text-red-500 dark:text-gray-600 dark:hover:text-red-400">
                      <Minus className="h-4 w-4" />
                    </button>
                  </div>
                ))}
                <button type="button" onClick={() => setArgs(prev => [...prev, ''])}
                  className="flex items-center gap-1 text-xs text-gray-400 hover:text-gray-700 dark:text-gray-500 dark:hover:text-gray-300">
                  <Plus className="h-3.5 w-3.5" /> {t('agentForm.addArg')}
                </button>
              </div>
            </Field>

            <div className="grid grid-cols-2 gap-3">
              <Field label={t('agentForm.port')}>
                <input type="number" value={port} onChange={e => setPort(e.target.value)}
                  placeholder={t('agentForm.portPlaceholder')} min={1} max={65535} className="field-input" />
              </Field>
              <Field label="">
                <label className="flex h-full items-end gap-2 pb-0.5 text-sm text-gray-600 cursor-pointer dark:text-gray-300">
                  <input type="checkbox" checked={autoRestart}
                    onChange={e => setAutoRestart(e.target.checked)} className="rounded" />
                  {t('agentForm.autoRestart')}
                </label>
              </Field>
            </div>

            <Field label={t('agentForm.uiToken')}>
              <div className="flex gap-2">
                <input
                  type={showToken ? 'text' : 'password'}
                  value={uiToken} onChange={e => setUiToken(e.target.value)}
                  placeholder="ad01a3b0282356d6..."
                  className="field-input flex-1 font-mono" autoComplete="off"
                />
                <button type="button" onClick={() => setShowToken(v => !v)}
                  className="flex items-center rounded-lg border border-gray-200 px-3 py-2 text-gray-400 hover:bg-gray-100 hover:text-gray-600 dark:border-gray-700 dark:hover:bg-gray-800 dark:hover:text-gray-200"
                  title={showToken ? t('agentForm.hide') : t('agentForm.show')}>
                  {showToken ? <EyeOff className="h-4 w-4" /> : <Eye className="h-4 w-4" />}
                </button>
              </div>
              <p className="mt-1 text-xs text-gray-400 dark:text-gray-600">
                {t('agentForm.uiTokenHint')}
              </p>
            </Field>

            <Field label={t('agentForm.env')}>
              <div className="space-y-2">
                {envPairs.map(([k, v], i) => (
                  <div key={i} className="flex gap-2">
                    <input value={k}
                      onChange={e => setEnvPairs(p => p.map((pair, j) => j === i ? [e.target.value, pair[1]] : pair))}
                      placeholder={t('agentForm.envKey')} className="field-input flex-1 font-mono" />
                    <input value={v}
                      onChange={e => setEnvPairs(p => p.map((pair, j) => j === i ? [pair[0], e.target.value] : pair))}
                      placeholder={t('agentForm.envValue')} className="field-input flex-1 font-mono" />
                    <button type="button"
                      onClick={() => setEnvPairs(p => p.filter((_, j) => j !== i))}
                      className="text-gray-400 hover:text-red-500 dark:text-gray-600 dark:hover:text-red-400">
                      <Minus className="h-4 w-4" />
                    </button>
                  </div>
                ))}
                <button type="button" onClick={() => setEnvPairs(p => [...p, ['', '']])}
                  className="flex items-center gap-1 text-xs text-gray-400 hover:text-gray-700 dark:text-gray-500 dark:hover:text-gray-300">
                  <Plus className="h-3.5 w-3.5" /> {t('agentForm.addEnv')}
                </button>
              </div>
            </Field>

            {error && (
              <div className="flex items-center gap-2 rounded-lg bg-red-50 px-3 py-2 text-sm text-red-600 dark:bg-red-900/30 dark:text-red-400">
                <AlertCircle className="h-4 w-4 shrink-0" />
                {error}
              </div>
            )}
          </div>

          {/* Footer */}
          <div className="flex justify-end gap-2 border-t border-gray-200 px-6 py-4 dark:border-gray-800">
            <button type="button" onClick={onClose}
              className="rounded-lg px-4 py-2 text-sm text-gray-500 hover:bg-gray-100 hover:text-gray-700 dark:text-gray-400 dark:hover:bg-gray-800 dark:hover:text-gray-200">
              {t('agentForm.cancel')}
            </button>
            <button type="submit" disabled={saving}
              className="flex items-center gap-2 rounded-lg bg-blue-600 px-4 py-2 text-sm font-medium text-white hover:bg-blue-500 disabled:opacity-60">
              {saving && <Loader2 className="h-4 w-4 animate-spin" />}
              {saving ? t('agentForm.saving') : t('agentForm.save')}
            </button>
          </div>
        </form>
        )}
      </div>
    </div>
  )
}

function Field({ label, children }: { label: string; children: React.ReactNode }) {
  return (
    <div>
      {label && <label className="mb-1 block text-sm font-medium text-gray-600 dark:text-gray-400">{label}</label>}
      {children}
    </div>
  )
}

function TabBtn({
  active, onClick, icon, label,
}: {
  active: boolean
  onClick: () => void
  icon: React.ReactNode
  label: string
}) {
  return (
    <button
      type="button"
      onClick={onClick}
      className={`flex items-center gap-1.5 rounded-t-md border-b-2 px-3 pb-2.5 pt-1 text-sm font-medium transition-colors ${
        active
          ? 'border-blue-500 text-blue-600 dark:border-blue-400 dark:text-blue-400'
          : 'border-transparent text-gray-400 hover:text-gray-600 dark:text-gray-500 dark:hover:text-gray-300'
      }`}
    >
      {icon}
      {label}
    </button>
  )
}
