import { create } from 'zustand'
import { invoke } from '@tauri-apps/api/core'
import { createBgeConsolidationCandidateBatches, searchMemoriesWithBge } from '../lib/bgeSemanticSearch'
import type { AddEventItem, AddResult, AgentSourceInfo, ConsolidationResult, EngineStatus, HookStatus, IngestStatus, L1ResetResult, LocalMemoryStats, McpAccessLog, McpAgentStatus, McpCatalogEntry, McpImportCandidate, MemoryConversationDetail, MemoryImportance, MemoryImportanceSummary, MemoryImportResult, MemoryInjectionStats, MemoryItem, MemoryLayerDocument, MemoryLayerRunResult, MemoryMcpStatus, MemoryMcpTarget, OrganizeConversationsResult, PendingMemorySession, SkillAdoptResult, SkillDocument, SkillDriftFile, SkillItem, SkillPublishedDrift, SkillSyncPreview, TelemetryEvent, TelemetryLiveStatus, TelemetrySummary, TelemetryUsageAnalytics, TelemetryUsageRecord, TelemetryUsageRefresh } from '../types/memory'

/** 全局记忆归属（未选择具体 Agent 时的 user_id） */
export const GLOBAL_USER_ID = 'agent-manager'

interface MemoryApiError extends Error {
  status?: number
}

async function request<T>(path: string, body: unknown): Promise<T> {
  try {
    const data = await invoke<T & { code?: string, message?: string }>('memory_backend_request', { path, body })
    if (data.code && data.code !== 'ok' && data.code !== 'queued') {
      const err = new Error(data.message || 'memory API request failed') as MemoryApiError
      throw err
    }
    return data
  } catch (cause) {
    const err = cause instanceof Error ? cause as MemoryApiError : new Error(String(cause)) as MemoryApiError
    throw err
  }
}

/** 轮询等值守卫用的序列化深比较；内容一致即视为未变化。 */
function jsonEqual(a: unknown, b: unknown): boolean {
  return JSON.stringify(a) === JSON.stringify(b)
}

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
  /** Codex / Claude connection state for the shared-memory MCP server. */
  memoryMcp: Record<MemoryMcpTarget, MemoryMcpStatus | null>
  /** 沉淀管道状态 */
  ingestStatus: IngestStatus | null
  telemetrySummary: TelemetrySummary | null
  telemetryLiveStatus: TelemetryLiveStatus | null
  telemetryEvents: TelemetryEvent[]
  telemetryUsageRecords: TelemetryUsageRecord[]
  mcpAccessLogs: McpAccessLog[]
  memoryInjectionStats: MemoryInjectionStats[]
  agentSources: AgentSourceInfo[]
  l2Documents: MemoryLayerDocument[]
  l3Documents: MemoryLayerDocument[]
  skills: SkillItem[]
  localMemoryStats: LocalMemoryStats | null
  memoryCacheReady: boolean
  skillCacheReady: boolean
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
  installHook: (agentType?: 'claude' | 'qoder' | 'codex' | 'workbuddy') => Promise<void>
  uninstallHook: (agentType?: 'claude' | 'qoder' | 'codex' | 'workbuddy') => Promise<void>
  checkMemoryMcp: () => Promise<void>
  installMemoryMcp: (agentType: MemoryMcpTarget) => Promise<MemoryMcpStatus>
  uninstallMemoryMcp: (agentType: MemoryMcpTarget) => Promise<void>
  setIngestEnabled: (on: boolean) => Promise<void>
  flushIngestQueue: () => Promise<number>
  organizeConversations: () => Promise<OrganizeConversationsResult>
  loadPendingSessions: (limit?: number) => Promise<PendingMemorySession[]>
  loadOrganizedSessions: (limit?: number) => Promise<PendingMemorySession[]>
  loadConversationDetail: (eventKey: string) => Promise<MemoryConversationDetail>
  organizeSession: (eventKey: string) => Promise<OrganizeConversationsResult>
  importMemoryFolder: (folder: string) => Promise<MemoryImportResult>
  /** 常规刷新只读取账本；仅在用户主动刷新时回扫本机转录。 */
  checkTelemetry: (options?: { backfill?: boolean; refreshUsage?: boolean; limit?: number }) => Promise<TelemetryUsageRefresh | null>
  loadUsageAnalytics: (filters: { startAt?: string; endAt?: string; source?: string; bucket: 'hour' | 'day' }) => Promise<TelemetryUsageAnalytics>
  checkMcpAccessLogs: () => Promise<void>
  loadInjectionStats: () => Promise<void>
  loadAgentSources: () => Promise<void>
  setAgentSourceOverride: (id: string, transcriptRoots?: string[] | null, configHome?: string | null) => Promise<AgentSourceInfo>
  loadMemoryLayers: () => Promise<void>
  consolidateShortTermMemory: () => Promise<MemoryLayerRunResult>
  draftLongTermProfile: () => Promise<MemoryLayerRunResult>
  publishLongTermProfile: (documentId: string, content?: string) => Promise<MemoryLayerDocument>
  /** 手动编辑已发布的层级记忆文档（L2 工作记忆 / L3 Profile）：保存即覆盖当前发布版并立刻生效。 */
  updatePublishedMemoryDocument: (documentId: string, content: string) => Promise<MemoryLayerDocument>
  deleteLongTermProfileDraft: (documentId: string) => Promise<void>
  scanSkills: () => Promise<SkillItem[]>
  loadSkills: (force?: boolean) => Promise<SkillItem[]>
  readSkill: (source: string, name: string) => Promise<SkillDocument>
  skillDriftDetail: (target: string, source: string, name: string) => Promise<SkillDriftFile[]>
  publishedDrift: () => Promise<SkillPublishedDrift[]>
  adoptLocalSkill: (target: string, source: string, name: string) => Promise<SkillAdoptResult>
  applySkillSyncOne: (target: string, source: string, name: string) => Promise<SkillItem>
  previewSkillSync: (target: string) => Promise<SkillSyncPreview>
  applySkillSync: (target: string, overwrite: boolean) => Promise<SkillSyncPreview>
  setSkillStatus: (source: string, name: string, status: 'draft' | 'published') => Promise<SkillItem>
  setSkillAssignment: (source: string, name: string, target: string, equipped: boolean) => Promise<SkillItem>
  setSkillStatusBulk: (items: { source: string; name: string }[], status: 'draft' | 'published') => Promise<SkillItem[]>
  setSkillAssignmentBulk: (items: { source: string; name: string }[], target: string, equipped: boolean) => Promise<SkillItem[]>
  publishSkill: (source: string, name: string, assignedAgents: string[]) => Promise<SkillItem>
  rollbackSkillLatest: (source: string, name: string) => Promise<SkillItem>
  deleteSkill: (source: string, name: string) => Promise<void>
  // ── MCP 库 ──
  mcpCatalog: McpCatalogEntry[]
  mcpStatuses: McpAgentStatus[]
  mcpStatusError: string
  loadMcpCatalog: () => Promise<McpCatalogEntry[]>
  upsertMcpEntry: (entry: McpCatalogEntry) => Promise<McpCatalogEntry>
  importMcpEntry: (name: string, agent: string) => Promise<McpCatalogEntry>
  deleteMcpEntry: (name: string) => Promise<void>
  equipMcp: (name: string, agent: string) => Promise<McpCatalogEntry>
  unequipMcp: (name: string, agent: string) => Promise<McpCatalogEntry>
  syncMcpAgent: (agent: string) => Promise<void>
  checkMcpStatuses: () => Promise<void>
  importMcpFromAgents: () => Promise<McpImportCandidate[]>
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
  memoryMcp: { codex_cli: null, claude_cli: null, codex_desktop: null, claude_desktop: null, qoder: null, workbuddy: null, minimax: null, kimi: null, zcode: null },
  ingestStatus: null,
  telemetrySummary: null,
  telemetryLiveStatus: null,
  telemetryEvents: [],
  telemetryUsageRecords: [],
  mcpAccessLogs: [],
  memoryInjectionStats: [],
  agentSources: [],
  l2Documents: [],
  l3Documents: [],
  skills: [],
  mcpCatalog: [],
  mcpStatuses: [],
  mcpStatusError: '',
  localMemoryStats: null,
  memoryCacheReady: false,
  skillCacheReady: false,
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
      const [hook, qoderHook, codexHook, workbuddyHook, ingest] = await Promise.all([
        invoke<HookStatus>('memory_hook_status', { agentType: 'claude' }),
        invoke<HookStatus>('memory_hook_status', { agentType: 'qoder' }),
        invoke<HookStatus>('memory_hook_status', { agentType: 'codex' }),
        invoke<HookStatus>('memory_hook_status', { agentType: 'workbuddy' }),
        invoke<IngestStatus>('memory_ingest_status'),
      ])
      // 数据未变化时返回同一 state 引用，zustand 不通知订阅者 →
      // 空闲轮询零重渲染。
      set((state) => (
        jsonEqual(state.hookStatus, hook) && jsonEqual(state.qoderHookStatus, qoderHook) && jsonEqual(state.codexHookStatus, codexHook) && jsonEqual(state.workbuddyHookStatus, workbuddyHook) && jsonEqual(state.ingestStatus, ingest)
          ? state
          : { hookStatus: hook, qoderHookStatus: qoderHook, codexHookStatus: codexHook, workbuddyHookStatus: workbuddyHook, ingestStatus: ingest }
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

  async checkMemoryMcp() {
    try {
      const targets: MemoryMcpTarget[] = ['codex_cli', 'claude_cli', 'codex_desktop', 'claude_desktop', 'qoder', 'workbuddy', 'minimax', 'kimi', 'zcode']
      const statuses = await Promise.all(targets.map((agentType) => invoke<MemoryMcpStatus>('memory_mcp_status', { agentType })))
      const next = Object.fromEntries(statuses.map((status) => [status.agent_type, status])) as Record<MemoryMcpTarget, MemoryMcpStatus>
      set((state) => (jsonEqual(state.memoryMcp, next) ? state : { memoryMcp: next }))
    } catch {
      /* CLI may not be installed yet; retain the last visible state. */
    }
  },

  async installMemoryMcp(agentType) {
    const status = await invoke<MemoryMcpStatus>('memory_mcp_install', { agentType })
    set((state) => ({ memoryMcp: { ...state.memoryMcp, [agentType]: status } }))
    return status
  },

  async uninstallMemoryMcp(agentType) {
    await invoke('memory_mcp_uninstall', { agentType })
    await get().checkMemoryMcp()
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
    const result = await invoke<OrganizeConversationsResult>('memory_ingest_organize_conversations')
    // This action writes new SQLite memories; bypass the in-session page
    // cache so the count and list change immediately after a successful run.
    await Promise.all([get().checkTelemetry(), get().listMemories(true), get().checkIngest()])
    return result
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
    const result = await invoke<OrganizeConversationsResult>('memory_ingest_organize_session', { eventKey })
    await Promise.all([get().checkTelemetry(), get().listMemories(true), get().checkIngest()])
    return result
  },

  async importMemoryFolder(folder) {
    const result = await invoke<MemoryImportResult>('memory_import_folder', { folder })
    await Promise.all([get().listMemories(true), get().checkTelemetry(), get().checkIngest()])
    return result
  },

  async checkTelemetry(options = {}) {
    try {
      if (options.backfill) await invoke<number>('telemetry_backfill_conversations')
      const usageRefresh = options.refreshUsage
        ? await invoke<TelemetryUsageRefresh>('telemetry_refresh_usage')
        : null
      // The summary is the primary UI. Live status is a supplementary query,
      // so a migration/older backend must never hide the Token panel merely
      // because that supplementary query is temporarily unavailable.
      const [summaryResult, eventsResult, usageRecordsResult, liveResult] = await Promise.allSettled([
        invoke<TelemetrySummary>('telemetry_summary'),
        invoke<TelemetryEvent[]>('telemetry_recent_events', { limit: options.limit ?? 20 }),
        invoke<TelemetryUsageRecord[]>('telemetry_usage_records', { limit: Math.max(options.limit ?? 20, 200) }),
        invoke<TelemetryLiveStatus>('telemetry_live_status'),
      ])
      set((state) => {
        const next = {
          telemetrySummary: summaryResult.status === 'fulfilled' ? summaryResult.value : state.telemetrySummary,
          telemetryEvents: eventsResult.status === 'fulfilled' ? eventsResult.value : state.telemetryEvents,
          telemetryUsageRecords: usageRecordsResult.status === 'fulfilled' ? usageRecordsResult.value : state.telemetryUsageRecords,
          telemetryLiveStatus: liveResult.status === 'fulfilled' ? liveResult.value : state.telemetryLiveStatus,
        }
        return jsonEqual(state.telemetrySummary, next.telemetrySummary) && jsonEqual(state.telemetryEvents, next.telemetryEvents) && jsonEqual(state.telemetryUsageRecords, next.telemetryUsageRecords) && jsonEqual(state.telemetryLiveStatus, next.telemetryLiveStatus)
          ? state
          : next
      })
      return usageRefresh
    } catch {
      /* 浏览器预览下无 Tauri 后端，忽略 */
      return null
    }
  },

  async loadUsageAnalytics(filters) {
    return invoke<TelemetryUsageAnalytics>('telemetry_usage_analytics', {
      startAt: filters.startAt ?? null,
      endAt: filters.endAt ?? null,
      source: filters.source && filters.source !== 'all' ? filters.source : null,
      bucket: filters.bucket,
      recordLimit: 500,
    })
  },

  async checkMcpAccessLogs() {
    try {
      const logs = await invoke<McpAccessLog[]>('memory_mcp_access_logs', { limit: 30 })
      set((state) => (jsonEqual(state.mcpAccessLogs, logs) ? state : { mcpAccessLogs: logs }))
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

  async scanSkills() {
    const imported = await invoke<SkillItem[]>('skill_scan')
    // 扫描返回值仅用于提示数量；列表数据以共享库缓存为准重新加载，
    // 与其他变更操作保持一致，避免把中间态直接当成目录状态。
    await get().loadSkills(true)
    return imported
  },

  async loadSkills(force = false) {
    if (!force && get().skillCacheReady) return get().skills
    const skills = await invoke<SkillItem[]>('skill_list')
    set({ skills, skillCacheReady: true })
    return skills
  },

  async readSkill(source, name) {
    return invoke<SkillDocument>('skill_read', { source, name })
  },

  async skillDriftDetail(target, source, name) {
    return invoke<SkillDriftFile[]>('skill_drift_detail', { target, source, name })
  },

  async publishedDrift() {
    return invoke<SkillPublishedDrift[]>('skill_published_drift')
  },

  async adoptLocalSkill(target, source, name) {
    const result = await invoke<SkillAdoptResult>('skill_adopt_local', { target, source, name })
    set({ skills: get().skills.map(s => s.source === result.item.source && s.name === result.item.name ? result.item : s) })
    return result
  },

  async applySkillSyncOne(target, source, name) {
    const item = await invoke<SkillItem>('skill_sync_apply_one', { target, source, name })
    set({ skills: get().skills.map(s => s.source === item.source && s.name === item.name ? item : s) })
    return item
  },

  async previewSkillSync(target) {
    return invoke<SkillSyncPreview>('skill_sync_preview', { target })
  },

  async applySkillSync(target, overwrite) {
    const result = await invoke<SkillSyncPreview>('skill_sync_apply', { target, overwrite })
    return result
  },

 async setSkillStatus(source, name, status) {
   const item = await invoke<SkillItem>('skill_set_status', { source, name, status })
   set({ skills: get().skills.map(s => s.source === item.source && s.name === item.name ? item : s) })
   return item
 },

 async setSkillAssignment(source, name, target, equipped) {
   const item = await invoke<SkillItem>('skill_set_assignment', { source, name, target, equipped })
   set({ skills: get().skills.map(s => s.source === item.source && s.name === item.name ? item : s) })
   return item
 },

 async setSkillStatusBulk(items, status) {
   const updated = await invoke<SkillItem[]>('skill_set_status_bulk', { items, status })
   await get().loadSkills(true)
   return updated
 },

 async setSkillAssignmentBulk(items, target, equipped) {
   const updated = await invoke<SkillItem[]>('skill_set_assignment_bulk', { items, target, equipped })
   await get().loadSkills(true)
   return updated
 },

 async publishSkill(source, name, assignedAgents) {
   const item = await invoke<SkillItem>('skill_publish', { source, name, assignedAgents })
   set({ skills: get().skills.map(s => s.source === item.source && s.name === item.name ? item : s) })
   return item
 },

 async rollbackSkillLatest(source, name) {
   const item = await invoke<SkillItem>('skill_rollback_latest', { source, name })
   set({ skills: get().skills.map(s => s.source === item.source && s.name === item.name ? item : s) })
   return item
 },

 async deleteSkill(source, name) {
   await invoke('skill_delete', { source, name })
   set({ skills: get().skills.filter(s => !(s.source === source && s.name === name)) })
 },

  // ── MCP 库 ──

  async loadMcpCatalog() {
    const catalog = await invoke<McpCatalogEntry[]>('mcp_catalog_list')
    set((state) => (jsonEqual(state.mcpCatalog, catalog) ? state : { mcpCatalog: catalog }))
    return catalog
  },

  async upsertMcpEntry(entry) {
    const saved = await invoke<McpCatalogEntry>('mcp_catalog_upsert', { entry })
    set((state) => {
      const exists = state.mcpCatalog.some((e) => e.name === saved.name)
      const mcpCatalog = exists
        ? state.mcpCatalog.map((e) => (e.name === saved.name ? saved : e))
        : [...state.mcpCatalog, saved]
      return { mcpCatalog }
    })
    return saved
  },

  async importMcpEntry(name, agent) {
    const saved = await invoke<McpCatalogEntry>('mcp_catalog_import', { name, agent })
    set((state) => ({
      mcpCatalog: state.mcpCatalog.some(entry => entry.name === saved.name)
        ? state.mcpCatalog.map(entry => entry.name === saved.name ? saved : entry)
        : [...state.mcpCatalog, saved],
    }))
    // Import succeeded. Status failures are retained separately for the UI.
    await get().checkMcpStatuses().catch(() => {})
    return saved
  },

  async deleteMcpEntry(name) {
    await invoke('mcp_catalog_delete', { name })
    set((state) => ({
      mcpCatalog: state.mcpCatalog.filter((e) => e.name !== name),
      mcpStatuses: state.mcpStatuses.filter((s) => s.name !== name),
    }))
  },

  async equipMcp(name, agent) {
    const saved = await invoke<McpCatalogEntry>('mcp_equip', { name, agent })
    set((state) => ({ mcpCatalog: state.mcpCatalog.map((e) => (e.name === saved.name ? saved : e)) }))
    // The write succeeded; status errors remain visible through mcpStatusError.
    await get().checkMcpStatuses().catch(() => {})
    return saved
  },

  async unequipMcp(name, agent) {
    const saved = await invoke<McpCatalogEntry>('mcp_unequip', { name, agent })
    set((state) => ({ mcpCatalog: state.mcpCatalog.map((e) => (e.name === saved.name ? saved : e)) }))
    // The write succeeded; status errors remain visible through mcpStatusError.
    await get().checkMcpStatuses().catch(() => {})
    return saved
  },

  async syncMcpAgent(agent) {
    await invoke('mcp_sync_agent', { agent })
    await get().checkMcpStatuses().catch(() => {})
  },

  async checkMcpStatuses() {
    try {
      const statuses = await invoke<McpAgentStatus[]>('mcp_status_all')
      set((state) => (jsonEqual(state.mcpStatuses, statuses) && !state.mcpStatusError
        ? state : { mcpStatuses: statuses, mcpStatusError: '' }))
    } catch (error) {
      set({ mcpStatusError: String(error) })
      throw error
    }
  },

  async importMcpFromAgents() {
    return invoke<McpImportCandidate[]>('mcp_import_from_agents')
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
    await get().checkTelemetry()
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
  },
}))
