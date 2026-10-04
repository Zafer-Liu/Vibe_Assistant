import { memo, useEffect, useMemo, useState } from 'react'
import { invoke } from '@tauri-apps/api/core'
import { useTranslation } from 'react-i18next'
import {
  BadgeCheck, BookOpenText, Check, Download, ExternalLink, FileCode2,
  Loader2, PackageCheck, RefreshCw, Search, ShieldAlert,
} from 'lucide-react'
import { useMemoryStore } from '../store/memoryStore'
import type {
  MarketplaceCatalog, MarketplaceSkill, MarketplaceSkillPreview, SkillItem,
} from '../types/memory'

type SourceFilter = 'all' | MarketplaceSkill['source']

export const SkillMarketplace = memo(function SkillMarketplace() {
  const { t, i18n } = useTranslation()
  const { skills, loadSkills } = useMemoryStore()
  const [catalog, setCatalog] = useState<MarketplaceCatalog | null>(null)
  const [query, setQuery] = useState('')
  const [source, setSource] = useState<SourceFilter>('all')
  const [selectedId, setSelectedId] = useState<string | null>(null)
  const [preview, setPreview] = useState<MarketplaceSkillPreview | null>(null)
  const [loading, setLoading] = useState(true)
  const [previewing, setPreviewing] = useState(false)
  const [installing, setInstalling] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const [notice, setNotice] = useState<string | null>(null)

  const installedIds = useMemo(() => new Set(
    skills
      .filter((skill) => skill.source.startsWith('market-'))
      .map((skill) => `${skill.source.slice('market-'.length)}:${skill.name}`),
  ), [skills])

  const visibleItems = useMemo(() => {
    const normalized = query.trim().toLowerCase()
    return (catalog?.items ?? []).filter((item) => (
      (source === 'all' || item.source === source)
      && (!normalized || `${item.name} ${item.description} ${item.source_label}`.toLowerCase().includes(normalized))
    ))
  }, [catalog, query, source])

  async function loadCatalog(refresh = false) {
    setLoading(true)
    setError(null)
    try {
      const next = await invoke<MarketplaceCatalog>('skill_marketplace_list', { refresh })
      setCatalog(next)
      if (next.warning) setNotice(t('marketplace.cachedWarning'))
      const nextId = selectedId && next.items.some((item) => item.id === selectedId)
        ? selectedId
        : next.items[0]?.id ?? null
      setSelectedId(nextId)
      if (nextId) {
        const item = next.items.find((candidate) => candidate.id === nextId)
        if (item) void loadPreview(item)
      }
    } catch (cause) {
      setError(String(cause))
    } finally {
      setLoading(false)
    }
  }

  async function loadPreview(item: MarketplaceSkill) {
    setSelectedId(item.id)
    setPreviewing(true)
    setError(null)
    try {
      setPreview(await invoke<MarketplaceSkillPreview>('skill_marketplace_preview', {
        source: item.source,
        name: item.name,
      }))
    } catch (cause) {
      setPreview(null)
      setError(t('marketplace.previewFailed', { error: String(cause) }))
    } finally {
      setPreviewing(false)
    }
  }

  async function installSelected() {
    if (!preview) return
    setInstalling(true)
    setError(null)
    try {
      const installed = await invoke<SkillItem>('skill_marketplace_install', {
        source: preview.item.source,
        name: preview.item.name,
      })
      await loadSkills(true)
      setNotice(t(
        installedIds.has(preview.item.id) ? 'marketplace.updated' : 'marketplace.installed',
        { name: installed.name },
      ))
    } catch (cause) {
      setError(t('marketplace.installFailed', { error: String(cause) }))
    } finally {
      setInstalling(false)
    }
  }

  useEffect(() => {
    void loadSkills()
    void loadCatalog()
    // Catalog is intentionally loaded once; refresh is an explicit user action.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [])

  useEffect(() => {
    if (!notice) return
    const timer = setTimeout(() => setNotice(null), 5000)
    return () => clearTimeout(timer)
  }, [notice])

  const selectedItem = (catalog?.items ?? []).find((item) => item.id === selectedId) ?? null
  const isInstalled = selectedItem ? installedIds.has(selectedItem.id) : false
  const fetchedAt = catalog?.fetched_at
    ? new Intl.DateTimeFormat(i18n.language, { dateStyle: 'medium', timeStyle: 'short' }).format(new Date(catalog.fetched_at))
    : null

  return (
    <div className="flex h-full min-w-0 flex-col bg-gray-50 dark:bg-gray-950">
      <header className="border-b border-gray-200 bg-white px-6 py-4 dark:border-gray-800 dark:bg-gray-900">
        <div className="flex flex-wrap items-start justify-between gap-3">
          <div>
            <div className="mb-1 flex items-center gap-1.5 text-xs text-gray-400 dark:text-gray-500">
              <BookOpenText className="h-3.5 w-3.5" />
              <span>{t('nav.skills')}</span><span>/</span><span>{t('nav.skillsMarketplace')}</span>
            </div>
            <h1 className="text-xl font-semibold text-gray-900 dark:text-white">{t('marketplace.title')}</h1>
            <p className="mt-1 text-sm text-gray-500 dark:text-gray-400">{t('marketplace.subtitle')}</p>
          </div>
          <button
            type="button"
            onClick={() => { void loadCatalog(true) }}
            disabled={loading}
            className="inline-flex items-center gap-2 rounded-lg border border-gray-300 bg-white px-3 py-2 text-sm font-medium text-gray-700 transition hover:bg-gray-50 disabled:opacity-50 dark:border-gray-700 dark:bg-gray-900 dark:text-gray-200 dark:hover:bg-gray-800"
          >
            <RefreshCw className={`h-4 w-4 ${loading ? 'animate-spin' : ''}`} />
            {t('marketplace.refresh')}
          </button>
        </div>
      </header>

      {notice && (
        <div className="mx-6 mt-4 rounded-lg border border-emerald-200 bg-emerald-50 px-4 py-2 text-sm text-emerald-800 dark:border-emerald-900/70 dark:bg-emerald-950/40 dark:text-emerald-200">
          {notice}
        </div>
      )}
      {error && (
        <div className="mx-6 mt-4 flex items-start justify-between gap-4 rounded-lg border border-red-200 bg-red-50 px-4 py-3 text-sm text-red-800 dark:border-red-900/70 dark:bg-red-950/40 dark:text-red-200">
          <span className="break-all">{error}</span>
          <button type="button" onClick={() => { void loadCatalog(true) }} className="shrink-0 font-medium underline underline-offset-2">
            {t('common.reload')}
          </button>
        </div>
      )}

      <div className="flex min-h-0 flex-1 gap-4 p-6">
        <aside className="flex w-[360px] shrink-0 flex-col overflow-hidden rounded-xl border border-gray-200 bg-white dark:border-gray-800 dark:bg-gray-900">
          <div className="space-y-3 border-b border-gray-200 p-4 dark:border-gray-800">
            <label className="relative block">
              <Search className="pointer-events-none absolute left-3 top-2.5 h-4 w-4 text-gray-400" />
              <input
                value={query}
                onChange={(event) => setQuery(event.target.value)}
                placeholder={t('marketplace.search')}
                className="w-full rounded-lg border border-gray-300 bg-white py-2 pl-9 pr-3 text-sm text-gray-900 outline-none transition focus:border-blue-500 focus:ring-2 focus:ring-blue-500/20 dark:border-gray-700 dark:bg-gray-950 dark:text-white"
              />
            </label>
            <div className="flex gap-2" role="group" aria-label={t('marketplace.sourceFilter')}>
              {(['all', 'openai', 'anthropic'] as const).map((id) => (
                <button
                  type="button"
                  key={id}
                  onClick={() => setSource(id)}
                  className={`rounded-full px-3 py-1 text-xs font-medium transition ${source === id
                    ? 'bg-blue-600 text-white'
                    : 'bg-gray-100 text-gray-600 hover:bg-gray-200 dark:bg-gray-800 dark:text-gray-300 dark:hover:bg-gray-700'}`}
                >
                  {id === 'all' ? t('skills.all') : id === 'openai' ? 'OpenAI' : 'Anthropic'}
                </button>
              ))}
            </div>
            <div className="flex items-center justify-between text-xs text-gray-400 dark:text-gray-500">
              <span>{t('marketplace.count', { count: visibleItems.length })}</span>
              {fetchedAt && <span>{catalog?.from_cache ? t('marketplace.cached') : t('marketplace.updatedAt', { time: fetchedAt })}</span>}
            </div>
          </div>

          <div className="min-h-0 flex-1 overflow-y-auto p-2">
            {loading && !catalog ? (
              <div className="flex h-full items-center justify-center text-sm text-gray-400">
                <Loader2 className="mr-2 h-4 w-4 animate-spin" />{t('marketplace.loading')}
              </div>
            ) : visibleItems.length === 0 ? (
              <div className="px-4 py-12 text-center text-sm text-gray-400">{t('marketplace.empty')}</div>
            ) : visibleItems.map((item) => {
              const installed = installedIds.has(item.id)
              return (
                <button
                  type="button"
                  key={item.id}
                  onClick={() => { void loadPreview(item) }}
                  className={`mb-1 w-full rounded-lg px-3 py-3 text-left transition ${selectedId === item.id
                    ? 'bg-blue-50 ring-1 ring-inset ring-blue-200 dark:bg-blue-950/40 dark:ring-blue-900'
                    : 'hover:bg-gray-50 dark:hover:bg-gray-800/70'}`}
                >
                  <div className="flex items-center gap-2">
                    <span className="truncate text-sm font-semibold text-gray-900 dark:text-white">{item.name}</span>
                    {installed && <Check className="h-3.5 w-3.5 shrink-0 text-emerald-500" aria-label={t('marketplace.installedBadge')} />}
                  </div>
                  <p className="mt-1 line-clamp-2 text-xs leading-5 text-gray-500 dark:text-gray-400">
                    {item.description || t('skills.noDescription')}
                  </p>
                  <div className="mt-2 flex items-center gap-1 text-[11px] font-medium text-gray-400">
                    <BadgeCheck className="h-3.5 w-3.5 text-blue-500" />{item.source_label}
                    <span className="mx-1">·</span>{t('marketplace.fileCount', { count: item.files.length })}
                  </div>
                </button>
              )
            })}
          </div>
        </aside>

        <main className="min-w-0 flex-1 overflow-y-auto rounded-xl border border-gray-200 bg-white dark:border-gray-800 dark:bg-gray-900">
          {!selectedItem ? (
            <div className="flex h-full flex-col items-center justify-center px-8 text-center text-gray-400">
              <BookOpenText className="mb-3 h-9 w-9 opacity-50" />
              <p className="text-sm">{t('marketplace.selectHint')}</p>
            </div>
          ) : (
            <div>
              <div className="sticky top-0 z-10 border-b border-gray-200 bg-white/95 px-6 py-4 backdrop-blur dark:border-gray-800 dark:bg-gray-900/95">
                <div className="flex flex-wrap items-start justify-between gap-4">
                  <div className="min-w-0">
                    <div className="flex flex-wrap items-center gap-2">
                      <h2 className="text-lg font-semibold text-gray-900 dark:text-white">{selectedItem.name}</h2>
                      <span className="inline-flex items-center gap-1 rounded-full bg-blue-50 px-2 py-0.5 text-xs font-medium text-blue-700 dark:bg-blue-950/50 dark:text-blue-300">
                        <BadgeCheck className="h-3.5 w-3.5" />{selectedItem.source_label}
                      </span>
                      {isInstalled && (
                        <span className="inline-flex items-center gap-1 rounded-full bg-emerald-50 px-2 py-0.5 text-xs font-medium text-emerald-700 dark:bg-emerald-950/50 dark:text-emerald-300">
                          <PackageCheck className="h-3.5 w-3.5" />{t('marketplace.installedBadge')}
                        </span>
                      )}
                    </div>
                    <p className="mt-1 max-w-3xl text-sm leading-6 text-gray-500 dark:text-gray-400">
                      {selectedItem.description || t('skills.noDescription')}
                    </p>
                    <a href={selectedItem.skill_url} target="_blank" rel="noreferrer" className="mt-2 inline-flex items-center gap-1 text-xs font-medium text-blue-600 hover:underline dark:text-blue-400">
                      {t('marketplace.viewSource')}<ExternalLink className="h-3 w-3" />
                    </a>
                  </div>
                  <button
                    type="button"
                    onClick={() => { void installSelected() }}
                    disabled={!preview || previewing || installing}
                    className="inline-flex items-center gap-2 rounded-lg bg-blue-600 px-4 py-2 text-sm font-semibold text-white shadow-sm transition hover:bg-blue-700 disabled:cursor-not-allowed disabled:opacity-50"
                  >
                    {installing ? <Loader2 className="h-4 w-4 animate-spin" /> : <Download className="h-4 w-4" />}
                    {isInstalled ? t('marketplace.updateInstall') : t('marketplace.install')}
                  </button>
                </div>
              </div>

              <div className="space-y-5 p-6">
                <div className="flex gap-3 rounded-lg border border-amber-200 bg-amber-50 px-4 py-3 text-sm text-amber-900 dark:border-amber-900/60 dark:bg-amber-950/30 dark:text-amber-200">
                  <ShieldAlert className="mt-0.5 h-4 w-4 shrink-0" />
                  <p className="leading-6">{t('marketplace.securityNotice')}</p>
                </div>

                <section>
                  <div className="mb-2 flex items-center justify-between">
                    <h3 className="text-sm font-semibold text-gray-900 dark:text-white">SKILL.md</h3>
                    {previewing && <Loader2 className="h-4 w-4 animate-spin text-gray-400" />}
                  </div>
                  <pre className="max-h-[520px] overflow-auto whitespace-pre-wrap rounded-lg border border-gray-200 bg-gray-50 p-4 font-mono text-xs leading-6 text-gray-700 dark:border-gray-800 dark:bg-gray-950 dark:text-gray-200">
                    {preview?.item.id === selectedItem.id ? preview.content : t('marketplace.loadingPreview')}
                  </pre>
                </section>

                <section>
                  <h3 className="mb-2 flex items-center gap-2 text-sm font-semibold text-gray-900 dark:text-white">
                    <FileCode2 className="h-4 w-4" />{t('marketplace.packageFiles', { count: selectedItem.files.length })}
                  </h3>
                  <div className="flex flex-wrap gap-2">
                    {selectedItem.files.map((file) => (
                      <span key={file} className="rounded-md bg-gray-100 px-2 py-1 font-mono text-[11px] text-gray-600 dark:bg-gray-800 dark:text-gray-300">{file}</span>
                    ))}
                  </div>
                </section>
              </div>
            </div>
          )}
        </main>
      </div>
    </div>
  )
})
