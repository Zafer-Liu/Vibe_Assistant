import { memo, useCallback, useEffect, useMemo, useState } from 'react'
import { useTranslation } from 'react-i18next'
import { ArrowLeft, CheckCircle2, Loader2, MessagesSquare, RefreshCw } from 'lucide-react'
import { useMemoryStore } from '../store/memoryStore'
import type { MemoryConversationDetail, PendingMemorySession } from '../types/memory'
import { ConversationDialog, displayTime, sourceLabel } from '../components/ConversationDialog'
import { MemoryBreadcrumb } from '../components/MemoryBreadcrumb'

export const OrganizedConversations = memo(function OrganizedConversations({ onBack, active = true }: { onBack: () => void; active?: boolean }) {
  const { t } = useTranslation()
  const { loadOrganizedSessions, loadConversationDetail, checkTelemetry } = useMemoryStore()
  const [sessions, setSessions] = useState<PendingMemorySession[] | null>(null)
  const [sourceFilter, setSourceFilter] = useState<string>('all')
  const [refreshing, setRefreshing] = useState(false)
  const [detailKey, setDetailKey] = useState<string | null>(null)
  const [detail, setDetail] = useState<MemoryConversationDetail | null>(null)
  const [detailError, setDetailError] = useState<string | null>(null)

  const reload = useCallback(async () => {
    try {
      setSessions(await loadOrganizedSessions())
    } catch {
      setSessions([])
    }
  }, [loadOrganizedSessions])

  useEffect(() => {
    if (!active) return
    void reload()
    void checkTelemetry({ limit: 20 })
  }, [reload, checkTelemetry, active])

  const sourceCounts = useMemo(() => {
    const counts = new Map<string, number>()
    for (const item of sessions ?? []) counts.set(item.source, (counts.get(item.source) ?? 0) + 1)
    return [...counts.entries()].sort((a, b) => b[1] - a[1])
  }, [sessions])

  const visible = useMemo(
    () => (sessions ?? []).filter((item) => sourceFilter === 'all' || item.source === sourceFilter),
    [sessions, sourceFilter],
  )

  async function handleRefresh() {
    if (refreshing) return
    setRefreshing(true)
    try {
      await checkTelemetry({ limit: 20 })
      await reload()
    } finally {
      setRefreshing(false)
    }
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
          <button type="button" onClick={onBack} className="mt-0.5 inline-flex h-9 w-9 items-center justify-center rounded-lg text-slate-500 transition hover:bg-slate-100 hover:text-slate-900 focus:outline-none focus-visible:ring-2 focus-visible:ring-blue-500 dark:text-slate-400 dark:hover:bg-slate-800 dark:hover:text-white" aria-label={t('memory.organized.back')}><ArrowLeft size={19} /></button>
          <div>
            <MemoryBreadcrumb currentKey="memory.organized.title" onBack={onBack} />
            <h1 className="mt-1 text-2xl font-bold tracking-[-0.025em]">{t('memory.organized.title')}</h1>
            <p className="mt-1 text-sm text-slate-500 dark:text-slate-400">{t('memory.organized.desc')}</p>
          </div>
        </div>
        <button type="button" onClick={() => { void handleRefresh() }} disabled={refreshing} className="inline-flex h-10 items-center gap-2 rounded-lg border border-slate-200 bg-white px-3 text-sm font-medium text-slate-700 transition hover:border-slate-300 hover:bg-slate-50 disabled:cursor-wait disabled:opacity-60 focus:outline-none focus-visible:ring-2 focus-visible:ring-blue-500 dark:border-slate-700 dark:bg-slate-900 dark:text-slate-200 dark:hover:bg-slate-800"><RefreshCw size={16} className={refreshing ? 'animate-spin motion-reduce:animate-none' : ''} />{t('memory.organized.refresh')}</button>
      </header>

      <section className="mt-7" aria-label={t('memory.organized.overviewLabel')}>
        <div className="flex flex-wrap items-center gap-2">
          <button type="button" onClick={() => setSourceFilter('all')} aria-pressed={sourceFilter === 'all'} className={`inline-flex items-center gap-1.5 rounded-lg border px-3 py-1.5 text-xs font-medium transition focus:outline-none focus-visible:ring-2 focus-visible:ring-blue-500 ${sourceFilter === 'all' ? 'border-emerald-400 bg-emerald-50/60 text-emerald-700 dark:border-emerald-500/60 dark:bg-emerald-500/10 dark:text-emerald-300' : 'border-slate-200 bg-white text-slate-600 hover:border-slate-300 dark:border-slate-800 dark:bg-slate-900 dark:text-slate-300 dark:hover:border-slate-700'}`}>
            {t('memory.organized.all')} <span className="font-mono tabular-nums">{sessions === null ? '—' : sessions.length}</span>
          </button>
          {sourceCounts.map(([source, count]) => (
            <button key={source} type="button" onClick={() => setSourceFilter(sourceFilter === source ? 'all' : source)} aria-pressed={sourceFilter === source} className={`inline-flex items-center gap-1.5 rounded-lg border px-3 py-1.5 text-xs font-medium transition focus:outline-none focus-visible:ring-2 focus-visible:ring-blue-500 ${sourceFilter === source ? 'border-emerald-400 bg-emerald-50/60 text-emerald-700 dark:border-emerald-500/60 dark:bg-emerald-500/10 dark:text-emerald-300' : 'border-slate-200 bg-white text-slate-600 hover:border-slate-300 dark:border-slate-800 dark:bg-slate-900 dark:text-slate-300 dark:hover:border-slate-700'}`}>
              {sourceLabel(source)} <span className="font-mono tabular-nums">{count}</span>
            </button>
          ))}
        </div>
      </section>

      <section className="mt-6 rounded-2xl border border-slate-200 bg-white p-5 shadow-[0_1px_2px_rgba(15,23,42,0.03)] dark:border-slate-800 dark:bg-slate-900 sm:p-6" aria-label={t('memory.organized.listLabel')}>
        {sessions === null ? (
          <div className="flex min-h-48 items-center justify-center gap-2 text-sm text-slate-400"><Loader2 size={16} className="animate-spin motion-reduce:animate-none" />{t('memory.organized.loading')}</div>
        ) : visible.length === 0 ? (
          <div className="flex min-h-48 flex-col items-center justify-center text-center">
            <span className="grid h-11 w-11 place-items-center rounded-xl bg-slate-500/10 text-slate-400"><MessagesSquare size={20} /></span>
            <h2 className="mt-3 font-medium">{sourceFilter === 'all' ? t('memory.organized.emptyAll') : t('memory.organized.emptyFiltered', { source: sourceLabel(sourceFilter) })}</h2>
            <p className="mt-1 max-w-md text-sm text-slate-500 dark:text-slate-400">{t('memory.organized.emptyDesc')}</p>
          </div>
        ) : (
          <div className="divide-y divide-slate-100 dark:divide-slate-800">
            {visible.map((item) => (
              <div key={item.event_key} role="button" tabIndex={0} title={t('memory.organized.expandFull')}
                onClick={() => { void handleOpenDetail(item.event_key) }}
                onKeyDown={(event) => { if (event.key === 'Enter' || event.key === ' ') { event.preventDefault(); void handleOpenDetail(item.event_key) } }}
                className="group -mx-2 flex cursor-pointer flex-wrap items-center gap-x-3 gap-y-2 rounded-lg px-2 py-3.5 transition hover:bg-slate-50 focus:outline-none focus-visible:ring-2 focus-visible:ring-emerald-500 dark:hover:bg-slate-800/50">
                <span className="inline-flex shrink-0 items-center gap-1 rounded bg-emerald-500/10 px-1.5 py-0.5 text-xs text-emerald-700 dark:text-emerald-300"><CheckCircle2 size={12} />{t('memory.organized.storedBadge')}</span>
                <span className="shrink-0 text-sm font-medium">{sourceLabel(item.source)}</span>
                <span className="min-w-0 flex-1 truncate font-mono text-xs text-slate-400" title={item.session_id}>{item.session_id}</span>
                <span className="shrink-0 font-mono text-xs text-slate-400">{displayTime(item.occurred_at)}</span>
                <span className="shrink-0 text-xs text-slate-400">{item.message_count} {t('memory.organized.messageCount')}</span>
                <span className="inline-flex shrink-0 items-center gap-1 text-xs text-slate-300 transition group-hover:text-emerald-600 dark:text-slate-600 dark:group-hover:text-emerald-300"><MessagesSquare size={13} />{t('memory.organized.viewConversation')}</span>
                <p className="basis-full truncate text-xs text-slate-500 dark:text-slate-400" title={item.excerpt}>{item.excerpt.replace(/\s+/g, ' ').trim() || t('memory.organized.noExcerpt')}</p>
              </div>
            ))}
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
