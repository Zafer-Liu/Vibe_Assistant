import { useCallback, useEffect, useId, useState } from 'react'
import { useTranslation } from 'react-i18next'
import { invoke } from '@tauri-apps/api/core'
import { Plus, Trash2, CheckCircle, XCircle, Loader2, Eye, EyeOff, AlertCircle, Brain, ExternalLink } from 'lucide-react'

const SPONSOR_PROVIDER_ID = '88api'
const SPONSOR_CREDITS_URL = 'https://88api.ai/sign-up?aff=VNNV'

export interface LlmProvider {
  id: string
  name: string
  base_url: string
  model: string
  api_key: string
  is_custom: boolean
  enabled: boolean
  context_window?: number
  max_output_tokens?: number
}

interface MemoryExtractionConfig {
  provider_id: string | null
}

interface OllamaConfig {
  base_url: string
}

interface OllamaModelInfo {
  name: string
  size: number
  parameter_size?: string
  quantization_level?: string
}

interface OllamaTestResult {
  version: string | null
  models: OllamaModelInfo[]
}

const EMPTY_CUSTOM: LlmProvider = {
  id: '', name: '', base_url: '', model: '',
  api_key: '', is_custom: true, enabled: true,
  context_window: 128000, max_output_tokens: 16384,
}

export function LlmSettings({ embedded = false, onBusyChange, onReadinessChange }: {
  embedded?: boolean
  onBusyChange?: (busy: boolean) => void
  onReadinessChange?: (ready: boolean) => void
}) {
  const { t } = useTranslation()
  const [loading, setLoading] = useState(true)
  const [loaded, setLoaded] = useState(false)
  const [ollamaBusy, setOllamaBusy] = useState(false)
  const [ollamaReady, setOllamaReady] = useState(false)
  const [builtinReadiness, setBuiltinReadiness] = useState<Record<string, boolean>>({})
  const [providers, setProviders] = useState<LlmProvider[]>([])
  const [showCustomForm, setShowCustomForm] = useState(false)
  const [editingCustom, setEditingCustom] = useState<LlmProvider | null>(null)
  const [form, setForm] = useState<LlmProvider>({ ...EMPTY_CUSTOM })
  const [testing, setTesting] = useState<string | null>(null)
  const [testResult, setTestResult] = useState<Record<string, { ok: boolean; msg: string }>>({})
  const [saving, setSaving] = useState(false)
  const [errors, setErrors] = useState<Record<string, string>>({})
  const [showKeys, setShowKeys] = useState<Record<string, boolean>>({})
  const [memoryConfig, setMemoryConfig] = useState<MemoryExtractionConfig>({ provider_id: null })
  const [savingMemoryConfig, setSavingMemoryConfig] = useState(false)
  const busy = loading || saving || savingMemoryConfig || testing !== null || ollamaBusy
  const hasErrors = Object.values(errors).some(Boolean)
  const ready = loaded && !hasErrors && !showCustomForm && ollamaReady
    && providers.filter(p => !p.is_custom).every(p => builtinReadiness[p.id] !== false)

  const setError = useCallback((scope: string, message: string) => {
    setErrors(previous => previous[scope] === message ? previous : { ...previous, [scope]: message })
  }, [])

  // Stable callbacks and no-op updates avoid card effect/render feedback loops.
  const onBuiltinReadinessChange = useCallback((id: string, value: boolean) => {
    setBuiltinReadiness(previous => previous[id] === value ? previous : { ...previous, [id]: value })
  }, [])
  const discardBuiltinError = useCallback((id: string) => setError(`provider:${id}`, ''), [setError])

  useEffect(() => {
    onBusyChange?.(busy)
    return () => onBusyChange?.(false)
  }, [busy, onBusyChange])

  useEffect(() => { onReadinessChange?.(ready) }, [ready, onReadinessChange])

  const load = useCallback(async () => {
    setLoading(true)
    try {
      const [list, config] = await Promise.all([
        invoke<LlmProvider[]>('list_llm_providers'),
        invoke<MemoryExtractionConfig>('memory_extraction_config_get'),
      ])
      setProviders(list)
      setMemoryConfig(config)
      setLoaded(true)
      setError('load', '')
    } catch (e) {
      setError('load', String(e))
      throw e
    } finally {
      setLoading(false)
    }
  }, [setError])

  useEffect(() => { void load().catch(() => { /* load already reports the error */ }) }, [load])

  async function refresh() {
    try {
      await load()
      // Explicitly reconcile failed operations with storage, retaining all drafts.
      setErrors({})
    } catch { /* load already reports the error */ }
  }

  async function saveBuiltin(p: LlmProvider): Promise<boolean> {
    setSaving(true)
    try {
      await invoke('save_llm_provider', { provider: p })
      await load()
      setError(`provider:${p.id}`, '')
      return true
    } catch (e) {
      setError(`provider:${p.id}`, String(e))
      return false
    } finally {
      setSaving(false)
    }
  }

  async function testProvider(p: LlmProvider) {
    setTesting(p.id)
    try {
      const msg = await invoke<string>('test_llm_provider', { provider: p })
      setTestResult(r => ({ ...r, [p.id]: { ok: true, msg } }))
    } catch (e) {
      setTestResult(r => ({ ...r, [p.id]: { ok: false, msg: String(e) } }))
    } finally {
      setTesting(null)
    }
  }

  async function saveCustom() {
    if (!form.name.trim()) { setError('custom', t('llm.nameRequired')); return }
    if (!form.base_url.trim()) { setError('custom', t('llm.baseUrlRequired')); return }
    if (!form.model.trim()) { setError('custom', t('llm.modelRequired')); return }
    if (!form.api_key.trim()) { setError('custom', t('llm.apiKeyRequired')); return }

    const id = editingCustom?.id || form.id || `custom_${form.name.toLowerCase().replace(/\s+/g, '_')}_${Date.now()}`
    // Retain the id if the write succeeds but the following reload fails.
    setForm(f => ({ ...f, id }))
    setSaving(true)
    try {
      await invoke('save_llm_provider', { provider: { ...form, id, is_custom: true } })
      await load()
      setError('custom', '')
      setShowCustomForm(false)
      setEditingCustom(null)
      setForm({ ...EMPTY_CUSTOM })
    } catch (e) {
      setError('custom', String(e))
    } finally {
      setSaving(false)
    }
  }

  async function deleteCustom(id: string) {
    setSaving(true)
    try {
      await invoke('delete_llm_provider', { id })
      if (memoryConfig.provider_id === id) {
        await invoke('memory_extraction_config_set', { config: { provider_id: null } })
      }
      await load()
      setError(`provider:${id}`, '')
    } catch (e) {
      setError(`provider:${id}`, String(e))
    } finally {
      setSaving(false)
    }
  }

  async function saveMemoryConfig(providerId: string) {
    setSavingMemoryConfig(true)
    try {
      const config = { provider_id: providerId || null }
      await invoke('memory_extraction_config_set', { config })
      setMemoryConfig(config)
      setError('memory', '')
    } catch (e) {
      setError('memory', String(e))
    } finally {
      setSavingMemoryConfig(false)
    }
  }

  const builtins = providers.filter(p => !p.is_custom)
  const customs = providers.filter(p => p.is_custom)

  return (
    <fieldset disabled={busy} className={embedded ? 'min-w-0 space-y-3' : 'flex h-full min-w-0 flex-col overflow-y-auto bg-gray-50 p-6 space-y-6 dark:bg-gray-950'}>
      {!embedded && <div>
        <h2 className="text-base font-semibold text-gray-900 dark:text-gray-100">{t('llm.title')}</h2>
        <p className="text-xs text-gray-500 mt-0.5">{t('llm.subtitle')}</p>
      </div>}

      <section className="rounded-lg border border-gray-200 bg-white p-4 space-y-2 dark:border-gray-700 dark:bg-gray-900">
        <div className="flex items-center gap-2"><Brain size={15} className="text-violet-600 dark:text-violet-400" /><h3 className="text-sm font-semibold text-gray-800 dark:text-gray-100">{t('llm.memoryModelTitle')}</h3></div>
        <p className="text-xs leading-5 text-gray-600 dark:text-gray-300">{t('llm.memoryModelHint')}</p>
        <div className="flex flex-wrap items-center gap-2">
          <select aria-label={t('llm.memoryModelTitle')} value={memoryConfig.provider_id ?? ''} onChange={(event) => { void saveMemoryConfig(event.target.value) }} disabled={savingMemoryConfig} className="min-w-60 rounded-md border border-gray-300 bg-white px-2.5 py-1.5 text-sm text-gray-800 outline-none focus:border-violet-500 disabled:opacity-50 dark:border-gray-600 dark:bg-gray-800 dark:text-gray-100">
            <option value="">{t('llm.memoryModelNone')}</option>
            {providers.filter((provider) => provider.enabled && provider.api_key.trim() && provider.model.trim()).map((provider) => <option key={provider.id} value={provider.id}>{provider.name} · {provider.model}</option>)}
          </select>
          {savingMemoryConfig && <Loader2 size={14} className="animate-spin text-violet-600" />}
          {memoryConfig.provider_id && <span className="text-xs text-green-700 dark:text-green-400">{t('llm.memoryModelReady')}</span>}
        </div>
      </section>

      {hasErrors && (
        <div role="alert" className="flex items-center gap-2 rounded-lg bg-red-50 px-3 py-2 text-xs text-red-600 dark:bg-red-900/20 dark:text-red-400">
          <AlertCircle className="h-3.5 w-3.5 shrink-0" />
          <div className="min-w-0 flex-1 break-words">
            {Object.entries(errors).filter(([, message]) => message).map(([scope, message]) => <p key={scope}>{message}</p>)}
          </div>
          <button onClick={() => { void refresh() }} className="shrink-0 underline">{t('common.refresh')}</button>
        </div>
      )}

      {/* Built-in providers */}
      <div className="space-y-3">
        <h3 className="text-xs font-semibold uppercase tracking-wide text-gray-500">{t('llm.builtin')}</h3>
        {builtins.map(p => (
          <BuiltinCard
            key={p.id}
            provider={p}
            showKey={showKeys[p.id] ?? false}
            onToggleKey={() => setShowKeys(s => ({ ...s, [p.id]: !s[p.id] }))}
            testResult={testResult[p.id]}
            testing={testing === p.id}
            onSave={saveBuiltin}
            onTest={testProvider}
            saveFailed={!!errors[`provider:${p.id}`]}
            onDiscard={discardBuiltinError}
            onReadinessChange={onBuiltinReadinessChange}
          />
        ))}
      </div>

      {/* Ollama local models */}
      <OllamaPanel providers={providers} onChanged={load} onBusyChange={setOllamaBusy} onReadinessChange={setOllamaReady} />

      {/* Custom providers */}
      <div className="space-y-3">
        <div className="flex items-center justify-between">
          <h3 className="text-xs font-semibold uppercase tracking-wide text-gray-500">{t('llm.custom')}</h3>
          <button
            onClick={() => { setForm({ ...EMPTY_CUSTOM }); setEditingCustom(null); setShowCustomForm(true) }}
            disabled={showCustomForm}
            className="flex items-center gap-1 text-xs text-blue-600 hover:text-blue-500 disabled:opacity-50"
          >
            <Plus className="h-3.5 w-3.5" /> {t('llm.add')}
          </button>
        </div>

        {customs.map(p => (
          <div key={p.id} className="rounded-lg border border-gray-200 bg-white p-4 dark:border-gray-700 dark:bg-gray-900">
            <div className="flex items-center justify-between">
              <div>
                <p className="text-sm font-medium text-gray-900 dark:text-gray-100">{p.name}</p>
                <p className="text-xs font-mono text-gray-400">{p.model} · {p.base_url}</p>
              </div>
              <div className="flex items-center gap-2">
                <button
                  onClick={() => testProvider(p)}
                  disabled={testing === p.id}
                  className="text-xs text-gray-500 hover:text-blue-600"
                >
                  {testing === p.id ? <Loader2 className="h-3.5 w-3.5 animate-spin" /> : t('llm.test')}
                </button>
                <button
                  onClick={() => { setForm({ ...p }); setEditingCustom(p); setShowCustomForm(true) }}
                  disabled={showCustomForm}
                  className="text-xs text-gray-500 hover:text-gray-700 disabled:opacity-50"
                >{t('llm.edit')}</button>
                <button onClick={() => deleteCustom(p.id)} disabled={showCustomForm && (editingCustom?.id ?? form.id) === p.id}
                  aria-label={`${t('common.delete')} ${p.name}`} className="text-gray-400 hover:text-red-500 disabled:opacity-50">
                  <Trash2 className="h-3.5 w-3.5" />
                </button>
                <EnableToggle
                  name={p.name}
                  enabled={p.enabled}
                  onChange={v => saveBuiltin({ ...p, enabled: v })}
                />
              </div>
            </div>
            {testResult[p.id] && <TestBadge result={testResult[p.id]} />}
          </div>
        ))}

        {customs.length === 0 && !showCustomForm && (
          <p className="text-xs text-gray-400 text-center py-4">{t('llm.noModels')}</p>
        )}
      </div>

      {/* Custom form */}
      {showCustomForm && (
        <div className="rounded-lg border border-blue-200 bg-white p-5 dark:border-blue-800 dark:bg-gray-900 space-y-3">
          <h4 className="text-sm font-semibold text-gray-900 dark:text-gray-100">
            {editingCustom ? t('llm.editProvider', { name: editingCustom.name }) : t('llm.addCustomModel')}
          </h4>
          <div className="grid grid-cols-2 gap-3">
            <Field label={t('llm.displayName')}>
              {id => <input id={id} value={form.name} onChange={e => setForm(f => ({ ...f, name: e.target.value }))}
                placeholder={t('llm.displayNamePlaceholder')} className="field-input" />}
            </Field>
            <Field label={t('llm.modelId')}>
              {id => <input id={id} value={form.model} onChange={e => setForm(f => ({ ...f, model: e.target.value }))}
                placeholder={t('llm.modelIdPlaceholder')} className="field-input font-mono" />}
            </Field>
          </div>
          <Field label={t('llm.baseUrl')}>
            {id => <input id={id} value={form.base_url} onChange={e => setForm(f => ({ ...f, base_url: e.target.value }))}
              placeholder="https://api.deepseek.com" className="field-input font-mono" />}
          </Field>
          <Field label={t('llm.apiKey')}>
            {id => <input id={id} type="password" value={form.api_key} onChange={e => setForm(f => ({ ...f, api_key: e.target.value }))}
              placeholder="sk-..." className="field-input font-mono" />}
          </Field>
          <div className="grid grid-cols-2 gap-3">
            <Field label={t('llm.contextWindow')}>
              {id => <input id={id} type="number" min={1024} step={1024} value={form.context_window ?? ''}
                onChange={e => setForm(f => ({ ...f, context_window: e.target.value ? Number(e.target.value) : undefined }))}
                placeholder="128000" className="field-input font-mono" />}
            </Field>
            <Field label={t('llm.maxOutputTokens')}>
              {id => <>
                <input id={id} type="number" min={256} step={256} value={form.max_output_tokens ?? ''}
                  onChange={e => setForm(f => ({ ...f, max_output_tokens: e.target.value ? Number(e.target.value) : undefined }))}
                  placeholder="8192" className="field-input font-mono" />
                <p className="mt-1 text-[11px] leading-4 text-gray-500 dark:text-gray-400">{t('llm.maxOutputTokensHint')}</p>
              </>}
            </Field>
          </div>
          <div className="flex gap-2">
            <button onClick={saveCustom} disabled={saving}
              className="flex items-center gap-1.5 rounded-lg bg-blue-600 px-3 py-1.5 text-xs font-medium text-white hover:bg-blue-500 disabled:opacity-60">
              {saving && <Loader2 className="h-3 w-3 animate-spin" />} {t('llm.save')}
            </button>
            <button onClick={() => { testProvider({ ...form, id: editingCustom?.id ?? 'temp' }) }}
              disabled={testing !== null}
              className="rounded-lg border border-gray-200 px-3 py-1.5 text-xs text-gray-600 hover:bg-gray-50 dark:border-gray-700 dark:text-gray-400">
              {t('llm.testConnection')}
            </button>
            <button onClick={() => { setShowCustomForm(false); setEditingCustom(null); setForm({ ...EMPTY_CUSTOM }); setError('custom', '') }}
              className="rounded-lg px-3 py-1.5 text-xs text-gray-500 hover:bg-gray-100">{t('llm.cancel')}</button>
          </div>
        </div>
      )}
    </fieldset>
  )
}

function BuiltinCard({ provider, showKey, onToggleKey, testResult, testing, onSave, onTest, saveFailed, onDiscard, onReadinessChange }: {
  provider: LlmProvider
  showKey: boolean
  onToggleKey: () => void
  testResult?: { ok: boolean; msg: string }
  testing: boolean
  onSave: (p: LlmProvider) => Promise<boolean>
  onTest: (p: LlmProvider) => void
  saveFailed: boolean
  onDiscard: (id: string) => void
  onReadinessChange: (id: string, ready: boolean) => void
}) {
  const { t } = useTranslation()
  // Only this card's successful save or explicit cancel drops its draft.
  // Reloading another provider must not replace these inputs.
  const [draft, setDraft] = useState<Pick<LlmProvider, 'api_key' | 'model'> | null>(null)
  const isSponsor = provider.id === SPONSOR_PROVIDER_ID
  const key = draft?.api_key ?? provider.api_key
  const model = draft?.model ?? provider.model
  const dirty = key !== provider.api_key || model !== provider.model

  useEffect(() => {
    onReadinessChange(provider.id, !dirty)
  }, [provider.id, dirty, onReadinessChange])

  async function save() {
    if (await onSave({ ...provider, api_key: key, model, enabled: !!key })) setDraft(null)
  }

  return (
    <div className="rounded-lg border border-gray-200 bg-white p-4 dark:border-gray-700 dark:bg-gray-900 space-y-3">
      <div className="flex items-center justify-between">
        <div className="flex items-center gap-2">
          {isSponsor && <img src="/88api.png" alt="88API" className="h-8 w-8 rounded-lg object-cover" />}
          <span className="text-sm font-semibold text-gray-900 dark:text-gray-100">{provider.name}</span>
          {provider.enabled && provider.api_key && model && (
            <span className="rounded-full bg-green-100 px-2 py-0.5 text-xs text-green-700 dark:bg-green-900/30 dark:text-green-400">{t('llm.active')}</span>
          )}
        </div>
        <EnableToggle name={provider.name} enabled={provider.enabled} onChange={v => onSave({ ...provider, enabled: v })} />
      </div>
      {isSponsor && (
        <div className="flex flex-wrap items-center justify-between gap-2 rounded-md bg-violet-50 px-3 py-2 text-xs text-violet-800 dark:bg-violet-950/40 dark:text-violet-200">
          <div>
            <p>{t('llm.sponsorHint')}</p>
            <p className="mt-0.5 font-mono text-[11px] text-violet-600 dark:text-violet-300">https://api.88api.ai/v1</p>
          </div>
          <a href={SPONSOR_CREDITS_URL} target="_blank" rel="noreferrer" className="inline-flex shrink-0 items-center gap-1 font-medium text-violet-700 underline underline-offset-2 hover:text-violet-900 dark:text-violet-300 dark:hover:text-violet-100">
            {t('llm.sponsorCredits')} <ExternalLink className="h-3 w-3" />
          </a>
        </div>
      )}

      <div className="grid grid-cols-2 gap-3">
        <Field label={t('llm.model')}>
          {id => <input id={id} value={model} onChange={e => setDraft({ api_key: key, model: e.target.value })}
            className="field-input font-mono text-xs" placeholder={isSponsor ? t('llm.sponsorModelPlaceholder') : provider.model} />}
        </Field>
        <Field label={t('llm.apiKey')}>
          {id => <div className="relative">
            <input
              id={id}
              type={showKey ? 'text' : 'password'}
              value={key}
              onChange={e => setDraft({ api_key: e.target.value, model })}
              placeholder="sk-..."
              className="field-input font-mono text-xs pr-8"
            />
            <button onClick={onToggleKey} aria-label={`${t(showKey ? 'agentForm.hide' : 'agentForm.show')} ${t('llm.apiKey')} · ${provider.name}`}
              className="absolute right-2 top-1/2 -translate-y-1/2 text-gray-400 hover:text-gray-600">
              {showKey ? <EyeOff className="h-3.5 w-3.5" /> : <Eye className="h-3.5 w-3.5" />}
            </button>
          </div>}
        </Field>
      </div>

      {testResult && <TestBadge result={testResult} />}

      <div className="flex gap-2">
        <button onClick={save}
          className="rounded-lg bg-gray-900 px-3 py-1.5 text-xs font-medium text-white hover:bg-gray-700 dark:bg-gray-100 dark:text-gray-900">
          {t('llm.save')}
        </button>
        <button onClick={() => onTest({ ...provider, api_key: key, model })} disabled={testing || !key || !model.trim()}
          className="flex items-center gap-1 rounded-lg border border-gray-200 px-3 py-1.5 text-xs text-gray-600 hover:bg-gray-50 disabled:opacity-40 dark:border-gray-700 dark:text-gray-400">
          {testing ? <Loader2 className="h-3 w-3 animate-spin" /> : null} {t('llm.test')}
        </button>
        {(dirty || saveFailed) && <button onClick={() => { setDraft(null); onDiscard(provider.id) }}
          className="rounded-lg px-3 py-1.5 text-xs text-gray-500 hover:bg-gray-100">{t('llm.cancel')}</button>}
      </div>
    </div>
  )
}

function OllamaPanel({ providers, onChanged, onBusyChange, onReadinessChange }: {
  providers: LlmProvider[]
  onChanged: () => Promise<void>
  onBusyChange: (busy: boolean) => void
  onReadinessChange: (ready: boolean) => void
}) {
  const { t } = useTranslation()
  const [savedBaseUrl, setSavedBaseUrl] = useState<string | null>(null)
  const [draftBaseUrl, setDraftBaseUrl] = useState<string | null>(null)
  const [loading, setLoading] = useState(true)
  const [loadError, setLoadError] = useState('')
  const [saveError, setSaveError] = useState('')
  const [addErrors, setAddErrors] = useState<Record<string, string>>({})
  const [saving, setSaving] = useState(false)
  const [testing, setTesting] = useState(false)
  const [adding, setAdding] = useState<string | null>(null)
  const [result, setResult] = useState<{ ok: boolean; msg: string } | null>(null)
  const [report, setReport] = useState<OllamaTestResult | null>(null)
  const baseUrl = draftBaseUrl ?? savedBaseUrl ?? 'http://localhost:11434'
  const dirty = draftBaseUrl !== null && draftBaseUrl !== savedBaseUrl
  const hasAddErrors = Object.values(addErrors).some(Boolean)
  const ready = savedBaseUrl !== null && !dirty && !loadError && !saveError && !hasAddErrors
  const busy = loading || saving || testing || adding !== null

  useEffect(() => {
    onBusyChange(busy)
    return () => onBusyChange(false)
  }, [busy, onBusyChange])

  useEffect(() => { onReadinessChange(ready) }, [ready, onReadinessChange])

  const loadConfig = useCallback(async () => {
    setLoading(true)
    setReport(null)
    setResult(null)
    try {
      const config = await invoke<OllamaConfig>('ollama_config_get')
      setSavedBaseUrl(config.base_url)
      setLoadError('')
      return true
    } catch (e) {
      setLoadError(String(e))
      return false
    } finally {
      setLoading(false)
    }
  }, [])

  useEffect(() => { void loadConfig() }, [loadConfig])

  const normalizedBase = baseUrl.trim().replace(/\/+$/, '')

  async function refresh() {
    const configLoaded = await loadConfig()
    try {
      await onChanged()
      setAddErrors({})
      if (configLoaded) setSaveError('')
    } catch (e) {
      setAddErrors(previous => ({ ...previous, refresh: String(e) }))
    }
  }

  async function saveConfig() {
    setSaving(true)
    try {
      await invoke('ollama_config_set', { config: { base_url: baseUrl.trim() } })
      if (await loadConfig()) {
        setDraftBaseUrl(null)
        setSaveError('')
      }
    } catch (e) {
      setSaveError(String(e))
    } finally {
      setSaving(false)
    }
  }

  async function testConnection() {
    setTesting(true)
    setResult(null)
    try {
      const res = await invoke<OllamaTestResult>('test_ollama_connection', { baseUrl: baseUrl.trim() })
      setReport(res)
      const version = res.version ? ` · v${res.version}` : ''
      setResult({ ok: true, msg: `${t('llm.ollamaConnected')}${version} · ${t('llm.ollamaModelCount', { count: res.models.length })}` })
    } catch (e) {
      setReport(null)
      setResult({ ok: false, msg: String(e) })
    } finally {
      setTesting(false)
    }
  }

  function matchesModel(provider: LlmProvider, model: OllamaModelInfo) {
    return provider.is_custom && provider.base_url.trim().replace(/\/+$/, '') === `${normalizedBase}/v1` && provider.model === model.name
  }

  function isAdded(model: OllamaModelInfo) {
    return providers.some(p => matchesModel(p, model))
  }

  async function addAsProvider(model: OllamaModelInfo) {
    const scope = JSON.stringify([normalizedBase, model.name])
    setAdding(model.name)
    try {
      // Check storage again on retries: a previous write may have succeeded
      // even if the subsequent reload failed. Keep existing (including old) ids.
      const current = await invoke<LlmProvider[]>('list_llm_providers')
      if (!current.some(p => matchesModel(p, model))) {
        // Ollama ignores Authorization; the key satisfies provider validation.
        const provider: LlmProvider = {
          id: `ollama_${crypto.randomUUID()}`,
          name: `Ollama · ${model.name}`,
          base_url: `${normalizedBase}/v1`,
          model: model.name,
          api_key: 'ollama',
          is_custom: true,
          enabled: true,
          context_window: 32768,
          max_output_tokens: 8192,
        }
        await invoke('save_llm_provider', { provider })
      }
      await onChanged()
      setAddErrors(previous => ({ ...previous, [scope]: '' }))
      setResult({ ok: true, msg: `${model.name} — ${t('llm.ollamaAddedHint')}` })
    } catch (e) {
      setAddErrors(previous => ({ ...previous, [scope]: String(e) }))
    } finally {
      setAdding(null)
    }
  }

  return (
    <div className="space-y-3">
      <h3 className="text-xs font-semibold uppercase tracking-wide text-gray-500">{t('llm.ollamaTitle')}</h3>
      <div className="rounded-lg border border-gray-200 bg-white p-4 dark:border-gray-700 dark:bg-gray-900 space-y-3">
        <div className="flex flex-wrap items-end gap-2">
          <Field label={t('llm.ollamaBaseUrl')} className="flex-1">
            {id => <input id={id} value={baseUrl} onChange={e => { setDraftBaseUrl(e.target.value); setReport(null); setResult(null) }}
              placeholder="http://localhost:11434" disabled={loading}
              className="field-input font-mono text-xs" />}
          </Field>
          <button onClick={saveConfig} disabled={saving || loading}
            className="flex items-center gap-1 rounded-lg border border-gray-200 px-3 py-1.5 text-xs text-gray-600 hover:bg-gray-50 disabled:opacity-50 dark:border-gray-700 dark:text-gray-400">
            {saving && <Loader2 className="h-3 w-3 animate-spin" />} {t('llm.save')}
          </button>
          <button onClick={testConnection} disabled={testing || loading}
            className="flex items-center gap-1.5 rounded-lg bg-emerald-600 px-3 py-1.5 text-xs font-medium text-white hover:bg-emerald-500 disabled:opacity-60">
            {testing && <Loader2 className="h-3 w-3 animate-spin" />} {t('llm.testConnection')}
          </button>
          {(dirty || saveError) && savedBaseUrl !== null && <button
            onClick={() => { setDraftBaseUrl(null); setSaveError(''); setReport(null); setResult(null) }}
            className="rounded-lg px-3 py-1.5 text-xs text-gray-500 hover:bg-gray-100">{t('llm.cancel')}</button>}
        </div>

        {(loadError || saveError || hasAddErrors) && <div role="alert" className="space-y-2">
          {loadError && <TestBadge result={{ ok: false, msg: loadError }} />}
          {saveError && <TestBadge result={{ ok: false, msg: saveError }} />}
          {Object.entries(addErrors).filter(([, message]) => message).map(([scope, message]) => <TestBadge key={scope} result={{ ok: false, msg: message }} />)}
          <button onClick={() => { void refresh() }} className="text-xs text-blue-600 underline">{t('common.refresh')}</button>
        </div>}
        {result && <TestBadge result={result} />}

        {report && report.models.length > 0 && (
          <ul className="divide-y divide-gray-100 dark:divide-gray-800">
            {report.models.map(m => (
              <li key={m.name} className="flex items-center justify-between gap-3 py-2">
                <div className="min-w-0">
                  <p className="truncate text-xs font-mono text-gray-800 dark:text-gray-200">{m.name}</p>
                  <p className="text-[11px] text-gray-400">
                    {[m.parameter_size, m.quantization_level, formatOllamaSize(m.size)].filter(Boolean).join(' · ')}
                  </p>
                </div>
                {isAdded(m) ? (
                  <span className="shrink-0 rounded-full bg-emerald-100 px-2 py-0.5 text-xs text-emerald-700 dark:bg-emerald-900/30 dark:text-emerald-400">{t('llm.ollamaAdded')}</span>
                ) : (
                  <button onClick={() => addAsProvider(m)} disabled={adding !== null}
                    className="shrink-0 rounded-lg border border-emerald-200 px-2.5 py-1 text-xs text-emerald-700 hover:bg-emerald-50 disabled:opacity-50 dark:border-emerald-800 dark:text-emerald-400 dark:hover:bg-emerald-900/20">
                    {adding === m.name ? <Loader2 className="h-3 w-3 animate-spin" /> : t('llm.ollamaAdd')}
                  </button>
                )}
              </li>
            ))}
          </ul>
        )}
        {report && report.models.length === 0 && (
          <p className="text-xs text-gray-400">{t('llm.ollamaNoModels')}</p>
        )}
      </div>
    </div>
  )
}

function formatOllamaSize(bytes: number): string {
  if (!bytes) return ''
  const gb = bytes / 1e9
  return gb >= 1 ? `${gb.toFixed(1)} GB` : `${Math.round(bytes / 1e6)} MB`
}

function TestBadge({ result }: { result: { ok: boolean; msg: string } }) {
  return (
    <div className={`flex items-center gap-1.5 rounded-lg px-3 py-2 text-xs ${
      result.ok ? 'bg-green-50 text-green-700 dark:bg-green-900/20 dark:text-green-400'
                : 'bg-red-50 text-red-600 dark:bg-red-900/20 dark:text-red-400'
    }`}>
      {result.ok ? <CheckCircle className="h-3.5 w-3.5 shrink-0" /> : <XCircle className="h-3.5 w-3.5 shrink-0" />}
      {result.msg}
    </div>
  )
}

function EnableToggle({ name, enabled, onChange }: { name: string; enabled: boolean; onChange: (v: boolean) => void }) {
  const { t } = useTranslation()
  return (
    <button
      role="switch"
      aria-checked={enabled}
      aria-label={`${name} · ${t('llm.active')}`}
      onClick={() => onChange(!enabled)}
      // overflow-hidden keeps the thumb clipped inside the rounded track
      // throughout the slide animation; left-0.5 pins the resting position
      // so translate-x-4 lands flush with the track's right edge.
      className={`relative h-5 w-9 shrink-0 overflow-hidden rounded-full transition-colors ${enabled ? 'bg-blue-600' : 'bg-gray-200 dark:bg-gray-700'}`}
    >
      <span className={`absolute left-0.5 top-0.5 h-4 w-4 rounded-full bg-white shadow transition-transform ${enabled ? 'translate-x-4' : 'translate-x-0'}`} />
    </button>
  )
}

function Field({ label, children, className }: { label: string; children: (id: string) => React.ReactNode; className?: string }) {
  const id = useId()
  return (
    <div className={className}>
      <label htmlFor={id} className="mb-1 block text-xs font-medium text-gray-500 dark:text-gray-400">{label}</label>
      {children(id)}
    </div>
  )
}
