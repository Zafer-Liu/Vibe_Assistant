use super::metrics::{AggregateMetrics, RankingMetrics};
use serde::{Deserialize, Serialize};
use serde_json::Value;

fn is_false(value: &bool) -> bool { !*value }

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Dataset {
    pub schema_version: u32,
    pub dataset_id: String,
    pub name: String,
    pub description: String,
    pub synthetic: bool,
    pub domains: Vec<DatasetDomain>,
    pub memories: Vec<DatasetMemory>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sources: Vec<DatasetSource>,
    pub cases: Vec<EvaluationCase>,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct DatasetDomain {
    pub id: String,
    pub name: String,
    pub description: String,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct DatasetMemory {
    pub id: String,
    pub content: String,
    pub domains: Vec<String>,
    pub memory_type: String,
    pub durability: String,
    pub event_time: Option<String>,
    pub source_event_key: String,
    pub source_agent: String,
    pub session_id: String,
    #[serde(default, skip_serializing_if = "is_false")]
    pub user_defined: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin_device: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub provenance: Vec<DatasetProvenance>,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct DatasetSource {
    pub event_key: String,
    pub source_agent: String,
    pub session_id: String,
    pub occurred_at: String,
    pub text: String,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct DatasetProvenance {
    pub source_id: String,
    pub source_event_key: String,
    pub source_agent: String,
    pub session_id: String,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ExpectedEvidence {
    pub source_event_key: String,
    pub contains: String,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct EvaluationCase {
    pub case_id: String,
    pub category: String,
    pub split: String,
    pub query: String,
    pub limit: usize,
    pub domain_mode: String,
    #[serde(default)]
    pub domains: Vec<String>,
    pub expected_ids: Vec<String>,
    #[serde(default)]
    pub forbidden_ids: Vec<String>,
    #[serde(default)]
    pub expected_domain_ids: Vec<String>,
    pub answerable: bool,
    #[serde(default)]
    pub expected_source_event_keys: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub expected_evidence: Vec<ExpectedEvidence>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_evidence_status: Option<String>,
}

#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct DatasetSummary {
    pub dataset_id: String,
    pub name: String,
    pub description: String,
    pub synthetic: bool,
    pub memory_count: usize,
    pub case_count: usize,
    pub development_count: usize,
    pub holdout_count: usize,
}

#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct BackendInfo {
    pub embedding_model: Option<String>,
    pub embedding_location: String,
    pub routing_model: Option<String>,
    pub routing_provider: String,
    pub routing_remote: bool,
}

#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct EvaluationPlan {
    pub plan_id: String,
    pub dataset_id: String,
    pub dataset_name: String,
    pub synthetic: bool,
    pub dataset_fingerprint: String,
    pub split: String,
    pub mode: String,
    pub memory_count: usize,
    pub case_count: usize,
    pub embedding_batches: usize,
    pub query_calls: usize,
    pub routing_calls: usize,
    pub remote_calls_estimate: usize,
    pub budget_ms: u64,
    pub backend: BackendInfo,
    pub expires_at: String,
}

#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct CaseResult {
    pub case_id: String,
    pub category: String,
    pub split: String,
    pub query: String,
    pub status: String,
    pub error: Option<String>,
    pub expected_ids: Vec<String>,
    pub forbidden_ids: Vec<String>,
    pub returned_ids: Vec<String>,
    pub expected_domain_ids: Vec<String>,
    pub active_domains: Vec<String>,
    pub metrics: RankingMetrics,
    pub domain_hit: Option<bool>,
    pub degraded: bool,
    pub index_complete: bool,
    pub retrieval: Value,
    pub elapsed_ms: u64,
    pub returned_chars: usize,
    #[serde(default)]
    pub evidence_coverage: Option<f64>,
    #[serde(default)]
    pub evidence_status_hit: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence_ms: Option<u64>,
}

#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct EvaluationReport {
    pub schema_version: u32,
    pub run_id: String,
    pub dataset_id: String,
    pub dataset_name: String,
    pub synthetic: bool,
    pub dataset_fingerprint: String,
    pub split: String,
    pub mode: String,
    pub status: String,
    pub planned_count: usize,
    pub started_at: String,
    pub finished_at: Option<String>,
    pub preparation_ms: u64,
    pub backend: BackendInfo,
    pub cases: Vec<CaseResult>,
    pub summary: AggregateMetrics,
}

#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct RunStatus {
    pub run_id: String,
    pub status: String,
    pub completed: usize,
    pub total: usize,
    pub prepared: usize,
    pub total_memories: usize,
    pub error: Option<String>,
}
