import { useId, useLayoutEffect, useRef, useState, type ReactNode } from 'react'
import { invoke } from '@tauri-apps/api/core'
import { useTranslation } from 'react-i18next'
import { Loader2 } from 'lucide-react'
import type { EvidenceAccess, EvidenceResponse } from '../types/memoryEvidence'

const ns = 'memory.evidence.'
const buttonClass = 'inline-flex min-h-8 items-center justify-center gap-2 rounded-[6px] border border-[var(--app-control-stroke)] bg-[var(--app-surface)] px-3 py-1.5 text-xs font-medium hover:bg-[var(--app-hover)] focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-[var(--app-focus)] disabled:cursor-not-allowed disabled:opacity-50'
const mutedClass = 'text-xs leading-5 text-[var(--app-secondary)]'
const fieldClass = 'w-full min-w-0 rounded-[6px] border border-[var(--app-control-stroke)] bg-[var(--app-surface)] px-3 py-2 text-sm focus-visible:outline-2 focus-visible:outline-[var(--app-focus)] disabled:opacity-50'
const statuses = new Set(['ready', 'ok', 'partial', 'source_unavailable', 'invalid_context', 'context_expired', 'invalid_cursor', 'forbidden_scope', 'budget_exhausted', 'stale_memory', 'scope_changed', 'source_changed', 'timeout', 'cancelled', 'unavailable', 'available', 'missing_source', 'remote_source_missing', 'manual_no_source', 'source_identity_mismatch', 'pending'])
const successStatuses = new Set(['ok', 'partial', 'source_unavailable'])
const refreshStatuses = new Set(['invalid_context', 'context_expired', 'forbidden_scope', 'stale_memory', 'scope_changed', 'source_changed'])
const truncateQuery = (value: string) => Array.from(value).slice(0, 512).join('')

function Field({ label, children }: { label: string; children: ReactNode }) {
  return <div className="grid min-w-0 gap-1 sm:grid-cols-[9rem_minmax(0,1fr)] sm:gap-3">
    <dt className={mutedClass}>{label}</dt>
    <dd className="min-w-0 whitespace-pre-wrap break-words text-xs leading-5 [overflow-wrap:anywhere]">{children}</dd>
  </div>
}

/** Current search evidence stays usable even when auxiliary trace storage fails. */
export function CurrentMemoryEvidence({ access, requestId, memories, query, active = true }: {
  access: EvidenceAccess | null
  requestId: string | undefined
  memories: readonly { id: string, memory: string }[]
  query: string
  active?: boolean
}) {
  const { t } = useTranslation()
  if (!active || !access || access.status !== 'ready' || !requestId || access.request_id !== requestId || memories.length === 0) return null
  return <details key={requestId} className="min-w-0 space-y-3">
    <summary className="w-fit cursor-pointer rounded-[6px] py-2 text-sm font-medium focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-[var(--app-focus)]">{t(`${ns}title`)}</summary>
    <MemoryEvidencePanel access={access} memoryIds={memories.map(memory => memory.id)} query={query}
      memoryLabels={Object.fromEntries(memories.map(memory => [memory.id, Array.from(memory.memory).slice(0, 80).join('')]))} />
  </details>
}

export function MemoryEvidencePanel({ access, memoryIds, query, memoryLabels }: {
  access: EvidenceAccess | null
  memoryIds: string[]
  query: string
  memoryLabels?: Record<string, string>
}) {
  const { t } = useTranslation()
  const id = useId()
  const idsKey = JSON.stringify([...new Set(memoryIds.filter(Boolean))])
  const ids: string[] = JSON.parse(idsKey)
  const [selectedIds, setSelectedIds] = useState<string[]>(() => ids.slice(0, 2))
  const [locatingQuery, setLocatingQuery] = useState(() => truncateQuery(query))
  const [maxChars, setMaxChars] = useState(4000)
  const [response, setResponse] = useState<EvidenceResponse | null>(null)
  const [loading, setLoading] = useState(false)
  const [localStatus, setLocalStatus] = useState<string | null>(null)
  const [expired, setExpired] = useState(false)
  const mounted = useRef(false)
  const sequence = useRef(0)
  const inFlight = useRef<string | null>(null)
  const responseAccess = useRef<EvidenceAccess | null>(null)
  const responseExpiry = useRef<string | null>(null)

  function invalidate() {
    sequence.current += 1
    const requestId = inFlight.current
    inFlight.current = null
    if (requestId) void invoke<boolean>('memory_evidence_cancel', { requestId }).catch(() => { /* Never expose submitted parameters through errors. */ })
  }

  useLayoutEffect(() => {
    mounted.current = true
    return () => { mounted.current = false; invalidate() }
  }, [])

  // Layout cleanup invalidates requests before the changed scope can be painted.
  useLayoutEffect(() => {
    invalidate()
    setSelectedIds((JSON.parse(idsKey) as string[]).slice(0, 2))
    setLocatingQuery(truncateQuery(query))
    setResponse(null)
    setLocalStatus(null)
    setLoading(false)
    setExpired(false)
    return invalidate
  }, [access, access?.context_token, access?.status, access?.expires_at, access?.request_id, access?.max_chars, idsKey, query])

  const selectionKey = JSON.stringify(selectedIds)
  useLayoutEffect(() => {
    invalidate()
    setResponse(null)
    setLocalStatus(null)
    setLoading(false)
    return invalidate
  }, [selectionKey, locatingQuery, maxChars])

  // Expiry only disables access; it never triggers a read or another retrieval.
  useLayoutEffect(() => {
    const deadlines = [access?.expires_at, responseAccess.current === access ? responseExpiry.current : null].filter((value): value is string => value != null).map(Date.parse)
    if (deadlines.length === 0) return
    const deadline = Math.min(...deadlines)
    let timer: ReturnType<typeof setTimeout> | undefined
    function check() {
      const remaining = deadline - Date.now()
      if (!Number.isFinite(remaining) || remaining <= 0) {
        invalidate()
        setExpired(true)
        setLoading(false)
      } else timer = setTimeout(check, Math.min(remaining, 2_147_483_647))
    }
    check()
    return () => { if (timer !== undefined) clearTimeout(timer) }
  }, [access, access?.expires_at, response?.expires_at])

  const accessReady = access?.status === 'ready' && !!access.context_token?.trim() && !expired
  const blockedByResponse = !!response && (refreshStatuses.has(response.status) || response.status === 'budget_exhausted')
  const canRead = accessReady && selectedIds.length >= 1 && selectedIds.length <= 2 && selectedIds.every(value => ids.includes(value)) && !loading && !blockedByResponse
  const canContinue = canRead && !!response?.next_cursor && response.remaining_chars > 0 && successStatuses.has(response.status)
  const statusText = (status: string) => statuses.has(status)
    ? `${t(`${ns}status_${status}`)} (${status})`
    : t(`${ns}status_unknown`)
  const empty = t(`${ns}notProvided`)

  async function read(next: boolean) {
    if (!mounted.current || inFlight.current || !canRead || !access?.context_token || (next && !canContinue)) return
    const deadline = access.expires_at == null ? null : Date.parse(access.expires_at)
    const responseDeadline = responseAccess.current !== access || responseExpiry.current == null ? null : Date.parse(responseExpiry.current)
    if ([deadline, responseDeadline].some(value => value !== null && (!Number.isFinite(value) || value <= Date.now()))) {
      setExpired(true)
      return
    }
    const cursor = next ? response?.next_cursor ?? null : null
    const requestId = crypto.randomUUID()
    const current = ++sequence.current
    inFlight.current = requestId
    setLoading(true)
    setLocalStatus(null)
    setResponse(null)
    const valid = () => mounted.current && sequence.current === current && inFlight.current === requestId
    try {
      const result = await invoke<EvidenceResponse>('memory_evidence_read', {
        arguments: { context_token: access.context_token, memory_ids: [...selectedIds], query: locatingQuery, max_chars: maxChars, cursor },
        requestId,
      })
      if (valid()) { responseAccess.current = access; responseExpiry.current = result.expires_at; setResponse(result) }
    } catch {
      if (valid()) setLocalStatus('unavailable')
    } finally {
      if (valid()) { inFlight.current = null; setLoading(false) }
    }
  }

  function cancel() {
    invalidate()
    setLoading(false)
    setResponse(null)
    setLocalStatus('cancelled')
  }

  return <section aria-labelledby={`${id}-title`} className="min-w-0 space-y-4 text-[var(--app-text)]">
    <header className="space-y-1">
      <h3 id={`${id}-title`} className="text-base font-semibold">{t(`${ns}title`)}</h3>
      <p className={mutedClass}>{t(`${ns}intro`)}</p>
    </header>
    {!accessReady && <p role="status" className="rounded-[6px] bg-amber-50 p-3 text-xs leading-5 text-amber-900 dark:bg-amber-950/40 dark:text-amber-200">{t(`${ns}${expired ? 'accessExpired' : 'accessRequired'}`)}</p>}
    <fieldset className="min-w-0 space-y-2">
      <legend className="mb-1 text-sm font-semibold">{t(`${ns}selection`)}</legend>
      <p id={`${id}-selection-hint`} className={mutedClass}>{t(`${ns}selectionHint`)}</p>
      {ids.length === 0 ? <p className={mutedClass}>{t(`${ns}noHits`)}</p> : <div className="max-h-40 overflow-y-auto overscroll-contain rounded-[6px] border border-[var(--app-stroke)] px-3" aria-describedby={`${id}-selection-hint`}>
        {ids.map(memoryId => <label key={memoryId} className="flex min-h-10 min-w-0 cursor-pointer items-start gap-2 py-2 text-sm">
          <input type="checkbox" checked={selectedIds.includes(memoryId)} disabled={!selectedIds.includes(memoryId) && selectedIds.length >= 2}
            className="mt-1 shrink-0 accent-[var(--app-accent)] focus-visible:outline-2 focus-visible:outline-[var(--app-focus)] disabled:cursor-not-allowed"
            onChange={event => { invalidate(); setSelectedIds(previous => event.target.checked ? [...previous, memoryId].slice(0, 2) : previous.filter(value => value !== memoryId)) }} />
          <span className="min-w-0 [overflow-wrap:anywhere]">{memoryLabels?.[memoryId] && <span className="block">{memoryLabels[memoryId]}</span>}<span className={memoryLabels?.[memoryId] ? mutedClass : ''}>{memoryId}</span></span>
        </label>)}
      </div>}
    </fieldset>
    <div className="grid min-w-0 gap-3 sm:grid-cols-[minmax(0,1fr)_10rem]">
      <div className="min-w-0 space-y-1">
        <label htmlFor={`${id}-query`} className="text-sm font-medium">{t(`${ns}query`)}</label>
        <textarea id={`${id}-query`} rows={2} value={locatingQuery} aria-describedby={`${id}-query-hint`} className={`${fieldClass} resize-y`}
          onChange={event => { const value = truncateQuery(event.target.value); if (value !== locatingQuery) { invalidate(); setLocatingQuery(value) } }} />
        <p id={`${id}-query-hint`} className={mutedClass}>{t(`${ns}queryHint`, { used: Array.from(locatingQuery).length })}</p>
      </div>
      <div className="space-y-1">
        <label htmlFor={`${id}-budget`} className="text-sm font-medium">{t(`${ns}budget`)}</label>
        <select id={`${id}-budget`} value={maxChars} aria-describedby={`${id}-budget-hint`} className={fieldClass} onChange={event => { invalidate(); setMaxChars(Number(event.target.value)) }}>
          {[2000, 4000, 8000].map(value => <option key={value} value={value}>{t(`${ns}characters`, { value })}</option>)}
        </select>
      </div>
    </div>
    <p id={`${id}-budget-hint`} className={mutedClass}>{t(`${ns}budgetHint`)}</p>
    <div className="flex flex-wrap items-center gap-2">
      <button type="button" className={buttonClass} disabled={!canRead} onClick={() => { void read(false) }}>{t(`${ns}read`)}</button>
      {loading && <button type="button" className={buttonClass} onClick={cancel}>{t(`${ns}cancel`)}</button>}
      {canContinue && <button type="button" className={buttonClass} onClick={() => { void read(true) }}>{t(`${ns}continue`)}</button>}
    </div>
    <div aria-live="polite" aria-atomic="true">
      {loading && <p role="status" className={`flex items-center gap-2 ${mutedClass}`}><Loader2 size={14} aria-hidden="true" className="animate-spin motion-reduce:animate-none" />{t(`${ns}loading`)}</p>}
      {localStatus && <p role="status" className={mutedClass}>{statusText(localStatus)}</p>}
      {!loading && !localStatus && !response && accessReady && <p className={mutedClass}>{t(`${ns}idle`)}</p>}
    </div>
    {response && <div className="min-w-0 space-y-4 border-t border-[var(--app-stroke)] pt-4">
      <dl className="space-y-2 tabular-nums">
        <Field label={t(`${ns}responseStatus`)}>{statusText(response.status)}</Field>
        <Field label={t(`${ns}expiresAt`)}>{response.expires_at || empty}</Field>
        <Field label={t(`${ns}returnedChars`)}>{response.returned_chars}</Field>
        <Field label={t(`${ns}remainingChars`)}>{response.remaining_chars}</Field>
        <Field label={t(`${ns}partial`)}>{t(`${ns}${response.partial ? 'yes' : 'no'}`)}</Field>
      </dl>
      {expired && <p role="note" className={mutedClass}>{t(`${ns}expiredResults`)}</p>}
      {!successStatuses.has(response.status) && <p role="status" className={mutedClass}>{t(`${ns}failedResponse`)}</p>}
      <>
        <p className={mutedClass}>{t(`${ns}excerptHint`)}</p>
        {response.items.length === 0 && <p className={mutedClass}>{t(`${ns}empty`)}</p>}
        {response.items.map((item, itemIndex) => <article key={`${item.memory_id}-${itemIndex}`} className="min-w-0 space-y-3 border-t border-[var(--app-stroke)] pt-4">
          <dl className="space-y-2">
            <Field label={t(`${ns}memoryId`)}>{item.memory_id}</Field>
            <Field label={t(`${ns}itemStatus`)}>{statusText(item.status)}</Field>
          </dl>
          {item.sources.length === 0 && <p className={mutedClass}>{t(`${ns}noSources`)}</p>}
          {item.sources.map((source, sourceIndex) => <div key={`${source.source_event_key}-${sourceIndex}`} className="min-w-0 space-y-3 border-t border-[var(--app-stroke)] pt-3">
            <dl className="space-y-2">
              <Field label={t(`${ns}sourceStatus`)}>{statusText(source.status)}</Field>
              <Field label={t(`${ns}client`)}>{source.source_agent || empty}</Field>
              <Field label={t(`${ns}session`)}>{source.session_id || empty}</Field>
              <Field label={t(`${ns}time`)}>{source.occurred_at || empty}</Field>
              <Field label={t(`${ns}eventKey`)}>{source.source_event_key || empty}</Field>
              <Field label={t(`${ns}sourceFingerprint`)}>{source.source_fingerprint || empty}</Field>
            </dl>
            {source.excerpt && successStatuses.has(response.status) ? <>
              <dl className="space-y-2 tabular-nums">
                <Field label={t(`${ns}range`)}>{t(`${ns}rangeValue`, { start: source.excerpt.char_start, end: source.excerpt.char_end, total: source.excerpt.total_chars })}</Field>
                <Field label={t(`${ns}matchMethod`)}>{t(`${ns}match_${['lexical', 'prefix_fallback', 'continuation'].includes(source.excerpt.match_method) ? source.excerpt.match_method : 'unknown'}`)}</Field>
                <Field label={t(`${ns}excerptFingerprint`)}>{source.excerpt.excerpt_fingerprint || empty}</Field>
              </dl>
              {source.excerpt.match_method === 'prefix_fallback' && <p role="note" className="text-xs leading-5 text-amber-900 dark:text-amber-200">{t(`${ns}prefixHint`)}</p>}
              <p tabIndex={0} aria-label={t(`${ns}excerptText`)} className="max-h-80 max-w-[75ch] overflow-y-auto overscroll-contain whitespace-pre-wrap break-words rounded-[6px] bg-[var(--app-layer)] p-3 text-sm leading-6 [overflow-wrap:anywhere] focus-visible:outline-2 focus-visible:outline-[var(--app-focus)]">{source.excerpt.text}</p>
            </> : <p className={mutedClass}>{t(`${ns}noExcerpt`)}</p>}
          </div>)}
        </article>)}
      </>
      {response.remaining_chars <= 0 && <p role="status" className={mutedClass}>{statusText('budget_exhausted')}</p>}
    </div>}
  </section>
}
