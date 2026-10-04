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

/** Agent 数据源（转录目录 / MCP 配置）的当前生效路径，支持按设备覆盖。 */
export interface AgentSourcePathInfo {
  path: string
  exists: boolean
  is_override: boolean
}

export interface AgentSourceInfo {
  id: string
  label: string
  supports_hooks: boolean
  transcript_roots: AgentSourcePathInfo[]
  default_transcript_roots: string[]
  mcp_config_path?: string | null
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
  state: 'working' | 'stored' | 'retrying' | 'failed'
  detail: string
}

export interface HookStatus {
  installed: boolean
  agent_type: string
  events: string[]
}

/** User-level shared-memory MCP installation status for an external Agent. */
export interface MemoryMcpStatus {
  agent_type: MemoryMcpTarget
  installed: boolean
  executable: string
  detail: string
}

export type MemoryMcpTarget = 'codex_cli' | 'claude_cli' | 'codex_desktop' | 'claude_desktop' | 'qoder' | 'workbuddy' | 'minimax' | 'kimi' | 'zcode'

export interface IngestStatus {
  enabled: boolean
  buffered_sessions: number
  model_provider_id: string | null
  model_ready: boolean
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

/** One external Agent memory-injection record (MCP tool or SessionStart hook),
 *  with the full exchanged content kept in `detail` for replay. */
export interface McpAccessLog {
  id: number
  occurred_at: string
  client_name: string
  tool_name: string
  summary: string
  detail?: string | null
  success: boolean
}

/** Per-agent injection-cost rollup over the audit window. `prompt_skips` counts
 *  turns whose rendered context fingerprint was unchanged and were therefore
 *  not re-injected — the hit rate of the per-turn injection gate. */
export interface MemoryInjectionStats {
  client_name: string
  session_injections: number
  prompt_injections: number
  prompt_skips: number
  injected_chars: number
}

export interface TelemetrySummary {
  events: number
  sessions: number
  input_tokens: number
  output_tokens: number
  cached_tokens: number
  total_tokens: number
  reported_events: number
  estimated_events: number
  usage_sessions: number
  unavailable_usage_sessions: number
  usage_refreshed_at: string | null
}

export interface TelemetryUsageRefresh {
  scanned_sessions: number
  updated_sessions: number
  unavailable_sessions: number
  message: string
}

export interface TelemetryUsageBucket {
  label: string
  input_tokens: number
  output_tokens: number
  cached_tokens: number
  record_count: number
}

export interface TelemetryUsageHeatCell {
  /** Local-time weekday returned by SQLite: 0 = Sunday, 6 = Saturday. */
  weekday: number
  hour: number
  input_tokens: number
  output_tokens: number
  record_count: number
}

export interface TelemetryUsageCostGroup {
  source: string
  model: string | null
  input_tokens: number
  output_tokens: number
  cached_tokens: number
  estimated_tokens: number
}

export interface TelemetryUsageAnalytics {
  record_count: number
  input_tokens: number
  output_tokens: number
  cached_tokens: number
  /** Tokens from estimated origins (optional: older backends omit it). */
  estimated_tokens?: number
  /** False 当所选范围只有估算/远端总量行（无缓存计数）；旧后端缺省视为 true。 */
  cache_capable?: boolean
  records: TelemetryUsageRecord[]
  truncated_records: boolean
  buckets: TelemetryUsageBucket[]
  heatmap?: TelemetryUsageHeatCell[]
  cost_groups?: TelemetryUsageCostGroup[]
  sources: string[]
}

/** Live local-ledger state, refreshed by the Memory Center polling loop. */
export interface TelemetryLiveStatus {
  captured_sessions: number
  active_sessions: number
  /** 活跃会话对应的 Agent 来源（如 claude / codex），无活跃会话时为空。 */
  active_sources?: string[]
  completed_conversations: number
  organized_memory_conversations: number
  pending_memory_sessions: number
  retrying_memory_sessions: number
  failed_memory_sessions: number
  pending_usage_sessions: number
  failed_transcript_scans: number
  last_event_at: string | null
}

/** Durable local hook record.  It is retained even when memory extraction is offline. */
export interface TelemetryEvent {
  id: number
  source: string
  session_id: string
  event_type: string
  occurred_at: string
  token_source: 'reported' | 'estimated' | 'unavailable'
  input_tokens: number
  output_tokens: number
  cached_tokens: number
  conversation_state: 'full' | 'partial' | 'unavailable'
  conversation_message_count: number
  conversation_text: string | null
  l1_state: 'unavailable' | 'pending' | 'stored' | 'retrying'
}

/** A request/response observation reconstructed from a native transcript.
 * `session_total` is an explicit fallback for agents that do not expose
 * request-level provider counters. */
export interface TelemetryUsageRecord {
  record_id: string
  source: string
  session_id: string
  occurred_at: string
  model: string | null
  record_kind: 'response' | 'session_total'
  input_tokens: number
  output_tokens: number
  cached_tokens: number
  origin: string
}

export interface SkillItem {
  source: string
  name: string
  description: string
  path: string
  hash: string
  version: number
  status: 'draft' | 'published'
  assigned_agents: string[]
  /** Skill 目录内打包发布的全部文件（相对路径，含 SKILL.md） */
  files?: string[]
  /** 技能性质：builtin（厂商内置）/ plugin（插件市场安装）/ local（本地
   *  自建或 Vibe Assistant 部署）/ marketplace（技能市场导入）。旧数据缺省
   *  时按 local 处理。 */
  origin?: 'builtin' | 'plugin' | 'local' | 'marketplace'
}

export interface SkillSyncPreview {
  target: string
  create: SkillItem[]
  update: SkillItem[]
  unchanged: SkillItem[]
  conflict: SkillItem[]
  /** 已发布、应装备到该 target，但 target 本地无副本的 Skill */
  missing: SkillItem[]
}

export interface SkillDocument {
  item: SkillItem
  content: string
}

/** 一份漂移对比文件：None 表示该侧不存在此文件或非 UTF-8 文本（二进制）。 */
export interface SkillDriftFile {
  path: string
  shared_text: string | null
  local_text: string | null
}

/** 已装备 Agent 在某个已发布 Skill 上的本地同步状态。 */
export interface SkillDriftAgent {
  agent: string
  state: 'in_sync' | 'modified' | 'missing'
  changed_files: number
}

/** 一个已发布 Skill 在全部已装备 Agent 上的漂移总览（「已发布」页仲裁数据）。 */
export interface SkillPublishedDrift {
  source: string
  name: string
  agents: SkillDriftAgent[]
}

/** 采纳 Agent 本地版本后，向其他 Agent 同步的结果。 */
export interface SkillAdoptResult {
  item: SkillItem
  adopted_agent: string
  /** 成功用新共享版本覆盖的 Agent（本地此前无修改）。 */
  synced: string[]
  /** 因自身本地副本也有修改而被跳过、未覆盖的 Agent。 */
  skipped: string[]
}

export interface MarketplaceSkill {
  id: string
  source: 'openai' | 'anthropic'
  source_label: string
  name: string
  description: string
  repository_url: string
  skill_url: string
  revision: string
  files: string[]
}

export interface MarketplaceCatalog {
  items: MarketplaceSkill[]
  fetched_at: string
  from_cache: boolean
  warning: string | null
}

export interface MarketplaceSkillPreview {
  item: MarketplaceSkill
  content: string
}

// ── MCP 库 ───────────────────────────────────────────────────────────────────

/** MCP 库的一个条目：跨 Agent 复用的 MCP 服务器定义（name 即服务器 id）。 */
/** Runtime projection of the central MCP catalog. */
export interface McpServer {
  name: string
  command: string
  args: string[]
  env: Record<string, string>
  transport?: string
  url?: string
  headers?: Record<string, string>
  description?: string
}

export interface McpCatalogEntry {
  name: string
  description: string
  /** "stdio" | "sse" | "http" */
  transport: string
  command: string
  args: string[]
  env: Record<string, string>
  url: string
  headers: Record<string, string>
  assigned_agents: string[]
  created_at: string
  updated_at: string
}

/** 库条目在某个已装备 Agent 上的安装状态。 */
export interface McpAgentStatus {
  name: string
  agent: string
  /** "installed" | "missing" | "differs" */
  state: string
}

/** 「从 Agent 导入」在某个 Agent 配置里发现的候选服务器。 */
export interface McpImportCandidate {
  agent: string
  entry: McpCatalogEntry
  already_in_catalog: boolean
}

/** MCP 配置档案：已命名的一组启用服务器名（引用中央目录条目）。 */
export interface McpProfile {
  name: string
  description: string
  servers: string[]
  created_at: string
  updated_at: string
}
