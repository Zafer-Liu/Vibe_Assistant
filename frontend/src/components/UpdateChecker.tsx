import { useCallback, useEffect, useState } from 'react'
import { invoke } from '@tauri-apps/api/core'
import { useTranslation } from 'react-i18next'
import { RefreshCw, Download, CheckCircle, XCircle, ArrowUpCircle } from 'lucide-react'

interface VersionInfo {
  current: string
  latest: string
  has_update: boolean
  release_url: string
  release_notes: string
  published_at: string
}

type CheckState = 'idle' | 'checking' | 'done' | 'error'

const LAST_CHECK_KEY = 'updater_last_check'
const CHECK_INTERVAL_MS = 7 * 24 * 60 * 60 * 1000 // 7 days

interface UpdateCheckerProps {
  /** When true, runs a silent background check on mount (skipped if checked within 7 days). */
  autoCheck?: boolean
  /** Show as a compact inline badge instead of a full card. */
  compact?: boolean
  /** Compact sidebar footer: just a check button + inline status. */
  sidebar?: boolean
}

export function UpdateChecker({ autoCheck = false, compact = false, sidebar = false }: UpdateCheckerProps) {
  const { t } = useTranslation()
  const [state, setState] = useState<CheckState>('idle')
  const [info, setInfo] = useState<VersionInfo | null>(null)
  const [error, setError] = useState('')
  const [expanded, setExpanded] = useState(false)

  const check = useCallback(async (silent = false) => {
    setState('checking')
    setError('')
    try {
      const result = await invoke<VersionInfo>('check_for_update')
      setInfo(result)
      setState('done')
      localStorage.setItem(LAST_CHECK_KEY, String(Date.now()))
    } catch (e) {
      const msg = String(e)
      setError(msg)
      // swallow errors in silent/auto-check mode so the UI stays clean
      setState(silent ? 'idle' : 'error')
    }
  }, [])

  useEffect(() => {
    if (!autoCheck) return
    const last = localStorage.getItem(LAST_CHECK_KEY)
    if (last && Date.now() - Number(last) < CHECK_INTERVAL_MS) return
    check(true)
  }, [autoCheck, check])

  function openRelease() {
    if (info?.release_url) {
      window.open(info.release_url, '_blank')
    }
  }

  // ── Compact badge mode (used in sidebar / title area) ────────────────
  if (compact) {
    if (state === 'done' && info?.has_update) {
      return (
        <button
          onClick={openRelease}
          className="flex items-center gap-1 rounded-full bg-blue-100 px-2 py-0.5 text-[10px] font-medium text-blue-700 hover:bg-blue-200 dark:bg-blue-900/40 dark:text-blue-300 dark:hover:bg-blue-800/60"
          title={t('updater.newVersionAvailable', { version: info.latest })}
        >
          <ArrowUpCircle className="h-3 w-3" />
          {info.latest}
        </button>
      )
    }
    return null
  }

  // ── Sidebar footer mode ──────────────────────────────────────────────
 if (sidebar) {
   return (
      <div className="space-y-1">
        <button
          onClick={() => check()}
          disabled={state === 'checking'}
          className="flex items-center gap-1.5 text-xs font-bold text-gray-500 hover:text-gray-700 dark:text-gray-400 dark:hover:text-gray-200 disabled:opacity-50"
        >
          <RefreshCw className={`h-3.5 w-3.5 ${state === 'checking' ? 'animate-spin' : ''}`} />
          {state === 'checking' ? t('updater.checking') : t('updater.checkNow')}
        </button>
        {state === 'done' && info?.has_update && (
          <button
            onClick={openRelease}
            className="flex items-center gap-1.5 text-xs font-bold text-blue-600 hover:underline dark:text-blue-400"
          >
            <ArrowUpCircle className="h-3.5 w-3.5 shrink-0" />
            {t('updater.newVersionAvailable', { version: info.latest })}
          </button>
        )}
        {state === 'done' && info && !info.has_update && (
          <p className="text-xs font-bold text-gray-400 dark:text-gray-500">{t('updater.alreadyLatest')}</p>
        )}
        {state === 'error' && error && (
          <p className="text-xs font-bold text-red-500 dark:text-red-400">{error}</p>
        )}
      </div>
    )
  }

  // ── Full card mode (used in Settings / About) ─────────────────────────
  const formattedDate = info?.published_at
    ? new Date(info.published_at).toLocaleDateString()
    : ''

 return (
    <div className="space-y-3">
     {/* Header row */}
      <div className="flex items-center justify-between">
        <div>
          <p className="text-sm font-semibold text-gray-900 dark:text-gray-100">
            {t('updater.title')}
          </p>
          <p className="text-xs text-gray-400 dark:text-gray-500 font-mono">
            {t('updater.currentVersion', { version: '1.0.0' })}
          </p>
        </div>

        <button
          onClick={() => check()}
          disabled={state === 'checking'}
          className="flex items-center gap-1.5 rounded-lg border border-gray-200 px-3 py-1.5 text-xs text-gray-600 hover:bg-gray-50 disabled:opacity-40 dark:border-gray-700 dark:text-gray-400 dark:hover:bg-gray-800"
        >
          <RefreshCw className={`h-3.5 w-3.5 ${state === 'checking' ? 'animate-spin' : ''}`} />
          {state === 'checking' ? t('updater.checking') : t('updater.checkNow')}
        </button>
      </div>

      {/* Result area */}
      {state === 'done' && info && (
        <div className={`rounded-xl p-3 ${
          info.has_update
            ? 'bg-blue-50 dark:bg-blue-900/20'
            : 'bg-green-50 dark:bg-green-900/20'
        }`}>
          {info.has_update ? (
            <div className="space-y-2">
              <div className="flex items-center gap-2">
                <ArrowUpCircle className="h-4 w-4 text-blue-600 dark:text-blue-400 shrink-0" />
                <div>
                  <p className="text-sm font-medium text-blue-700 dark:text-blue-300">
                    {t('updater.newVersionAvailable', { version: info.latest })}
                  </p>
                  {formattedDate && (
                    <p className="text-xs text-blue-500 dark:text-blue-400">
                      {t('updater.releasedOn', { date: formattedDate })}
                    </p>
                  )}
                </div>
              </div>

              {info.release_notes && (
                <div>
                  <button
                    onClick={() => setExpanded(e => !e)}
                    className="text-xs text-blue-500 hover:text-blue-400"
                  >
                    {expanded ? t('updater.hideNotes') : t('updater.showNotes')}
                  </button>
                  {expanded && (
                    <pre className="mt-2 max-h-40 overflow-y-auto whitespace-pre-wrap rounded-lg bg-white/60 p-2 text-xs text-gray-700 dark:bg-gray-800/60 dark:text-gray-300">
                      {info.release_notes.slice(0, 2000)}
                    </pre>
                  )}
                </div>
              )}

              <button
                onClick={openRelease}
                className="flex items-center gap-1.5 rounded-lg bg-blue-600 px-3 py-1.5 text-xs font-medium text-white hover:bg-blue-500"
              >
                <Download className="h-3.5 w-3.5" />
                {t('updater.downloadNow')}
              </button>
            </div>
          ) : (
            <div className="flex items-center gap-2">
              <CheckCircle className="h-4 w-4 text-green-600 dark:text-green-400 shrink-0" />
              <p className="text-sm text-green-700 dark:text-green-300">
                {t('updater.alreadyLatest')}
              </p>
            </div>
          )}
        </div>
      )}

      {state === 'error' && error && (
        <div className="flex items-center gap-2 rounded-xl bg-red-50 p-3 dark:bg-red-900/20">
          <XCircle className="h-4 w-4 text-red-500 shrink-0" />
          <p className="text-xs text-red-600 dark:text-red-400">{error}</p>
        </div>
      )}
    </div>
  )
}
