use serde::{Deserialize, Serialize};

#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct EvidenceAccess {
    pub request_id: String,
    pub status: String,
    pub context_token: Option<String>,
    pub expires_at: Option<String>,
    pub max_chars: usize,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct EvidenceRequest {
    pub context_token: String,
    pub memory_ids: Vec<String>,
    #[serde(default)]
    pub query: Option<String>,
    #[serde(default)]
    pub max_chars: Option<usize>,
    #[serde(default)]
    pub cursor: Option<String>,
}

#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct SourceBinding {
    pub source_event_key: String,
    pub source_agent: String,
    pub session_id: String,
    pub occurred_at: Option<String>,
    pub source_fingerprint: Option<String>,
    pub status: String,
}

#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct MemoryBinding {
    pub memory_id: String,
    pub memory_fingerprint: String,
    pub domains: Vec<String>,
    pub provenance_fingerprint: String,
    pub kind: String,
    pub sources: Vec<SourceBinding>,
}

pub(crate) struct ResolvedMemory {
    pub binding: MemoryBinding,
    pub memory_text: String,
}

#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct EvidenceExcerpt {
    pub text: String,
    pub char_start: usize,
    pub char_end: usize,
    pub total_chars: usize,
    pub match_method: String,
    pub excerpt_fingerprint: String,
}

#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct EvidenceSource {
    #[serde(flatten)]
    pub source: SourceBinding,
    pub excerpt: Option<EvidenceExcerpt>,
}

#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct EvidenceItem {
    pub memory_id: String,
    pub status: String,
    pub sources: Vec<EvidenceSource>,
}

#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct EvidenceResponse {
    pub request_id: String,
    pub context_request_id: Option<String>,
    pub status: String,
    pub expires_at: Option<String>,
    pub returned_chars: usize,
    pub remaining_chars: usize,
    pub partial: bool,
    pub items: Vec<EvidenceItem>,
    pub next_cursor: Option<String>,
    pub instruction: String,
}
