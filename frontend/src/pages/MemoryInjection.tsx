import { memo, useEffect, useState } from 'react'
import { useTranslation } from 'react-i18next'
import { open } from '@tauri-apps/plugin-dialog'
import {
  ArrowLeft, ArrowDownToLine, ArrowUpFromLine, CheckCircle2, ChevronDown, ChevronUp, FolderOpen, Gauge, Loader2,
  PlugZap, RefreshCw, Webhook, X, XCircle, Zap,
} from 'lucide-react'
import { useMemoryStore } from '../store/memoryStore'
import { useVisiblePolling } from '../hooks/useVisiblePolling'
import { ErrorRecovery, type RecoveryTarget } from '../components/ErrorRecovery'
import type { HookStatus, MemoryMcpTarget } from '../types/memory'
import { displayFullTime } from '../components/ConversationDialog'
import { MemoryBreadcrumb } from '../components/MemoryBreadcrumb'

const MCP_ADAPTERS: { type: MemoryMcpTarget; label: string }[] = [
  { type: 'codex_cli', label: 'Codex CLI' },
  { type: 'claude_cli', label: 'Claude Code CLI' },
  { type: 'codex_desktop', label: 'Codex Desktop' },
  { type: 'claude_desktop', label: 'Claude Desktop' },
  { type: 'qoder', label: 'Qoder' },
  { type: 'workbuddy', label: 'WorkBuddy' },
  { type: 'minimax', label: 'MiniMax Code' },
  { type: 'kimi', label: 'Kimi' },
  { type: 'zcode', label: 'ZCode' },
]

type HookAgentType = 'claude' | 'qoder' | 'codex' | 'workbuddy'

/** 详情回放：JSON 内容格式化展示，纯文本（如注入上下文）原样返回。 */
function formatLogDetail(detail: string) {
  try {
    return JSON.stringify(JSON.parse(detail), null, 2)
  } catch {
    return detail
  }
}

/** 支持命令式 Hook 的 Agent；SessionStart 注入只对这些 Agent 可用。 */
const HOOK_AGENTS: { id: HookAgentType; label: string }[] = [
  { id: 'codex', label: 'Codex' },
  { id: 'claude', label: 'Claude Code' },
  { id: 'qoder', label: 'Qoder' },
  { id: 'workbuddy', label: 'WorkBuddy' },
]

export const MemoryInjection = memo(function MemoryInjection({ onBack, active = true }: { onBack: () => void; active?: boolean }) {
  const { t } = useTranslation()
  const {
    hookStatus, qoderHookStatus, codexHookStatus, workbuddyHookStatus,
    ingestStatus, agentSources, memoryMcp, mcpAccessLogs, memoryInjectionStats,
    checkIngest, installHook, uninstallHook, setIngestEnabled,
    checkMemoryMcp, installMemoryMcp, uninstallMemoryMcp, checkMcpAccessLogs, loadInjectionStats,
    loadAgentSources, setAgentSourceOverride,
  } = useMemoryStore()
  const [mcpAction, setMcpAction] = useState<MemoryMcpTarget | null>(null)
  const [hookAction, setHookAction] = useState<string | null>(null)
  const [refreshing, setRefreshing] = useState(false)
  const [expandedLogId, setExpandedLogId] = useState<number | null>(null)
  const [logsExpanded, setLogsExpanded] = useState(false)
  const SUMMARY_PREVIEW_COUNT = 5
  const [notice, setNotice] = useState<{ kind: 'ok' | 'err'; text: string; recovery?: RecoveryTarget } | null>(null)

  useEffect(() => {
    if (!active) return
    void Promise.allSettled([checkIngest(), checkMemoryMcp(), loadAgentSources()])
  }, [checkIngest, checkMemoryMcp, loadAgentSources, active])

  useVisiblePolling(checkMcpAccessLogs, 10_000, active)
  // 30 天汇总口径变化慢，低于审计日志的轮询频率即可。
  useVisiblePolling(loadInjectionStats, 30_000, active)

  function flash(kind: 'ok' | 'err', text: string, recovery?: RecoveryTarget) {
    const next = { kind, text, recovery }
    setNotice(next)
    if (kind === 'ok') setTimeout(() => setNotice(current => current === next ? null : current), 3500)
  }

  async function handleRefresh() {
    if (refreshing) return
    setRefreshing(true)
    try {
      await Promise.all([checkIngest(), checkMemoryMcp(), checkMcpAccessLogs(), loadInjectionStats(), loadAgentSources()])
    } catch (error) {
      flash('err', String(error))
    } finally {
      setRefreshing(false)
    }
  }

  // ── 会话采集（Hook / 转录扫描） ──────────────────────────────────────────

  const hookStatusByAgent: Record<string, HookStatus | null> = {
    codex: codexHookStatus,
    claude: hookStatus,
    qoder: qoderHookStatus,
    workbuddy: workbuddyHookStatus,
  }

  async function handleHookInstall(agentType: HookAgentType, label: string) {
    if (hookAction) return
    setHookAction(agentType)
    try {
      await installHook(agentType)
      flash('ok', t('memory.injection.hookInstalledToast', { label }))
    } catch (error) {
      flash('err', t('memory.injection.hookInstallFailed', { error: String(error) }))
    } finally {
      setHookAction(null)
    }
  }

  async function handleHookUninstall(agentType: HookAgentType, label: string) {
    if (hookAction) return
    setHookAction(agentType)
    try {
      await uninstallHook(agentType)
      flash('ok', t('memory.injection.hookRemovedToast', { label }))
    } catch (error) {
      flash('err', t('memory.injection.hookRemoveFailed', { error: String(error) }))
    } finally {
      setHookAction(null)
    }
  }

  async function handleChangeSourceDir(id: string) {
    const selected = await open({ directory: true, multiple: false, title: t('memory.injection.dirDialogTitle') })
    if (!selected || Array.isArray(selected)) return
    try {
      await setAgentSourceOverride(id, [selected], null)
      flash('ok', t('memory.injection.dirUpdated'))
    } catch (error) {
      flash('err', t('memory.injection.dirUpdateFailed', { error: String(error) }))
    }
  }

  async function handleResetSourceDir(id: string) {
    try {
      await setAgentSourceOverride(id, null, null)
      flash('ok', t('memory.injection.dirReset'))
    } catch (error) {
      flash('err', t('memory.injection.dirResetFailed', { error: String(error) }))
    }
  }

  // ── 记忆注入（共享记忆 MCP） ─────────────────────────────────────────────

  function adapterLabel(agentType: MemoryMcpTarget) {
    return MCP_ADAPTERS.find((adapter) => adapter.type === agentType)?.label ?? agentType
  }

  async function handleMcpInstall(agentType: MemoryMcpTarget) {
    if (mcpAction) return
    setMcpAction(agentType)
    try {
      await installMemoryMcp(agentType)
      flash('ok', t('memory.injection.mcpConnectedToast', { label: adapterLabel(agentType) }))
    } catch (error) {
      flash('err', t('memory.injection.mcpConnectFailed', { error: String(error) }), 'mcp-library')
    } finally {
      setMcpAction(null)
    }
  }

  async function handleMcpUninstall(agentType: MemoryMcpTarget) {
    if (mcpAction) return
    setMcpAction(agentType)
    try {
      await uninstallMemoryMcp(agentType)
      flash('ok', t('memory.injection.mcpDisconnectedToast', { label: adapterLabel(agentType) }))
    } catch (error) {
      flash('err', t('memory.injection.mcpDisconnectFailed', { error: String(error) }), 'mcp-library')
    } finally {
      setMcpAction(null)
    }
  }

  const hookReadyCount = agentSources.filter((source) =>
    source.supports_hooks ? hookStatusByAgent[source.id]?.installed : true,
  ).length
  const mcpStatusList = Object.values(memoryMcp)
  const mcpStatusLoaded = mcpStatusList.some(Boolean)
  const connectedMcpCount = mcpStatusList.filter((status) => status?.installed).length
  // Hook 注入（SessionStart 启动 + UserPromptSubmit 每轮）与采集共用同一份
  // Hook 配置；早期版本只装了 3 个采集事件，因此采集已就绪不代表注入已
  // 启用，两者必须分开表达。
  const COLLECT_EVENTS = ['UserPromptSubmit', 'PostToolUse', 'Stop']
  const collectEventCount = (hook: HookStatus | null) => hook?.events.filter((event) => COLLECT_EVENTS.includes(event)).length ?? 0
  const injectEnabled = (hook: HookStatus | null) => Boolean(hook?.events.includes('SessionStart') && hook?.events.includes('UserPromptSubmit'))
  const injectReadyCount = HOOK_AGENTS.filter((agent) => injectEnabled(hookStatusByAgent[agent.id])).length

  return <main className="h-full overflow-y-auto bg-[#fbfcfe] text-slate-900 dark:bg-slate-950 dark:text-slate-100">
    <div className="mx-auto max-w-[1200px] px-5 py-6 lg:px-9 lg:py-8">
      <header className="flex flex-wrap items-start justify-between gap-4">
        <div className="flex items-start gap-3">
          <button type="button" onClick={onBack} className="mt-0.5 inline-flex h-9 w-9 items-center justify-center rounded-lg text-slate-500 transition hover:bg-slate-100 hover:text-slate-900 focus:outline-none focus-visible:ring-2 focus-visible:ring-blue-500 dark:text-slate-400 dark:hover:bg-slate-800 dark:hover:text-white" aria-label={t('memory.injection.back')}><ArrowLeft size={19} /></button>
          <div>
            <MemoryBreadcrumb currentKey="memory.injection.title" onBack={onBack} />
            <h1 className="mt-1 text-2xl font-bold tracking-[-0.025em]">{t('memory.injection.title')}</h1>
            <p className="mt-1 text-sm text-slate-500 dark:text-slate-400">{t('memory.injection.desc')}</p>
          </div>
        </div>
        <div className="flex flex-wrap items-center gap-2">
          {notice && <span className={`rounded-lg px-3 py-2 text-xs ${notice.kind === 'ok' ? 'bg-emerald-500/10 text-emerald-700 dark:text-emerald-300' : 'bg-red-500/10 text-red-700 dark:text-red-300'}`} role="status">{notice.text}</span>}
          <button type="button" onClick={() => { void handleRefresh() }} disabled={refreshing} className="inline-flex h-10 items-center gap-2 rounded-lg border border-slate-200 bg-white px-3 text-sm font-medium text-slate-700 transition hover:border-slate-300 hover:bg-slate-50 disabled:cursor-wait disabled:opacity-60 focus:outline-none focus-visible:ring-2 focus-visible:ring-blue-500 dark:border-slate-700 dark:bg-slate-900 dark:text-slate-200 dark:hover:bg-slate-800"><RefreshCw size={16} className={refreshing ? 'animate-spin motion-reduce:animate-none' : ''} />{t('memory.injection.refreshStatus')}</button>
        </div>
      </header>
      {notice?.kind === 'err' && <ErrorRecovery error={notice.text} fallback={notice.recovery} onRetry={() => { void handleRefresh() }} disabled={refreshing} />}
      {ingestStatus && !ingestStatus.model_ready && <ErrorRecovery fallback="settings" />}

      <section className="mt-7 grid gap-3 sm:grid-cols-2" aria-label={t('memory.injection.overviewLabel')}>
        <div className="rounded-xl border border-slate-200 bg-white px-4 py-3 dark:border-slate-800 dark:bg-slate-900">
          <div className="flex items-center gap-2 text-sm text-slate-500 dark:text-slate-400"><ArrowDownToLine size={15} className="text-sky-500" />{t('memory.injection.captureCard')}</div>
          <p className="mt-2 font-mono text-2xl font-semibold tabular-nums">{agentSources.length === 0 ? '—' : `${hookReadyCount} / ${agentSources.length}`}</p>
          <p className="mt-1 text-xs text-slate-400">{t('memory.injection.captureCardHint')}</p>
        </div>
        <div className="rounded-xl border border-slate-200 bg-white px-4 py-3 dark:border-slate-800 dark:bg-slate-900">
          <div className="flex items-center gap-2 text-sm text-slate-500 dark:text-slate-400"><ArrowUpFromLine size={15} className="text-violet-500" />{t('memory.injection.injectCard')}</div>
          <p className="mt-2 font-mono text-2xl font-semibold tabular-nums">{mcpStatusLoaded ? `${connectedMcpCount} / ${MCP_ADAPTERS.length}` : '—'}</p>
          <p className="mt-1 text-xs text-slate-400">{t('memory.injection.injectCardHint', { ready: injectReadyCount, total: HOOK_AGENTS.length })}</p>
        </div>
      </section>

      <section className="mt-6 rounded-2xl border border-slate-200 bg-white p-5 shadow-[0_1px_2px_rgba(15,23,42,0.03)] dark:border-slate-800 dark:bg-slate-900 sm:p-6" aria-label={t('memory.injection.captureLabel')}>
        <div className="flex flex-wrap items-center gap-3">
          <span className="grid h-9 w-9 shrink-0 place-items-center rounded-xl bg-sky-500/10 text-sky-600 dark:text-sky-300"><Webhook size={18} /></span>
          <div className="min-w-0 flex-1">
            <h2 className="text-sm font-semibold text-slate-700 dark:text-slate-200">{t('memory.injection.captureTitle')}</h2>
            <p className="mt-0.5 text-xs text-slate-500 dark:text-slate-400">{t('memory.injection.captureDesc')}</p>
          </div>
          <div className="flex shrink-0 items-center gap-2">
            {ingestStatus?.enabled
              ? <span className="inline-flex items-center gap-1 text-xs text-emerald-600 dark:text-emerald-400"><CheckCircle2 size={12} />{t('memory.injection.autoCaptureOn')}</span>
              : <span className="inline-flex items-center gap-1 text-xs text-slate-400"><XCircle size={12} />{t('memory.injection.autoCaptureOff')}</span>}
            <button
              type="button"
              onClick={() => setIngestEnabled(!ingestStatus?.enabled)}
              className={`relative h-5 w-9 rounded-full transition-colors ${ingestStatus?.enabled ? 'bg-sky-600' : 'bg-slate-300 dark:bg-slate-600'}`}
              title={ingestStatus?.enabled ? t('memory.injection.autoCaptureDisable') : t('memory.injection.autoCaptureEnable')}
              aria-pressed={Boolean(ingestStatus?.enabled)}
            >
              <span className="absolute top-0.5 h-4 w-4 rounded-full bg-white transition-all" style={{ left: ingestStatus?.enabled ? '18px' : '2px' }} />
            </button>
          </div>
        </div>

        <div className={`mt-3 flex items-center gap-1.5 text-xs ${ingestStatus?.model_ready ? 'text-emerald-700 dark:text-emerald-400' : 'text-amber-800 dark:text-amber-300'}`}>
          {ingestStatus?.model_ready ? <CheckCircle2 size={13} /> : <XCircle size={13} />}
          {ingestStatus?.model_ready ? t('memory.injection.modelReady', { provider: ingestStatus.model_provider_id ?? t('memory.injection.modelConfigured') }) : t('memory.injection.modelMissing')}
        </div>

        <div className="mt-3 space-y-1.5">
          {agentSources.map((source) => {
            const hook = hookStatusByAgent[source.id]
            const supportsHook = source.supports_hooks
            const roots = source.transcript_roots
            const primary = roots[0]
            const missing = roots.every((root) => !root.exists)
            const working = hookAction === source.id
            return <div key={source.id} className="rounded-lg border border-slate-200 bg-slate-50/70 px-3 py-2.5 dark:border-slate-700 dark:bg-slate-900/35">
              <div className="flex flex-wrap items-center gap-2">
                <span className="text-sm font-medium text-slate-700 dark:text-slate-200">{source.label}</span>
                {supportsHook ? (
                  hook?.installed
                    ? <span className="inline-flex items-center gap-1 rounded-full bg-emerald-500/10 px-2 py-1 text-xs text-emerald-600 dark:text-emerald-400" title={t('memory.injection.captureEventsTitle', { events: hook.events.filter((event) => COLLECT_EVENTS.includes(event)).join(' / ') || t('memory.injection.captureEventsNone') })}><CheckCircle2 size={12} />{t('memory.injection.hookInstalled', { count: collectEventCount(hook) })}</span>
                    : <span className="inline-flex items-center gap-1 rounded-full bg-slate-100 px-2 py-1 text-xs text-slate-500 dark:bg-slate-700 dark:text-slate-400"><XCircle size={12} />{t('memory.injection.hookNotInstalled')}</span>
                ) : (
                  <span className="inline-flex items-center gap-1 rounded-full bg-sky-500/10 px-2 py-1 text-xs text-sky-700 dark:text-sky-300" title={t('memory.injection.transcriptScanTitle')}><CheckCircle2 size={12} />{t('memory.injection.transcriptScan')}</span>
                )}
                {supportsHook && (
                  hook?.installed
                    ? <button onClick={() => { void handleHookUninstall(source.id as HookAgentType, source.label) }} disabled={working} className="ml-auto inline-flex shrink-0 items-center gap-1 rounded-md border border-slate-300 px-2.5 py-1 text-xs text-slate-600 hover:bg-slate-50 disabled:opacity-50 dark:border-slate-600 dark:text-slate-300 dark:hover:bg-slate-700">{working ? <Loader2 size={13} className="animate-spin motion-reduce:animate-none" /> : <X size={13} />}{t('memory.injection.uninstallHook')}</button>
                    : <button onClick={() => { void handleHookInstall(source.id as HookAgentType, source.label) }} disabled={working} className="ml-auto inline-flex shrink-0 items-center gap-1 rounded-md bg-sky-600 px-2.5 py-1 text-xs font-medium text-white transition-colors hover:bg-sky-500 disabled:opacity-50">{working ? <Loader2 size={13} className="animate-spin motion-reduce:animate-none" /> : <Zap size={13} />}{t('memory.injection.installHook')}</button>
                )}
              </div>
              <div className="mt-1.5 flex flex-wrap items-center gap-2 text-xs">
                <span className="shrink-0 text-slate-400">{t('memory.injection.dataDir')}</span>
                <span className={`min-w-0 flex-1 truncate font-mono ${missing ? 'text-amber-600 dark:text-amber-400' : 'text-slate-500 dark:text-slate-400'}`} title={roots.map((root) => root.path).join('\n')}>
                  {primary ? primary.path : t('memory.injection.dataDirUnset')}{roots.length > 1 ? t('memory.injection.dataDirCount', { count: roots.length }) : ''}{missing ? t('memory.injection.dataDirMissing') : ''}
                </span>
                {primary?.is_override && <button onClick={() => { void handleResetSourceDir(source.id) }} className="shrink-0 rounded border border-slate-300 px-2 py-0.5 text-[11px] text-slate-600 hover:bg-slate-100 dark:border-slate-600 dark:text-slate-300 dark:hover:bg-slate-700">{t('memory.injection.resetDefault')}</button>}
                <button onClick={() => { void handleChangeSourceDir(source.id) }} className="inline-flex shrink-0 items-center gap-1 rounded border border-sky-200 bg-white px-2 py-0.5 text-[11px] font-medium text-sky-700 hover:bg-sky-50 dark:border-sky-500/30 dark:bg-slate-800 dark:text-sky-300 dark:hover:bg-sky-500/10"><FolderOpen size={11} />{t('memory.injection.change')}</button>
              </div>
            </div>
          })}
        </div>
        <p className="mt-2 text-[11px] leading-4 text-slate-500 dark:text-slate-400">{t('memory.injection.hookDesc')}</p>

        {ingestStatus && ingestStatus.buffered_sessions > 0 && (
          <p className="mt-3 rounded-md bg-amber-500/10 px-2.5 py-2 text-xs text-amber-800 dark:text-amber-200">
            {ingestStatus.model_ready ? t('memory.injection.buffered', { count: ingestStatus.buffered_sessions }) : t('memory.injection.bufferedModelUnavailable', { count: ingestStatus.buffered_sessions })}
          </p>
        )}

        {ingestStatus && ingestStatus.recent.length > 0 && (
          <div className="mt-3 min-w-0">
            <p className="text-xs text-slate-500 dark:text-slate-400">{t('memory.injection.recordsLabel')}</p>
            <div className="scrollbar-slim mt-1 max-h-44 space-y-1 overflow-y-auto rounded-md border border-slate-200 bg-slate-50 p-2 pr-1.5 dark:border-slate-700 dark:bg-slate-900/40">
              {ingestStatus.recent.map((log, i) => (
                <div key={`${log.at}-${log.state}-${log.detail}-${i}`} className="flex min-w-0 items-start gap-2 text-xs text-slate-600 dark:text-slate-300">
                  <span className="shrink-0 font-mono text-slate-400">{log.at}</span>
                  <span className={`shrink-0 rounded px-1.5 ${log.state === 'retrying' ? 'bg-amber-500/10 text-amber-700 dark:text-amber-300' : log.state === 'working' ? 'bg-sky-500/10 text-sky-700 dark:text-sky-300' : log.state === 'failed' ? 'bg-red-500/10 text-red-700 dark:text-red-300' : 'bg-violet-500/10 text-violet-600 dark:text-violet-400'}`}>
                    {log.state === 'retrying' ? t('memory.injection.recordRetrying') : log.state === 'working' ? t('memory.injection.recordWorking') : log.state === 'failed' ? t('memory.injection.recordFailed') : t('memory.injection.recordStored')}
                  </span>
                  <span className="min-w-0 flex-1 break-words" title={log.detail}>{log.detail}</span>
                </div>
              ))}
            </div>
          </div>
        )}
      </section>

      <section className="mt-6 rounded-2xl border border-slate-200 bg-white p-5 shadow-[0_1px_2px_rgba(15,23,42,0.03)] dark:border-slate-800 dark:bg-slate-900 sm:p-6" aria-label={t('memory.injection.injectLabel')}>
        <div className="flex flex-wrap items-center gap-3">
          <span className="grid h-9 w-9 shrink-0 place-items-center rounded-xl bg-violet-500/10 text-violet-600 dark:text-violet-300"><PlugZap size={18} /></span>
          <div className="min-w-0 flex-1">
            <h2 className="text-sm font-semibold text-slate-700 dark:text-slate-200">{t('memory.injection.injectTitle')}</h2>
            <p className="mt-0.5 text-xs text-slate-500 dark:text-slate-400">{t('memory.injection.injectDesc')}</p>
          </div>
        </div>

        <div className="mt-4 rounded-xl border border-violet-200/70 bg-violet-50/40 p-3.5 dark:border-violet-500/20 dark:bg-violet-500/5">
          <div className="flex flex-wrap items-center gap-2">
            <Zap size={14} className="text-violet-600 dark:text-violet-300" />
            <h3 className="text-xs font-semibold text-slate-700 dark:text-slate-200">{t('memory.injection.hookInjectTitle')}</h3>
            <span className="text-[11px] text-slate-500 dark:text-slate-400">{t('memory.injection.hookInjectBadge')}</span>
          </div>
          <p className="mt-1.5 text-[11px] leading-4 text-slate-500 dark:text-slate-400">{t('memory.injection.hookInjectDesc')}</p>
          <div className="mt-2.5 space-y-1.5">
            {HOOK_AGENTS.map((agent) => {
              const hook = hookStatusByAgent[agent.id]
              const enabled = injectEnabled(hook)
              const working = hookAction === agent.id
              return <div key={agent.id} className="flex flex-wrap items-center gap-2 rounded-lg border border-slate-200 bg-white px-3 py-2.5 dark:border-slate-700 dark:bg-slate-900/35">
                <span className="text-sm font-medium text-slate-700 dark:text-slate-200">{agent.label}</span>
                {enabled
                  ? <span className="inline-flex items-center gap-1 rounded-full bg-emerald-500/10 px-2 py-1 text-xs text-emerald-600 dark:text-emerald-400"><CheckCircle2 size={12} />{t('memory.injection.injectEnabled')}</span>
                  : <span className="inline-flex items-center gap-1 rounded-full bg-slate-100 px-2 py-1 text-xs text-slate-500 dark:bg-slate-700 dark:text-slate-400"><XCircle size={12} />{hook?.installed ? t('memory.injection.injectDisabledOld') : t('memory.injection.injectDisabled')}</span>}
                {!enabled && <button onClick={() => { void handleHookInstall(agent.id, agent.label) }} disabled={working} title={t('memory.injection.enableInjectTitle')} className="ml-auto inline-flex shrink-0 items-center gap-1 rounded-md bg-violet-600 px-2.5 py-1 text-xs font-medium text-white transition-colors hover:bg-violet-500 disabled:opacity-50">{working ? <Loader2 size={13} className="animate-spin motion-reduce:animate-none" /> : <Zap size={13} />}{t('memory.injection.enableInject')}</button>}
              </div>
            })}
          </div>
          <p className="mt-2 text-[11px] leading-4 text-slate-500 dark:text-slate-400">{t('memory.injection.hookInjectNote')}</p>
        </div>

        <div className="mt-4 rounded-xl border border-violet-200/70 bg-violet-50/40 p-3.5 dark:border-violet-500/20 dark:bg-violet-500/5">
          <div className="flex flex-wrap items-center gap-2">
            <PlugZap size={14} className="text-violet-600 dark:text-violet-300" />
            <h3 className="text-xs font-semibold text-slate-700 dark:text-slate-200">{t('memory.injection.mcpTitle')}</h3>
            <span className="text-[11px] text-slate-500 dark:text-slate-400">{t('memory.injection.mcpBadge')}</span>
          </div>
          <p className="mt-1.5 text-[11px] leading-4 text-slate-500 dark:text-slate-400">{t('memory.injection.mcpDesc')}</p>
          <div className="mt-2.5 space-y-1.5">
            {MCP_ADAPTERS.map((adapter) => {
              const status = memoryMcp[adapter.type]
              const working = mcpAction === adapter.type
              return <div key={adapter.type} className="flex flex-wrap items-center gap-2 rounded-lg border border-slate-200 bg-white px-3 py-2.5 dark:border-slate-700 dark:bg-slate-900/35">
                <span className="text-sm font-medium text-slate-700 dark:text-slate-200">{adapter.label}</span>
                {status?.installed
                  ? <span className="inline-flex items-center gap-1 rounded-full bg-emerald-500/10 px-2 py-1 text-xs text-emerald-600 dark:text-emerald-400" title={status.detail}><CheckCircle2 size={12} />{t('memory.injection.mcpConnected')}</span>
                  : <span className="inline-flex items-center gap-1 rounded-full bg-slate-100 px-2 py-1 text-xs text-slate-500 dark:bg-slate-700 dark:text-slate-400" title={status?.detail}><XCircle size={12} />{t('memory.injection.mcpNotConnected')}</span>}
                {status?.installed
                  ? <button onClick={() => { void handleMcpUninstall(adapter.type) }} disabled={working} className="ml-auto inline-flex shrink-0 items-center gap-1 rounded-md border border-slate-300 px-2.5 py-1 text-xs text-slate-600 hover:bg-slate-50 disabled:opacity-50 dark:border-slate-600 dark:text-slate-300 dark:hover:bg-slate-700">{working ? <Loader2 size={13} className="animate-spin motion-reduce:animate-none" /> : <X size={13} />}{t('memory.injection.disconnect')}</button>
                  : <button onClick={() => { void handleMcpInstall(adapter.type) }} disabled={working} className="ml-auto inline-flex shrink-0 items-center gap-1 rounded-md bg-violet-600 px-2.5 py-1 text-xs font-medium text-white transition-colors hover:bg-violet-500 disabled:opacity-50">{working ? <Loader2 size={13} className="animate-spin motion-reduce:animate-none" /> : <PlugZap size={13} />}{t('memory.injection.connect')}</button>}
              </div>
            })}
          </div>
        </div>
      </section>

      <section className="mt-6 rounded-2xl border border-slate-200 bg-white p-5 shadow-[0_1px_2px_rgba(15,23,42,0.03)] dark:border-slate-800 dark:bg-slate-900 sm:p-6" aria-label={t('memory.injection.summaryLabel')}>
        <h2 className="text-sm font-semibold text-slate-700 dark:text-slate-200">{t('memory.injection.summaryTitle')}</h2>
        <p className="mt-1 text-xs text-slate-500 dark:text-slate-400">{t('memory.injection.summaryDesc')}</p>
        {memoryInjectionStats.length > 0 && (
          <div className="mt-3 rounded-xl border border-slate-200 bg-slate-50/70 p-3 dark:border-slate-700 dark:bg-slate-900/35" aria-label={t('memory.injection.statsLabel')}>
            <div className="flex items-center gap-1.5 text-xs font-semibold text-slate-600 dark:text-slate-300">
              <Gauge size={13} className="text-indigo-500" />
              {t('memory.injection.statsTitle')}
            </div>
            <div className="mt-2 space-y-1.5">
              {memoryInjectionStats.map((row) => {
                const promptTurns = row.prompt_injections + row.prompt_skips
                const gateRate = promptTurns > 0 ? `${Math.round((row.prompt_skips / promptTurns) * 100)}%` : null
                const estTokens = Math.round(row.injected_chars / 3)
                return <div key={row.client_name} className="flex flex-wrap items-center gap-x-3 gap-y-1 px-1 text-xs text-slate-600 dark:text-slate-300">
                  <span className="min-w-28 font-medium text-slate-700 dark:text-slate-200">{row.client_name}</span>
                  <span title={t('memory.injection.statsSessionTitle')}>{t('memory.injection.statsSessionStart', { count: row.session_injections })}</span>
                  <span title={t('memory.injection.statsPromptTitle')}>{t('memory.injection.statsPrompt', { count: row.prompt_injections })}</span>
                  <span title={t('memory.injection.statsSkippedTitle')}>{t('memory.injection.statsSkipped', { count: row.prompt_skips })}</span>
                  {gateRate && <span className="rounded bg-emerald-500/10 px-1.5 py-0.5 text-emerald-700 dark:text-emerald-300" title={t('memory.injection.statsGateTitle')}>{t('memory.injection.statsGateRate', { rate: gateRate })}</span>}
                  <span className="ml-auto font-mono tabular-nums text-slate-500 dark:text-slate-400" title={t('memory.injection.statsCharsTitle', { chars: row.injected_chars.toLocaleString() })}>≈{estTokens.toLocaleString()} tokens</span>
                </div>
              })}
            </div>
            <p className="mt-2 text-[11px] leading-4 text-slate-400">{t('memory.injection.statsNote')}</p>
          </div>
        )}
        <div className="mt-3 space-y-1.5">
          {mcpAccessLogs.length === 0
            ? <p className="rounded-md bg-slate-50 px-2.5 py-2 text-xs text-slate-500 dark:bg-slate-900/40 dark:text-slate-400">{t('memory.injection.summaryEmpty')}</p>
            : mcpAccessLogs.slice(0, logsExpanded ? mcpAccessLogs.length : SUMMARY_PREVIEW_COUNT).map((log) => {
              const expanded = expandedLogId === log.id
              return <div key={log.id} className="rounded-md bg-slate-50 dark:bg-slate-900/40">
                <div role="button" tabIndex={0} title={log.detail ? t('memory.injection.expandFull') : undefined}
                  onClick={() => { if (log.detail) setExpandedLogId(expanded ? null : log.id) }}
                  onKeyDown={(event) => { if (log.detail && (event.key === 'Enter' || event.key === ' ')) { event.preventDefault(); setExpandedLogId(expanded ? null : log.id) } }}
                  className={`flex flex-wrap items-center gap-x-2 gap-y-1 px-2.5 py-2 text-xs ${log.detail ? 'cursor-pointer transition hover:bg-slate-100 dark:hover:bg-slate-800/60' : ''}`}>
                  <span className="font-mono text-slate-400">{displayFullTime(log.occurred_at)}</span>
                  <span className="font-medium text-slate-700 dark:text-slate-200">{log.client_name}</span>
                  {log.tool_name === 'session_start_inject' ? <span className="inline-flex items-center gap-1 rounded bg-violet-500/10 px-1.5 py-0.5 text-violet-700 dark:text-violet-300"><Zap size={11} />{t('memory.injection.logL2')}</span> : log.tool_name === 'prompt_inject' ? <span className="inline-flex items-center gap-1 rounded bg-indigo-500/10 px-1.5 py-0.5 text-indigo-700 dark:text-indigo-300"><Zap size={11} />{t('memory.injection.logL3')}</span> : <code className="rounded bg-sky-500/10 px-1 py-0.5 text-sky-700 dark:text-sky-300">{log.tool_name}</code>}
                  <span className={`ml-auto ${log.success ? 'text-emerald-700 dark:text-emerald-300' : 'text-amber-700 dark:text-amber-300'}`}>{log.summary}</span>
                  {log.detail && <span className="shrink-0 text-[11px] text-slate-400">{expanded ? t('memory.injection.collapse') : t('memory.injection.detail')}</span>}
                </div>
                {expanded && log.detail && <pre className="mx-2.5 mb-2 max-h-72 overflow-auto whitespace-pre-wrap break-words rounded border border-slate-200 bg-white p-2.5 font-mono text-[11px] leading-5 text-slate-700 dark:border-slate-700 dark:bg-slate-950/40 dark:text-slate-200">{formatLogDetail(log.detail)}</pre>}
              </div>
            })}
            {mcpAccessLogs.length > SUMMARY_PREVIEW_COUNT && (
              <button type="button" onClick={() => setLogsExpanded(!logsExpanded)}
                className="mt-1 inline-flex items-center gap-1 rounded-md border border-slate-200 bg-white px-2.5 py-1.5 text-xs text-slate-600 transition hover:bg-slate-50 focus:outline-none focus-visible:ring-2 focus-visible:ring-blue-500 dark:border-slate-700 dark:bg-slate-900 dark:text-slate-300 dark:hover:bg-slate-800">
                {logsExpanded ? <ChevronUp size={13} /> : <ChevronDown size={13} />}
                {logsExpanded ? t('memory.injection.collapseAll') : t('memory.injection.expandAll', { count: mcpAccessLogs.length })}
              </button>
            )}
        </div>
      </section>
    </div>
  </main>
})
