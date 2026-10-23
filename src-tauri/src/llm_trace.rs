//! M0 direct-call evidence. No prompts, response bodies, URLs or secrets are stored.
//! This is request evidence, not a second billing ledger or a routing policy engine.
use crate::llm::LlmProvider;
use crate::telemetry_store::TelemetryStore;
use chrono::Utc;
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::Instant;
use uuid::Uuid;

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ModelPurpose {
    Plan,
    Review,
    MemoryExtract,
    MemoryConsolidate,
    MemoryReview,
    ProfileBuild,
    ConnectionTest,
    Interactive,
}

#[derive(Clone, Debug)]
pub struct ModelContext {
    pub purpose: ModelPurpose,
    pub task_id: Option<String>,
    pub run_id: Option<String>,
    pub provider_namespace: ProviderNamespace,
}
impl ModelContext {
    pub fn new(purpose: ModelPurpose) -> Self {
        Self {
            purpose,
            task_id: None,
            run_id: None,
            provider_namespace: ProviderNamespace::LlmProviders,
        }
    }
    // Historical collaboration fixtures are retained for compatibility tests only.
    #[cfg(test)]
    pub fn collaboration(purpose: ModelPurpose) -> Self {
        Self {
            provider_namespace: ProviderNamespace::CollaborationSettings,
            ..Self::new(purpose)
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub enum ProviderNamespace {
    LlmProviders,
    #[cfg(test)]
    CollaborationSettings,
}

/// Runtime identity refers to an existing configuration store; no copied secret.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProviderRef {
    pub namespace: String,
    pub id: String,
}
impl ProviderRef {
    fn legacy(provider: &LlmProvider, namespace: ProviderNamespace) -> Self {
        Self {
            namespace: match namespace {
                #[cfg(test)]
                ProviderNamespace::CollaborationSettings => "collaboration_settings",
                ProviderNamespace::LlmProviders => "llm_providers",
            }
            .into(),
            id: provider.id.chars().take(256).collect(),
        }
    }
}

/// None means unreported. Cached/reasoning tokens are subsets, never added twice.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct TokenUsage {
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub total_tokens: Option<u64>,
    pub cached_input_tokens: Option<u64>,
    pub reasoning_output_tokens: Option<u64>,
}
impl TokenUsage {
    pub(crate) fn from_response(value: &serde_json::Value) -> Self {
        let usage = &value["usage"];
        Self {
            input_tokens: usage["prompt_tokens"].as_u64(),
            output_tokens: usage["completion_tokens"].as_u64(),
            total_tokens: usage["total_tokens"].as_u64(),
            cached_input_tokens: usage
                .pointer("/prompt_tokens_details/cached_tokens")
                .and_then(|v| v.as_u64()),
            reasoning_output_tokens: usage
                .pointer("/completion_tokens_details/reasoning_tokens")
                .and_then(|v| v.as_u64()),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ModelRequestSnapshot {
    pub request_id: String,
    pub attempt_id: String,
    #[serde(default = "first_ordinal")]
    pub ordinal: u32,
    pub purpose: ModelPurpose,
    pub task_id: Option<String>,
    pub run_id: Option<String>,
    pub provider_ref: ProviderRef,
    #[serde(default)]
    pub identity: Option<crate::model_registry::ModelIdentity>,
    pub requested_model: String,
    pub served_model: Option<String>,
    pub upstream_request_id: Option<String>,
    pub upstream_response_id: Option<String>,
    pub finish_reason: Option<String>,
    pub usage: TokenUsage,
    pub http_status: Option<u16>,
    pub started_at: String,
    pub finished_at: Option<String>,
    pub elapsed_ms: Option<u64>,
    pub state: String,
    pub error_kind: Option<String>,
    /// No trusted deployment price exists yet. Unknown is never free.
    pub cost_usd: Option<f64>,
    pub cost_status: String,
}

fn first_ordinal() -> u32 { 1 }

pub(crate) fn initialize(conn: &Connection) -> Result<(), String> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS model_requests (
        request_id TEXT PRIMARY KEY, task_id TEXT, run_id TEXT,
        purpose TEXT NOT NULL, state TEXT NOT NULL, started_at TEXT NOT NULL,
        finished_at TEXT);
        CREATE INDEX IF NOT EXISTS model_requests_task ON model_requests(task_id,started_at);
        CREATE TABLE IF NOT EXISTS model_attempts (
        attempt_id TEXT PRIMARY KEY, request_id TEXT NOT NULL REFERENCES model_requests(request_id),
        ordinal INTEGER NOT NULL, snapshot_json TEXT NOT NULL,
        UNIQUE(request_id,ordinal));",
    )
    .map_err(|e| e.to_string())
}

pub(crate) fn recover_interrupted(conn: &mut Connection) -> Result<(), String> {
    let tx = conn.transaction().map_err(|e| e.to_string())?;
    // json_set preserves observed usage when shutdown happened after the response.
    let finished = Utc::now().to_rfc3339();
    tx.execute("UPDATE model_attempts SET snapshot_json=json_set(snapshot_json,
        '$.state','interrupted','$.error_kind','process_interrupted','$.finished_at',?1,'$.elapsed_ms',NULL)
        WHERE request_id IN (SELECT request_id FROM model_requests WHERE state='running') AND json_extract(snapshot_json,'$.state')='running'", [&finished]).map_err(|e| e.to_string())?;
    tx.execute(
        "UPDATE model_requests SET state='interrupted',finished_at=?1 WHERE state='running'",
        [&finished],
    )
    .map_err(|e| e.to_string())?;
    tx.commit().map_err(|e| e.to_string())
}

pub(crate) struct ModelTrace {
    pub snapshot: ModelRequestSnapshot,
    store: Option<Arc<TelemetryStore>>,
    started: Instant,
    finished: bool,
}
impl ModelTrace {
    pub fn start(
        provider: &LlmProvider,
        context: ModelContext,
        store: Option<Arc<TelemetryStore>>,
    ) -> Result<Self, String> {
        let snapshot = ModelRequestSnapshot {
            request_id: Uuid::new_v4().to_string(),
            attempt_id: Uuid::new_v4().to_string(),
            ordinal: 1,
            purpose: context.purpose,
            task_id: context.task_id,
            run_id: context.run_id,
            provider_ref: ProviderRef::legacy(provider, context.provider_namespace),
            identity: provider.identity.clone(),
            requested_model: safe_text(&provider.model, &provider.api_key),
            served_model: None,
            upstream_request_id: None,
            upstream_response_id: None,
            finish_reason: None,
            usage: TokenUsage::default(),
            http_status: None,
            started_at: Utc::now().to_rfc3339(),
            finished_at: None,
            elapsed_ms: None,
            state: "running".into(),
            error_kind: None,
            cost_usd: None,
            cost_status: "unknown_price".into(),
        };
        let trace = Self {
            snapshot,
            store,
            started: Instant::now(),
            finished: false,
        };
        trace.persist()?;
        Ok(trace)
    }

    fn persist(&self) -> Result<(), String> {
        let Some(store) = &self.store else {
            return Ok(());
        };
        let mut conn = store
            .conn
            .lock()
            .map_err(|_| "model trace database lock poisoned")?;
        let tx = conn.transaction().map_err(|e| e.to_string())?;
        let s = &self.snapshot;
        tx.execute("INSERT INTO model_requests(request_id,task_id,run_id,purpose,state,started_at,finished_at)
            VALUES(?1,?2,?3,?4,?5,?6,?7) ON CONFLICT(request_id) DO UPDATE SET state=excluded.state,finished_at=excluded.finished_at",
            params![s.request_id,s.task_id,s.run_id,serde_json::to_value(s.purpose).map_err(|e| e.to_string())?.as_str(),s.state,s.started_at,s.finished_at]).map_err(|e| e.to_string())?;
        tx.execute("INSERT INTO model_attempts(attempt_id,request_id,ordinal,snapshot_json) VALUES(?1,?2,1,?3)
            ON CONFLICT(attempt_id) DO UPDATE SET snapshot_json=excluded.snapshot_json",
            params![s.attempt_id,s.request_id,serde_json::to_string(s).map_err(|e| e.to_string())?]).map_err(|e| e.to_string())?;
        tx.commit().map_err(|e| e.to_string())
    }

    pub fn observe(&mut self, provider: &LlmProvider, value: &serde_json::Value) {
        let bounded = |v: &serde_json::Value| {
            v.as_str()
                .filter(|s| !s.trim().is_empty())
                .map(|s| safe_text(s, &provider.api_key))
        };
        self.snapshot.served_model = bounded(&value["model"]);
        self.snapshot.upstream_response_id = bounded(&value["id"]);
        self.snapshot.finish_reason = bounded(&value["choices"][0]["finish_reason"]);
        self.snapshot.usage = TokenUsage::from_response(value);
    }

    #[cfg(test)]
    pub fn cancel(&mut self) -> Result<(), String> {
        self.snapshot.state = "cancelled".into();
        self.snapshot.error_kind = Some("task_cancelled".into());
        self.snapshot.elapsed_ms =
            Some(self.started.elapsed().as_millis().min(u64::MAX as u128) as u64);
        self.snapshot.finished_at = Some(Utc::now().to_rfc3339());
        self.persist()?;
        self.finished = true;
        Ok(())
    }

    pub fn finish(&mut self, error_kind: Option<&str>) -> Result<(), String> {
        if self.finished {
            return Ok(());
        }
        self.snapshot.state = if error_kind.is_some() {
            "failed"
        } else {
            "completed"
        }
        .into();
        self.snapshot.error_kind = error_kind.map(str::to_string);
        self.snapshot.elapsed_ms =
            Some(self.started.elapsed().as_millis().min(u64::MAX as u128) as u64);
        self.snapshot.finished_at = Some(Utc::now().to_rfc3339());
        self.persist()
            .map_err(|_| "模型调用已结束，但请求追踪保存失败；请勿自动重发请求".to_string())?;
        self.finished = true;
        Ok(())
    }
}
impl Drop for ModelTrace {
    fn drop(&mut self) {
        if !self.finished {
            // A failed final write must retry the observed outcome, not replace
            // an already completed/failed/cancelled response with interruption.
            if self.snapshot.state == "running" {
                self.snapshot.state = "interrupted".into();
                self.snapshot.error_kind = Some("future_dropped".into());
                self.snapshot.finished_at = Some(Utc::now().to_rfc3339());
                self.snapshot.elapsed_ms =
                    Some(self.started.elapsed().as_millis().min(u64::MAX as u128) as u64);
            }
            if let Err(error) = self.persist() {
                eprintln!("model trace finalization failed: {error}");
            }
        }
    }
}

pub(crate) fn safe_text(value: &str, secret: &str) -> String {
    let clean = if secret.is_empty() {
        value.to_string()
    } else {
        value.replace(secret, "[key]")
    };
    clean
        .chars()
        .filter(|c| !c.is_control())
        .take(256)
        .collect()
}

/// Called only after the collaboration task's execution threads have stopped.
#[cfg(test)]
pub(crate) fn delete_task_traces(store: &TelemetryStore, task_id: &str) -> Result<(), String> {
    let mut conn = store
        .conn
        .lock()
        .map_err(|_| "model trace database lock poisoned")?;
    conn.pragma_update(None, "secure_delete", "ON")
        .map_err(|e| e.to_string())?;
    let tx = conn.transaction().map_err(|e| e.to_string())?;
    tx.execute("DELETE FROM model_attempts WHERE request_id IN (SELECT request_id FROM model_requests WHERE task_id=?1)", [task_id]).map_err(|e| e.to_string())?;
    tx.execute("DELETE FROM model_requests WHERE task_id=?1", [task_id])
        .map_err(|e| e.to_string())?;
    tx.commit().map_err(|e| e.to_string())
}

// Runtime consumers query the telemetry ledger; this reader verifies trace evidence.
#[cfg(test)]
pub(crate) fn read_traces(
    store: &TelemetryStore,
    task: Option<&str>,
    limit: u32,
) -> Result<Vec<ModelRequestSnapshot>, String> {
    let conn = store
        .conn
        .lock()
        .map_err(|_| "model trace database lock poisoned")?;
    let mut stmt = conn.prepare("SELECT a.snapshot_json FROM model_requests r JOIN model_attempts a ON a.request_id=r.request_id
        WHERE (?1 IS NULL OR r.task_id=?1) ORDER BY r.rowid DESC,a.ordinal LIMIT ?2").map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map(params![task, limit.clamp(1, 100)], |row| {
            row.get::<_, String>(0)
        })
        .map_err(|e| e.to_string())?;
    rows.map(|row| {
        serde_json::from_str(&row.map_err(|e| e.to_string())?).map_err(|e| e.to_string())
    })
    .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    fn provider() -> LlmProvider {
        LlmProvider {
            id: "test-provider".into(),
            name: "test".into(),
            base_url: "http://localhost/v1".into(),
            model: "requested".into(),
            api_key: "secret-key".into(),
            is_custom: true,
            enabled: true,
            context_window: None,
            max_output_tokens: None,
            identity: None,
        }
    }
    #[test]
    fn usage_unknown_is_not_zero_and_details_are_subsets() {
        assert_eq!(
            TokenUsage::from_response(&serde_json::json!({})),
            TokenUsage::default()
        );
        let u = TokenUsage::from_response(
            &serde_json::json!({"usage":{"prompt_tokens":100,"completion_tokens":20,"total_tokens":120,"prompt_tokens_details":{"cached_tokens":80},"completion_tokens_details":{"reasoning_tokens":10}}}),
        );
        assert_eq!(u.total_tokens, Some(120));
        assert_eq!(u.cached_input_tokens, Some(80));
        assert_eq!(u.reasoning_output_tokens, Some(10));
        assert_eq!(
            TokenUsage::from_response(&serde_json::json!({"usage":{"prompt_tokens":-1}}))
                .input_tokens,
            None
        );
    }
    #[test]
    fn stable_identity_redacted_metadata_and_single_evidence_record() {
        let store = Arc::new(TelemetryStore::in_memory());
        let mut t = ModelTrace::start(
            &provider(),
            ModelContext::new(ModelPurpose::Plan),
            Some(store.clone()),
        )
        .unwrap();
        let request = t.snapshot.request_id.clone();
        t.observe(&provider(),&serde_json::json!({"model":"served","id":"secret-key","choices":[{"finish_reason":"stop","message":{"content":"private answer"}}],"usage":{"prompt_tokens":4}}));
        t.finish(None).unwrap();
        t.finish(None).unwrap();
        let records = read_traces(&store, None, 100).unwrap();
        assert_eq!(records.len(), 1);
        let r = &records[0];
        assert_eq!(r.request_id, request);
        assert_eq!(r.served_model.as_deref(), Some("served"));
        assert_eq!(r.upstream_response_id.as_deref(), Some("[key]"));
        assert_eq!(r.cost_usd, None);
        let json = serde_json::to_string(r).unwrap();
        assert!(!json.contains("secret-key"));
        assert!(!json.contains("private answer"));
    }
    #[test]
    fn drop_and_restart_close_pending_without_losing_observed_usage() {
        let store = Arc::new(TelemetryStore::in_memory());
        {
            let _t = ModelTrace::start(
                &provider(),
                ModelContext::new(ModelPurpose::Review),
                Some(store.clone()),
            )
            .unwrap();
        }
        assert_eq!(
            read_traces(&store, None, 1).unwrap()[0].state,
            "interrupted"
        );
        let mut t = ModelTrace::start(
            &provider(),
            ModelContext::new(ModelPurpose::Review),
            Some(store.clone()),
        )
        .unwrap();
        t.observe(
            &provider(),
            &serde_json::json!({"usage":{"prompt_tokens":8}}),
        );
        t.persist().unwrap();
        recover_interrupted(&mut store.conn.lock().unwrap()).unwrap();
        let records = read_traces(&store, None, 1).unwrap();
        assert_eq!(records[0].state, "interrupted");
        assert_eq!(records[0].usage.input_tokens, Some(8));
        assert_eq!(records[0].elapsed_ms, None);
        t.finished = true;
    }
    #[test]
    fn failed_response_preserves_usage_and_task_filter() {
        let store = Arc::new(TelemetryStore::in_memory());
        let mut c = ModelContext::new(ModelPurpose::MemoryExtract);
        c.task_id = Some("task-a".into());
        let mut t = ModelTrace::start(&provider(), c, Some(store.clone())).unwrap();
        t.observe(&provider(),&serde_json::json!({"usage":{"completion_tokens":23},"choices":[{"finish_reason":"length"}]}));
        t.finish(Some("empty_content")).unwrap();
        let r = read_traces(&store, Some("task-a"), 1).unwrap().remove(0);
        assert_eq!(r.state, "failed");
        assert_eq!(r.usage.output_tokens, Some(23));
        assert_eq!(r.finish_reason.as_deref(), Some("length"));
        assert!(read_traces(&store, Some("other"), 1).unwrap().is_empty());
    }
    #[test]
    fn cancellation_and_task_deletion_are_scoped() {
        let store = Arc::new(TelemetryStore::in_memory());
        let mut c = ModelContext::collaboration(ModelPurpose::Plan);
        c.task_id = Some("task-a".into());
        let mut t = ModelTrace::start(&provider(), c, Some(store.clone())).unwrap();
        t.cancel().unwrap();
        drop(t);
        let r = read_traces(&store, None, 1).unwrap().remove(0);
        assert_eq!(r.state, "cancelled");
        assert_eq!(r.provider_ref.namespace, "collaboration_settings");
        let mut t = ModelTrace::start(
            &provider(),
            ModelContext::new(ModelPurpose::MemoryExtract),
            Some(store.clone()),
        )
        .unwrap();
        t.finish(None).unwrap();
        delete_task_traces(&store, "task-a").unwrap();
        delete_task_traces(&store, "task-a").unwrap();
        let rows = read_traces(&store, None, 100).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].provider_ref.namespace, "llm_providers");
    }
    #[test]
    fn retrying_a_failed_final_write_preserves_the_observed_outcome() {
        let store = Arc::new(TelemetryStore::in_memory());
        let mut trace = ModelTrace::start(
            &provider(),
            ModelContext::new(ModelPurpose::Review),
            Some(store.clone()),
        )
        .unwrap();
        store.conn.lock().unwrap().execute_batch("CREATE TRIGGER fail_final_trace BEFORE UPDATE ON model_attempts BEGIN SELECT RAISE(ABORT,'fixture write failure'); END;").unwrap();
        trace.snapshot.usage.output_tokens = Some(12);
        assert!(trace.finish(None).is_err());
        store
            .conn
            .lock()
            .unwrap()
            .execute_batch("DROP TRIGGER fail_final_trace;")
            .unwrap();
        drop(trace);
        let record = read_traces(&store, None, 1).unwrap().remove(0);
        assert_eq!(record.state, "completed");
        assert_eq!(record.error_kind, None);
        assert_eq!(record.usage.output_tokens, Some(12));
    }
}

#[cfg(test)]
mod historical_compatibility_tests {
    use super::*;
    fn provider() -> LlmProvider {
        LlmProvider { id: "fixed".into(), name: "fixed".into(), base_url: "http://localhost/v1".into(), model: "fixed-model".into(), api_key: "fixture-key".into(), is_custom: true, enabled: true, context_window: None, max_output_tokens: None, identity: None }
    }
    #[test]
    fn old_dispatch_records_still_decode_without_loading_removed_policy_or_cost_types() {
        let trace = ModelTrace::start(&provider(), ModelContext::new(ModelPurpose::Plan), None).unwrap();
        let mut raw = serde_json::to_value(&trace.snapshot).unwrap();
        raw["ordinal"] = serde_json::json!(0);
        raw["route"] = serde_json::json!({"group_name":"historical","mode":"ordered","config_version":1});
        raw["cost"] = serde_json::json!({"state":"held","quote":null});
        let legacy: ModelRequestSnapshot = serde_json::from_value(raw).unwrap();
        assert_eq!(legacy.ordinal, 0);
        let new = ModelTrace::start(&provider(), ModelContext::new(ModelPurpose::Plan), None).unwrap();
        assert_eq!(new.snapshot.ordinal, 1);
        assert_eq!(new.snapshot.requested_model, "fixed-model");
        let new_json = serde_json::to_value(&new.snapshot).unwrap();
        assert!(new_json.get("route").is_none()); assert!(new_json.get("cost").is_none());
    }
    #[test]
    fn restart_does_not_overwrite_closed_failures_left_between_historical_attempts() {
        let store = Arc::new(TelemetryStore::in_memory());
        let mut trace = ModelTrace::start(&provider(), ModelContext::new(ModelPurpose::Review), Some(store.clone())).unwrap();
        trace.snapshot.usage.output_tokens = Some(7);
        trace.finish(Some("http")).unwrap();
        store.conn.lock().unwrap().execute("UPDATE model_requests SET state='running'",[]).unwrap();
        recover_interrupted(&mut store.conn.lock().unwrap()).unwrap();
        let record = read_traces(&store,None,1).unwrap().remove(0);
        assert_eq!(record.state,"failed"); assert_eq!(record.usage.output_tokens,Some(7));
    }
}
