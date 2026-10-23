import { create } from 'zustand'
import { useShallow } from 'zustand/react/shallow'
import { invoke } from '@tauri-apps/api/core'
import { createBgeConsolidationCandidateBatches, searchMemoriesWithBge } from '../lib/bgeSemanticSearch'
import type { AddEventItem, AddResult, ConsolidationResult, EngineStatus, HookStatus, IngestStatus, L1ResetResult, LocalMemoryStats, MemoryConversationDetail, MemoryImportance, MemoryImportanceSummary, MemoryImportResult, MemoryItem, MemoryLayerDocument, MemoryLayerRunResult, OrganizeConversationsResult, PendingMemorySession } from '../types/memory'
import type { AgentSourceInfo } from '../types/agentSources'
import type { MemoryInjectionLog, MemoryInjectionStats } from '../types/memoryInjection'

import { jsonEqual } from '../lib/stateEquality'
import { requestMemory as request } from '../api/memory'
import { useTelemetryStore } from './telemetryStore'

export const GLOBAL_USER_ID = 'agent-manager'

interface MemoryStore {
  /** 内置记忆引擎在线状态 */
  engineOnline: boolean | null
  /** Diagnostic details for the optional semantic sidecar. */
  engineStatus: EngineStatus | null
  /** 选中的 Agent（仅用于界面筛选；记忆在所有 Agent 间共享） */
  activeAgentId: string | null
  /** One stable, local-user memory space shared by every adapter. */
  memoryScope: string
  /** Claude Code hook 安装状态（自动沉淀） */
  hookStatus: HookStatus | null
  /** Qoder Hook 安装状态（自动沉淀） */
  qoderHookStatus: HookStatus | null
  /** Codex Hook 安装状态（自动沉淀） */
  codexHookStatus: HookStatus | null
  /** WorkBuddy Hook 安装状态（自动沉淀） */
  workbuddyHookStatus: HookStatus | null
  /** DeepSeek Harness 注入插件状态（pre-step 注入，语义同 Hook） */
  dshHookStatus: HookStatus | null
  /** 沉淀管道状态 */
  ingestStatus: IngestStatus | null
  memoryInjectionLogs: MemoryInjectionLog[]
  memoryInjectionStats: MemoryInjectionStats[]
  agentSources: AgentSourceInfo[]
  l2Documents: MemoryLayerDocument[]
  l3Documents: MemoryLayerDocument[]
  localMemoryStats: LocalMemoryStats | null
  memoryCacheReady: boolean
  memories: MemoryItem[]
  lastAddEvents: AddEventItem[]
  lastSearchResults: MemoryItem[]
  lastSearchQuery: string
  importance: Record<string, MemoryImportance>
  loading: boolean
  setActiveAgent: (id: string | null) => Promise<void>
  checkEngine: () => Promise<boolean>
  startEngine: () => Promise<boolean>
  stopEngine: () => Promise<void>
  checkIngest: () => Promise<void>
  installHook: (agentType?: 'claude' | 'qoder' | 'codex' | 'workbuddy' | 'dsh') => Promise<void>
  uninstallHook: (agentType?: 'claude' | 'qoder' | 'codex' | 'workbuddy' | 'dsh') => Promise<void>
  setIngestEnabled: (on: boolean) => Promise<void>
  flushIngestQueue: () => Promise<number>
  organizeConversations: () => Promise<OrganizeConversationsResult>
  cancelOrganize: () => Promise<boolean>
  loadPendingSessions: (limit?: number) => Promise<PendingMemorySession[]>
  loadOrganizedSessions: (limit?: number) => Promise<PendingMemorySession[]>
  loadConversationDetail: (eventKey: string) => Promise<MemoryConversationDetail>
  organizeSession: (eventKey: string) => Promise<OrganizeConversationsResult>
  importMemoryFolder: (folder: string) => Promise<MemoryImportResult>
  loadMemoryInjectionLogs: () => Promise<void>
  loadInjectionStats: () => Promise<void>
  loadAgentSources: () => Promise<void>
  setAgentSourceOverride: (id: string, transcriptRoots?: string[] | null, configHome?: string | null) => Promise<AgentSourceInfo>
  setAgentSourceScanEnabled: (id: string, enabled: boolean) => Promise<AgentSourceInfo>
  loadMemoryLayers: () => Promise<void>
  consolidateShortTermMemory: () => Promise<MemoryLayerRunResult>
  draftLongTermProfile: () => Promise<MemoryLayerRunResult>
  publishLongTermProfile: (documentId: string, content?: string) => Promise<MemoryLayerDocument>
  /** 手动编辑已发布的层级记忆文档（L2 工作记忆 / L3 Profile）：保存即覆盖当前发布版并立刻生效。 */
  updatePublishedMemoryDocument: (documentId: string, content: string) => Promise<MemoryLayerDocument>
  deleteLongTermProfileDraft: (documentId: string) => Promise<void>
  addMemory: (messages: { role: string; content: string }[]) => Promise<AddEventItem[]>
  /** 用户手动添加自定义长期记忆：只有用户能删改；scope 指定归属 L2/L3，两层相互独立。 */
  addUserMemory: (content: string, memoryType: string, scope: 'l2' | 'l3') => Promise<MemoryItem>
  search: (query: string, topK: number) => Promise<MemoryItem[]>
  listMemories: (force?: boolean) => Promise<void>
  resetL1ForReextraction: () => Promise<L1ResetResult>
  updateMemory: (id: string, content: string) => Promise<void>
  deleteMemory: (id: string) => Promise<void>
  dreaming: (onProgress?: (processed: number, total: number) => void) => Promise<ConsolidationResult>
  restoreConsolidation: (snapshotId: string) => Promise<ConsolidationResult>
  refreshImportance: () => Promise<MemoryImportanceSummary>
  setMemoryPinned: (memoryId: string, pinned: boolean) => Promise<void>
}

export const useMemoryStore = create<MemoryStore>((set, get) => ({
  engineOnline: null,

  engineStatus: null,

  activeAgentId: null,

  memoryScope: GLOBAL_USER_ID,

  hookStatus: null,

  qoderHookStatus: null,

  codexHookStatus: null,

  workbuddyHookStatus: null,

  dshHookStatus: null,

  ingestStatus: null,

  memoryInjectionLogs: [],

  memoryInjectionStats: [],

  agentSources: [],

  l2Documents: [],

  l3Documents: [],

  localMemoryStats: null,

  memoryCacheReady: false,

  memories: [],

  lastAddEvents: [],

  lastSearchResults: [],

  lastSearchQuery: '',

  importance: {},

  loading: false,

  async setActiveAgent(id) {
    set({ activeAgentId: id, memoryScope: GLOBAL_USER_ID })
  },

  async checkIngest() {
    try {
      const [hook, qoderHook, codexHook, workbuddyHook, dshHook, ingest] = await Promise.all([
        invoke<HookStatus>('memory_hook_status', { agentType: 'claude' }),
        invoke<HookStatus>('memory_hook_status', { agentType: 'qoder' }),
        invoke<HookStatus>('memory_hook_status', { agentType: 'codex' }),
        invoke<HookStatus>('memory_hook_status', { agentType: 'workbuddy' }),
        invoke<HookStatus>('memory_hook_status', { agentType: 'dsh' }),
        invoke<IngestStatus>('memory_ingest_status'),
      ])
      // 数据未变化时返回同一 state 引用，zustand 不通知订阅者 →
      // 空闲轮询零重渲染。
      set((state) => (
        jsonEqual(state.hookStatus, hook) && jsonEqual(state.qoderHookStatus, qoderHook) && jsonEqual(state.codexHookStatus, codexHook) && jsonEqual(state.workbuddyHookStatus, workbuddyHook) && jsonEqual(state.dshHookStatus, dshHook) && jsonEqual(state.ingestStatus, ingest)
          ? state
          : { hookStatus: hook, qoderHookStatus: qoderHook, codexHookStatus: codexHook, workbuddyHookStatus: workbuddyHook, dshHookStatus: dshHook, ingestStatus: ingest }
      ))
    } catch {
      /* 浏览器预览下无 Tauri 后端，忽略 */
    }
  },

  async installHook(agentType = 'claude'): Promise<void> {
    await invoke<string[]>('memory_hook_install', { agentType })
    await get().checkIngest()
  },

  async uninstallHook(agentType = 'claude') {
    await invoke('memory_hook_uninstall', { agentType })
    await get().checkIngest()
  },

  async setIngestEnabled(on) {
    await invoke('memory_ingest_set_enabled', { enabled: on })
    await get().checkIngest()
  },

  async flushIngestQueue() {
    const count = await invoke<number>('memory_ingest_flush_pending')
    await get().checkIngest()
    return count
  },

  async organizeConversations() {
    try {
      return await invoke<OrganizeConversationsResult>('memory_ingest_organize_conversations')
    } finally {
      // Partial results are kept when extraction is interrupted.
      await Promise.all([useTelemetryStore.getState().checkTelemetry(), get().listMemories(true), get().checkIngest()])
    }
  },

  async cancelOrganize() {
    const requested = await invoke<boolean>('memory_ingest_cancel_organize')
    await get().checkIngest()
    return requested
  },

  async loadPendingSessions(limit?: number) {
    return invoke<PendingMemorySession[]>('memory_pending_l1_sessions', { limit: limit ?? 200 })
  },

  async loadOrganizedSessions(limit?: number) {
    return invoke<PendingMemorySession[]>('memory_organized_l1_sessions', { limit: limit ?? 200 })
  },

  async loadConversationDetail(eventKey) {
    return invoke<MemoryConversationDetail>('memory_l1_conversation_detail', { eventKey })
  },

  async organizeSession(eventKey) {
    try {
      return await invoke<OrganizeConversationsResult>('memory_ingest_organize_session', { eventKey })
    } finally {
      await Promise.all([useTelemetryStore.getState().checkTelemetry(), get().listMemories(true), get().checkIngest()])
    }
  },

  async importMemoryFolder(folder) {
    const result = await invoke<MemoryImportResult>('memory_import_folder', { folder })
    await Promise.all([get().listMemories(true), useTelemetryStore.getState().checkTelemetry(), get().checkIngest()])
    return result
  },

  async loadMemoryInjectionLogs() {
    try {
      const logs = await invoke<MemoryInjectionLog[]>('memory_mcp_access_logs', { limit: 30 })
      set((state) => (jsonEqual(state.memoryInjectionLogs, logs) ? state : { memoryInjectionLogs: logs }))
    } catch {
      /* 浏览器预览或旧版后端不可用时保留当前内容。 */
    }
  },

  async loadInjectionStats() {
    try {
      const stats = await invoke<MemoryInjectionStats[]>('memory_injection_stats', { days: 30 })
      set((state) => (jsonEqual(state.memoryInjectionStats, stats) ? state : { memoryInjectionStats: stats }))
    } catch {
      /* 浏览器预览或旧版后端不可用时保留当前内容。 */
    }
  },

  async loadAgentSources() {
    try {
      set({ agentSources: await invoke<AgentSourceInfo[]>('agent_sources_list') })
    } catch {
      /* 浏览器预览下无 Tauri 后端，忽略 */
    }
  },

  async setAgentSourceOverride(id, transcriptRoots = null, configHome = null) {
    const info = await invoke<AgentSourceInfo>('agent_source_set_override', {
      id,
      transcriptRoots: transcriptRoots ?? null,
      configHome: configHome ?? null,
    })
    await get().loadAgentSources()
    return info
  },

  async setAgentSourceScanEnabled(id, enabled) {
    const info = await invoke<AgentSourceInfo>('agent_source_set_scan_enabled', { id, enabled })
    await get().loadAgentSources()
    return info
  },

  async loadMemoryLayers() {
    const [l2, l3] = await Promise.all([
      invoke<MemoryLayerDocument[]>('memory_layer_documents', { layer: 'l2' }),
      invoke<MemoryLayerDocument[]>('memory_layer_documents', { layer: 'l3' }),
    ])
    set({ l2Documents: l2, l3Documents: l3 })
  },

  async consolidateShortTermMemory() {
    const result = await invoke<MemoryLayerRunResult>('memory_short_term_consolidate', { days: 30 })
    // Layer regeneration does not own user-defined L1 rows. Refresh both views
    // so older custom memories remain visible even after a large L1 rebuild.
    await Promise.all([get().loadMemoryLayers(), get().listMemories(true)])
    return result
  },

  async draftLongTermProfile() {
    const result = await invoke<MemoryLayerRunResult>('memory_long_term_profile_draft')
    await Promise.all([get().loadMemoryLayers(), get().listMemories(true)])
    return result
  },

  async publishLongTermProfile(documentId, content) {
    const document = await invoke<MemoryLayerDocument>('memory_long_term_profile_publish', { documentId, content: content ?? null })
    await get().loadMemoryLayers()
    return document
  },

  async updatePublishedMemoryDocument(documentId, content) {
    const document = await invoke<MemoryLayerDocument>('memory_layer_document_update', { documentId, content })
    await get().loadMemoryLayers()
    return document
  },

  async deleteLongTermProfileDraft(documentId) {
    await invoke('memory_long_term_profile_delete_draft', { documentId })
    await get().loadMemoryLayers()
  },

  async checkEngine() {
    // 通过 Tauri 后端探测；浏览器预览中没有内置引擎。
    try {
      const status = await invoke<EngineStatus>('memory_backend_status')
      set({ engineOnline: status.online, engineStatus: status })
      return status.online
    } catch {
      set({ engineOnline: false, engineStatus: null })
      return false
    }
  },

  async startEngine() {
    try {
      const status = await invoke<EngineStatus>('memory_backend_start')
      set({ engineOnline: status.online, engineStatus: status })
      return status.online
    } catch {
      return get().checkEngine()
    }
  },

  async stopEngine() {
    try {
      await invoke('memory_backend_stop')
    } catch {
      /* 浏览器预览下无可调用后端，忽略 */
    }
    set({ engineOnline: false, engineStatus: null })
  },

  async addMemory(messages) {
    const { memoryScope } = get()
    set({ loading: true })
    try {
      const data = await request<{ data?: AddResult }>('/v1/memory/add', {
        user_id: memoryScope,
        agent_id: memoryScope,
        messages,
        mode: 'sync',
      })
      const events = data.data?.memories ?? []
      set({ lastAddEvents: events })
      return events
    } finally {
      set({ loading: false })
    }
  },

  async addUserMemory(content, memoryType, scope) {
    set({ loading: true })
    try {
      const item = await invoke<MemoryItem>('local_memory_add_user', { content, memoryType, scope })
      await get().listMemories(true)
      return item
    } finally {
      set({ loading: false })
    }
  },

  async search(query, topK) {
    set({ loading: true, lastSearchQuery: query })
    try {
      const results = await searchMemoriesWithBge(query, get().memories, topK)
      set({ lastSearchResults: results })
      return results
    } finally {
      set({ loading: false })
    }
  },

  async listMemories(force = false) {
    // Switching pages in the same app session should be instant.  A manual
    // refresh or a write operation supplies `true` to reconcile the cache.
    if (!force && get().memoryCacheReady) return
    set({ loading: true })
    try {
      const [extracted, stats] = await Promise.allSettled([
        invoke<MemoryItem[]>('local_memory_list', { limit: 200 }),
        invoke<LocalMemoryStats>('local_memory_stats'),
      ])
      if (extracted.status === 'rejected') throw extracted.reason
      const localMemories = extracted.status === 'fulfilled'
        ? extracted.value.map((item) => ({ ...item, retrieval_source: 'local' as const, retrieval_method: 'local' as const }))
        : []
      // SQLite L1 is authoritative. The optional sidecar may temporarily
      // retain old embeddings after a deliberate rebuild, so it is used for
      // retrieval only and never rendered as an independent memory inventory.
      const memories = [...localMemories]
      const records = await invoke<MemoryImportance[]>('memory_importance_list', { memoryIds: memories.map((memory) => memory.id) })
      set({ memories, importance: Object.fromEntries(records.map((record) => [record.memory_id, record])), localMemoryStats: stats.status === 'fulfilled' ? stats.value : get().localMemoryStats, memoryCacheReady: true })
    } finally {
      set({ loading: false })
    }
  },

  async resetL1ForReextraction() {
    const result = await invoke<L1ResetResult>('local_memory_reset_for_reextraction')
    set({ memories: [], importance: {}, localMemoryStats: { total: 0, facts: 0, preferences: 0, session_only: 0, short_term: 0, long_term: 0 }, l2Documents: [], l3Documents: [], memoryCacheReady: true })
    await useTelemetryStore.getState().checkTelemetry()
    return result
  },

  async updateMemory(id, content) {
    if (id.startsWith('local-')) await invoke('local_memory_update', { id, content })
    else await request('/v1/memory/update', { memory_id: id, content })
    await get().listMemories(true)
  },

  async deleteMemory(id) {
    if (id.startsWith('local-')) await invoke('local_memory_delete', { id })
    else await request('/v1/memory/delete', { memory_id: id })
    await get().listMemories(true)
  },

  async dreaming(onProgress) {
    const candidateBatches = await createBgeConsolidationCandidateBatches(get().memories, onProgress)
    return invoke<ConsolidationResult>('memory_consolidate', { candidateBatches })
  },

  async restoreConsolidation(snapshotId) {
    return invoke<ConsolidationResult>('memory_consolidation_restore', { snapshotId })
  },

  async refreshImportance() {
    const summary = await invoke<MemoryImportanceSummary>('memory_importance_refresh')
    await get().listMemories(true)
    return summary
  },

  async setMemoryPinned(memoryId, pinned) {
    await invoke('memory_importance_set_pinned', { memoryId, pinned })
    await get().listMemories(true)
  }
}))

/** Subscribe only to fields owned by this feature. */
export function useMemoryFields<const K extends keyof MemoryStore>(...keys: K[]): Pick<MemoryStore, K> {
  return useMemoryStore(useShallow((state) => {
    const selected = {} as Pick<MemoryStore, K>
    for (const key of keys) selected[key] = state[key]
    return selected
  }))
}
