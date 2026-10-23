import { memo, useEffect, useState } from 'react'
import { useTranslation } from 'react-i18next'
import { ArrowLeft, CheckCircle2, ChevronDown, ChevronRight, ChevronUp, Gauge, Loader2, RefreshCw, Server, Zap } from 'lucide-react'
import { useMcpFields } from '../store/mcpStore'
import { useMemoryFields } from '../store/memoryStore'
import { useVisiblePolling } from '../hooks/useVisiblePolling'
import { ErrorRecovery, type RecoveryTarget } from '../components/ErrorRecovery'
import type { HookStatus } from '../types/memory'
import type { MemoryMcpTarget } from '../types/mcp'
import { displayFullTime } from '../components/ConversationDialog'
import { MemoryBreadcrumb } from '../components/MemoryBreadcrumb'
import { isInjectionReady } from '../lib/memoryAgentStatus'

type HookAgentType = 'claude' | 'qoder' | 'codex' | 'workbuddy' | 'dsh'
const HOOK_AGENTS: { id: HookAgentType; label: string }[] = [
  { id: 'codex', label: 'Codex' }, { id: 'claude', label: 'Claude Code' },
  { id: 'qoder', label: 'Qoder' }, { id: 'workbuddy', label: 'WorkBuddy' },
  { id: 'dsh', label: 'DeepSeek Harness' },
]
function formatLogDetail(detail: string) {
  try { return JSON.stringify(JSON.parse(detail), null, 2) } catch { return detail }
}

export const MemoryInjection = memo(function MemoryInjection({ onBack, onOpenCapture, active = true }: { onBack: () => void; onOpenCapture: () => void; active?: boolean }) {
  const { t } = useTranslation()
  const { hookStatus, qoderHookStatus, codexHookStatus, workbuddyHookStatus, dshHookStatus, agentSources, memoryInjectionLogs, memoryInjectionStats, checkIngest, installHook, loadAgentSources, loadMemoryInjectionLogs, loadInjectionStats, setAgentSourceScanEnabled } = useMemoryFields('hookStatus', 'qoderHookStatus', 'codexHookStatus', 'workbuddyHookStatus', 'dshHookStatus', 'agentSources', 'memoryInjectionLogs', 'memoryInjectionStats', 'checkIngest', 'installHook', 'loadAgentSources', 'loadMemoryInjectionLogs', 'loadInjectionStats', 'setAgentSourceScanEnabled')
  const { memoryMcp, checkMemoryMcp, installMemoryMcp, uninstallMemoryMcp } = useMcpFields('memoryMcp', 'checkMemoryMcp', 'installMemoryMcp', 'uninstallMemoryMcp')
  const [hookAction, setHookAction] = useState<string | null>(null)
  const [mcpAction, setMcpAction] = useState<string | null>(null)
  const [scanAction, setScanAction] = useState<string | null>(null)
  const [refreshing, setRefreshing] = useState(false)
  const [expandedLogId, setExpandedLogId] = useState<number | null>(null)
  const [logsExpanded, setLogsExpanded] = useState(false)
  const [notice, setNotice] = useState<{ kind: 'ok' | 'err'; text: string; recovery?: RecoveryTarget } | null>(null)
  const SUMMARY_PREVIEW_COUNT = 5

  useEffect(() => { if (active) { void loadAgentSources(); void checkMemoryMcp().catch(() => {}) } }, [active, loadAgentSources, checkMemoryMcp])
  useVisiblePolling(checkIngest, 30_000, active)
  useVisiblePolling(loadMemoryInjectionLogs, 10_000, active)
  useVisiblePolling(loadInjectionStats, 30_000, active)

  function flash(kind: 'ok' | 'err', text: string, recovery?: RecoveryTarget) {
    const next = { kind, text, recovery }
    setNotice(next)
    if (kind === 'ok') setTimeout(() => setNotice(current => current === next ? null : current), 3500)
  }
  async function handleRefresh() {
    if (refreshing) return
    setRefreshing(true)
    try { await Promise.all([checkIngest(), loadAgentSources(), loadMemoryInjectionLogs(), loadInjectionStats()]) }
    catch (error) { flash('err', String(error)) }
    finally { setRefreshing(false) }
  }
  const hookStatusByAgent: Record<string, HookStatus | null> = {
    codex: codexHookStatus, claude: hookStatus, qoder: qoderHookStatus, workbuddy: workbuddyHookStatus,
    dsh: dshHookStatus,
  }
  // agentSources 的 id 与共享记忆 MCP 的目标 id 并不一致，按来源映射。
  // Claude 的 CLI 与 Desktop 是两份独立配置，分别显示；Codex 两端共用 config.toml，只显示一个真实状态。
  const mcpTargetsBySource: Record<string, string[]> = {
    codex: ['codex_cli'], claude: ['claude_cli', 'claude_desktop'], qoder: ['qoder'], workbuddy: ['workbuddy'],
    minimax: ['minimax'], kimi: ['kimi'], zcode: ['zcode'], dsh: ['dsh'],
  }
  const mcpTargetLabel: Record<string, string> = {
    claude_cli: 'Claude Code', claude_desktop: 'Claude Desktop', codex_cli: 'Codex',
    qoder: 'Qoder', workbuddy: 'WorkBuddy', minimax: 'MiniMax Code', kimi: 'Kimi', zcode: 'ZCode', dsh: 'DeepSeek Harness',
  }
  const hookLogs = memoryInjectionLogs.filter(log => log.tool_name === 'session_start_inject' || log.tool_name === 'prompt_inject')
  const mcpLogs = memoryInjectionLogs.filter(log => log.tool_name !== 'session_start_inject' && log.tool_name !== 'prompt_inject')
  const statusLoaded = Object.values(hookStatusByAgent).some(Boolean)
  const injectEnabled = (hook: HookStatus | null) => isInjectionReady(hook)
  const readyCount = HOOK_AGENTS.filter(agent => injectEnabled(hookStatusByAgent[agent.id])).length
  const scanReadyCount = agentSources.filter(source => source.scan_enabled && source.transcript_roots.some(root => root.exists)).length
  async function handleHookInstall(agentType: HookAgentType, label: string) {
    if (hookAction) return
    setHookAction(agentType)
    try { await installHook(agentType); flash('ok', t('memory.injection.injectEnabledToast', { label })) }
    catch (error) { flash('err', t('memory.injection.hookInstallFailed', { error: String(error) })) }
    finally { setHookAction(null) }
  }
  async function handleScanToggle(id: string, enabled: boolean) {
    if (scanAction) return
    setScanAction(id)
    try { await setAgentSourceScanEnabled(id, enabled) }
    catch (error) { flash('err', t('memory.capture.scanToggleFailed', { error: String(error) })) }
    finally { setScanAction(null) }
  }
  async function handleMcpToggle(target: string, label: string, install: boolean) {
    if (mcpAction) return
    setMcpAction(target)
    try {
      if (install) await installMemoryMcp(target as MemoryMcpTarget)
      else await uninstallMemoryMcp(target as MemoryMcpTarget)
      await checkMemoryMcp().catch(() => {})
      flash('ok', t(install ? 'mcpLibrary.memoryMcpInstalledToast' : 'mcpLibrary.memoryMcpRemovedToast', { label }))
    } catch (error) {
      flash('err', t('mcpLibrary.memoryMcpActionFailed', { error: String(error) }))
    } finally {
      setMcpAction(null)
    }
  }

  return <main className="h-full overflow-y-auto bg-gray-50 text-slate-900 dark:bg-slate-950 dark:text-slate-100">
    <div className="mx-auto max-w-[1120px] px-5 py-6 lg:px-9 lg:py-8">
      <header className="fluent-page-header flex flex-wrap items-start justify-between gap-4 border-b border-slate-200 pb-5 dark:border-slate-800">
        <div className="flex items-start gap-3">
          <button type="button" onClick={onBack} className="mt-0.5 inline-flex h-9 w-9 items-center justify-center rounded-lg text-slate-500 transition hover:bg-slate-100 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-violet-500 dark:hover:bg-slate-800" aria-label={t('memory.injection.back')}><ArrowLeft size={19} /></button>
          <div><MemoryBreadcrumb currentKey="memory.injection.title" onBack={onBack} /><h1 className="mt-1 text-2xl font-semibold tracking-tight">{t('memory.injection.title')}</h1><p className="mt-1 max-w-2xl text-sm leading-6 text-slate-500 dark:text-slate-400">{t('memory.injection.desc')}</p></div>
        </div>
        <div className="flex flex-wrap items-center gap-2">
          {notice && <span className={`rounded-lg px-3 py-2 text-xs ${notice.kind === 'ok' ? 'bg-emerald-500/10 text-emerald-700 dark:text-emerald-300' : 'bg-red-500/10 text-red-700 dark:text-red-300'}`} role="status">{notice.text}</span>}
          <button type="button" onClick={() => { void handleRefresh() }} disabled={refreshing} className="inline-flex h-9 items-center gap-2 rounded-lg border border-slate-200 bg-white px-3 text-sm font-medium text-slate-700 transition hover:bg-slate-50 disabled:opacity-60 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-violet-500 dark:border-slate-700 dark:bg-slate-900 dark:text-slate-200 dark:hover:bg-slate-800"><RefreshCw size={15} className={refreshing ? 'animate-spin motion-reduce:animate-none' : ''} />{t('memory.injection.refreshStatus')}</button>
        </div>
      </header>
      {notice?.kind === 'err' && <ErrorRecovery error={notice.text} fallback={notice.recovery} onRetry={() => { void handleRefresh() }} disabled={refreshing} />}
      <div className="mt-6 flex flex-wrap items-center gap-x-6 gap-y-2 border-b border-slate-200 pb-5 text-sm dark:border-slate-800" aria-label={t('memory.injection.overviewLabel')}>
        <span className="text-slate-500 dark:text-slate-400">{t('memory.injection.injectCard')}</span>
        <strong className="font-mono text-lg tabular-nums">{statusLoaded ? `${readyCount} / ${HOOK_AGENTS.length}` : '—'}</strong>
        <span className="text-xs text-slate-500 dark:text-slate-400">{t('memory.injection.injectCardHint', { count: HOOK_AGENTS.length })}</span>
        <span className="text-xs text-slate-500 dark:text-slate-400">{t('memory.injection.scanReady', { count: scanReadyCount, total: agentSources.length })}</span>
        <button type="button" onClick={onOpenCapture} className="ml-auto inline-flex items-center gap-1 text-xs font-medium text-violet-700 hover:underline focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-violet-500 dark:text-violet-300">{t('memory.injection.openCapture')}<ChevronRight size={13} /></button>
      </div>
      <section className="mt-6 rounded-xl border border-slate-200 bg-white p-5 dark:border-slate-800 dark:bg-slate-900 sm:p-6" aria-label={t('memory.injection.injectLabel')}>
        <div className="flex flex-wrap items-center gap-3">
          <span className="grid h-9 w-9 shrink-0 place-items-center rounded-xl bg-violet-500/10 text-violet-600 dark:text-violet-300"><Zap size={18} /></span>
          <div className="min-w-0 flex-1">
            <h2 className="text-sm font-semibold text-slate-700 dark:text-slate-200">{t('memory.injection.injectTitle')}</h2>
            <p className="mt-0.5 text-xs text-slate-500 dark:text-slate-400">{t('memory.injection.injectDesc')}</p>
          </div>
        </div>

        <div className="mt-5 border-t border-slate-100 pt-4 dark:border-slate-800">
          <div className="flex flex-wrap items-center gap-2">
            <Zap size={14} className="text-violet-600 dark:text-violet-300" />
            <h3 className="text-xs font-semibold text-slate-700 dark:text-slate-200">{t('memory.injection.hookInjectTitle')}</h3>
            <span className="text-[11px] text-slate-500 dark:text-slate-400">{t('memory.injection.hookInjectBadge')}</span>
          </div>
          <div className="mt-3 grid gap-2 border-t border-slate-100 pt-3 dark:border-slate-800">
            {agentSources.map(source => {
              const hook = hookStatusByAgent[source.id]
              const enabled = injectEnabled(hook)
              const working = hookAction === source.id
              const scanAvailable = source.supports_transcript_scan && source.transcript_roots.some(root => root.exists)
              return <div key={source.id} className="grid gap-2 rounded-lg border border-slate-100 p-3 dark:border-slate-800 md:grid-cols-[minmax(110px,1fr)_minmax(0,1.7fr)_minmax(0,1.7fr)] md:items-center">
                <span className="text-sm font-medium text-slate-700 dark:text-slate-200">{source.label}</span>
                <div className="flex min-w-0 flex-wrap items-center gap-2 text-xs"><span className="font-medium text-violet-700 dark:text-violet-300">{t('memory.injection.hookColumn')}</span>
                  {!source.supports_hooks ? <span className="text-slate-500">{t('memory.injection.hookUnavailable')}</span> : !hook ? <span className="text-slate-500">{t('memory.injection.statusChecking')}</span> : enabled ? <span className="inline-flex items-center gap-1 text-emerald-700 dark:text-emerald-300"><CheckCircle2 size={12} />{t('memory.injection.injectEnabled')}</span> : <><span className="text-slate-500 dark:text-slate-400">{t('memory.injection.injectDisabled')}</span><button type="button" onClick={() => { void handleHookInstall(source.id as HookAgentType, source.label) }} disabled={working} className="rounded-md bg-violet-600 px-2 py-1 font-medium text-white hover:bg-violet-500 disabled:opacity-50">{working ? <Loader2 size={12} className="animate-spin motion-reduce:animate-none" /> : t('memory.injection.enableInject')}</button></>}
                </div>
                <div className="flex min-w-0 flex-wrap items-center gap-2 text-xs"><span className="font-medium text-sky-700 dark:text-sky-300">{t('memory.injection.scanColumn')}</span>
                  {source.supports_transcript_scan ? <><span className={source.scan_enabled && scanAvailable ? 'text-emerald-700 dark:text-emerald-300' : 'text-slate-500 dark:text-slate-400'}>{t(source.scan_enabled ? (scanAvailable ? 'memory.capture.scanEnabled' : 'memory.capture.scanMissing') : 'memory.capture.scanDisabled')}</span><button type="button" role="switch" aria-checked={source.scan_enabled} aria-label={t('memory.capture.localScanFor', { label: source.label })} onClick={() => { void handleScanToggle(source.id, !source.scan_enabled) }} disabled={scanAction !== null} className={`relative ml-auto h-5 w-9 rounded-full transition-colors disabled:opacity-50 ${source.scan_enabled ? 'bg-sky-600' : 'bg-slate-300 dark:bg-slate-600'}`}><span className="absolute top-0.5 h-4 w-4 rounded-full bg-white transition-all" style={{ left: source.scan_enabled ? '18px' : '2px' }} /></button></> : <span className="text-slate-500">{t('memory.capture.scanUnavailable')}</span>}
                </div>
              </div>
            })}
          </div>
          <p className="mt-2 text-[11px] leading-4 text-slate-500 dark:text-slate-400">{t('memory.injection.hookInjectNote')}</p>
        </div>

        <div className="mt-5 border-t border-slate-100 pt-4 dark:border-slate-800">
          <div className="flex flex-wrap items-center gap-2">
            <Server size={14} className="text-indigo-600 dark:text-indigo-300" />
            <h3 className="text-xs font-semibold text-slate-700 dark:text-slate-200">{t('memory.injection.mcpInjectTitle')}</h3>
            <span className="text-[11px] text-slate-500 dark:text-slate-400">{t('memory.injection.mcpInjectBadge')}</span>
          </div>
          <div className="mt-3 grid gap-2 border-t border-slate-100 pt-3 dark:border-slate-800">
            {agentSources.map(source => {
              const mcpTargets = mcpTargetsBySource[source.id] ?? []
              return <div key={source.id} className="grid gap-2 rounded-lg border border-slate-100 p-3 dark:border-slate-800 md:grid-cols-[minmax(110px,1fr)_minmax(0,3.4fr)] md:items-center">
                <span className="text-sm font-medium text-slate-700 dark:text-slate-200">{source.label}</span>
                <div className="flex min-w-0 flex-col gap-1.5 text-xs">
                  {mcpTargets.length === 0 && <span className="text-slate-500">{t('memory.injection.mcpUnavailable')}</span>}
                  {mcpTargets.map(mcpTarget => {
                    const mcpConfigured = memoryMcp[mcpTarget as keyof typeof memoryMcp]?.installed ?? false
                    return <div key={mcpTarget} className="flex min-w-0 flex-wrap items-center gap-2">
                      <span className="text-slate-500 dark:text-slate-400">{mcpTargetLabel[mcpTarget]}</span>
                      {mcpConfigured
                        ? <>
                            <span className="inline-flex items-center gap-1 text-emerald-700 dark:text-emerald-300"><CheckCircle2 size={12} />{t('memory.injection.mcpReady')}</span>
                            <button type="button" onClick={() => { void handleMcpToggle(mcpTarget, mcpTargetLabel[mcpTarget], false) }} disabled={mcpAction !== null} className="ml-auto rounded-md border border-slate-300 px-2 py-1 font-medium text-slate-600 hover:bg-slate-50 disabled:opacity-50 dark:border-slate-600 dark:text-slate-300 dark:hover:bg-slate-700">{mcpAction === mcpTarget ? <Loader2 size={12} className="animate-spin motion-reduce:animate-none" /> : t('memory.injection.mcpDisable')}</button>
                          </>
                        : <>
                            <span className="text-slate-500 dark:text-slate-400">{t('memory.injection.mcpNotReady')}</span>
                            <button type="button" onClick={() => { void handleMcpToggle(mcpTarget, mcpTargetLabel[mcpTarget], true) }} disabled={mcpAction !== null} className="ml-auto rounded-md bg-indigo-600 px-2 py-1 font-medium text-white hover:bg-indigo-500 disabled:opacity-50">{mcpAction === mcpTarget ? <Loader2 size={12} className="animate-spin motion-reduce:animate-none" /> : t('memory.injection.mcpEnable')}</button>
                          </>}
                    </div>
                  })}
                </div>
              </div>
            })}
          </div>
          <p className="mt-2 text-[11px] leading-4 text-slate-500 dark:text-slate-400">{t('memory.injection.mcpInjectNote')}</p>
        </div>

      </section>

      <section className="mt-6 rounded-xl border border-slate-200 bg-white p-5 dark:border-slate-800 dark:bg-slate-900 sm:p-6" aria-label={t('memory.injection.summaryLabel')}>
        <h2 className="text-sm font-semibold text-slate-700 dark:text-slate-200">{t('memory.injection.summaryTitle')}</h2>
        <p className="mt-1 text-xs text-slate-500 dark:text-slate-400">{t('memory.injection.summaryDesc')}</p>
        {memoryInjectionStats.length > 0 ? (
          <div className="mt-5 border-t border-slate-100 pt-4 dark:border-slate-800" aria-label={t('memory.injection.statsLabel')}>
            <div className="flex items-center gap-1.5 text-xs font-semibold text-slate-600 dark:text-slate-300">
              <Gauge size={13} className="text-indigo-500" />
              {t('memory.injection.statsTitle')}
            </div>
            <div className="mt-2 divide-y divide-slate-100 dark:divide-slate-800">
              {memoryInjectionStats.map((row) => {
                const promptTurns = row.prompt_injections + row.prompt_skips
                const gateRate = promptTurns > 0 ? `${Math.round((row.prompt_skips / promptTurns) * 100)}%` : null
                const estTokens = Math.round(row.injected_chars / 3)
                return <div key={row.client_name} className="flex flex-wrap items-center gap-x-3 gap-y-1 py-2 text-xs text-slate-600 dark:text-slate-300">
                  <span className="min-w-28 font-medium text-slate-700 dark:text-slate-200">{row.client_name}</span>
                  <span title={t('memory.injection.statsSessionTitle')}>{t('memory.injection.statsSessionStart', { count: row.session_injections })}</span>
                  <span title={t('memory.injection.statsPromptTitle')}>{t('memory.injection.statsPrompt', { count: row.prompt_injections })}</span>
                  <span title={t('memory.injection.statsSkippedTitle')}>{t('memory.injection.statsSkipped', { count: row.prompt_skips })}</span>
                  {gateRate && <span className="rounded bg-emerald-500/10 px-1.5 py-0.5 text-emerald-700 dark:text-emerald-300" title={t('memory.injection.statsGateTitle')}>{t('memory.injection.statsGateRate', { rate: gateRate })}</span>}
                  <span className="ml-auto font-mono tabular-nums text-slate-500 dark:text-slate-400" title={t('memory.injection.statsCharsTitle', { chars: row.injected_chars.toLocaleString() })}>≈{estTokens.toLocaleString()} tokens</span>
                </div>
              })}
            </div>
            <p className="mt-2 text-[11px] leading-4 text-slate-500">{t('memory.injection.statsNote')}</p>
          </div>
        ) : <p className="mt-5 border-t border-slate-100 pt-4 text-xs text-slate-500 dark:border-slate-800 dark:text-slate-400">{t('memory.injection.statsEmpty')}</p>}
        {([
          { key: 'hook', title: t('memory.injection.hookLogTitle'), logs: hookLogs },
          { key: 'mcp', title: t('memory.injection.mcpLogTitle'), logs: mcpLogs },
        ] as const).map(({ key, title, logs }) => (
          <div key={key} className="mt-6">
            <h3 className="text-xs font-semibold text-slate-600 dark:text-slate-300">{title}</h3>
            <div className="mt-3 divide-y divide-slate-100 border-t border-slate-100 dark:divide-slate-800 dark:border-slate-800">
              {logs.length === 0
                ? <p className="rounded-md bg-slate-50 px-2.5 py-2 text-xs text-slate-500 dark:bg-slate-900/40 dark:text-slate-400">{t('memory.injection.summaryEmpty')}</p>
                : logs.slice(0, logsExpanded ? logs.length : SUMMARY_PREVIEW_COUNT).map((log) => {
                  const expanded = expandedLogId === log.id
                  return <div key={log.id}>
                    <div role="button" tabIndex={0} title={log.detail ? t('memory.injection.expandFull') : undefined}
                      onClick={() => { if (log.detail) setExpandedLogId(expanded ? null : log.id) }}
                      onKeyDown={(event) => { if (log.detail && (event.key === 'Enter' || event.key === ' ')) { event.preventDefault(); setExpandedLogId(expanded ? null : log.id) } }}
                      className={`flex flex-wrap items-center gap-x-2 gap-y-1 px-2.5 py-2 text-xs ${log.detail ? 'cursor-pointer transition hover:bg-slate-100 dark:hover:bg-slate-800/60' : ''}`}>
                      <span className="font-mono text-slate-500">{displayFullTime(log.occurred_at)}</span>
                      <span className="font-medium text-slate-700 dark:text-slate-200">{log.client_name}</span>
                      {log.tool_name === 'session_start_inject' ? <span className="inline-flex items-center gap-1 rounded bg-violet-500/10 px-1.5 py-0.5 text-violet-700 dark:text-violet-300"><Zap size={11} />{t('memory.injection.logL2')}</span> : log.tool_name === 'prompt_inject' ? <span className="inline-flex items-center gap-1 rounded bg-indigo-500/10 px-1.5 py-0.5 text-indigo-700 dark:text-indigo-300"><Zap size={11} />{t('memory.injection.logL3')}</span> : <code className="rounded bg-sky-500/10 px-1 py-0.5 text-sky-700 dark:text-sky-300">{log.tool_name}</code>}
                      <span className={`ml-auto ${log.success ? 'text-emerald-700 dark:text-emerald-300' : 'text-amber-700 dark:text-amber-300'}`}>{log.summary}</span>
                      {log.detail && <span className="shrink-0 text-[11px] text-slate-500">{expanded ? t('memory.injection.collapse') : t('memory.injection.detail')}</span>}
                    </div>
                    {expanded && log.detail && <pre className="mx-2.5 mb-2 max-h-72 overflow-auto whitespace-pre-wrap break-words rounded border border-slate-200 bg-white p-2.5 font-mono text-[11px] leading-5 text-slate-700 dark:border-slate-700 dark:bg-slate-950/40 dark:text-slate-200">{formatLogDetail(log.detail)}</pre>}
                  </div>
                })}
              {logs.length > SUMMARY_PREVIEW_COUNT && (
                <button type="button" onClick={() => setLogsExpanded(!logsExpanded)}
                  className="mt-1 inline-flex items-center gap-1 rounded-md border border-slate-200 bg-white px-2.5 py-1.5 text-xs text-slate-600 transition hover:bg-slate-50 focus:outline-none focus-visible:ring-2 focus-visible:ring-blue-500 dark:border-slate-700 dark:bg-slate-900 dark:text-slate-300 dark:hover:bg-slate-800">
                  {logsExpanded ? <ChevronUp size={13} /> : <ChevronDown size={13} />}
                  {logsExpanded ? t('memory.injection.collapseAll') : t('memory.injection.expandAll', { count: logs.length })}
                </button>
              )}
            </div>
          </div>
        ))}
      </section>
    </div>
  </main>
})
