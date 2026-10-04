import { useCallback, useEffect, useState } from 'react'
import { useTranslation } from 'react-i18next'
import { invoke } from '@tauri-apps/api/core'
import { CheckCircle2, Loader2, Bot, RefreshCw } from 'lucide-react'
import { useAgentStore } from '../../store/agentStore'

interface DetectedAgentCli {
  id: string
  name: string
  command: string
  imported: boolean
}

export function StepAgents({ onNext, onBusyChange }: {
  onNext: () => void
  onBusyChange: (busy: boolean) => void
}) {
  const { t } = useTranslation()
  const [agents, setAgents] = useState<DetectedAgentCli[]>([])
  const [selected, setSelected] = useState<string[]>([])
  const [scanning, setScanning] = useState(true)
  const [importing, setImporting] = useState(false)
  const [error, setError] = useState('')
  const [importedCount, setImportedCount] = useState<number | null>(null)
  const busy = scanning || importing

  useEffect(() => {
    onBusyChange(busy)
    return () => onBusyChange(false)
  }, [busy, onBusyChange])

  const scan = useCallback(async () => {
    setScanning(true)
    setError('')
    try {
      const detected = await invoke<DetectedAgentCli[]>('detect_agent_clis')
      setAgents(detected)
      setSelected(detected.filter(agent => !agent.imported).map(agent => agent.id))
    } catch (cause) {
      setError(String(cause))
    } finally {
      setScanning(false)
    }
  }, [])

  useEffect(() => { void scan() }, [scan])

  async function importSelected() {
    if (!selected.length || busy) return
    setImporting(true)
    setError('')
    setImportedCount(null)
    try {
      const ids = await invoke<string[]>('import_detected_agent_clis', { ids: selected })
      await useAgentStore.getState().fetchAgents()
      if (ids[0]) useAgentStore.getState().selectAgent(ids[0])
      setImportedCount(ids.length)
      await scan()
    } catch (cause) {
      setError(String(cause))
    } finally {
      setImporting(false)
    }
  }

  return (
    <div>
      <div className="flex items-center justify-between gap-3">
        <h2 className="text-lg font-bold text-gray-900 dark:text-gray-100">{t('onboarding.agents.title')}</h2>
        <button type="button" onClick={() => { void scan() }} disabled={busy} className="inline-flex items-center gap-1.5 rounded-lg px-2 py-1 text-xs text-blue-600 hover:bg-blue-50 focus-visible:outline-2 focus-visible:outline-blue-500 disabled:opacity-50 dark:text-blue-400 dark:hover:bg-gray-800">
          <RefreshCw size={13} />{t('common.refresh')}
        </button>
      </div>
      <p className="mt-2 text-sm text-gray-500 dark:text-gray-400">{t('onboarding.agents.desc')}</p>
      <div className="mt-5 min-h-[120px]">
        {scanning ? (
          <div className="flex items-center justify-center gap-2 py-8 text-sm text-gray-500" role="status"><Loader2 className="h-5 w-5 animate-spin text-blue-500" />{t('common.loading')}</div>
        ) : error && agents.length === 0 ? null : agents.length === 0 ? (
          <div className="rounded-xl border border-dashed border-gray-200 px-4 py-8 text-center dark:border-gray-700">
            <Bot className="mx-auto h-8 w-8 text-gray-300 dark:text-gray-600" />
            <p className="mt-2 text-sm text-gray-500 dark:text-gray-400">{t('onboarding.agents.empty')}</p>
          </div>
        ) : (
          <div className="space-y-2">
            {agents.map(agent => (
              <label key={agent.id} className="flex items-center gap-3 rounded-xl border border-gray-200 px-4 py-3 dark:border-gray-700">
                <input type="checkbox" checked={agent.imported || selected.includes(agent.id)} disabled={busy || agent.imported}
                  onChange={event => setSelected(ids => event.target.checked ? [...ids, agent.id] : ids.filter(id => id !== agent.id))}
                  className="h-4 w-4 rounded accent-blue-600 focus-visible:outline-2 focus-visible:outline-blue-500" />
                <span className="min-w-0 flex-1">
                  <span className="block text-sm font-medium text-gray-900 dark:text-gray-100">{agent.name}</span>
                  <span className="mt-0.5 block break-all font-mono text-xs text-gray-500 dark:text-gray-400">{agent.command}</span>
                </span>
                {agent.imported && <span className="inline-flex shrink-0 items-center gap-1 text-xs text-emerald-600 dark:text-emerald-400"><CheckCircle2 size={14} />{t('onboarding.agents.imported')}</span>}
              </label>
            ))}
          </div>
        )}
      </div>
      <p className="mt-3 text-xs leading-5 text-gray-500 dark:text-gray-400">{t('onboarding.agents.hint')}</p>
      {error && <p role="alert" className="mt-3 break-words rounded-lg bg-red-50 p-3 text-sm text-red-600 dark:bg-red-500/10 dark:text-red-400">{error}</p>}
      {importedCount !== null && <p role="status" className="mt-3 text-sm text-emerald-600 dark:text-emerald-400">{t('onboarding.agents.importDone', { count: importedCount })}</p>}
      <div className="mt-6 flex flex-wrap gap-2">
        {selected.length > 0 && <button type="button" onClick={() => { void importSelected() }} disabled={busy} className="inline-flex flex-1 items-center justify-center gap-2 rounded-xl bg-blue-600 px-4 py-2.5 text-sm font-medium text-white hover:bg-blue-500 focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-blue-500 disabled:opacity-50">
          {importing && <Loader2 size={16} className="animate-spin" />}{t('onboarding.agents.importSelected', { count: selected.length })}
        </button>}
        <button type="button" onClick={onNext} disabled={busy} className="flex-1 rounded-xl border border-gray-200 px-4 py-2.5 text-sm font-medium text-gray-700 hover:bg-gray-50 focus-visible:outline-2 focus-visible:outline-blue-500 disabled:opacity-50 dark:border-gray-700 dark:text-gray-300 dark:hover:bg-gray-800">
          {t(selected.length > 0 ? 'onboarding.agents.skip' : 'onboarding.agents.next')}
        </button>
      </div>
    </div>
  )
}
