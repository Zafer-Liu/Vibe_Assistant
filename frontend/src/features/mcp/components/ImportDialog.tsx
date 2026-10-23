import { McpModal } from './McpModal'
import { useEffect, useId, useMemo, useState } from 'react'

import { useTranslation } from 'react-i18next'

import { Download, Loader2, X } from 'lucide-react'
import { useMcpFields } from '../../../store/mcpStore'

import type { McpImportCandidate, McpImportIssue } from '../../../types/mcp'
import { targetOf } from '../targets'
import { defaultImportSelection, groupImportCandidates, selectedImports } from '../importSelection'

// ── 从 Agent 导入对话框 ───────────────────────────────────────────────────────

export function ImportDialog({ onClose, onNotice, onCompare }: { onClose: () => void; onNotice: (message: string) => void; onCompare: (name: string) => void }) {
  const { t } = useTranslation()
  const { importMcpFromAgents, importMcpEntry } = useMcpFields('importMcpFromAgents', 'importMcpEntry')
  const [scanning, setScanning] = useState(true)
  const [candidates, setCandidates] = useState<McpImportCandidate[]>([])
  const [picked, setPicked] = useState<Map<string, string>>(new Map())
  const [importing, setImporting] = useState(false)
  const [scanError, setScanError] = useState('')
  const [scanIssues, setScanIssues] = useState<McpImportIssue[]>([])
  const [scanAttempt, setScanAttempt] = useState(0)
  const [importError, setImportError] = useState('')
  const groupId = useId()
  const groups = useMemo(() => groupImportCandidates(candidates), [candidates])

  useEffect(() => {
    let mounted = true
    setScanning(true)
    setScanError('')
    setImportError('')
    setScanIssues([])
    setPicked(new Map())
    importMcpFromAgents()
      .then(({ candidates: found, issues }) => {
        if (!mounted) return
        setCandidates(found)
        setScanIssues(issues)
        setPicked(defaultImportSelection(found, issues.length === 0))
      })
      .catch((error) => {
        if (mounted) setScanError(t('mcpLibrary.importFailed', { error: String(error) }))
      })
      .finally(() => { if (mounted) setScanning(false) })
    return () => { mounted = false }
  }, [importMcpFromAgents, scanAttempt, t])

  function pick(name: string, agent: string) {
    setPicked((prev) => {
      const next = new Map(prev)
      if (agent) next.set(name, agent)
      else next.delete(name)
      return next
    })
  }

  async function importSelected() {
    setImporting(true)
    setImportError('')
    let count = 0
    try {
      for (const candidate of selectedImports(candidates, picked)) {
        await importMcpEntry(candidate.entry.name, candidate.agent)
        count += 1
        pick(candidate.entry.name, '')
      }
      onNotice(t('mcpLibrary.importDone', { count }))
      onClose()
    } catch (error) {
      setImportError(t('mcpLibrary.importPartialFailed', { count, error: String(error) }))
    } finally {
      setImporting(false)
    }
  }

  return (
    <McpModal label={t('mcpLibrary.importTitle')} busy={importing} size="2xl" onClose={onClose}>
      <div className="fluent-dialog-panel relative flex max-h-[85vh] w-full max-w-2xl flex-col overflow-hidden rounded-2xl border border-gray-200 bg-white shadow-2xl dark:border-gray-700 dark:bg-gray-900">
        <div className="flex items-center gap-2 border-b border-gray-100 px-5 py-4 dark:border-gray-800">
          <div className="rounded-lg bg-violet-500/10 p-1.5 text-violet-600 dark:text-violet-400">
            <Download size={16} />
          </div>
          <h2 className="text-sm font-semibold text-gray-900 dark:text-gray-100">{t('mcpLibrary.importTitle')}</h2>
          <button onClick={onClose} disabled={importing} className="ml-auto rounded-md p-1 text-gray-500 transition hover:text-gray-600 disabled:opacity-50 dark:hover:text-gray-300" aria-label={t('mcpLibrary.cancel')}>
            <X size={16} />
          </button>
        </div>
        <div className="min-h-0 flex-1 overflow-y-auto px-5 py-4">
          <p className="mb-4 text-sm leading-6 text-gray-600 dark:text-gray-300">{t('mcpLibrary.importVersionHint')}</p>
          {scanning && (
            <div className="flex items-center gap-2 py-8 text-sm text-gray-500 dark:text-gray-400">
              <Loader2 size={16} className="animate-spin" />{t('mcpLibrary.importScanning')}
            </div>
          )}
          {scanError && (
            <div role="alert" className="space-y-2 rounded-lg bg-red-50 p-3 text-xs text-red-600 dark:bg-red-900/20 dark:text-red-300">
              <p className="break-words">{scanError}</p>
              <button type="button" onClick={() => setScanAttempt(value => value + 1)} disabled={scanning} className="rounded border border-current px-2 py-1 disabled:opacity-50">{t('common.refresh')}</button>
            </div>
          )}
          {!scanning && !scanError && scanIssues.length > 0 && (
            <div role="status" className="mb-4 space-y-2 rounded-lg bg-amber-50 p-3 text-sm leading-6 text-amber-900 dark:bg-amber-500/10 dark:text-amber-200">
              <p className="font-medium">{t('mcpLibrary.importIncomplete', { count: scanIssues.length })}</p>
              <p>{t('mcpLibrary.importIncompleteHint')}</p>
              <ul className="space-y-1">
                {scanIssues.map((issue, index) => <li key={`${issue.agent}:${issue.name ?? ''}:${index}`} className="break-words">
                  {issue.agent === 'codex_cli' ? t('mcpLibrary.codexSharedConfig') : targetOf(issue.agent)?.label ?? issue.agent}{issue.name ? ` / ${issue.name}` : ''} · {issue.message}
                </li>)}
              </ul>
              <button type="button" onClick={() => setScanAttempt(value => value + 1)} disabled={importing} className="rounded-md border border-current px-3 py-1 font-medium focus-visible:outline focus-visible:outline-2 focus-visible:outline-violet-500 disabled:opacity-50">{t('common.refresh')}</button>
            </div>
          )}
          {!scanning && !scanError && candidates.length === 0 && (
            <p className="py-8 text-center text-sm text-gray-500 dark:text-gray-400">{t(scanIssues.length > 0 ? 'mcpLibrary.importIncompleteEmpty' : 'mcpLibrary.importEmpty')}</p>
          )}
          {!scanning && !scanError && [...groups].map(([name, group], index) => (
            <fieldset key={name} disabled={importing} className="mb-4 min-w-0 border-t border-gray-200 pt-3 dark:border-gray-700">
              <legend className="max-w-full break-all px-1 text-sm font-semibold text-gray-900 dark:text-gray-100">
                {name}
                {group[0].variant_count > 1 && <span className="ml-2 font-normal text-amber-800 dark:text-amber-300">{t('mcpLibrary.importVariants', { count: group[0].variant_count })}</span>}
                <button type="button" disabled={importing} onClick={() => onCompare(name)} className="ml-3 rounded-md px-2 py-1 text-sm font-medium text-violet-700 hover:bg-violet-50 focus-visible:outline focus-visible:outline-2 focus-visible:outline-violet-500 disabled:opacity-50 dark:text-violet-300 dark:hover:bg-violet-500/10">{t('mcpLibrary.arbitration.open')}</button>
              </legend>
              {group[0].peer_differing_fields.length > 0 && <p className="mb-1 break-words px-2 text-sm text-amber-800 dark:text-amber-300">{t('mcpLibrary.importPeerDifferences', { fields: group[0].peer_differing_fields.map(field => t(`mcpLibrary.configFields.${field}`, { defaultValue: field })).join(' / ') })}</p>}
              <label className="flex cursor-pointer items-center gap-3 rounded-md px-2 py-2 text-sm text-gray-600 focus-within:outline focus-within:outline-2 focus-within:outline-violet-500 dark:text-gray-300">
                <input type="radio" name={`${groupId}-${index}`} checked={!picked.has(name)} onChange={() => pick(name, '')} className="accent-violet-600" />
                {t('mcpLibrary.importSkip')}
              </label>
              {group.map(candidate => (
                <label key={candidate.agent} className="flex cursor-pointer items-start gap-3 rounded-md px-2 py-3 hover:bg-gray-50 focus-within:outline focus-within:outline-2 focus-within:outline-violet-500 dark:hover:bg-gray-800">
                  <input type="radio" name={`${groupId}-${index}`} checked={picked.get(name) === candidate.agent} onChange={() => pick(name, candidate.agent)} className="mt-1 accent-violet-600" />
                  <span className="min-w-0 flex-1 space-y-1 text-sm">
                    <span className="flex flex-wrap items-center gap-x-3 gap-y-1">
                      <span className="font-medium text-gray-900 dark:text-gray-100">{candidate.agent === 'codex_cli' ? t('mcpLibrary.codexSharedConfig') : targetOf(candidate.agent)?.label ?? candidate.agent}</span>
                      <span className={candidate.catalog_state === 'differs' ? 'text-amber-800 dark:text-amber-300' : 'text-gray-600 dark:text-gray-300'}>{t(`mcpLibrary.importState.${candidate.catalog_state}`)}</span>
                      {candidate.variant_count > 1 && <span className="text-gray-600 dark:text-gray-300">{t('mcpLibrary.importVariant', { version: candidate.variant })}</span>}
                    </span>
                    {candidate.differing_fields.length > 0 && <span className="block break-words text-amber-800 dark:text-amber-300">{t('mcpLibrary.differingFields', { fields: candidate.differing_fields.map(field => t(`mcpLibrary.configFields.${field}`, { defaultValue: field })).join(' / ') })}</span>}
                    {picked.get(name) === candidate.agent && candidate.catalog_state === 'differs' && <span className="block text-amber-800 dark:text-amber-300">{t('mcpLibrary.importReplaceHint')}</span>}
                  </span>
                </label>
              ))}
            </fieldset>
          ))}
          {importError && <p role="alert" className="mt-3 break-words text-sm text-red-700 dark:text-red-300">{importError}</p>}
        </div>
        <div className="flex justify-end gap-2 border-t border-gray-100 px-5 py-3 dark:border-gray-800">
          <button onClick={onClose} disabled={importing} className="rounded-lg px-3 py-1.5 text-xs font-medium text-gray-500 transition hover:text-gray-700 dark:text-gray-400 dark:hover:text-gray-200">{t('mcpLibrary.cancel')}</button>
          <button onClick={() => { void importSelected() }} disabled={scanning || !!scanError || importing || picked.size === 0} className="inline-flex items-center gap-1 rounded-lg bg-violet-600 px-3 py-1.5 text-xs font-medium text-white transition hover:bg-violet-500 disabled:opacity-50">
            {importing ? <Loader2 size={12} className="animate-spin" /> : <Download size={12} />}
            {t('mcpLibrary.importApply', { count: picked.size })}
          </button>
        </div>
      </div>
    </McpModal>
  )
}
