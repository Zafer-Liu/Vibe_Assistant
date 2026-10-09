import { memo, useCallback, useEffect, useMemo, useState } from 'react'
import { useTranslation } from 'react-i18next'
import {
  ArrowLeft, Brain, CheckCircle2, Clock3, Loader2, MessagesSquare, RefreshCw, RotateCcw, XCircle, X,
} from 'lucide-react'
import { useMemoryStore } from '../store/memoryStore'
import type { MemoryConversationDetail, PendingMemorySession } from '../types/memory'
import { ConversationDialog, STATE_META, displayTime, sourceLabel } from '../components/ConversationDialog'
import { MemoryBreadcrumb } from '../components/MemoryBreadcrumb'
import { useVisiblePolling } from '../hooks/useVisiblePolling'
import { ErrorRecovery } from '../components/ErrorRecovery'

type StateFilter = 'all' | 'pending' | 'retrying' | 'failed'

export const PendingMemories = memo(function PendingMemories({ onBack, active = true }: { onBack: () => void; active?: boolean }) {
  const { t } = useTranslation()
  const { loadPendingSessions, loadConversationDetail, organizeSession, organizeConversations, cancelOrganize, ingestStatus, checkIngest, checkTelemetry } = useMemoryStore()
  const [sessions, setSessions] = useState<PendingMemorySession[] | null>(null)
  const [filter, setFilter] = useState<StateFilter>('all')
  const [refreshing, setRefreshing] = useState(false)
  const [organizingAll, setOrganizingAll] = useState(false)
  const [organizingKey, setOrganizingKey] = useState<string | null>(null)
  const [notice, setNotice] = useState<{ kind: 'ok' | 'err'; text: string } | null>(null)
  const [detailKey, setDetailKey] = useState<string | null>(null)
  const [detail, setDetail] = useState<MemoryConversationDetail | null>(null)
  const [detailError, setDetailError] = useState<string | null>(null)

  const reload = useCallback(async () => {
    try {
      setSessions(await loadPendingSessions())
    } catch {
      setSessions([])
    }
  }, [loadPendingSessions])

  useEffect(() => {
    if (!active) return
    void reload()
    void checkIngest()
    void checkTelemetry({ limit: 20 })
  }, [reload, checkIngest, checkTelemetry, active])
  useVisiblePolling(checkIngest, 2_000, active)

  const counts = useMemo(() => {
    const list = sessions ?? []
    return {
      pending: list.filter((item) => item.l1_state === 'pending').length,
      retrying: list.filter((item) => item.l1_state === 'retrying').length,
      failed: list.filter((item) => item.l1_state === 'failed').length,
    }
  }, [sessions])

  const visible = useMemo(
    () => (sessions ?? []).filter((item) => filter === 'all' || item.l1_state === filter),
    [sessions, filter],
  )

  const modelReady = Boolean(ingestStatus?.model_ready)
  const extractionRunning = organizingAll || organizingKey !== null || Boolean(ingestStatus?.manual_extraction_running)

  async function handleRefresh() {
    if (refreshing) return
    setRefreshing(true)
    try {
      await Promise.all([checkIngest(), checkTelemetry({ limit: 20 })])
      await reload()
    } finally {
      setRefreshing(false)
    }
  }

  async function handleOrganizeAll() {
    if (extractionRunning) return
    setOrganizingAll(true)
    setNotice(null)
    try {
      const result = await organizeConversations()
      setNotice(result.cancelled
        ? { kind: 'ok', text: t('memory.pending.interrupted', { succeeded: result.succeeded, failed: result.failed }) }
        : result.failed > 0
          ? { kind: 'err', text: t('memory.pending.organizeAllDonePartial', { succeeded: result.succeeded, failed: result.failed }) }
          : { kind: 'ok', text: t('memory.pending.organizeAllDone', { succeeded: result.succeeded }) })
      await reload()
    } catch (error) {
      setNotice({ kind: 'err', text: t('memory.pending.organizeFailed', { error: String(error) }) })
    } finally {
      setOrganizingAll(false)
    }
  }

  async function handleOrganizeOne(eventKey: string) {
    if (extractionRunning) return
    setOrganizingKey(eventKey)
    setNotice(null)
    try {
      const result = await organizeSession(eventKey)
      setNotice(result.cancelled
        ? { kind: 'ok', text: t('memory.pending.interrupted', { succeeded: result.succeeded, failed: result.failed }) }
        : result.failed > 0
          ? { kind: 'err', text: result.failure_reasons[0] ?? t('memory.pending.organizeOneFailed') }
          : { kind: 'ok', text: t('memory.pending.organizeOneDone') })
      await reload()
    } catch (error) {
      setNotice({ kind: 'err', text: t('memory.pending.organizeFailed', { error: String(error) }) })
    } finally {
      setOrganizingKey(null)
    }
  }

  async function handleCancelOrganize() {
    try { await cancelOrganize() } catch (error) { setNotice({ kind: 'err', text: t('memory.pending.organizeFailed', { error: String(error) }) }) }
  }

  async function handleOpenDetail(eventKey: string) {
    setDetailKey(eventKey)
    setDetail(null)
    setDetailError(null)
    try {
      setDetail(await loadConversationDetail(eventKey))
    } catch (error) {
      setDetailError(String(error))
    }
  }

  function handleCloseDetail() {
    setDetailKey(null)
    setDetail(null)
    setDetailError(null)
  }

  return <main className="h-full overflow-y-auto bg-[#fbfcfe] text-slate-900 dark:bg-slate-950 dark:text-slate-100">
    <div className="mx-auto max-w-[1200px] px-5 py-6 lg:px-9 lg:py-8">
      <header className="flex flex-wrap items-start justify-between gap-4">
        <div className="flex items-start gap-3">
          <button type="button" onClick={onBack} className="mt-0.5 inline-flex h-9 w-9 items-center justify-center rounded-lg text-slate-500 transition hover:bg-slate-100 hover:text-slate-900 focus:outline-none focus-visible:ring-2 focus-visible:ring-blue-500 dark:text-slate-400 dark:hover:bg-slate-800 dark:hover:text-white" aria-label={t('memory.pending.back')}><ArrowLeft size={19} /></button>
          <div>
            <MemoryBreadcrumb currentKey="memory.pending.title" onBack={onBack} />
            <h1 className="mt-1 text-2xl font-bold tracking-[-0.025em]">{t('memory.pending.title')}</h1>
            <p className="mt-1 text-sm text-slate-500 dark:text-slate-400">{t('memory.pending.desc')}</p>
          </div>
        </div>
        <div className="flex flex-wrap items-center gap-2">
          {notice && <span className={`rounded-lg px-3 py-2 text-xs ${notice.kind === 'ok' ? 'bg-emerald-500/10 text-emerald-700 dark:text-emerald-300' : 'bg-red-500/10 text-red-700 dark:text-red-300'}`} role="status">{notice.text}</span>}
          <button type="button" onClick={() => { void handleRefresh() }} disabled={refreshing} className="inline-flex h-10 items-center gap-2 rounded-lg border border-slate-200 bg-white px-3 text-sm font-medium text-slate-700 transition hover:border-slate-300 hover:bg-slate-50 disabled:cursor-wait disabled:opacity-60 focus:outline-none focus-visible:ring-2 focus-visible:ring-blue-500 dark:border-slate-700 dark:bg-slate-900 dark:text-slate-200 dark:hover:bg-slate-800"><RefreshCw size={16} className={refreshing ? 'animate-spin motion-reduce:animate-none' : ''} />{t('memory.pending.refresh')}</button>
          <button type="button" onClick={() => { void handleOrganizeAll() }} disabled={extractionRunning || !modelReady || (sessions ?? []).length === 0} title={modelReady ? t('memory.pending.organizeAllTitle') : t('memory.pending.modelMissingTitle')} className="inline-flex h-10 items-center gap-2 rounded-lg bg-violet-600 px-4 text-sm font-medium text-white transition hover:bg-violet-500 disabled:cursor-not-allowed disabled:opacity-50 focus:outline-none focus-visible:ring-2 focus-visible:ring-violet-500">{extractionRunning ? <Loader2 size={16} className="animate-spin motion-reduce:animate-none" /> : <Brain size={16} />}{extractionRunning ? t('memory.pending.organizing') : t('memory.pending.organizeAll')}</button>
          {ingestStatus?.manual_extraction_running && <button type="button" onClick={() => { void handleCancelOrganize() }} disabled={ingestStatus?.manual_extraction_cancelling} className="inline-flex h-10 items-center gap-2 rounded-lg border border-red-300 px-3 text-sm font-medium text-red-700 transition hover:bg-red-50 disabled:opacity-50 focus:outline-none focus-visible:ring-2 focus-visible:ring-red-500 dark:border-red-700 dark:text-red-300 dark:hover:bg-red-950/40"><X size={16} />{ingestStatus?.manual_extraction_cancelling ? t('memory.pending.interrupting') : t('memory.pending.interrupt')}</button>}
        </div>
      </header>
      {notice?.kind === 'err' && <ErrorRecovery error={notice.text} fallback="settings" />}

      <section className="mt-7 grid gap-3 sm:grid-cols-3" aria-label={t('memory.pending.overviewLabel')}>
        {(['pending', 'retrying', 'failed'] as const).map((state) => {
          const meta = STATE_META[state]
          const active = filter === state
          return <button key={state} type="button" onClick={() => setFilter(active ? 'all' : state)} aria-pressed={active} className={`rounded-xl border px-4 py-3 text-left transition focus:outline-none focus-visible:ring-2 focus-visible:ring-blue-500 ${active ? 'border-violet-400 bg-violet-50/60 dark:border-violet-500/60 dark:bg-violet-500/10' : 'border-slate-200 bg-white hover:border-slate-300 dark:border-slate-800 dark:bg-slate-900 dark:hover:border-slate-700'}`}>
            <div className="flex items-center gap-2 text-sm text-slate-500 dark:text-slate-400">
              {state === 'pending' ? <Clock3 size={15} className="text-violet-500" /> : state === 'retrying' ? <RotateCcw size={15} className="text-amber-500" /> : <XCircle size={15} className="text-red-500" />}
              {t(meta.labelKey)}
            </div>
            <p className="mt-2 font-mono text-2xl font-semibold tabular-nums">{sessions === null ? '—' : counts[state]}</p>
          </button>
        })}
      </section>

      {!modelReady && ingestStatus && (
        <div className="mt-4 rounded-lg bg-amber-500/10 px-3 py-2.5 text-xs text-amber-800 dark:text-amber-200">
          <p>{t('memory.pending.modelWarn')}</p>
          <ErrorRecovery fallback="settings" />
        </div>
      )}

      <section className="mt-6 rounded-2xl border border-slate-200 bg-white p-5 shadow-[0_1px_2px_rgba(15,23,42,0.03)] dark:border-slate-800 dark:bg-slate-900 sm:p-6" aria-label={t('memory.pending.listLabel')}>
        {sessions === null ? (
          <div className="flex min-h-48 items-center justify-center gap-2 text-sm text-slate-400"><Loader2 size={16} className="animate-spin motion-reduce:animate-none" />{t('memory.pending.loading')}</div>
        ) : visible.length === 0 ? (
          <div className="flex min-h-48 flex-col items-center justify-center text-center">
            <span className="grid h-11 w-11 place-items-center rounded-xl bg-emerald-500/10 text-emerald-500"><CheckCircle2 size={20} /></span>
            <h2 className="mt-3 font-medium">{filter === 'all' ? t('memory.pending.emptyAll') : t('memory.pending.emptyFiltered', { label: t(STATE_META[filter].labelKey) })}</h2>
            <p className="mt-1 max-w-md text-sm text-slate-500 dark:text-slate-400">{t('memory.pending.emptyDesc')}</p>
          </div>
        ) : (
          <div className="divide-y divide-slate-100 dark:divide-slate-800">
            {visible.map((item) => {
              const meta = STATE_META[item.l1_state] ?? STATE_META.pending
              const working = organizingKey === item.event_key
              return <div key={item.event_key} role="button" tabIndex={0} title={t('memory.dialog.expandFull')}
                onClick={() => { void handleOpenDetail(item.event_key) }}
                onKeyDown={(event) => { if (event.key === 'Enter' || event.key === ' ') { event.preventDefault(); void handleOpenDetail(item.event_key) } }}
                className="group -mx-2 flex cursor-pointer flex-wrap items-center gap-x-3 gap-y-2 rounded-lg px-2 py-3.5 transition hover:bg-slate-50 focus:outline-none focus-visible:ring-2 focus-visible:ring-violet-500 dark:hover:bg-slate-800/50">
                <span className={`inline-flex shrink-0 items-center rounded px-1.5 py-0.5 text-xs ${meta.badge}`}>{t(meta.labelKey)}</span>
                <span className="shrink-0 text-sm font-medium">{sourceLabel(item.source)}</span>
                <span className="min-w-0 flex-1 truncate font-mono text-xs text-slate-400" title={item.session_id}>{item.session_id}</span>
                <span className="shrink-0 font-mono text-xs text-slate-400">{displayTime(item.occurred_at)}</span>
                <span className="shrink-0 text-xs text-slate-400">{item.message_count} {t('memory.pending.messageCount')}</span>
                <span className="inline-flex shrink-0 items-center gap-1 text-xs text-slate-300 transition group-hover:text-violet-500 dark:text-slate-600 dark:group-hover:text-violet-300"><MessagesSquare size={13} />{t('memory.pending.viewConversation')}</span>
                <button type="button" onClick={(event) => { event.stopPropagation(); void handleOrganizeOne(item.event_key) }} disabled={extractionRunning || !modelReady} title={modelReady ? t('memory.pending.organizeOneTitle') : t('memory.pending.modelMissingTitle')} className="inline-flex shrink-0 items-center gap-1 rounded-md border border-violet-200 bg-white px-2.5 py-1 text-xs font-medium text-violet-700 transition hover:bg-violet-50 disabled:cursor-not-allowed disabled:opacity-50 dark:border-violet-500/30 dark:bg-slate-900 dark:text-violet-300 dark:hover:bg-violet-500/10">{working ? <Loader2 size={12} className="animate-spin motion-reduce:animate-none" /> : <Brain size={12} />}{t('memory.pending.organize')}</button>
                <p className="basis-full truncate text-xs text-slate-500 dark:text-slate-400" title={item.excerpt}>{item.excerpt.replace(/\s+/g, ' ').trim() || t('memory.pending.noExcerpt')}</p>
                {item.error && <p className="basis-full truncate text-xs text-red-600 dark:text-red-400" title={item.error}>{t('memory.pending.lastFailed')}{item.error}</p>}
              </div>
            })}
          </div>
        )}
      </section>

      {detailKey && <ConversationDialog
        detail={detail}
        error={detailError}
        onClose={handleCloseDetail}
      />}
    </div>
  </main>
})

