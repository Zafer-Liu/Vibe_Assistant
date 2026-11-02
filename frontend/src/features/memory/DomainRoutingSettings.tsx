import { useCallback, useEffect, useState } from 'react'
import { invoke } from '@tauri-apps/api/core'
import { useTranslation } from 'react-i18next'
import { Brain, CheckCircle2, Loader2, Pencil, Plus, RefreshCw, Trash2, X } from 'lucide-react'

type DecisionProvider = 'jev' | 'ollama'

type DomainConfig = {
  enabled: boolean
  provider: DecisionProvider
  endpoint: string
  model: string
  has_api_key: boolean
  key_source: string
  timeout_ms: number
  tag_threshold: number
  route_threshold: number
  second_threshold: number
}

type Domain = {
  id: string
  name: string
  kind: string
  description: string
  aliases: string[]
  enabled: boolean
  memory_count: number
}

type DomainDraft = {
  id: string | null
  name: string
  description: string
  aliases: string
  enabled: boolean
}

const EMPTY_DOMAIN: DomainDraft = { id: null, name: '', description: '', aliases: '', enabled: true }

export function DomainRoutingSettings() {
  const { t } = useTranslation()
  const [config, setConfig] = useState<DomainConfig | null>(null)
  const [domains, setDomains] = useState<Domain[]>([])
  const [models, setModels] = useState<string[]>([])
  const [apiKey, setApiKey] = useState('')
  const [busy, setBusy] = useState<'save' | 'test' | 'models' | 'domain' | null>(null)
  const [error, setError] = useState<string | null>(null)
  const [notice, setNotice] = useState<string | null>(null)
  const [domainDraft, setDomainDraft] = useState<DomainDraft | null>(null)

  const loadOllamaModels = useCallback(async (baseUrl: string) => {
    const result = await invoke<{ models: { name: string }[] }>('test_ollama_connection', { baseUrl })
    setModels(result.models.map((model) => model.name))
  }, [])

  useEffect(() => {
    let mounted = true
    void Promise.all([
      invoke<DomainConfig>('memory_domain_config_get'),
      invoke<Domain[]>('memory_domain_list'),
    ]).then(([nextConfig, nextDomains]) => {
      if (!mounted) return
      setConfig(nextConfig)
      setDomains(nextDomains)
      if (nextConfig.provider === 'ollama') {
        void loadOllamaModels(nextConfig.endpoint).catch(() => {})
      }
    }).catch((reason) => {
      if (mounted) setError(String(reason))
    })
    return () => { mounted = false }
  }, [loadOllamaModels])

  function updateConfig(patch: Partial<DomainConfig>) {
    setConfig((current) => current ? { ...current, ...patch } : current)
  }

  function changeProvider(provider: DecisionProvider) {
    if (!config || provider === config.provider) return
    setError(null)
    setNotice(null)
    if (provider === 'jev') {
      updateConfig({
        provider,
        endpoint: 'https://api.typesafe.ai/v1/systemone',
        model: 'jev-latest',
        timeout_ms: 8000,
      })
      return
    }
    updateConfig({ provider, endpoint: 'http://localhost:11434', model: '', timeout_ms: 30000 })
    void invoke<{ base_url: string }>('ollama_config_get').then(async (ollama) => {
      setConfig((current) => current?.provider === 'ollama'
        && current.endpoint === 'http://localhost:11434'
        ? { ...current, endpoint: ollama.base_url }
        : current)
      await loadOllamaModels(ollama.base_url)
    }).catch((reason) => setError(String(reason)))
  }

  async function saveConfig() {
    if (!config) return null
    const saved = await invoke<DomainConfig>('memory_domain_config_set', {
      update: {
        enabled: config.enabled,
        provider: config.provider,
        endpoint: config.endpoint,
        model: config.model,
        api_key: apiKey.trim() || null,
        clear_api_key: false,
        timeout_ms: config.timeout_ms,
        tag_threshold: config.tag_threshold,
        route_threshold: config.route_threshold,
        second_threshold: config.second_threshold,
      },
    })
    setConfig(saved)
    setApiKey('')
    return saved
  }

  async function run(action: 'save' | 'test') {
    if (busy) return
    setBusy(action)
    setError(null)
    setNotice(null)
    try {
      const saved = await saveConfig()
      if (!saved) return
      if (action === 'save') {
        setNotice(t('memory.domainSaved'))
      } else {
        const result = await invoke<{ model: string; probability: number }>('memory_domain_config_test')
        setNotice(t('memory.domainTestSucceeded', { model: result.model }))
      }
    } catch (reason) {
      setError(String(reason))
    } finally {
      setBusy(null)
    }
  }

  async function saveDomain() {
    if (!domainDraft || busy) return
    setBusy('domain')
    setError(null)
    setNotice(null)
    try {
      await invoke('memory_domain_upsert', {
        input: {
          id: domainDraft.id,
          name: domainDraft.name,
          description: domainDraft.description,
          aliases: domainDraft.aliases.split(/[,，]/).map((alias) => alias.trim()).filter(Boolean),
          enabled: domainDraft.enabled,
        },
      })
      setDomains(await invoke<Domain[]>('memory_domain_list'))
      setDomainDraft(null)
      setNotice(t('memory.domainTopicSaved'))
    } catch (reason) {
      setError(String(reason))
    } finally {
      setBusy(null)
    }
  }

  async function deleteDomain(domain: Domain) {
    if (busy || !window.confirm(t('memory.domainDeleteConfirm', { name: domain.name }))) return
    setBusy('domain')
    setError(null)
    setNotice(null)
    try {
      await invoke('memory_domain_delete', { id: domain.id })
      setDomains(await invoke<Domain[]>('memory_domain_list'))
      if (domainDraft?.id === domain.id) setDomainDraft(null)
      setNotice(t('memory.domainTopicDeleted', { name: domain.name }))
    } catch (reason) {
      setError(String(reason))
    } finally {
      setBusy(null)
    }
  }

  if (!config) {
    return error
      ? <p role="alert" className="rounded-md bg-red-500/10 px-3 py-2 text-xs text-red-700 dark:text-red-300">{error}</p>
      : <Loader2 size={15} className="animate-spin text-violet-600" />
  }
  const canDecide = config.provider === 'ollama'
    ? !!config.model.trim()
    : config.has_api_key || !!apiKey.trim()

  return (
    <section className="space-y-3 rounded-lg border border-gray-200 bg-white p-4 dark:border-gray-700 dark:bg-gray-900">
      <div className="flex items-center gap-2">
        <Brain size={15} className="text-violet-600 dark:text-violet-400" />
        <div className="min-w-0 flex-1">
          <h3 className="text-sm font-semibold text-gray-800 dark:text-gray-100">{t('memory.domainTitle')}</h3>
          <p className="mt-0.5 text-xs leading-5 text-gray-600 dark:text-gray-300">{t('memory.domainHint')}</p>
        </div>
        <span className={`text-xs ${config.enabled ? 'text-green-600 dark:text-green-400' : 'text-gray-500'}`}>
          {config.enabled ? t('memory.ingestOn') : t('memory.ingestOff')}
        </span>
      </div>
      {config.provider === 'ollama' && <p className="rounded-md bg-amber-500/10 px-2.5 py-2 text-xs leading-5 text-amber-800 dark:text-amber-200">{t('memory.domainPrivacyOllama')}</p>}
      <div className="grid gap-2 md:grid-cols-2">
        <label className="text-xs text-gray-500">{t('memory.domainProvider')}
          <select value={config.provider} onChange={(event) => changeProvider(event.target.value as DecisionProvider)} className="mt-1 w-full rounded-md border border-gray-300 bg-white px-2.5 py-1.5 text-sm text-gray-800 dark:border-gray-600 dark:bg-gray-800 dark:text-gray-100">
            <option value="jev">TypeSafe JEV · default</option>
            <option value="ollama">Ollama · local</option>
          </select>
        </label>
        <label className="text-xs text-gray-500">{t('memory.domainModel')}
          <input list={config.provider === 'ollama' ? 'domain-ollama-models' : undefined} value={config.model} onChange={(event) => updateConfig({ model: event.target.value })} placeholder={config.provider === 'ollama' ? t('memory.domainOllamaModelPlaceholder') : 'jev-latest'} className="mt-1 w-full rounded-md border border-gray-300 bg-white px-2.5 py-1.5 text-sm text-gray-800 dark:border-gray-600 dark:bg-gray-800 dark:text-gray-100" />
          {config.provider === 'ollama' && <datalist id="domain-ollama-models">{models.map((model) => <option key={model} value={model} />)}</datalist>}
        </label>
        <label className="text-xs text-gray-500">{t('memory.domainEndpoint')}
          <input value={config.endpoint} onChange={(event) => updateConfig({ endpoint: event.target.value })} className="mt-1 w-full rounded-md border border-gray-300 bg-white px-2.5 py-1.5 text-sm text-gray-800 dark:border-gray-600 dark:bg-gray-800 dark:text-gray-100" />
        </label>
        <label className="text-xs text-gray-500">{t('memory.domainTimeout')}
          <input type="number" min={250} max={120000} step={250} value={config.timeout_ms} onChange={(event) => updateConfig({ timeout_ms: Number(event.target.value) })} className="mt-1 w-full rounded-md border border-gray-300 bg-white px-2.5 py-1.5 text-sm text-gray-800 dark:border-gray-600 dark:bg-gray-800 dark:text-gray-100" />
        </label>
        {config.provider === 'jev' && <label className="text-xs text-gray-500 md:col-span-2">TypeSafe API Key
          <input type="password" value={apiKey} onChange={(event) => setApiKey(event.target.value)} placeholder={config.has_api_key ? t('memory.domainKeyConfigured') : 'TYPESAFE_API_KEY'} autoComplete="off" className="mt-1 w-full rounded-md border border-gray-300 bg-white px-2.5 py-1.5 text-sm text-gray-800 dark:border-gray-600 dark:bg-gray-800 dark:text-gray-100" />
        </label>}
      </div>
      <div className="flex flex-wrap items-center gap-2">
        <label className="mr-2 flex items-center gap-2 text-xs text-gray-700 dark:text-gray-200">
          <input type="checkbox" checked={config.enabled} onChange={(event) => updateConfig({ enabled: event.target.checked })} />
          {t('memory.domainEnable')}
        </label>
        {config.provider === 'ollama' && <button type="button" onClick={() => {
          setBusy('models')
          setError(null)
          void loadOllamaModels(config.endpoint).catch((reason) => setError(String(reason))).finally(() => setBusy(null))
        }} disabled={busy !== null} className="text-xs text-violet-600 hover:underline disabled:opacity-50 dark:text-violet-300">{busy === 'models' ? <Loader2 size={12} className="inline animate-spin" /> : null}{t('memory.domainLoadModels')}</button>}
      </div>
      <div className="flex flex-wrap items-center gap-2">
        <button type="button" onClick={() => { void run('save') }} disabled={busy !== null} className="inline-flex items-center gap-1.5 rounded-md bg-violet-600 px-2.5 py-1.5 text-xs font-medium text-white hover:bg-violet-500 disabled:opacity-50">
          {busy === 'save' ? <Loader2 size={13} className="animate-spin" /> : <CheckCircle2 size={13} />}{t('common.save')}
        </button>
        <button type="button" onClick={() => { void run('test') }} disabled={busy !== null || !config.enabled || !canDecide} className="rounded-md border border-gray-300 px-2.5 py-1.5 text-xs text-gray-700 hover:bg-gray-50 disabled:opacity-50 dark:border-gray-600 dark:text-gray-200 dark:hover:bg-gray-700">
          {busy === 'test' ? <Loader2 size={13} className="inline animate-spin" /> : null}{t('memory.domainTest')}
        </button>
      </div>
      {error && <p role="alert" className="whitespace-pre-wrap rounded-md bg-red-500/10 px-2.5 py-2 text-xs text-red-700 dark:text-red-300">{error}</p>}
      {notice && <p role="status" className="rounded-md bg-green-500/10 px-2.5 py-2 text-xs text-green-700 dark:text-green-300">{notice}</p>}

      <div className="border-t border-gray-100 pt-3 dark:border-gray-700">
        <div className="flex items-center justify-between">
          <h4 className="text-xs font-medium text-gray-700 dark:text-gray-200">{t('memory.domainTopics')}</h4>
          <button type="button" onClick={() => setDomainDraft({ ...EMPTY_DOMAIN })} disabled={busy !== null} className="inline-flex items-center gap-1 text-xs text-violet-600 hover:underline disabled:opacity-50 dark:text-violet-300"><Plus size={13} />{t('memory.domainAdd')}</button>
        </div>
        <div className="mt-2 divide-y divide-gray-100 dark:divide-gray-700">
          {domains.map((domain) => <div key={domain.id} className="flex items-center gap-2 py-2 text-xs">
            <div className="min-w-0 flex-1">
              <span className="text-gray-700 dark:text-gray-200">{domain.name}</span>
              <span className="ml-2 text-gray-500">{domain.memory_count}</span>
              {!domain.enabled && <span className="ml-2 text-gray-400">{t('memory.ingestOff')}</span>}
              {domain.description && <p className="mt-0.5 line-clamp-1 text-gray-500" title={domain.description}>{domain.description}</p>}
            </div>
            <button type="button" onClick={() => setDomainDraft({ id: domain.id, name: domain.name, description: domain.description, aliases: domain.aliases.join('，'), enabled: domain.enabled })} disabled={busy !== null} title={t('common.edit')} className="rounded p-1 text-gray-500 hover:bg-gray-100 dark:hover:bg-gray-700"><Pencil size={13} /></button>
            <button type="button" onClick={() => { void deleteDomain(domain) }} disabled={busy !== null} title={t('common.delete')} className="rounded p-1 text-gray-500 hover:bg-red-50 hover:text-red-600 dark:hover:bg-red-950/30"><Trash2 size={13} /></button>
          </div>)}
        </div>
        {domainDraft && <div className="mt-3 space-y-2 border-t border-gray-100 pt-3 dark:border-gray-700">
          <div className="grid gap-2 md:grid-cols-2">
            <label className="text-xs text-gray-500">{t('memory.domainName')}
              <input autoFocus value={domainDraft.name} onChange={(event) => setDomainDraft({ ...domainDraft, name: event.target.value })} className="mt-1 w-full rounded-md border border-gray-300 bg-white px-2.5 py-1.5 text-sm dark:border-gray-600 dark:bg-gray-800" />
            </label>
            <label className="text-xs text-gray-500">{t('memory.domainAliases')}
              <input value={domainDraft.aliases} onChange={(event) => setDomainDraft({ ...domainDraft, aliases: event.target.value })} placeholder={t('memory.domainAliasesPlaceholder')} className="mt-1 w-full rounded-md border border-gray-300 bg-white px-2.5 py-1.5 text-sm dark:border-gray-600 dark:bg-gray-800" />
            </label>
          </div>
          <label className="block text-xs text-gray-500">{t('memory.domainDescription')}
            <textarea rows={2} value={domainDraft.description} onChange={(event) => setDomainDraft({ ...domainDraft, description: event.target.value })} className="mt-1 w-full rounded-md border border-gray-300 bg-white px-2.5 py-1.5 text-sm dark:border-gray-600 dark:bg-gray-800" />
          </label>
          <div className="flex flex-wrap items-center gap-2">
            <label className="mr-2 flex items-center gap-2 text-xs text-gray-700 dark:text-gray-200"><input type="checkbox" checked={domainDraft.enabled} onChange={(event) => setDomainDraft({ ...domainDraft, enabled: event.target.checked })} />{t('memory.domainEnabled')}</label>
            <button type="button" onClick={() => { void saveDomain() }} disabled={busy !== null || !domainDraft.name.trim()} className="inline-flex items-center gap-1 rounded-md bg-violet-600 px-2.5 py-1.5 text-xs text-white disabled:opacity-50">{busy === 'domain' ? <Loader2 size={12} className="animate-spin" /> : <CheckCircle2 size={12} />}{t('common.save')}</button>
            <button type="button" onClick={() => setDomainDraft(null)} disabled={busy !== null} className="inline-flex items-center gap-1 rounded-md border border-gray-300 px-2.5 py-1.5 text-xs dark:border-gray-600"><X size={12} />{t('common.cancel')}</button>
          </div>
        </div>}
      </div>
    </section>
  )
}
