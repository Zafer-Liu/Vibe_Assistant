export interface EvaluationDatasetSummary {
  dataset_id: string
  name: string
  description: string
  synthetic: boolean
  memory_count: number
  case_count: number
  development_count: number
  holdout_count: number
}

export interface EvaluationBackendInfo {
  embedding_model: string | null
  embedding_location: string
  routing_model: string | null
  routing_provider: string
  routing_remote: boolean
}

export interface EvaluationPlan {
  plan_id: string
  dataset_id: string
  dataset_name: string
  synthetic: boolean
  dataset_fingerprint: string
  split: 'development' | 'holdout' | 'all'
  mode: 'hybrid' | 'keyword'
  memory_count: number
  case_count: number
  embedding_batches: number
  query_calls: number
  routing_calls: number
  remote_calls_estimate: number
  budget_ms: number
  backend: EvaluationBackendInfo
  expires_at: string
}

export interface EvaluationRunStatus {
  run_id: string
  status: 'preparing' | 'running' | 'cancelling' | 'completed' | 'cancelled' | 'failed'
  completed: number
  total: number
  prepared: number
  total_memories: number
  error: string | null
}

export interface RankingMetrics {
  recall_at_k: number | null
  mrr: number | null
  ndcg_at_k: number | null
  duplicate_rate: number
  forbidden_rate: number
  false_positive_on_empty_query: boolean
}

export interface EvaluationCaseResult {
  case_id: string
  category: string
  split: string
  query: string
  status: string
  error: string | null
  expected_ids: string[]
  forbidden_ids: string[]
  returned_ids: string[]
  expected_domain_ids: string[]
  active_domains: string[]
  metrics: RankingMetrics
  domain_hit: boolean | null
  degraded: boolean
  index_complete: boolean
  retrieval: Record<string, unknown>
  elapsed_ms: number
  returned_chars: number
  evidence_coverage: number | null
  evidence_status_hit?: boolean | null
  evidence_ms?: number | null
}

export interface EvaluationAggregate {
  planned_count: number
  executed_count: number
  successful_count: number
  failed_count: number
  degraded_count: number
  partial_index_count: number
  not_run_count: number
  answerable_count: number
  unanswerable_count: number
  recall_at_k: number | null
  mrr: number | null
  ndcg_at_k: number | null
  duplicate_rate: number | null
  forbidden_rate: number | null
  false_positive_rate: number | null
  domain_accuracy: number | null
  p50_ms: number | null
  p95_ms: number | null
  mean_returned_chars: number | null
  evidence_coverage?: number | null
  evidence_status_accuracy?: number | null
}

export interface EvaluationReport {
  schema_version: number
  run_id: string
  dataset_id: string
  dataset_name: string
  synthetic: boolean
  dataset_fingerprint: string
  split: string
  mode: string
  status: string
  planned_count: number
  started_at: string
  finished_at: string | null
  preparation_ms: number
  backend: EvaluationBackendInfo
  cases: EvaluationCaseResult[]
  summary: EvaluationAggregate
}
