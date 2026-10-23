import { useCallback, useEffect, useState } from 'react'
import { invoke } from '@tauri-apps/api/core'
import { useTranslation } from 'react-i18next'
import { AlertTriangle, Cloud, GitMerge, Loader2, RefreshCw, X } from 'lucide-react'

interface VaultStatusView {
  configured: boolean
  enabled: boolean
  dirty: number
  conflicts: number
  last_sync_at: string | null
  last_report: string | null
  auto_interval_min: number
}

interface CloudConflict {
  object_key: string
  object_kind: 'l1' | 'l2' | 'l3'
  base_content: string | null
  local_content: string | null
  remote_content: string | null
  local_updated_at: string | null
  remote_updated_at: string | null
}

interface SyncReport {
  pushed: number
  pulled: number
  skipped: number
  conflicts: number
  usage_imported: number
  errors: string[]
}

type Phase =
  | { kind: 'idle' }
  | { kind: 'syncing' }
  | { kind: 'pulling' }
  | { kind: 'done'; report: SyncReport }
  | { kind: 'error'; message: string }

export function CloudVaultCard({ onSynced }: { onSynced?: () => void }) {
  const { t } = useTranslation()
  const [status, setStatus] = useState<VaultStatusView | null>(null)
  const [phase, setPhase] = useState<Phase>({ kind: 'idle' })
  const [conflicts, setConflicts] = useState<CloudConflict[]>([])
  const [conflictsOpen, setConflictsOpen] = useState(false)
  const [selectedConflictKey, setSelectedConflictKey] = useState<string | null>(null)
  const [mergedContent, setMergedContent] = useState('')
  const [resolving, setResolving] = useState<string | null>(null)

  const refresh = useCallback(() => {
    invoke<VaultStatusView>('cloud_vault_status')
      .then(setStatus)
      .catch(() => setStatus(null))
  }, [])

  useEffect(refresh, [refresh])

  const loadConflicts = useCallback(async () => {
    try {
      const entries = await invoke<CloudConflict[]>('cloud_vault_list_conflicts')
      setConflicts(entries)
      setSelectedConflictKey((current) => entries.some((item) => item.object_key === current) ? current : entries[0]?.object_key ?? null)
    } catch {
      setConflicts([])
    }
  }, [])

  useEffect(() => { void loadConflicts() }, [loadConflicts])

  const handleSync = async () => {
    setPhase({ kind: 'syncing' })
    try {
      const report = await invoke<SyncReport>('cloud_vault_sync')
      setPhase({ kind: 'done', report })
      refresh()
      void loadConflicts()
      onSynced?.()
    } catch (e) {
      setPhase({ kind: 'error', message: String(e) })
    }
  }

  const handlePull = async () => {
    setPhase({ kind: 'pulling' })
    try {
      const report = await invoke<SyncReport>('cloud_vault_pull')
      setPhase({ kind: 'done', report })
      refresh()
      void loadConflicts()
      onSynced?.()
    } catch (e) {
      setPhase({ kind: 'error', message: String(e) })
    }
  }

  const configured = status?.configured ?? false
  const enabled = status?.enabled ?? false
  const selected = conflicts.find((item) => item.object_key === selectedConflictKey) ?? null
  const textFromPayload = (content: string | null) => {
    if (!content) return t('memory.cloudCard.deleted')
    if (selected?.object_kind !== 'l1') return content
    try { return (JSON.parse(content) as { content?: string }).content ?? content } catch { return content }
  }
  const localText = selected ? textFromPayload(selected.local_content) : ''
  const remoteText = selected ? textFromPayload(selected.remote_content) : ''

  useEffect(() => {
    if (selected) setMergedContent(selected.local_content ? localText : remoteText)
  // Reset only when switching conflict; localText is deliberately derived from it.
  // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [selectedConflictKey])

  const resolve = async (resolution: 'local' | 'remote' | 'merged') => {
    if (!selected) return
    setResolving(resolution)
    try {
      await invoke('cloud_vault_resolve_conflict', {
        objectKey: selected.object_key,
        resolution,
        mergedContent: resolution === 'merged' ? mergedContent : null,
      })
      await loadConflicts()
      refresh()
      onSynced?.()
    } finally {
      setResolving(null)
    }
  }
  const dotClass = !configured || !enabled
    ? 'bg-gray-400'
    : phase.kind === 'syncing' || phase.kind === 'pulling'
      ? 'bg-blue-500 animate-pulse'
      : phase.kind === 'error'
        ? 'bg-red-500'
        : phase.kind === 'done' && phase.report.errors.length > 0
          ? 'bg-amber-500'
          : 'bg-green-500'

  return (
    <section className="order-15 rounded-xl border border-cyan-200 bg-cyan-50/40 p-4 dark:border-cyan-900/70 dark:bg-cyan-950/20">
      <div className="flex flex-wrap items-center gap-2">
        <div className="mt-0.5 rounded-md bg-cyan-500/10 p-1.5 text-cyan-600 dark:text-cyan-300"><Cloud size={14} /></div>
        <h2 className="text-sm font-semibold text-gray-800 dark:text-gray-100">{t('memory.cloudCard.title')}</h2>
        <span className={`h-2 w-2 rounded-full ${dotClass}`} title={t('memory.cloudCard.status')} />
        {status?.dirty ? (
          <span className="rounded bg-amber-100 px-1.5 py-0.5 text-xs text-amber-800 dark:bg-amber-950/40 dark:text-amber-300">
            {t('memory.cloudCard.dirty', { count: status.dirty })}
          </span>
        ) : null}
        {(status?.conflicts ?? conflicts.length) > 0 ? (
          <button type="button" onClick={() => { setConflictsOpen(true); void loadConflicts() }} className="inline-flex items-center gap-1 rounded-full bg-amber-500/15 px-2 py-0.5 text-xs font-medium text-amber-800 hover:bg-amber-500/25 dark:text-amber-200">
            <AlertTriangle size={12} />{t('memory.cloudCard.conflicts', { count: status?.conflicts ?? conflicts.length })}
          </button>
        ) : null}
        <div className="ml-auto flex items-center gap-2">
          <button
            type="button"
            onClick={() => { void handleSync() }}
            disabled={!enabled || phase.kind === 'syncing' || phase.kind === 'pulling'}
            className="inline-flex items-center gap-1.5 rounded-md bg-cyan-600 px-2.5 py-1.5 text-xs font-medium text-white hover:bg-cyan-500 disabled:opacity-50"
          >
            {phase.kind === 'syncing' ? <Loader2 size={13} className="animate-spin" /> : <RefreshCw size={13} />}
            {phase.kind === 'syncing' ? t('memory.cloudCard.syncing') : t('memory.cloudCard.syncNow')}
          </button>
          <button
            type="button"
            onClick={() => { void handlePull() }}
            disabled={!enabled || phase.kind === 'syncing' || phase.kind === 'pulling'}
            className="inline-flex items-center gap-1.5 rounded-md border border-cyan-300 bg-white px-2.5 py-1.5 text-xs font-medium text-cyan-800 hover:bg-cyan-50 disabled:opacity-50 dark:border-cyan-800 dark:bg-gray-800 dark:text-cyan-200 dark:hover:bg-cyan-950/40"
          >
            {phase.kind === 'pulling' ? <Loader2 size={13} className="animate-spin" /> : <Cloud size={13} />}
            {phase.kind === 'pulling' ? t('memory.cloudCard.pulling') : t('memory.cloudCard.pullNow')}
          </button>
        </div>
      </div>
      {!configured ? (
        <p className="mt-2 text-xs text-gray-500 dark:text-gray-400">{t('memory.cloudCard.notConfigured')}</p>
      ) : !enabled ? (
        <p className="mt-2 text-xs text-gray-500 dark:text-gray-400">{t('memory.cloudCard.disabled')}</p>
      ) : (
        <div className="mt-2 space-y-1 text-xs text-gray-500 dark:text-gray-400">
          <p>
            {status && status.auto_interval_min > 0
              ? t('memory.cloudCard.autoOn', { min: status.auto_interval_min })
              : t('memory.cloudCard.autoOff')}
          </p>
          {status?.last_sync_at && (
            <p>
              {t('memory.cloudCard.lastSync', { time: new Date(status.last_sync_at).toLocaleString() })}
              {status.last_report ? ` · ${status.last_report}` : ''}
            </p>
          )}
          {phase.kind === 'done' && (
            <p className={phase.report.errors.length ? 'text-amber-700 dark:text-amber-300' : 'text-green-600 dark:text-green-400'}>
              {t('memory.cloudCard.report', {
                pushed: phase.report.pushed,
                pulled: phase.report.pulled,
                usage: phase.report.usage_imported,
                skipped: phase.report.skipped,
                conflicts: phase.report.conflicts,
                errors: phase.report.errors.length,
              })}
              {phase.report.errors.length ? ` — ${phase.report.errors[0]}` : ''}
            </p>
          )}
          {phase.kind === 'error' && (
            <p className="text-red-600 dark:text-red-400">{phase.message}</p>
          )}
        </div>
      )}
      {conflictsOpen && (
        <div className="fixed inset-0 z-50 flex items-center justify-center bg-slate-950/45 p-4" role="dialog" aria-modal="true" aria-labelledby="cloud-conflict-title">
          <div className="flex max-h-[88vh] w-full max-w-5xl flex-col overflow-hidden rounded-xl border border-amber-200 bg-white shadow-2xl dark:border-amber-900/70 dark:bg-gray-900">
            <header className="flex items-start gap-3 border-b border-amber-100 bg-amber-50/70 px-5 py-4 dark:border-amber-900/50 dark:bg-amber-950/20">
              <div className="rounded-md bg-amber-500/15 p-2 text-amber-700 dark:text-amber-300"><GitMerge size={18} /></div>
              <div className="min-w-0"><h3 id="cloud-conflict-title" className="text-sm font-semibold text-gray-900 dark:text-gray-100">{t('memory.cloudCard.conflictTitle')}</h3><p className="mt-0.5 text-xs text-gray-600 dark:text-gray-300">{t('memory.cloudCard.conflictHint')}</p></div>
              <button type="button" onClick={() => setConflictsOpen(false)} className="ml-auto rounded p-1 text-gray-500 hover:bg-black/5 dark:hover:bg-white/10" aria-label={t('common.close')}><X size={17} /></button>
            </header>
            {selected ? <div className="min-h-0 overflow-y-auto p-5">
              {conflicts.length > 1 && <select value={selected.object_key} onChange={(event) => setSelectedConflictKey(event.target.value)} className="mb-4 w-full rounded-md border border-gray-200 bg-white px-2.5 py-1.5 text-xs dark:border-gray-700 dark:bg-gray-800"><>{conflicts.map((item) => <option key={item.object_key} value={item.object_key}>{item.object_key}</option>)}</></select>}
              <div className="grid gap-3 md:grid-cols-2">
                <ConflictPane title={t('memory.cloudCard.local')} value={localText} changedAt={selected.local_updated_at} />
                <ConflictPane title={t('memory.cloudCard.remote')} value={remoteText} changedAt={selected.remote_updated_at} />
              </div>
              {selected.base_content && <details className="mt-3 rounded-md border border-gray-200 px-3 py-2 text-xs dark:border-gray-700"><summary className="cursor-pointer font-medium text-gray-600 dark:text-gray-300">{t('memory.cloudCard.base')}</summary><pre className="mt-2 whitespace-pre-wrap break-words text-gray-500 dark:text-gray-400">{textFromPayload(selected.base_content)}</pre></details>}
              {selected.local_content && selected.remote_content && <div className="mt-4"><label className="mb-1 block text-xs font-medium text-gray-700 dark:text-gray-200">{t('memory.cloudCard.merge')}</label><textarea value={mergedContent} onChange={(event) => setMergedContent(event.target.value)} rows={8} className="w-full rounded-md border border-cyan-300 bg-cyan-50/30 px-3 py-2 font-mono text-xs leading-5 outline-none focus:ring-1 focus:ring-cyan-500 dark:border-cyan-800 dark:bg-cyan-950/20 dark:text-gray-100" /></div>}
            </div> : <div className="p-8 text-center text-sm text-gray-500">{t('memory.cloudCard.noConflicts')}</div>}
            {selected && <footer className="flex flex-wrap justify-end gap-2 border-t border-gray-200 bg-gray-50 px-5 py-3 dark:border-gray-700 dark:bg-gray-800/70">
              <button type="button" onClick={() => { void resolve('remote') }} disabled={resolving !== null} className="rounded-md border border-gray-300 px-3 py-1.5 text-xs font-medium text-gray-700 hover:bg-white disabled:opacity-50 dark:border-gray-600 dark:text-gray-200">{resolving === 'remote' ? <Loader2 size={13} className="inline animate-spin" /> : null} {t('memory.cloudCard.keepRemote')}</button>
              <button type="button" onClick={() => { void resolve('local') }} disabled={resolving !== null} className="rounded-md border border-violet-300 px-3 py-1.5 text-xs font-medium text-violet-700 hover:bg-violet-50 disabled:opacity-50 dark:border-violet-700 dark:text-violet-200">{resolving === 'local' ? <Loader2 size={13} className="inline animate-spin" /> : null} {t('memory.cloudCard.keepLocal')}</button>
              {selected.local_content && selected.remote_content && <button type="button" onClick={() => { void resolve('merged') }} disabled={resolving !== null || !mergedContent.trim()} className="rounded-md bg-cyan-600 px-3 py-1.5 text-xs font-medium text-white hover:bg-cyan-500 disabled:opacity-50">{resolving === 'merged' ? <Loader2 size={13} className="inline animate-spin" /> : null} {t('memory.cloudCard.saveMerge')}</button>}
            </footer>}
          </div>
        </div>
      )}
    </section>
  )
}

function ConflictPane({ title, value, changedAt }: { title: string; value: string; changedAt: string | null }) {
  return <article className="min-w-0 rounded-lg border border-gray-200 bg-gray-50/70 dark:border-gray-700 dark:bg-gray-800/60"><header className="flex items-center justify-between border-b border-gray-200 px-3 py-2 text-xs font-medium text-gray-700 dark:border-gray-700 dark:text-gray-200"><span>{title}</span>{changedAt && <time className="font-normal text-gray-500">{new Date(changedAt).toLocaleString()}</time>}</header><pre className="max-h-64 overflow-auto whitespace-pre-wrap break-words p-3 text-xs leading-5 text-gray-700 dark:text-gray-300">{value}</pre></article>
}
