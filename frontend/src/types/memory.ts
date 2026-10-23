/** Vibe Assistant 内置本地语义索引类型定义 */

export interface MemoryConfig {
  /** Vibe Assistant 私有本机 sidecar 地址（仅内部使用） */
  baseUrl: string
  /** API Key（Bearer 认证） */
  apiKey: string
  /** 记忆归属用户 ID */
  userId: string
}

/** get / search 返回的记忆条目 */
export interface MemoryItem {
  id: string
  memory: string
  memory_type?: string
  durability?: 'session' | 'short_term' | 'long_term'
  /** 用户手动添加的自定义记忆：只有用户能删改，自动整理会跳过。 */
  user_defined?: boolean
  last_update_at?: string
  event_time?: string | null
  score?: number | null
  retrieval_source?: 'memory' | 'conversation' | 'local'
  retrieval_method?: 'semantic' | 'keyword' | 'local'
  session_id?: string
  source_agent?: string
  occurred_at?: string
}

export interface MemoryImportance {
  memory_id: string
  score: number
  supporting_sessions: number
  supporting_agents: number
  duplicate_count: number
  recall_count: number
  pinned: boolean
  updated_at: string
}

export interface LocalMemoryStats {
  total: number
  facts: number
  preferences: number
  session_only: number
  short_term: number
  long_term: number
}

export interface L1ResetResult {
  cleared_l1: number
  cleared_derived_documents: number
  requeued_conversations: number
}

export interface MemoryImportanceSummary {
  reviewed: number
  high: number
  medium: number
  low: number
  message: string
}

export type MemorySearchMode = 'strict' | 'balanced' | 'broad'

export interface ConversationSearchHit {
  id: number
  source: string
  session_id: string
  occurred_at: string
  score: number
  preview: string
}

export interface MemoryLayerDocument {
  id: string
  layer: 'l2' | 'l3'
  scope: string
  content: string
  state: 'draft' | 'published' | 'archived' | 'failed'
  token_estimate: number
  source_count: number
  window_start?: string | null
  window_end?: string | null
  created_at: string
  published_at?: string | null
}

export interface MemoryLayerRunResult {
  document: MemoryLayerDocument
  selected_l1_count: number
  stage_count: number
  message: string
}

/** add 返回的抽取事件条目 */
export interface AddEventItem {
  operation?: string
  memory_id?: string
  content?: string
}

export interface AddResult {
  memories: AddEventItem[]
}

export interface MemoryListResult {
  memories: MemoryItem[]
}

/** 内置记忆引擎组件在线状态（来自 Tauri 后端） */
export interface ComponentStatus {
  name: string
  online: boolean
  detail: string
}

export interface EngineStatus {
  online: boolean
  api_port: number
  components: ComponentStatus[]
}

export interface ConsolidationResult {
  snapshot_id: string
  before_count: number
  after_count: number
  scopes: number
  clusters: number
  actions: number
  message: string
}

/** A locally selected BGE semantic-neighbour batch for safe LLM consolidation. */
export interface ConsolidationCandidate {
  id: string
  memory: string
  memory_type?: string
}

export type HealthState = 'unknown' | 'online' | 'offline'

/** 自动沉淀管道（hook 回调）相关类型 */
export interface IngestLog {
  at: string
  agent_id: string
  kind: 'memory' | 'skill'
  state: 'working' | 'stored' | 'retrying' | 'failed' | 'cancelled'
  detail: string
}

export interface HookStatus {
  installed: boolean
  agent_type: string
  events: string[]
}

export interface ExtractionProgress {
  total: number
  attempted: number
  succeeded: number
  failed: number
  batch_index: number
  batch_total: number
}

export interface IngestStatus {
  enabled: boolean
  buffered_sessions: number
  model_provider_id: string | null
  model_ready: boolean
  manual_extraction_running: boolean
  manual_extraction_cancelling: boolean
  extraction_progress: ExtractionProgress
  recent: IngestLog[]
}

/** Result of importing a user-selected folder of exported Agent memories. */
export interface MemoryImportResult {
  folder: string
  scanned_files: number
  recognized_files: number
  skipped_files: number
  imported_memories: number
  message: string
}

export interface OrganizeConversationsResult {
  attempted: number
  succeeded: number
  failed: number
  failure_reasons: string[]
  cancelled: boolean
}

/** 「待提取记忆」面板的一行：已完成但尚未成功提炼为记忆的会话。 */
export interface PendingMemorySession {
  event_key: string
  source: string
  session_id: string
  occurred_at: string
  message_count: number
  l1_state: string
  error: string | null
  excerpt: string
}

/** 面板弹窗展示的完整 sanitized 对话（仅 user/assistant 正文）。 */
export interface MemoryConversationDetail {
  event_key: string
  source: string
  session_id: string
  occurred_at: string
  message_count: number
  l1_state: string
  error: string | null
  conversation_text: string
}

// Compatibility exports for consumers outside the feature modules.
export type * from './agentSources'
export type * from './mcp'
export type * from './memoryInjection'
export type * from './telemetry'
export type * from './skill'
