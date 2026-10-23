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
  conversation_state: 'full' | 'partial' | 'covered' | 'unavailable'
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
