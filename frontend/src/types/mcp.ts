import type { MemoryInjectionLog } from './memoryInjection'

/** User-level shared-memory MCP installation status for an external Agent. */
export interface MemoryMcpStatus {
  agent_type: MemoryMcpTarget
  installed: boolean
  executable: string
  detail: string
}

export interface MemoryMcpRuntimeStatus {
  agent_type: MemoryMcpTarget
  configured: boolean
  last_initialize_at: string | null
  last_tool_call_at: string | null
  probe_state: 'not_probed'
  probe_checked_at: string | null
  detail: string
}

export type MemoryMcpTarget = 'codex_cli' | 'claude_cli' | 'codex_desktop' | 'claude_desktop' | 'qoder' | 'workbuddy' | 'minimax' | 'kimi' | 'zcode' | 'dsh'

/** Shared event shape used by the MCP connection center's passive activity rows. */
export type McpAccessLog = MemoryInjectionLog

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

/** 库条目在已装备或发现同名配置的 Agent 上的比较结果。 */
export interface McpAgentStatus {
  name: string
  agent: string
  state: 'installed' | 'missing' | 'differs' | 'unavailable'
  differing_fields: string[]
  error: string | null
}

export type McpObservationState = 'off' | 'awaiting_connection' | 'observing' | 'fallback' | 'drifted' | 'recovery' | 'unsupported'

/** Live stdio observation for a catalog assignment. */
export interface McpObservationStatus {
  name: string
  agent: string
  state: McpObservationState
  active_connections: number
}

/** dsh 桥接插件的特性开关（Rust 侧覆盖行未写时默认全开）。 */
export interface DshBridgeFeatures {
  injection: boolean
  observation: boolean
  usage: boolean
  collect: boolean
  tools: boolean
}

/** 统一桥接插件（dsh-vibe-assistant-bridge）的安装状态；对全部 MCP 服务生效。 */
export interface DshBridgeStatus {
  installed: boolean
  version: string | null
  detail: string
  features: DshBridgeFeatures
  legacy_memory_inject: boolean
  legacy_mcp_observe: boolean
}

/** 「从 Agent 导入」在某个 Agent 配置里发现的候选服务器。 */
export interface McpImportCandidate {
  agent: string
  entry: McpCatalogEntry
  already_in_catalog: boolean
  catalog_state: 'new' | 'matches' | 'differs'
  differing_fields: string[]
  variant: number
  variant_count: number
  peer_differing_fields: string[]
}

export interface McpImportIssue {
  agent: string
  name: string | null
  message: string
}

export interface McpImportScan {
  candidates: McpImportCandidate[]
  issues: McpImportIssue[]
}

export interface McpComparisonSource {
  agent: string
  entry: McpCatalogEntry | null
  state: 'installed' | 'differs' | 'missing' | 'unavailable' | 'observed' | 'unmanaged'
  assigned: boolean
  observed: boolean
  error: string | null
}

export interface McpConfigurationComparison {
  name: string
  catalog: McpCatalogEntry | null
  sources: McpComparisonSource[]
  revision: string
  blocked_reason: string | null
}

export interface McpArbitrationResult {
  entry: McpCatalogEntry
  catalog_updated: boolean
  applied_agents: string[]
  errors: McpImportIssue[]
}

/** MCP 配置档案：已命名的一组启用服务器名（引用中央目录条目）。 */
export interface McpProfile {
  name: string
  description: string
  servers: string[]
  created_at: string
  updated_at: string
}

/** A sanitized transcript-observed MCP tool call. */
export interface McpCallObservation {
  id: number
  source: string
  session_id: string
  occurred_at: string
  server_name: string
  tool_name: string
  status: 'success' | 'failed' | 'pending' | 'unknown'
  origin: 'stdio_proxy' | 'stdio_relay' | 'transcript' | 'memory_mcp' | 'dsh_plugin'
  error_category?: string | null
  result_observed: boolean
  /** 有界失败元数据；旧记录或无可靠证据的来源为空。 */
  error_phase?: 'request' | 'response' | 'transport' | null
  failure_kind?: 'parse_error' | 'rpc_error' | 'tool_error' | 'transport_closed' | 'unknown' | null
  error_code?: number | null
  error_reason?: string | null
}

export interface McpCallGroup {
  name: string
  server_name?: string | null
  calls: number
  success: number
  failed: number
  pending: number
  unknown: number
}

export type McpCallCoverageState = 'observed' | 'observed_empty' | 'unavailable' | 'unsupported' | 'not_configured'

export interface McpCallCoverage {
  source: string
  state: McpCallCoverageState | string
  transcript_count: number
  call_count: number
  scanned_at?: string | null
  reason?: string | null
}

export interface McpFailedCallPage {
  total: number
  snapshot_id: number
  categories: { category: string; count: number }[]
  calls: McpCallObservation[]
}

/** 服务通过独立探测公布的工具清单（仅显式刷新时获取，不持久化）。 */
export interface McpToolDiscovery {
  name: string
  transport: string
  state: 'complete' | 'empty' | 'partial' | 'unsupported' | 'error'
  tools: { name: string; description: string }[]
  fetched_at: string
  complete: boolean
  reason?: string | null
}

export interface McpToolHealth {
  server_name: string
  tool_name: string
  successful_calls: number
  failed_calls: number
}

export interface McpDashboardStats {
  total_calls: number
  health_start_at: string
  health_end_at: string
  successful_calls_7d: number
  failed_calls_7d: number
  high_failure_tools: McpToolHealth[]
}

export interface McpCallStats {
  total_calls: number
  proxy_calls: number
  transcript_calls: number
  /** DeepSeek Harness 插件上报的调用数。 */
  plugin_calls: number
  /** 排除 proxy / transcript / plugin 后的原生记录数。 */
  native_calls: number
  successful_calls: number
  failed_calls: number
  pending_calls: number
  unknown_calls: number
  server_count: number
  tool_count: number
  last_observed_at?: string | null
  servers: McpCallGroup[]
  tools: McpCallGroup[]
  recent: McpCallObservation[]
  coverage: McpCallCoverage[]
}

export interface McpCallRefresh {
  imported: boolean
  scanned_transcripts: number
  updated_transcripts: number
  observed_calls: number
  message: string
}

/** Presentation model for existing memory-MCP activity in the Connection Center. */
export interface McpActivityRow {
  id: number
  occurredAt: string
  clientName: string
  toolName: string
  summary: string
  detail: string | null
  success: boolean
}

/** 目录扫描结果（复用后端 scan_mcp_local，不传 provider 即不调 LLM）。 */
export interface McpScanResult {
  transport: string
  name: string
  command: string
  args: string[]
  env: Record<string, string>
  url: string
  headers: Record<string, string>
  description: string
  warnings: string[]
  confidence: number
}

// ── Claude Desktop 配置路径（仅保存 Vibe Assistant 内部偏好） ──────────────────

export interface ClaudeDesktopMcpConfig {
  candidates: { path: string; exists: boolean }[]
  selected_path: string | null
  override_path: string | null
  needs_selection: boolean
}
