import { memo, useCallback, useEffect, useMemo, useRef, useState } from 'react'
import { useTranslation } from 'react-i18next'
import { listen } from '@tauri-apps/api/event'
import { open } from '@tauri-apps/plugin-dialog'
import {
  Brain, Search, Trash2, Pencil, MoonStar, CheckCircle2, XCircle, Loader2, X,
  PowerOff, Webhook, History, RefreshCw, ChevronDown, ChevronUp, FileText, Star, Database, FolderOpen,
  ChevronRight, Plus,
} from 'lucide-react'
import { useMemoryFields, type MemoryLoadStatus } from '../store/memoryStore'
import { useTelemetryFields } from '../store/telemetryStore'
import { useVisiblePolling } from '../hooks/useVisiblePolling'
import { ErrorRecovery } from '../components/ErrorRecovery'
import type { ConsolidationResult, HookStatus, MemoryDomainItem, MemoryImportance, MemoryImportResult, MemoryItem, MemoryLayerDocument } from '../types/memory'
import { normalizeL2Document } from '../lib/thinking'
import { MemoryMarkdown } from '../components/MemoryMarkdown'
import { CloudVaultCard } from '../components/CloudVaultCard'
import { Modal } from '../components/Modal'
import { useConfirmDialog } from '../components/ConfirmDialog'
import { sourceLabel } from '../components/ConversationDialog'
import { intlLocale } from '../i18n'
import { compactTokenCount } from '../lib/compactNumber'
import { isInjectionReady } from '../lib/memoryAgentStatus'
import { afterNextPaint, recordStartupPhase } from '../lib/startupPerformance'

function localTime(value: string) {
  // Rust serializes UTC timestamps with nanosecond precision. Normalise to
  // JavaScript's millisecond precision before parsing so WebView variants do
  // not fall back to exposing the raw RFC3339 value.
  const normalized = value.trim().replace(/(\.\d{3})\d+(?=(Z|[+-]\d{2}:\d{2})$)/, '$1')
  const date = new Date(normalized)
  return Number.isNaN(date.getTime()) ? value : new Intl.DateTimeFormat(intlLocale(), {
    year: 'numeric', month: '2-digit', day: '2-digit', hour: '2-digit', minute: '2-digit', second: '2-digit', hour12: false,
  }).format(date)
}

function looksLikeModelAnalysis(content: string) {
  const opening = content.trim().slice(0, 180).toLowerCase()
  return /^(the user wants|let me (analyze|review|think)|i (need|will|should) |analysis:)/.test(opening)
}

function isInvalidL3Draft(content: string) {
  const normalized = content.trim().toLowerCase()
  return looksLikeModelAnalysis(content)
    || normalized.includes('pending human approval')
    || normalized.includes('[内容已截断]')
    || normalized.includes('[truncated]')
}

function errorMessage(error: unknown) {
  if (error instanceof Error && error.message.trim()) return error.message.trim()
  if (typeof error === 'string' && error.trim()) return error.trim()
  if (error && typeof error === 'object') {
    for (const key of ['message', 'error', 'detail'] as const) {
      const value = (error as Record<string, unknown>)[key]
      if (typeof value === 'string' && value.trim()) return value.trim()
    }
    try {
      return JSON.stringify(error)
    } catch {
      // Fall through to the generic representation below.
    }
  }
  return String(error)
}

function InlineActionError({ text }: { text: string }) {
  return (
    <div role="alert" aria-live="assertive" className="mt-2 whitespace-pre-wrap break-words rounded-md border border-red-200 bg-red-50 px-2.5 py-2 text-xs leading-5 text-red-800 dark:border-red-900/70 dark:bg-red-950/30 dark:text-red-200">
      {text}
      <ErrorRecovery error={text} />
    </div>
  )
}

function InlineActionSuccess({ text }: { text: string }) {
  return (
    <div role="status" aria-live="polite" className="mt-2 whitespace-pre-wrap break-words rounded-md border border-green-200 bg-green-50 px-2.5 py-2 text-xs leading-5 text-green-800 dark:border-green-900/70 dark:bg-green-950/30 dark:text-green-200">
      {text}
    </div>
  )
}

function MemorySectionStatus({ status, loadingLabel, onRetry }: { status: MemoryLoadStatus; loadingLabel?: string; onRetry: () => void }) {
  const { t } = useTranslation()
  if (status === 'ready') return null
  return <p role="status" aria-live="polite" className="inline-flex items-center gap-1.5 text-xs text-gray-500 dark:text-gray-400">
    {status === 'error' ? <>{t('memory.sectionLoadFailed')}<button type="button" onClick={onRetry} className="font-medium text-violet-600 hover:underline dark:text-violet-300">{t('common.retry')}</button></> : <><Loader2 size={13} className="animate-spin motion-reduce:animate-none" />{loadingLabel ?? t('memory.overviewLoading')}</>}
  </p>
}

function LayerWindow({ document }: { document: MemoryLayerDocument }) {
  const { t } = useTranslation()
  if (!document.window_start || !document.window_end) return null
  return <span className="text-gray-500">{t('memory.layerWindowRange', { start: localTime(document.window_start), end: localTime(document.window_end) })}</span>
}

// L2 正文与 L3 文档列表使用同一套内部滚动，标题和顶部操作不参与滚动。
const MEMORY_CONTENT_SCROLL_CLASS = 'max-h-80 min-w-0 max-w-full overflow-y-auto overscroll-contain pr-1'

/** L1 列表行。memo + 稳定回调隔离：无关 store 更新（agent 轮询等）不再
 *  重渲染最多 200 张卡片；content-visibility 让滚出视口的行跳过布局绘制。 */
const MemoryCard = memo(function MemoryCard({ memory: m, ranking, pinDisabled, onPin, onEdit, onDelete }: {
  memory: MemoryItem
  ranking: MemoryImportance
  pinDisabled: boolean
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
          <button type="button" disabled={pinDisabled} onClick={() => { void onPin(m.id, !ranking?.pinned) }} className={`inline-flex h-7 w-7 items-center justify-center rounded hover:bg-amber-100 hover:text-amber-800 disabled:opacity-50 dark:hover:bg-amber-900/40 dark:hover:text-amber-200 ${ranking?.pinned ? 'text-amber-500' : 'text-gray-500 dark:text-gray-400'}`} title={pinLabel} aria-label={pinLabel}>
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
        {level && <span className={`text-xs px-1.5 rounded ${level === 'high' ? 'bg-amber-500/15 text-amber-700 dark:text-amber-300' : level === 'medium' ? 'bg-sky-500/10 text-sky-700 dark:text-sky-300' : 'bg-gray-200 text-gray-600 dark:bg-gray-600 dark:text-gray-200'}`}>{t(`memory.importance${level[0].toUpperCase()}${level.slice(1)}`, { score: ranking.score })}</span>}
        {ranking && <span className="text-xs text-gray-500">{t('memory.importanceEvidence', { sessions: ranking.supporting_sessions, agents: ranking.supporting_agents, recalls: ranking.recall_count })}</span>}
        {(m.event_time || m.last_update_at) && <span className="text-xs text-gray-500" title={m.event_time ? t('memory.eventTimeTitle') : t('memory.memoryOrganizeTime')}>{localTime(m.event_time || m.last_update_at || '')}</span>}
      </div>
    </div>
  )
})

export const MemoryCenter = memo(function MemoryCenter({ onOpenUsage, onOpenGraph, onOpenPending, onOpenOrganized, onOpenCapture, onOpenInjection, active = true }: { onOpenUsage?: () => void; onOpenGraph?: () => void; onOpenPending?: () => void; onOpenOrganized?: () => void; onOpenCapture?: () => void; onOpenInjection?: () => void; active?: boolean }) {
  const { t } = useTranslation()
  const { engineOnline, memories, memoryDomains, selectedMemoryDomainId, importance, loading, lastSearchResults, lastSearchQuery, memoryCacheReady, localMemoryStats, ingestStatus, l2Documents, l3Documents, checkIngest, loadMemoryLayers, consolidateShortTermMemory, draftLongTermProfile, publishLongTermProfile, updatePublishedMemoryDocument, deleteLongTermProfileDraft, organizeConversations, cancelOrganize, importMemoryFolder, checkEngine, stopEngine, search, listMemories, loadMemoryDomains, setSelectedMemoryDomain, resetL1ForReextraction, updateMemory, deleteMemory, addUserMemory, dreaming, restoreConsolidation, refreshImportance, setMemoryPinned, hookStatus, qoderHookStatus, codexHookStatus, workbuddyHookStatus, geminiHookStatus, antigravityHookStatus, agentSources, loadAgentSources } = useMemoryFields('engineOnline', 'memories', 'memoryDomains', 'selectedMemoryDomainId', 'importance', 'loading', 'lastSearchResults', 'lastSearchQuery', 'memoryCacheReady', 'localMemoryStats', 'ingestStatus', 'l2Documents', 'l3Documents', 'checkIngest', 'loadMemoryLayers', 'consolidateShortTermMemory', 'draftLongTermProfile', 'publishLongTermProfile', 'updatePublishedMemoryDocument', 'deleteLongTermProfileDraft', 'organizeConversations', 'cancelOrganize', 'importMemoryFolder', 'checkEngine', 'stopEngine', 'search', 'listMemories', 'loadMemoryDomains', 'setSelectedMemoryDomain', 'resetL1ForReextraction', 'updateMemory', 'deleteMemory', 'addUserMemory', 'dreaming', 'restoreConsolidation', 'refreshImportance', 'setMemoryPinned', 'hookStatus', 'qoderHookStatus', 'codexHookStatus', 'workbuddyHookStatus', 'geminiHookStatus', 'antigravityHookStatus', 'agentSources', 'loadAgentSources')
  const { memoryLoadState } = useMemoryFields('memoryLoadState')
  const { telemetrySummary, telemetryLiveStatus, checkTelemetry } = useTelemetryFields('telemetrySummary', 'telemetryLiveStatus', 'checkTelemetry')

  const [query, setQuery] = useState('')
  const [topK, setTopK] = useState(10)
  const [notice, setNotice] = useState<{ kind: 'ok' | 'err'; text: string } | null>(null)
  const [editing, setEditing] = useState<MemoryItem | null>(null)
  const [editContent, setEditContent] = useState('')
  const [deleteTarget, setDeleteTarget] = useState<MemoryItem | null>(null)
  const [deleteDontAsk, setDeleteDontAsk] = useState(false)
  // 「不再提醒」的直删路径要调用组件后部声明的 confirmDelete：用
  // latest-ref 桥接（函数声明提升，渲染时即可赋值），保持 handleDelete
  // 的稳定引用。
  const confirmDeleteRef = useRef<((item: MemoryItem) => Promise<void>) | null>(null)
  // latest-ref 桥接：每次渲染提交后同步组件后部声明的 confirmDelete，
  // 供「不再提醒」的直删路径调用（函数声明提升，回调执行时已定义）。
  useEffect(() => {
    confirmDeleteRef.current = confirmDelete
  })
  const [refreshing, setRefreshing] = useState(false)
  const [organizingConversations, setOrganizingConversations] = useState(false)
  const [importingMemories, setImportingMemories] = useState(false)
  const [memoryImportResult, setMemoryImportResult] = useState<MemoryImportResult | null>(null)
  const [consolidating, setConsolidating] = useState(false)
  const [consolidationResult, setConsolidationResult] = useState<{ kind: 'ok' | 'err'; text: string; result?: ConsolidationResult } | null>(null)
  const [restoringConsolidation, setRestoringConsolidation] = useState(false)
  const [refreshingImportance, setRefreshingImportance] = useState(false)
  const [ingestOpen, setIngestOpen] = useState(false)
  const [consolidationOpen, setConsolidationOpen] = useState(false)
  const [consolidationProgress, setConsolidationProgress] = useState<string | null>(null)
  const [consolidatingL2, setConsolidatingL2] = useState(false)
  const [l2ConsolidationError, setL2ConsolidationError] = useState<string | null>(null)
  const [l2ConsolidationSuccess, setL2ConsolidationSuccess] = useState<string | null>(null)
  const [draftingL3, setDraftingL3] = useState(false)
  const [l3DraftError, setL3DraftError] = useState<string | null>(null)
  const [publishingL3, setPublishingL3] = useState<string | null>(null)
  const [deletingL3, setDeletingL3] = useState<string | null>(null)
  const [l3DeleteTarget, setL3DeleteTarget] = useState<MemoryLayerDocument | null>(null)
  const [resettingL1, setResettingL1] = useState(false)
  const [resetL1ConfirmOpen, setResetL1ConfirmOpen] = useState(false)
  // 用户自定义添加：折叠表单，提交后写入 user_defined 长期记忆。
  const [userMemoryOpen, setUserMemoryOpen] = useState(false)
  const [userMemoryContent, setUserMemoryContent] = useState('')
  const [userMemoryType, setUserMemoryType] = useState('fact')
  const [addingUserMemory, setAddingUserMemory] = useState(false)
  const [userMemoryErrors, setUserMemoryErrors] = useState<Partial<Record<'l2' | 'l3', string>>>({})
  const [userMemorySuccesses, setUserMemorySuccesses] = useState<Partial<Record<'l2' | 'l3', string>>>({})
  // 草案编辑：只存在组件状态里，不单独持久化；发布时随发布落盘，
  // 离开页面即丢弃。
  const [editingL3Id, setEditingL3Id] = useState<string | null>(null)
  const [editingL3Content, setEditingL3Content] = useState('')
  // L2 工作记忆编辑：L2 生成即发布，保存即覆盖当前发布版。
  const [editingL2, setEditingL2] = useState(false)
  const [editingL2Content, setEditingL2Content] = useState('')
  const [savingL2, setSavingL2] = useState(false)
  // L2 卡片的自定义添加展开态（与 L3 卡片共用同一份表单内容与提交状态）。
  const [l2UserMemoryOpen, setL2UserMemoryOpen] = useState(false)
  // Cached sections and local editor state stay visible during background reads.
  const domainFilterReady = selectedMemoryDomainId === 'all' || memoryLoadState.domains === 'ready'
  const inventoryReady = memoryCacheReady && domainFilterReady
  // Native L1 extraction stores conversation summaries as `conversation`.
  // They are factual, searchable records—not user preferences. Only the
  // profile-oriented kinds belong to the preference total.
  const isPreference = useCallback((memory: MemoryItem) => ['preference', 'profile', 'constraint'].includes((memory.memory_type ?? '').toLowerCase()), [])
  const visibleMemories = useMemo(() => {
    if (selectedMemoryDomainId === 'all') return memories
    return memories.filter((memory) => (memory.domains ?? []).some((domain) => domain.id === selectedMemoryDomainId))
  }, [memories, selectedMemoryDomainId])
  const visibleSearchResults = useMemo(() => {
    if (selectedMemoryDomainId === 'all') return lastSearchResults
    return lastSearchResults.filter((memory) => (memory.domains ?? []).some((domain) => domain.id === selectedMemoryDomainId))
  }, [lastSearchResults, selectedMemoryDomainId])
  const visibleStats = useMemo(() => {
    if (selectedMemoryDomainId === 'all') return null
    return {
      total: visibleMemories.length,
      facts: visibleMemories.filter((memory) => !isPreference(memory)).length,
      preferences: visibleMemories.filter(isPreference).length,
      session_only: visibleMemories.filter((memory) => memory.durability === 'session').length,
      short_term: visibleMemories.filter((memory) => memory.durability === 'short_term').length,
      long_term: visibleMemories.filter((memory) => memory.durability === 'long_term').length,
    }
  }, [selectedMemoryDomainId, visibleMemories, isPreference])
  const factCount = useMemo(() => memories.filter((memory) => !isPreference(memory)).length, [memories, isPreference])
  const prefCount = useMemo(() => memories.filter(isPreference).length, [memories, isPreference])
  const visibleL3Documents = l3Documents.filter((document) => !isInvalidL3Draft(document.content))
  const invalidL3Drafts = l3Documents.filter((document) => document.state === 'draft' && isInvalidL3Draft(document.content))

  useEffect(() => {
    const started = performance.now()
    const cancelPaintTiming = afterNextPaint(() => recordStartupPhase('memory_shell_painted', performance.now() - started))
    // Each section publishes independently. SQLite inventory does not depend
    // on the optional semantic engine becoming available.
    void listMemories().catch(() => {})
    void loadMemoryLayers().catch(() => {})
    void checkEngine()
    // Token reconciliation remains an application-start background job.
    return cancelPaintTiming
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [])

  useEffect(() => {
    let unlisten: (() => void) | undefined
    void listen<{ detail?: string }>('memory-consolidation-progress', (event) => {
      setConsolidationProgress(event.payload.detail ?? t('memory.consolidateJudging'))
    }).then((dispose) => { unlisten = dispose }).catch(() => {})
    return () => unlisten?.()
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [])

  // L2/L3 后台自动重算：重算完成事件到达时刷新已挂载页的文档（开关与
  // 间隔在设置页）。
  useEffect(() => {
    let unlisten: (() => void) | undefined
    void listen('memory-layers-updated', () => { void loadMemoryLayers(true).catch(() => {}) })
      .then((dispose) => { unlisten = dispose })
      .catch(() => {})
    return () => unlisten?.()
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [])

  const refreshSummary = useCallback(() => Promise.allSettled([
    checkIngest(), checkTelemetry(), loadAgentSources(),
  ]), [checkIngest, checkTelemetry, loadAgentSources])
  useVisiblePolling(refreshSummary, organizingConversations || ingestStatus?.manual_extraction_running ? 2_000 : 10_000, active)

  const flash = useCallback((kind: 'ok' | 'err', text: string) => {
    const next = { kind, text }
    setNotice(next)
    if (kind === 'ok') setTimeout(() => setNotice(current => current === next ? null : current), 3500)
  }, [])

  const openEdit = useCallback((item: MemoryItem) => {
    setEditing(item)
    setEditContent(item.memory)
  }, [])

  const handleDelete = useCallback((item: MemoryItem) => {
    // 「不再提醒」后直接执行：删除会经云保险库墓碑同步到所有设备。
    if (localStorage.getItem('memory.deleteSkipConfirm') === '1') {
      setDeleteTarget(item)
      void confirmDeleteRef.current?.(item)
      return
    }
    setDeleteDontAsk(false)
    setDeleteTarget(item)
  }, [])

  const handleRefreshImportance = useCallback(async () => {
    if (refreshingImportance) return
    setRefreshingImportance(true)
    try {
      const result = await refreshImportance()
      flash('ok', result.message)
    } catch (error) {
      flash('err', `${t('common.failed')}: ${String(error)}`)
    } finally {
      setRefreshingImportance(false)
    }
  }, [refreshingImportance, refreshImportance, flash, t])

  const handlePinMemory = useCallback(async (memoryId: string, pinned: boolean) => {
    try {
      await setMemoryPinned(memoryId, pinned)
      await handleRefreshImportance()
    } catch (error) {
      flash('err', `${t('common.failed')}: ${String(error)}`)
    }
  }, [setMemoryPinned, handleRefreshImportance, flash, t])

  async function handleStop() {
    await stopEngine()
    flash('ok', t('memory.engineStopped'))
  }

  async function handleSearch() {
    if (!query.trim() || !memoryCacheReady) return
    try {
      await search(query.trim(), topK)
    } catch (e) {
      flash('err', `${t('common.failed')}: ${String(e)}`)
    }
  }

  async function handleRefresh() {
    if (refreshing) return
    setRefreshing(true)
    const results = await Promise.allSettled([
      checkEngine(),
      listMemories(true),
      loadMemoryLayers(true),
      checkIngest(),
      checkTelemetry({ backfill: true, refreshUsage: true, limit: 50 }),
    ])
    setRefreshing(false)
    const failure = results.find((result) => result.status === 'rejected')
    flash(failure ? 'err' : 'ok', failure ? t('memory.refreshFailed') : t('memory.refreshDone'))
  }

  async function handleL2Consolidation() {
    if (consolidatingL2) return
    setL2ConsolidationError(null)
    setL2ConsolidationSuccess(null)
    setConsolidatingL2(true)
    try {
      const result = await consolidateShortTermMemory()
      setL2ConsolidationSuccess(result.message)
      flash('ok', t('memory.l2ConsolidationSucceeded', { message: result.message }))
    } catch (error) {
      const message = errorMessage(error)
      setL2ConsolidationError(message)
      flash('err', t('memory.l2ConsolidationFailed', { error: message }))
    } finally { setConsolidatingL2(false) }
  }

  // L2 编辑保存：直接覆盖当前发布版，下一次注入与 L3 草案立刻生效。
  async function handleL2Save(document: MemoryLayerDocument) {
    if (savingL2 || !editingL2Content.trim()) return
    setSavingL2(true)
    try {
      await updatePublishedMemoryDocument(document.id, editingL2Content)
      setEditingL2(false)
      setEditingL2Content('')
      flash('ok', t('memory.l2SaveToast'))
    } catch (error) { flash('err', `${t('common.failed')}: ${String(error)}`) } finally { setSavingL2(false) }
  }

  // 已发布 L3 Profile 的手动编辑：保存即覆盖当前发布版，立刻用于注入，
  // 并作为下一次「创建草案」的权威基线输入。
  async function handleL3SavePublished(document: MemoryLayerDocument) {
    if (publishingL3 || !editingL3Content.trim()) return
    if (isInvalidL3Draft(editingL3Content)) {
      flash('err', t('memory.l3InvalidSave'))
      return
    }
    setPublishingL3(document.id)
    try {
      await updatePublishedMemoryDocument(document.id, editingL3Content)
      setEditingL3Id(null)
      setEditingL3Content('')
      flash('ok', t('memory.l3UpdatedToast'))
    } catch (error) { flash('err', `${t('common.failed')}: ${String(error)}`) } finally { setPublishingL3(null) }
  }

  async function handleL3Draft() {
    if (draftingL3) return
    setL3DraftError(null)
    setDraftingL3(true)
    try {
      const result = await draftLongTermProfile()
      flash('ok', result.message)
    } catch (error) {
      const message = String(error)
      setL3DraftError(message)
      flash('err', `${t('common.failed')}: ${message}`)
    } finally { setDraftingL3(false) }
  }

  async function handleL3Publish(document: MemoryLayerDocument) {
    const edited = editingL3Id === document.id ? editingL3Content : null
    const effectiveContent = edited ?? document.content
    if (isInvalidL3Draft(effectiveContent)) {
      flash('err', t('memory.l3InvalidPublish'))
      return
    }
    if (publishingL3) return
    setPublishingL3(document.id)
    try {
      // 编辑不单独保存：仅在发布这一刻把编辑后的正文交给后端覆盖发布。
      await publishLongTermProfile(document.id, edited?.trim() || undefined)
      setEditingL3Id(null)
      setEditingL3Content('')
      flash('ok', t('memory.l3PublishedToast'))
    } catch (error) { flash('err', `${t('common.failed')}: ${String(error)}`) } finally { setPublishingL3(null) }
  }

  async function confirmL3DraftDelete() {
    if (!l3DeleteTarget || deletingL3) return
    const document = l3DeleteTarget
    setDeletingL3(document.id)
    try {
      await deleteLongTermProfileDraft(document.id)
      setL3DeleteTarget(null)
      flash('ok', t('memory.l3DraftDeleted'))
    } catch (error) { flash('err', `${t('common.failed')}: ${String(error)}`) } finally { setDeletingL3(null) }
  }

  function requestResetL1() {
    if (!resettingL1) setResetL1ConfirmOpen(true)
  }

  async function confirmResetL1() {
    if (resettingL1) return
    setResettingL1(true)
    try {
      const result = await resetL1ForReextraction()
      setResetL1ConfirmOpen(false)
      flash('ok', t('memory.l1ResetToast', { clearedL1: result.cleared_l1, clearedDerived: result.cleared_derived_documents, requeued: result.requeued_conversations }))
    } catch (error) { flash('err', `${t('common.failed')}: ${String(error)}`) } finally { setResettingL1(false) }
  }

  async function handleOrganizeConversations() {
    if (organizingConversations || ingestStatus?.manual_extraction_running) return
    setOrganizingConversations(true)
    try {
      const result = await organizeConversations()
      flash(result.cancelled ? 'ok' : result.failed ? 'err' : 'ok', result.cancelled
        ? t('memory.organizeConversationsInterrupted', { succeeded: result.succeeded, failed: result.failed })
        : t('memory.organizeDoneToast', { succeeded: result.succeeded, failed: result.failed, reason: result.failure_reasons[0] ? t('memory.organizeDoneReason', { reason: result.failure_reasons[0] }) : '' }))
    } catch (error) { flash('err', `${t('common.failed')}: ${String(error)}`) } finally { setOrganizingConversations(false) }
  }

  async function handleCancelOrganize() {
    try { await cancelOrganize() } catch (error) { flash('err', `${t('common.failed')}: ${String(error)}`) }
  }

  async function handleImportMemoryFolder() {
    if (importingMemories) return
    const selected = await open({ directory: true, multiple: false, title: t('memory.importDialogTitle') })
    if (!selected || Array.isArray(selected)) return
    setImportingMemories(true)
    setMemoryImportResult(null)
    try {
      const result = await importMemoryFolder(selected)
      setMemoryImportResult(result)
      flash('ok', result.message)
    } catch (error) {
      flash('err', `${t('common.failed')}: ${String(error)}`)
    } finally { setImportingMemories(false) }
  }

  async function handleSaveEdit() {
    if (!editing) return
    try {
      await updateMemory(editing.id, editContent)
      flash('ok', t('memory.updated'))
      setEditing(null)
    } catch (e) {
      flash('err', `${t('common.failed')}: ${String(e)}`)
    }
  }

  const userMemoryTypeOptions = [
    { value: 'fact', label: t('memory.userDefinedTypeFact') },
    { value: 'decision', label: t('memory.userDefinedTypeDecision') },
    { value: 'constraint', label: t('memory.userDefinedTypeConstraint') },
    { value: 'preference_candidate', label: t('memory.userDefinedTypePreference') },
    { value: 'open_item', label: t('memory.userDefinedTypeOpenItem') },
  ]

  async function handleAddUserMemory(scope: 'l2' | 'l3') {
    const content = userMemoryContent.trim()
    if (!content || addingUserMemory) return
    setUserMemoryErrors((previous) => ({ ...previous, [scope]: undefined }))
    setUserMemorySuccesses((previous) => ({ ...previous, [scope]: undefined }))
    setAddingUserMemory(true)
    try {
      await addUserMemory(content, userMemoryType, scope)
      setUserMemoryContent('')
      if (scope === 'l2') setL2UserMemoryOpen(false)
      else setUserMemoryOpen(false)
      const message = t(scope === 'l2' ? 'memory.workMemoryAdded' : 'memory.longTermMemoryAdded')
      setUserMemorySuccesses((previous) => ({ ...previous, [scope]: message }))
      flash('ok', message)
    } catch (e) {
      const message = errorMessage(e)
      setUserMemoryErrors((previous) => ({ ...previous, [scope]: message }))
      flash('err', t(scope === 'l2' ? 'memory.workMemoryAddFailed' : 'memory.longTermMemoryAddFailed', { error: message }))
    } finally { setAddingUserMemory(false) }
  }

  async function confirmDelete(item?: MemoryItem) {
    const target = item ?? deleteTarget
    if (!target) return
    try {
      await deleteMemory(target.id)
      if (deleteDontAsk) localStorage.setItem('memory.deleteSkipConfirm', '1')
      flash('ok', t('memory.deleted'))
    } catch (e) {
      flash('err', `${t('common.failed')}: ${String(e)}`)
    } finally {
      setDeleteTarget(null)
    }
  }

  async function handleDreaming() {
    if (consolidating) return
    setConsolidating(true)
    setConsolidationResult(null)
    setConsolidationProgress(t('memory.consolidatePreparing'))
    try {
      const result = await dreaming((processed, total) => {
        setConsolidationProgress(t('memory.consolidateGenerating', { processed, total }))
      })
      await listMemories(true)
      const text = result.message || t('memory.dreamDone')
      setConsolidationResult({ kind: 'ok', text, result })
      flash('ok', t('memory.dreamDone'))
    } catch (e) {
      const text = `${t('common.failed')}: ${String(e)}`
      setConsolidationResult({ kind: 'err', text })
      flash('err', text)
    } finally {
      setConsolidating(false)
      setConsolidationProgress(null)
    }
  }

  async function handleRestoreConsolidation() {
    const snapshotId = consolidationResult?.result?.snapshot_id
    if (!snapshotId || restoringConsolidation) return
    setRestoringConsolidation(true)
    try {
      const result = await restoreConsolidation(snapshotId)
      await listMemories(true)
      setConsolidationResult({ kind: 'ok', text: t('memory.dreamRestoreDone'), result })
      flash('ok', t('memory.dreamRestoreDone'))
    } catch (error) {
      const text = `${t('common.failed')}: ${String(error)}`
      setConsolidationResult({ kind: 'err', text })
      flash('err', text)
    } finally {
      setRestoringConsolidation(false)
    }
  }

  const engineState = engineOnline === null ? 'unknown' : engineOnline ? 'online' : 'offline'

  // 记忆注入入口卡只显示已开启/已关闭：任一座椅注入 Hook 事件齐备即视为已开启，
  // 判定与「记忆注入」页共用（见 memoryAgentStatus）。
  const hookStatusByAgent: Record<string, HookStatus | null> = {
    codex: codexHookStatus, claude: hookStatus, qoder: qoderHookStatus, workbuddy: workbuddyHookStatus,
    gemini: geminiHookStatus, antigravity: antigravityHookStatus,
  }
  const injectionReadyCount = agentSources
    .filter((source) => source.supports_hooks && isInjectionReady(hookStatusByAgent[source.id] ?? null))
    .length
  // 概览卡的公共样式：主数字 text-xl，四周留白 px-4 / py-3，标题与主数字之间
  // 再用 mt-2 撑开（原本紧贴为 0px）。悬停色统一为 violet，与「待提取记忆」一致。
  const overviewCardClass = 'group rounded-lg border border-gray-200 bg-white px-4 py-3 text-left transition hover:border-violet-300 hover:bg-violet-50/40 focus:outline-none focus-visible:ring-2 focus-visible:ring-violet-500 dark:border-gray-700 dark:bg-gray-800 dark:hover:border-violet-500/60 dark:hover:bg-violet-500/10'
  const overviewLabelClass = 'text-xs text-gray-500'
  const overviewValueClass = 'mt-2 text-xl font-semibold leading-tight text-gray-800 dark:text-gray-100'

  // 实时运行状态：优先展示活跃会话所属的 Agent / 来源；无法识别时回退为纯会话数。
  const activeSourceNames = (telemetryLiveStatus?.active_sources ?? []).map(sourceLabel)
  const liveRunText = telemetryLiveStatus?.active_sessions
    ? activeSourceNames.length === 0
      ? t('memory.liveRunActive', { count: telemetryLiveStatus.active_sessions })
      : telemetryLiveStatus.active_sessions === 1 && activeSourceNames.length === 1
        ? t('memory.liveRunSingle', { name: activeSourceNames[0] })
        : t('memory.liveRunMulti', { count: telemetryLiveStatus.active_sessions, names: activeSourceNames.join(t('memory.liveRunJoiner')) })
    : t('memory.liveRunIdle', { count: telemetryLiveStatus?.captured_sessions ?? 0 })

  return (
    <div className="flex h-full flex-col gap-4 overflow-y-auto bg-[var(--app-canvas)] px-[var(--app-page-gutter)] py-4">
      {/* 本地记忆与智能检索始终可用；旧语义侧车仅作为可选高级能力。 */}
      <div className="fluent-page-header flex flex-wrap items-center gap-3 rounded-xl">
        <div className="p-2 rounded-lg bg-violet-500/10 text-violet-600 dark:text-violet-400">
          <Brain size={20} />
        </div>
        <div>
          <h1 className="font-semibold text-gray-800 dark:text-gray-100">{t('memory.title')}</h1>
          <p className="text-xs text-gray-500 dark:text-gray-400">{t('memory.subtitle')}</p>
        </div>
        <div className="ml-auto flex flex-wrap items-center justify-end gap-1.5">
          {notice && (
            <span className={`text-xs px-2 py-1 rounded ${notice.kind === 'ok' ? 'bg-green-500/10 text-green-600 dark:text-green-400' : 'bg-red-500/10 text-red-600 dark:text-red-400'}`}>
              {notice.text}
            </span>
          )}
          {onOpenGraph && (
            <button
              type="button"
              onClick={onOpenGraph}
              className="inline-flex items-center gap-1.5 rounded-full border border-violet-500/40 bg-violet-500/10 px-2.5 py-1 text-xs font-medium text-violet-700 transition hover:bg-violet-500/20 dark:text-violet-300"
            >
              <Brain size={13} />
              {t('nav.memoryGraph')}
            </button>
          )}
          <span className={`inline-flex items-center gap-1.5 rounded-full px-2.5 py-1 text-xs ${telemetryLiveStatus?.active_sessions ? 'bg-sky-500/10 text-sky-700 dark:text-sky-300' : 'bg-gray-100 text-gray-600 dark:bg-gray-800 dark:text-gray-300'}`} title={t('memory.memoryStatusTitle')}><span className={`h-1.5 w-1.5 rounded-full ${telemetryLiveStatus?.active_sessions ? 'animate-pulse bg-sky-500 motion-reduce:animate-none' : 'bg-gray-400'}`} />{t('memory.liveRunLabel')}{liveRunText}</span>
          <span className="inline-flex items-center gap-1.5 rounded-full border border-emerald-500/35 px-2.5 py-1 text-xs text-emerald-700 dark:text-emerald-300" title={t('memory.memoryStatusTitle')}><Database size={13} />{t('memory.memoryStatusLocal')}</span>
          <span className={`inline-flex items-center gap-1.5 rounded-full px-2.5 py-1 text-xs ${telemetryLiveStatus?.pending_memory_sessions ? 'bg-violet-500/10 text-violet-700 dark:text-violet-300' : telemetryLiveStatus?.retrying_memory_sessions || telemetryLiveStatus?.failed_memory_sessions || telemetryLiveStatus?.failed_transcript_scans ? 'bg-amber-500/10 text-amber-700 dark:text-amber-300' : 'bg-gray-100 text-gray-600 dark:text-gray-300'}`} title={t('memory.syncStatusTitle')}><Brain size={13} />{t('memory.syncLabel')}{telemetryLiveStatus?.pending_memory_sessions ? t('memory.syncPending', { count: telemetryLiveStatus.pending_memory_sessions }) : telemetryLiveStatus?.failed_memory_sessions ? t('memory.syncFailed', { count: telemetryLiveStatus.failed_memory_sessions }) : telemetryLiveStatus?.retrying_memory_sessions ? t('memory.syncRetrying', { count: telemetryLiveStatus.retrying_memory_sessions }) : telemetryLiveStatus?.failed_transcript_scans ? t('memory.syncScanFailed', { count: telemetryLiveStatus.failed_transcript_scans }) : t('memory.syncDone')}</span>
          {engineState === 'online' && (
              <button
                onClick={handleStop}
                className="inline-flex items-center gap-1 px-2.5 py-1 rounded-md border border-gray-300 dark:border-gray-600 text-xs text-gray-600 dark:text-gray-300 hover:bg-gray-50 dark:hover:bg-gray-700"
              >
                <PowerOff size={12} />{t('memory.engineStop')}
              </button>
            )}
        </div>
      </div>

      {telemetrySummary && (
        <section className="grid grid-cols-2 lg:grid-cols-5 gap-3">
          <button type="button" onClick={onOpenUsage} className={overviewCardClass}>
            <div className="flex items-center justify-between gap-2"><p className={overviewLabelClass}>{t('memory.overviewTokenUsage')}</p><ChevronRight size={14} className="text-gray-300 transition group-hover:translate-x-0.5 group-hover:text-violet-500 dark:text-gray-400 dark:group-hover:text-violet-300" /></div><p className={`${overviewValueClass} tabular-nums`} title={telemetrySummary.total_tokens.toLocaleString()}>{compactTokenCount(telemetrySummary.total_tokens)}</p>
          </button>
          <button type="button" onClick={onOpenOrganized} className={overviewCardClass}>
            <div className="flex items-center justify-between gap-2"><p className={overviewLabelClass}>{t('memory.overviewOrganized')}</p><ChevronRight size={14} className="text-gray-300 transition group-hover:translate-x-0.5 group-hover:text-violet-500 dark:text-gray-400 dark:group-hover:text-violet-300" /></div><p className={`${overviewValueClass} tabular-nums`}>{telemetryLiveStatus?.organized_memory_conversations ?? 0}</p>
          </button>
          <button type="button" onClick={onOpenPending} className={overviewCardClass}>
            <div className="flex items-center justify-between gap-2"><p className={overviewLabelClass}>{t('memory.overviewPending')}</p><ChevronRight size={14} className="text-gray-300 transition group-hover:translate-x-0.5 group-hover:text-violet-500 dark:text-gray-400 dark:group-hover:text-violet-300" /></div><p className={`${overviewValueClass} tabular-nums`}>{(telemetryLiveStatus?.pending_memory_sessions ?? 0) + (telemetryLiveStatus?.retrying_memory_sessions ?? 0) + (telemetryLiveStatus?.failed_memory_sessions ?? 0)}</p>
          </button>
          <button type="button" onClick={onOpenCapture} className={overviewCardClass}>
            <div className="flex items-center justify-between gap-2"><p className={overviewLabelClass}>{t('memory.overviewCapture')}</p><ChevronRight size={14} className="text-gray-300 transition group-hover:translate-x-0.5 group-hover:text-violet-500" /></div><p className={overviewValueClass}>{ingestStatus ? (ingestStatus.enabled ? t('memory.ingestOn') : t('memory.ingestOff')) : t('memory.overviewLoading')}</p>
          </button>
          <button type="button" onClick={onOpenInjection} className={overviewCardClass}>
            <div className="flex items-center justify-between gap-2"><p className={overviewLabelClass}>{t('memory.overviewInjection')}</p><ChevronRight size={14} className="text-gray-300 transition group-hover:translate-x-0.5 group-hover:text-violet-500 dark:text-gray-400 dark:group-hover:text-violet-300" /></div><p className={overviewValueClass}>{injectionReadyCount > 0 ? t('memory.ingestOn') : t('memory.ingestOff')}</p>
          </button>
        </section>
      )}
      {!telemetrySummary && (
        <section className="grid grid-cols-2 lg:grid-cols-5 gap-3" aria-live="polite">
          {[t('memory.overviewTokenUsage'), t('memory.overviewOrganized'), t('memory.overviewPending'), t('memory.overviewCapture'), t('memory.overviewInjection')].map((label) => {
            const handler = label === t('memory.overviewTokenUsage') ? onOpenUsage : label === t('memory.overviewPending') ? onOpenPending : label === t('memory.overviewOrganized') ? onOpenOrganized : label === t('memory.overviewCapture') ? onOpenCapture : onOpenInjection
            return <button key={label} type="button" onClick={handler} className={overviewCardClass}><div className="flex items-center justify-between gap-2"><p className={overviewLabelClass}>{label}</p><ChevronRight size={14} className="text-gray-300 transition group-hover:translate-x-0.5 group-hover:text-violet-500 dark:text-gray-400 dark:group-hover:text-violet-300" /></div><p className="mt-2 inline-flex items-center gap-1.5 text-xs text-gray-500 dark:text-gray-400"><Loader2 className="animate-spin motion-reduce:animate-none" size={13} />{t('memory.overviewLoading')}</p></button>
          })}
        </section>
      )}

      {notice?.kind === 'err' && <ErrorRecovery error={notice.text} />}
      {ingestStatus && !ingestStatus.model_ready && (
        <div className="rounded-lg bg-amber-500/10 px-3 py-2 text-xs text-amber-800 dark:text-amber-200">
          <p>{t('memory.modelMissing')}</p>
          <ErrorRecovery fallback="settings" />
        </div>
      )}

      {/* 采集设置在记忆采集页集中管理。 */}
      <section className="order-10 rounded-xl border border-gray-200 dark:border-gray-700 bg-white dark:bg-gray-800 p-4 space-y-3">
        <div className="flex items-center gap-2">
          <Webhook size={15} className="text-violet-600 dark:text-violet-400" />
          <div className="min-w-0 flex-1"><h2 className="text-sm font-semibold text-gray-700 dark:text-gray-200">{t('memory.ingestTitle')}</h2><p className="mt-0.5 text-xs text-gray-500">{t('memory.ingestHint')}{t('memory.ingestPanelLinkBefore')}<button type="button" onClick={onOpenCapture} className="mx-0.5 inline-flex items-center gap-0.5 font-medium text-violet-600 hover:underline dark:text-violet-300">{t('memory.ingestPanelLinkText')}<ChevronRight size={12} /></button>{t('memory.ingestPanelLinkAfter')}</p></div>
          <div className="flex shrink-0 items-center gap-2">
            {!ingestStatus
              ? <span className="text-xs text-gray-500">{t('memory.overviewLoading')}</span>
              : ingestStatus.enabled
              ? <span className="inline-flex items-center gap-1 text-xs text-green-600 dark:text-green-400"><CheckCircle2 size={12} />{t('memory.ingestOn')}</span>
              : <span className="inline-flex items-center gap-1 text-xs text-gray-500"><XCircle size={12} />{t('memory.ingestOff')}</span>}
            <button type="button" onClick={() => setIngestOpen((open) => !open)} aria-expanded={ingestOpen} className="inline-flex items-center rounded-md p-1 text-gray-500 hover:bg-gray-100 dark:text-gray-400 dark:hover:bg-gray-700" title={ingestOpen ? t('memory.ingestCollapse') : t('memory.ingestExpand')}>{ingestOpen ? <ChevronUp size={15} /> : <ChevronDown size={15} />}</button>
          </div>
        </div>

        {ingestOpen && <div className="space-y-3 border-t border-gray-100 pt-3 dark:border-gray-700">
        <div className={`flex items-center gap-1.5 text-xs ${ingestStatus?.model_ready ? 'text-green-700 dark:text-green-400' : 'text-amber-800 dark:text-amber-300'}`}>
          {ingestStatus?.model_ready ? <CheckCircle2 size={13} /> : <XCircle size={13} />}
          {ingestStatus?.model_ready
            ? t('memory.modelReady', { provider: ingestStatus.model_provider_id })
            : t('memory.modelMissing')}
        </div>

        <div className="flex flex-wrap items-center gap-2 rounded-lg border border-gray-200 bg-gray-50 px-3 py-2.5 dark:border-gray-700 dark:bg-gray-900/40">
          <FileText size={14} className="text-violet-600 dark:text-violet-400" />
          <div className="min-w-0 flex-1"><p className="text-xs font-medium text-gray-700 dark:text-gray-200">{t('memory.organizeConversationsTitle')}</p><p className="mt-0.5 text-xs text-gray-500 dark:text-gray-400">{t('memory.organizeConversationsHint')}</p></div>
          <button onClick={() => { void handleOrganizeConversations() }} disabled={organizingConversations || ingestStatus?.manual_extraction_running || !ingestStatus?.model_ready} className="inline-flex items-center gap-1.5 rounded-md bg-violet-600 px-2.5 py-1.5 text-xs font-medium text-white hover:bg-violet-500 disabled:cursor-not-allowed disabled:opacity-50">{organizingConversations || ingestStatus?.manual_extraction_running ? <Loader2 size={13} className="animate-spin" /> : <RefreshCw size={13} />}{organizingConversations || ingestStatus?.manual_extraction_running ? t('memory.organizeConversationsWorking') : t('memory.organizeConversationsAction')}</button>
          {ingestStatus?.manual_extraction_running && <button type="button" onClick={() => { void handleCancelOrganize() }} disabled={ingestStatus?.manual_extraction_cancelling} className="inline-flex items-center gap-1 rounded-md border border-red-300 px-2.5 py-1.5 text-xs font-medium text-red-700 hover:bg-red-50 disabled:opacity-50 dark:border-red-700 dark:text-red-300 dark:hover:bg-red-950/40"><X size={13} />{ingestStatus?.manual_extraction_cancelling ? t('memory.pending.interrupting') : t('memory.pending.interrupt')}</button>}
        </div>

        <div className="flex flex-wrap items-center gap-2 rounded-lg border border-gray-200 bg-gray-50 px-3 py-2.5 dark:border-gray-700 dark:bg-gray-900/40">
          <FolderOpen size={14} className="text-violet-600 dark:text-violet-400" />
          <div className="min-w-0 flex-1"><p className="text-xs font-medium text-gray-700 dark:text-gray-200">{t('memory.importMemoryTitle')}</p><p className="mt-0.5 text-xs text-gray-500 dark:text-gray-400">{t('memory.importMemoryHint')}</p></div>
          <button onClick={() => { void handleImportMemoryFolder() }} disabled={importingMemories || !ingestStatus?.model_ready} className="inline-flex items-center gap-1.5 rounded-md border border-violet-200 bg-white px-2.5 py-1.5 text-xs font-medium text-violet-700 hover:bg-violet-50 disabled:cursor-not-allowed disabled:opacity-50 dark:border-violet-500/30 dark:bg-gray-800 dark:text-violet-300 dark:hover:bg-violet-500/10">{importingMemories ? <Loader2 size={13} className="animate-spin" /> : <FolderOpen size={13} />}{importingMemories ? t('memory.importMemoryWorking') : t('memory.importMemoryButton')}</button>
          {memoryImportResult && <p className="basis-full text-xs text-green-700 dark:text-green-300" role="status">{t('memory.importResult', { scanned: memoryImportResult.scanned_files, recognized: memoryImportResult.recognized_files, imported: memoryImportResult.imported_memories, skipped: memoryImportResult.skipped_files > 0 ? t('memory.importResultSkipped', { count: memoryImportResult.skipped_files }) : '' })}</p>}
        </div>

        {ingestStatus && ingestStatus.buffered_sessions > 0 && (
          <p className="rounded-md bg-amber-500/10 px-2.5 py-2 text-xs text-amber-800 dark:text-amber-200">
            {t(ingestStatus.model_ready ? 'memory.ingestBuffering' : 'memory.ingestModelUnavailable', { count: ingestStatus.buffered_sessions })}
          </p>
        )}

        </div>}

      </section>

      {/* L2 / L3 层级记忆：明确与 L1 清洗分开，只有已发布 L3 才会注入 Agent。 */}
      <section className="order-30 grid auto-rows-fr items-stretch gap-3 xl:grid-cols-2">
        <article className="min-w-0 max-h-[36rem] overflow-y-auto overscroll-contain rounded-xl border border-sky-200 bg-sky-50/40 p-4 dark:border-sky-900/70 dark:bg-sky-950/20">
          <div className="flex items-start gap-2">
            <div className="mt-0.5 rounded-md bg-sky-500/10 p-1.5 text-sky-600 dark:text-sky-300"><FileText size={14} /></div>
            <div className="min-w-0 flex-1 min-h-[62px]"><h2 className="text-sm font-semibold text-gray-800 dark:text-gray-100">{t('memory.l2Title')}</h2><p className="mt-0.5 text-xs leading-5 text-gray-500 dark:text-gray-400 line-clamp-2" title={t('memory.l2Hint')}>{t('memory.l2Hint')}</p></div>
            <button onClick={() => { void handleL2Consolidation() }} disabled={consolidatingL2} className="inline-flex shrink-0 items-center gap-1.5 rounded-md bg-sky-600 px-2.5 py-1.5 text-xs font-medium text-white hover:bg-sky-500 disabled:opacity-50">{consolidatingL2 ? <Loader2 size={13} className="animate-spin" /> : <RefreshCw size={13} />}{consolidatingL2 ? t('memory.l2Generating') : t('memory.l2Generate')}</button>
          </div>
          {l2ConsolidationError && <InlineActionError text={t('memory.l2ConsolidationFailed', { error: l2ConsolidationError })} />}
          {l2ConsolidationSuccess && <InlineActionSuccess text={t('memory.l2ConsolidationSucceeded', { message: l2ConsolidationSuccess })} />}
          {/* 用户自定义添加：与右侧 L3 卡片同位置（操作按钮底下），共用同一份
              表单内容与提交状态；写入的条目会在 L2 巩固时强制并入证据。 */}
          <div className="mt-3 rounded-md border border-sky-200 bg-white/70 px-3 py-2.5 dark:border-sky-900/70 dark:bg-gray-900/30">
            <button
              type="button"
              onClick={() => setL2UserMemoryOpen((open) => !open)}
              aria-expanded={l2UserMemoryOpen}
              className="inline-flex items-center gap-1.5 text-sm font-medium text-sky-700 dark:text-sky-300"
              title={t('memory.userDefinedPlaceholder')}
            >
              <Plus size={15} />
              {t('memory.userDefinedAdd')}
              {l2UserMemoryOpen ? <ChevronUp size={14} /> : <ChevronDown size={14} />}
            </button>
            {l2UserMemoryOpen && (
              <div className="mt-2 space-y-2">
                <textarea
                  value={userMemoryContent}
                  onChange={(e) => setUserMemoryContent(e.target.value)}
                  placeholder={t('memory.userDefinedPlaceholder')}
                  rows={3}
                  className="w-full rounded-md border border-gray-300 dark:border-gray-600 bg-white dark:bg-gray-700 px-2.5 py-1.5 text-sm text-gray-800 dark:text-gray-100 focus:outline-none focus:ring-1 focus:ring-sky-500"
                />
                <div className="flex items-center gap-2">
                  <select
                    value={userMemoryType}
                    onChange={(e) => setUserMemoryType(e.target.value)}
                    className="rounded-md border border-gray-300 dark:border-gray-600 bg-white dark:bg-gray-700 px-2 py-1.5 text-sm text-gray-800 dark:text-gray-100 focus:outline-none"
                  >
                    {userMemoryTypeOptions.map((option) => <option key={option.value} value={option.value}>{option.label}</option>)}
                  </select>
                  <button
                    type="button"
                    onClick={() => { void handleAddUserMemory('l2') }}
                    disabled={addingUserMemory || !userMemoryContent.trim()}
                    className="inline-flex items-center gap-1.5 px-3 py-1.5 rounded-md bg-sky-600 hover:bg-sky-500 disabled:opacity-50 text-white text-sm font-medium"
                  >
                    {addingUserMemory ? <Loader2 size={15} className="animate-spin" /> : <Plus size={15} />}
                    {t('memory.userDefinedAdd')}
                  </button>
                </div>
              </div>
            )}
          </div>
          {userMemoryErrors.l2 && <InlineActionError text={t('memory.workMemoryAddFailed', { error: userMemoryErrors.l2 })} />}
          {userMemorySuccesses.l2 && <InlineActionSuccess text={userMemorySuccesses.l2} />}
          <MemorySectionStatus status={memoryLoadState.l2} onRetry={() => { void loadMemoryLayers(true).catch(() => {}) }} />
          <UserMemoryList accent="sky" scope="l2" />
          {l2Documents.find((document) => document.state === 'published') ? (() => {
            const document = l2Documents.find((item) => item.state === 'published')!
            const content = normalizeL2Document(document.content)
            const repairedLegacyContent = content !== document.content.trim()
            const hasAnalysisPreamble = looksLikeModelAnalysis(content)
            return <div className="mt-3 flex min-h-0 flex-1 flex-col border-t border-sky-200/70 pt-3 dark:border-sky-900/70">
              {/* 已发布区块与右侧 L3 文档行同构：同一套底纹卡片 +
                  绿色状态标签 + 灰色元信息行，两卡格式统一。
                  content-visibility：滚出视口时跳过子树布局与绘制，
                  降低滚动成本。 */}
              <div className="min-w-0 flex-1 rounded-md bg-white/70 px-2.5 py-2 dark:bg-gray-900/30 [content-visibility:auto] [contain-intrinsic-size:auto_320px]">
                <div className="flex flex-wrap items-center gap-x-2 gap-y-1 text-xs">
                  <span className="text-green-600 dark:text-green-300">{t('memory.l2Published')}</span>
                  <span className="min-w-0 text-gray-500 dark:text-gray-400">{t('memory.l2SourceLine', { count: document.source_count, tokens: document.token_estimate, time: localTime(document.created_at) })}</span>
                  <LayerWindow document={document} />
                  <button type="button" onClick={() => { setEditingL2(true); setEditingL2Content(content) }} disabled={savingL2 || consolidatingL2} className="ml-auto inline-flex shrink-0 items-center gap-1 rounded border border-gray-300 px-2 py-0.5 font-medium text-gray-700 hover:bg-gray-100 disabled:opacity-50 dark:border-gray-600 dark:text-gray-200 dark:hover:bg-gray-700"><Pencil size={12} />{t('memory.l2Edit')}</button>
                </div>
              {editingL2 ? (
                <div className="mt-2 space-y-1.5">
                  <textarea
                    value={editingL2Content}
                    onChange={(e) => setEditingL2Content(e.target.value)}
                    rows={12}
                    className="w-full rounded-md border border-gray-300 dark:border-gray-600 bg-white dark:bg-gray-700 px-2.5 py-2 font-mono text-xs leading-5 text-gray-800 dark:text-gray-100 focus:outline-none focus:ring-1 focus:ring-sky-500"
                  />
                  <div className="flex items-center gap-2">
                    <button type="button" onClick={() => { void handleL2Save(document) }} disabled={savingL2 || !editingL2Content.trim()} className="inline-flex items-center gap-1 rounded border border-sky-300 px-2 py-0.5 text-xs font-medium text-sky-700 hover:bg-sky-100 disabled:opacity-50 dark:border-sky-700 dark:text-sky-200">{savingL2 ? <Loader2 size={12} className="animate-spin" /> : null}{savingL2 ? t('memory.l3Saving') : t('memory.l3Save')}</button>
                    <button type="button" onClick={() => { setEditingL2(false); setEditingL2Content('') }} disabled={savingL2} className="rounded border border-gray-300 px-2 py-0.5 text-xs font-medium text-gray-700 hover:bg-gray-100 disabled:opacity-50 dark:border-gray-600 dark:text-gray-200 dark:hover:bg-gray-700">{t('memory.l3Cancel')}</button>
                    <p className="text-xs text-gray-500 dark:text-gray-400">{t('memory.l2SaveOverwriteHint')}</p>
                  </div>
                </div>
              ) : (<>
                {repairedLegacyContent && <p className="mt-2 rounded-md border border-sky-200 bg-sky-100/60 px-2.5 py-2 text-xs leading-5 text-sky-800 dark:border-sky-900/70 dark:bg-sky-950/30 dark:text-sky-200">{t('memory.l2LegacyHidden')}</p>}
                {hasAnalysisPreamble && <p className="mt-2 rounded-md border border-amber-200 bg-amber-50 px-2.5 py-2 text-xs leading-5 text-amber-800 dark:border-amber-900/70 dark:bg-amber-950/30 dark:text-amber-200">{t('memory.l2PreambleWarn')}</p>}
                <div id="l2-published-content" role="region" aria-label={t('memory.l2Title')} tabIndex={0} className={`mt-2 ${MEMORY_CONTENT_SCROLL_CLASS}`}>
                  <MemoryMarkdown content={content} unwrapDocumentFence />
                </div>
              </>)}
              </div>
            </div>
          })() : memoryLoadState.l2 === 'ready' && <p className="mt-3 text-xs text-gray-500 dark:text-gray-400">{t('memory.l2Empty')}</p>}
        </article>
        <article className="min-w-0 max-h-[36rem] overflow-y-auto overscroll-contain rounded-xl border border-violet-200 bg-violet-50/40 p-4 dark:border-violet-900/70 dark:bg-violet-950/20">
          <div className="flex items-start gap-2">
            <div className="mt-0.5 rounded-md bg-violet-500/10 p-1.5 text-violet-600 dark:text-violet-300"><Brain size={14} /></div>
            <div className="min-w-0 flex-1 min-h-[62px]"><h2 className="text-sm font-semibold text-gray-800 dark:text-gray-100">{t('memory.l3Title')}</h2><p className="mt-0.5 text-xs leading-5 text-gray-500 dark:text-gray-400 line-clamp-2" title={t('memory.l3Hint')}>{t('memory.l3Hint')}</p></div>
            <button onClick={() => { void handleL3Draft() }} disabled={draftingL3 || !l2Documents.some((document) => document.state === 'published')} className="inline-flex shrink-0 items-center gap-1.5 rounded-md bg-violet-600 px-2.5 py-1.5 text-xs font-medium text-white hover:bg-violet-500 disabled:opacity-50">{draftingL3 ? <Loader2 size={13} className="animate-spin" /> : <Pencil size={13} />}{draftingL3 ? t('memory.l3Drafting') : t('memory.l3CreateDraft')}</button>
          </div>
          {/* 用户自定义添加：放在「创建草案」底下。写入 user_defined 长期
              记忆，自动整理不会删改，并随 L2/L3 整理与注入一起下发给 Agent。 */}
          <div className="mt-3 rounded-md border border-violet-200 bg-white/70 px-3 py-2.5 dark:border-violet-900/70 dark:bg-gray-900/30">
            <button
              type="button"
              onClick={() => setUserMemoryOpen((open) => !open)}
              aria-expanded={userMemoryOpen}
              className="inline-flex items-center gap-1.5 text-sm font-medium text-violet-700 dark:text-violet-300"
              title={t('memory.userDefinedPlaceholder')}
            >
              <Plus size={15} />
              {t('memory.userDefinedAdd')}
              {userMemoryOpen ? <ChevronUp size={14} /> : <ChevronDown size={14} />}
            </button>
            {userMemoryOpen && (
              <div className="mt-2 space-y-2">
                <textarea
                  value={userMemoryContent}
                  onChange={(e) => setUserMemoryContent(e.target.value)}
                  placeholder={t('memory.userDefinedPlaceholder')}
                  rows={3}
                  className="w-full rounded-md border border-gray-300 dark:border-gray-600 bg-white dark:bg-gray-700 px-2.5 py-1.5 text-sm text-gray-800 dark:text-gray-100 focus:outline-none focus:ring-1 focus:ring-violet-500"
                />
                <div className="flex items-center gap-2">
                  <select
                    value={userMemoryType}
                    onChange={(e) => setUserMemoryType(e.target.value)}
                    className="rounded-md border border-gray-300 dark:border-gray-600 bg-white dark:bg-gray-700 px-2 py-1.5 text-sm text-gray-800 dark:text-gray-100 focus:outline-none"
                  >
                    {userMemoryTypeOptions.map((option) => <option key={option.value} value={option.value}>{option.label}</option>)}
                  </select>
                  <button
                    type="button"
                    onClick={() => { void handleAddUserMemory('l3') }}
                    disabled={addingUserMemory || !userMemoryContent.trim()}
                    className="inline-flex items-center gap-1.5 px-3 py-1.5 rounded-md bg-violet-600 hover:bg-violet-500 disabled:opacity-50 text-white text-sm font-medium"
                  >
                    {addingUserMemory ? <Loader2 size={15} className="animate-spin" /> : <Plus size={15} />}
                    {t('memory.userDefinedAdd')}
                  </button>
                </div>
              </div>
            )}
          </div>
          {userMemoryErrors.l3 && <InlineActionError text={t('memory.longTermMemoryAddFailed', { error: userMemoryErrors.l3 })} />}
          {userMemorySuccesses.l3 && <InlineActionSuccess text={userMemorySuccesses.l3} />}
         {l3DraftError && <div role="alert" className="mt-3 rounded-md border border-red-200 bg-red-50 px-2.5 py-2 text-xs leading-5 text-red-800 dark:border-red-900/70 dark:bg-red-950/30 dark:text-red-200"><span className="font-medium">{t('memory.l3DraftFailedTitle')}</span>{l3DraftError}</div>}
        <MemorySectionStatus status={memoryLoadState.l3} onRetry={() => { void loadMemoryLayers(true).catch(() => {}) }} />
        <div id="l3-documents-content" role="region" aria-label={t('memory.l3Title')} tabIndex={0} className={`mt-3 space-y-2 border-t border-violet-200/70 pt-3 dark:border-violet-900/70 ${MEMORY_CONTENT_SCROLL_CLASS}`}><UserMemoryList accent="violet" scope="l3" />{visibleL3Documents.slice(0, 2).map((document) => {
          return <div key={document.id} className="min-w-0 rounded-md bg-white/70 px-2.5 py-2 dark:bg-gray-900/30 [content-visibility:auto] [contain-intrinsic-size:auto_320px]">
              <div className="flex flex-wrap items-center gap-x-2 gap-y-1 text-xs">
                <span className={document.state === 'published' ? 'text-green-600 dark:text-green-300' : document.state === 'archived' ? 'text-gray-500 dark:text-gray-400' : 'text-amber-700 dark:text-amber-300'}>{document.state === 'published' ? t('memory.l3Published') : document.state === 'archived' ? t('memory.l3Archived') : t('memory.l3Draft')}</span>
                <span className="min-w-0 text-gray-500 dark:text-gray-400">{t('memory.l3SourceLine', { count: document.source_count, tokens: document.token_estimate })}</span>
                {document.state === 'draft' && <div className="ml-auto flex shrink-0 items-center gap-1"><button type="button" onClick={() => { setEditingL3Id(document.id); setEditingL3Content(document.content) }} disabled={publishingL3 === document.id || deletingL3 === document.id} className="inline-flex items-center gap-1 rounded border border-gray-300 px-2 py-0.5 font-medium text-gray-700 hover:bg-gray-100 disabled:opacity-50 dark:border-gray-600 dark:text-gray-200 dark:hover:bg-gray-700"><Pencil size={12} />{t('memory.l2Edit')}</button><button onClick={() => { void handleL3Publish(document) }} disabled={publishingL3 === document.id || deletingL3 === document.id} className="rounded border border-violet-300 px-2 py-0.5 font-medium text-violet-700 hover:bg-violet-100 disabled:opacity-50 dark:border-violet-700 dark:text-violet-200">{publishingL3 === document.id ? t('memory.l3Publishing') : t('memory.l3ConfirmPublish')}</button><button type="button" onClick={() => setL3DeleteTarget(document)} disabled={publishingL3 === document.id || deletingL3 === document.id} className="inline-flex items-center gap-1 rounded border border-red-200 px-2 py-0.5 font-medium text-red-700 hover:bg-red-50 disabled:opacity-50 dark:border-red-900/70 dark:text-red-300 dark:hover:bg-red-950/30"><Trash2 size={12} />{t('memory.l3DeleteDraft')}</button></div>}
                {document.state === 'published' && <div className="ml-auto flex shrink-0 items-center gap-1"><button type="button" onClick={() => { setEditingL3Id(document.id); setEditingL3Content(document.content) }} disabled={publishingL3 === document.id} className="inline-flex items-center gap-1 rounded border border-gray-300 px-2 py-0.5 font-medium text-gray-700 hover:bg-gray-100 disabled:opacity-50 dark:border-gray-600 dark:text-gray-200 dark:hover:bg-gray-700"><Pencil size={12} />{t('memory.l2Edit')}</button></div>}
                {document.state === 'archived' && <div className="ml-auto flex shrink-0 items-center gap-1"><button type="button" onClick={() => setL3DeleteTarget(document)} disabled={deletingL3 === document.id} className="inline-flex items-center gap-1 rounded border border-red-200 px-2 py-0.5 font-medium text-red-700 hover:bg-red-50 disabled:opacity-50 dark:border-red-900/70 dark:text-red-300 dark:hover:bg-red-950/30"><Trash2 size={12} />{t('memory.l3DeleteArchived')}</button></div>}
              </div>
              {editingL3Id === document.id ? (
                <div className="mt-2 space-y-1.5">
                  <textarea
                    value={editingL3Content}
                    onChange={(e) => setEditingL3Content(e.target.value)}
                    rows={12}
                    className="w-full rounded-md border border-gray-300 dark:border-gray-600 bg-white dark:bg-gray-700 px-2.5 py-2 font-mono text-xs leading-5 text-gray-800 dark:text-gray-100 focus:outline-none focus:ring-1 focus:ring-violet-500"
                  />
                  <div className="flex items-center gap-2">
                    {document.state === 'published' ? (
                      <button type="button" onClick={() => { void handleL3SavePublished(document) }} disabled={publishingL3 === document.id || !editingL3Content.trim()} className="inline-flex items-center gap-1 rounded border border-violet-300 px-2 py-0.5 text-xs font-medium text-violet-700 hover:bg-violet-100 disabled:opacity-50 dark:border-violet-700 dark:text-violet-200">{publishingL3 === document.id ? <Loader2 size={12} className="animate-spin" /> : null}{publishingL3 === document.id ? t('memory.l3Saving') : t('memory.l3Save')}</button>
                    ) : null}
                    <button type="button" onClick={() => { setEditingL3Id(null); setEditingL3Content('') }} disabled={publishingL3 === document.id} className="rounded border border-gray-300 px-2 py-0.5 text-xs font-medium text-gray-700 hover:bg-gray-100 disabled:opacity-50 dark:border-gray-600 dark:text-gray-200 dark:hover:bg-gray-700">{document.state === 'published' ? t('memory.l3Cancel') : t('memory.l3CancelEdit')}</button>
                    <p className="text-xs text-gray-500 dark:text-gray-400">{document.state === 'published' ? t('memory.l3PublishedEditHint') : t('memory.l3DraftEditHint')}</p>
                  </div>
                </div>
              ) : (
                <div id={`l3-document-${document.id}`} className="mt-2 min-w-0">
                  <MemoryMarkdown content={document.content} unwrapDocumentFence />
                </div>
              )}
            </div>
          })}{invalidL3Drafts.map((document) => <div key={document.id} className="rounded-md border border-amber-200 bg-amber-50/80 px-2.5 py-2 text-xs dark:border-amber-900/70 dark:bg-amber-950/25"><div className="flex flex-wrap items-center gap-x-2 gap-y-1"><span className="font-medium text-amber-800 dark:text-amber-200">{t('memory.l3InvalidDraftBadge')}</span><span className="text-amber-700/80 dark:text-amber-300/80">{t('memory.l3InvalidDraftSource', { count: document.source_count })}</span><button type="button" onClick={() => setL3DeleteTarget(document)} disabled={deletingL3 === document.id} className="ml-auto inline-flex shrink-0 items-center gap-1 rounded border border-red-200 bg-white/70 px-2 py-0.5 font-medium text-red-700 hover:bg-red-50 disabled:opacity-50 dark:border-red-900/70 dark:bg-gray-900/40 dark:text-red-300 dark:hover:bg-red-950/30"><Trash2 size={12} />{deletingL3 === document.id ? t('memory.l3DraftInvalidDeleting') : t('memory.l3DraftInvalidDelete')}</button></div><p className="mt-1.5 leading-5 text-amber-800 dark:text-amber-200">{t('memory.l3InvalidDraftHint')}</p></div>)}{memoryLoadState.l3 === 'ready' && visibleL3Documents.length === 0 && invalidL3Drafts.length === 0 && <p className="text-xs text-gray-500 dark:text-gray-400">{t('memory.l3Empty')}</p>}</div>
        </article>
      </section>

      {/* WebDAV/S3 同步全部 L1，作用域标签保持不变。 */}
      <CloudVaultCard onSynced={() => { void loadMemoryLayers(true).catch(() => {}); void listMemories(true).catch(() => {}) }} />

      {/* 可检索记忆（L1） */}
      <section className="order-20 rounded-xl border border-gray-200 dark:border-gray-700 bg-white dark:bg-gray-800 p-4 space-y-3">
        <div className="flex flex-wrap items-center gap-2">
          <h2 className="text-sm font-semibold text-gray-700 dark:text-gray-200">{t('memory.storageTitle')}</h2>
          <select
            value={selectedMemoryDomainId}
            disabled={memoryLoadState.domains !== 'ready'}
            onChange={(event) => { void setSelectedMemoryDomain(event.target.value) }}
            className="rounded-md border border-gray-300 bg-white px-2 py-1 text-xs text-gray-700 dark:border-gray-600 dark:bg-gray-700 dark:text-gray-200"
            title={t('memory.domainFilterTitle')}
          >
            <option value="all">{t('memory.domainAll')}</option>
            {memoryDomains.map((domain: MemoryDomainItem) => <option key={domain.id} value={domain.id}>{domain.name}</option>)}
          </select>
          <div className="ml-auto flex flex-wrap items-center gap-2 text-xs text-gray-500 dark:text-gray-400">
            <span>{t('memory.total')}: <b className="text-gray-700 dark:text-gray-200">{inventoryReady ? visibleStats?.total ?? localMemoryStats?.total ?? memories.length : '—'}</b></span>
            <span className="text-violet-600 dark:text-violet-400">{t('memory.fact')}: {inventoryReady ? visibleStats?.facts ?? localMemoryStats?.facts ?? factCount : '—'}</span>
            <span className="text-amber-600 dark:text-amber-400">{t('memory.pref')}: {inventoryReady ? visibleStats?.preferences ?? localMemoryStats?.preferences ?? prefCount : '—'}</span>
            <span className="rounded bg-gray-100 px-1.5 py-0.5 text-gray-600 dark:bg-gray-700 dark:text-gray-200">{t('memory.durabilitySession')}: {inventoryReady ? visibleStats?.session_only ?? localMemoryStats?.session_only ?? memories.filter((memory) => memory.durability === 'session').length : '—'}</span>
            <span className="rounded bg-sky-500/10 px-1.5 py-0.5 text-sky-700 dark:text-sky-300">{t('memory.durabilityShortTerm')}: {inventoryReady ? visibleStats?.short_term ?? localMemoryStats?.short_term ?? memories.filter((memory) => memory.durability === 'short_term').length : '—'}</span>
            <span className="rounded bg-emerald-500/10 px-1.5 py-0.5 text-emerald-700 dark:text-emerald-300">{t('memory.durabilityLongTerm')}: {inventoryReady ? visibleStats?.long_term ?? localMemoryStats?.long_term ?? memories.filter((memory) => memory.durability === 'long_term').length : '—'}</span>
            <button
              onClick={() => { void handleRefreshImportance() }}
              disabled={refreshingImportance || memories.length === 0}
              className="inline-flex items-center gap-1 rounded-md border border-gray-300 px-2 py-1 font-medium text-gray-700 hover:bg-gray-50 disabled:cursor-wait disabled:opacity-60 dark:border-gray-600 dark:text-gray-200 dark:hover:bg-gray-700"
              title={t('memory.importanceRefreshHint')}
            >
              <Star size={13} className={refreshingImportance ? 'animate-pulse fill-current text-amber-500' : ''} />
              {refreshingImportance ? t('memory.importanceRefreshing') : t('memory.importanceRefresh')}
            </button>
            <button
              onClick={requestResetL1}
              disabled={resettingL1}
              className="inline-flex items-center gap-1 rounded-md border border-red-200 px-2 py-1 font-medium text-red-700 hover:bg-red-50 disabled:cursor-wait disabled:opacity-60 dark:border-red-900/70 dark:text-red-300 dark:hover:bg-red-950/30"
              title={t('memory.l1ResetTitle')}
            >
              {resettingL1 ? <Loader2 size={13} className="animate-spin" /> : <Trash2 size={13} />}
              {resettingL1 ? t('memory.l1Resetting') : t('memory.l1Reset')}
            </button>
            <button
              onClick={() => { void handleRefresh() }}
              disabled={refreshing}
              className="rounded-md p-1 hover:bg-gray-100 disabled:cursor-wait disabled:opacity-60 dark:hover:bg-gray-700"
              title={refreshing ? t('memory.refreshing') : t('common.refresh')}
              aria-label={refreshing ? t('memory.refreshing') : t('common.refresh')}
            >
              <RefreshCw size={14} className={refreshing ? 'animate-spin' : ''} />
            </button>
          </div>
        </div>

        {[memoryLoadState.inventory, memoryLoadState.domains, memoryLoadState.importance].some((status) => status !== 'ready') && <div className="flex flex-wrap gap-x-4 gap-y-1">
          <MemorySectionStatus status={memoryLoadState.inventory} onRetry={() => { void listMemories(true).catch(() => {}) }} />
          <MemorySectionStatus status={memoryLoadState.domains} loadingLabel={t('memory.domainsLoading')} onRetry={() => { void loadMemoryDomains() }} />
          <MemorySectionStatus status={memoryLoadState.importance} loadingLabel={t('memory.importanceLoading')} onRetry={() => { void listMemories(true).catch(() => {}) }} />
        </div>}

        {/* 语义检索：检索对象就是本区块的可检索记忆（含 L0 对话回退）。 */}
        <div className="flex flex-wrap gap-2">
          <input
            value={query}
            onChange={(e) => setQuery(e.target.value)}
            onKeyDown={(e) => e.key === 'Enter' && handleSearch()}
            placeholder={t('memory.searchPlaceholder')}
            className="flex-1 rounded-md border border-gray-300 dark:border-gray-600 bg-white dark:bg-gray-700 px-2.5 py-1.5 text-sm text-gray-800 dark:text-gray-100 focus:outline-none focus:ring-1 focus:ring-violet-500"
          />
          <select
            value={topK}
            onChange={(e) => setTopK(Number(e.target.value))}
            className="rounded-md border border-gray-300 dark:border-gray-600 bg-white dark:bg-gray-700 px-2 py-1.5 text-sm text-gray-800 dark:text-gray-100 focus:outline-none"
          >
            {[5, 10, 20].map((n) => <option key={n} value={n}>{n}</option>)}
          </select>
          <button
            onClick={handleSearch}
            disabled={loading || !memoryCacheReady || !query.trim()}
            className="inline-flex items-center gap-1.5 px-3 py-1.5 rounded-md bg-violet-600 hover:bg-violet-500 disabled:opacity-50 text-white text-sm font-medium"
          >
            {loading ? <Loader2 size={15} className="animate-spin" /> : <Search size={15} />}
            {t('memory.search')}
          </button>
        </div>
        {visibleSearchResults.length > 0 && (
          <div className="space-y-1.5">
            <p className="text-xs text-gray-500 dark:text-gray-400">
              {t('memory.resultsFor')}「{lastSearchQuery}」
            </p>
            {visibleSearchResults.map((m, i) => (
              <div key={m.id} className="rounded-md bg-gray-50 dark:bg-gray-700/50 border border-gray-200 dark:border-gray-600 px-3 py-2">
                <div className="flex flex-wrap items-center gap-2 mb-1">
                  <span className="text-xs font-mono text-gray-500">#{i + 1}</span>
                  {m.score != null && (
                    <span className="text-xs font-mono text-amber-600 dark:text-amber-400">score {Number(m.score).toFixed(3)}</span>
                  )}
                  {m.memory_type && <span className="text-xs px-1.5 rounded bg-violet-500/10 text-violet-600 dark:text-violet-400">{t(`memory.type.${m.memory_type}`, { defaultValue: m.memory_type })}</span>}
                  {(m.domains ?? []).map((domain) => <span key={domain.id} className="text-xs px-1.5 rounded bg-indigo-500/10 text-indigo-700 dark:text-indigo-300" title={domain.description || domain.name}>{domain.name}</span>)}
                  {m.durability && <span className={`text-xs px-1.5 rounded ${m.durability === 'long_term' ? 'bg-emerald-500/10 text-emerald-700 dark:text-emerald-300' : m.durability === 'short_term' ? 'bg-sky-500/10 text-sky-700 dark:text-sky-300' : 'bg-gray-200 text-gray-600 dark:bg-gray-600 dark:text-gray-200'}`}>{t(`memory.durability${m.durability === 'long_term' ? 'LongTerm' : m.durability === 'short_term' ? 'ShortTerm' : 'Session'}`)}</span>}
                  {m.retrieval_source === 'conversation' && <span className="text-xs px-1.5 rounded bg-emerald-500/10 text-emerald-700 dark:text-emerald-300">{t('memory.searchSourceConversation')}</span>}
                  {m.retrieval_source === 'memory' && <span className="text-xs px-1.5 rounded bg-sky-500/10 text-sky-700 dark:text-sky-300">{t('memory.searchSourceMemory')}</span>}
                  {(m.last_update_at || m.occurred_at) && <span className="text-xs text-gray-500">{localTime(m.last_update_at || m.occurred_at || '')}</span>}
                </div>
                <p className="text-sm text-gray-700 dark:text-gray-200 whitespace-pre-wrap break-all">{m.memory}</p>
              </div>
            ))}
          </div>
        )}
        {visibleSearchResults.length === 0 && lastSearchQuery && !loading && inventoryReady && memoryLoadState.inventory === 'ready' && (
          <p className="text-sm text-gray-500 text-center py-4">{t('memory.noResults')}</p>
        )}

        {visibleMemories.length > 0 ? (
          <div className="space-y-1.5 max-h-[420px] overflow-y-auto pr-1">
            {visibleMemories.map((m) => (
              <MemoryCard key={m.id} memory={m} ranking={importance[m.id]} pinDisabled={memoryLoadState.importance !== 'ready'} onPin={handlePinMemory} onEdit={openEdit} onDelete={handleDelete} />
            ))}
          </div>
        ) : inventoryReady && memoryLoadState.inventory === 'ready' && (
          <p className="text-sm text-gray-500 dark:text-gray-400 text-center py-6">{selectedMemoryDomainId === 'all' ? t('memory.empty') : t('memory.domainEmpty')}</p>
        )}

        {/* L1 清洗与去重：作为可检索记忆的子项；不是 L2/L3 的层级巩固。 */}
        <div className="border-t border-gray-100 pt-3 dark:border-gray-700">
          <div className="flex flex-wrap items-center gap-4">
            <div className="flex-1">
            <h3 className="text-xs font-semibold text-gray-700 dark:text-gray-200">{t('memory.consolidateTitle')}</h3>
            <p className="text-xs text-gray-500 dark:text-gray-400 mt-0.5">{t('memory.consolidateHint')}</p>
            </div>
            <button type="button" onClick={() => setConsolidationOpen((open) => !open)} aria-expanded={consolidationOpen} className="inline-flex items-center gap-1 rounded-md border border-gray-300 px-2.5 py-1.5 text-xs font-medium text-gray-600 hover:bg-gray-50 dark:border-gray-600 dark:text-gray-300 dark:hover:bg-gray-700">{consolidationOpen ? t('memory.ingestCollapse') : t('memory.consolidateView')}{consolidationOpen ? <ChevronUp size={14} /> : <ChevronDown size={14} />}</button>
            {consolidationOpen &&
            <button
            onClick={handleDreaming}
            disabled={consolidating}
            className="inline-flex items-center gap-1.5 px-3 py-1.5 rounded-md bg-violet-600 hover:bg-violet-500 disabled:opacity-50 text-white text-sm font-medium"
            >
            {consolidating ? <Loader2 size={15} className="animate-spin" /> : <MoonStar size={15} />}
            {consolidating ? t('memory.consolidateWorking') : t('memory.consolidateStart')}
            </button>
            }
          </div>
          {consolidationOpen && <>{consolidating && <p className="mt-3 rounded-md bg-violet-500/10 px-2.5 py-2 text-xs text-violet-800 dark:text-violet-200">{consolidationProgress ?? t('memory.dreamInProgress')}<span className="ml-1.5 inline-block animate-pulse">…</span></p>}
          {consolidationResult && <div className={`mt-3 rounded-md px-2.5 py-2.5 text-xs ${consolidationResult.kind === 'ok' ? 'bg-green-500/10 text-green-800 dark:text-green-200' : 'bg-red-500/10 text-red-800 dark:text-red-200'}`}><p>{consolidationResult.text}</p>{consolidationResult.result && <div className="mt-1.5 flex flex-wrap gap-x-3 gap-y-1 text-green-700 dark:text-green-300"><span>{t('memory.dreamSummaryScanned', { count: consolidationResult.result.scopes })}</span><span>{t('memory.dreamSummaryClusters', { count: consolidationResult.result.clusters })}</span><span>{t('memory.dreamSummaryActions', { count: consolidationResult.result.actions })}</span><span>{t('memory.dreamSummaryCount', { before: consolidationResult.result.before_count, after: consolidationResult.result.after_count })}</span></div>}{consolidationResult.kind === 'ok' && consolidationResult.result?.snapshot_id && <button onClick={() => { void handleRestoreConsolidation() }} disabled={restoringConsolidation} className="mt-2 inline-flex items-center gap-1 rounded border border-green-700/30 px-2 py-1 font-medium text-green-800 hover:bg-green-500/10 disabled:opacity-50 dark:text-green-200">{restoringConsolidation ? <Loader2 size={12} className="animate-spin" /> : <History size={12} />}{restoringConsolidation ? t('memory.dreamRestoring') : t('memory.dreamRestore')}</button>}</div>}</>}
        </div>
      </section>

      {/* 编辑弹窗 */}
      {editing && (
        <Modal labelledBy="memory-edit-title" onClose={() => setEditing(null)}>
          <div className="w-full max-w-md rounded-xl bg-white dark:bg-gray-800 border border-gray-200 dark:border-gray-700 p-4 space-y-3">
            <div className="flex items-center justify-between">
              <h3 id="memory-edit-title" className="text-sm font-semibold text-gray-800 dark:text-gray-100">{t('memory.editTitle')}</h3>
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
              <button onClick={handleSaveEdit} className="px-3 py-1.5 rounded-md bg-violet-600 hover:bg-violet-500 text-white text-sm font-medium">
                {t('common.save')}
              </button>
            </div>
          </div>
        </Modal>
      )}

      {/* L3 草案删除确认：仅允许删除未发布的草案。 */}
      {l3DeleteTarget && (
        <Modal labelledBy="memory-l3-delete-title" busy={Boolean(deletingL3)} dismissOnBackdrop onClose={() => setL3DeleteTarget(null)}>
          <div className="w-full max-w-sm space-y-3 rounded-xl border border-gray-200 bg-white p-4 dark:border-gray-700 dark:bg-gray-800">
            <div className="flex items-center gap-2">
              <Trash2 size={18} className="text-red-500" />
              <h3 id="memory-l3-delete-title" className="text-sm font-semibold text-gray-800 dark:text-gray-100">{l3DeleteTarget.state === 'archived' ? t('memory.l3DeleteArchivedTitle') : t('memory.l3DeleteTitle')}</h3>
            </div>
            <p className="text-sm leading-6 text-gray-600 dark:text-gray-300">{t('memory.l3DeleteHint', { kind: l3DeleteTarget.state === 'archived' ? t('memory.l3Archived').toLowerCase() : t('memory.l3Draft').toLowerCase() })}</p>
            <p className="text-xs text-gray-500 dark:text-gray-400">{t('memory.l3SourceLine', { count: l3DeleteTarget.source_count, tokens: l3DeleteTarget.token_estimate })}</p>
            <div className="flex justify-end gap-2">
              <button onClick={() => setL3DeleteTarget(null)} disabled={Boolean(deletingL3)} className="rounded-md border border-gray-300 px-3 py-1.5 text-sm text-gray-600 hover:bg-gray-50 disabled:opacity-50 dark:border-gray-600 dark:text-gray-300 dark:hover:bg-gray-700">{t('common.cancel')}</button>
              <button onClick={() => { void confirmL3DraftDelete() }} disabled={Boolean(deletingL3)} className="inline-flex items-center gap-1 rounded-md bg-red-600 px-3 py-1.5 text-sm font-medium text-white hover:bg-red-500 disabled:opacity-50">{deletingL3 ? <Loader2 size={14} className="animate-spin" /> : <Trash2 size={14} />}{deletingL3 ? t('memory.l3Deleting') : t('memory.l3DeleteDraft')}</button>
            </div>
          </div>
        </Modal>
      )}

      {/* L1 重新提取确认：保留 L0 与使用量，只重建派生层。 */}
      {resetL1ConfirmOpen && (
        <Modal labelledBy="memory-l1-reset-title" busy={resettingL1} dismissOnBackdrop onClose={() => setResetL1ConfirmOpen(false)}>
          <div className="w-full max-w-md space-y-3 rounded-xl border border-gray-200 bg-white p-4 dark:border-gray-700 dark:bg-gray-800">
            <div className="flex items-center gap-2"><RefreshCw size={18} className="text-violet-600 dark:text-violet-300" /><h3 id="memory-l1-reset-title" className="text-sm font-semibold text-gray-800 dark:text-gray-100">{t('memory.l1ResetConfirmTitle')}</h3></div>
            <p className="text-sm leading-6 text-gray-600 dark:text-gray-300">{t('memory.l1ResetConfirmDesc')}</p>
            <p className="text-xs leading-5 text-violet-700 dark:text-violet-300">{t('memory.l1ResetConfirmHint')}</p>
            <div className="flex justify-end gap-2"><button onClick={() => setResetL1ConfirmOpen(false)} disabled={resettingL1} className="rounded-md border border-gray-300 px-3 py-1.5 text-sm text-gray-600 hover:bg-gray-50 disabled:opacity-50 dark:border-gray-600 dark:text-gray-300 dark:hover:bg-gray-700">{t('common.cancel')}</button><button onClick={() => { void confirmResetL1() }} disabled={resettingL1} className="inline-flex items-center gap-1 rounded-md bg-violet-600 px-3 py-1.5 text-sm font-medium text-white hover:bg-violet-500 disabled:opacity-50">{resettingL1 ? <Loader2 size={14} className="animate-spin" /> : <RefreshCw size={14} />}{resettingL1 ? t('memory.l1Resetting') : t('memory.l1ResetRun')}</button></div>
          </div>
        </Modal>
      )}

      {/* 删除确认弹窗 */}
      {deleteTarget && (
        <Modal labelledBy="memory-delete-title" dismissOnBackdrop onClose={() => setDeleteTarget(null)}>
          <div className="w-full max-w-sm rounded-xl bg-white dark:bg-gray-800 border border-gray-200 dark:border-gray-700 p-4 space-y-3">
            <div className="flex items-center gap-2">
              <Trash2 size={18} className="text-red-500" />
              <h3 id="memory-delete-title" className="text-sm font-semibold text-gray-800 dark:text-gray-100">{t('memory.deleteConfirm')}</h3>
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
  )
})

// 用户自定义记忆列表：L2 工作记忆与 L3 长期 Profile 卡片各自只显示
// 归属本层的 user_defined 条目（id 前缀 local-user-l2: / local-user-l3:，
// 旧版 local-user: 归入 l3），支持内联编辑与删除；复用 store 的
// updateMemory/deleteMemory，保存/删除后自动刷新列表。
function UserMemoryList({ accent, scope }: { accent: 'sky' | 'violet'; scope: 'l2' | 'l3' }) {
  const { t } = useTranslation()
  const { memories, updateMemory, deleteMemory } = useMemoryFields('memories', 'updateMemory', 'deleteMemory')
  const customMemories = useMemo(() => memories.filter((memory) => {
    if (!memory.user_defined) return false
    return scope === 'l2'
      ? memory.id.startsWith('local-user-l2:')
      : !memory.id.startsWith('local-user-l2:')
  }), [memories, scope])
  const [editingId, setEditingId] = useState<string | null>(null)
  const [editContent, setEditContent] = useState('')
  const [savingId, setSavingId] = useState<string | null>(null)
  const [deletingId, setDeletingId] = useState<string | null>(null)
  const [error, setError] = useState<string | null>(null)
  const { confirm, confirmDialog } = useConfirmDialog()

  // 列表在确认期间被清空时仍要渲染挂起的确认弹窗，否则 Promise 会一直悬空。
  if (!customMemories.length) return confirmDialog

  const accentText = accent === 'sky' ? 'text-sky-700 dark:text-sky-300' : 'text-violet-700 dark:text-violet-300'
  const accentSave = accent === 'sky'
    ? 'border-sky-300 text-sky-700 hover:bg-sky-100 dark:border-sky-700 dark:text-sky-200 dark:hover:bg-sky-950/40'
    : 'border-violet-300 text-violet-700 hover:bg-violet-100 dark:border-violet-700 dark:text-violet-200 dark:hover:bg-violet-950/40'

  const handleSave = async (id: string) => {
    const content = editContent.trim()
    if (!content) return
    setSavingId(id)
    setError(null)
    try {
      await updateMemory(id, content)
      setEditingId(null)
      setEditContent('')
    } catch (err) {
      setError(String(err))
    } finally {
      setSavingId(null)
    }
  }

  const handleDelete = async (id: string) => {
    const target = customMemories.find((memory) => memory.id === id)
    if (!(await confirm({
      title: t('memory.customMemoryDeleteTitle'),
      message: t('memory.customMemoryDeleteConfirm'),
      detailLines: target ? [{ label: t('memory.customMemoryContent'), value: target.memory }] : undefined,
      confirmLabel: t('memory.customMemoryDelete'),
      tone: 'danger',
    }))) return
    setDeletingId(id)
    setError(null)
    try {
      await deleteMemory(id)
    } catch (err) {
      setError(String(err))
    } finally {
      setDeletingId(null)
    }
  }

  return (
    <div className="mt-3 space-y-1.5">
      {confirmDialog}
      <p className={`text-xs font-medium ${accentText}`}>{t('memory.customMemoryList', { count: customMemories.length })}</p>
      {error && <p className="rounded border border-red-200 bg-red-50 px-2 py-1 text-xs text-red-700 dark:border-red-900/70 dark:bg-red-950/30 dark:text-red-300">{error}</p>}
      {customMemories.map((memory) => (
        <div key={memory.id} className="rounded-md border border-gray-200 bg-white/70 px-2.5 py-2 dark:border-gray-700 dark:bg-gray-900/30">
          {editingId === memory.id ? (
            <div className="space-y-1.5">
              <textarea
                value={editContent}
                onChange={(event) => setEditContent(event.target.value)}
                rows={3}
                className="w-full rounded-md border border-gray-300 bg-white px-2.5 py-1.5 text-sm text-gray-800 focus:outline-none focus:ring-1 focus:ring-violet-500 dark:border-gray-600 dark:bg-gray-700 dark:text-gray-100"
              />
              <div className="flex items-center gap-2">
                <button type="button" onClick={() => { void handleSave(memory.id) }} disabled={savingId === memory.id || !editContent.trim()} className={`inline-flex items-center gap-1 rounded border px-2 py-0.5 text-xs font-medium disabled:opacity-50 ${accentSave}`}>
                  {savingId === memory.id ? <Loader2 size={12} className="animate-spin" /> : null}
                  {savingId === memory.id ? t('memory.customMemorySaving') : t('memory.l3Save')}
                </button>
                <button type="button" onClick={() => { setEditingId(null); setEditContent('') }} disabled={savingId === memory.id} className="rounded border border-gray-300 px-2 py-0.5 text-xs font-medium text-gray-700 hover:bg-gray-100 disabled:opacity-50 dark:border-gray-600 dark:text-gray-200 dark:hover:bg-gray-700">{t('memory.l3Cancel')}</button>
              </div>
            </div>
          ) : (
            <div className="flex items-start gap-2">
              <span className="mt-0.5 shrink-0 rounded bg-gray-100 px-1.5 py-0.5 font-mono text-[11px] text-gray-600 dark:bg-gray-700 dark:text-gray-300">{memory.memory_type ?? 'fact'}</span>
              <p className="min-w-0 flex-1 whitespace-pre-wrap break-words text-sm text-gray-800 dark:text-gray-100">{memory.memory}</p>
              <div className="flex shrink-0 items-center gap-1">
                <button type="button" onClick={() => { setEditingId(memory.id); setEditContent(memory.memory) }} disabled={deletingId === memory.id} className="inline-flex items-center gap-1 rounded border border-gray-300 px-1.5 py-0.5 text-xs font-medium text-gray-700 hover:bg-gray-100 disabled:opacity-50 dark:border-gray-600 dark:text-gray-200 dark:hover:bg-gray-700"><Pencil size={11} />{t('memory.l2Edit')}</button>
                <button type="button" onClick={() => { void handleDelete(memory.id) }} disabled={deletingId === memory.id} className="inline-flex items-center gap-1 rounded border border-red-200 px-1.5 py-0.5 text-xs font-medium text-red-700 hover:bg-red-50 disabled:opacity-50 dark:border-red-900/70 dark:text-red-300 dark:hover:bg-red-950/30">{deletingId === memory.id ? <Loader2 size={11} className="animate-spin" /> : <Trash2 size={11} />}{t('memory.customMemoryDelete')}</button>
              </div>
            </div>
          )}
        </div>
      ))}
    </div>
  )
}
