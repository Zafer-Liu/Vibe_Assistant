import { memo, useCallback, useEffect, useMemo, useRef, useState } from 'react'
import { useTranslation } from 'react-i18next'
import { invoke } from '@tauri-apps/api/core'
import { ArrowLeft, Brain, Loader2, Pencil, RefreshCw, Star, Tags, Trash2, X } from 'lucide-react'
import { useMemoryFields } from '../store/memoryStore'
import type { MemoryImportance, MemoryItem } from '../types/memory'
import { MemoryBreadcrumb } from '../components/MemoryBreadcrumb'
import { MemoryMarkdown } from '../components/MemoryMarkdown'
import { Modal } from '../components/Modal'
import { intlLocale } from '../i18n'

function localTime(value: string) {
  const normalized = value.trim().replace(/(\.\d{3})\d+(?=(Z|[+-]\d{2}:\d{2})$)/, '$1')
  const date = new Date(normalized)
  return Number.isNaN(date.getTime()) ? value : new Intl.DateTimeFormat(intlLocale(), {
    year: 'numeric', month: '2-digit', day: '2-digit', hour: '2-digit', minute: '2-digit', second: '2-digit', hour12: false,
  }).format(date)
}

const MemoryRow = memo(function MemoryRow({
  memory: m,
  ranking,
  onPin,
  onEdit,
  onDelete,
}: {
  memory: MemoryItem
  ranking?: MemoryImportance
  onPin: (memoryId: string, pinned: boolean) => void
  onEdit: (item: MemoryItem) => void
  onDelete: (item: MemoryItem) => void
}) {
  const { t } = useTranslation()
  const level = ranking?.score == null ? null : ranking.score >= 70 ? 'high' : ranking.score >= 40 ? 'medium' : 'low'
  const pinLabel = ranking?.pinned ? t('memory.importanceUnpin') : t('memory.importancePin')
  return (
    <div className="rounded-md bg-gray-50 dark:bg-gray-700/50 border border-gray-200 dark:border-gray-600 px-3 py-2 [content-visibility:auto] [contain-intrinsic-size:auto_64px]">
      <div className="flex items-start gap-2">
        <MemoryMarkdown content={m.memory} className="flex-1 break-words" />
        <div className="flex shrink-0 gap-1">
          <button type="button" onClick={() => { void onPin(m.id, !ranking?.pinned) }} className={`inline-flex h-7 w-7 items-center justify-center rounded hover:bg-amber-100 hover:text-amber-800 dark:hover:bg-amber-900/40 dark:hover:text-amber-200 ${ranking?.pinned ? 'text-amber-500' : 'text-gray-500 dark:text-gray-400'}`} title={pinLabel} aria-label={pinLabel}>
            <Star size={13} className={ranking?.pinned ? 'fill-current' : ''} />
          </button>
          <button type="button" onClick={() => onEdit(m)} className="inline-flex h-7 w-7 items-center justify-center rounded hover:bg-gray-200 dark:hover:bg-gray-600 text-gray-500 dark:text-gray-400" title={t('common.edit')} aria-label={t('common.edit')}>
            <Pencil size={13} />
          </button>
          <button type="button" onClick={() => onDelete(m)} className="ml-2 inline-flex h-7 w-7 items-center justify-center rounded hover:bg-red-100 dark:hover:bg-red-900/40 text-gray-500 dark:text-gray-400 hover:text-red-800 dark:hover:text-red-200" title={t('common.delete')} aria-label={t('common.delete')}>
            <Trash2 size={13} />
          </button>
        </div>
      </div>
      <div className="mt-0.5 flex flex-wrap items-center gap-2">
        {m.user_defined && <span className="text-xs px-1.5 rounded bg-violet-600/15 font-medium text-violet-700 dark:text-violet-300" title={t('memory.userDefinedPlaceholder')}>{t('memory.userDefinedBadge')}</span>}
        {m.memory_type && <span className="text-xs px-1.5 rounded bg-violet-500/10 text-violet-600 dark:text-violet-400">{t(`memory.type.${m.memory_type}`, { defaultValue: m.memory_type })}</span>}
        {m.durability && <span className={`text-xs px-1.5 rounded ${m.durability === 'long_term' ? 'bg-emerald-500/10 text-emerald-700 dark:text-emerald-300' : m.durability === 'short_term' ? 'bg-sky-500/10 text-sky-700 dark:text-sky-300' : 'bg-gray-200 text-gray-600 dark:bg-gray-600 dark:text-gray-200'}`}>{t(`memory.durability${m.durability === 'long_term' ? 'LongTerm' : m.durability === 'short_term' ? 'ShortTerm' : 'Session'}`)}</span>}
        {(m.domains ?? []).map((domain) => <span key={domain.id} className="text-xs px-1.5 rounded bg-indigo-500/10 text-indigo-700 dark:text-indigo-300" title={domain.description || domain.name}>{domain.name}</span>)}
        {level && ranking && <span className={`text-xs px-1.5 rounded ${level === 'high' ? 'bg-amber-500/15 text-amber-700 dark:text-amber-300' : level === 'medium' ? 'bg-sky-500/10 text-sky-700 dark:text-sky-300' : 'bg-gray-200 text-gray-600 dark:bg-gray-600 dark:text-gray-200'}`}>{t(`memory.importance${level[0].toUpperCase()}${level.slice(1)}`, { score: ranking.score })}</span>}
        {ranking && <span className="text-xs text-gray-500">{t('memory.importanceEvidence', { sessions: ranking.supporting_sessions, agents: ranking.supporting_agents, recalls: ranking.recall_count })}</span>}
        {(m.event_time || m.last_update_at) && <span className="text-xs text-gray-500" title={m.event_time ? t('memory.eventTimeTitle') : t('memory.memoryOrganizeTime')}>{localTime(m.event_time || m.last_update_at || '')}</span>}
      </div>
    </div>
  )
})

interface ClassificationStatus {
  total: number
  processed: number
  pending_ids: string[]
}

export const OrganizedConversations = memo(function OrganizedConversations({ onBack, active = true }: { onBack: () => void; active?: boolean }) {
  const { t } = useTranslation()
  const { memories, memoryDomains, importance, memoryCacheReady, listMemories, loadMemoryDomains, updateMemory, deleteMemory, setMemoryPinned, refreshImportance } = useMemoryFields('memories', 'memoryDomains', 'importance', 'memoryCacheReady', 'listMemories', 'loadMemoryDomains', 'updateMemory', 'deleteMemory', 'setMemoryPinned', 'refreshImportance')
  const [domainFilter, setDomainFilter] = useState<string>('all')
  const [refreshing, setRefreshing] = useState(false)
  const [classifying, setClassifying] = useState(false)
  const [classificationStatus, setClassificationStatus] = useState<ClassificationStatus | null>(null)
  const [classifyNotice, setClassifyNotice] = useState<{ kind: 'ok' | 'err'; text: string } | null>(null)
  const [editing, setEditing] = useState<MemoryItem | null>(null)
  const [editContent, setEditContent] = useState('')
  const [deleteTarget, setDeleteTarget] = useState<MemoryItem | null>(null)
  const [deleteDontAsk, setDeleteDontAsk] = useState(false)
  const confirmDeleteRef = useRef<((item: MemoryItem) => Promise<void>) | null>(null)

  useEffect(() => {
    if (!active) return
    void listMemories(true)
    void loadMemoryDomains()
  }, [listMemories, loadMemoryDomains, active])

  const domainCounts = useMemo(() => {
    const counts = new Map<string, number>()
    for (const item of memories) {
      for (const d of item.domains ?? []) {
        counts.set(d.id, (counts.get(d.id) ?? 0) + 1)
      }
    }
    return counts
  }, [memories])

  const visible = useMemo(() => {
    if (domainFilter === 'all') return memories
    return memories.filter((item) => (item.domains ?? []).some((d) => d.id === domainFilter))
  }, [memories, domainFilter])

  const loadClassificationStatus = useCallback(async () => {
    const status = await invoke<ClassificationStatus>('memory_domain_classification_status')
    setClassificationStatus(status)
    return status
  }, [])

  useEffect(() => {
    if (!active) return
    void loadClassificationStatus().catch((error) => {
      setClassifyNotice({ kind: 'err', text: String(error) })
    })
  }, [active, memories, loadClassificationStatus])

  async function handleRefresh() {
    if (refreshing) return
    setRefreshing(true)
    try {
      await Promise.all([listMemories(true), loadMemoryDomains(), loadClassificationStatus()])
    } finally {
      setRefreshing(false)
    }
  }

  async function handleClassifyAll() {
    if (classifying) return
    setClassifying(true)
    setClassifyNotice(null)
    try {
      // Rebuild from SQLite, never from the page's transient counter. Even an
      // interrupted four-item batch may already have committed its first items.
      const initial = await loadClassificationStatus()
      if (initial.pending_ids.length === 0) return
      const config = await invoke<{
        enabled: boolean
        provider: 'jev' | 'ollama'
        model: string
        has_api_key: boolean
      }>('memory_domain_config_get')
      const ready = config.enabled && !!config.model.trim()
        && (config.provider === 'ollama' || config.has_api_key)
      if (!ready) throw new Error(t('memory.pending.classifyNotReady'))
      const BATCH_SIZE = 4
      let saved = initial
      for (let index = 0; index < initial.pending_ids.length; index += BATCH_SIZE) {
        const slice = initial.pending_ids.slice(index, index + BATCH_SIZE)
        await invoke('memory_domain_classify_batch', { memoryIds: slice })
        saved = await loadClassificationStatus()
      }
      setClassifyNotice({
        kind: 'ok',
        text: t('memory.organized.classifyMemoriesDone', { processed: saved.processed, total: saved.total }),
      })
    } catch (error) {
      const saved = await loadClassificationStatus().catch(() => null)
      setClassifyNotice({
        kind: 'err',
        text: saved
          ? t('memory.organized.classifyInterrupted', { processed: saved.processed, total: saved.total, error: String(error) })
          : t('memory.pending.classifyFailed', { error: String(error) }),
      })
    } finally {
      setClassifying(false)
      await listMemories(true).catch(() => undefined)
    }
  }

  const handlePinMemory = useCallback(async (memoryId: string, pinned: boolean) => {
    try {
      await setMemoryPinned(memoryId, pinned)
      await refreshImportance()
      await listMemories(true)
    } catch (error) {
      setClassifyNotice({ kind: 'err', text: `${t('common.failed')}: ${String(error)}` })
    }
  }, [setMemoryPinned, refreshImportance, listMemories, t])

  const openEdit = useCallback((item: MemoryItem) => {
    setEditing(item)
    setEditContent(item.memory)
  }, [])

  async function handleSaveEdit() {
    if (!editing || !editContent.trim()) return
    try {
      await updateMemory(editing.id, editContent.trim())
      setEditing(null)
      await listMemories(true)
    } catch (error) {
      setClassifyNotice({ kind: 'err', text: `${t('common.failed')}: ${String(error)}` })
    }
  }

  const confirmDelete = useCallback(async (targetOverride?: MemoryItem) => {
    const target = targetOverride ?? deleteTarget
    if (!target) return
    if (!targetOverride && deleteDontAsk) {
      localStorage.setItem('memory.deleteSkipConfirm', '1')
    }
    try {
      await deleteMemory(target.id)
      setDeleteTarget(null)
      setDeleteDontAsk(false)
      await listMemories(true)
    } catch (error) {
      setDeleteTarget(null)
      setClassifyNotice({ kind: 'err', text: `${t('common.failed')}: ${String(error)}` })
    }
  }, [deleteTarget, deleteDontAsk, deleteMemory, listMemories, t])

  useEffect(() => {
    confirmDeleteRef.current = confirmDelete
  }, [confirmDelete])

  const handleDelete = useCallback((item: MemoryItem) => {
    if (localStorage.getItem('memory.deleteSkipConfirm') === '1') {
      setDeleteTarget(item)
      void confirmDeleteRef.current?.(item)
      return
    }
    setDeleteDontAsk(false)
    setDeleteTarget(item)
  }, [])

  // 页头与分类筛选固定不动，只有下方记忆列表在剩余高度内部滚动。
  return <div className="flex h-full min-h-0 flex-1 flex-col overflow-hidden bg-[var(--app-canvas)] text-slate-900 dark:text-slate-100">
    <div className="app-page-fluid flex min-h-0 flex-1 flex-col py-6 lg:py-8">
      <header className="fluent-page-header flex shrink-0 flex-wrap items-start justify-between gap-4">
        <div className="flex items-start gap-3">
          <button type="button" onClick={onBack} className="mt-0.5 inline-flex h-9 w-9 items-center justify-center rounded-lg text-slate-500 transition hover:bg-slate-100 hover:text-slate-900 focus:outline-none focus-visible:ring-2 focus-visible:ring-blue-500 dark:text-slate-400 dark:hover:bg-slate-800 dark:hover:text-white" aria-label={t('memory.organized.back')}><ArrowLeft size={19} /></button>
          <div>
            <MemoryBreadcrumb currentKey="memory.organized.title" onBack={onBack} />
            <h1 className="mt-1 font-bold tracking-[-0.025em]">{t('memory.organized.title')}</h1>
            <p className="mt-1 text-sm text-slate-500 dark:text-slate-400">{t('memory.organized.desc')}</p>
          </div>
        </div>
        <div className="flex flex-wrap items-center gap-2">
          {classifyNotice && <span className={`rounded-lg px-3 py-2 text-xs ${classifyNotice.kind === 'ok' ? 'bg-emerald-500/10 text-emerald-700 dark:text-emerald-300' : 'bg-red-500/10 text-red-700 dark:text-red-300'}`} role="status">{classifyNotice.text}</span>}
          <button type="button" onClick={() => { void handleClassifyAll() }} disabled={classifying || !classificationStatus || classificationStatus.pending_ids.length === 0} title={t('memory.pending.classifyTitle')} className="inline-flex h-10 items-center gap-2 rounded-lg border border-emerald-200 bg-white px-3 text-sm font-medium text-emerald-700 transition hover:bg-emerald-50 disabled:cursor-not-allowed disabled:opacity-50 focus:outline-none focus-visible:ring-2 focus-visible:ring-emerald-500 dark:border-emerald-500/30 dark:bg-slate-900 dark:text-emerald-300 dark:hover:bg-emerald-500/10">{classifying ? <Loader2 size={16} className="animate-spin motion-reduce:animate-none" /> : <Tags size={16} />}{classifying ? t('memory.pending.classifyRunning') : classificationStatus && classificationStatus.processed > 0 && classificationStatus.pending_ids.length > 0 ? t('memory.organized.classifyResume') : t('memory.pending.classify')}</button>
          <button type="button" onClick={() => { void handleRefresh() }} disabled={refreshing || classifying} className="inline-flex h-10 items-center gap-2 rounded-lg border border-slate-200 bg-white px-3 text-sm font-medium text-slate-700 transition hover:border-slate-300 hover:bg-slate-50 disabled:cursor-wait disabled:opacity-60 focus:outline-none focus-visible:ring-2 focus-visible:ring-blue-500 dark:border-slate-700 dark:bg-slate-900 dark:text-slate-200 dark:hover:bg-slate-800"><RefreshCw size={16} className={refreshing ? 'animate-spin motion-reduce:animate-none' : ''} />{t('memory.organized.refresh')}</button>
        </div>
      </header>

      {classificationStatus && classificationStatus.total > 0 && (() => {
        const percent = Math.min(100, Math.round((classificationStatus.processed / classificationStatus.total) * 100))
        return <div className="mt-5 shrink-0 rounded-xl border border-emerald-200 bg-emerald-50/60 px-4 py-3 dark:border-emerald-500/30 dark:bg-emerald-500/10" role="status">
          <div className="flex flex-wrap items-center gap-x-3 gap-y-1 text-xs text-emerald-800 dark:text-emerald-200">
            {classifying ? <Loader2 size={13} className="animate-spin motion-reduce:animate-none" /> : <Tags size={13} />}
            <span className="font-medium">{t('memory.organized.classifySavedProgress', { processed: classificationStatus.processed, total: classificationStatus.total, remaining: classificationStatus.pending_ids.length })}</span>
            <span className="ml-auto font-mono tabular-nums">{percent}%</span>
          </div>
          <div className="mt-2 h-1.5 overflow-hidden rounded-full bg-emerald-200/60 dark:bg-emerald-900/40" aria-hidden="true">
            <div className="h-full rounded-full bg-emerald-500 transition-all" style={{ width: `${percent}%` }} />
          </div>
        </div>
      })()}

      <section className="mt-7 shrink-0" aria-label={t('memory.organized.overviewLabel')}>
        <div className="flex flex-wrap items-center gap-2">
          <button type="button" onClick={() => setDomainFilter('all')} aria-pressed={domainFilter === 'all'} className={`inline-flex items-center gap-1.5 rounded-lg border px-3 py-1.5 text-xs font-medium transition focus:outline-none focus-visible:ring-2 focus-visible:ring-blue-500 ${domainFilter === 'all' ? 'border-emerald-400 bg-emerald-50/60 text-emerald-700 dark:border-emerald-500/60 dark:bg-emerald-500/10 dark:text-emerald-300' : 'border-slate-200 bg-white text-slate-600 hover:border-slate-300 dark:border-slate-800 dark:bg-slate-900 dark:text-slate-300 dark:hover:border-slate-700'}`}>
            {t('memory.domainAll')} <span className="font-mono tabular-nums">{memoryCacheReady ? memories.length : '—'}</span>
          </button>
          {memoryDomains.map((domain) => {
            const count = domainCounts.get(domain.id) ?? 0
            return (
              <button key={domain.id} type="button" onClick={() => setDomainFilter(domainFilter === domain.id ? 'all' : domain.id)} aria-pressed={domainFilter === domain.id} title={domain.description || domain.name} className={`inline-flex items-center gap-1.5 rounded-lg border px-3 py-1.5 text-xs font-medium transition focus:outline-none focus-visible:ring-2 focus-visible:ring-blue-500 ${domainFilter === domain.id ? 'border-emerald-400 bg-emerald-50/60 text-emerald-700 dark:border-emerald-500/60 dark:bg-emerald-500/10 dark:text-emerald-300' : 'border-slate-200 bg-white text-slate-600 hover:border-slate-300 dark:border-slate-800 dark:bg-slate-900 dark:text-slate-300 dark:hover:border-slate-700'}`}>
                {domain.name} <span className="font-mono tabular-nums">{count}</span>
              </button>
            )
          })}
        </div>
      </section>

      <section className="mt-6 flex min-h-0 flex-1 flex-col rounded-2xl border border-slate-200 bg-white p-5 shadow-[0_1px_2px_rgba(15,23,42,0.03)] dark:border-slate-800 dark:bg-slate-900 sm:p-6" aria-label={t('memory.organized.listLabel')}>
        {!memoryCacheReady ? (
          <div className="flex min-h-48 items-center justify-center gap-2 text-sm text-slate-500"><Loader2 size={16} className="animate-spin motion-reduce:animate-none" />{t('memory.organized.loading')}</div>
        ) : visible.length === 0 ? (
          <div className="flex min-h-48 flex-col items-center justify-center text-center">
            <span className="grid h-11 w-11 place-items-center rounded-xl bg-slate-500/10 text-slate-500"><Brain size={20} /></span>
            <h2 className="mt-3 font-medium">{domainFilter === 'all' ? t('memory.empty') : t('memory.domainEmpty')}</h2>
            <p className="mt-1 max-w-md text-sm text-slate-500 dark:text-slate-400">{t('memory.organized.emptyDesc')}</p>
          </div>
        ) : (
          <div className="min-h-0 flex-1 space-y-1.5 overflow-y-auto overscroll-contain pr-1">
            {visible.map((m) => (
              <MemoryRow key={m.id} memory={m} ranking={importance[m.id]} onPin={handlePinMemory} onEdit={openEdit} onDelete={handleDelete} />
            ))}
          </div>
        )}
      </section>

      {editing && (
        <Modal labelledBy="organized-edit-title" onClose={() => setEditing(null)}>
          <div className="w-full max-w-md rounded-xl bg-white dark:bg-gray-800 border border-gray-200 dark:border-gray-700 p-4 space-y-3">
            <div className="flex items-center justify-between">
              <h3 id="organized-edit-title" className="text-sm font-semibold text-gray-800 dark:text-gray-100">{t('memory.editTitle')}</h3>
              <button onClick={() => setEditing(null)} className="p-1 rounded hover:bg-gray-100 dark:hover:bg-gray-700 text-gray-500">
                <X size={15} />
              </button>
            </div>
            <textarea
              data-autofocus
              value={editContent}
              onChange={(e) => setEditContent(e.target.value)}
              rows={4}
              className="w-full rounded-md border border-gray-300 dark:border-gray-600 bg-white dark:bg-gray-700 px-2.5 py-2 text-sm text-gray-800 dark:text-gray-100 focus:outline-none focus:ring-1 focus:ring-violet-500"
            />
            <div className="flex justify-end gap-2">
              <button onClick={() => setEditing(null)} className="px-3 py-1.5 rounded-md border border-gray-300 dark:border-gray-600 text-sm text-gray-600 dark:text-gray-300 hover:bg-gray-50 dark:hover:bg-gray-700">
                {t('common.cancel')}
              </button>
              <button onClick={() => { void handleSaveEdit() }} className="px-3 py-1.5 rounded-md bg-violet-600 hover:bg-violet-500 text-white text-sm font-medium">
                {t('common.save')}
              </button>
            </div>
          </div>
        </Modal>
      )}

      {deleteTarget && (
        <Modal labelledBy="organized-delete-title" dismissOnBackdrop onClose={() => setDeleteTarget(null)}>
          <div className="w-full max-w-sm rounded-xl bg-white dark:bg-gray-800 border border-gray-200 dark:border-gray-700 p-4 space-y-3">
            <div className="flex items-center gap-2">
              <Trash2 size={18} className="text-red-500" />
              <h3 id="organized-delete-title" className="text-sm font-semibold text-gray-800 dark:text-gray-100">{t('memory.deleteConfirm')}</h3>
            </div>
            <p className="text-sm text-gray-600 dark:text-gray-300 line-clamp-3 break-all">{deleteTarget.memory}</p>
            <p className="text-xs text-amber-600 dark:text-amber-400">{t('memory.deleteGlobalHint')}</p>
            <label className="flex items-center gap-2 text-xs text-gray-500 dark:text-gray-400 select-none">
              <input
                type="checkbox"
                checked={deleteDontAsk}
                onChange={(event) => setDeleteDontAsk(event.target.checked)}
                className="rounded border-gray-300 dark:border-gray-600"
              />
              {t('memory.deleteDontAsk')}
            </label>
            <div className="flex justify-end gap-2">
              <button data-autofocus onClick={() => setDeleteTarget(null)} className="px-3 py-1.5 rounded-md border border-gray-300 dark:border-gray-600 text-sm text-gray-600 dark:text-gray-300 hover:bg-gray-50 dark:hover:bg-gray-700">
                {t('common.cancel')}
              </button>
              <button onClick={() => { void confirmDelete() }} className="px-3 py-1.5 rounded-md bg-red-600 hover:bg-red-500 text-white text-sm font-medium">
                {t('common.delete')}
              </button>
            </div>
          </div>
        </Modal>
      )}
    </div>
  </div>
})
