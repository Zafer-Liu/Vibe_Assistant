import { memo, useEffect, useState } from 'react'
import { useTranslation } from 'react-i18next'
import { open } from '@tauri-apps/plugin-dialog'
import { ArrowLeft, CheckCircle2, ChevronDown, ChevronRight, ChevronUp, FileText, FolderOpen, History, Loader2, RefreshCw, Webhook, X, XCircle, Zap } from 'lucide-react'
import { useMemoryFields } from '../store/memoryStore'
import { useTelemetryFields } from '../store/telemetryStore'
import { useVisiblePolling } from '../hooks/useVisiblePolling'
import { ErrorRecovery, type RecoveryTarget } from '../components/ErrorRecovery'
import type { HookStatus } from '../types/memory'
import { MemoryBreadcrumb } from '../components/MemoryBreadcrumb'
import { displayFullTime } from '../components/ConversationDialog'
import { isCaptureReady, isInjectionReady } from '../lib/memoryAgentStatus'

type HookAgentType = 'claude' | 'qoder' | 'codex' | 'workbuddy' | 'dsh'
const COLLECT_EVENTS = ['UserPromptSubmit', 'PostToolUse', 'Stop']
const collectEventCount = (hook: HookStatus | null) => hook?.events.filter(event => COLLECT_EVENTS.includes(event)).length ?? 0

export const MemoryCapture = memo(function MemoryCapture({ onBack, onOpenInjection, active = true }: { onBack: () => void; onOpenInjection: () => void; active?: boolean }) {
  const { t } = useTranslation()
  const { hookStatus, qoderHookStatus, codexHookStatus, workbuddyHookStatus, dshHookStatus, ingestStatus, agentSources, checkIngest, installHook, uninstallHook, setIngestEnabled, loadAgentSources, setAgentSourceOverride, setAgentSourceScanEnabled } = useMemoryFields('hookStatus', 'qoderHookStatus', 'codexHookStatus', 'workbuddyHookStatus', 'dshHookStatus', 'ingestStatus', 'agentSources', 'checkIngest', 'installHook', 'uninstallHook', 'setIngestEnabled', 'loadAgentSources', 'setAgentSourceOverride', 'setAgentSourceScanEnabled')
  const { telemetryEvents, checkTelemetry } = useTelemetryFields('telemetryEvents', 'checkTelemetry')
  const [hookAction, setHookAction] = useState<string | null>(null)
  const [scanAction, setScanAction] = useState<string | null>(null)
  const [captureToggleBusy, setCaptureToggleBusy] = useState(false)
  const [refreshing, setRefreshing] = useState(false)
  const [recordsOpen, setRecordsOpen] = useState(true)
  const [expandedReceiptId, setExpandedReceiptId] = useState<number | null>(null)
  const [notice, setNotice] = useState<{ kind: 'ok' | 'err'; text: string; recovery?: RecoveryTarget } | null>(null)

  useEffect(() => {
    if (active) void Promise.allSettled([loadAgentSources(), checkTelemetry({ limit: 50 })])
  }, [active, loadAgentSources, checkTelemetry])
  useVisiblePolling(checkIngest, 10_000, active)

  function flash(kind: 'ok' | 'err', text: string, recovery?: RecoveryTarget) {
    const next = { kind, text, recovery }
    setNotice(next)
    if (kind === 'ok') setTimeout(() => setNotice(current => current === next ? null : current), 3500)
  }

  async function handleRefresh() {
    if (refreshing) return
    setRefreshing(true)
    try { await Promise.all([checkIngest(), loadAgentSources(), checkTelemetry({ backfill: true, limit: 50 })]) }
    catch (error) { flash('err', String(error)) }
    finally { setRefreshing(false) }
  }

  async function handleToggleCapture() {
    if (!ingestStatus || captureToggleBusy) return
    setCaptureToggleBusy(true)
    try { await setIngestEnabled(!ingestStatus.enabled) }
    catch (error) { flash('err', t('memory.capture.toggleFailed', { error: String(error) })) }
    finally { setCaptureToggleBusy(false) }
  }

  const hookStatusByAgent: Record<string, HookStatus | null> = {
    codex: codexHookStatus, claude: hookStatus, qoder: qoderHookStatus, workbuddy: workbuddyHookStatus, dsh: dshHookStatus,
  }
  const readyCount = agentSources.filter(source => isCaptureReady(source, hookStatusByAgent[source.id])).length

  async function handleScanToggle(id: string, enabled: boolean) {
    if (scanAction) return
    setScanAction(id)
    try { await setAgentSourceScanEnabled(id, enabled) }
    catch (error) { flash('err', t('memory.capture.scanToggleFailed', { error: String(error) })) }
    finally { setScanAction(null) }
  }

  async function handleHookInstall(agentType: HookAgentType, label: string) {
    if (hookAction) return
    setHookAction(agentType)
    try { await installHook(agentType); flash('ok', t('memory.capture.hookInstalledToast', { label })) }
    catch (error) { flash('err', t('memory.capture.hookInstallFailed', { error: String(error) })) }
    finally { setHookAction(null) }
  }
  async function handleHookUninstall(agentType: HookAgentType, label: string) {
    if (hookAction) return
    setHookAction(agentType)
    try { await uninstallHook(agentType); flash('ok', t('memory.capture.hookRemovedToast', { label })) }
    catch (error) { flash('err', t('memory.capture.hookRemoveFailed', { error: String(error) })) }
    finally { setHookAction(null) }
  }
  async function handleChangeSourceDir(id: string) {
    const selected = await open({ directory: true, multiple: false, title: t('memory.capture.dirDialogTitle') })
    if (!selected || Array.isArray(selected)) return
    try { await setAgentSourceOverride(id, [selected], null); flash('ok', t('memory.capture.dirUpdated')) }
    catch (error) { flash('err', t('memory.capture.dirUpdateFailed', { error: String(error) })) }
  }
  async function handleResetSourceDir(id: string) {
    try { await setAgentSourceOverride(id, null, null); flash('ok', t('memory.capture.dirReset')) }
    catch (error) { flash('err', t('memory.capture.dirResetFailed', { error: String(error) })) }
  }

  return <main className="h-full overflow-y-auto bg-gray-50 text-slate-900 dark:bg-slate-950 dark:text-slate-100">
    <div className="mx-auto max-w-[1120px] px-5 py-6 lg:px-9 lg:py-8">
      <header className="fluent-page-header flex flex-wrap items-start justify-between gap-4 border-b border-slate-200 pb-5 dark:border-slate-800">
        <div className="flex items-start gap-3">
          <button type="button" onClick={onBack} className="mt-0.5 inline-flex h-9 w-9 items-center justify-center rounded-lg text-slate-500 transition hover:bg-slate-100 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-violet-500 dark:hover:bg-slate-800" aria-label={t('memory.capture.back')}><ArrowLeft size={19} /></button>
          <div><MemoryBreadcrumb currentKey="memory.capture.title" onBack={onBack} /><h1 className="mt-1 text-2xl font-semibold tracking-tight">{t('memory.capture.title')}</h1><p className="mt-1 max-w-2xl text-sm leading-6 text-slate-500 dark:text-slate-400">{t('memory.capture.desc')}</p></div>
        </div>
        <div className="flex flex-wrap items-center gap-2">
          {notice && <span className={`rounded-lg px-3 py-2 text-xs ${notice.kind === 'ok' ? 'bg-emerald-500/10 text-emerald-700 dark:text-emerald-300' : 'bg-red-500/10 text-red-700 dark:text-red-300'}`} role="status">{notice.text}</span>}
          <button type="button" onClick={() => { void handleRefresh() }} disabled={refreshing} className="inline-flex h-9 items-center gap-2 rounded-lg border border-slate-200 bg-white px-3 text-sm font-medium text-slate-700 transition hover:bg-slate-50 disabled:opacity-60 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-violet-500 dark:border-slate-700 dark:bg-slate-900 dark:text-slate-200 dark:hover:bg-slate-800"><RefreshCw size={15} className={refreshing ? 'animate-spin motion-reduce:animate-none' : ''} />{t('memory.capture.refreshStatus')}</button>
        </div>
      </header>
      {notice?.kind === 'err' && <ErrorRecovery error={notice.text} fallback={notice.recovery} onRetry={() => { void handleRefresh() }} disabled={refreshing} />}
      {ingestStatus && !ingestStatus.model_ready && <ErrorRecovery fallback="settings" />}
      <div className="mt-6 flex flex-wrap items-center gap-x-6 gap-y-2 border-b border-slate-200 pb-5 text-sm dark:border-slate-800" aria-label={t('memory.capture.overviewLabel')}>
        <span className="text-slate-500 dark:text-slate-400">{t('memory.capture.captureCard')}</span>
        <strong className="font-mono text-lg tabular-nums">{agentSources.length ? `${readyCount} / ${agentSources.length}` : '—'}</strong>
        <span className="text-xs text-slate-500 dark:text-slate-400">{t('memory.capture.captureCardHint')}</span>
        {ingestStatus && <span className="text-xs text-slate-500 dark:text-slate-400">{t('memory.capture.bufferedCount', { count: ingestStatus.buffered_sessions })}</span>}
        <button type="button" onClick={onOpenInjection} className="ml-auto inline-flex items-center gap-1 text-xs font-medium text-violet-700 hover:underline focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-violet-500 dark:text-violet-300">{t('memory.capture.openInjection')}<ChevronRight size={13} /></button>
      </div>
      <section className="mt-6 rounded-xl border border-slate-200 bg-white p-5 dark:border-slate-800 dark:bg-slate-900 sm:p-6" aria-label={t('memory.capture.captureLabel')}>
        <div className="flex flex-wrap items-center gap-3">
          <span className="grid h-9 w-9 shrink-0 place-items-center rounded-xl bg-sky-500/10 text-sky-600 dark:text-sky-300"><Webhook size={18} /></span>
          <div className="min-w-0 flex-1">
            <h2 className="text-sm font-semibold text-slate-700 dark:text-slate-200">{t('memory.capture.captureTitle')}</h2>
            <p className="mt-0.5 text-xs text-slate-500 dark:text-slate-400">{t('memory.capture.captureDesc')}</p>
          </div>
          <div className="flex shrink-0 items-center gap-2">
            {ingestStatus?.enabled
              ? <span className="inline-flex items-center gap-1 text-xs text-emerald-600 dark:text-emerald-400"><CheckCircle2 size={12} />{t('memory.capture.autoCaptureOn')}</span>
              : <span className="inline-flex items-center gap-1 text-xs text-slate-500"><XCircle size={12} />{t('memory.capture.autoCaptureOff')}</span>}
            <button
              type="button"
              onClick={() => { void handleToggleCapture() }}
              disabled={!ingestStatus || captureToggleBusy}
              className={`relative h-5 w-9 rounded-full transition-colors disabled:cursor-wait disabled:opacity-50 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-sky-500 ${ingestStatus?.enabled ? 'bg-sky-600' : 'bg-slate-300 dark:bg-slate-600'}`}
              title={ingestStatus?.enabled ? t('memory.capture.autoCaptureDisable') : t('memory.capture.autoCaptureEnable')}
              aria-pressed={Boolean(ingestStatus?.enabled)}
              aria-label={ingestStatus?.enabled ? t('memory.capture.autoCaptureDisable') : t('memory.capture.autoCaptureEnable')}
            >
              <span className="absolute top-0.5 h-4 w-4 rounded-full bg-white transition-all" style={{ left: ingestStatus?.enabled ? '18px' : '2px' }} />
            </button>
          </div>
        </div>

        <div className={`mt-3 flex items-center gap-1.5 text-xs ${ingestStatus?.model_ready ? 'text-emerald-700 dark:text-emerald-400' : 'text-amber-800 dark:text-amber-300'}`}>
          {ingestStatus?.model_ready ? <CheckCircle2 size={13} /> : <XCircle size={13} />}
          {ingestStatus?.model_ready ? t('memory.capture.modelReady', { provider: ingestStatus.model_provider_id ?? t('memory.capture.modelConfigured') }) : t('memory.capture.modelMissing')}
        </div>

        <div className="mt-5 divide-y divide-slate-100 border-t border-slate-100 dark:divide-slate-800 dark:border-slate-800">
          {agentSources.map((source) => {
            const hook = hookStatusByAgent[source.id]
            const supportsHook = source.supports_hooks
            const roots = source.transcript_roots
            const primary = roots[0]
            const missing = roots.every((root) => !root.exists)
            const working = hookAction === source.id
            return <div key={source.id} className="py-3.5">
              <div className="flex flex-wrap items-center gap-2">
                <span className="text-sm font-medium text-slate-700 dark:text-slate-200">{source.label}</span>
                {supportsHook ? (
                  !hook
                    ? <span className="text-xs text-slate-500">{t('memory.capture.statusChecking')}</span>
                    : hook.installed && (collectEventCount(hook) > 0 || isInjectionReady(hook))
                    ? <span className="inline-flex items-center gap-1 rounded-full bg-emerald-500/10 px-2 py-1 text-xs text-emerald-600 dark:text-emerald-400" title={t('memory.capture.captureEventsTitle', { events: hook.events.filter((event) => COLLECT_EVENTS.includes(event) || event === 'agent/pre-step').join(' / ') || t('memory.capture.captureEventsNone') })}><CheckCircle2 size={12} />{collectEventCount(hook) > 0 ? t('memory.capture.hookInstalled', { count: collectEventCount(hook) }) : t('memory.capture.pluginInstalled')}</span>
                    : hook.installed
                      ? <span className="inline-flex items-center gap-1 rounded-full bg-amber-500/10 px-2 py-1 text-xs text-amber-700 dark:text-amber-300"><XCircle size={12} />{t('memory.capture.hookNeedsRepair')}</span>
                    : <span className="inline-flex items-center gap-1 rounded-full bg-slate-100 px-2 py-1 text-xs text-slate-500 dark:bg-slate-700 dark:text-slate-400"><XCircle size={12} />{t('memory.capture.hookNotInstalled')}</span>
                ) : null}
                {supportsHook && hook && (
                  hook.installed && (collectEventCount(hook) > 0 || isInjectionReady(hook))
                    ? <button onClick={() => { void handleHookUninstall(source.id as HookAgentType, source.label) }} disabled={working} className="ml-auto inline-flex shrink-0 items-center gap-1 rounded-md border border-slate-300 px-2.5 py-1 text-xs text-slate-600 hover:bg-slate-50 disabled:opacity-50 dark:border-slate-600 dark:text-slate-300 dark:hover:bg-slate-700">{working ? <Loader2 size={13} className="animate-spin motion-reduce:animate-none" /> : <X size={13} />}{t('memory.capture.uninstallHook')}</button>
                    : <button onClick={() => { void handleHookInstall(source.id as HookAgentType, source.label) }} disabled={working} className="ml-auto inline-flex shrink-0 items-center gap-1 rounded-md bg-sky-600 px-2.5 py-1 text-xs font-medium text-white transition-colors hover:bg-sky-500 disabled:opacity-50">{working ? <Loader2 size={13} className="animate-spin motion-reduce:animate-none" /> : <Zap size={13} />}{t(hook.installed ? 'memory.capture.repairHook' : 'memory.capture.installHook')}</button>
                )}
              </div>
              <div className="mt-2 flex flex-wrap items-center gap-2 text-xs">
                <span className="text-slate-500 dark:text-slate-400">{t('memory.capture.localScan')}</span>
                {source.supports_transcript_scan ? <>
                  <span className={source.scan_enabled && !missing ? 'text-emerald-700 dark:text-emerald-300' : 'text-slate-500 dark:text-slate-400'}>{t(source.scan_enabled ? (missing ? 'memory.capture.scanMissing' : 'memory.capture.scanEnabled') : 'memory.capture.scanDisabled')}</span>
                  <button type="button" role="switch" aria-checked={source.scan_enabled} aria-label={t('memory.capture.localScanFor', { label: source.label })} onClick={() => { void handleScanToggle(source.id, !source.scan_enabled) }} disabled={scanAction !== null} className={`relative ml-auto h-5 w-9 rounded-full transition-colors disabled:opacity-50 ${source.scan_enabled ? 'bg-sky-600' : 'bg-slate-300 dark:bg-slate-600'}`}><span className="absolute top-0.5 h-4 w-4 rounded-full bg-white transition-all" style={{ left: source.scan_enabled ? '18px' : '2px' }} /></button>
                </> : <span className="text-slate-500">{t('memory.capture.scanUnavailable')}</span>}
              </div>
              <div className="mt-1.5 flex flex-wrap items-center gap-2 text-xs">
                <span className="shrink-0 text-slate-500">{t('memory.capture.dataDir')}</span>
                <span className={`min-w-0 flex-1 truncate font-mono ${missing ? 'text-amber-600 dark:text-amber-400' : 'text-slate-500 dark:text-slate-400'}`} title={roots.map((root) => root.path).join('\n')}>
                  {primary ? primary.path : t('memory.capture.dataDirUnset')}{roots.length > 1 ? t('memory.capture.dataDirCount', { count: roots.length }) : ''}{missing ? t('memory.capture.dataDirMissing') : ''}
                </span>
                {source.supports_transcript_scan && primary?.is_override && <button onClick={() => { void handleResetSourceDir(source.id) }} className="shrink-0 rounded border border-slate-300 px-2 py-0.5 text-[11px] text-slate-600 hover:bg-slate-100 dark:border-slate-600 dark:text-slate-300 dark:hover:bg-slate-700">{t('memory.capture.resetDefault')}</button>}
                {source.supports_transcript_scan && <button onClick={() => { void handleChangeSourceDir(source.id) }} className="inline-flex shrink-0 items-center gap-1 rounded border border-sky-200 bg-white px-2 py-0.5 text-[11px] font-medium text-sky-700 hover:bg-sky-50 dark:border-sky-500/30 dark:bg-slate-800 dark:text-sky-300 dark:hover:bg-sky-500/10"><FolderOpen size={11} />{t('memory.capture.change')}</button>}
              </div>
            </div>
          })}
        </div>

        {ingestStatus && ingestStatus.buffered_sessions > 0 && (
          <p className="mt-3 rounded-md bg-amber-500/10 px-2.5 py-2 text-xs text-amber-800 dark:text-amber-200">
            {ingestStatus.model_ready ? t('memory.capture.buffered', { count: ingestStatus.buffered_sessions }) : t('memory.capture.bufferedModelUnavailable', { count: ingestStatus.buffered_sessions })}
          </p>
        )}

      </section>

      <section className="mt-6 rounded-xl border border-slate-200 bg-white p-5 dark:border-slate-800 dark:bg-slate-900 sm:p-6" aria-label={t('memory.capture.recordsLabel')}>
        <h2 className="text-sm font-semibold text-slate-700 dark:text-slate-200">{t('memory.capture.recordsLabel')}</h2>
        <p className="mt-1 text-xs text-slate-500 dark:text-slate-400">{t('memory.capture.recordsHint')}</p>
        {ingestStatus && ingestStatus.recent.length > 0 ? (
          <div className="scrollbar-slim mt-4 max-h-72 divide-y divide-slate-100 overflow-y-auto border-t border-slate-100 dark:divide-slate-800 dark:border-slate-800">
              {ingestStatus.recent.map((log, i) => (
                <div key={`${log.at}-${log.state}-${log.detail}-${i}`} className="flex min-w-0 items-start gap-2 py-2 text-xs text-slate-600 dark:text-slate-300">
                  <span className="shrink-0 font-mono text-slate-500">{log.at}</span>
                  <span className={`shrink-0 rounded px-1.5 ${log.state === 'retrying' ? 'bg-amber-500/10 text-amber-700 dark:text-amber-300' : log.state === 'working' ? 'bg-sky-500/10 text-sky-700 dark:text-sky-300' : log.state === 'failed' ? 'bg-red-500/10 text-red-700 dark:text-red-300' : 'bg-violet-500/10 text-violet-600 dark:text-violet-400'}`}>
                    {log.state === 'retrying' ? t('memory.capture.recordRetrying') : log.state === 'working' ? t('memory.capture.recordWorking') : log.state === 'failed' ? t('memory.capture.recordFailed') : t('memory.capture.recordStored')}
                  </span>
                  <span className="min-w-0 flex-1 break-words" title={log.detail}>{log.detail}</span>
                </div>
              ))}
          </div>
        ) : <p className="mt-4 border-t border-slate-100 pt-4 text-xs text-slate-500 dark:border-slate-800 dark:text-slate-400">{t('memory.capture.recordsEmpty')}</p>}
      </section>

      <section className="mt-6 rounded-xl border border-slate-200 bg-white p-5 dark:border-slate-800 dark:bg-slate-900 sm:p-6" aria-label={t('memory.recordsTitle')}>
        {/* 已接收的原始 Hook 事件保存在遥测记录中。 */}
        <div>
          <div className="flex flex-wrap items-center gap-2">
            <History size={14} className="text-violet-600 dark:text-violet-400" />
            <h3 className="text-xs font-semibold text-gray-700 dark:text-gray-200">{t('memory.recordsTitle')}</h3>
            <span className="text-xs text-gray-500 dark:text-gray-400">{t('memory.recordsHint')}</span>
            <button type="button" onClick={() => setRecordsOpen((open) => !open)} aria-expanded={recordsOpen} className="ml-auto inline-flex items-center gap-1 rounded-md px-2 py-1 text-xs font-medium text-gray-600 hover:bg-gray-100 dark:text-gray-300 dark:hover:bg-gray-700">
              {recordsOpen ? t('memory.ingestCollapse') : t('memory.recordsView')}{recordsOpen ? <ChevronUp size={14} /> : <ChevronDown size={14} />}
            </button>
          </div>
          {recordsOpen && <>
          {telemetryEvents.length === 0 ? (
            <div className="py-6 text-center">
              <p className="text-sm text-gray-600 dark:text-gray-300">{t('memory.recordsEmpty')}</p>
              <p className="mt-1 text-xs text-gray-500 dark:text-gray-400">{t('memory.recordsEmptyHint')}</p>
            </div>
          ) : (
            <div className="mt-3 max-h-72 divide-y divide-gray-100 overflow-y-auto rounded-lg border border-gray-200 dark:divide-gray-700 dark:border-gray-700">
              {telemetryEvents.map((event) => {
                const tokenTotal = event.input_tokens + event.output_tokens + event.cached_tokens
                const hasConversation = Boolean(event.conversation_text)
                const expanded = expandedReceiptId === event.id
                const conversationState = event.conversation_state === 'full'
                  ? t('memory.recordConversationFull', { count: event.conversation_message_count })
                  : event.conversation_state === 'partial'
                    ? t('memory.recordConversationPartial')
                    : event.conversation_state === 'covered'
                      ? t('memory.recordConversationCovered')
                    : t('memory.recordConversationUnavailable')
                return <div key={event.id} className="px-3 py-2.5 text-xs">
                  <div className="grid grid-cols-[minmax(0,1fr)_auto] gap-x-3 gap-y-1">
                    <div className="min-w-0"><div className="flex flex-wrap items-center gap-1.5"><span className="rounded bg-violet-500/10 px-1.5 py-0.5 font-medium text-violet-700 dark:text-violet-300">{event.source}</span><span className="text-gray-700 dark:text-gray-200">{event.event_type}</span><span className="font-mono text-gray-500">{event.session_id}</span></div><p className="mt-1 truncate text-gray-500 dark:text-gray-400">{displayFullTime(event.occurred_at)}</p></div>
                    <span className={event.token_source === 'reported' ? 'self-center font-mono text-gray-700 dark:text-gray-200' : 'self-center text-gray-500'}>{event.token_source === 'reported' ? `${tokenTotal.toLocaleString()} tokens` : t('memory.tokenUnavailable')}</span>
                  </div>
                  {event.event_type === 'Stop' && <div className={`mt-2 rounded-md px-2.5 py-2 ${event.conversation_state === 'full' ? 'bg-emerald-500/10 text-emerald-800 dark:text-emerald-200' : 'bg-gray-100 text-gray-600 dark:bg-gray-700/60 dark:text-gray-300'}`}>
                    <div className="flex items-center gap-1.5"><FileText size={13} aria-hidden="true" /><span className="font-medium">{conversationState}</span>{hasConversation && <button type="button" onClick={() => setExpandedReceiptId(expanded ? null : event.id)} aria-expanded={expanded} className="ml-auto inline-flex items-center gap-1 rounded px-1 py-0.5 font-medium hover:bg-black/5 focus:outline-none focus:ring-1 focus:ring-violet-500 dark:hover:bg-white/10">{expanded ? t('memory.recordConversationCollapse') : t('memory.recordConversationExpand')}{expanded ? <ChevronUp size={13} /> : <ChevronDown size={13} />}</button>}</div>
                    {hasConversation && !expanded && <p className="mt-1.5 max-h-10 overflow-hidden whitespace-pre-wrap text-gray-700 dark:text-gray-200">{event.conversation_text}</p>}
                    {hasConversation && expanded && <pre className="mt-2 max-h-72 overflow-auto whitespace-pre-wrap rounded border border-black/10 bg-white/70 p-2.5 font-sans text-xs leading-5 text-gray-800 dark:border-white/10 dark:bg-gray-900/40 dark:text-gray-100">{event.conversation_text}</pre>}
                  </div>}
                </div>
              })}
            </div>
          )}
          </>}
        </div>
      </section>

    </div>
  </main>
})
