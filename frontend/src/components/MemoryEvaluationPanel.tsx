import { Fragment, useCallback, useEffect, useId, useRef, useState, type ReactNode } from 'react'
import { invoke } from '@tauri-apps/api/core'
import { open, save } from '@tauri-apps/plugin-dialog'
import { useTranslation } from 'react-i18next'
import type { EvaluationBackendInfo, EvaluationDatasetSummary, EvaluationPlan, EvaluationReport, EvaluationRunStatus } from '../types/memoryEvaluation'

const ns = 'memory.evaluation.'
const buttonClass = 'inline-flex min-h-8 items-center justify-center rounded-[6px] border border-[var(--app-control-stroke)] bg-[var(--app-surface)] px-2.5 py-1.5 text-xs font-medium text-[var(--app-text)] hover:bg-[var(--app-hover)] focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-[var(--app-focus)] disabled:cursor-not-allowed disabled:opacity-50'
const mutedClass = 'text-xs leading-5 text-[var(--app-secondary)]'
const cellClass = 'border-b border-[var(--app-stroke)] px-3 py-2 text-left align-top'
const terminal = (status: EvaluationRunStatus['status']) => ['completed', 'cancelled', 'failed'].includes(status)

// Only backend-approved model metadata and a small numeric diagnostic allowlist are rendered.
function safeText(value: unknown): string {
  const text = value instanceof Error ? value.message : typeof value === 'string' ? value : 'message' in Object(value) ? String((value as { message: unknown }).message) : String(value)
  return text.replace(/https?:\/\/[^\s"'<>]+/gi, '[redacted]')
    .replace(/\b(?:Bearer\s+\S+|sk-[\w-]+)\b/gi, '[redacted]')
    .replace(/((?:api[_-]?key|authorization|password|secret|token)["']?\s*[=:]\s*["']?)[^\s,;}"']+/gi, '$1[redacted]')
}

function Field({ label, children }: { label: string; children: ReactNode }) {
  return <div className="grid min-w-0 gap-1 sm:grid-cols-[12rem_minmax(0,1fr)] sm:gap-3"><dt className={mutedClass}>{label}</dt><dd className="min-w-0 whitespace-pre-wrap break-words text-sm [overflow-wrap:anywhere]">{children}</dd></div>
}

function Backend({ backend }: { backend: EvaluationBackendInfo }) {
  const { t } = useTranslation()
  return <dl className="space-y-2">
    {(['embedding_model', 'embedding_location', 'routing_model', 'routing_provider'] as const).map(key => <Field key={key} label={t(`${ns}${key}`)}>{backend[key] ? safeText(backend[key]) : t(`${ns}none`)}</Field>)}
    <Field label={t(`${ns}routing_remote`)}>{t(`${ns}${backend.routing_remote ? 'yes' : 'no'}`)}</Field>
  </dl>
}

function EvaluationFailure({ error, onRetry }: { error: string | null; onRetry?: () => void }) {
  const { t } = useTranslation()
  return error ? <div role="alert" className="space-y-2 rounded-[6px] bg-red-50 p-3 text-sm text-red-800 dark:bg-red-950/40 dark:text-red-200">
    <p className="whitespace-pre-wrap break-words [overflow-wrap:anywhere]">{error}</p>
    {onRetry && <button type="button" className={buttonClass} onClick={onRetry}>{t(`${ns}retry`)}</button>}
  </div> : null
}

export function MemoryEvaluationPanel() {
  const { t } = useTranslation()
  const id = useId()
  const alive = useRef(false)
  const seq = useRef({ datasets: 0, plan: 0, status: 0, report: 0, files: 0 })
  const starting = useRef(false)
  const cancelling = useRef(false)
  const statusInFlight = useRef(0)
  const [datasets, setDatasets] = useState<EvaluationDatasetSummary[]>([])
  const [datasetId, setDatasetId] = useState('')
  const [split, setSplit] = useState<EvaluationPlan['split']>('development')
  const [mode, setMode] = useState<EvaluationPlan['mode']>('hybrid')
  const [datasetsLoading, setDatasetsLoading] = useState(false)
  const [datasetsError, setDatasetsError] = useState<string | null>(null)
  const [plan, setPlan] = useState<EvaluationPlan | null>(null)
  const [planLoading, setPlanLoading] = useState(false)
  const [planError, setPlanError] = useState<string | null>(null)
  const [confirmed, setConfirmed] = useState(false)
  const [expired, setExpired] = useState(false)
  const [run, setRun] = useState<EvaluationRunStatus | null>(null)
  const [statusLoading, setStatusLoading] = useState(true)
  const [statusError, setStatusError] = useState<string | null>(null)
  const [runBusy, setRunBusy] = useState(false)
  const [runError, setRunError] = useState<string | null>(null)
  const [report, setReport] = useState<EvaluationReport | null>(null)
  const [reportLoading, setReportLoading] = useState(false)
  const [reportError, setReportError] = useState<string | null>(null)
  const [reportRetry, setReportRetry] = useState(0)
  const [fileBusy, setFileBusy] = useState(false)
  const [fileError, setFileError] = useState<string | null>(null)
  const [fileNotice, setFileNotice] = useState<string | null>(null)
  const activeRunId = run?.run_id
  const activeRunStatus = run?.status
  const active = run != null && !terminal(run.status)
  const selected = datasets.find(item => item.dataset_id === datasetId)
  const valid = (channel: keyof typeof seq.current, request: number) => alive.current && seq.current[channel] === request
  const number = (value: number | null | undefined) => value == null || !Number.isFinite(value) ? t(`${ns}unmeasured`) : value.toLocaleString(undefined, { maximumFractionDigits: 4 })
  const yesNo = (value: boolean | null) => t(`${ns}${value == null ? 'unmeasured' : value ? 'yes' : 'no'}`)
  const ids = (values: string[]) => values.join(' · ') || t(`${ns}none`)

  const loadDatasets = useCallback(async () => {
    const request = ++seq.current.datasets
    setDatasetsLoading(true)
    setDatasetsError(null)
    try {
      const result = await invoke<EvaluationDatasetSummary[]>('memory_retrieval_eval_datasets')
      if (!alive.current || seq.current.datasets !== request) return
      setDatasets(result)
      setDatasetId(current => current || result[0]?.dataset_id || '')
    } catch (error) {
      if (alive.current && seq.current.datasets === request) setDatasetsError(safeText(error))
    } finally {
      if (alive.current && seq.current.datasets === request) setDatasetsLoading(false)
    }
  }, [])

  const restoreStatus = useCallback(async (runId: string | null = null) => {
    if (statusInFlight.current || starting.current || cancelling.current) return
    const request = ++seq.current.status
    statusInFlight.current = request
    setStatusLoading(true)
    setStatusError(null)
    try {
      const result = await invoke<EvaluationRunStatus | null>('memory_retrieval_eval_status', { runId })
      if (alive.current && seq.current.status === request) setRun(result)
    } catch (error) {
      if (alive.current && seq.current.status === request) setStatusError(safeText(error))
    } finally {
      if (statusInFlight.current === request) {
        statusInFlight.current = 0
        if (alive.current) setStatusLoading(false)
      }
    }
  }, [])

  useEffect(() => {
    const counters = seq.current
    alive.current = true
    void loadDatasets()
    void restoreStatus()
    return () => {
      alive.current = false
      statusInFlight.current = 0
      for (const channel of Object.keys(counters) as (keyof typeof counters)[]) ++counters[channel]
    }
  }, [loadDatasets, restoreStatus])

  useEffect(() => {
    if (!plan) { setExpired(false); return }
    const remaining = Date.parse(plan.expires_at) - Date.now()
    setExpired(!Number.isFinite(remaining) || remaining <= 0)
    const timer = window.setTimeout(() => { setExpired(true); setConfirmed(false) }, Math.max(0, Math.min(remaining || 0, 600000)))
    return () => window.clearTimeout(timer)
  }, [plan])

  useEffect(() => {
    if (!activeRunId || !active) return
    const runId = activeRunId
    const counters = seq.current
    let stopped = false
    let timer: number | undefined
    async function poll() {
      if (stopped) return
      if (starting.current || cancelling.current || statusInFlight.current) { timer = window.setTimeout(poll, 1000); return }
      const request = ++seq.current.status
      statusInFlight.current = request
      let finished = false
      try {
        const result = await invoke<EvaluationRunStatus | null>('memory_retrieval_eval_status', { runId })
        if (stopped || !alive.current || seq.current.status !== request) return
        setStatusError(null)
        setRun(result)
        finished = !result || terminal(result.status)
      } catch (error) {
        if (!stopped && alive.current && seq.current.status === request) setStatusError(safeText(error))
      } finally {
        if (statusInFlight.current === request) statusInFlight.current = 0
        if (!stopped && !finished) timer = window.setTimeout(poll, 1000)
      }
    }
    timer = window.setTimeout(poll, 1000)
    return () => { stopped = true; window.clearTimeout(timer); ++counters.status }
  }, [activeRunId, active])

  useEffect(() => {
    if (!activeRunId || !activeRunStatus || !terminal(activeRunStatus)) return
    const runId = activeRunId
    const counters = seq.current
    const request = ++counters.report
    let stopped = false
    setReportLoading(true)
    setReportError(null)
    void invoke<EvaluationReport>('memory_retrieval_eval_report', { runId }).then(result => {
      if (!stopped && alive.current && seq.current.report === request) setReport(result)
    }).catch(error => {
      if (!stopped && alive.current && seq.current.report === request) setReportError(safeText(error))
    }).finally(() => {
      if (!stopped && alive.current && seq.current.report === request) setReportLoading(false)
    })
    return () => { stopped = true; ++counters.report }
  }, [activeRunId, activeRunStatus, reportRetry])

  function clearPlan() {
    ++seq.current.plan
    setPlan(null)
    setPlanLoading(false)
    setPlanError(null)
    setConfirmed(false)
  }

  async function preview() {
    if (!datasetId || active || starting.current) return
    clearPlan()
    const request = ++seq.current.plan
    setPlanLoading(true)
    try {
      const result = await invoke<EvaluationPlan>('memory_retrieval_eval_plan', { datasetId, split, mode, budgetMs: 12000 })
      if (valid('plan', request)) setPlan(result)
    } catch (error) {
      if (valid('plan', request)) setPlanError(safeText(error))
    } finally {
      if (valid('plan', request)) setPlanLoading(false)
    }
  }

  async function start() {
    if (!plan || !confirmed || expired || active || starting.current || statusLoading || statusError) return
    if (Date.parse(plan.expires_at) <= Date.now()) { setExpired(true); setConfirmed(false); return }
    const previousRun = run
    const previousReport = report
    starting.current = true
    const request = ++seq.current.status
    ++seq.current.report
    setRunBusy(true)
    setRunError(null)
    setRun(null)
    setReport(null)
    setReportError(null)
    setReportLoading(false)
    try {
      const result = await invoke<EvaluationRunStatus>('memory_retrieval_eval_start', { planId: plan.plan_id, confirmed: true })
      if (valid('status', request)) { setRun(result); setPlan(null); setConfirmed(false) }
    } catch (error) {
      if (valid('status', request)) { setRun(previousRun); setReport(previousReport); setRunError(safeText(error)) }
    } finally {
      starting.current = false
      if (alive.current) setRunBusy(false)
    }
  }

  async function cancel() {
    if (!run || !active || run.status === 'cancelling' || cancelling.current) return
    cancelling.current = true
    const request = ++seq.current.status
    setRunBusy(true)
    setRunError(null)
    try {
      const result = await invoke<EvaluationRunStatus>('memory_retrieval_eval_cancel', { runId: run.run_id })
      if (valid('status', request)) setRun(result)
    } catch (error) {
      if (valid('status', request)) setRunError(safeText(error))
    } finally {
      cancelling.current = false
      if (alive.current) setRunBusy(false)
    }
  }

  async function fileAction(kind: 'import' | 'dataset' | 'json' | 'markdown') {
    if (fileBusy) return
    const request = ++seq.current.files
    const targetId = kind === 'dataset' ? datasetId : report?.run_id
    setFileBusy(true)
    setFileError(null)
    setFileNotice(null)
    try {
      if (kind === 'import') {
        const path = await open({ multiple: false, directory: false, filters: [{ name: t(`${ns}jsonFile`), extensions: ['json'] }] })
        if (!path || typeof path !== 'string' || !valid('files', request)) return
        const result = await invoke<EvaluationDatasetSummary>('memory_retrieval_eval_import', { path })
        if (!valid('files', request)) return
        ++seq.current.datasets
        setDatasetsLoading(false)
        setDatasetsError(null)
        setDatasets(current => [...current.filter(item => item.dataset_id !== result.dataset_id), result])
        clearPlan()
        setDatasetId(result.dataset_id)
        setFileNotice('imported')
        void loadDatasets()
      } else {
        if (!targetId) return
        const extension = kind === 'markdown' ? 'md' : 'json'
        const path = await save({ defaultPath: `memory-evaluation.${extension}`, filters: [{ name: t(`${ns}${kind === 'markdown' ? 'markdownFile' : 'jsonFile'}`), extensions: [extension] }] })
        if (!path || !valid('files', request)) return
        await invoke<void>('memory_retrieval_eval_export', { id: targetId, kind, path })
        if (valid('files', request)) setFileNotice('exported')
      }
    } catch (error) {
      if (valid('files', request)) setFileError(safeText(error))
    } finally {
      if (valid('files', request)) setFileBusy(false)
    }
  }

  return <section aria-label={t(`${ns}title`)} className="min-w-0 space-y-6 text-[var(--app-text)]">
    <header className="space-y-2"><h3 className="text-base font-semibold">{t(`${ns}title`)}</h3><p className={`max-w-[75ch] ${mutedClass}`}>{t(`${ns}intro`)}</p><p className={mutedClass}>{t(`${ns}persistence`)}</p></header>
    <section aria-labelledby={`${id}-dataset`} className="space-y-3">
      <h4 id={`${id}-dataset`} className="text-sm font-semibold">{t(`${ns}dataset`)}</h4>
      <div className="grid gap-3 sm:grid-cols-[minmax(0,2fr)_minmax(0,1fr)_minmax(0,1fr)]">
        <label className={`min-w-0 space-y-1 ${mutedClass}`}><span>{t(`${ns}dataset`)}</span><select className="w-full" value={datasetId} disabled={active || runBusy || fileBusy} onChange={event => { clearPlan(); setDatasetId(event.target.value) }}><option value="" disabled>{t(`${ns}chooseDataset`)}</option>{datasets.map(item => <option key={item.dataset_id} value={item.dataset_id}>{item.name} · {t(`${ns}${item.synthetic ? 'synthetic' : 'importedDataset'}`)}</option>)}</select></label>
        <label className={`min-w-0 space-y-1 ${mutedClass}`}><span>{t(`${ns}split`)}</span><select className="w-full" value={split} disabled={active || runBusy} onChange={event => { clearPlan(); setSplit(event.target.value as EvaluationPlan['split']) }}>{(['development', 'holdout', 'all'] as const).map(value => <option key={value} value={value}>{t(`${ns}${value}`)}</option>)}</select></label>
        <label className={`min-w-0 space-y-1 ${mutedClass}`}><span>{t(`${ns}mode`)}</span><select className="w-full" value={mode} disabled={active || runBusy} onChange={event => { clearPlan(); setMode(event.target.value as EvaluationPlan['mode']) }}>{(['hybrid', 'keyword'] as const).map(value => <option key={value} value={value}>{t(`${ns}${value}`)}</option>)}</select></label>
      </div>
      <div className="flex flex-wrap gap-2"><button type="button" className={buttonClass} disabled={fileBusy || active || runBusy} onClick={() => void fileAction('import')}>{t(`${ns}importDataset`)}</button><button type="button" className={buttonClass} disabled={!datasetId || fileBusy} onClick={() => void fileAction('dataset')}>{t(`${ns}exportTemplate`)}</button><button type="button" className={buttonClass} disabled={datasetsLoading || fileBusy} onClick={() => void loadDatasets()}>{t(`${ns}refreshDatasets`)}</button></div>
      {datasetsLoading && <p role="status" className={mutedClass}>{t(`${ns}datasetsLoading`)}</p>}
      <EvaluationFailure error={datasetsError} onRetry={() => void loadDatasets()} />
      {!datasetsLoading && !datasetsError && datasets.length === 0 && <p className={mutedClass}>{t(`${ns}noDatasets`)}</p>}
      {selected && <dl className="space-y-2"><Field label={t(`${ns}datasetId`)}>{selected.dataset_id}</Field><Field label={t(`${ns}description`)}>{selected.description}</Field><Field label={t(`${ns}provenance`)}>{t(`${ns}${selected.synthetic ? 'syntheticHint' : 'importedDataset'}`)}</Field><Field label={t(`${ns}memory_count`)}>{number(selected.memory_count)}</Field><Field label={t(`${ns}case_count`)}>{number(selected.case_count)}</Field><Field label={t(`${ns}splitCounts`)}>{t(`${ns}splitCountsValue`, { development: selected.development_count, holdout: selected.holdout_count })}</Field></dl>}
      {fileBusy && <p role="status" className={mutedClass}>{t(`${ns}fileWorking`)}</p>}<EvaluationFailure error={fileError} />{fileNotice && <p role="status" className={mutedClass}>{t(`${ns}${fileNotice}`)}</p>}
    </section>
    <section aria-labelledby={`${id}-plan`} className="space-y-3 border-t border-[var(--app-stroke)] pt-5">
      <h4 id={`${id}-plan`} className="text-sm font-semibold">{t(`${ns}plan`)}</h4>
      <p className={`max-w-[75ch] ${mutedClass}`}>{t(`${ns}planHint`)}</p>
      <button type="button" className={buttonClass} disabled={!selected || planLoading || active || runBusy} onClick={() => void preview()}>{t(`${ns}${planLoading ? 'planLoading' : 'preview'}`)}</button>
      <EvaluationFailure error={planError} onRetry={() => void preview()} />
      {plan && <>
        <dl className="space-y-2">{(['case_count', 'memory_count', 'embedding_batches', 'query_calls', 'routing_calls', 'remote_calls_estimate', 'budget_ms'] as const).map(key => <Field key={key} label={t(`${ns}${key}`)}>{number(plan[key])}</Field>)}<Field label={t(`${ns}expires`)}>{plan.expires_at}</Field><Field label={t(`${ns}fingerprint`)}>{plan.dataset_fingerprint}</Field></dl>
        <Backend backend={plan.backend} />
        <p className={mutedClass}>{t(`${ns}estimateHint`)}</p>
        {expired && <p role="status" className={mutedClass}>{t(`${ns}expired`)}</p>}
        <label className="flex items-start gap-2 text-sm leading-6"><input type="checkbox" className="mt-1 accent-[var(--app-accent)] focus-visible:outline-2 focus-visible:outline-[var(--app-focus)]" checked={confirmed} disabled={expired || active || runBusy} onChange={event => setConfirmed(event.target.checked)} /><span>{t(`${ns}confirmation`)}</span></label>
        <button type="button" className={buttonClass} disabled={!confirmed || expired || active || runBusy || statusLoading || !!statusError} onClick={() => void start()}>{t(`${ns}${runBusy ? 'working' : 'start'}`)}</button>
      </>}
      <EvaluationFailure error={runError} onRetry={() => void restoreStatus(run?.run_id ?? null)} />
    </section>
    <section aria-labelledby={`${id}-run`} className="space-y-3 border-t border-[var(--app-stroke)] pt-5">
      <h4 id={`${id}-run`} className="text-sm font-semibold">{t(`${ns}run`)}</h4>
      {statusLoading && <p role="status" className={mutedClass}>{t(`${ns}restoring`)}</p>}
      <EvaluationFailure error={statusError} onRetry={() => void restoreStatus(run?.run_id ?? null)} />
      {run ? <><p role="status" className="text-sm">{t(`${ns}status_${run.status}`)} · {t(`${ns}${terminal(run.status) ? 'progressDone' : 'progress'}`, { completed: run.completed, total: run.total, prepared: run.prepared, memories: run.total_memories })}</p><dl><Field label={t(`${ns}runId`)}>{run.run_id}</Field></dl><EvaluationFailure error={run.error ? safeText(run.error) : null} />{active && <><button type="button" className={buttonClass} disabled={runBusy || run.status === 'cancelling'} onClick={() => void cancel()}>{t(`${ns}${runBusy || run.status === 'cancelling' ? 'status_cancelling' : 'cancel'}`)}</button><p className={mutedClass}>{t(`${ns}cancelHint`)}</p></>}</> : !statusLoading && <p className={mutedClass}>{t(`${ns}noRun`)}</p>}
    </section>
    <section aria-labelledby={`${id}-report`} className="space-y-3 border-t border-[var(--app-stroke)] pt-5">
      <h4 id={`${id}-report`} className="text-sm font-semibold">{t(`${ns}report`)}</h4>
      {reportLoading && <p role="status" className={mutedClass}>{t(`${ns}reportLoading`)}</p>}
      <EvaluationFailure error={reportError} onRetry={() => setReportRetry(value => value + 1)} />
      {!report && !reportLoading && !reportError && <p className={mutedClass}>{t(`${ns}noReport`)}</p>}
      {report && <>
        <p className="text-sm">{report.dataset_name} · {t(`${ns}${report.synthetic ? 'synthetic' : 'importedDataset'}`)}</p>
        <dl className="space-y-2"><Field label={t(`${ns}runId`)}>{report.run_id}</Field><Field label={t(`${ns}reportStatus`)}>{t(`${ns}status_${report.status}`, { defaultValue: report.status })}</Field><Field label={t(`${ns}split`)}>{t(`${ns}${report.split}`, { defaultValue: report.split })}</Field><Field label={t(`${ns}mode`)}>{t(`${ns}${report.mode}`, { defaultValue: report.mode })}</Field>{(['planned_count', 'executed_count', 'not_run_count', 'successful_count', 'failed_count', 'degraded_count', 'partial_index_count', 'answerable_count', 'unanswerable_count'] as const).map(key => <Field key={key} label={t(`${ns}${key}`)}>{number(report.summary[key])}</Field>)}</dl>
        <p className={mutedClass}>{t(`${ns}denominatorHint`)}</p>
        <dl className="space-y-2">{(['recall_at_k', 'mrr', 'ndcg_at_k', 'duplicate_rate', 'forbidden_rate', 'false_positive_rate', 'domain_accuracy', 'p50_ms', 'p95_ms', 'mean_returned_chars', 'evidence_coverage', 'evidence_status_accuracy'] as const).map(key => <Field key={key} label={t(`${ns}${key}`)}>{number(report.summary[key])}</Field>)}</dl>
        <p role="note" className={mutedClass}>{t(`${ns}evidenceHint`)}</p>
        <details><summary className="cursor-pointer rounded-[6px] py-2 text-sm font-semibold focus-visible:outline-2 focus-visible:outline-[var(--app-focus)]">{t(`${ns}reportMetadata`)}</summary><dl className="space-y-2 py-2"><Field label={t(`${ns}datasetId`)}>{report.dataset_id}</Field><Field label={t(`${ns}fingerprint`)}>{report.dataset_fingerprint}</Field><Field label={t(`${ns}schemaVersion`)}>{report.schema_version}</Field><Field label={t(`${ns}startedAt`)}>{report.started_at}</Field><Field label={t(`${ns}finishedAt`)}>{report.finished_at || t(`${ns}none`)}</Field><Field label={t(`${ns}preparationMs`)}>{number(report.preparation_ms)}</Field></dl><Backend backend={report.backend} /></details>
        <div className="flex flex-wrap gap-2">{(['json', 'markdown'] as const).map(kind => <button key={kind} type="button" className={buttonClass} disabled={fileBusy} onClick={() => void fileAction(kind)}>{t(`${ns}export_${kind}`)}</button>)}</div>
        {report.cases.length === 0 ? <p className={mutedClass}>{t(`${ns}noCases`)}</p> : <div role="region" aria-label={t(`${ns}cases`)} tabIndex={0} className="max-h-[28rem] overflow-auto rounded-[6px] border border-[var(--app-stroke)] focus-visible:outline-2 focus-visible:outline-[var(--app-focus)]"><table className="w-full min-w-[760px] text-xs tabular-nums"><caption className="p-3 text-left text-[var(--app-secondary)]">{t(`${ns}casesHint`)}</caption><thead className="sticky top-0 bg-[var(--app-layer)]"><tr>{['query', 'caseStatus', 'expectedIds', 'returnedIds', 'recall_at_k', 'mrr', 'ndcg_at_k', 'error'].map(key => <th key={key} scope="col" className={cellClass}>{t(`${ns}${key}`)}</th>)}</tr></thead><tbody>{report.cases.map((item, index) => <Fragment key={`${item.case_id}-${index}`}><tr><th scope="row" className={`${cellClass} min-w-40 max-w-64 whitespace-pre-wrap break-words font-normal [overflow-wrap:anywhere]`}>{item.query}</th><td className={cellClass}>{t(`${ns}case_${item.status}`, { defaultValue: item.status })}</td><td className={`${cellClass} max-w-44 break-all`}>{ids(item.expected_ids)}</td><td className={`${cellClass} max-w-44 break-all`}>{ids(item.returned_ids)}</td>{[item.metrics.recall_at_k, item.metrics.mrr, item.metrics.ndcg_at_k].map((value, i) => <td key={i} className={cellClass}>{number(value)}</td>)}<td className={`${cellClass} max-w-48 break-words [overflow-wrap:anywhere]`}>{item.error ? safeText(item.error) : t(`${ns}none`)}</td></tr><tr><td colSpan={8} className={cellClass}><details><summary className="cursor-pointer rounded-[6px] py-1 font-medium focus-visible:outline-2 focus-visible:outline-[var(--app-focus)]">{t(`${ns}caseDetails`, { id: item.case_id })}</summary><dl className="space-y-2 py-3"><Field label={t(`${ns}answerability`)}>{t(`${ns}${item.expected_ids.length ? 'answerable' : 'unanswerable'}`)}</Field><Field label={t(`${ns}category`)}>{item.category}</Field><Field label={t(`${ns}split`)}>{t(`${ns}${item.split}`, { defaultValue: item.split })}</Field><Field label={t(`${ns}forbiddenIds`)}>{ids(item.forbidden_ids)}</Field><Field label={t(`${ns}expectedDomains`)}>{ids(item.expected_domain_ids)}</Field><Field label={t(`${ns}activeDomains`)}>{ids(item.active_domains)}</Field><Field label={t(`${ns}routingFallback`)}>{typeof item.retrieval?.routing_fallback === 'string' ? safeText(item.retrieval.routing_fallback) : t(`${ns}none`)}</Field><Field label={t(`${ns}domainHit`)}>{yesNo(item.domain_hit)}</Field><Field label={t(`${ns}degraded`)}>{yesNo(item.status === 'not_run' ? null : item.degraded)}</Field><Field label={t(`${ns}indexComplete`)}>{yesNo(item.status === 'not_run' ? null : item.index_complete)}</Field><Field label={t(`${ns}duplicate_rate`)}>{number(item.status === 'not_run' ? null : item.metrics.duplicate_rate)}</Field><Field label={t(`${ns}forbidden_rate`)}>{number(item.status === 'not_run' ? null : item.metrics.forbidden_rate)}</Field><Field label={t(`${ns}falsePositive`)}>{yesNo(item.status === 'not_run' ? null : item.metrics.false_positive_on_empty_query)}</Field><Field label={t(`${ns}elapsedMs`)}>{number(item.status === 'not_run' ? null : item.elapsed_ms)}</Field><Field label={t(`${ns}returnedChars`)}>{number(item.status === 'not_run' ? null : item.returned_chars)}</Field><Field label={t(`${ns}evidence`)}>{number(item.evidence_coverage)}</Field><Field label={t(`${ns}evidenceStatusHit`)}>{yesNo(item.evidence_status_hit ?? null)}</Field><Field label={t(`${ns}evidenceMs`)}>{number(item.evidence_ms)}</Field><Field label={t(`${ns}diagnostics`)}>{JSON.stringify(Object.fromEntries(Object.entries(item.retrieval ?? {}).filter(([key, value]) => ['budget_ms', 'injected_chars', 'semantic_candidates', 'local_keyword_candidates', 'unique_candidates', 'discarded_candidates', 'partial_index', 'elapsed_ms'].includes(key) && (typeof value === 'number' || typeof value === 'boolean'))))}</Field></dl></details></td></tr></Fragment>)}</tbody></table></div>}
      </>}
    </section>
  </section>
}
