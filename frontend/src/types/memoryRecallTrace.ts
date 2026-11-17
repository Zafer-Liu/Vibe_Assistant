import type { MemoryRetrievalDiagnostics, MemoryRetrievalResponse, MemoryRetrievalRouting } from './memory'

export interface TraceNomination {
  route: 'semantic' | 'keyword'
  memory_id: string
  input_rank: number
  eligible_rank: number | null
  score: number | null
  status: 'accepted' | 'missing_current_memory' | 'scope_mismatch' | 'content_mismatch' | 'invalid_score' | 'duplicate_in_route'
}

export interface TraceRanking {
  memory_id: string
  domains: string[]
  semantic_rank: number | null
  keyword_rank: number | null
  semantic_score: number | null
  keyword_score: number | null
  fused_score: number
  selected_rank: number | null
  diversity_penalty: number
  selection_score: number
  outcome: 'selected' | 'not_selected_at_limit'
}

export interface TraceResult {
  memory_id: string
  fingerprint: string
  domains: string[]
  source_event_key: string
  source_agent: string
  session_id: string
  event_time: string | null
  updated_at: string
}

export interface RecallTrace {
  schema_version: number
  request_id: string
  created_at: string
  entrypoint: string
  query: string
  arguments: { query: string; limit: number; domain_mode: 'auto' | 'manual' | 'off'; domains?: string[] }
  routing: MemoryRetrievalRouting & { confidence?: number | null; fallback?: string | null }
  retrieval: MemoryRetrievalDiagnostics
  candidates: TraceNomination[]
  ranking: TraceRanking[]
  results: TraceResult[]
  truncated: boolean
}

export interface RecallTraceSummary {
  request_id: string
  created_at: string
  entrypoint: string
  query: string
  active_domains: string[]
  mode: string
  selected_count: number
  total_ms: number
  truncated: boolean
}

export interface TraceMemoryView {
  memory_id: string
  state: 'unchanged' | 'changed' | 'deleted'
  content: string | null
  domains: string[]
  source_event_key: string | null
  source_agent: string | null
  session_id: string | null
}

export interface RecallTraceDetail {
  trace: RecallTrace
  current_memories: TraceMemoryView[]
}

export type TraceRerun = (response: MemoryRetrievalResponse) => void
