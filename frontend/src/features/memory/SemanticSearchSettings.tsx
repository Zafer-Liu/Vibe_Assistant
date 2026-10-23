import { useCallback, useEffect, useId, useRef, useState } from 'react'
import { invoke } from '@tauri-apps/api/core'
import { listen } from '@tauri-apps/api/event'
import { useTranslation } from 'react-i18next'
import { Loader2, Info, Database, RefreshCw } from 'lucide-react'

type Runtime = {
  command: string
  args: string[]
  env: Record<string, string>
  [key: string]: unknown
}
type Config = {
  base_url: string
  model: string
  api_key?: string | null
  has_api_key?: boolean
  runtime?: Runtime | null
  [key: string]: unknown
}
type Mode = 'local' | 'remote'
type Draft = {
  enabled: boolean
  mode: Mode
  baseUrl: string
  model: string
  apiKey: string
  clearApiKey: boolean
  hasApiKey: boolean
  runtimeEnabled: boolean
  command: string
  args: string
  env: string
}
type TestResult = { dimension: number; model: string; latency_ms: number }
type Field = 'baseUrl' | 'model' | 'apiKey' | 'command' | 'args' | 'env'
type Errors = Partial<Record<Field, string>>

const LOCAL_DEFAULT_URL = 'http://127.0.0.1:7997/v1'
const LOCAL_DEFAULT_MODEL = 'bge-m3'

function isLoopbackUrl(raw: string): boolean {
  try {
    const url = new URL(raw)
    const host = url.hostname.toLowerCase()
    return host === 'localhost' || host === '[::1]' || /^127\.\d{1,3}\.\d{1,3}\.\d{1,3}$/.test(host)
  } catch {
    return false
  }
}

function toDraft(config: Config | null): Draft {
  const baseUrl = config?.base_url ?? LOCAL_DEFAULT_URL
  const loopback = isLoopbackUrl(baseUrl)
  return {
    enabled: config !== null,
    mode: config === null ? 'local' : (loopback ? 'local' : 'remote'),
    baseUrl,
    model: config?.model ?? LOCAL_DEFAULT_MODEL,
    apiKey: '',
    clearApiKey: false,
    hasApiKey: config?.has_api_key === true,
    runtimeEnabled: config?.runtime != null,
    command: config?.runtime?.command ?? '',
    args: JSON.stringify(config?.runtime?.args ?? [], null, 2),
    env: JSON.stringify(config?.runtime?.env ?? {}, null, 2),
  }
}

const inputClass = 'w-full min-w-0 rounded-md border border-[var(--app-control-stroke)] bg-[var(--app-surface)] px-3 py-2 text-sm text-[var(--app-text)] focus-visible:outline-2 focus-visible:outline-[var(--app-focus)] disabled:opacity-50'
const secondaryClass = 'rounded-md border border-[var(--app-stroke)] px-3 py-1.5 text-xs font-medium text-[var(--app-secondary)] hover:bg-[var(--app-hover)] focus-visible:outline-2 focus-visible:outline-[var(--app-focus)] disabled:opacity-50'
const labelClass = 'block text-xs font-medium text-[var(--app-secondary)]'
const hintClass = 'text-xs leading-5 text-[var(--app-secondary)]'

export function SemanticSearchSettings() {
  const { t } = useTranslation()
  const id = useId()
  const [draft, setDraft] = useState<Draft | null>(null)
  const [baseline, setBaseline] = useState<Draft | null>(null)
  const [stored, setStored] = useState<Config | null>(null)
  const [loading, setLoading] = useState(true)
  const [loadFailed, setLoadFailed] = useState(false)
  const [operation, setOperation] = useState<'test' | 'save' | null>(null)
  const [feedback, setFeedback] = useState<'saved' | 'saveFailed' | 'testFailed' | null>(null)
  const [result, setResult] = useState<TestResult | null>(null)
  const [errors, setErrors] = useState<Errors>({})
  const advanced = useRef<HTMLDetailsElement>(null)
  const alive = useRef(false)
  const busy = useRef(false)
  const revision = useRef(0)
  const loadSequence = useRef(0)

  const load = useCallback(async () => {
    const sequence = ++loadSequence.current
    setLoading(true)
    setLoadFailed(false)
    try {
      const config = await invoke<Config | null>('memory_semantic_config_get')
      if (!alive.current || sequence !== loadSequence.current) return
      const next = toDraft(config)
      setStored(config)
      setDraft(next)
      setBaseline(next)
    } catch {
      if (alive.current && sequence === loadSequence.current) setLoadFailed(true)
    } finally {
      if (alive.current && sequence === loadSequence.current) setLoading(false)
    }
  }, [])

  useEffect(() => {
    alive.current = true
    void load()
    return () => {
      alive.current = false
      loadSequence.current += 1
      revision.current += 1
    }
  }, [load])

  function edit(patch: Partial<Draft>) {
    revision.current += 1
    setDraft(current => current ? { ...current, ...patch } : current)
    setResult(null)
    setFeedback(null)
    setErrors({})
  }

  function switchMode(mode: Mode) {
    if (!draft || draft.mode === mode) return
    // Switching mode never carries secrets across; the key field is always
    // write-only and starts blank on every edit session.
    edit({ mode, apiKey: '', clearApiKey: false })
  }

  function validate(): Config | null | undefined {
    if (!draft) return undefined
    if (!draft.enabled) return null
    const invalid: Errors = {}
    const trimmedUrl = draft.baseUrl.trim()
    let parsed: URL | null = null
    try {
      parsed = new URL(trimmedUrl)
    } catch {
      invalid.baseUrl = 'urlInvalid'
    }
    if (parsed) {
      const loopback = isLoopbackUrl(trimmedUrl)
      const credentials = parsed.username || parsed.password
      const extras = parsed.search || parsed.hash
      if (credentials || extras) {
        invalid.baseUrl = 'urlInvalid'
      } else if (draft.mode === 'local') {
        if (!loopback || !['http:', 'https:'].includes(parsed.protocol)) invalid.baseUrl = 'urlInvalidLocal'
      } else {
        // Remote: HTTPS for any non-loopback host; plain HTTP stays loopback-only.
        if (parsed.protocol === 'https:') {
          // allowed
        } else if (parsed.protocol === 'http:' && loopback) {
          // allowed (a remote-mode draft may still point at a loopback HTTP service)
        } else {
          invalid.baseUrl = 'urlInvalidRemote'
        }
      }
    }
    if (!draft.model.trim()) invalid.model = 'modelRequired'
    let runtime: Runtime | undefined
    if (draft.mode === 'local' && draft.runtimeEnabled) {
      if (!draft.command.trim()) invalid.command = 'commandRequired'
      let args: string[] = []
      let env: Record<string, string> = {}
      try {
        const parsedArgs: unknown = JSON.parse(draft.args)
        if (!Array.isArray(parsedArgs) || !parsedArgs.every(item => typeof item === 'string')) throw new Error()
        args = parsedArgs
      } catch {
        invalid.args = 'argsInvalid'
      }
      try {
        const parsedEnv: unknown = JSON.parse(draft.env)
        if (parsedEnv === null || typeof parsedEnv !== 'object' || Array.isArray(parsedEnv) || !Object.values(parsedEnv).every(value => typeof value === 'string')) throw new Error()
        env = parsedEnv as Record<string, string>
      } catch {
        invalid.env = 'envInvalid'
      }
      runtime = { ...stored?.runtime, command: draft.command.trim(), args, env }
    }
    setErrors(invalid)
    if ((invalid.command || invalid.args || invalid.env) && advanced.current) advanced.current.open = true
    if (Object.keys(invalid).length) return undefined
    const config: Config = { ...stored, base_url: trimmedUrl, model: draft.model.trim() }
    // API key semantics: blank input keeps the stored key; the explicit clear
    // checkbox removes it; a non-empty input replaces it. The backend never
    // echoes the stored key back, so `has_api_key` is the only read signal.
    if (draft.clearApiKey) {
      config.api_key = null
    } else if (draft.apiKey.trim()) {
      config.api_key = draft.apiKey.trim()
    } else {
      delete config.api_key
    }
    delete config.has_api_key
    if (runtime) config.runtime = runtime
    else delete config.runtime
    return config
  }

  async function run(kind: 'test' | 'save') {
    if (busy.current || !draft) return
    const config = validate()
    if (config === undefined || (kind === 'test' && config === null)) return
    busy.current = true
    const version = revision.current
    setOperation(kind)
    setFeedback(null)
    setResult(null)
    try {
      if (kind === 'test') {
        // The native side decides from the URL whether to spin up a runtime;
        // remote HTTPS configs are tested with a plain HTTP request only.
        const tested = await invoke<TestResult>('memory_semantic_test', { config })
        if (alive.current && version === revision.current) setResult(tested)
      } else {
        const saved = await invoke<Config | null>('memory_semantic_config_set', { config })
        if (alive.current && version === revision.current) {
          const next = toDraft(saved)
          setStored(saved)
          setDraft(next)
          setBaseline(next)
          setFeedback('saved')
        }
      }
    } catch {
      // Native errors may echo runtime environment values; never render or log them.
      if (alive.current && version === revision.current) setFeedback(kind === 'test' ? 'testFailed' : 'saveFailed')
    } finally {
      busy.current = false
      if (alive.current) setOperation(null)
    }
  }

  if (loading) return <p role="status" className="flex items-center gap-2 text-xs text-[var(--app-secondary)]"><Loader2 size={14} className="animate-spin" aria-hidden="true" />{t('common.loading')}</p>
  if (loadFailed || !draft || !baseline) return <div role="alert" className="space-y-2 text-xs text-red-600 dark:text-red-400"><p>{t('settings.semanticSearch.loadFailed')}</p><button type="button" onClick={() => { void load() }} className={secondaryClass}>{t('settings.semanticSearch.retry')}</button></div>

  const dirty = draft.enabled !== baseline.enabled
    || (draft.enabled && (
      draft.mode !== baseline.mode
      || draft.baseUrl !== baseline.baseUrl
      || draft.model !== baseline.model
      || draft.apiKey.trim() !== ''
      || draft.clearApiKey
      || draft.runtimeEnabled !== baseline.runtimeEnabled
      || (draft.runtimeEnabled && (draft.command !== baseline.command || draft.args !== baseline.args || draft.env !== baseline.env))
    ))
  const saving = operation === 'save'
  function fieldError(field: Field) {
    return errors[field] ? <p id={`${id}-${field}-error`} className="mt-1 text-xs text-red-600 dark:text-red-400">{t(`settings.semanticSearch.${errors[field]}`)}</p> : null
  }
  function describedBy(field: Field, hint?: string) {
    return [hint, errors[field] ? `${id}-${field}-error` : null].filter(Boolean).join(' ') || undefined
  }

  return <div className="space-y-4">
    {/* Purpose banner: one calm strip, not a boxed card. */}
    <div className="flex items-start gap-2 rounded-md bg-[var(--app-accent-soft)] px-3 py-2 text-xs leading-5 text-[var(--app-accent)]">
      <Info size={14} className="mt-0.5 shrink-0" aria-hidden="true" />
      <p>{t('settings.semanticSearch.purpose')}</p>
    </div>

    <fieldset disabled={saving} className="min-w-0 space-y-4">
      {/* Enable row */}
      <div className="flex items-start justify-between gap-4">
        <div>
          <p id={`${id}-enable`} className="text-sm font-medium text-[var(--app-text)]">{t('settings.semanticSearch.enable')}</p>
          <p id={`${id}-enableHint`} className="mt-0.5 text-xs leading-5 text-[var(--app-secondary)]">{t('settings.semanticSearch.enableHint')}</p>
        </div>
        <button type="button" role="switch" aria-checked={draft.enabled} aria-labelledby={`${id}-enable`} aria-describedby={`${id}-enableHint`} onClick={() => edit({ enabled: !draft.enabled })} className={`relative mt-0.5 h-6 w-11 shrink-0 rounded-full focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-[var(--app-focus)] disabled:opacity-50 ${draft.enabled ? 'bg-[var(--app-accent)]' : 'bg-[var(--app-control-stroke)]'}`}>
          <span className={`absolute top-0.5 h-5 w-5 rounded-full bg-white shadow-sm ${draft.enabled ? 'left-5.5' : 'left-0.5'}`} />
        </button>
      </div>

      {draft.enabled && <>
        {/* Mode segmented control: flat, no nested cards. */}
        <div role="tablist" aria-label={t('settings.semanticSearch.modeLabel')} className="inline-flex rounded-md bg-[var(--app-layer)] p-0.5">
          {(['local', 'remote'] as const).map(mode => {
            const active = draft.mode === mode
            return <button
              key={mode}
              type="button"
              role="tab"
              aria-selected={active}
              onClick={() => switchMode(mode)}
              className={`rounded px-3 py-1.5 text-xs font-medium focus-visible:outline-2 focus-visible:outline-[var(--app-focus)] ${active ? 'bg-[var(--app-surface)] text-[var(--app-text)] shadow-sm' : 'text-[var(--app-secondary)] hover:text-[var(--app-text)]'}`}
            >
              {t(`settings.semanticSearch.mode.${mode}`)}
            </button>
          })}
        </div>
        <div className="rounded-md bg-[var(--app-layer)] px-3 py-2">
          <p className={hintClass}>{t(`settings.semanticSearch.mode.${draft.mode}Hint`)}</p>
        </div>

        {/* Unified field group: same grid for both modes, no extra borders. */}
        <div className="grid gap-3 sm:grid-cols-2">
          <div className="min-w-0 space-y-1.5">
            <label htmlFor={`${id}-baseUrl`} className={labelClass}>{draft.mode === 'local' ? t('settings.semanticSearch.baseUrl') : t('settings.semanticSearch.baseUrlRemote')}</label>
            <input
              id={`${id}-baseUrl`}
              type="url"
              value={draft.baseUrl}
              onChange={event => edit({ baseUrl: event.target.value })}
              placeholder={draft.mode === 'local' ? LOCAL_DEFAULT_URL : 'https://embeddings.example.com/v1'}
              spellCheck={false}
              autoComplete="off"
              aria-invalid={Boolean(errors.baseUrl)}
              aria-describedby={describedBy('baseUrl', `${id}-urlHint`)}
              className={inputClass}
            />
            {fieldError('baseUrl')}
          </div>
          <div className="min-w-0 space-y-1.5">
            <label htmlFor={`${id}-model`} className={labelClass}>{t('settings.semanticSearch.model')}</label>
            <input
              id={`${id}-model`}
              value={draft.model}
              onChange={event => edit({ model: event.target.value })}
              placeholder={LOCAL_DEFAULT_MODEL}
              spellCheck={false}
              autoComplete="off"
              aria-invalid={Boolean(errors.model)}
              aria-describedby={describedBy('model')}
              className={inputClass}
            />
            {fieldError('model')}
          </div>
          {draft.mode === 'remote' && (
            <div className="min-w-0 space-y-1.5 sm:col-span-2">
              <label htmlFor={`${id}-apiKey`} className={labelClass}>{t('settings.semanticSearch.apiKey')}</label>
              <div className="flex items-center gap-2">
                <input
                  id={`${id}-apiKey`}
                  type="password"
                  value={draft.apiKey}
                  onChange={event => edit({ apiKey: event.target.value, clearApiKey: false })}
                  placeholder={draft.hasApiKey ? t('settings.semanticSearch.apiKeySaved') : t('settings.semanticSearch.apiKeyPlaceholder')}
                  spellCheck={false}
                  autoComplete="new-password"
                  aria-invalid={Boolean(errors.apiKey)}
                  aria-describedby={describedBy('apiKey', `${id}-apiKeyHint`)}
                  className={inputClass}
                  disabled={draft.clearApiKey}
                />
                {draft.hasApiKey && (
                  <button
                    type="button"
                    onClick={() => edit({ clearApiKey: !draft.clearApiKey, apiKey: '' })}
                    className={secondaryClass}
                    aria-pressed={draft.clearApiKey}
                  >
                    {draft.clearApiKey ? t('settings.semanticSearch.apiKeyClearUndo') : t('settings.semanticSearch.apiKeyClear')}
                  </button>
                )}
              </div>
              {fieldError('apiKey')}
              <div className="rounded-md bg-[var(--app-layer)] px-3 py-2">
                <p id={`${id}-apiKeyHint`} className={hintClass}>
                  {draft.clearApiKey ? t('settings.semanticSearch.apiKeyClearHint') : t('settings.semanticSearch.apiKeyHint')}
                </p>
              </div>
            </div>
          )}
        </div>
        <div className="rounded-md bg-[var(--app-layer)] px-3 py-2">
          <p id={`${id}-urlHint`} className={hintClass}>{t(draft.mode === 'local' ? 'settings.semanticSearch.urlHint' : 'settings.semanticSearch.urlHintRemote')}</p>
        </div>

        {/* Local-only advanced runtime, folded away by default. */}
        {draft.mode === 'local' && (
          <details ref={advanced}>
            <summary className="w-fit cursor-pointer rounded-sm text-xs font-medium text-[var(--app-secondary)] focus-visible:outline-2 focus-visible:outline-[var(--app-focus)]">{t('settings.semanticSearch.advanced')}</summary>
            <div className="mt-3 space-y-3">
              <label className="flex items-center gap-2 text-xs text-[var(--app-text)]"><input type="checkbox" checked={draft.runtimeEnabled} onChange={event => edit({ runtimeEnabled: event.target.checked })} className="h-4 w-4 accent-[var(--app-accent)]" />{t('settings.semanticSearch.runtimeEnable')}</label>
              <p className={hintClass}>{t('settings.semanticSearch.runtimeHint')}</p>
              {draft.runtimeEnabled && <>
                <div className="space-y-1.5">
                  <label htmlFor={`${id}-command`} className={labelClass}>{t('settings.semanticSearch.command')}</label>
                  <input id={`${id}-command`} value={draft.command} onChange={event => edit({ command: event.target.value })} spellCheck={false} autoComplete="off" aria-invalid={Boolean(errors.command)} aria-describedby={describedBy('command')} className={inputClass} />
                  {fieldError('command')}
                </div>
                <div className="space-y-1.5">
                  <label htmlFor={`${id}-args`} className={labelClass}>{t('settings.semanticSearch.args')}</label>
                  <textarea id={`${id}-args`} rows={3} value={draft.args} onChange={event => edit({ args: event.target.value })} spellCheck={false} autoComplete="off" aria-invalid={Boolean(errors.args)} aria-describedby={describedBy('args')} className={`${inputClass} resize-y font-mono`} />
                  {fieldError('args')}
                </div>
                <div className="space-y-1.5">
                  <label htmlFor={`${id}-env`} className={labelClass}>{t('settings.semanticSearch.env')}</label>
                  <textarea id={`${id}-env`} rows={3} value={draft.env} onChange={event => edit({ env: event.target.value })} spellCheck={false} autoComplete="off" aria-invalid={Boolean(errors.env)} aria-describedby={describedBy('env', `${id}-envHint`)} className={`${inputClass} resize-y font-mono`} />
                  {fieldError('env')}
                  <p id={`${id}-envHint`} className={hintClass}>{t('settings.semanticSearch.envHint')}</p>
                </div>
              </>}
            </div>
          </details>
        )}
      </>}
    </fieldset>

    {/* Single action row + single status line. */}
    <div className="flex flex-wrap items-center gap-2">
      {draft.enabled && <button type="button" disabled={operation !== null} onClick={() => { void run('test') }} className={secondaryClass}>{operation === 'test' ? t('settings.semanticSearch.testing') : t('settings.semanticSearch.test')}</button>}
      <button type="button" disabled={operation !== null || !dirty} onClick={() => { void run('save') }} className="inline-flex items-center gap-1.5 rounded-md bg-[var(--app-accent)] px-3 py-1.5 text-xs font-medium text-white hover:opacity-90 focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-[var(--app-focus)] disabled:opacity-50">{saving && <Loader2 size={14} className="animate-spin" aria-hidden="true" />}{saving ? t('settings.semanticSearch.saving') : t('settings.semanticSearch.save')}</button>
      <span role="status" className="text-xs text-[var(--app-secondary)]">{dirty ? t('settings.semanticSearch.unsaved') : feedback === 'saved' ? t('settings.semanticSearch.saved') : ''}</span>
    </div>
    <div aria-live="polite" className="space-y-1">
      {operation === 'test' && <p role="status" className="flex items-center gap-2 text-xs text-[var(--app-secondary)]"><Loader2 size={14} className="animate-spin" aria-hidden="true" />{t('settings.semanticSearch.testing')}</p>}
      {result && <p role="status" className="break-words text-xs leading-5 text-green-700 dark:text-green-400">{t('settings.semanticSearch.testSuccess', { model: result.model, dimension: result.dimension, latency: result.latency_ms })}</p>}
      {(feedback === 'testFailed' || feedback === 'saveFailed') && <p role="alert" className="text-xs leading-5 text-red-600 dark:text-red-400">{t(`settings.semanticSearch.${feedback}`)}</p>}
      {Object.keys(errors).length > 0 && <p role="alert" className="text-xs text-red-600 dark:text-red-400">{t('settings.semanticSearch.fixErrors')}</p>}
      {draft.enabled && !result && operation !== 'test' && feedback !== 'testFailed' && (
        <div className="rounded-md bg-[var(--app-layer)] px-3 py-2">
          <p className={hintClass}>{t(draft.mode === 'local' ? 'settings.semanticSearch.testHint' : 'settings.semanticSearch.testHintRemote')}</p>
        </div>
      )}
    </div>

    {/* Vector index: progress + manual build + scheduled auto-build. Only
        meaningful once a config is saved and semantic search is enabled. */}
    {draft.enabled && stored !== null && <EmbeddingIndexPanel />}
  </div>
}

type IndexStatus = { total: number; cached: number; pending: number }
type AutoBuildConfig = { enabled: boolean; interval_hours: number }

/// 向量索引面板：构建进度 + 立即构建 + 定时自动构建。进度来自廉价的
/// `memory_semantic_index_status`（不 embedding、不起 runtime），构建期间每 2s
/// 轮询；构建结束由 `memory-embedding-index-updated` 事件通知其他窗口。
function EmbeddingIndexPanel() {
  const { t } = useTranslation()
  const id = useId()
  const [status, setStatus] = useState<IndexStatus | null>(null)
  const [building, setBuilding] = useState(false)
  const [autoConfig, setAutoConfig] = useState<AutoBuildConfig | null>(null)
  const [hoursDraft, setHoursDraft] = useState('')
  const [feedback, setFeedback] = useState<'done' | 'busy' | 'failed' | null>(null)
  const [embedded, setEmbedded] = useState(0)
  const alive = useRef(true)

  const refreshStatus = useCallback(async () => {
    try {
      const next = await invoke<IndexStatus>('memory_semantic_index_status')
      if (alive.current) setStatus(next)
    } catch {
      // 未配置或读取失败：保持现状，面板已在更上层按 stored 门控。
    }
  }, [])

  useEffect(() => {
    alive.current = true
    void refreshStatus()
    void invoke<AutoBuildConfig>('memory_embedding_auto_build_get')
      .then(config => {
        if (!alive.current) return
        setAutoConfig(config)
        setHoursDraft(String(config.interval_hours))
      })
      .catch(() => {})
    let unlisten: (() => void) | undefined
    void listen('memory-embedding-index-updated', () => { void refreshStatus() })
      .then(dispose => { unlisten = dispose })
      .catch(() => {})
    return () => {
      alive.current = false
      unlisten?.()
    }
  }, [refreshStatus])

  // 构建期间每 2s 轮询进度；settle 后停止。
  useEffect(() => {
    if (!building) return
    const timer = window.setInterval(() => { void refreshStatus() }, 2000)
    return () => window.clearInterval(timer)
  }, [building, refreshStatus])

  async function buildNow() {
    setBuilding(true)
    setFeedback(null)
    try {
      const result = await invoke<IndexStatus & { embedded?: number }>('memory_semantic_index_build')
      if (!alive.current) return
      setStatus({ total: result.total, cached: result.cached, pending: result.pending })
      setEmbedded(result.embedded ?? 0)
      setFeedback('done')
    } catch (error) {
      if (!alive.current) return
      const message = String(error)
      setFeedback(/另一个进程|another process|进行中/.test(message) ? 'busy' : 'failed')
    } finally {
      if (alive.current) setBuilding(false)
    }
  }

  async function saveAutoBuild(enabled: boolean, intervalHours: number) {
    try {
      const saved = await invoke<AutoBuildConfig>('memory_embedding_auto_build_set', { enabled, intervalHours })
      if (!alive.current) return
      setAutoConfig(saved)
      setHoursDraft(String(saved.interval_hours))
    } catch {
      // 保存失败：保持现有显示，下次进入页面重读。
    }
  }

  function commitHours() {
    const parsed = Number.parseInt(hoursDraft, 10)
    if (!autoConfig) return
    if (Number.isNaN(parsed) || parsed < 1 || parsed > 168) {
      setHoursDraft(String(autoConfig.interval_hours))
      return
    }
    if (parsed !== autoConfig.interval_hours) void saveAutoBuild(autoConfig.enabled, parsed)
    else setHoursDraft(String(autoConfig.interval_hours))
  }

  const total = status?.total ?? 0
  const cached = status?.cached ?? 0
  const pending = status?.pending ?? 0
  const percent = total > 0 ? Math.min(100, Math.round((cached / total) * 100)) : 100

  return (
    <div className="space-y-4 border-t border-[var(--app-stroke)] pt-4">
      <div className="flex items-start gap-2">
        <Database size={16} className="mt-0.5 shrink-0 text-[var(--app-accent)]" aria-hidden="true" />
        <div className="min-w-0">
          <p className="text-sm font-medium text-[var(--app-text)]">{t('settings.semanticSearch.indexTitle')}</p>
          <p className="mt-0.5 text-xs leading-5 text-[var(--app-secondary)]">{t('settings.semanticSearch.indexHint')}</p>
        </div>
      </div>

      {/* 进度条 + 状态行 + 立即构建。 */}
      <div className="space-y-2">
        <div className="flex items-baseline justify-between gap-3">
          <span className="font-mono text-xs tabular-nums text-[var(--app-secondary)]">
            {pending === 0 && status !== null
              ? t('settings.semanticSearch.indexUpToDate')
              : t('settings.semanticSearch.indexStatus', { cached, total, pending })}
          </span>
          <span className="font-mono text-xs tabular-nums text-[var(--app-secondary)]">{percent}%</span>
        </div>
        <div
          role="progressbar"
          aria-valuenow={cached}
          aria-valuemin={0}
          aria-valuemax={total}
          aria-label={t('settings.semanticSearch.indexTitle')}
          className="h-1.5 overflow-hidden rounded-full bg-[var(--app-layer)]"
        >
          <div className="h-full rounded-full bg-[var(--app-accent)] transition-all" style={{ width: `${percent}%` }} />
        </div>
        <div className="flex flex-wrap items-center gap-2">
          <button
            type="button"
            disabled={building || pending === 0}
            onClick={() => { void buildNow() }}
            className={secondaryClass}
          >
            {building
              ? <><Loader2 size={14} className="mr-1 inline animate-spin" aria-hidden="true" />{t('settings.semanticSearch.building')}</>
              : <><RefreshCw size={14} className="mr-1 inline" aria-hidden="true" />{t('settings.semanticSearch.buildNow')}</>}
          </button>
          <div aria-live="polite" className="text-xs leading-5">
            {feedback === 'done' && <span role="status" className="text-green-700 dark:text-green-400">{t('settings.semanticSearch.buildDone', { count: embedded })}</span>}
            {feedback === 'busy' && <span role="status" className="text-[var(--app-secondary)]">{t('settings.semanticSearch.buildBusy')}</span>}
            {feedback === 'failed' && <span role="alert" className="text-red-600 dark:text-red-400">{t('settings.semanticSearch.buildFailed')}</span>}
          </div>
        </div>
      </div>

      {/* 定时自动构建。 */}
      {autoConfig !== null && (
        <div className="space-y-3">
          <div className="flex items-start justify-between gap-4">
            <div>
              <p id={`${id}-autoBuild`} className="text-sm font-medium text-[var(--app-text)]">{t('settings.semanticSearch.autoBuild')}</p>
              <p id={`${id}-autoBuildHint`} className="mt-0.5 text-xs leading-5 text-[var(--app-secondary)]">{t('settings.semanticSearch.autoBuildHint')}</p>
            </div>
            <button
              type="button"
              role="switch"
              aria-checked={autoConfig.enabled}
              aria-labelledby={`${id}-autoBuild`}
              aria-describedby={`${id}-autoBuildHint`}
              onClick={() => { void saveAutoBuild(!autoConfig.enabled, autoConfig.interval_hours) }}
              className={`relative mt-0.5 h-6 w-11 shrink-0 rounded-full focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-[var(--app-focus)] disabled:opacity-50 ${autoConfig.enabled ? 'bg-[var(--app-accent)]' : 'bg-[var(--app-control-stroke)]'}`}
            >
              <span className={`absolute top-0.5 h-5 w-5 rounded-full bg-white shadow-sm ${autoConfig.enabled ? 'left-5.5' : 'left-0.5'}`} />
            </button>
          </div>
          {autoConfig.enabled && (
            <label className="block rounded-lg border border-[var(--app-stroke)] bg-[var(--app-layer)] p-3">
              <span className={labelClass}>{t('settings.semanticSearch.intervalLabel')}</span>
              <span className="mt-1.5 flex items-center gap-2">
                <input
                  type="number"
                  min={1}
                  max={168}
                  value={hoursDraft}
                  onChange={event => setHoursDraft(event.target.value)}
                  onBlur={commitHours}
                  onKeyDown={event => { if (event.key === 'Enter') { event.preventDefault(); commitHours() } }}
                  className="w-20 rounded-md border border-[var(--app-control-stroke)] bg-[var(--app-surface)] px-2 py-1 text-sm text-[var(--app-text)] focus-visible:outline-2 focus-visible:outline-[var(--app-focus)]"
                />
                <span className="text-xs text-[var(--app-secondary)]">{t('settings.semanticSearch.hoursUnit')}</span>
              </span>
              <span className="mt-1.5 block text-[11px] leading-4 text-[var(--app-secondary)]">{t('settings.semanticSearch.intervalHint')}</span>
            </label>
          )}
        </div>
      )}
    </div>
  )
}
