import { useCallback, useEffect, useRef, useState } from 'react'
import { useTranslation } from 'react-i18next'
import { Loader2, RefreshCw, X } from 'lucide-react'
import { mcpApi } from '../../../api/mcp'
import { useMcpStore } from '../../../store/mcpStore'
import type { McpArbitrationResult, McpComparisonSource, McpConfigurationComparison } from '../../../types/mcp'
import { buildMcpComparisonRows } from '../configurationComparison'
import { targetOf } from '../targets'

export function ConfigurationArbitrationDialog({ name, initialAgent, onClose, onApplied }: {
  name: string
  initialAgent?: string
  onClose: () => void
  onApplied: (result: McpArbitrationResult) => Promise<void>
}) {
  const { t } = useTranslation()
  const modal = useRef<HTMLDialogElement>(null)
  const mounted = useRef(true)
  const submitting = useRef(false)
  const requestId = useRef(0)
  const [comparison, setComparison] = useState<McpConfigurationComparison | null>(null)
  const [compareAgent, setCompareAgent] = useState('')
  const [leftSource, setLeftSource] = useState('catalog')
  const [winner, setWinner] = useState('')
  const [targets, setTargets] = useState<Set<string>>(new Set())
  const [onlyDifferences, setOnlyDifferences] = useState(true)
  const [showSensitive, setShowSensitive] = useState(false)
  const [loading, setLoading] = useState(true)
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState('')
  const [outcome, setOutcome] = useState<McpArbitrationResult | null>(null)

  const agentLabel = (agent: string) => agent === 'codex_cli'
    ? t('mcpLibrary.codexSharedConfig') : targetOf(agent)?.label ?? agent
  const stateLabel = (state: McpComparisonSource['state']) => t(`mcpLibrary.arbitration.states.${state}`)

  useEffect(() => {
    mounted.current = true
    const previous = document.activeElement
    const dialog = modal.current
    dialog?.showModal()
    return () => {
      mounted.current = false
      requestId.current += 1
      dialog?.close()
      if (previous instanceof HTMLElement && previous.isConnected) previous.focus()
    }
  }, [])

  const reload = useCallback(async (preserveSelection = false) => {
    const request = ++requestId.current
    setLoading(true)
    setError('')
    setComparison(null)
    if (!preserveSelection) {
      setWinner('')
      setTargets(new Set())
    }
    setShowSensitive(false)
    setOutcome(null)
    try {
      const next = await mcpApi.mcpConfigurationCompare({ name })
      if (!mounted.current || request !== requestId.current) return
      setComparison(next)
      const right = next.sources.find(source => source.agent === initialAgent)
        ?? next.sources.find(source => source.state === 'differs' && source.entry)
        ?? next.sources.find(source => source.entry) ?? next.sources[0]
      if (!preserveSelection) {
        setCompareAgent(right?.agent ?? '')
        setLeftSource(next.catalog ? 'catalog' : next.sources.find(source => source.entry && !source.observed && source.agent !== right?.agent)?.agent ?? 'catalog')
      } else {
        setWinner(previous => previous === 'catalog' ? next.catalog ? previous : ''
          : next.sources.some(source => source.agent === previous && source.entry && !source.observed) ? previous : '')
      }
    } catch (reason) {
      if (mounted.current && request === requestId.current) setError(String(reason))
    } finally {
      if (mounted.current && request === requestId.current) setLoading(false)
    }
  }, [name, initialAgent])

  useEffect(() => { void reload() }, [reload])

  const viewedSource = comparison?.sources.find(source => source.agent === compareAgent)
  const leftAgent = comparison?.sources.find(source => source.agent === leftSource)
  const leftVersion = leftSource === 'catalog' ? comparison?.catalog : leftAgent?.entry
  const rows = buildMcpComparisonRows(leftVersion ?? null, viewedSource?.entry ?? null)
  const displayedRows = onlyDifferences ? rows.filter(row => row.changed) : rows
  const selectedVersion = winner === 'catalog' ? comparison?.catalog
    : comparison?.sources.find(source => source.agent === winner)?.entry
  const canSync = (source: McpComparisonSource) => selectedVersion && !source.observed
    && source.state !== 'unavailable' && source.agent !== winner
    && ((selectedVersion.transport || 'stdio') === 'stdio' || targetOf(source.agent)?.remote === true)
  const effectiveTargets = comparison?.sources.filter(source => targets.has(source.agent) && canSync(source)).map(source => source.agent) ?? []
  const canApply = !!selectedVersion && !comparison?.blocked_reason && !loading && !busy
    && (winner !== 'catalog' || effectiveTargets.length > 0)

  function toggleTarget(agent: string) {
    setTargets(previous => {
      const next = new Set(previous)
      if (next.has(agent)) next.delete(agent)
      else next.add(agent)
      return next
    })
  }

  async function apply() {
    if (!comparison || !canApply || submitting.current) return
    submitting.current = true
    setBusy(true)
    setError('')
    setOutcome(null)
    try {
      const result = await mcpApi.mcpConfigurationResolve({
        name, revision: comparison.revision,
        source: winner === 'catalog' ? null : winner, targets: effectiveTargets,
      })
      await onApplied(result)
      if (!mounted.current) return
      if (result.errors.length === 0) onClose()
      else {
        await reload()
        if (!mounted.current) return
        setOutcome(result)
        setWinner('catalog')
        setTargets(new Set(result.errors.map(issue => issue.agent)))
      }
    } catch (reason) {
      if (mounted.current) setError(String(reason))
    } finally {
      submitting.current = false
      if (mounted.current) setBusy(false)
    }
  }

  async function prepareArbitration() {
    if (!comparison || submitting.current || loading) return
    const observed = comparison.sources.filter(source => source.observed)
    if (observed.length === 0) return
    submitting.current = true
    setBusy(true)
    setError('')
    const failures: string[] = []
    try {
      for (const source of observed) {
        try {
          const status = await useMcpStore.getState().setMcpObservation(name, source.agent, false)
          if (status.state !== 'off') failures.push(`${agentLabel(source.agent)} · ${t('mcpLibrary.arbitration.prepareIncomplete')}`)
        } catch (reason) {
          failures.push(`${agentLabel(source.agent)} · ${String(reason)}`)
        }
      }
      if (mounted.current) await reload(true)
      await useMcpStore.getState().checkMcpStatuses().catch(() => {})
      if (mounted.current && failures.length) setError(failures.join('\n'))
    } finally {
      submitting.current = false
      if (mounted.current) setBusy(false)
    }
  }

  const cellValue = (value: string | undefined, sensitive: boolean) => value === undefined
    ? t('mcpLibrary.arbitration.absent') : value === '' ? t('mcpLibrary.arbitration.emptyValue')
      : sensitive && !showSensitive ? t('mcpLibrary.arbitration.hiddenValue') : value
  const fieldLabel = (key: string) => key.startsWith('env.')
    ? `${t('mcpLibrary.configFields.env')} · ${key.slice(4)}` : key.startsWith('headers.')
      ? `${t('mcpLibrary.configFields.headers')} · ${key.slice(8)}`
      : t(`mcpLibrary.configFields.${key}`, { defaultValue: key })

  return (
    <dialog ref={modal} aria-labelledby="mcp-arbitration-title" onCancel={event => {
      event.preventDefault()
      if (!submitting.current) onClose()
    }} className="m-auto w-[calc(100vw-2rem)] max-w-6xl overflow-hidden rounded-2xl border border-gray-200 bg-white p-0 text-gray-900 shadow-xl backdrop:bg-gray-950/40 dark:border-gray-700 dark:bg-gray-900 dark:text-gray-100">
      <div className="flex max-h-[calc(100vh-2rem)] flex-col">
        <div className="flex items-start gap-3 border-b border-gray-200 px-5 py-4 dark:border-gray-700">
          <div className="min-w-0 flex-1">
            <h2 id="mcp-arbitration-title" className="break-all text-base font-semibold">{t('mcpLibrary.arbitration.title')} · {name}</h2>
            <p className="mt-1 text-sm leading-6 text-gray-600 dark:text-gray-300">{t('mcpLibrary.arbitration.hint')}</p>
          </div>
          <button type="button" disabled={busy || loading} onClick={() => { void reload() }} className="shrink-0 rounded-md border border-gray-300 px-3 py-2 text-sm disabled:opacity-50 dark:border-gray-600">
            <RefreshCw size={14} className="mr-1 inline" />{t('mcpLibrary.arbitration.reload')}
          </button>
          <button type="button" aria-label={t('mcpLibrary.cancel')} disabled={busy} onClick={onClose} className="shrink-0 rounded-md p-2 hover:bg-gray-100 disabled:opacity-50 dark:hover:bg-gray-800"><X size={18} /></button>
        </div>
        <div className="min-h-0 space-y-5 overflow-y-auto p-5">
          {loading && <p role="status" className="flex items-center gap-2 text-sm"><Loader2 size={16} className="animate-spin motion-reduce:animate-none" />{t('mcpLibrary.checkingStatus')}</p>}
          {error && <p role="alert" className="whitespace-pre-line break-words text-sm leading-6 text-red-700 dark:text-red-300">{error}</p>}
          {outcome && <div role="alert" className="space-y-1 text-sm leading-6 text-amber-900 dark:text-amber-200">
            <p>{t('mcpLibrary.arbitration.partial', { updated: outcome.catalog_updated ? t('mcpLibrary.arbitration.catalogSaved') : t('mcpLibrary.arbitration.catalogKept'), count: outcome.applied_agents.length })}</p>
            <ul>{outcome.errors.map(issue => <li key={issue.agent}>{agentLabel(issue.agent)} · {issue.message}</li>)}</ul>
          </div>}
          {comparison && <>
            {comparison.blocked_reason && <div className="space-y-2 text-sm leading-6 text-amber-900 dark:text-amber-200">
              <p role="status">{comparison.blocked_reason}</p>
              <p>{t('mcpLibrary.arbitration.prepareHint')}</p>
              <button type="button" disabled={busy || loading} onClick={() => { void prepareArbitration() }} className="rounded-md border border-amber-400 px-3 py-1.5 disabled:opacity-50 dark:border-amber-600">
                {t('mcpLibrary.arbitration.prepare')}
              </button>
            </div>}
            <div className="flex flex-wrap items-center gap-x-5 gap-y-3 text-sm">
              <label className="flex min-w-0 flex-wrap items-center gap-2">{t('mcpLibrary.arbitration.leftSource')}
                <select value={leftSource} disabled={busy} onChange={event => { setLeftSource(event.target.value); setWinner(''); setTargets(new Set()) }} className="max-w-full rounded-md border border-gray-300 bg-white px-3 py-2 dark:border-gray-600 dark:bg-gray-800">
                  <option value="catalog">{t(comparison.catalog ? 'mcpLibrary.arbitration.catalogVersion' : 'mcpLibrary.arbitration.catalogMissing')}</option>
                  {comparison.sources.map(source => <option key={source.agent} value={source.agent} disabled={source.agent === compareAgent}>{agentLabel(source.agent)} · {stateLabel(source.state)}</option>)}
                </select>
              </label>
              <label className="flex min-w-0 flex-wrap items-center gap-2">{t('mcpLibrary.arbitration.compareSource')}
                <select value={compareAgent} disabled={busy} onChange={event => { setCompareAgent(event.target.value); setWinner(''); setTargets(new Set()) }} className="max-w-full rounded-md border border-gray-300 bg-white px-3 py-2 dark:border-gray-600 dark:bg-gray-800">
                  {comparison.sources.map(source => <option key={source.agent} value={source.agent} disabled={source.agent === leftSource}>{agentLabel(source.agent)} · {stateLabel(source.state)}</option>)}
                </select>
              </label>
              <label className="inline-flex items-center gap-2"><input type="checkbox" checked={onlyDifferences} onChange={event => setOnlyDifferences(event.target.checked)} className="accent-violet-600" />{t('mcpLibrary.arbitration.onlyDifferences')}</label>
              <label className="inline-flex items-center gap-2"><input type="checkbox" checked={showSensitive} onChange={event => setShowSensitive(event.target.checked)} className="accent-violet-600" />{t('mcpLibrary.arbitration.showSensitive')}</label>
            </div>
            {leftAgent?.error && <p className="break-words text-sm text-red-700 dark:text-red-300">{agentLabel(leftAgent.agent)} · {leftAgent.error}</p>}
            {viewedSource?.error && <p className="break-words text-sm text-red-700 dark:text-red-300">{agentLabel(viewedSource.agent)} · {viewedSource.error}</p>}
            <div className="overflow-x-auto rounded-lg border border-gray-200 dark:border-gray-700">
              <table className="w-full min-w-[38rem] table-fixed border-collapse text-left text-sm">
                <thead className="bg-gray-50 dark:bg-gray-800">
                  <tr>
                    <th scope="col" className="w-1/4 px-4 py-3 font-medium">{t('mcpLibrary.arbitration.field')}</th>
                    <th scope="col" className="px-4 py-3 font-medium">
                      <label className="flex items-center gap-2"><input type="radio" name="mcp-winner" checked={winner === leftSource} disabled={!leftVersion || leftAgent?.observed || busy} onChange={() => setWinner(leftSource)} className="accent-violet-600" />{leftSource === 'catalog' ? t('mcpLibrary.arbitration.useCatalog') : t('mcpLibrary.arbitration.useAgent', { agent: agentLabel(leftSource) })}</label>
                    </th>
                    <th scope="col" className="px-4 py-3 font-medium">
                      <label className="flex items-center gap-2"><input type="radio" name="mcp-winner" checked={winner === compareAgent && winner !== ''} disabled={!viewedSource?.entry || viewedSource?.observed || busy} onChange={() => setWinner(compareAgent)} className="accent-violet-600" />{t('mcpLibrary.arbitration.useAgent', { agent: agentLabel(compareAgent) })}</label>
                    </th>
                  </tr>
                </thead>
                <tbody className="divide-y divide-gray-200 dark:divide-gray-700">
                  {displayedRows.map(row => <tr key={row.key} className={row.changed ? 'bg-amber-50/60 dark:bg-amber-500/5' : ''}>
                    <th scope="row" className="break-words px-4 py-3 align-top font-medium">{fieldLabel(row.key)}{row.changed && <span className="mt-1 block text-xs font-normal text-amber-800 dark:text-amber-300">{t('mcpLibrary.arbitration.changed')}</span>}</th>
                    <td className="px-4 py-3 align-top"><pre className="whitespace-pre-wrap break-all font-mono text-xs leading-6">{row.left === undefined && leftAgent?.state === 'unavailable' ? t('mcpLibrary.arbitration.states.unavailable') : cellValue(row.left, row.sensitive)}</pre></td>
                    <td className="px-4 py-3 align-top"><pre className="whitespace-pre-wrap break-all font-mono text-xs leading-6">{row.right === undefined && viewedSource?.state === 'unavailable' ? t('mcpLibrary.arbitration.states.unavailable') : cellValue(row.right, row.sensitive)}</pre></td>
                  </tr>)}
                  {displayedRows.length === 0 && <tr><td colSpan={3} className="px-4 py-6 text-center text-gray-600 dark:text-gray-300">{t(!leftVersion && !viewedSource?.entry ? 'mcpLibrary.arbitration.noComparable' : 'mcpLibrary.arbitration.noDifferences')}</td></tr>}
                </tbody>
              </table>
            </div>
            <fieldset disabled={!selectedVersion || busy} className="min-w-0 border-t border-gray-200 pt-3 dark:border-gray-700">
              <legend className="px-1 text-sm font-medium">{t('mcpLibrary.arbitration.syncTargets')}</legend>
              <p className="mb-3 text-sm leading-6 text-gray-600 dark:text-gray-300">{winner === 'catalog' ? t('mcpLibrary.arbitration.catalogScope') : t('mcpLibrary.arbitration.agentScope')}</p>
              <button type="button" disabled={!selectedVersion} onClick={() => setTargets(new Set(comparison.sources.filter(source => canSync(source) && buildMcpComparisonRows(selectedVersion ?? null, source.entry).some(row => row.changed)).map(source => source.agent)))} className="mb-3 rounded-md border border-gray-300 px-3 py-1.5 text-sm disabled:opacity-50 dark:border-gray-600">{t('mcpLibrary.arbitration.selectDifferent')}</button>
              <div className="grid gap-2 sm:grid-cols-2">
                {comparison.sources.map(source => <label key={source.agent} className={`flex items-start gap-2 text-sm leading-6 ${canSync(source) ? '' : 'text-gray-500 dark:text-gray-400'}`}>
                  <input type="checkbox" checked={effectiveTargets.includes(source.agent)} disabled={!canSync(source)} onChange={() => toggleTarget(source.agent)} className="mt-1 accent-violet-600" />
                  <span className="min-w-0 break-words">{agentLabel(source.agent)} · {source.agent === winner ? t('mcpLibrary.arbitration.chosenSource') : source.observed ? t('mcpLibrary.arbitration.observationMustClose') : selectedVersion && (selectedVersion.transport || 'stdio') !== 'stdio' && !targetOf(source.agent)?.remote ? t('mcpLibrary.remoteUnsupported') : stateLabel(source.state)}</span>
                </label>)}
              </div>
            </fieldset>
            {winner && <p className="text-sm leading-6 text-gray-700 dark:text-gray-200">{t(winner === 'catalog' ? 'mcpLibrary.arbitration.catalogPlan' : 'mcpLibrary.arbitration.agentPlan', { agent: agentLabel(winner), count: effectiveTargets.length })}</p>}
          </>}
        </div>
        <div className="flex flex-wrap items-center justify-end gap-3 border-t border-gray-200 px-5 py-4 dark:border-gray-700">
          {!winner && !loading && comparison && <p className="mr-auto text-sm text-gray-600 dark:text-gray-300">{t('mcpLibrary.arbitration.chooseVersion')}</p>}
          <button type="button" onClick={onClose} disabled={busy} className="rounded-md border border-gray-300 px-4 py-2 text-sm disabled:opacity-50 dark:border-gray-600">{t(outcome ? 'mcpLibrary.arbitration.close' : 'mcpLibrary.arbitration.keepDifferences')}</button>
          <button type="button" onClick={() => { void apply() }} disabled={!canApply} className="inline-flex items-center gap-2 rounded-md bg-violet-600 px-4 py-2 text-sm font-medium text-white hover:bg-violet-500 disabled:opacity-50">{busy && <Loader2 size={14} className="animate-spin motion-reduce:animate-none" />}{t('mcpLibrary.arbitration.apply')}</button>
        </div>
      </div>
    </dialog>
  )
}
