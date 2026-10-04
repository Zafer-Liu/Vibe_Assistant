import { useTranslation } from 'react-i18next'
import { invoke } from '@tauri-apps/api/core'
import { save, open } from '@tauri-apps/plugin-dialog'
import { Download, Upload, Loader2, CheckCircle2, AlertCircle, Brain, Info, DatabaseBackup, Cloud, Smartphone, Copy, RotateCcw, Timer } from 'lucide-react'
import { useState, useCallback, useEffect, type ReactNode } from 'react'
import { LlmSettings } from './LlmSettings'
import { ErrorRecovery } from '../components/ErrorRecovery'

export function Settings({ onOpenOnboarding }: { onOpenOnboarding: () => void }) {
  const { t } = useTranslation()

  return (
    <div className="flex h-full flex-col overflow-y-auto bg-gray-50 dark:bg-gray-950">
      <div className="mx-auto w-full max-w-3xl px-6 py-8 space-y-6">
      {/* Page header */}
      <div>
        <h2 className="text-lg font-semibold text-gray-900 dark:text-gray-100">
          {t('settings.title')}
        </h2>
        <p className="text-xs text-gray-500 mt-0.5">{t('settings.subtitle')}</p>
        <button type="button" onClick={onOpenOnboarding} className="mt-3 rounded-lg border border-gray-200 px-3 py-1.5 text-xs font-medium text-gray-600 hover:bg-white focus-visible:outline-2 focus-visible:outline-blue-500 dark:border-gray-700 dark:text-gray-300 dark:hover:bg-gray-800">{t('onboarding.reopen')}</button>
      </div>

      {/* LLM & Memory */}
      <SettingsSection
        icon={<Brain size={15} className="text-violet-600 dark:text-violet-400" />}
        title={t('settings.sectionLlm')}
        desc={t('settings.sectionLlmHint')}
      >
        <LlmSettings embedded />
      </SettingsSection>

      {/* Memory scheduled refresh */}
      <SettingsSection
        icon={<Timer size={15} className="text-sky-600 dark:text-sky-400" />}
        title={t('settings.memoryAutoRefresh.title')}
        desc={t('settings.memoryAutoRefresh.hint')}
      >
        <Card>
        <MemoryAutoRefreshSettings />
        </Card>
      </SettingsSection>

      {/* Backup & Restore */}
      <SettingsSection
        icon={<DatabaseBackup size={15} className="text-blue-600 dark:text-blue-400" />}
        title={t('settings.sectionBackup')}
        desc={t('settings.sectionBackupHint')}
      >
        <Card>
        <BackupRestore />
        </Card>
      </SettingsSection>

      {/* Cloud Memory Vault */}
      <SettingsSection
        icon={<Cloud size={15} className="text-cyan-600 dark:text-cyan-400" />}
        title={t('settings.cloudVault.title')}
        desc={t('settings.cloudVault.hint')}
      >
        <Card>
        <CloudVaultSettings />
        </Card>
      </SettingsSection>

      {/* Mobile read-only status */}
      <SettingsSection
        icon={<Smartphone size={15} className="text-emerald-600 dark:text-emerald-400" />}
        title={t('settings.mobileStatus.title')}
        desc={t('settings.mobileStatus.hint')}
      >
        <Card>
          <MobileStatusSettings />
        </Card>
      </SettingsSection>

      {/* About */}
      <SettingsSection
        icon={<Info size={15} className="text-gray-500" />}
        title={t('settings.sectionAbout')}
      >
        <Card>
        <div className="space-y-2">
          <AboutRow label={t('settings.aboutName')} value={t('app.title')} />
          <AboutRow label={t('settings.aboutLicense')} value="Apache 2.0" />
          <AboutRow
            label={t('settings.aboutSource')}
            value="GitHub"
            href="https://github.com/Zafer-Liu/Agent_Manager"
          />
        </div>
        </Card>
      </SettingsSection>
      </div>
    </div>
  )
}

interface MobileStatusSettingsView {
  enabled: boolean
  url: string
  port: number
  token_set: boolean
}

interface MemoryAutoRefreshView {
  enabled: boolean
  l2_days: number
  l3_days: number
}

/** 记忆定时重算：开关 + L2/L3 重建间隔（天）。改动即保存、即生效。 */
function MemoryAutoRefreshSettings() {
  const { t } = useTranslation()
  const [config, setConfig] = useState<MemoryAutoRefreshView | null>(null)
  const [draft, setDraft] = useState<{ l2: string; l3: string } | null>(null)
  const [working, setWorking] = useState(false)
  const [saved, setSaved] = useState(false)
  const [error, setError] = useState<string | null>(null)

  useEffect(() => {
    invoke<MemoryAutoRefreshView>('memory_auto_refresh_get')
      .then((loaded) => {
        setConfig(loaded)
        setDraft({ l2: String(loaded.l2_days), l3: String(loaded.l3_days) })
      })
      .catch((cause) => setError(String(cause)))
  }, [])

  async function save(next: Partial<MemoryAutoRefreshView>) {
    if (!config) return
    setWorking(true)
    setError(null)
    try {
      const merged = { ...config, ...next }
      // 后端收敛越界值后回传，输入框同步为生效值。
      const stored = await invoke<MemoryAutoRefreshView>('memory_auto_refresh_set', {
        enabled: merged.enabled,
        l2Days: merged.l2_days,
        l3Days: merged.l3_days,
      })
      setConfig(stored)
      setDraft({ l2: String(stored.l2_days), l3: String(stored.l3_days) })
      setSaved(true)
      setTimeout(() => setSaved(false), 2400)
    } catch (cause) {
      setError(String(cause))
    } finally {
      setWorking(false)
    }
  }

  /** 数字输入失焦/回车时提交；非法输入回退当前生效值。 */
  function commitDays(which: 'l2' | 'l3') {
    if (!config || !draft) return
    const current = which === 'l2' ? config.l2_days : config.l3_days
    const parsed = Number.parseInt(draft[which], 10)
    const value = Number.isFinite(parsed) && parsed > 0 ? parsed : current
    if (value === current) {
      setDraft({ ...draft, [which]: String(current) })
      return
    }
    void save(which === 'l2' ? { l2_days: value } : { l3_days: value })
  }

  if (!config && !error) {
    return <div className="flex items-center gap-2 text-xs text-gray-400"><Loader2 size={14} className="animate-spin" />{t('settings.mobileStatus.loading')}</div>
  }

  return (
    <div className="space-y-3">
      <div className="flex items-start justify-between gap-4">
        <div>
          <div className="text-sm font-medium text-gray-800 dark:text-gray-200">{t('settings.memoryAutoRefresh.enable')}</div>
          <p className="mt-0.5 text-xs leading-5 text-gray-500 dark:text-gray-400">{t('settings.memoryAutoRefresh.enableHint')}</p>
        </div>
        <button
          type="button"
          role="switch"
          aria-checked={config?.enabled ?? false}
          aria-label={t('settings.memoryAutoRefresh.enable')}
          disabled={working || !config}
          onClick={() => { void save({ enabled: !(config?.enabled ?? false) }) }}
          className={`relative mt-0.5 h-6 w-11 shrink-0 rounded-full transition ${config?.enabled ? 'bg-sky-600' : 'bg-gray-300 dark:bg-gray-700'} disabled:opacity-50`}
        >
          <span className={`absolute top-0.5 h-5 w-5 rounded-full bg-white shadow-sm transition-all ${config?.enabled ? 'left-5.5' : 'left-0.5'}`} />
        </button>
      </div>

      {config?.enabled && draft && (
        <div className="grid gap-3 sm:grid-cols-2">
          <label className="block rounded-lg border border-sky-200 bg-sky-50/60 p-3 dark:border-sky-900/60 dark:bg-sky-950/20">
            <span className="text-xs font-medium text-sky-900 dark:text-sky-200">{t('settings.memoryAutoRefresh.l2Label')}</span>
            <div className="mt-1.5 flex items-center gap-2">
              <input
                type="number"
                min={1}
                max={90}
                value={draft.l2}
                disabled={working}
                spellCheck={false}
                onChange={(e) => { setDraft({ ...draft, l2: e.target.value }) }}
                onBlur={() => { commitDays('l2') }}
                onKeyDown={(e) => { if (e.key === 'Enter') { e.currentTarget.blur() } }}
                className="w-20 rounded-md border border-gray-300 bg-white px-2 py-1 text-sm text-gray-900 focus:border-sky-500 focus:outline-none dark:border-gray-600 dark:bg-gray-900 dark:text-gray-100"
              />
              <span className="text-xs text-gray-500 dark:text-gray-400">{t('settings.memoryAutoRefresh.daysUnit')}</span>
              {saved && <CheckCircle2 size={14} className="text-emerald-500" />}
            </div>
            <p className="mt-1.5 text-[11px] leading-4 text-sky-800/80 dark:text-sky-300/80">{t('settings.memoryAutoRefresh.l2Hint')}</p>
          </label>
          <label className="block rounded-lg border border-sky-200 bg-sky-50/60 p-3 dark:border-sky-900/60 dark:bg-sky-950/20">
            <span className="text-xs font-medium text-sky-900 dark:text-sky-200">{t('settings.memoryAutoRefresh.l3Label')}</span>
            <div className="mt-1.5 flex items-center gap-2">
              <input
                type="number"
                min={1}
                max={365}
                value={draft.l3}
                disabled={working}
                spellCheck={false}
                onChange={(e) => { setDraft({ ...draft, l3: e.target.value }) }}
                onBlur={() => { commitDays('l3') }}
                onKeyDown={(e) => { if (e.key === 'Enter') { e.currentTarget.blur() } }}
                className="w-20 rounded-md border border-gray-300 bg-white px-2 py-1 text-sm text-gray-900 focus:border-sky-500 focus:outline-none dark:border-gray-600 dark:bg-gray-900 dark:text-gray-100"
              />
              <span className="text-xs text-gray-500 dark:text-gray-400">{t('settings.memoryAutoRefresh.daysUnit')}</span>
              {saved && <CheckCircle2 size={14} className="text-emerald-500" />}
            </div>
            <p className="mt-1.5 text-[11px] leading-4 text-sky-800/80 dark:text-sky-300/80">{t('settings.memoryAutoRefresh.l3Hint')}</p>
          </label>
        </div>
      )}

      <p className="border-t border-gray-100 pt-3 text-xs leading-5 text-gray-400 dark:border-gray-800 dark:text-gray-500">{t('settings.memoryAutoRefresh.footer')}</p>

      {error && (
        <div role="alert" className="flex items-start gap-2 text-xs text-red-600 dark:text-red-400">
          <AlertCircle size={14} className="mt-0.5 shrink-0" />
          <span className="break-all">{error}</span>
          <button type="button" onClick={() => { window.location.reload() }} className="ml-auto shrink-0 underline underline-offset-2">{t('common.retry')}</button>
        </div>
      )}
    </div>
  )
}

function MobileStatusSettings() {
  const { t } = useTranslation()
  const [settings, setSettings] = useState<MobileStatusSettingsView | null>(null)
  const [working, setWorking] = useState(false)
  const [copied, setCopied] = useState(false)
  const [error, setError] = useState<string | null>(null)

  useEffect(() => {
    invoke<MobileStatusSettingsView>('mobile_status_get_settings')
      .then(setSettings)
      .catch((cause) => setError(String(cause)))
  }, [])

  async function toggle(enabled: boolean) {
    setWorking(true)
    setError(null)
    try {
      setSettings(await invoke<MobileStatusSettingsView>('mobile_status_set_enabled', { enabled }))
    } catch (cause) {
      setError(String(cause))
    } finally {
      setWorking(false)
    }
  }

  async function rotateToken() {
    setWorking(true)
    setError(null)
    try {
      setSettings(await invoke<MobileStatusSettingsView>('mobile_status_rotate_token'))
    } catch (cause) {
      setError(String(cause))
    } finally {
      setWorking(false)
    }
  }

  async function copyUrl() {
    if (!settings) return
    try {
      await navigator.clipboard.writeText(settings.url)
      setCopied(true)
      setTimeout(() => setCopied(false), 1800)
    } catch (cause) {
      setError(String(cause))
    }
  }

  if (!settings && !error) {
    return <div className="flex items-center gap-2 text-xs text-gray-400"><Loader2 size={14} className="animate-spin" />{t('settings.mobileStatus.loading')}</div>
  }

  return (
    <div className="space-y-3">
      <div className="flex items-start justify-between gap-4">
        <div>
          <div className="text-sm font-medium text-gray-800 dark:text-gray-200">{t('settings.mobileStatus.enable')}</div>
          <p className="mt-0.5 text-xs leading-5 text-gray-500 dark:text-gray-400">{t('settings.mobileStatus.enableHint')}</p>
        </div>
        <button
          type="button"
          role="switch"
          aria-checked={settings?.enabled ?? false}
          aria-label={t('settings.mobileStatus.enable')}
          disabled={working || !settings}
          onClick={() => { void toggle(!(settings?.enabled ?? false)) }}
          className={`relative mt-0.5 h-6 w-11 shrink-0 rounded-full transition ${settings?.enabled ? 'bg-emerald-600' : 'bg-gray-300 dark:bg-gray-700'} disabled:opacity-50`}
        >
          <span className={`absolute top-0.5 h-5 w-5 rounded-full bg-white shadow-sm transition-all ${settings?.enabled ? 'left-5.5' : 'left-0.5'}`} />
        </button>
      </div>

      {settings?.enabled && (
        <div className="rounded-lg border border-emerald-200 bg-emerald-50/70 p-3 dark:border-emerald-900/60 dark:bg-emerald-950/20">
          <div className="text-xs font-medium text-emerald-900 dark:text-emerald-200">{t('settings.mobileStatus.url')}</div>
          <div className="mt-2 flex items-start gap-2">
            <code className="min-w-0 flex-1 break-all rounded-md bg-white px-2.5 py-2 text-xs leading-5 text-gray-700 dark:bg-gray-900 dark:text-gray-200">{settings.url}</code>
            <button
              type="button"
              onClick={() => { void copyUrl() }}
              className="inline-flex shrink-0 items-center gap-1.5 rounded-md border border-emerald-300 bg-white px-2.5 py-2 text-xs font-medium text-emerald-800 hover:bg-emerald-50 dark:border-emerald-800 dark:bg-gray-900 dark:text-emerald-300 dark:hover:bg-gray-800"
            >
              {copied ? <CheckCircle2 size={14} /> : <Copy size={14} />}
              {copied ? t('common.copied') : t('common.copy')}
            </button>
          </div>
          <p className="mt-2 text-xs leading-5 text-emerald-800/80 dark:text-emerald-300/80">{t('settings.mobileStatus.urlHint', { port: settings.port })}</p>
        </div>
      )}

      <div className="flex flex-wrap items-center justify-between gap-3 border-t border-gray-100 pt-3 dark:border-gray-800">
        <p className="max-w-xl text-xs leading-5 text-gray-400 dark:text-gray-500">{t('settings.mobileStatus.security')}</p>
        <button
          type="button"
          disabled={working || !settings}
          onClick={() => { void rotateToken() }}
          className="inline-flex shrink-0 items-center gap-1.5 rounded-lg border border-gray-200 px-3 py-1.5 text-xs font-medium text-gray-600 hover:bg-gray-50 disabled:opacity-50 dark:border-gray-700 dark:text-gray-300 dark:hover:bg-gray-800"
        >
          {working ? <Loader2 size={14} className="animate-spin" /> : <RotateCcw size={14} />}
          {t('settings.mobileStatus.rotate')}
        </button>
      </div>

      {error && (
        <div role="alert" className="flex items-start gap-2 text-xs text-red-600 dark:text-red-400">
          <AlertCircle size={14} className="mt-0.5 shrink-0" />
          <span className="break-all">{error}</span>
        </div>
      )}
    </div>
  )
}

/// Section header with icon badge — no card wrapper so children manage
/// their own visual containment (avoids cards-in-cards nesting).
function SettingsSection({
  icon,
  title,
  desc,
  children,
}: {
  icon: ReactNode
  title: string
  desc?: string
  children: ReactNode
}) {
  return (
    <section className="space-y-3">
      <div className="flex items-center gap-2.5">
        <div className="flex h-7 w-7 shrink-0 items-center justify-center rounded-md bg-gray-100 dark:bg-gray-800">
          {icon}
        </div>
        <div>
          <h3 className="text-sm font-semibold text-gray-800 dark:text-gray-200">{title}</h3>
          {desc && (
            <p className="text-xs leading-5 text-gray-500 dark:text-gray-400">{desc}</p>
          )}
        </div>
      </div>
      {children}
    </section>
  )
}

function Card({ children }: { children: ReactNode }) {
  return (
    <div className="rounded-lg border border-gray-200 bg-white p-4 dark:border-gray-800 dark:bg-gray-900">
      {children}
    </div>
  )
}

interface VaultSettingsView {
  url: string
  enabled: boolean
  pat_set: boolean
  password_set: boolean
  auto_interval_min: number
}

type VaultStatus =
  | { kind: 'idle' }
  | { kind: 'working'; label: string }
  | { kind: 'success'; label: string }
  | { kind: 'error'; label: string }

function CloudVaultSettings() {
  const { t } = useTranslation()
  const [url, setUrl] = useState('')
  const [pat, setPat] = useState('')
  const [password, setPassword] = useState('')
  const [passwordSet, setPasswordSet] = useState(false)
  const [autoInterval, setAutoInterval] = useState(0)
  const [enabled, setEnabled] = useState(false)
  const [patSet, setPatSet] = useState(false)
  const [status, setStatus] = useState<VaultStatus>({ kind: 'idle' })
  const [loaded, setLoaded] = useState(false)

  useEffect(() => {
    invoke<VaultSettingsView>('cloud_vault_get_settings').then((v) => {
      setUrl(v.url)
      setEnabled(v.enabled)
      setPatSet(v.pat_set)
      setPasswordSet(v.password_set)
      setAutoInterval(v.auto_interval_min)
      setLoaded(true)
    })
  }, [])

  if (!loaded) {
    return <div className="text-xs text-gray-400">{t('settings.cloudVault.loading')}</div>
  }

  const handleTest = async () => {
    setStatus({ kind: 'working', label: t('settings.cloudVault.testing') })
    try {
      // When the field is blank, let the native process use the encrypted PAT
      // already stored locally.  Never send a placeholder as a bearer token.
      const version = await invoke<string>('cloud_vault_test_connection', { url, pat: pat || null })
      setStatus({ kind: 'success', label: t('settings.cloudVault.testOk', { version }) })
    } catch (e) {
      setStatus({ kind: 'error', label: String(e) })
    }
  }

  const handleSave = async () => {
    setStatus({ kind: 'working', label: t('settings.cloudVault.saving') })
    try {
      await invoke('cloud_vault_save_settings', {
        url,
        pat: pat || null,
        password: password || null,
        autoIntervalMin: autoInterval,
        enabled,
      })
      if (pat) {
        setPat('')
        setPatSet(true)
      }
      if (password) {
        setPassword('')
        setPasswordSet(true)
      }
      setStatus({ kind: 'success', label: t('settings.cloudVault.saved') })
    } catch (e) {
      setStatus({ kind: 'error', label: String(e) })
    }
  }

  return (
    <div className="space-y-3">
      <div className="space-y-1.5">
        <label className="text-xs font-medium text-gray-600 dark:text-gray-300">
          {t('settings.cloudVault.url')}
        </label>
        <input
          value={url}
          onChange={(e) => setUrl(e.target.value)}
          placeholder="http://192.168.x.x:8787"
          className="w-full rounded-lg border border-gray-200 bg-white px-3 py-1.5 text-sm text-gray-900 outline-none focus:border-cyan-400 dark:border-gray-700 dark:bg-gray-800 dark:text-gray-100"
        />
      </div>
      <div className="space-y-1.5">
        <label className="text-xs font-medium text-gray-600 dark:text-gray-300">
          {t('settings.cloudVault.pat')}
          {patSet && !pat && (
            <span className="ml-2 font-normal text-green-600 dark:text-green-400">
              {t('settings.cloudVault.patSaved')}
            </span>
          )}
        </label>
        <input
          value={pat}
          onChange={(e) => setPat(e.target.value)}
          type="password"
          placeholder={patSet ? '••••••••' : 'vault-...'}
          className="w-full rounded-lg border border-gray-200 bg-white px-3 py-1.5 text-sm text-gray-900 outline-none focus:border-cyan-400 dark:border-gray-700 dark:bg-gray-800 dark:text-gray-100"
        />
      </div>
      <div className="space-y-1.5">
        <label className="text-xs font-medium text-gray-600 dark:text-gray-300">
          {t('settings.cloudVault.password')}
          {passwordSet && !password ? (
            <span className="ml-2 font-normal text-green-600 dark:text-green-400">
              {t('settings.cloudVault.patSaved')}
            </span>
          ) : !passwordSet ? (
            <span className="ml-2 font-normal text-amber-600 dark:text-amber-400">
              {t('settings.cloudVault.passwordNotSet')}
            </span>
          ) : null}
        </label>
        <input
          value={password}
          onChange={(e) => setPassword(e.target.value)}
          type="password"
          placeholder={passwordSet ? '••••••••' : t('settings.cloudVault.passwordPlaceholder')}
          className="w-full rounded-lg border border-gray-200 bg-white px-3 py-1.5 text-sm text-gray-900 outline-none focus:border-cyan-400 dark:border-gray-700 dark:bg-gray-800 dark:text-gray-100"
        />
      </div>
      <div className="space-y-1.5">
        <label className="text-xs font-medium text-gray-600 dark:text-gray-300">
          {t('settings.cloudVault.autoInterval')}
        </label>
        <select
          value={autoInterval}
          onChange={(e) => setAutoInterval(Number(e.target.value))}
          className="w-full rounded-lg border border-gray-200 bg-white px-3 py-1.5 text-sm text-gray-900 outline-none dark:border-gray-700 dark:bg-gray-800 dark:text-gray-100"
        >
          {[0, 5, 15, 30, 60, 360].map((min) => (
            <option key={min} value={min}>
              {min === 0
                ? t('settings.cloudVault.autoOff')
                : t('settings.cloudVault.autoEvery', { min })}
            </option>
          ))}
        </select>
      </div>
      <label className="flex items-center gap-2 text-xs text-gray-600 dark:text-gray-300">
        <input
          type="checkbox"
          checked={enabled}
          onChange={(e) => setEnabled(e.target.checked)}
          className="h-3.5 w-3.5 rounded border-gray-300"
        />
        {t('settings.cloudVault.enable')}
      </label>
      <div className="flex flex-wrap gap-2">
        <button
          onClick={handleTest}
          className="inline-flex items-center gap-1.5 rounded-lg border border-gray-200 px-3 py-1.5 text-xs font-medium text-gray-600 hover:bg-gray-50 disabled:opacity-40 dark:border-gray-700 dark:text-gray-300 dark:hover:bg-gray-800"
        >
          {status.kind === 'working' ? <Loader2 size={14} className="animate-spin" /> : <Cloud size={14} />}
          {t('settings.cloudVault.test')}
        </button>
        <button
          onClick={handleSave}
          className="inline-flex items-center gap-1.5 rounded-lg bg-cyan-600 px-3 py-1.5 text-xs font-medium text-white hover:bg-cyan-500"
        >
          {t('settings.cloudVault.save')}
        </button>
      </div>
      {status.kind === 'success' && (
        <div className="flex items-start gap-2 text-xs text-green-600 dark:text-green-400">
          <CheckCircle2 size={14} className="mt-0.5 shrink-0" />
          <span>{status.label}</span>
        </div>
      )}
      {status.kind === 'error' && (
        <div role="alert" className="flex items-start gap-2 text-xs text-red-600 dark:text-red-400">
          <AlertCircle size={14} className="mt-0.5 shrink-0" />
          <div className="min-w-0 flex-1">
            <p className="break-all">{status.label}</p>
            <ErrorRecovery error={status.label} onRetry={() => { void handleTest() }} />
          </div>
        </div>
      )}
      <p className="text-xs text-gray-400 dark:text-gray-500">
        {t('settings.cloudVault.passwordStoreHint')}
      </p>
    </div>
  )
}

function AboutRow({
  label,
  value,
  href,
}: {
  label: string
  value: string
  href?: string
}) {
  return (
    <div className="flex items-center justify-between text-sm">
      <span className="text-gray-500 dark:text-gray-400">{label}</span>
      {href ? (
        <a
          href={href}
          target="_blank"
          rel="noreferrer"
          className="font-medium text-blue-600 hover:underline dark:text-blue-400"
        >
          {value}
        </a>
      ) : (
        <span className="font-medium text-gray-900 dark:text-gray-100">{value}</span>
      )}
    </div>
  )
}

interface ExportManifest {
  version: number
  created_at: string
  app_version: string
  tables: string[]
  skill_count: number
  backup_count: number
}

interface ImportResult {
  tables_restored: number
  skills_restored: number
  backups_restored: number
  app_restart_required: boolean
}

type Status =
  | { kind: 'idle' }
  | { kind: 'working'; label: string }
  | { kind: 'success'; label: string }
  | { kind: 'error'; label: string }

function BackupRestore() {
  const { t } = useTranslation()
  const [status, setStatus] = useState<Status>({ kind: 'idle' })

  const handleExport = useCallback(async () => {
    setStatus({ kind: 'working', label: t('backup.exporting') })
    try {
      const path = await save({
        title: t('backup.exportTitle'),
        defaultPath: 'agent-manager-backup.zip',
        filters: [{ name: 'ZIP', extensions: ['zip'] }],
      })
      if (!path) {
        setStatus({ kind: 'idle' })
        return
      }
      const manifest = await invoke<ExportManifest>('config_export', { destPath: path })
      setStatus({
        kind: 'success',
        label: t('backup.exportDone', {
          tables: manifest.tables.length,
          skills: manifest.skill_count,
          backups: manifest.backup_count,
        }),
      })
    } catch (e) {
      setStatus({ kind: 'error', label: String(e) })
    }
  }, [t])

  const handleImport = useCallback(async () => {
    setStatus({ kind: 'working', label: t('backup.importing') })
    try {
      const path = await open({
        title: t('backup.importTitle'),
        filters: [{ name: 'ZIP', extensions: ['zip'] }],
        multiple: false,
        directory: false,
      })
      if (!path || typeof path !== 'string') {
        setStatus({ kind: 'idle' })
        return
      }
      const result = await invoke<ImportResult>('config_import', { sourcePath: path })
      setStatus({
        kind: 'success',
        label: t('backup.importDone', {
          tables: result.tables_restored,
          skills: result.skills_restored,
          backups: result.backups_restored,
        }),
      })
    } catch (e) {
      setStatus({ kind: 'error', label: String(e) })
    }
  }, [t])

  const busy = status.kind === 'working'

  return (
    <div className="space-y-3">
      <div className="flex flex-wrap gap-2">
        <button
          onClick={handleExport}
          disabled={busy}
          className="inline-flex items-center gap-1.5 rounded-lg bg-blue-600 px-3 py-1.5 text-xs font-medium text-white hover:bg-blue-500 disabled:opacity-40"
        >
          {busy ? <Loader2 size={14} className="animate-spin" /> : <Download size={14} />}
          {t('backup.export')}
        </button>
        <button
          onClick={handleImport}
          disabled={busy}
          className="inline-flex items-center gap-1.5 rounded-lg border border-gray-200 px-3 py-1.5 text-xs font-medium text-gray-600 hover:bg-gray-50 disabled:opacity-40 dark:border-gray-700 dark:text-gray-300 dark:hover:bg-gray-800"
        >
          {busy ? <Loader2 size={14} className="animate-spin" /> : <Upload size={14} />}
          {t('backup.import')}
        </button>
      </div>

      {status.kind === 'success' && (
        <div className="flex items-start gap-2 text-xs text-green-600 dark:text-green-400">
          <CheckCircle2 size={14} className="mt-0.5 shrink-0" />
          <span>{status.label}</span>
        </div>
      )}
      {status.kind === 'error' && (
        <div role="alert" className="flex items-start gap-2 text-xs text-red-600 dark:text-red-400">
          <AlertCircle size={14} className="mt-0.5 shrink-0" />
          <div className="min-w-0 flex-1">
            <p className="break-all">{status.label}</p>
            <ErrorRecovery error={status.label} />
          </div>
        </div>
      )}
      {status.kind === 'working' && (
        <div className="flex items-center gap-2 text-xs text-gray-500">
          <Loader2 size={14} className="animate-spin" />
          <span>{status.label}</span>
        </div>
      )}

      <p className="text-xs text-gray-400 dark:text-gray-500">{t('backup.importWarning')}</p>
    </div>
  )
}
