import { McpModal } from './McpModal'
import { useState } from 'react'

import { useTranslation } from 'react-i18next'

import { Plug, RefreshCw, Loader2, X, Check } from 'lucide-react'
import { useMcpStore, useMcpFields } from '../../../store/mcpStore'

import type { McpAgentStatus, McpCatalogEntry, McpObservationStatus } from '../../../types/mcp'
import { TARGETS } from '../targets'

// ── 装备对话框 ────────────────────────────────────────────────────────────────

export function EquipDialog({ entry, onClose, onNotice }: {
  entry: McpCatalogEntry
  onClose: () => void
  onNotice: (message: string) => void
}) {
  const { t } = useTranslation()
  const { mcpStatuses, mcpStatusState, mcpObservationStatuses, mcpObservationError, equipMcp, unequipMcp } = useMcpFields('mcpStatuses', 'mcpStatusState', 'mcpObservationStatuses', 'mcpObservationError', 'equipMcp', 'unequipMcp')
  const [targets, setTargets] = useState<Set<string>>(new Set(entry.assigned_agents))
  const [busyAgent, setBusyAgent] = useState<string | null>(null)
  const [applying, setApplying] = useState(false)
  const [equipError, setEquipError] = useState('')

  const isRemote = entry.transport === 'sse' || entry.transport === 'http'
  const statusOf = (agent: string): McpAgentStatus | undefined =>
    mcpStatuses.find((s) => s.name === entry.name && s.agent === agent)
  const observationOf = (agent: string): McpObservationStatus | undefined =>
    mcpObservationStatuses.find((s) => s.name === entry.name && s.agent === (agent === 'codex_desktop' ? 'codex_cli' : agent))

  function toggle(id: string) {
    setTargets((prev) => {
      const next = new Set(prev)
      if (next.has(id)) next.delete(id)
      else next.add(id)
      return next
    })
  }

  async function rewrite(agent: string) {
    setBusyAgent(agent)
    setEquipError('')
    try {
      await equipMcp(entry.name, agent)
    } catch (error) {
      setEquipError(t('mcpLibrary.equipFailed', { error: String(error) }))
    } finally {
      setBusyAgent(null)
    }
  }

  async function apply() {
    if (applying || busyAgent !== null) return
    const current = useMcpStore.getState().mcpCatalog.find((item) => item.name === entry.name)
    const assignedAgents = current?.assigned_agents ?? entry.assigned_agents
    const changes: { agent: string; equipped: boolean }[] = []
    for (const target of TARGETS) {
      const assigned = targets.has(target.id)
      const was = assignedAgents.includes(target.id)
      if (assigned !== was) changes.push({ agent: target.id, equipped: assigned })
    }
    if (changes.length === 0) {
      onClose()
      return
    }
    setApplying(true)
    setEquipError('')
    let failed = ''
    for (const change of changes) {
      try {
        if (change.equipped) await equipMcp(entry.name, change.agent)
        else await unequipMcp(entry.name, change.agent)
      } catch (error) {
        failed = t('mcpLibrary.equipFailed', { error: String(error) })
        break
      }
    }
    setApplying(false)
    if (failed) {
      setEquipError(failed)
      return
    }
    onNotice(t('mcpLibrary.equipDone', { count: changes.length }))
    onClose()
  }

  const stateBadge = (state: string | undefined) => {
    if (!state) return <span className="text-[10px] text-gray-500 dark:text-gray-400">{t('mcpLibrary.statusUnavailable')}</span>
    if (state === 'installed') return <span className="rounded-full bg-emerald-500/10 px-1.5 py-0.5 text-[10px] font-medium text-emerald-700 dark:text-emerald-300">{t('mcpLibrary.stateInstalled')}</span>
    if (state === 'differs') return <span className="rounded-full bg-amber-500/10 px-1.5 py-0.5 text-[10px] font-medium text-amber-700 dark:text-amber-300">{t('mcpLibrary.stateDiffers')}</span>
    if (state === 'missing') return <span className="rounded-full bg-gray-500/10 px-1.5 py-0.5 text-[10px] font-medium text-gray-500 dark:text-gray-400">{t('mcpLibrary.stateMissing')}</span>
    return <span className="text-[10px] text-gray-500 dark:text-gray-400">{t('mcpLibrary.statusUnavailable')}</span>
  }

  return (
    <McpModal label={t('mcpLibrary.equipTitle')} busy={applying || busyAgent !== null} size="md" onClose={onClose}>
      <div className="fluent-dialog-panel relative flex max-h-[85vh] w-full max-w-md flex-col overflow-hidden rounded-2xl border border-gray-200 bg-white shadow-2xl dark:border-gray-700 dark:bg-gray-900">
        <div className="flex items-center gap-2 border-b border-gray-100 px-5 py-4 dark:border-gray-800">
          <div className="rounded-lg bg-violet-500/10 p-1.5 text-violet-600 dark:text-violet-400">
            <Plug size={16} />
          </div>
          <h2 className="min-w-0 truncate text-sm font-semibold text-gray-900 dark:text-gray-100">
            {t('mcpLibrary.equipTitle')} · <span className="font-mono">{entry.name}</span>
          </h2>
          <button onClick={onClose} disabled={applying || busyAgent !== null} className="ml-auto rounded-md p-1 text-gray-500 transition hover:text-gray-600 disabled:opacity-50 dark:hover:text-gray-300" aria-label={t('mcpLibrary.cancel')}>
            <X size={16} />
          </button>
        </div>
        <div className="min-h-0 flex-1 space-y-1 overflow-y-auto px-5 py-4">
          <p className="mb-2 text-xs leading-5 text-gray-500 dark:text-gray-400">{t('mcpLibrary.equipHint')}</p>
          {TARGETS.map((target) => {
            const unsupported = isRemote && !target.remote
            const checked = targets.has(target.id)
            const assigned = entry.assigned_agents.includes(target.id)
            const status = mcpStatusState === 'ready' ? statusOf(target.id) : undefined
            const observation = mcpObservationError ? undefined : observationOf(target.id)
            const observationState = observation?.state === 'observing' && observation.active_connections === 0 ? 'awaiting_connection' : observation?.state
            const observing = observationState !== undefined && observationState !== 'off' && observationState !== 'unsupported'
            return (
              <div key={target.id} className={`rounded-lg px-2 py-2 transition ${checked ? 'bg-violet-50 dark:bg-violet-500/10' : 'hover:bg-gray-50 dark:hover:bg-gray-800'}`}>
                <div className="flex items-center gap-2">
                  <input
                    type="checkbox"
                    checked={checked}
                    disabled={unsupported || applying || busyAgent !== null || (assigned && (!!mcpObservationError || observing))}
                    onChange={() => toggle(target.id)}
                    className="h-3.5 w-3.5 cursor-pointer accent-violet-600 disabled:cursor-not-allowed"
                    aria-label={target.label}
                  />
                  <span className={`h-2 w-2 shrink-0 rounded-full ${target.color}`} />
                  <span className={`min-w-0 flex-1 truncate text-sm ${unsupported ? 'text-gray-500 dark:text-gray-400' : 'text-gray-800 dark:text-gray-100'}`}>{target.label}</span>
                  {unsupported ? (
                    <span className="text-[10px] text-gray-500 dark:text-gray-400">{t('mcpLibrary.remoteUnsupported')}</span>
                  ) : (
                    <>
                      {checked && (mcpStatusState === 'ready' ? stateBadge(status?.state) : <span className="text-[10px] text-gray-500 dark:text-gray-400">{t('mcpLibrary.statusUnavailable')}</span>)}
                      {checked && busyAgent !== target.id && (
                        <button
                          onClick={() => { void rewrite(target.id) }}
                          disabled={applying || busyAgent !== null || !!mcpObservationError || observing}
                          title={observing ? t('mcpLibrary.observationDisableBeforeRewrite') : t('mcpLibrary.rewrite')}
                          className="rounded-md p-1 text-gray-500 transition hover:text-violet-600 disabled:opacity-50 dark:hover:text-violet-400"
                          aria-label={t('mcpLibrary.rewrite')}
                        >
                          <RefreshCw size={12} />
                        </button>
                      )}
                      {busyAgent === target.id && <Loader2 size={12} className="animate-spin text-violet-500" />}
                    </>
                  )}
                </div>
              </div>
            )
          })}
          {equipError && <p role="alert" className="rounded-lg bg-red-50 px-3 py-2 text-xs leading-5 text-red-700 dark:bg-red-500/10 dark:text-red-300">{equipError}</p>}
        </div>
        <div className="flex justify-end gap-2 border-t border-gray-100 px-5 py-3 dark:border-gray-800">
          <button onClick={onClose} disabled={applying || busyAgent !== null} className="rounded-lg px-3 py-1.5 text-xs font-medium text-gray-500 transition hover:text-gray-700 disabled:opacity-50 dark:text-gray-400 dark:hover:text-gray-200">{t('mcpLibrary.cancel')}</button>
          <button onClick={() => { void apply() }} disabled={applying || busyAgent !== null} className="inline-flex items-center gap-1 rounded-lg bg-violet-600 px-3 py-1.5 text-xs font-medium text-white transition hover:bg-violet-500 disabled:opacity-50">
            {applying ? <Loader2 size={12} className="animate-spin" /> : <Check size={12} />}
            {t('mcpLibrary.equipApply')}
          </button>
        </div>
      </div>
    </McpModal>
  )
}
