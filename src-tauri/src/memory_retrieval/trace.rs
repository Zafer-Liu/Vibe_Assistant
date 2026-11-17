//! Bounded explanations of the actual selection pass. Never stores L0 or L1 text.
use crate::telemetry_store::LocalMemorySnapshot;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

pub(crate) const MAX_TRACE_BYTES: usize = 64 * 1024;

#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct TraceNomination {
    pub route: String,
    pub memory_id: String,
    pub input_rank: usize,
    pub eligible_rank: Option<usize>,
    pub score: Option<f64>,
    pub status: String,
}

#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct TraceRanking {
    pub memory_id: String,
    pub domains: Vec<String>,
    pub semantic_rank: Option<usize>,
    pub keyword_rank: Option<usize>,
    pub semantic_score: Option<f64>,
    pub keyword_score: Option<f64>,
    pub fused_score: f64,
    pub selected_rank: Option<usize>,
    pub diversity_penalty: f64,
    pub selection_score: f64,
    pub outcome: String,
}

#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct TraceResult {
    pub memory_id: String,
    pub fingerprint: String,
    pub domains: Vec<String>,
    pub source_event_key: String,
    pub source_agent: String,
    pub session_id: String,
    pub event_time: Option<String>,
    pub updated_at: String,
}

#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct RecallTrace {
    pub schema_version: u32,
    pub request_id: String,
    pub created_at: String,
    pub entrypoint: String,
    pub query: String,
    pub arguments: Value,
    pub routing: Value,
    pub retrieval: Value,
    pub candidates: Vec<TraceNomination>,
    pub ranking: Vec<TraceRanking>,
    pub results: Vec<TraceResult>,
    pub truncated: bool,
}

#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct RecallTraceSummary {
    pub request_id: String,
    pub created_at: String,
    pub entrypoint: String,
    pub query: String,
    pub active_domains: Vec<String>,
    pub mode: String,
    pub selected_count: usize,
    pub total_ms: u64,
    pub truncated: bool,
}

#[derive(Serialize, Deserialize)]
pub(crate) struct TraceMemoryView {
    pub memory_id: String,
    pub state: String,
    pub content: Option<String>,
    pub domains: Vec<String>,
    pub source_event_key: Option<String>,
    pub source_agent: Option<String>,
    pub session_id: Option<String>,
}

#[derive(Serialize, Deserialize)]
pub(crate) struct RecallTraceDetail {
    pub trace: RecallTrace,
    pub current_memories: Vec<TraceMemoryView>,
}

pub(crate) fn fingerprint(memory: &LocalMemorySnapshot, domains: &[String]) -> String {
    let mut domains = domains.to_vec();
    domains.sort();
    domains.dedup();
    let bytes = serde_json::to_vec(&(memory, domains)).expect("memory snapshot serializes");
    format!("{:x}", Sha256::digest(bytes))
}

impl RecallTrace {
    /// Preserve final IDs, fingerprints and request scope; trim diagnostic detail only.
    pub(crate) fn bounded_json(&self) -> Result<String, String> {
        let mut bounded = self.clone();
        loop {
            bounded.retrieval["trace_truncated"] = Value::Bool(bounded.truncated);
            let text = serde_json::to_string(&bounded).map_err(|error| error.to_string())?;
            if text.len() <= MAX_TRACE_BYTES { return Ok(text); }
            if bounded.candidates.pop().is_none() && bounded.ranking.pop().is_none() {
                return Err("recall trace metadata exceeds storage limit".into());
            }
            bounded.truncated = true;
        }
    }

    pub(crate) fn summary(&self) -> RecallTraceSummary {
        RecallTraceSummary {
            request_id: self.request_id.clone(), created_at: self.created_at.clone(),
            entrypoint: self.entrypoint.clone(), query: self.query.clone(),
            active_domains: self.routing["active_domains"].as_array().map(|items| {
                items.iter().filter_map(|item| item.as_str().map(str::to_string)).collect()
            }).unwrap_or_default(),
            mode: self.retrieval["mode"].as_str().unwrap_or("").to_string(),
            selected_count: self.results.len(),
            total_ms: self.retrieval["timings_ms"]["total"].as_u64().unwrap_or(0),
            truncated: self.truncated,
        }
    }
}
