import { mcpApi } from '../../../api/mcp'
import type { ClaudeDesktopMcpConfig } from '../../../types/mcp'
import { useEffect, useId, useState } from 'react'
import { open } from '@tauri-apps/plugin-dialog'
import { useTranslation } from 'react-i18next'

import { RefreshCw, Loader2, Check, FolderOpen, ChevronRight } from 'lucide-react'

export function ClaudeDesktopConfigSelector({ disabled, onBusyChange, onChanged }: {
  disabled: boolean
  onBusyChange: (busy: boolean) => void
  onChanged: () => Promise<void>
}) {
  const { t } = useTranslation()
  const id = useId()
  const [config, setConfig] = useState<ClaudeDesktopMcpConfig | null>(null)
  // undefined follows the saved preference; null is an explicit return to auto mode.
  const [draftPath, setDraftPath] = useState<string | null | undefined>(undefined)
  const [busy, setBusy] = useState<'load' | 'pick' | 'save' | null>('load')
  const [reloadAttempt, setReloadAttempt] = useState(0)
  const [error, setError] = useState<{ kind: 'load' | 'pick' | 'save'; detail: string } | null>(null)
  const [saved, setSaved] = useState(false)
  const [expanded, setExpanded] = useState(false)

  useEffect(() => {
    let mounted = true
    setBusy('load')
    setError(null)
    onBusyChange(true)
    async function load() {
      try {
        let next: ClaudeDesktopMcpConfig
        try {
          next = await mcpApi.claudeDesktopMcpConfigGet()
        } catch (e) {
          if (mounted) setError({ kind: 'load', detail: String(e) })
          return
        }
        if (!mounted) return
        // Keep any unsaved selection, including after a failed save or refresh.
        setConfig(next)
        // Rediscovery can change the effective path even without saving a preference.
        if (reloadAttempt > 0) await onChanged()
      } finally {
        if (mounted) {
          setBusy(null)
          onBusyChange(false)
        }
      }
    }
    void load()
    return () => { mounted = false; onBusyChange(false) }
  }, [reloadAttempt, onBusyChange, onChanged])

  const locked = disabled || busy !== null
  const dirty = draftPath !== undefined && draftPath !== config?.override_path
  const value = draftPath === undefined ? config?.override_path ?? '' : draftPath ?? ''
  const paths = [...new Set([
    ...(config?.candidates.map((candidate) => candidate.path) ?? []),
    ...(config?.override_path ? [config.override_path] : []),
    ...(draftPath ? [draftPath] : []),
  ])]

  async function pickFile() {
    if (locked) return
    setBusy('pick')
    onBusyChange(true)
    setError(null)
    try {
      const picked = await open({ directory: false, multiple: false, filters: [{ name: 'JSON', extensions: ['json'] }] })
      if (typeof picked === 'string') {
        setDraftPath(picked)
        setSaved(false)
      }
    } catch (e) {
      setError({ kind: 'pick', detail: String(e) })
    } finally {
      setBusy(null)
      onBusyChange(false)
    }
  }

  async function savePath() {
    // A corrupt/missing preference must remain recoverable through a file choice.
    if (locked || !dirty || draftPath === undefined) return
    setBusy('save')
    onBusyChange(true)
    setError(null)
    setSaved(false)
    try {
      let next: ClaudeDesktopMcpConfig
      try {
        next = await mcpApi.claudeDesktopMcpConfigSet({ path: draftPath })
      } catch (e) {
        setError({ kind: 'save', detail: String(e) })
        return
      }
      setConfig(next)
      setDraftPath(undefined)
      setSaved(true)
      // The preference is saved. Catalog/status refresh errors are shown by the page,
      // not reported as save failures. Explicit re-import is a separate user action.
      await onChanged()
    } finally {
      setBusy(null)
      onBusyChange(false)
    }
  }

  const button = 'inline-flex items-center gap-1 rounded-lg border border-gray-300 bg-white px-2.5 py-1.5 text-xs font-medium text-gray-700 transition hover:bg-gray-50 disabled:opacity-50 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-violet-500 dark:border-gray-600 dark:bg-gray-800 dark:text-gray-200 dark:hover:bg-gray-700'
  const errorKey = error?.kind === 'save' ? 'mcpLibrary.desktopConfigSaveFailed'
    : error?.kind === 'pick' ? 'mcpLibrary.desktopConfigPickFailed' : 'mcpLibrary.desktopConfigLoadFailed'

  return (
    <section aria-label={t('mcpLibrary.desktopConfigLabel')} aria-busy={busy !== null} className="shrink-0 border-b border-gray-200 px-5 text-left dark:border-gray-800">
      <button type="button" onClick={() => setExpanded(current => !current)} aria-expanded={expanded} aria-controls={`${id}-manager`}
        title={config?.selected_path ?? undefined}
        className="flex w-full items-center gap-2 rounded py-2.5 text-xs text-gray-700 hover:text-violet-700 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-inset focus-visible:ring-violet-500 dark:text-gray-300 dark:hover:text-violet-300">
        <ChevronRight size={14} aria-hidden="true" className={`shrink-0 transition-transform motion-reduce:transition-none ${expanded ? 'rotate-90' : ''}`} />
        <span className="min-w-0 flex-1 truncate font-medium text-left">{t('mcpLibrary.desktopConfigLabel')}</span>
        {(error || config?.needs_selection || (config && !config.selected_path)) && <span role="status" className="shrink-0 text-amber-700 dark:text-amber-300">{t('mcpLibrary.desktopConfigNeedsAttention')}</span>}
        {dirty && <span className="shrink-0 text-amber-700 dark:text-amber-300">{t('mcpLibrary.desktopConfigPendingShort')}</span>}
        <span className="shrink-0">{t(expanded ? 'mcpLibrary.collapseDetails' : 'mcpLibrary.desktopConfigManage')}</span>
      </button>
      <div id={`${id}-manager`} hidden={!expanded} className="space-y-1.5 pb-3">
      <div className="flex flex-wrap items-center gap-2">
        <label htmlFor={id} className="sr-only">{t('mcpLibrary.desktopConfigLabel')}</label>
        <select
          id={id}
          value={value}
          onChange={(event) => { setDraftPath(event.target.value || null); setSaved(false) }}
          disabled={locked}
          aria-describedby={`${id}-details`}
          className="min-w-0 flex-1 basis-64 rounded-lg border border-gray-300 bg-white px-2.5 py-1.5 text-xs text-gray-800 outline-none transition focus:border-violet-400 focus-visible:ring-2 focus-visible:ring-violet-500 disabled:opacity-50 dark:border-gray-600 dark:bg-gray-800 dark:text-gray-100"
        >
          <option value="">{t(config ? 'mcpLibrary.desktopConfigAuto' : 'mcpLibrary.desktopConfigUnknown')}</option>
          {paths.map((path) => {
            const candidate = config?.candidates.find((item) => item.path === path)
            return <option key={path} value={path}>{path}{candidate ? (candidate.exists ? '' : ` · ${t('mcpLibrary.desktopConfigMissing')}`) : ` · ${t('mcpLibrary.desktopConfigCustom')}`}</option>
          })}
        </select>
        <button type="button" onClick={() => { void pickFile() }} disabled={locked} className={button}>
          <FolderOpen size={13} />{t('mcpLibrary.desktopConfigBrowse')}
        </button>
        <button type="button" onClick={() => { void savePath() }} disabled={locked || !dirty} className={button}>
          {busy === 'save' ? <Loader2 size={13} className="animate-spin" /> : <Check size={13} />}
          {t(busy === 'save' ? 'common.saving' : 'common.save')}
        </button>
        <button type="button" onClick={() => setReloadAttempt((attempt) => attempt + 1)} disabled={locked} className={button}>
          <RefreshCw size={13} className={busy === 'load' ? 'animate-spin' : ''} />{t('common.refresh')}
        </button>
      </div>
      <div id={`${id}-details`} className="space-y-1 text-[11px] leading-5 text-gray-500 dark:text-gray-400">
        <p className="break-all" aria-live="polite">
          {t('mcpLibrary.desktopConfigEffective')}{' '}
          <span className="font-mono">{config ? config.selected_path ?? t('mcpLibrary.desktopConfigNone') : t('mcpLibrary.desktopConfigUnknown')}</span>
        </p>
        {config?.needs_selection && <p role="alert" className="text-amber-700 dark:text-amber-300">{t('mcpLibrary.desktopConfigAmbiguous')}</p>}
      </div>
      {dirty && <p role="status" className="text-xs text-amber-700 dark:text-amber-300">{t('mcpLibrary.desktopConfigPending')}</p>}
      {saved && <p role="status" className="text-xs text-emerald-700 dark:text-emerald-300">{t('mcpLibrary.desktopConfigSaved')}</p>}
      {error && <p role="alert" className="break-words rounded-lg bg-red-50 px-3 py-2 text-xs text-red-600 dark:bg-red-900/30 dark:text-red-300">{t(errorKey, { error: error.detail })}</p>}
      </div>
    </section>
  )
}
