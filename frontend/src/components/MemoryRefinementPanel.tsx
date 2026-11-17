import { useCallback, useEffect, useId, useMemo, useRef, useState } from 'react'
import { invoke } from '@tauri-apps/api/core'
import { useTranslation } from 'react-i18next'
import { AlertTriangle, Check, ChevronLeft, ChevronRight, Loader2, RefreshCw, Trash2, Undo2 } from 'lucide-react'
import { sourceLabel } from './ConversationDialog'
import { intlLocale } from '../i18n'
import { useConfirmDialog } from './ConfirmDialog'
import { RefinementHistoryMenu } from './RefinementHistoryMenu'
import type { MemoryDomainItem, RefinementAction, RefinementDeleteSelection, RefinementHistorySnapshot, RefinementMemory, RefinementRun, RefinementSelection } from '../types/memory'

const ACTION_PAGE_SIZE = 5
const ORIGINAL_PAGE_SIZE = 3
const MAX_SELECTIONS = 200
const GENERATED_CONFLICT_CHOICE = '__generated_proposal__'
const buttonClass = 'inline-flex min-h-8 items-center justify-center gap-1.5 rounded-md border border-gray-300 bg-white px-2.5 py-1.5 text-xs font-medium text-gray-700 hover:bg-gray-50 disabled:cursor-not-allowed disabled:opacity-50 dark:border-gray-600 dark:bg-gray-800 dark:text-gray-200 dark:hover:bg-gray-700'
const primaryClass = 'inline-flex min-h-8 items-center justify-center gap-1.5 rounded-md bg-violet-600 px-3 py-1.5 text-xs font-medium text-white hover:bg-violet-700 disabled:cursor-not-allowed disabled:opacity-50'
const dangerClass = 'inline-flex min-h-8 items-center justify-center gap-1.5 rounded-md border border-red-300 bg-white px-2.5 py-1.5 text-xs font-medium text-red-700 hover:bg-red-50 disabled:cursor-not-allowed disabled:opacity-50 dark:border-red-800 dark:bg-gray-800 dark:text-red-300 dark:hover:bg-red-950/40'
const fieldClass = 'min-w-0 rounded-md border border-gray-300 bg-white px-2.5 py-1.5 text-sm text-gray-800 disabled:opacity-50 dark:border-gray-600 dark:bg-gray-800 dark:text-gray-100'
const errorLabels = {
  history: 'memory.refinementErrorHistory',
  forget: 'memory.refinementErrorForget',
  clear: 'memory.refinementErrorClear',
  open: 'memory.refinementErrorOpen',
  poll: 'memory.refinementErrorPoll',
  start: 'memory.refinementErrorStart',
  resume: 'memory.refinementErrorResume',
  cancel: 'memory.refinementErrorCancel',
  apply: 'memory.refinementErrorApply',
  suggest: 'memory.refinementErrorSuggest',
  skip: 'memory.refinementErrorSkip',
  delete: 'memory.refinementErrorDelete',
  undo: 'memory.refinementErrorUndo',
  refresh: 'memory.refinementErrorRefresh',
} as const

type ErrorKey = keyof typeof errorLabels
type Operation = Exclude<ErrorKey, 'history' | 'open' | 'poll' | 'refresh' | 'suggest'>

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

function formatTime(value: string): string {
  const normalized = value.trim().replace(/(\.\d{3})\d+(?=(Z|[+-]\d{2}:\d{2})$)/, '$1')
  const date = new Date(normalized)
  return Number.isNaN(date.getTime()) ? value : new Intl.DateTimeFormat(intlLocale(), {
    year: 'numeric', month: '2-digit', day: '2-digit', hour: '2-digit', minute: '2-digit',
  }).format(date)
}

function isRunning(run: RefinementRun | null): boolean {
  return run?.status === 'analyzing' || run?.status === 'cancelling'
}

function isSelectable(action: RefinementAction): boolean {
  return action.status === 'pending'
}

function isEditable(action: RefinementAction): boolean {
  return action.kind === 'rewrite' || action.kind === 'merge' || action.kind === 'conflict'
}

function Pagination({ page, pages, onChange, label }: { page: number; pages: number; onChange: (page: number) => void; label: string }) {
  const { t } = useTranslation()
  if (pages <= 1) return null
  return <nav aria-label={label} className="flex flex-wrap items-center justify-end gap-2 text-xs text-gray-600 dark:text-gray-300">
    <button type="button" className={buttonClass} disabled={page === 0} onClick={() => onChange(page - 1)} aria-label={t('memory.refinementPrevious')}><ChevronLeft size={14} aria-hidden="true" />{t('memory.refinementPrevious')}</button>
    <span aria-live="polite" className="tabular-nums">{t('memory.refinementPage', { current: page + 1, total: pages })}</span>
    <button type="button" className={buttonClass} disabled={page >= pages - 1} onClick={() => onChange(page + 1)} aria-label={t('memory.refinementNext')}>{t('memory.refinementNext')}<ChevronRight size={14} aria-hidden="true" /></button>
  </nav>
}

function MemoryMetadata({ memory }: { memory: Pick<RefinementMemory, 'memory_type' | 'durability'> }) {
  const { t } = useTranslation()
  const durabilityKey = memory.durability === 'long_term' ? 'memory.durabilityLongTerm'
    : memory.durability === 'short_term' ? 'memory.durabilityShortTerm'
      : memory.durability === 'session' ? 'memory.durabilitySession' : null
  return <dl className="flex flex-wrap gap-x-4 gap-y-1 text-xs text-gray-600 dark:text-gray-300">
    <div><dt className="inline font-medium">{t('memory.refinementMemoryType')}: </dt><dd className="inline">{memory.memory_type ? t(`memory.type.${memory.memory_type}`, { defaultValue: memory.memory_type }) : t('common.none')}</dd></div>
    <div><dt className="inline font-medium">{t('memory.refinementDurability')}: </dt><dd className="inline">{durabilityKey ? t(durabilityKey) : memory.durability || t('common.none')}</dd></div>
  </dl>
}

function actionDomainIds(action: RefinementAction): string[] {
  return [...new Set(action.originals.flatMap((memory) => memory.domains))].filter((id) => id !== 'unclassified').sort()
}

function LabelEditor({ domainIds, domains, label, disabled, onChange }: {
  domainIds: readonly string[]
  domains: readonly MemoryDomainItem[]
  label: string
  disabled: boolean
  onChange?: (ids: string[]) => void
}) {
  const { t } = useTranslation()
  const id = useId()
  const [editing, setEditing] = useState(false)
  const selectedIds = domainIds.filter((domainId) => domainId !== 'unclassified')
  const choices = domains.filter((domain) => domain.id !== 'unclassified')
  const missingIds = selectedIds.filter((domainId) => !choices.some((domain) => domain.id === domainId))
  return <>
    <div role="group" aria-label={label} className="flex min-w-0 flex-wrap items-center gap-1.5 text-xs">
      <span className="shrink-0 text-gray-600 dark:text-gray-300">{label}:</span>
      {selectedIds.length > 0 ? selectedIds.map((domainId) => <span key={domainId} className="max-w-full break-words rounded bg-gray-100 px-1.5 py-0.5 text-gray-700 [overflow-wrap:anywhere] dark:bg-gray-700 dark:text-gray-200">{domains.find((domain) => domain.id === domainId)?.name ?? domainId}</span>)
        : <span className="text-gray-600 dark:text-gray-300">{t('memory.refinementUnclassified')}</span>}
      {onChange && <button type="button" disabled={disabled} aria-expanded={editing} aria-controls={`${id}-choices`} aria-describedby={`${id}-hint`} title={t('memory.refinementLabelDraftHint')} onClick={() => setEditing((previous) => !previous)} className="inline-flex min-h-8 shrink-0 items-center gap-1 rounded px-1 font-medium text-violet-700 hover:underline focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-violet-600 disabled:opacity-50 dark:text-violet-300">
        <ChevronRight size={12} className={editing ? 'rotate-90' : ''} aria-hidden="true" />{t('memory.refinementEditLabels')}
      </button>}
      <p id={`${id}-hint`} className="sr-only">{t('memory.refinementLabelDraftHint')}</p>
    </div>
    {onChange && <fieldset id={`${id}-choices`} hidden={!editing} disabled={disabled} aria-describedby={`${id}-hint`} className="min-w-0 basis-full">
      <legend className="sr-only">{label}</legend>
      <div className="flex max-h-40 flex-wrap items-center gap-x-3 gap-y-1 overflow-y-auto">
        {[...choices.map((domain) => ({ id: domain.id, name: domain.name })), ...missingIds.map((domainId) => ({ id: domainId, name: domainId }))].map((domain) => <label key={domain.id} className="inline-flex min-h-8 max-w-full items-center gap-2 text-xs text-gray-700 dark:text-gray-200">
          <input type="checkbox" checked={selectedIds.includes(domain.id)} onChange={(event) => onChange(event.target.checked ? [...selectedIds, domain.id].sort() : selectedIds.filter((domainId) => domainId !== domain.id))} className="h-4 w-4 shrink-0 focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-violet-600 disabled:opacity-50" />
          <span className="break-words [overflow-wrap:anywhere]">{domain.name}</span>
        </label>)}
        {choices.length === 0 && missingIds.length === 0 && <p className="text-xs text-gray-600 dark:text-gray-300">{t('memory.refinementNoLabels')}</p>}
        <button type="button" className={buttonClass} disabled={disabled || selectedIds.length === 0} onClick={() => onChange([])}>{t('memory.refinementClearLabels')}</button>
      </div>
    </fieldset>}
  </>
}

function OriginalRecords({ originals, domains }: { originals: RefinementMemory[]; domains: readonly MemoryDomainItem[] }) {
  const { t } = useTranslation()
  const [page, setPage] = useState(0)
  const pages = Math.max(1, Math.ceil(originals.length / ORIGINAL_PAGE_SIZE))
  const currentPage = Math.min(page, pages - 1)
  return <div className="min-w-0 space-y-3">
    <h5 className="text-xs font-semibold text-gray-800 dark:text-gray-100">{t('memory.refinementOriginals', { count: originals.length })}</h5>
    {originals.slice(currentPage * ORIGINAL_PAGE_SIZE, (currentPage + 1) * ORIGINAL_PAGE_SIZE).map((memory, index) => <div key={memory.id} className="min-w-0 space-y-2 border-t border-gray-200 pt-2 first:border-t-0 first:pt-0 dark:border-gray-700">
      <p className="text-xs font-medium text-gray-600 dark:text-gray-300">{t('memory.refinementOriginalNumber', { number: currentPage * ORIGINAL_PAGE_SIZE + index + 1 })}</p>
      <p className="max-h-48 overflow-y-auto whitespace-pre-wrap break-words text-sm leading-6 text-gray-800 [overflow-wrap:anywhere] dark:text-gray-100">{memory.memory}</p>
      <MemoryMetadata memory={memory} />
      <dl className="space-y-1 break-words text-xs text-gray-600 [overflow-wrap:anywhere] dark:text-gray-300">
        <div><dt className="inline font-medium">{t('memory.refinementSource')}: </dt><dd className="inline">{memory.source ? sourceLabel(memory.source) : t('common.none')}</dd></div>
        {memory.session_id && <div><dt className="inline font-medium">{t('memory.refinementSession')}: </dt><dd className="inline">{memory.session_id}</dd></div>}
        {memory.event_time && <div><dt className="inline font-medium">{t('memory.refinementEventTime')}: </dt><dd className="inline"><time dateTime={memory.event_time}>{formatTime(memory.event_time)}</time></dd></div>}
        {memory.updated_at && <div><dt className="inline font-medium">{t('memory.refinementUpdated')}: </dt><dd className="inline"><time dateTime={memory.updated_at}>{formatTime(memory.updated_at)}</time></dd></div>}
        <div><dt className="inline font-medium">{t('memory.refinementDomains')}: </dt><dd className="inline">{memory.domains.filter((domainId) => domainId !== 'unclassified').map((domainId) => domains.find((domain) => domain.id === domainId)?.name ?? domainId).join(' · ') || t('memory.refinementUnclassified')}</dd></div>
      </dl>
      {memory.source_excerpt && <details className="text-xs text-gray-600 dark:text-gray-300">
        <summary className="cursor-pointer font-medium">{t('memory.refinementSourceExcerpt')}</summary>
        <blockquote className="mt-2 max-h-40 overflow-y-auto whitespace-pre-wrap break-words border border-gray-200 bg-gray-50 p-2 leading-5 [overflow-wrap:anywhere] dark:border-gray-700 dark:bg-gray-900/40">{memory.source_excerpt}</blockquote>
      </details>}
    </div>)}
    <Pagination page={currentPage} pages={pages} onChange={setPage} label={t('memory.refinementOriginalPages')} />
  </div>
}

function ActionReview({ action, number, selected, draft, conflictChoice, conflictSourceId, domainIds, domains, disabled, suggestionDisabled, selectionFull, selectionHintId, working, onSelect, onEdit, onConflictSource, onEditLabels, onSuggest, onApply, onSkip, onDelete }: {
  action: RefinementAction
  number: number
  selected: boolean
  draft: string
  conflictChoice?: string
  conflictSourceId?: string
  domainIds: string[]
  domains: readonly MemoryDomainItem[]
  disabled: boolean
  suggestionDisabled: boolean
  selectionFull: boolean
  selectionHintId: string
  working: 'apply' | 'skip' | 'delete' | 'suggest' | null
  onSelect: () => void
  onEdit: (content: string) => void
  onConflictSource: (sourceId: string) => void
  onEditLabels: (ids: string[]) => void
  onSuggest: () => void
  onApply: () => void
  onSkip: () => void
  onDelete: () => void
}) {
  const { t } = useTranslation()
  const id = useId()
  const selectable = isSelectable(action)
  const editable = isEditable(action)
  const conflictSource = action.originals.find((memory) => memory.id === conflictSourceId)
  const hasRecommendation = action.originals.some((memory) => memory.id === action.recommended_source_id)
  const unresolved = action.kind === 'conflict' && !conflictSource
  const empty = editable && !unresolved && action.status === 'pending' && !draft.trim()
  return <article className="min-w-0 space-y-3 border-t border-gray-200 py-4 first:border-t-0 dark:border-gray-700" aria-labelledby={`${id}-title`}>
    <header className="flex flex-wrap items-center gap-2">
      <input type="checkbox" checked={selected} onChange={onSelect} disabled={disabled || !selectable || (!selected && selectionFull)} aria-describedby={selectionHintId} aria-label={t('memory.refinementSelectAction', { number, kind: t(`memory.refinementKind_${action.kind}`) })} className="h-4 w-4 shrink-0 focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-violet-600 disabled:opacity-50" />
      <h4 id={`${id}-title`} className="text-sm font-semibold text-gray-800 dark:text-gray-100">{t('memory.refinementActionTitle', { number, kind: t(`memory.refinementKind_${action.kind}`) })}</h4>
      <span className={`rounded px-1.5 py-0.5 text-xs ${action.status === 'applied' ? 'bg-green-50 text-green-800 dark:bg-green-950/40 dark:text-green-200' : action.status === 'stale' ? 'bg-amber-50 text-amber-900 dark:bg-amber-950/40 dark:text-amber-200' : 'bg-gray-100 text-gray-700 dark:bg-gray-700 dark:text-gray-200'}`}>{t(`memory.refinementActionStatus_${action.status}`)}</span>
      <LabelEditor domainIds={domainIds} domains={domains} label={t(action.kind === 'archive' ? 'memory.refinementDomains' : 'memory.refinementResultLabels')} disabled={disabled} onChange={selectable && action.kind !== 'archive' ? onEditLabels : undefined} />
    </header>
    {action.kind === 'conflict' && <p id={`${id}-conflict-hint`} className="flex items-start gap-2 text-xs leading-5 text-amber-900 dark:text-amber-200"><AlertTriangle size={14} className="mt-0.5 shrink-0" aria-hidden="true" />{t(hasRecommendation ? 'memory.refinementConflictHint' : 'memory.refinementConflictNeedsSuggestion')}</p>}
    {action.kind === 'archive' && <p className="text-xs leading-5 text-amber-900 dark:text-amber-200">{t('memory.refinementArchiveHint')}</p>}
    <div className={action.kind === 'conflict' ? 'min-w-0 space-y-3' : 'grid min-w-0 gap-5 xl:grid-cols-2'}>
      {action.kind !== 'conflict' && <OriginalRecords originals={action.originals} domains={domains} />}
      <div className="min-w-0 space-y-3">
        {action.kind !== 'archive' && <div className="space-y-2">
          {action.kind === 'conflict' && selectable && !hasRecommendation && <button type="button" className={primaryClass} disabled={disabled || suggestionDisabled} onClick={onSuggest}>
            {working === 'suggest' ? <Loader2 size={14} className="animate-spin motion-reduce:animate-none" aria-hidden="true" /> : <RefreshCw size={14} aria-hidden="true" />}
            {t(working === 'suggest' ? 'memory.refinementSuggesting' : 'memory.refinementSuggest')}
          </button>}
          {action.kind === 'conflict' && <label className="flex flex-wrap items-center gap-2 text-xs font-medium text-gray-700 dark:text-gray-200">
            {t('memory.refinementConflictSource')}
            <select value={conflictChoice ?? ''} onChange={(event) => onConflictSource(event.target.value)} disabled={disabled || !selectable} aria-describedby={`${id}-conflict-hint`} className={`${fieldClass} max-w-full`}>
              <option value="" disabled>{t('memory.refinementConflictChoose')}</option>
              {hasRecommendation && <option value={GENERATED_CONFLICT_CHOICE}>{t('memory.refinementGeneratedProposal')}{t('memory.refinementRecommended')}</option>}
              {action.originals.map((memory, index) => <option key={memory.id} value={memory.id}>{t('memory.refinementConflictSourceOption', { number: index + 1, time: formatTime(memory.event_time ?? memory.updated_at) })}</option>)}
            </select>
          </label>}
          <label htmlFor={editable ? `${id}-content` : undefined} className="block text-xs font-semibold text-gray-800 dark:text-gray-100">{t(action.kind === 'conflict' ? 'memory.refinementConflictContent' : action.kind === 'metadata' ? 'memory.refinementProposedMetadata' : 'memory.refinementProposedContent')}</label>
          {editable ? <>
            <textarea id={`${id}-content`} rows={6} value={draft} onChange={(event) => onEdit(event.target.value)} disabled={disabled || action.status !== 'pending' || unresolved} placeholder={action.kind === 'conflict' ? t('memory.refinementConflictChoose') : undefined} aria-invalid={empty} aria-describedby={empty ? `${id}-empty` : action.kind === 'conflict' ? `${id}-conflict-hint` : `${id}-draft-hint`} className={`${fieldClass} max-h-80 w-full resize-y leading-6`} />
            {action.kind !== 'conflict' && <p id={`${id}-draft-hint`} className="text-xs text-gray-600 dark:text-gray-300">{t('memory.refinementDraftHint')}</p>}
            {empty && <p id={`${id}-empty`} className="text-xs text-red-800 dark:text-red-200">{t('memory.refinementEmptyContent')}</p>}
          </> : action.content && <p className="max-h-48 overflow-y-auto whitespace-pre-wrap break-words text-sm leading-6 text-gray-800 [overflow-wrap:anywhere] dark:text-gray-100">{action.content}</p>}
          {action.kind !== 'conflict' ? <MemoryMetadata memory={action} /> : conflictSource && <MemoryMetadata memory={conflictSource} />}
        </div>}
        <div><h5 className="text-xs font-semibold text-gray-800 dark:text-gray-100">{t('memory.refinementReason')}</h5><p className="mt-1 max-h-40 overflow-y-auto whitespace-pre-wrap break-words text-sm leading-6 text-gray-700 [overflow-wrap:anywhere] dark:text-gray-200">{action.reason}</p></div>
        {action.evidence.length > 0 && <div><h5 className="text-xs font-semibold text-gray-800 dark:text-gray-100">{t('memory.refinementEvidence')}</h5><ul className="mt-1 max-h-48 list-disc space-y-1 overflow-y-auto pl-5 text-xs leading-5 text-gray-600 dark:text-gray-300">{action.evidence.map((evidence, index) => <li key={index} className="whitespace-pre-wrap break-words [overflow-wrap:anywhere]">{evidence}</li>)}</ul></div>}
        {action.message && <p className="whitespace-pre-wrap break-words text-xs leading-5 text-amber-900 [overflow-wrap:anywhere] dark:text-amber-200">{action.message}</p>}
      </div>
      {action.kind === 'conflict' && <details className="text-xs text-gray-600 dark:text-gray-300">
        <summary className="w-fit cursor-pointer rounded font-medium focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-violet-600">{t('memory.refinementReviewOriginals', { count: action.originals.length })}</summary>
        <div className="mt-2"><OriginalRecords originals={action.originals} domains={domains} /></div>
      </details>}
    </div>
    {(action.status === 'pending' || action.status === 'skipped') && <footer className="flex flex-wrap items-center gap-2">
      {selectable && <button type="button" className={primaryClass} disabled={disabled || empty || unresolved} onClick={onApply}>
        {working === 'apply' ? <Loader2 size={14} className="animate-spin motion-reduce:animate-none" aria-hidden="true" /> : <Check size={14} aria-hidden="true" />}
        {t(working === 'apply' ? 'memory.refinementApplyingOne' : action.kind === 'conflict' ? 'memory.refinementConflictApply' : action.kind === 'archive' ? 'memory.refinementArchiveOne' : action.kind === 'merge' ? 'memory.refinementMergeOne' : 'memory.refinementApplyOne')}
      </button>}
      {action.status === 'pending' && <button type="button" className={buttonClass} disabled={disabled} onClick={onSkip}>
        {working === 'skip' && <Loader2 size={14} className="animate-spin motion-reduce:animate-none" aria-hidden="true" />}
        {t(working === 'skip' ? 'memory.refinementSkipping' : action.kind === 'conflict' ? 'memory.refinementConflictKeepBoth' : 'memory.refinementSkipOne')}
      </button>}
      <button type="button" className={dangerClass} disabled={disabled} onClick={onDelete}>
        {working === 'delete' ? <Loader2 size={14} className="animate-spin motion-reduce:animate-none" aria-hidden="true" /> : <Trash2 size={14} aria-hidden="true" />}
        {t(working === 'delete' ? 'memory.refinementDeleting' : 'memory.refinementDeleteOne')}
      </button>
      <p className="text-xs leading-5 text-gray-600 dark:text-gray-300">{t('memory.refinementOneHint')}</p>
    </footer>}
  </article>
}

export function MemoryRefinementPanel({ domains, onChanged }: { domains: readonly MemoryDomainItem[]; onChanged: () => Promise<void> }) {
  const { t } = useTranslation()
  const id = useId()
  const { confirm, confirmDialog } = useConfirmDialog()
  const [confirmingDelete, setConfirmingDelete] = useState(false)
  const deleteConfirmation = useRef(false)
  const [scope, setScope] = useState('')
  const [incremental, setIncremental] = useState(true)
  const [history, setHistory] = useState<RefinementRun[]>([])
  const [run, setRun] = useState<RefinementRun | null>(null)
  const [loadingHistory, setLoadingHistory] = useState(true)
  const [opening, setOpening] = useState(false)
  const [busy, setBusy] = useState<Operation | null>(null)
  const [busyActionId, setBusyActionId] = useState<string | null>(null)
  const [errors, setErrors] = useState<Partial<Record<ErrorKey, string>>>({})
  const [success, setSuccess] = useState<'apply' | 'skip' | 'delete' | 'undo' | 'forget' | 'clear' | null>(null)
  const [selected, setSelected] = useState<Set<string>>(() => new Set())
  const [drafts, setDrafts] = useState<Record<string, string>>({})
  const [labelDrafts, setLabelDrafts] = useState<Record<string, string[]>>({})
  const [conflictChoices, setConflictChoices] = useState<Record<string, string>>({})
  const [conflictDrafts, setConflictDrafts] = useState<Record<string, Record<string, string>>>({})
  const [suggesting, setSuggesting] = useState<{ runId: string; actionId: string } | null>(null)
  const [reviewRefreshNeeded, setReviewRefreshNeeded] = useState(false)
  const suggestionInFlight = useRef(false)
  const [page, setPage] = useState(0)
  const mounted = useRef(false)
  const operationInFlight = useRef(false)
  const historyInFlight = useRef(false)
  const openingInFlight = useRef(false)
  const pollInFlight = useRef(false)
  const progressRequest = useRef<Promise<RefinementRun> | null>(null)
  const viewVersion = useRef(0)
  const selectedRunId = useRef<string | null>(null)
  const refreshInFlight = useRef(false)
  const refreshQueued = useRef(false)

  const clearError = useCallback((key: ErrorKey) => setErrors((previous) => ({ ...previous, [key]: undefined })), [])
  const showError = useCallback((key: ErrorKey, error: unknown) => {
    if (mounted.current) setErrors((previous) => ({ ...previous, [key]: readableError(error) }))
  }, [])
  const resetReview = useCallback(() => {
    setDrafts({})
    setLabelDrafts({})
    setConflictChoices({})
    setConflictDrafts({})
    setReviewRefreshNeeded(false)
    setSelected(new Set())
    setPage(0)
    setSuccess(null)
    setErrors((previous) => ({ history: previous.history }))
  }, [])
  const acceptRun = useCallback((next: RefinementRun) => {
    selectedRunId.current = next.id
    setRun(next)
    setHistory((previous) => [next, ...previous.filter((item) => item.id !== next.id)]
      .sort((left, right) => right.created_at.localeCompare(left.created_at)).slice(0, 20))
    const pending = new Set(next.actions.filter(isSelectable).map((action) => action.id))
    setSelected((previous) => new Set([...previous].filter((actionId) => pending.has(actionId))))
    // Proposals are keyed by action and only reset when opening another run.
    // Polls and resume responses must never overwrite content, labels or the
    // explicit source selected for a human conflict decision.
  }, [])

  const loadHistory = useCallback(async (initial = false) => {
    if (historyInFlight.current) return
    historyInFlight.current = true
    setLoadingHistory(true)
    clearError('history')
    // A manual refresh supersedes any older progress response already in flight.
    const version = ++viewVersion.current
    try {
      const entries = (await invoke<RefinementRun[]>('memory_refinement_list')).slice(0, 20)
      if (!mounted.current || version !== viewVersion.current) return
      setHistory(entries)
      if (initial && !selectedRunId.current && entries[0]) acceptRun(entries[0])
      else if (version === viewVersion.current) {
        const current = entries.find((item) => item.id === selectedRunId.current)
        if (current) acceptRun(current)
      }
    } catch (error) { showError('history', error) }
    finally {
      historyInFlight.current = false
      if (mounted.current) setLoadingHistory(false)
    }
  }, [acceptRun, clearError, showError])

  useEffect(() => {
    mounted.current = true
    void loadHistory(true)
    return () => { mounted.current = false; viewVersion.current += 1 }
  }, [loadHistory])

  const running = isRunning(run)
  const runId = run?.id
  useEffect(() => {
    if (!runId || (!running && !reviewRefreshNeeded)) return
    let disposed = false
    const timer = window.setInterval(() => {
      if (pollInFlight.current || operationInFlight.current || openingInFlight.current || historyInFlight.current) return
      pollInFlight.current = true
      const version = viewVersion.current
      const request = invoke<RefinementRun>('memory_refinement_get', { runId })
      progressRequest.current = request
      void request.then((next) => {
        if (!disposed && mounted.current && version === viewVersion.current && selectedRunId.current === runId) {
          clearError('poll')
          setReviewRefreshNeeded(false)
          acceptRun(next)
        }
      }).catch((error: unknown) => {
        if (!disposed && version === viewVersion.current) showError('poll', error)
      }).finally(() => {
        progressRequest.current = null
        pollInFlight.current = false
      })
    }, 2_000)
    return () => { disposed = true; window.clearInterval(timer) }
  }, [runId, running, reviewRefreshNeeded, acceptRun, clearError, showError])

  async function openRun(runId: string) {
    if (operationInFlight.current || openingInFlight.current || !runId) return
    const entry = history.find((item) => item.id === runId)
    if (!entry) return
    const version = ++viewVersion.current
    resetReview()
    acceptRun(entry)
    openingInFlight.current = true
    setOpening(true)
    try {
      // Finish any previous progress read before opening another saved run.
      // The opening flag prevents the interval from starting another read.
      if (progressRequest.current) await progressRequest.current.catch(() => {})
      if (!mounted.current || version !== viewVersion.current) return
      const next = await invoke<RefinementRun>('memory_refinement_get', { runId })
      if (mounted.current && version === viewVersion.current) acceptRun(next)
    } catch (error) { if (version === viewVersion.current) showError('open', error) }
    finally {
      openingInFlight.current = false
      if (mounted.current) setOpening(false)
    }
  }

  async function refreshMemoryCenter() {
    if (refreshInFlight.current) { refreshQueued.current = true; return }
    refreshInFlight.current = true
    try {
      do {
        refreshQueued.current = false
        if (!mounted.current) break
        clearError('refresh')
        try { await onChanged() } catch (error) { showError('refresh', error) }
      } while (refreshQueued.current && mounted.current)
    } finally { refreshInFlight.current = false }
  }

  async function perform(operation: Operation, command: string, args: Record<string, unknown>, actionId?: string) {
    if (operationInFlight.current || openingInFlight.current || deleteConfirmation.current) return
    operationInFlight.current = true
    const version = ++viewVersion.current
    let received = false
    setBusy(operation)
    setBusyActionId(actionId ?? null)
    setSuccess(null)
    clearError(operation)
    try {
      const next = await invoke<RefinementRun>(command, args)
      if (!mounted.current || version !== viewVersion.current) return
      if (operation === 'start') resetReview()
      acceptRun(next)
      received = true
      if (operation === 'apply' || operation === 'skip' || operation === 'delete' || operation === 'undo') setSuccess(operation)
    } catch (error) { showError(operation, error) }
    finally {
      operationInFlight.current = false
      if (mounted.current) {
        setBusy(null)
        setBusyActionId(null)
      }
      if (mounted.current && version === viewVersion.current) {
        // Inventory refresh is coalesced in the background so a slow importance
        // refresh cannot lock every per-proposal button after a completed write.
        if (operation === 'apply' || operation === 'delete' || operation === 'undo') void refreshMemoryCenter()
        if (!received) void loadHistory(!selectedRunId.current)
      }
    }
  }

  const selectedActions = useMemo(() => run?.actions.filter((action) => selected.has(action.id) && isSelectable(action)) ?? [], [run, selected])
  const unresolvedConflictSelection = selectedActions.some((action) => action.kind === 'conflict'
    && (!action.originals.some((memory) => memory.id === conflictSourceForAction(action)) || !draftForAction(action).trim()))
  const invalidSelection = unresolvedConflictSelection || selectedActions.some((action) => isEditable(action) && !draftForAction(action).trim())
  const archiveCount = selectedActions.filter((action) => action.kind === 'archive').length
  const pendingActions = useMemo(() => run?.actions.filter(isSelectable) ?? [], [run])
  const safeActions = pendingActions.filter((action) => ['rewrite', 'merge', 'metadata'].includes(action.kind)).slice(0, MAX_SELECTIONS)
  const visibleActions = useMemo(() => run?.actions.filter((action) => action.status !== 'applied' && action.status !== 'deleted') ?? [], [run])
  const pages = Math.max(1, Math.ceil(visibleActions.length / ACTION_PAGE_SIZE))
  const currentPage = Math.min(page, pages - 1)
  useEffect(() => {
    setPage((previous) => Math.min(previous, pages - 1))
  }, [pages])
  const controlsDisabled = busy !== null || opening || confirmingDelete
  const batchApplying = busy === 'apply' && busyActionId === null
  const batchSkipping = busy === 'skip' && busyActionId === null
  const reviewDisabled = controlsDisabled || Boolean(errors.open)
  const knownRunning = running || history.some((item) => item.id !== run?.id && isRunning(item))
  const canResume = run && run.remaining > 0 && ['cancelled', 'failed', 'ready', 'applied', 'reverted'].includes(run.status)
  const canUndo = run && run.status !== 'reverted' && run.actions.some((action) => action.status === 'applied')
  const emptyListKey = (run?.actions.length ?? 0) > 0
    ? running ? 'memory.refinementAwaitingMoreActions' : 'memory.refinementNoRemainingActions'
    : running ? 'memory.refinementAwaitingActions' : 'memory.refinementNoActions'
  const runOptions = run && !history.some((item) => item.id === run.id) ? [run, ...history].slice(0, 20) : history
  const scopeValid = scope === '' || domains.some((domain) => domain.id === scope)
  const scopeLabel = (domainId: string | null) => domainId === null ? t('memory.refinementScopeGlobal')
    : domains.find((domain) => domain.id === domainId)?.name ?? t('memory.refinementScopeDomainRun')

  function conflictChoiceForAction(action: RefinementAction): string | undefined {
    const hasRecommendation = action.originals.some((memory) => memory.id === action.recommended_source_id)
    return conflictChoices[action.id] ?? (hasRecommendation ? GENERATED_CONFLICT_CHOICE : undefined)
  }

  function conflictSourceForAction(action: RefinementAction): string | undefined {
    const choice = conflictChoiceForAction(action)
    return choice === GENERATED_CONFLICT_CHOICE ? action.recommended_source_id ?? undefined : choice
  }

  function draftForAction(action: RefinementAction): string {
    if (action.kind !== 'conflict') return drafts[action.id] ?? action.content
    const choice = conflictChoiceForAction(action)
    if (!choice) return ''
    return conflictDrafts[action.id]?.[choice] ?? (choice === GENERATED_CONFLICT_CHOICE
      ? action.content : action.originals.find((memory) => memory.id === choice)?.memory ?? '')
  }

  function chooseConflictSource(action: RefinementAction, choice: string) {
    if (reviewDisabled || !isSelectable(action) || action.kind !== 'conflict') return
    const valid = choice === GENERATED_CONFLICT_CHOICE
      ? action.originals.some((memory) => memory.id === action.recommended_source_id)
      : action.originals.some((memory) => memory.id === choice)
    if (valid) setConflictChoices((previous) => ({ ...previous, [action.id]: choice }))
  }

  function editDraft(action: RefinementAction, content: string) {
    if (action.kind !== 'conflict') {
      setDrafts((previous) => ({ ...previous, [action.id]: content }))
      return
    }
    const choice = conflictChoiceForAction(action)
    if (choice) setConflictDrafts((previous) => ({ ...previous,
      [action.id]: { ...previous[action.id], [choice]: content },
    }))
  }

  function selectionForAction(action: RefinementAction): RefinementSelection {
    const selection: RefinementSelection = { action_id: action.id }
    if (isEditable(action)) selection.content = draftForAction(action).trim()
    if (action.kind === 'conflict') selection.conflict_source_id = conflictSourceForAction(action)
    if (action.kind !== 'archive' && labelDrafts[action.id] !== undefined) selection.domain_ids = labelDrafts[action.id]
    return selection
  }

  async function generateSuggestion(action: RefinementAction) {
    if (!run || reviewDisabled || suggestionInFlight.current || !isSelectable(action) || action.kind !== 'conflict') return
    const runId = run.id
    const version = viewVersion.current
    suggestionInFlight.current = true
    setSuggesting({ runId, actionId: action.id })
    clearError('suggest')
    try {
      const next = await invoke<RefinementRun>('memory_refinement_suggest', { runId, actionId: action.id })
      if (!mounted.current || selectedRunId.current !== runId) return
      const recommendation = next.actions.find((item) => item.id === action.id && item.status === 'pending' && item.recommended_source_id)
      const sourceId = recommendation?.recommended_source_id
      if (recommendation && sourceId) {
        setConflictDrafts((previous) => ({ ...previous,
          [action.id]: { ...previous[action.id], [GENERATED_CONFLICT_CHOICE]: recommendation.content },
        }))
        setConflictChoices((previous) => ({ ...previous, [action.id]: GENERATED_CONFLICT_CHOICE }))
      }
      if (version === viewVersion.current && !operationInFlight.current && !openingInFlight.current && !historyInFlight.current) {
        // Discard older progress reads. If another operation already advanced
        // the view, fetch again instead of restoring this response over it.
        viewVersion.current += 1
        acceptRun(next)
      } else setReviewRefreshNeeded(true)
    } catch (error) {
      if (mounted.current && selectedRunId.current === runId) showError('suggest', error)
    } finally {
      suggestionInFlight.current = false
      if (mounted.current) setSuggesting(null)
    }
  }

  function applyAction(action: RefinementAction) {
    if (!run || reviewDisabled || !isSelectable(action)) return
    const selection = selectionForAction(action)
    if (isEditable(action) && !selection.content) return
    if (action.kind === 'conflict' && !action.originals.some((memory) => memory.id === selection.conflict_source_id)) return
    void perform('apply', 'memory_refinement_apply', { runId: run.id, selections: [selection] }, action.id)
  }

  function skipAction(action: RefinementAction) {
    if (!run || reviewDisabled || action.status !== 'pending') return
    void perform('skip', 'memory_refinement_skip', { runId: run.id, actionIds: [action.id] }, action.id)
  }

  async function requestDelete(actions: readonly RefinementAction[]) {
    if (!run || reviewDisabled || operationInFlight.current || deleteConfirmation.current || actions.length === 0 || actions.length > MAX_SELECTIONS) return
    if (actions.some((action) => action.status !== 'pending' && action.status !== 'skipped')) return
    const runId = run.id
    const selections: RefinementDeleteSelection[] = actions.map((action) => ({ action_id: action.id, expected_status: action.status }))
    const originals = new Map(actions.flatMap((action) => action.originals.map((memory) => [memory.id, memory] as const)))
    const preview = [...originals.values()].slice(0, 2)
    deleteConfirmation.current = true
    setConfirmingDelete(true)
    let confirmed: boolean
    try {
      confirmed = await confirm({
        title: t('memory.refinementDeleteConfirmTitle'),
        message: t('memory.refinementDeleteConfirmMessage', { groups: actions.length, memories: originals.size })
          + (originals.size > preview.length ? `\n${t('memory.refinementDeletePreviewMore', { shown: preview.length, count: originals.size })}` : ''),
        detailLines: preview.map((memory, index) => {
          const text = Array.from(memory.memory)
          return {
            label: t('memory.refinementDeletePreviewLabel', { number: index + 1 }),
            value: text.length > 120 ? `${text.slice(0, 120).join('')}…` : memory.memory,
          }
        }),
        confirmLabel: t('memory.refinementDeleteRelated'),
        tone: 'danger',
      })
    } finally {
      deleteConfirmation.current = false
      if (mounted.current) setConfirmingDelete(false)
    }
    if (!confirmed || !mounted.current || selectedRunId.current !== runId) return
    void perform('delete', 'memory_refinement_delete', { runId, selections }, selections.length === 1 ? selections[0].action_id : undefined)
  }

  async function requestForgetRun(target: RefinementRun) {
    if (controlsDisabled || loadingHistory || isRunning(target) || suggesting?.runId === target.id
      || operationInFlight.current || openingInFlight.current || deleteConfirmation.current) return
    const viewedId = selectedRunId.current
    deleteConfirmation.current = true
    setConfirmingDelete(true)
    let confirmed: boolean
    try {
      confirmed = await confirm({
        title: t('memory.refinementForgetTitle'),
        message: t('memory.refinementForgetMessage'),
        detailLines: [{ label: t('memory.refinementHistory'), value: t('memory.refinementHistoryItem', {
          time: formatTime(target.created_at), scope: scopeLabel(target.domain_id), status: t(`memory.refinementStatus_${target.status}`),
        }) }],
        confirmLabel: t('memory.refinementForget'),
        tone: 'danger',
      })
    } finally {
      deleteConfirmation.current = false
      if (mounted.current) setConfirmingDelete(false)
    }
    if (!confirmed || !mounted.current || selectedRunId.current !== viewedId || operationInFlight.current) return
    operationInFlight.current = true
    const version = ++viewVersion.current
    setBusy('forget')
    setSuccess(null)
    clearError('forget')
    let deleted = false
    let replaceSelection = false
    try {
      await invoke('memory_refinement_forget', { runId: target.id, expectedUpdatedAt: target.updated_at })
      if (!mounted.current) return
      viewVersion.current += 1
      setHistory((previous) => previous.filter((item) => item.id !== target.id))
      replaceSelection = selectedRunId.current === target.id
      if (replaceSelection) {
        // Only the deleted selection needs a reset; other rows preserve drafts.
        selectedRunId.current = null
        setRun(null)
        resetReview()
      }
      setSuccess('forget')
      deleted = true
    } catch (error) {
      if (mounted.current && selectedRunId.current === viewedId) showError('forget', error)
    } finally {
      operationInFlight.current = false
      if (mounted.current) setBusy(null)
    }
    if (mounted.current && (deleted || viewVersion.current === version)) {
      await loadHistory(replaceSelection)
    }
  }

  async function requestClearHistory() {
    if (controlsDisabled || loadingHistory || knownRunning || suggesting || operationInFlight.current
      || openingInFlight.current || deleteConfirmation.current || runOptions.length === 0) return
    const viewedId = selectedRunId.current
    deleteConfirmation.current = true
    setConfirmingDelete(true)
    clearError('clear')
    let snapshot: RefinementHistorySnapshot | null = null
    let confirmed = false
    try {
      snapshot = await invoke<RefinementHistorySnapshot>('memory_refinement_history_snapshot')
      if (!mounted.current || selectedRunId.current !== viewedId) return
      confirmed = snapshot.count === 0 || await confirm({
        title: t('memory.refinementClearTitle'),
        message: t('memory.refinementClearMessage', { count: snapshot.count }),
        confirmLabel: t('memory.refinementClearHistory'),
        tone: 'danger',
      })
    } catch (error) { showError('clear', error) }
    finally {
      deleteConfirmation.current = false
      if (mounted.current) setConfirmingDelete(false)
    }
    if (!confirmed || !snapshot || !mounted.current || selectedRunId.current !== viewedId || operationInFlight.current) return
    operationInFlight.current = true
    viewVersion.current += 1
    setBusy('clear')
    setSuccess(null)
    try {
      await invoke<number>('memory_refinement_clear', { expectedRevision: snapshot.revision })
      if (!mounted.current) return
      viewVersion.current += 1
      selectedRunId.current = null
      setRun(null)
      setHistory([])
      resetReview()
      setSuccess('clear')
    } catch (error) { showError('clear', error) }
    finally {
      operationInFlight.current = false
      if (mounted.current) setBusy(null)
    }
    if (mounted.current) await loadHistory(!selectedRunId.current)
  }

  function applySelection() {
    if (!run || reviewDisabled || selectedActions.length === 0 || selectedActions.length > MAX_SELECTIONS || invalidSelection) return
    const selections = selectedActions.map(selectionForAction)
    void perform('apply', 'memory_refinement_apply', { runId: run.id, selections })
  }

  return <section aria-labelledby={`${id}-title`} className="min-w-0 space-y-4 border-t border-gray-200 pt-4 dark:border-gray-700">
    <div>
      <h3 id={`${id}-title`} className="text-sm font-semibold text-gray-800 dark:text-gray-100">{t('memory.refinementTitle')}</h3>
      <p className="mt-1 w-full max-w-none whitespace-normal break-words text-xs leading-5 text-gray-600 dark:text-gray-300">{t('memory.refinementHint')}</p>
    </div>
    <div className="flex flex-wrap items-end gap-3">
      <label className="flex min-w-0 flex-col gap-1 text-xs font-medium text-gray-700 dark:text-gray-200">
        {t('memory.refinementScope')}
        <select value={scope} onChange={(event) => setScope(event.target.value)} disabled={controlsDisabled || knownRunning} aria-invalid={!scopeValid} aria-describedby={!scopeValid ? `${id}-scope-error` : undefined} className={`${fieldClass} w-full min-w-48 sm:max-w-80`}>
          <option value="">{t('memory.refinementScopeGlobal')}</option>
          {!scopeValid && <option value={scope} disabled>{t('memory.refinementScopeUnavailable')}</option>}
          {domains.map((domain) => <option key={domain.id} value={domain.id}>{domain.name}</option>)}
        </select>
      </label>
      <label className="inline-flex min-h-8 items-center gap-2 text-xs text-gray-700 dark:text-gray-200">
        <input type="checkbox" checked={incremental} onChange={(event) => setIncremental(event.target.checked)} disabled={controlsDisabled || knownRunning} aria-describedby={`${id}-incremental-hint`} className="h-4 w-4 focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-violet-600 disabled:opacity-50" />
        {t('memory.refinementIncremental')}
      </label>
      <button type="button" className={primaryClass} disabled={controlsDisabled || knownRunning || loadingHistory || Boolean(errors.history) || !scopeValid} onClick={() => { void perform('start', 'memory_refinement_start', { domainId: scope || null, incremental }) }}>
        {busy === 'start' ? <Loader2 size={14} className="animate-spin motion-reduce:animate-none" aria-hidden="true" /> : <RefreshCw size={14} aria-hidden="true" />}{t(busy === 'start' ? 'memory.refinementStarting' : 'memory.refinementStart')}
      </button>
    </div>
    {!scopeValid && <p id={`${id}-scope-error`} role="status" className="text-xs leading-5 text-amber-900 dark:text-amber-200">{t('memory.refinementScopeUnavailable')}</p>}
    <p id={`${id}-incremental-hint`} className="text-xs leading-5 text-gray-600 dark:text-gray-300">{t('memory.refinementIncrementalHint')}</p>
    {knownRunning && !running && <p role="status" className="text-xs text-gray-600 dark:text-gray-300">{t('memory.refinementOtherRunning')}</p>}
    <div className="flex flex-wrap items-end gap-2">
      <div className="flex min-w-0 flex-1 flex-col gap-1 text-xs font-medium text-gray-700 dark:text-gray-200">
        <span>{t('memory.refinementHistory')}</span>
        <RefinementHistoryMenu className={`${fieldClass} w-full`} value={run?.id ?? ''}
          disabled={controlsDisabled || loadingHistory} label={t('memory.refinementHistory')}
          placeholder={t(loadingHistory ? 'memory.refinementLoadingHistory' : 'memory.refinementNoHistory')}
          deleteLabel={t('memory.refinementForget')} deleteHint={t('memory.refinementForgetHint')}
          options={runOptions.map((item) => ({ id: item.id,
            label: t('memory.refinementHistoryItem', { time: formatTime(item.created_at), scope: scopeLabel(item.domain_id), status: t(`memory.refinementStatus_${item.status}`) }),
            canDelete: !isRunning(item) && suggesting?.runId !== item.id,
          }))}
          onSelect={(runId) => { void openRun(runId) }}
          onDelete={(runId) => { const target = runOptions.find((item) => item.id === runId); if (target) void requestForgetRun(target) }} />
      </div>
      <button type="button" className={buttonClass} disabled={controlsDisabled || loadingHistory} onClick={() => { void loadHistory(!run) }} aria-label={t('memory.refinementRefreshHistory')}>
        <RefreshCw size={14} className={loadingHistory ? 'animate-spin motion-reduce:animate-none' : ''} aria-hidden="true" />{t('memory.refinementRefreshHistory')}
      </button>
      <button type="button" className={dangerClass} disabled={controlsDisabled || loadingHistory || runOptions.length === 0 || knownRunning || suggesting !== null} title={t('memory.refinementClearHint')} onClick={() => { void requestClearHistory() }}>
        {busy === 'clear' ? <Loader2 size={14} className="animate-spin motion-reduce:animate-none" aria-hidden="true" /> : <Trash2 size={14} aria-hidden="true" />}{t(busy === 'clear' ? 'memory.refinementClearing' : 'memory.refinementClearHistory')}
      </button>
    </div>
    {opening && <p role="status" className="flex items-center gap-2 text-xs text-gray-600 dark:text-gray-300"><Loader2 size={14} className="animate-spin motion-reduce:animate-none" aria-hidden="true" />{t('memory.refinementLoadingRun')}</p>}
    {(Object.keys(errorLabels) as ErrorKey[]).map((key) => errors[key] ? <div key={key} role="alert" className="space-y-1 rounded-md border border-red-200 bg-red-50 p-3 text-xs leading-5 text-red-800 dark:border-red-900/70 dark:bg-red-950/30 dark:text-red-200">
      <p className="font-semibold">{t(errorLabels[key])}</p><p className="whitespace-pre-wrap break-words [overflow-wrap:anywhere]">{errors[key]}</p>
      <p>{t(key === 'poll' ? 'memory.refinementPollRetryHint' : key === 'refresh' ? 'memory.refinementRefreshRetryHint' : 'memory.refinementRetryHint')}</p>
      {key === 'open' && run && <button type="button" className={buttonClass} disabled={controlsDisabled} onClick={() => { void openRun(run.id) }}>{t('memory.refinementReloadRun')}</button>}
    </div> : null)}
    {success && <p role="status" className="rounded-md bg-green-50 p-3 text-xs leading-5 text-green-800 dark:bg-green-950/30 dark:text-green-200">{t(`memory.refinementSuccess_${success}`)}</p>}
    {run && <>
      {(running || canResume) && <div className="flex flex-wrap items-center gap-2">
        {running && run.activity && <p role="status" aria-live="polite" className="min-w-0 flex-1 text-xs leading-5 text-sky-800 dark:text-sky-200">{run.activity}</p>}
        {running && <button type="button" className={buttonClass} disabled={controlsDisabled || run.status === 'cancelling'} onClick={() => { void perform('cancel', 'memory_refinement_cancel', { runId: run.id }) }}>{busy === 'cancel' && <Loader2 size={14} className="animate-spin motion-reduce:animate-none" aria-hidden="true" />}{t(run.status === 'cancelling' || busy === 'cancel' ? 'memory.refinementCancelling' : 'memory.refinementCancel')}</button>}
        {canResume && <button type="button" className={buttonClass} disabled={reviewDisabled} onClick={() => { void perform('resume', 'memory_refinement_resume', { runId: run.id }) }}>{busy === 'resume' && <Loader2 size={14} className="animate-spin motion-reduce:animate-none" aria-hidden="true" />}{t(busy === 'resume' ? 'memory.refinementResuming' : 'memory.refinementResume')}</button>}
      </div>}
      {run.warnings.length > 0 && <div className="rounded-md border border-amber-200 bg-amber-50 p-3 text-xs leading-5 text-amber-900 dark:border-amber-900/70 dark:bg-amber-950/30 dark:text-amber-200">
        <h4 className="flex items-center gap-2 font-semibold"><AlertTriangle size={14} aria-hidden="true" />{t('memory.refinementWarnings')}</h4>
        <ul className="mt-1 max-h-40 list-disc space-y-1 overflow-y-auto pl-5">{run.warnings.map((warning, index) => <li key={index} className="whitespace-pre-wrap break-words [overflow-wrap:anywhere]">{warning}</li>)}</ul>
      </div>}
      <div className="space-y-2">
        <div className="flex flex-wrap items-center gap-2">
          <h4 className="mr-auto text-sm font-semibold text-gray-800 dark:text-gray-100">{t('memory.refinementReviewTitle', { count: visibleActions.length })}</h4>
          <button type="button" className={buttonClass} disabled={reviewDisabled || safeActions.length === 0} onClick={() => setSelected(new Set(safeActions.map((action) => action.id)))}>{t('memory.refinementSelectSafe')}</button>
          <button type="button" className={buttonClass} disabled={reviewDisabled || selected.size === 0} onClick={() => setSelected(new Set())}>{t('memory.refinementClearSelection')}</button>
        </div>
        {running && <p role="status" aria-live="polite" className="text-xs tabular-nums leading-5 text-sky-800 dark:text-sky-200">{t('memory.refinementLiveReviewHint', { processed: run.processed, total: run.total })}</p>}
        <p id={`${id}-selection-hint`} className="text-xs leading-5 text-gray-600 dark:text-gray-300">{t('memory.refinementSelectionHint')}</p>
        {visibleActions.length > 0 ? <>
          <div role="region" aria-label={t('memory.refinementReviewList')} tabIndex={0} className="max-h-[65vh] min-w-0 overflow-y-auto overscroll-contain pr-2 focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-violet-600">
            {visibleActions.slice(currentPage * ACTION_PAGE_SIZE, (currentPage + 1) * ACTION_PAGE_SIZE).map((action, index) => <ActionReview
              key={`${run.id}:${action.id}`}
              action={action}
              number={currentPage * ACTION_PAGE_SIZE + index + 1}
              selected={selected.has(action.id)}
              draft={action.status === 'pending' ? draftForAction(action) : action.content}
              conflictChoice={conflictChoiceForAction(action)}
              conflictSourceId={conflictSourceForAction(action)}
              domainIds={action.status === 'pending' ? labelDrafts[action.id] ?? actionDomainIds(action) : actionDomainIds(action)}
              domains={domains}
              disabled={reviewDisabled || (suggesting?.runId === run.id && suggesting.actionId === action.id)}
              suggestionDisabled={suggesting !== null}
              working={suggesting?.runId === run.id && suggesting.actionId === action.id ? 'suggest' : busyActionId === action.id && (busy === 'apply' || busy === 'skip' || busy === 'delete') ? busy : null}
              onSuggest={() => { void generateSuggestion(action) }}
              onApply={() => applyAction(action)}
              onSkip={() => skipAction(action)}
              onDelete={() => { void requestDelete([action]) }}
              selectionFull={selectedActions.length >= MAX_SELECTIONS}
              selectionHintId={`${id}-selection-hint`}
              onSelect={() => setSelected((previous) => {
                const next = new Set(previous)
                if (next.has(action.id)) next.delete(action.id)
                else if (next.size < MAX_SELECTIONS) next.add(action.id)
                return next
              })}
              onEdit={(content) => editDraft(action, content)}
              onConflictSource={(sourceId) => chooseConflictSource(action, sourceId)}
              onEditLabels={(ids) => setLabelDrafts((previous) => ({ ...previous, [action.id]: ids }))}
            />)}
          </div>
          <Pagination page={currentPage} pages={pages} onChange={setPage} label={t('memory.refinementActionPages')} />
        </> : <p className="py-3 text-sm text-gray-600 dark:text-gray-300">{t(emptyListKey)}</p>}
      </div>
      <footer className="space-y-3 border-t border-gray-200 pt-3 dark:border-gray-700">
        <p className="text-xs text-gray-700 dark:text-gray-200">{t('memory.refinementSelected', { count: selectedActions.length })}</p>
        {archiveCount > 0 && <p className="text-xs leading-5 text-amber-900 dark:text-amber-200">{t('memory.refinementArchiveSelection', { count: archiveCount })}</p>}
        {unresolvedConflictSelection && <p role="status" className="text-xs leading-5 text-amber-900 dark:text-amber-200">{t('memory.refinementConflictSelectionIncomplete')}</p>}
        <div className="flex flex-wrap gap-2">
          <button type="button" className={primaryClass} disabled={reviewDisabled || selectedActions.length === 0 || invalidSelection} onClick={applySelection}>{batchApplying ? <Loader2 size={14} className="animate-spin motion-reduce:animate-none" aria-hidden="true" /> : <Check size={14} aria-hidden="true" />}{t(batchApplying ? 'memory.refinementApplying' : 'memory.refinementApply', { count: selectedActions.length })}</button>
          <button type="button" className={buttonClass} disabled={reviewDisabled || selectedActions.length === 0} onClick={() => { void perform('skip', 'memory_refinement_skip', { runId: run.id, actionIds: selectedActions.map((action) => action.id) }) }}>{batchSkipping && <Loader2 size={14} className="animate-spin motion-reduce:animate-none" aria-hidden="true" />}{t(batchSkipping ? 'memory.refinementSkipping' : 'memory.refinementSkip')}</button>
          <button type="button" className={dangerClass} disabled={reviewDisabled || selectedActions.length === 0} onClick={() => { void requestDelete(selectedActions) }}>
            {busy === 'delete' ? <Loader2 size={14} className="animate-spin motion-reduce:animate-none" aria-hidden="true" /> : <Trash2 size={14} aria-hidden="true" />}
            {t(busy === 'delete' ? 'memory.refinementDeleting' : 'memory.refinementDeleteRelated')}
          </button>
          <button type="button" className={`${buttonClass} sm:ml-auto`} disabled={reviewDisabled || running || !canUndo} onClick={() => { void perform('undo', 'memory_refinement_undo', { runId: run.id }) }}>{busy === 'undo' ? <Loader2 size={14} className="animate-spin motion-reduce:animate-none" aria-hidden="true" /> : <Undo2 size={14} aria-hidden="true" />}{t(busy === 'undo' ? 'memory.refinementUndoing' : 'memory.refinementUndo')}</button>
        </div>
        <p className="max-w-[75ch] text-xs leading-5 text-gray-600 dark:text-gray-300">{t('memory.refinementUndoHint')}</p>
      </footer>
    </>}
    {confirmDialog}
  </section>
}
