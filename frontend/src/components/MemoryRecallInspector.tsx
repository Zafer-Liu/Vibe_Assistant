import { useCallback, useEffect, useId, useMemo, useRef, useState, type ReactNode } from 'react'
import { invoke } from '@tauri-apps/api/core'
import { useTranslation } from 'react-i18next'
import { AlertTriangle, Loader2, RefreshCw, X } from 'lucide-react'
import { Modal } from './Modal'
import { MemoryEvaluationPanel } from './MemoryEvaluationPanel'
import { MemoryEvidencePanel } from './MemoryEvidencePanel'
import type { EvidenceAccess } from '../types/memoryEvidence'
import { displayFullTime, sourceLabel } from './ConversationDialog'
import { intlLocale } from '../i18n'
import type { MemoryRetrievalResponse } from '../types/memory'
import type { RecallTrace, RecallTraceDetail, RecallTraceSummary, TraceMemoryView, TraceRerun } from '../types/memoryRecallTrace'

const ns = 'memory.recallTrace.'
const buttonClass = 'inline-flex min-h-8 items-center justify-center gap-1.5 rounded-[6px] border border-[var(--app-control-stroke)] bg-[var(--app-surface)] px-2.5 py-1.5 text-xs font-medium text-[var(--app-text)] hover:bg-[var(--app-hover)] focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-[var(--app-focus)] disabled:cursor-not-allowed disabled:opacity-50'
const mutedClass = 'text-xs leading-5 text-[var(--app-secondary)]'
const disclosureClass = 'cursor-pointer rounded-[6px] py-2 text-sm font-semibold focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-[var(--app-focus)]'
const cellClass = 'border-b border-[var(--app-stroke)] px-3 py-2 text-left align-top'

type Section = 'overview' | 'candidates' | 'sources' | 'evaluation'
interface Comparison {
  before: RecallTrace
  after: RecallTrace | null
  requestId: string | null
  loading: boolean
  error: string | null
}

function readableError(error: unknown): string {
  if (error instanceof Error) return error.message
  if (typeof error === 'string') return error
  if (error && typeof error === 'object') {
    for (const key of ['message', 'error', 'detail']) {
      const value = (error as Record<string, unknown>)[key]
      if (typeof value === 'string') return value
    }
    try { return JSON.stringify(error) } catch { /* Use the representation below. */ }
  }
  return String(error)
}

function number(value: number | null | undefined): string {
  return value == null || !Number.isFinite(value) ? '—' : value.toLocaleString(intlLocale(), { maximumSignificantDigits: 6 })
}

function Field({ label, children }: { label: string; children: ReactNode }) {
  return <div className="grid min-w-0 gap-1 sm:grid-cols-[9rem_minmax(0,1fr)] sm:gap-3">
    <dt className={mutedClass}>{label}</dt>
    <dd className="min-w-0 whitespace-pre-wrap break-words text-sm [overflow-wrap:anywhere]">{children}</dd>
  </div>
}

function Loading({ label }: { label: string }) {
  return <p role="status" className={`flex items-center gap-2 py-3 ${mutedClass}`}><Loader2 size={15} className="shrink-0 animate-spin motion-reduce:animate-none" aria-hidden="true" />{label}</p>
}

function Failure({ label, error, onRetry }: { label: string; error: string; onRetry?: () => void }) {
  const { t } = useTranslation()
  return <div role="alert" className="space-y-2 rounded-[6px] bg-red-50 p-3 text-sm text-red-800 dark:bg-red-950/40 dark:text-red-200">
    <p className="font-medium">{label}</p>
    <p className="whitespace-pre-wrap break-words text-xs [overflow-wrap:anywhere]">{error}</p>
    {onRetry && <button type="button" className={buttonClass} onClick={onRetry}>{t(`${ns}retry`)}</button>}
  </div>
}

function TruncatedNotice() {
  const { t } = useTranslation()
  return <p role="note" className="flex items-start gap-2 rounded-[6px] bg-amber-50 p-3 text-xs leading-5 text-amber-900 dark:bg-amber-950/40 dark:text-amber-200"><AlertTriangle size={15} className="mt-0.5 shrink-0" aria-hidden="true" />{t(`${ns}truncated`)}</p>
}

function CurrentMemory({ view }: { view: TraceMemoryView | undefined }) {
  const { t } = useTranslation()
  if (!view) return <p className={mutedClass}>{t(`${ns}currentUnavailable`)}</p>
  const statusClass = view.state === 'changed' ? 'bg-amber-50 text-amber-900 dark:bg-amber-950/40 dark:text-amber-200'
    : view.state === 'deleted' ? 'bg-red-50 text-red-800 dark:bg-red-950/40 dark:text-red-200'
      : 'bg-[var(--app-selected)] text-[var(--app-secondary)]'
  return <div className="min-w-0 space-y-3">
    <div className="flex flex-wrap items-center gap-2">
      <h5 className="text-sm font-semibold">{t(`${ns}currentBody`)}</h5>
      <span className={`rounded px-2 py-0.5 text-xs ${statusClass}`}>{t(`${ns}state_${view.state}`)}</span>
    </div>
    <p className={mutedClass}>{t(`${ns}currentBodyHint`)}</p>
    {view.state === 'deleted' ? <p className={mutedClass}>{t(`${ns}deletedBody`)}</p>
      : <p className="max-h-64 overflow-y-auto whitespace-pre-wrap break-words text-sm leading-6 [overflow-wrap:anywhere]">{view.content ?? t(`${ns}currentUnavailable`)}</p>}
    {view.state !== 'deleted' && <details>
      <summary className={disclosureClass}>{t(`${ns}currentMetadata`)}</summary>
      <dl className="space-y-2 py-2">
        <Field label={t(`${ns}domains`)}>{view.domains.join(' · ') || t(`${ns}none`)}</Field>
        <Field label={t(`${ns}sourceAgent`)}>{view.source_agent ? sourceLabel(view.source_agent) : t(`${ns}none`)}</Field>
        <Field label={t(`${ns}sessionId`)}>{view.session_id || t(`${ns}none`)}</Field>
        <Field label={t(`${ns}eventKey`)}>{view.source_event_key || t(`${ns}none`)}</Field>
      </dl>
    </details>}
  </div>
}

function Overview({ trace }: { trace: RecallTrace }) {
  const { t } = useTranslation()
  const timings = trace.retrieval.timings_ms
  return <div className="space-y-6">
    <div>
      <h3 className="mb-3 text-base font-semibold">{t(`${ns}query`)}</h3>
      <p className="max-w-[75ch] whitespace-pre-wrap break-words text-sm leading-6 [overflow-wrap:anywhere]">{trace.arguments.query}</p>
    </div>
    <dl className="space-y-3">
      {trace.query !== trace.arguments.query && <Field label={t(`${ns}executedQuery`)}>{trace.query}</Field>}
      <Field label={t(`${ns}requestId`)}>{trace.request_id}</Field>
      <Field label={t(`${ns}createdAt`)}>{displayFullTime(trace.created_at)}</Field>
      <Field label={t(`${ns}entrypoint`)}>{trace.entrypoint}</Field>
      <Field label={t(`${ns}requestedMode`)}>{t(`${ns}domainMode_${trace.arguments.domain_mode}`)}</Field>
      <Field label={t(`${ns}requestedDomains`)}>{trace.arguments.domains?.join(' · ') || t(`${ns}noneSpecified`)}</Field>
      <Field label={t(`${ns}actualDomains`)}>{trace.routing.active_domains?.join(' · ') || t(`${ns}globalScope`)}</Field>
      <Field label={t(`${ns}actualMode`)}>{trace.routing.mode || t(`${ns}unknown`)}</Field>
      <Field label={t(`${ns}routingConfidence`)}>{number(trace.routing.confidence)}</Field>
      <Field label={t(`${ns}routingFallback`)}>{trace.routing.fallback || t(`${ns}noFallback`)}</Field>
      <Field label={t(`${ns}resultLimit`)}>{number(trace.arguments.limit)}</Field>
      <Field label={t(`${ns}retrievalMode`)}>{trace.retrieval.mode || t(`${ns}unknown`)}</Field>
      <Field label={t(`${ns}candidateCounts`)}>{t(`${ns}candidateCountsValue`, {
        semantic: number(trace.retrieval.semantic_candidates), keyword: number(trace.retrieval.local_keyword_candidates),
        unique: number(trace.retrieval.unique_candidates), discarded: number(trace.retrieval.discarded_candidates),
      })}</Field>
      {trace.retrieval.keyword_expansion && <Field label={t(`${ns}keywordExpansion`)}>{t(`${ns}keywordExpansionValue`, {
        searched: trace.retrieval.keyword_expansion.searched, added: trace.retrieval.keyword_expansion.added_candidates,
      })}</Field>}
      <Field label={t(`${ns}indexCoverage`)}>{trace.retrieval.index ? t(`${ns}indexCoverageValue`, {
        cached: number(trace.retrieval.index.cached), total: number(trace.retrieval.index.total), pending: number(trace.retrieval.index.pending),
      }) : t(`${ns}unknown`)}</Field>
      <Field label={t(`${ns}outputChars`)}>{number(trace.retrieval.injected_chars)}</Field>
      <Field label={t(`${ns}requestBudget`)}>{trace.retrieval.budget_ms == null ? t(`${ns}unknown`) : t(`${ns}milliseconds`, { value: number(trace.retrieval.budget_ms) })}</Field>
      <Field label={t(`${ns}rankingVersion`)}>{trace.retrieval.ranking_version || t(`${ns}unknown`)}</Field>
    </dl>
    <section>
      <h3 className="mb-3 text-base font-semibold">{t(`${ns}timings`)}</h3>
      <dl className="space-y-2 tabular-nums">
        {(['routing', 'keyword', 'semantic', 'keyword_expansion', 'selection', 'total'] as const).map(stage => <Field key={stage} label={t(`${ns}stage_${stage}`)}>{timings?.[stage] == null ? t(`${ns}unknown`) : t(`${ns}milliseconds`, { value: number(timings[stage]) })}</Field>)}
      </dl>
    </section>
    <details>
      <summary className={disclosureClass}>{t(`${ns}arguments`)}</summary>
      <pre className="mt-2 overflow-x-auto rounded-[6px] bg-[var(--app-layer)] p-3 text-xs leading-5">{JSON.stringify(trace.arguments, null, 2)}</pre>
    </details>
  </div>
}

function Candidates({ trace }: { trace: RecallTrace }) {
  const { t } = useTranslation()
  return <div className="space-y-5">
    <section>
      <h3 className="text-base font-semibold">{t(`${ns}finalResults`, { count: trace.results.length })}</h3>
      {trace.results.length === 0 ? <p className={`py-4 ${mutedClass}`}>{t(`${ns}noResults`)}</p> : <ol className="mt-3 divide-y divide-[var(--app-stroke)]">
        {trace.results.map((result, index) => <li key={result.memory_id} className="flex min-w-0 gap-3 py-3">
          <span className="shrink-0 text-sm font-semibold tabular-nums">{t(`${ns}rankNumber`, { rank: index + 1 })}</span>
          <div className="min-w-0 space-y-1">
            <p className="break-all text-sm">{result.memory_id}</p>
            <p className={mutedClass}>{result.domains.join(' · ') || t(`${ns}none`)}</p>
          </div>
        </li>)}
      </ol>}
    </section>
    <details>
      <summary className={disclosureClass}>{t(`${ns}nominations`, { count: trace.candidates.length })}</summary>
      <p className={`mb-3 ${mutedClass}`}>{t(`${ns}nominationsHint`)}</p>
      {trace.candidates.length === 0 ? <p className={mutedClass}>{t(`${ns}noNominations`)}</p> : <div role="region" aria-label={t(`${ns}nominationsTable`)} tabIndex={0} className="max-h-80 overflow-auto rounded-[6px] border border-[var(--app-stroke)] focus-visible:outline-2 focus-visible:outline-[var(--app-focus)]">
        <table className="w-full min-w-[610px] text-xs tabular-nums">
          <thead className="sticky top-0 bg-[var(--app-layer)]"><tr>{['memoryId', 'route', 'inputRank', 'eligibleRank', 'rawScore', 'nominationStatus'].map(key => <th key={key} scope="col" className={cellClass}>{t(`${ns}${key}`)}</th>)}</tr></thead>
          <tbody>{trace.candidates.map((candidate, index) => <tr key={`${candidate.route}-${candidate.memory_id}-${index}`}>
            <th scope="row" className={`${cellClass} max-w-44 break-all font-normal`}>{candidate.memory_id}</th>
            <td className={cellClass}>{t(`${ns}route_${candidate.route}`)}</td>
            <td className={cellClass}>{number(candidate.input_rank)}</td>
            <td className={cellClass}>{number(candidate.eligible_rank)}</td>
            <td className={cellClass}>{number(candidate.score)}</td>
            <td className={cellClass}>{t(`${ns}nomination_${candidate.status}`)}</td>
          </tr>)}</tbody>
        </table>
      </div>}
    </details>
    <details>
      <summary className={disclosureClass}>{t(`${ns}ranking`, { count: trace.ranking.length })}</summary>
      <p className={`mb-3 ${mutedClass}`}>{t(`${ns}rankingHint`)}</p>
      {trace.ranking.length === 0 ? <p className={mutedClass}>{t(`${ns}noRanking`)}</p> : <div role="region" aria-label={t(`${ns}rankingTable`)} tabIndex={0} className="max-h-80 overflow-auto rounded-[6px] border border-[var(--app-stroke)] focus-visible:outline-2 focus-visible:outline-[var(--app-focus)]">
        <table className="w-full min-w-[1060px] text-xs tabular-nums">
          <thead className="sticky top-0 bg-[var(--app-layer)]"><tr>{['memoryId', 'domains', 'semanticRank', 'keywordRank', 'semanticScore', 'keywordScore', 'fusedScore', 'diversityPenalty', 'selectionScore', 'selectedRank', 'outcome'].map(key => <th key={key} scope="col" className={cellClass}>{t(`${ns}${key}`)}</th>)}</tr></thead>
          <tbody>{trace.ranking.map(candidate => <tr key={candidate.memory_id}>
            <th scope="row" className={`${cellClass} max-w-44 break-all font-normal`}>{candidate.memory_id}</th>
            <td className={`${cellClass} max-w-44 break-words [overflow-wrap:anywhere]`}>{candidate.domains.join(' · ') || t(`${ns}none`)}</td>
            {[candidate.semantic_rank, candidate.keyword_rank, candidate.semantic_score, candidate.keyword_score, candidate.fused_score, candidate.diversity_penalty, candidate.selection_score, candidate.selected_rank].map((value, index) => <td key={index} className={cellClass}>{number(value)}</td>)}
            <td className={cellClass}>{t(`${ns}outcome_${candidate.outcome}`)}</td>
          </tr>)}</tbody>
        </table>
      </div>}
    </details>
  </div>
}

function ComparisonView({ comparison, onRetry, onShowBefore }: { comparison: Comparison; onRetry: () => void; onShowBefore: () => void }) {
  const { t } = useTranslation()
  const changes = useMemo(() => {
    if (!comparison.after) return []
    const before = new Map(comparison.before.results.map((result, index) => [result.memory_id, { result, rank: index + 1 }]))
    const after = new Map(comparison.after.results.map((result, index) => [result.memory_id, { result, rank: index + 1 }]))
    return [...new Set([...before.keys(), ...after.keys()])].map(id => ({ id, before: before.get(id), after: after.get(id) }))
      .filter(item => !item.before || !item.after || item.before.rank !== item.after.rank || item.before.result.fingerprint !== item.after.result.fingerprint)
  }, [comparison.before, comparison.after])
  return <details open className="space-y-3 border-b border-[var(--app-stroke)] pb-5">
    <summary className={`${disclosureClass} text-base`}>{t(`${ns}comparison`)}</summary>
    <dl className="space-y-2">
      <Field label={t(`${ns}beforeRequest`)}>{comparison.before.request_id}</Field>
      <Field label={t(`${ns}afterRequest`)}>{comparison.requestId || t(`${ns}noNewRequestId`)}</Field>
    </dl>
    <button type="button" className={buttonClass} onClick={onShowBefore}>{t(`${ns}showBefore`)}</button>
    {comparison.loading ? <Loading label={t(`${ns}comparisonLoading`)} />
      : comparison.error ? <Failure label={t(`${ns}comparisonFailed`)} error={comparison.error} onRetry={onRetry} />
        : !comparison.after && <p role="status" className={mutedClass}>{t(`${ns}comparisonUnavailable`)}</p>}
    {!comparison.loading && !comparison.error && !comparison.after && comparison.requestId && <button type="button" className={buttonClass} onClick={onRetry}>{t(`${ns}reloadComparison`)}</button>}
    {comparison.after && <>
      <p className={mutedClass}>{t(`${ns}comparisonHint`)}</p>
      {(comparison.before.truncated || comparison.after.truncated) && <TruncatedNotice />}
      {changes.length === 0 ? <p role="status" className={mutedClass}>{t(`${ns}noChanges`)}</p> : <ul className="divide-y divide-[var(--app-stroke)]">
        {changes.map(item => <li key={item.id} className="space-y-1 py-2">
          <p className="break-all text-sm">{item.id}</p>
          <div className={`flex flex-wrap gap-x-4 gap-y-1 tabular-nums ${mutedClass}`}>
            {!item.before ? <span>{t(`${ns}added`, { rank: item.after?.rank })}</span> : !item.after ? <span>{t(`${ns}removed`, { rank: item.before.rank })}</span> : <>
              {item.before.rank !== item.after.rank && <span>{t(`${ns}rankChanged`, { before: item.before.rank, after: item.after.rank })}</span>}
              {item.before.result.fingerprint !== item.after.result.fingerprint && <span>{t(`${ns}fingerprintChanged`)}</span>}
            </>}
          </div>
          {item.before && item.after && item.before.result.fingerprint !== item.after.result.fingerprint && <dl className="space-y-1">
            <Field label={t(`${ns}beforeFingerprint`)}>{item.before.result.fingerprint}</Field>
            <Field label={t(`${ns}afterFingerprint`)}>{item.after.result.fingerprint}</Field>
          </dl>}
        </li>)}
      </ul>}
    </>}
  </details>
}

export function MemoryRecallInspector({ initialRequestId, evidenceAccess, onClose, onRerun }: { initialRequestId?: string; evidenceAccess?: EvidenceAccess | null; onClose: () => void; onRerun: TraceRerun }) {
  const { t } = useTranslation()
  const id = useId()
  const [history, setHistory] = useState<RecallTraceSummary[]>([])
  const [historyLoading, setHistoryLoading] = useState(true)
  const [historyError, setHistoryError] = useState<string | null>(null)
  const [selectedId, setSelectedId] = useState<string | null>(initialRequestId || null)
  const [detail, setDetail] = useState<RecallTraceDetail | null>(null)
  const [detailLoading, setDetailLoading] = useState(false)
  const [detailError, setDetailError] = useState<string | null>(null)
  const [section, setSection] = useState<Section>('overview')
  const [rerunning, setRerunning] = useState(false)
  const [rerunError, setRerunError] = useState<string | null>(null)
  const [comparison, setComparison] = useState<Comparison | null>(null)
  const [localAccess, setLocalAccess] = useState<EvidenceAccess | null>(evidenceAccess ?? null)
  useEffect(() => { setLocalAccess(evidenceAccess ?? null) }, [evidenceAccess])
  const mounted = useRef(false)
  const selected = useRef<string | null>(initialRequestId || null)
  const historySequence = useRef(0)
  const detailSequence = useRef(0)
  const rerunSequence = useRef(0)
  const rerunInFlight = useRef(false)

  const invalidate = useCallback(() => {
    historySequence.current += 1
    detailSequence.current += 1
    rerunSequence.current += 1
    rerunInFlight.current = false
  }, [])

  const close = useCallback(() => {
    if (!mounted.current) return
    mounted.current = false
    invalidate()
    onClose()
  }, [invalidate, onClose])

  const selectTrace = useCallback(async (requestId: string) => {
    if (!mounted.current) return
    const sequence = ++detailSequence.current
    rerunSequence.current += 1
    rerunInFlight.current = false
    selected.current = requestId
    setSelectedId(requestId)
    setDetail(null)
    setDetailLoading(true)
    setDetailError(null)
    setRerunning(false)
    setRerunError(null)
    setComparison(null)
    try {
      const next = await invoke<RecallTraceDetail | null>('memory_recall_trace_get', { requestId })
      if (!mounted.current || sequence !== detailSequence.current) return
      setDetail(next)
    } catch (error) {
      if (mounted.current && sequence === detailSequence.current) setDetailError(readableError(error))
    } finally {
      if (mounted.current && sequence === detailSequence.current) setDetailLoading(false)
    }
  }, [])

  const loadHistory = useCallback(async (selectFirst = false) => {
    if (!mounted.current) return
    const sequence = ++historySequence.current
    setHistoryLoading(true)
    setHistoryError(null)
    try {
      const entries = await invoke<RecallTraceSummary[]>('memory_recall_trace_list', { limit: 30 })
      if (!mounted.current || sequence !== historySequence.current) return
      setHistory(entries)
      if (selectFirst && !selected.current && entries[0]) void selectTrace(entries[0].request_id)
    } catch (error) {
      if (mounted.current && sequence === historySequence.current) setHistoryError(readableError(error))
    } finally {
      if (mounted.current && sequence === historySequence.current) setHistoryLoading(false)
    }
  }, [selectTrace])

  useEffect(() => {
    mounted.current = true
    return () => { mounted.current = false; invalidate() }
  }, [invalidate])

  useEffect(() => {
    if (initialRequestId) void selectTrace(initialRequestId)
    void loadHistory(!initialRequestId)
  }, [initialRequestId, loadHistory, selectTrace])

  const readComparison = useCallback(async (before: RecallTrace, requestId: string, sequence: number) => {
    const selectionAtStart = selected.current
    const valid = () => mounted.current && sequence === rerunSequence.current && selected.current === selectionAtStart
    if (!valid()) return
    setComparison({ before, after: null, requestId, loading: true, error: null })
    try {
      const next = await invoke<RecallTraceDetail | null>('memory_recall_trace_get', { requestId })
      if (!valid()) return
      if (!next || next.trace.request_id !== requestId) {
        setComparison({ before, after: null, requestId, loading: false, error: null })
        return
      }
      setComparison({ before, after: next.trace, requestId, loading: false, error: null })
      detailSequence.current += 1
      selected.current = requestId
      setSelectedId(requestId)
      setDetail(next)
      setDetailLoading(false)
      setDetailError(null)
    } catch (error) {
      if (valid()) setComparison({ before, after: null, requestId, loading: false, error: readableError(error) })
    }
  }, [])

  async function rerun() {
    if (!mounted.current || !detail || rerunInFlight.current || comparison?.loading) return
    const before = detail.trace
    const selectionAtStart = selected.current
    const sequence = ++rerunSequence.current
    const valid = () => mounted.current && sequence === rerunSequence.current && selected.current === selectionAtStart
    rerunInFlight.current = true
    setRerunning(true)
    setRerunError(null)
    try {
      // Pass the stored object unchanged, including domain_mode, domains and limit.
      const response = await invoke<MemoryRetrievalResponse>('memory_retrieval_search', {
        arguments: before.arguments,
        budgetMs: before.retrieval.budget_ms ?? null,
      })
      if (!valid()) return
      setLocalAccess(response.evidence_access ?? null)
      onRerun(response)
      if (!valid()) return
      void loadHistory()
      const requestId = response.retrieval?.request_id
      if (typeof requestId === 'string' && requestId.length > 0 && requestId !== before.request_id) {
        await readComparison(before, requestId, sequence)
      } else {
        setComparison({ before, after: null, requestId: null, loading: false, error: null })
      }
    } catch (error) {
      if (valid()) setRerunError(readableError(error))
    } finally {
      if (mounted.current && sequence === rerunSequence.current) {
        rerunInFlight.current = false
        setRerunning(false)
      }
    }
  }

  function retryComparison() {
    if (!mounted.current || !comparison?.requestId || rerunInFlight.current || comparison.loading) return
    void readComparison(comparison.before, comparison.requestId, ++rerunSequence.current)
  }

  function showBefore() {
    if (!mounted.current || !comparison) return
    // Use the retained trace, not a fresh read of a possibly expired record.
    detailSequence.current += 1
    rerunSequence.current += 1
    rerunInFlight.current = false
    selected.current = comparison.before.request_id
    setSelectedId(comparison.before.request_id)
    setDetail({ trace: comparison.before, current_memories: [] })
    setDetailLoading(false)
    setDetailError(null)
    setRerunning(false)
    setRerunError(null)
    setSection('candidates')
    setComparison(previous => previous ? { ...previous, loading: false } : previous)
  }

  const trace = detail?.trace
  const currentById = useMemo(() => new Map(detail?.current_memories.map(memory => [memory.memory_id, memory]) ?? []), [detail])

  return <Modal labelledBy={`${id}-title`} onClose={close}>
    <div className="ml-auto flex h-full min-h-0 w-full max-w-[1100px] flex-col overflow-hidden rounded-xl bg-[var(--app-surface)] text-[var(--app-text)] shadow-xl">
      <header className="flex shrink-0 items-center gap-3 border-b border-[var(--app-stroke)] px-4 py-3">
        <h2 id={`${id}-title`} className="min-w-0 flex-1 text-lg font-semibold">{t(`${ns}title`)}</h2>
        <button type="button" data-autofocus className={`${buttonClass} w-8 shrink-0 px-0`} onClick={close} aria-label={t(`${ns}close`)}><X size={17} aria-hidden="true" /></button>
      </header>
      <div className="flex min-h-0 flex-1 flex-col sm:flex-row">
        <aside className="flex max-h-40 min-h-0 shrink-0 flex-col border-b border-[var(--app-stroke)] bg-[var(--app-layer)] sm:max-h-none sm:w-48 sm:border-r sm:border-b-0" aria-label={t(`${ns}history`)}>
          <div className="flex shrink-0 items-center justify-between gap-2 px-3 py-2">
            <h3 className="text-sm font-semibold">{t(`${ns}history`)}</h3>
            <button type="button" className={`${buttonClass} w-8 px-0`} disabled={historyLoading} onClick={() => { void loadHistory(true) }} aria-label={t(`${ns}refreshHistory`)}><RefreshCw size={14} aria-hidden="true" /></button>
          </div>
          <div className="min-h-0 flex-1 overflow-y-auto overscroll-contain px-2 pb-2">
            {historyLoading && <Loading label={t(`${ns}historyLoading`)} />}
            {historyError && <Failure label={t(`${ns}historyFailed`)} error={historyError} onRetry={() => { void loadHistory(true) }} />}
            {!historyLoading && !historyError && history.length === 0 && <p className={`px-1 py-3 ${mutedClass}`}>{t(`${ns}historyEmpty`)}</p>}
            <ul className="space-y-1">
              {history.map(entry => <li key={entry.request_id}>
                <button type="button" onClick={() => { setSection(current => current === 'evaluation' ? 'overview' : current); void selectTrace(entry.request_id) }} aria-current={selectedId === entry.request_id ? 'true' : undefined}
                  className={`relative w-full rounded-[6px] px-3 py-2 text-left hover:bg-[var(--app-hover)] focus-visible:outline-2 focus-visible:outline-[var(--app-focus)] ${selectedId === entry.request_id ? 'bg-[var(--app-selected)]' : ''}`}>
                  {selectedId === entry.request_id && <span className="absolute top-3 bottom-3 left-0 w-[3px] rounded bg-[var(--app-accent)]" aria-hidden="true" />}
                  <span className="block truncate text-sm" title={entry.query}>{entry.query || t(`${ns}emptyQuery`)}</span>
                  <span className={`mt-1 block tabular-nums ${mutedClass}`}>{displayFullTime(entry.created_at)}</span>
                  <span className={`block tabular-nums ${mutedClass}`}>{t(`${ns}historySummary`, { count: entry.selected_count, ms: number(entry.total_ms) })}</span>
                  {entry.active_domains.length > 0 && <span className={`block truncate ${mutedClass}`} title={entry.active_domains.join(' · ')}>{entry.active_domains.join(' · ')}</span>}
                  {entry.truncated && <span className="block text-xs text-amber-900 dark:text-amber-200">{t(`${ns}truncatedShort`)}</span>}
                </button>
              </li>)}
            </ul>
            {history.length > 0 && <p className={`px-1 pt-3 ${mutedClass}`}>{t(`${ns}historyLimit`)}</p>}
          </div>
        </aside>
        <div className="flex min-h-0 min-w-0 flex-1 flex-col">
          <div className="flex shrink-0 flex-wrap items-center justify-between gap-2 border-b border-[var(--app-stroke)] px-4 py-2">
            <nav className="flex flex-wrap gap-1" aria-label={t(`${ns}sections`)}>
              {(['overview', 'candidates', 'sources', 'evaluation'] as const).map(name => <button key={name} type="button" data-selected={section === name} aria-pressed={section === name} aria-controls={`${id}-content`} onClick={() => setSection(name)} className={`${buttonClass} data-[selected=true]:bg-[var(--app-selected)] data-[selected=true]:text-[var(--app-accent)]`}>{t(name === 'evaluation' ? 'memory.evaluation.open' : `${ns}${name}`)}</button>)}
            </nav>
            {section !== 'evaluation' && <button type="button" className={buttonClass} disabled={!trace || detailLoading || rerunning || comparison?.loading} onClick={() => { void rerun() }}>
              {rerunning ? <Loader2 size={14} className="animate-spin motion-reduce:animate-none" aria-hidden="true" /> : <RefreshCw size={14} aria-hidden="true" />}
              {t(`${ns}${rerunning ? 'rerunning' : 'rerun'}`)}
            </button>}
          </div>
          <div id={`${id}-content`} className="min-h-0 flex-1 space-y-5 overflow-y-auto overscroll-contain p-4" aria-busy={section !== 'evaluation' && detailLoading}>
            {section === 'evaluation' ? <MemoryEvaluationPanel /> : <>
            {rerunError && <Failure label={t(`${ns}rerunFailed`)} error={rerunError} onRetry={() => { void rerun() }} />}
            {rerunning && <Loading label={t(`${ns}rerunning`)} />}
            {comparison && <ComparisonView comparison={comparison} onRetry={retryComparison} onShowBefore={showBefore} />}
            {detailLoading ? <Loading label={t(`${ns}detailLoading`)} /> : detailError ? <Failure label={t(`${ns}detailFailed`)} error={detailError} onRetry={selectedId ? () => { void selectTrace(selectedId) } : undefined} /> : !trace ? <div className="space-y-3 py-6">
              <p className={mutedClass}>{t(`${ns}${selectedId ? 'detailMissing' : 'selectRecord'}`)}</p>
              {selectedId && <button type="button" className={buttonClass} onClick={() => { void selectTrace(selectedId) }}>{t(`${ns}retry`)}</button>}
            </div> : <>
              <p className="break-all text-xs text-[var(--app-secondary)]">{t(`${ns}viewingRequest`, { requestId: trace.request_id })}</p>
              {trace.truncated && <TruncatedNotice />}
              {section === 'overview' && <Overview trace={trace} />}
              {section === 'candidates' && <Candidates trace={trace} />}
              {section === 'sources' && <section className="space-y-3">
                <h3 className="text-base font-semibold">{t(`${ns}sources`)}</h3>
                <p className={mutedClass}>{t(`${ns}sourcesHint`)}</p>
                <MemoryEvidencePanel key={trace.request_id}
                  access={localAccess?.request_id === trace.request_id ? localAccess : null}
                  memoryIds={trace.results.map(result => result.memory_id)} query={trace.query}
                  memoryLabels={Object.fromEntries((detail?.current_memories ?? []).map(memory =>
                    [memory.memory_id, Array.from(memory.content ?? '').slice(0, 80).join('')]))}
                />
                {trace.results.length === 0 && <p className={`py-4 ${mutedClass}`}>{t(`${ns}noResults`)}</p>}
                {trace.results.map((result, index) => <article key={result.memory_id} className="min-w-0 space-y-4 border-t border-[var(--app-stroke)] py-4">
                  <h4 className="break-all text-sm font-semibold">{t(`${ns}resultTitle`, { rank: index + 1, memoryId: result.memory_id })}</h4>
                  <div>
                    <h5 className="mb-3 text-sm font-semibold">{t(`${ns}historicalMetadata`)}</h5>
                    <dl className="space-y-2">
                      <Field label={t(`${ns}domains`)}>{result.domains.join(' · ') || t(`${ns}none`)}</Field>
                      <Field label={t(`${ns}sourceAgent`)}>{result.source_agent ? sourceLabel(result.source_agent) : t(`${ns}none`)}</Field>
                      <Field label={t(`${ns}sessionId`)}>{result.session_id || t(`${ns}none`)}</Field>
                      <Field label={t(`${ns}eventKey`)}>{result.source_event_key || t(`${ns}none`)}</Field>
                      <Field label={t(`${ns}eventTime`)}>{result.event_time ? displayFullTime(result.event_time) : t(`${ns}none`)}</Field>
                      <Field label={t(`${ns}updatedAt`)}>{result.updated_at ? displayFullTime(result.updated_at) : t(`${ns}none`)}</Field>
                      <Field label={t(`${ns}fingerprint`)}>{result.fingerprint}</Field>
                    </dl>
                  </div>
                  <CurrentMemory view={currentById.get(result.memory_id)} />
                </article>)}
              </section>}
            </>}
            </>}
          </div>
        </div>
      </div>
    </div>
  </Modal>
}
