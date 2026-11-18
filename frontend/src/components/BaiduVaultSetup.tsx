import { Channel, invoke } from '@tauri-apps/api/core'
import { open } from '@tauri-apps/plugin-dialog'
import { CheckCircle2, Copy, Download, ExternalLink, Loader2 } from 'lucide-react'
import { useCallback, useEffect, useId, useState } from 'react'
import { useTranslation } from 'react-i18next'

type Setup = { ready: boolean; installParent: string | null }
type Phase = 'installing' | 'starting' | 'mounting'
const AUTH_URL = 'https://api.oplist.org/'
const actionClass = 'inline-flex items-center gap-1.5 rounded-md px-2.5 py-1.5 text-xs font-medium focus-visible:outline-2 focus-visible:outline-[var(--app-focus)] disabled:opacity-50'

export function BaiduVaultSetup({ onReady, onBusy }: {
  onReady: (ready: boolean) => void
  onBusy: (busy: boolean) => void
}) {
  const { t } = useTranslation()
  const id = useId()
  const [state, setState] = useState<Setup | null>(null)
  const [token, setToken] = useState('')
  const [editing, setEditing] = useState(false)
  const [openingAuth, setOpeningAuth] = useState(false)
  const [manualHandoff, setManualHandoff] = useState(false)
  const [copyStatus, setCopyStatus] = useState<'copied' | 'manual' | null>(null)
  const [phase, setPhase] = useState<Phase | null>(null)
  const [error, setError] = useState('')
  const [attempt, setAttempt] = useState(0)
  const accept = useCallback((value: Setup) => { setState(value); onReady(value.ready) }, [onReady])

  useEffect(() => {
    let current = true
    invoke<Setup>('cloud_vault_baidu_setup')
      .then(value => { if (current) accept(value) })
      .catch(e => { if (current) setError(String(e)) })
    return () => { current = false }
  }, [accept, attempt])

  async function openAuthorization() {
    setOpeningAuth(true)
    setError('')
    setCopyStatus(null)
    setManualHandoff(false)
    try {
      const action = await invoke<'opened' | 'openManually'>('open_external_url', { url: AUTH_URL })
      setManualHandoff(action === 'openManually')
    } catch (e) {
      setError(String(e))
    } finally {
      setOpeningAuth(false)
    }
  }

  async function copyAuthorizationUrl() {
    try {
      await navigator.clipboard.writeText(AUTH_URL)
      setCopyStatus('copied')
    } catch {
      setCopyStatus('manual')
    }
  }

  async function deploy() {
    if (!state || !token.trim() || phase) return
    setError('')
    setPhase('installing')
    onBusy(true)
    try {
      const parent = await open({ directory: true, multiple: false,
        title: t('settings.cloudVault.baidu.folder'), defaultPath: state.installParent ?? undefined })
      if (typeof parent !== 'string') return
      const progress = new Channel<Phase>(setPhase)
      const result = await invoke<Setup>('cloud_vault_baidu_deploy', {
        installParent: parent, refreshToken: token.trim(), progress,
      })
      setToken('')
      setEditing(false)
      accept(result)
    } catch (e) {
      setError(String(e))
      // A failed reauthorization can have changed the mount. Use native readiness.
      try { accept(await invoke<Setup>('cloud_vault_baidu_setup')) } catch { onReady(false) }
    } finally {
      setPhase(null)
      onBusy(false)
    }
  }

  return <section aria-label={t('settings.cloudVault.baidu.title')}
    className="space-y-3 border-l-2 border-[var(--app-accent)] pl-3 text-xs text-[var(--app-secondary)]">
    <p>{t('settings.cloudVault.baidu.hint')}</p>
    {state?.ready && <>
      <div className="flex flex-wrap items-center gap-x-3 gap-y-1">
        <span className="inline-flex items-center gap-1.5 font-medium text-[var(--app-text)]">
          <CheckCircle2 size={14} aria-hidden="true" />{t('settings.cloudVault.baidu.ready')}
        </span>
        {!editing && <button type="button" className="text-[var(--app-accent)] underline underline-offset-2"
          onClick={() => setEditing(true)}>{t('settings.cloudVault.baidu.reconfigure')}</button>}
      </div>
      <p>{t('settings.cloudVault.baidu.location')} <code className="break-all">/VibeAssistant/agent-manager/vault.amv</code></p>
      <p>{t('settings.cloudVault.baidu.next')}</p>
    </>}
    {state && (!state.ready || editing) && <>
      <div className="space-y-1.5">
        <div className="flex flex-wrap items-center gap-2">
          <button type="button" disabled={!!phase || openingAuth} className={`${actionClass} border border-[var(--app-stroke)]`}
            onClick={() => { void openAuthorization() }}>
            <ExternalLink size={14} aria-hidden="true" />{t('settings.cloudVault.baidu.authorize')}
          </button>
          <button type="button" className={actionClass} onClick={() => { void copyAuthorizationUrl() }}>
            <Copy size={14} aria-hidden="true" /><span aria-live="polite">{t(copyStatus === 'copied' ? 'common.copied' : 'settings.cloudVault.baidu.copyAuthUrl')}</span>
          </button>
        </div>
        <p><code className="select-all break-all">{AUTH_URL}</code> {t(copyStatus === 'manual' ? 'settings.cloudVault.baidu.copyManually' : 'settings.cloudVault.baidu.manualAuth')}</p>
        {manualHandoff && <p role="status" className="font-medium text-[var(--app-text)]">{t('settings.cloudVault.baidu.preserveEdge')}</p>}
      </div>
      <div className="space-y-1.5">
        <label htmlFor={id} className="font-medium text-[var(--app-text)]">{t('settings.cloudVault.baidu.tokenLabel')}</label>
        <input id={id} type="password" autoComplete="off" spellCheck={false} value={token} disabled={!!phase}
          aria-describedby={`${id}-help`} onChange={event => setToken(event.target.value)}
          className="w-full rounded-lg border border-[var(--app-stroke)] bg-[var(--app-surface)] px-3 py-1.5 text-sm text-[var(--app-text)] focus-visible:outline-2 focus-visible:outline-[var(--app-focus)]" />
        <p id={`${id}-help`}>{t('settings.cloudVault.baidu.tokenHint')}</p>
        <p>{t('settings.cloudVault.baidu.tokenPrivacy')}</p>
      </div>
      <button type="button" disabled={!!phase || !token.trim()} onClick={() => { void deploy() }}
        className={`${actionClass} bg-cyan-600 text-white hover:bg-cyan-500`}>
        {phase ? <Loader2 size={14} className="motion-safe:animate-spin" /> : <Download size={14} aria-hidden="true" />}
        <span aria-live="polite">{t(`settings.cloudVault.baidu.${phase ?? 'deploy'}`)}</span>
      </button>
    </>}
    {!state && !error && <p role="status">{t('settings.cloudVault.loading')}</p>}
    {error && <p role="alert" className="break-words text-red-600 dark:text-red-400">{error}</p>}
    {!state && error && <button type="button" className="underline" onClick={() => { setError(''); setAttempt(n => n + 1) }}>
      {t('settings.cloudVault.baidu.loadRetry')}
    </button>}
  </section>
}
