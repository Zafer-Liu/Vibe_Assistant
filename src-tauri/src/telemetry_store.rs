//! Durable, local-first event ledger for external agent adapters.
//!
//! Raw events are retained before they are projected into memories or usage
//! charts.  This makes ingestion auditable, retryable and independent from a
//! particular memory backend.

use crate::thinking::strip_thinking_blocks;
use rusqlite::{params, Connection, OptionalExtension, Transaction};
use serde::{de::DeserializeOwned, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use tauri::Emitter;

static SHARED: OnceLock<Arc<TelemetryStore>> = OnceLock::new();

pub fn init_shared(store: TelemetryStore) {
    let _ = SHARED.set(Arc::new(store));
}

pub fn shared_store() -> Option<Arc<TelemetryStore>> {
    SHARED.get().cloned()
}

#[derive(Clone)]
pub struct TelemetryStore {
    pub(crate) conn: Arc<Mutex<Connection>>,
    mcp_dashboard_cache: Arc<Mutex<Option<McpDashboardCache>>>,
}

struct McpDashboardCache {
    data_version: i64,
    total_changes: i64,
    captured_at: std::time::Instant,
    stats: McpDashboardStats,
}

mod schema;
mod transcripts;
mod types;

pub use transcripts::discover_all_native_transcripts;
pub(crate) use transcripts::hex;
use transcripts::*;
pub(crate) use types::{
    normalize_l1_memory_type, user_defined_memory_scope, validate_l1_durability,
};
pub use types::{
    CloudSyncConflict, ConversationSearchHit, L1MemoryCandidate, L1ResetResult, LocalMemory,
    LocalMemorySnapshot, LocalMemoryStats, McpAccessLog, McpCallCoverage, McpCallGroup,
    McpCallObservation, McpCallRefresh, McpCallStats, McpDashboardStats, McpToolHealth, McpFailedCallPage, McpFailureCategoryCount,
    MemoryConversationDetail, MemoryImportance, MemoryImportanceCandidate,
    MemoryImportanceEvidence, MemoryInjectionStats, MemoryLayerDocument, MemoryMcpActivity,
    PendingConversation, PendingMemorySession, TelemetryEvent, TelemetryLiveStatus,
    TelemetrySummary, TelemetryUsageAnalytics, TelemetryUsageBucket, TelemetryUsageCostGroup,
    TelemetryUsageHeatCell, TelemetryUsageRecord, TelemetryUsageRefresh, UsageDayAggregate,
};
use types::{TranscriptUsage, TranscriptUsageRecord};

impl TelemetryStore {
    pub fn new() -> Result<Self, String> {
        let mut conn = schema::open_database()?;
        crate::llm_trace::recover_interrupted(&mut conn)?;
        Ok(Self::from_connection(conn))
    }

    pub(crate) fn from_connection(conn: Connection) -> Self {
        Self { conn: Arc::new(Mutex::new(conn)), mcp_dashboard_cache: Default::default() }
    }

    #[cfg(test)]
    pub(crate) fn in_memory() -> Self {
        Self::from_connection(schema::initialize_database(Connection::open_in_memory().expect("open test database")).expect("initialize production schema"))
    }

    /// Read an application setting stored in the primary SQLite database.
    /// Returning `None` deliberately distinguishes a missing setting from a
    /// valid JSON value such as an empty list.
    pub fn app_setting_get<T: DeserializeOwned>(&self, key: &str) -> Option<T> {
        let value: Option<String> = self
            .conn
            .lock()
            .ok()?
            .query_row(
                "SELECT value_json FROM app_settings WHERE setting_key = ?1",
                params![key],
                |row| row.get(0),
            )
            .optional()
            .ok()?;
        value.and_then(|json| serde_json::from_str(&json).ok())
    }

    /// Save one application setting into the primary SQLite database.
    pub fn app_setting_set<T: Serialize>(&self, key: &str, value: &T) -> Result<(), String> {
        let json = serde_json::to_string(value).map_err(|error| error.to_string())?;
        self.conn
            .lock()
            .map_err(|_| "telemetry store lock poisoned".to_string())?
            .execute(
                "INSERT INTO app_settings (setting_key, value_json, updated_at)
                 VALUES (?1, ?2, ?3)
                 ON CONFLICT(setting_key) DO UPDATE SET
                   value_json = excluded.value_json,
                   updated_at = excluded.updated_at",
                params![key, json, chrono::Utc::now().to_rfc3339()],
            )
            .map_err(|error| error.to_string())?;
        Ok(())
    }

    /// A native transcript is immutable for our purposes until its size or
    /// mtime changes. Failed reads are retained separately and retried only by
    /// an explicit user scan (unless the file itself has changed).
    pub fn should_scan_transcript(&self, source: &str, path: &Path, retry_failed: bool) -> bool {
        let Ok(metadata) = std::fs::metadata(path) else {
            return true;
        };
        let size = metadata.len() as i64;
        let modified = metadata
            .modified()
            .ok()
            .map(chrono::DateTime::<chrono::Utc>::from)
            .map(|time| time.to_rfc3339())
            .unwrap_or_default();
        let prior: Option<(i64, String, String)> = self.conn.lock().ok().and_then(|conn| conn.query_row(
            "SELECT file_size, modified_at, state FROM transcript_scan_states WHERE source = ?1 AND transcript_path = ?2",
            params![source, path.display().to_string()], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        ).optional().ok().flatten());
        match prior {
            None => true,
            Some((old_size, old_modified, _state))
                if old_size != size || old_modified != modified =>
            {
                true
            }
            Some((_, _, state)) => state == "failed" && retry_failed,
        }
    }

    /// 读取器清洗规则升级后，旧扫描状态会让已污染的 L0 行永远不被重读；
    /// 清空全部扫描状态可强制一次全量重扫（内容寻址保证幂等）。
    pub fn reset_transcript_scan_states(&self) -> Result<(), String> {
        self.conn
            .lock()
            .map_err(|_| "telemetry store lock poisoned".to_string())?
            .execute("DELETE FROM transcript_scan_states", [])
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    pub fn record_transcript_scan(
        &self,
        source: &str,
        path: &Path,
        session_id: Option<&str>,
        result: Result<(), &str>,
    ) {
        let Ok(metadata) = std::fs::metadata(path) else {
            return;
        };
        let modified = metadata
            .modified()
            .ok()
            .map(chrono::DateTime::<chrono::Utc>::from)
            .map(|time| time.to_rfc3339())
            .unwrap_or_default();
        let (state, error) = match result {
            Ok(()) => ("stored", None),
            Err(detail) => ("failed", Some(detail)),
        };
        if let Ok(conn) = self.conn.lock() {
            let _ = conn.execute(
                "INSERT INTO transcript_scan_states (source, transcript_path, file_size, modified_at, state, error_detail, session_id, last_scanned_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
                 ON CONFLICT(source, transcript_path) DO UPDATE SET file_size=excluded.file_size, modified_at=excluded.modified_at,
                   state=excluded.state, error_detail=excluded.error_detail, session_id=excluded.session_id, last_scanned_at=excluded.last_scanned_at",
                params![source, path.display().to_string(), metadata.len() as i64, modified, state, error, session_id, chrono::Utc::now().to_rfc3339()],
            );
        }
    }

    /// Store the incoming event before asynchronous memory extraction.  A
    /// stable key makes duplicate delivery by a hook harmless.
    pub fn record_hook(&self, source: &str, payload: &Value) -> Result<(), String> {
        let session_id =
            get_str(payload, &["session_id", "conversation_id", "thread_id"]).unwrap_or("unknown");
        let event_type =
            get_str(payload, &["hook_event_name", "event", "type"]).unwrap_or("unknown");
        let project_path = get_str(payload, &["cwd", "project_path", "workspace_path"]);
        let occurred_at = get_str(payload, &["timestamp", "occurred_at", "time"])
            .map(canonical_timestamp)
            .unwrap_or_else(|| chrono::Utc::now().to_rfc3339());
        let usage = extract_usage(payload);
        let serialized = serde_json::to_string(payload).map_err(|e| e.to_string())?;
        let digest = Sha256::digest(serialized.as_bytes());
        let event_key = if event_type == "TranscriptSync" {
            // Native transcript revisions replace one durable conversation;
            // they must never become an ever-growing chain of memory rows.
            format!("native:{source}:{session_id}:TranscriptSync")
        } else {
            format!("{source}:{session_id}:{event_type}:{}", hex(&digest))
        };

        self.conn
            .lock()
            .map_err(|_| "telemetry store lock poisoned".to_string())?
            .execute(
                "INSERT OR IGNORE INTO agent_events
                 (event_key, source, session_id, project_path, event_type, occurred_at, token_source, input_tokens, output_tokens, cached_tokens, payload_json)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
                params![
                    event_key,
                    source,
                    session_id,
                    project_path,
                    event_type,
                    occurred_at,
                    usage.0,
                    usage.1,
                    usage.2,
                    usage.3,
                    serialized,
                ],
            )
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    /// Accept an adapter's *final session total* as an authoritative usage
    /// record. This is intentionally separate from `record_hook`: lifecycle
    /// Hooks can be emitted many times for one response and may contain a
    /// cumulative counter, neither of which is safe to sum.
    pub fn record_final_session_usage(
        &self,
        source: &str,
        payload: &Value,
    ) -> Result<bool, String> {
        let is_final = matches!(
            get_str(payload, &["event", "type", "hook_event_name"]),
            Some("session_usage" | "session_end")
        ) || payload.get("usage_scope").and_then(Value::as_str) == Some("session");
        if !is_final {
            return Ok(false);
        }
        let session_id = get_str(payload, &["session_id", "conversation_id", "thread_id"])
            .filter(|id| !id.trim().is_empty() && *id != "unknown")
            .ok_or("final session usage requires a stable session_id")?;
        let (provenance, input_tokens, output_tokens, cached_tokens) = extract_usage(payload);
        if provenance != "reported" {
            return Err(
                "final session usage requires usage.input_tokens or usage.output_tokens".into(),
            );
        }
        self.conn.lock().map_err(|_| "telemetry store lock poisoned".to_string())?.execute(
            "INSERT INTO session_token_usage (source, session_id, input_tokens, output_tokens, cached_tokens, origin, refreshed_at)
             VALUES (?1, ?2, ?3, ?4, ?5, 'adapter_session_report', ?6)
             ON CONFLICT(source, session_id) DO UPDATE SET
               input_tokens=excluded.input_tokens, output_tokens=excluded.output_tokens,
               cached_tokens=excluded.cached_tokens, origin=excluded.origin, refreshed_at=excluded.refreshed_at",
            params![source, session_id, input_tokens, output_tokens, cached_tokens, chrono::Utc::now().to_rfc3339()],
        ).map_err(|e| e.to_string())?;
        Ok(true)
    }

    /// Attach the completed conversation to the exact Stop receipt already
    /// written by `record_hook`.  Keeping it in the local event ledger makes
    /// it visible even when the memory engine is unavailable or rejects the
    /// later extraction request.  Returns the anchored `event_key` so the
    /// extraction pipeline can write memories against this exact row instead
    /// of re-querying by session_id.
    pub fn record_conversation(
        &self,
        source: &str,
        payload: &Value,
        state: &str,
        messages: &[(String, String)],
    ) -> Result<String, String> {
        let serialized = serde_json::to_string(payload).map_err(|e| e.to_string())?;
        let session_id =
            get_str(payload, &["session_id", "conversation_id", "thread_id"]).unwrap_or("unknown");
        let event_type =
            get_str(payload, &["hook_event_name", "event", "type"]).unwrap_or("unknown");
        let digest = Sha256::digest(serialized.as_bytes());
        let event_key = format!("{source}:{session_id}:{event_type}:{}", hex(&digest));
        let conversation_text = format_conversation(messages);
        // A later Stop may replay a turn already captured by a native scan.
        // Keep the Hook receipt, but never enqueue the same L0 dialogue twice.
        let covered_by_native = state == "full" && !conversation_text.is_empty() && self.conn
            .lock().map_err(|_| "telemetry store lock poisoned".to_string())?
            .query_row(
                "SELECT 1 FROM agent_events WHERE source = ?1 AND session_id = ?2
                   AND event_type = 'TranscriptSync' AND conversation_state = 'full'
                   AND instr(conversation_text, ?3) > 0 LIMIT 1",
                params![source, session_id, &conversation_text], |_| Ok(()),
            ).optional().map_err(|e| e.to_string())?.is_some();
        let ledger_state = if covered_by_native { "covered" } else { state };
        let estimated_usage = (state == "full")
            .then(|| estimate_ledger_conversation_usage(&conversation_text))
            .flatten();
        self.conn
            .lock()
            .map_err(|_| "telemetry store lock poisoned".to_string())?
            .execute(
                "UPDATE agent_events
                    SET conversation_state = ?1,
                        conversation_message_count = ?2,
                        conversation_text = ?3,
                        l1_state = CASE
                          WHEN ?1 = 'full' AND l1_state != 'stored' THEN 'pending'
                          ELSE l1_state
                        END
                  WHERE event_key = ?4",
                params![
                    ledger_state,
                    messages.len() as i64,
                    (!covered_by_native && !conversation_text.is_empty()).then_some(conversation_text),
                    event_key,
                ],
            )
            .map_err(|e| e.to_string())?;
        // Do not leave a captured conversation without any Token value. Native
        // transcript reconciliation may later overwrite this fallback with a
        // provider-reported total; an existing real report is never replaced.
        if session_id != "unknown" {
            if let Some(usage) = estimated_usage {
                self.conn.lock().map_err(|_| "telemetry store lock poisoned".to_string())?.execute(
                    "INSERT INTO session_token_usage (source, session_id, input_tokens, output_tokens, cached_tokens, origin, refreshed_at)
                     VALUES (?1, ?2, ?3, ?4, 0, 'estimated_ledger', ?5)
                     ON CONFLICT(source, session_id) DO UPDATE SET
                       input_tokens=excluded.input_tokens, output_tokens=excluded.output_tokens,
                       cached_tokens=0, origin=excluded.origin, refreshed_at=excluded.refreshed_at
                     WHERE session_token_usage.origin LIKE 'estimated%'",
                    params![source, session_id, usage.input_tokens, usage.output_tokens, chrono::Utc::now().to_rfc3339()],
                ).map_err(|e| e.to_string())?;
            }
        }
        Ok(event_key)
    }

    pub fn conversation_is_covered(&self, event_key: &str) -> Result<bool, String> {
        self.conn.lock().map_err(|_| "telemetry store lock poisoned".to_string())?
            .query_row(
                "SELECT conversation_state = 'covered' FROM agent_events WHERE event_key = ?1",
                [event_key], |row| row.get::<_, bool>(0),
            ).map_err(|e| e.to_string())
    }

    /// Retain a conversation discovered directly from a local transcript, so
    /// native scans can feed the same L1 queue as Hooks. The fingerprint is
    /// content-addressed: unchanged files are idempotent, while a new Qoder
    /// transcript revision becomes a new pending memory candidate.
    pub fn record_native_conversation(
        &self,
        source: &str,
        session_id: &str,
        transcript_path: &Path,
        occurred_at: &str,
        messages: &[(String, String)],
    ) -> Result<bool, String> {
        if session_id.trim().is_empty() || messages.is_empty() {
            return Ok(false);
        }
        let digest_input = messages
            .iter()
            .map(|(role, content)| format!("{role}\n{content}\n"))
            .collect::<String>();
        let digest = Sha256::digest(digest_input.as_bytes());
        let full_text = format_conversation(messages);
        // Hook Stop rows represent individual turns, whereas a native file
        // contains the whole session. Remove each already-ledgered turn once
        // before staging the native remainder for L1 extraction.
        let hook_turns = {
            let conn = self.conn.lock().map_err(|_| "telemetry store lock poisoned".to_string())?;
            let mut statement = conn.prepare(
                "SELECT conversation_text, conversation_message_count FROM agent_events WHERE source = ?1 AND session_id = ?2
                   AND event_type = 'Stop' AND conversation_state = 'full'
                   AND conversation_text IS NOT NULL ORDER BY id ASC",
            ).map_err(|e| e.to_string())?;
            let rows = statement.query_map(params![source, session_id], |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?)))
                .map_err(|e| e.to_string())?;
            rows.collect::<Result<Vec<_>, _>>().map_err(|e| e.to_string())?
        };
        let mut uncovered_text = full_text.clone();
        let mut uncovered_message_count = messages.len() as i64;
        for (turn, turn_message_count) in hook_turns {
            if !turn.is_empty() && uncovered_text.contains(&turn) {
                uncovered_text = uncovered_text.replacen(&turn, "", 1);
                uncovered_message_count = (uncovered_message_count - turn_message_count).max(0);
            }
        }
        let uncovered_text = uncovered_text.trim_matches('\n').to_string();
        let l0_hash = hex(&Sha256::digest(uncovered_text.as_bytes()));
        let payload = serde_json::json!({
            "event": "TranscriptSync",
            "session_id": session_id,
            "timestamp": occurred_at,
            "transcript_path": transcript_path,
            "content_hash": hex(&digest),
            "l0_content_hash": l0_hash.clone(),
        });
        let event_key = format!("native:{source}:{session_id}:TranscriptSync");
        // Native scans may revisit the same immutable transcript whenever the
        // app opens.  Compare the content fingerprint before touching the L0
        // event so a successfully extracted conversation is never re-queued.
        let previous_hashes = self
            .conn
            .lock()
            .map_err(|_| "telemetry store lock poisoned".to_string())?
            .query_row(
                "SELECT payload_json FROM agent_events WHERE event_key = ?1",
                [&event_key],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(|e| e.to_string())?
            .and_then(|existing| serde_json::from_str::<Value>(&existing).ok())
            .map(|existing| (
                existing.get("content_hash").and_then(Value::as_str).map(str::to_string),
                existing.get("l0_content_hash").and_then(Value::as_str).map(str::to_string),
            ));
        let raw_hash = hex(&digest);
        if previous_hashes.as_ref().is_some_and(|(raw, l0)| raw.as_deref() == Some(raw_hash.as_str()) && l0.as_deref() == Some(l0_hash.as_str())) {
            return Ok(false);
        }
        self.record_hook(source, &payload)?;
        if previous_hashes.as_ref().is_some_and(|(_, l0)| l0.as_deref() == Some(l0_hash.as_str())) {
            // A newly scanned turn was already captured by Hook. Update the
            // transcript fingerprint without invalidating its L1 memories.
            let serialized = serde_json::to_string(&payload).map_err(|e| e.to_string())?;
            self.conn.lock().map_err(|_| "telemetry store lock poisoned".to_string())?
                .execute("UPDATE agent_events SET payload_json = ?1, occurred_at = ?2 WHERE event_key = ?3",
                    params![serialized, canonical_timestamp(occurred_at), event_key])
                .map_err(|e| e.to_string())?;
            return Ok(false);
        }
        let serialized = serde_json::to_string(&payload).map_err(|e| e.to_string())?;
        let conversation_text = uncovered_text;
        let has_uncovered_text = !conversation_text.is_empty();
        let mut conn = self
            .conn
            .lock()
            .map_err(|_| "telemetry store lock poisoned".to_string())?;
        let transaction = conn.transaction().map_err(|e| e.to_string())?;
        transaction
            .execute(
                "DELETE FROM local_memory_items WHERE source_event_key = ?1 AND user_defined = 0",
                [&event_key],
            )
            .map_err(|e| e.to_string())?;
        transaction.execute(
            "UPDATE agent_events SET payload_json = ?1, occurred_at = ?2,
                 conversation_state = ?3, conversation_message_count = ?4,
                 conversation_text = ?5, l1_state = ?6 WHERE event_key = ?7",
            params![serialized, canonical_timestamp(occurred_at),
                if has_uncovered_text { "full" } else { "covered" },
                uncovered_message_count,
                has_uncovered_text.then_some(conversation_text),
                if has_uncovered_text { "pending" } else { "unavailable" }, event_key],
        ).map_err(|e| e.to_string())?;
        transaction.commit().map_err(|e| e.to_string())?;
        if let Some(usage) = estimate_ledger_conversation_usage(&full_text) {
            conn.execute(
                "INSERT INTO session_token_usage (source, session_id, input_tokens, output_tokens, cached_tokens, origin, refreshed_at)
                 VALUES (?1, ?2, ?3, ?4, 0, 'estimated_ledger', ?5)
                 ON CONFLICT(source, session_id) DO UPDATE SET
                   input_tokens=excluded.input_tokens, output_tokens=excluded.output_tokens,
                   cached_tokens=0, origin=excluded.origin, refreshed_at=excluded.refreshed_at
                 WHERE session_token_usage.origin LIKE 'estimated%'",
                params![source, session_id, usage.input_tokens, usage.output_tokens, chrono::Utc::now().to_rfc3339()],
            ).map_err(|e| e.to_string())?;
        }
        Ok(has_uncovered_text)
    }

    /// 该会话是否已由其他来源录入原生转录。ZCode 的 Claude 引擎会把会话
    /// 镜像到 ~/.claude/projects（同一 UUID 文件名），双份扫描若都记账会
    /// 重复对话与用量；扫描侧据此跳过后到的镜像副本。
    pub fn native_session_recorded_by_other_source(
        &self,
        source: &str,
        session_id: &str,
    ) -> Result<bool, String> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| "telemetry store lock poisoned".to_string())?;
        for other in crate::agent_sources::AGENT_SOURCE_IDS {
            if other == source {
                continue;
            }
            let event_key = format!("native:{other}:{session_id}:TranscriptSync");
            let exists = conn
                .query_row(
                    "SELECT 1 FROM agent_events WHERE event_key = ?1",
                    [&event_key],
                    |_| Ok(()),
                )
                .optional()
                .map_err(|e| e.to_string())?
                .is_some();
            if exists {
                return Ok(true);
            }
        }
        Ok(false)
    }

    pub fn unannotated_stop_payloads(&self, limit: u32) -> Result<Vec<(String, Value)>, String> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| "telemetry store lock poisoned".to_string())?;
        let mut statement = conn
            .prepare(
                "SELECT source, payload_json FROM agent_events
                  WHERE event_type = 'Stop' AND conversation_state = 'unavailable'
                  ORDER BY id DESC LIMIT ?1",
            )
            .map_err(|e| e.to_string())?;
        let rows = statement
            .query_map([limit.min(100)], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .map_err(|e| e.to_string())?;
        rows.filter_map(|row| match row {
            Ok((source, payload)) => serde_json::from_str(&payload)
                .ok()
                .map(|payload| Ok((source, payload))),
            Err(error) => Some(Err(error.to_string())),
        })
        .collect()
    }

    pub fn pending_l1_conversations(&self, limit: u32) -> Result<Vec<PendingConversation>, String> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| "telemetry store lock poisoned".to_string())?;
        let mut statement = conn.prepare(
            "SELECT event_key, conversation_text FROM agent_events
              WHERE conversation_state = 'full'
                AND l1_state IN ('failed', 'retrying', 'pending')
                AND conversation_text IS NOT NULL AND conversation_text != ''
              ORDER BY CASE l1_state WHEN 'failed' THEN 0 WHEN 'retrying' THEN 1 ELSE 2 END, id ASC LIMIT ?1",
        ).map_err(|e| e.to_string())?;
        let pending = statement
            .query_map([limit.min(100)], |row| {
                Ok(PendingConversation {
                    event_key: row.get(0)?,
                    conversation_text: row.get(1)?,
                })
            })
            .map_err(|e| e.to_string())?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())?;
        Ok(pending)
    }

    pub fn set_l1_failure(&self, event_key: &str, error: &str) -> Result<(), String> {
        self.conn.lock().map_err(|_| "telemetry store lock poisoned".to_string())?
            .execute("UPDATE agent_events SET l1_state = 'failed', l1_error_detail = ?1 WHERE event_key = ?2",
                params![truncate_chars(error, 600), event_key])
            .map_err(|error| error.to_string())?;
        Ok(())
    }

    /// List complete conversations still waiting for memory extraction, for
    /// the「待提取记忆」面板。Failed sessions sort first so users see what
    /// needs attention; only a short sanitized excerpt leaves the ledger.
    pub fn pending_memory_session_list(
        &self,
        limit: u32,
    ) -> Result<Vec<PendingMemorySession>, String> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| "telemetry store lock poisoned".to_string())?;
        let mut statement = conn
            .prepare(
                "SELECT event_key, source, session_id, occurred_at, conversation_message_count,
                    l1_state, l1_error_detail, substr(conversation_text, 1, 160)
               FROM agent_events
              WHERE conversation_state = 'full'
                AND l1_state IN ('failed', 'retrying', 'pending')
                AND conversation_text IS NOT NULL AND conversation_text != ''
              ORDER BY CASE l1_state WHEN 'failed' THEN 0 WHEN 'retrying' THEN 1 ELSE 2 END,
                       id DESC LIMIT ?1",
            )
            .map_err(|e| e.to_string())?;
        let rows = statement
            .query_map([limit.min(500)], |row| {
                Ok(PendingMemorySession {
                    event_key: row.get(0)?,
                    source: row.get(1)?,
                    session_id: row.get(2)?,
                    occurred_at: row.get(3)?,
                    message_count: row.get(4)?,
                    l1_state: row.get(5)?,
                    error: row.get(6)?,
                    excerpt: row.get(7)?,
                })
            })
            .map_err(|e| e.to_string())?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())?;
        Ok(rows)
    }

    /// List complete conversations already organized into L1 memories, for
    /// the「已整理对话」面板。Newest first; only a short sanitized excerpt
    /// leaves the ledger, same as the pending list.
    pub fn organized_memory_session_list(
        &self,
        limit: u32,
    ) -> Result<Vec<PendingMemorySession>, String> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| "telemetry store lock poisoned".to_string())?;
        let mut statement = conn
            .prepare(
                "SELECT event_key, source, session_id, occurred_at, conversation_message_count,
                    l1_state, l1_error_detail, substr(conversation_text, 1, 160)
               FROM agent_events
              WHERE conversation_state = 'full'
                AND l1_state = 'stored'
                AND conversation_text IS NOT NULL AND conversation_text != ''
              ORDER BY id DESC LIMIT ?1",
            )
            .map_err(|e| e.to_string())?;
        let rows = statement
            .query_map([limit.min(500)], |row| {
                Ok(PendingMemorySession {
                    event_key: row.get(0)?,
                    source: row.get(1)?,
                    session_id: row.get(2)?,
                    occurred_at: row.get(3)?,
                    message_count: row.get(4)?,
                    l1_state: row.get(5)?,
                    error: row.get(6)?,
                    excerpt: row.get(7)?,
                })
            })
            .map_err(|e| e.to_string())?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())?;
        Ok(rows)
    }

    /// Load one staged conversation for a single-session organize action.
    pub fn conversation_by_event_key(
        &self,
        event_key: &str,
    ) -> Result<Option<PendingConversation>, String> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| "telemetry store lock poisoned".to_string())?;
        conn.query_row(
            "SELECT event_key, conversation_text FROM agent_events
              WHERE event_key = ?1 AND conversation_state = 'full'
                AND l1_state IN ('failed', 'retrying', 'pending')
                AND conversation_text IS NOT NULL AND conversation_text != ''",
            [event_key],
            |row| {
                Ok(PendingConversation {
                    event_key: row.get(0)?,
                    conversation_text: row.get(1)?,
                })
            },
        )
        .optional()
        .map_err(|e| e.to_string())
    }

    /// Full sanitized conversation for the pending panel's detail dialog.
    /// Any completed conversation may be viewed, including rows already
    /// organized, so an open dialog never breaks when extraction succeeds.
    pub fn conversation_detail_by_event_key(
        &self,
        event_key: &str,
    ) -> Result<Option<MemoryConversationDetail>, String> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| "telemetry store lock poisoned".to_string())?;
        conn.query_row(
            "SELECT event_key, source, session_id, occurred_at, conversation_message_count,
                    l1_state, l1_error_detail, conversation_text
               FROM agent_events
              WHERE event_key = ?1 AND conversation_state = 'full'
                AND conversation_text IS NOT NULL AND conversation_text != ''",
            [event_key],
            |row| {
                Ok(MemoryConversationDetail {
                    event_key: row.get(0)?,
                    source: row.get(1)?,
                    session_id: row.get(2)?,
                    occurred_at: row.get(3)?,
                    message_count: row.get(4)?,
                    l1_state: row.get(5)?,
                    error: row.get(6)?,
                    conversation_text: row.get(7)?,
                })
            },
        )
        .optional()
        .map_err(|e| e.to_string())
    }

    pub fn set_l1_failure_for_session(&self, session_id: &str, error: &str) -> Result<(), String> {
        let event_key = self.latest_full_event_key_for_session(session_id)?;
        if let Some(event_key) = event_key {
            self.set_l1_failure(&event_key, error)?;
        }
        Ok(())
    }

    /// 找到该会话最近一次完整会话记录的锚点。提取结果必须锚定到这一行，
    /// 而不是按 session_id 盲查后直接报错。
    pub fn latest_full_event_key_for_session(
        &self,
        session_id: &str,
    ) -> Result<Option<String>, String> {
        self.conn
            .lock()
            .map_err(|_| "telemetry store lock poisoned".to_string())?
            .query_row(
                "SELECT event_key FROM agent_events WHERE session_id = ?1 AND conversation_state = 'full' ORDER BY id DESC LIMIT 1",
                [session_id],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(|error| error.to_string())
    }

    pub fn store_typed_l1_memories(
        &self,
        event_key: &str,
        candidates: &[L1MemoryCandidate],
    ) -> Result<usize, String> {
        if candidates.is_empty() {
            return Err("L1 候选为空，拒绝写入".into());
        }
        for (index, candidate) in candidates.iter().enumerate() {
            if candidate.content.trim().is_empty() {
                return Err(format!("第 {} 条 L1 内容为空", index + 1));
            }
            let durability = validate_l1_durability(&candidate.durability)?;
            let memory_type = normalize_l1_memory_type(&candidate.memory_type);
            if memory_type == "summary" && durability != "session" {
                return Err("summary 的 durability 必须为 session".into());
            }
        }
        let mut conn = self
            .conn
            .lock()
            .map_err(|_| "telemetry store lock poisoned".to_string())?;
        let (source, session_id, event_time): (String, String, Option<String>) = conn
            .query_row(
                "SELECT source, session_id, occurred_at FROM agent_events WHERE event_key = ?1",
                [event_key],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .map_err(|error| format!("未找到对应的完整会话：{error}"))?;
        let now = chrono::Utc::now().to_rfc3339();
        let transaction = conn.transaction().map_err(|e| e.to_string())?;
        // A source-event is a mutable transcript revision. Clearing it first
        // prevents stale trailing items if a later extraction is shorter.
        transaction
            .execute(
                "DELETE FROM local_memory_items WHERE source_event_key = ?1",
                [event_key],
            )
            .map_err(|e| e.to_string())?;
        let mut written = 0usize;
        for candidate in candidates {
            let filtered_content = strip_thinking_blocks(&candidate.content);
            let content = filtered_content.trim();
            if content.is_empty() {
                continue;
            }
            let ordinal = written;
            let digest = Sha256::digest(format!("{event_key}:{ordinal}").as_bytes());
            let id = format!("local-l1:{}", hex(&digest)[..24].to_string());
            let memory_type = normalize_l1_memory_type(&candidate.memory_type);
            let durability = validate_l1_durability(&candidate.durability)?;
            transaction.execute(
                "INSERT INTO local_memory_items
                    (id, source_event_key, ordinal, memory, memory_type, durability, source, session_id, event_time, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?10)",
                params![id, event_key, ordinal as i64, content, memory_type, durability, source, session_id, event_time, now],
            ).map_err(|e| e.to_string())?;
            written += 1;
        }
        transaction
            .execute(
                "UPDATE agent_events SET l1_state = 'stored' WHERE event_key = ?1",
                [event_key],
            )
            .map_err(|e| e.to_string())?;
        transaction.commit().map_err(|e| e.to_string())?;
        Ok(written)
    }

    /// Session-level variant of [`store_typed_l1_memories`]: anchors the
    /// session to its latest full conversation receipt before writing, so the
    /// hook-driven ingest path never stores memory against a stale anchor.
    pub fn store_typed_l1_memories_for_session(
        &self,
        session_id: &str,
        candidates: &[L1MemoryCandidate],
    ) -> Result<usize, String> {
        let event_key = self
            .latest_full_event_key_for_session(session_id)?
            .ok_or_else(|| "未找到该会话的完整对话记录，无法写入 L1".to_string())?;
        self.store_typed_l1_memories(&event_key, candidates)
    }

    /// Store memories from a user-selected folder.  A folder has one stable
    /// source key, so importing it again refreshes that source instead of
    /// creating a second copy of the same memories.
    pub fn store_imported_memories(
        &self,
        import_key: &str,
        folder: &str,
        candidates: &[L1MemoryCandidate],
    ) -> Result<usize, String> {
        let candidates = candidates
            .iter()
            .filter(|candidate| !candidate.content.trim().is_empty())
            .collect::<Vec<_>>();
        if candidates.is_empty() {
            return Err("导入 L1 候选为空，拒绝写入".into());
        }
        for (index, candidate) in candidates.iter().enumerate() {
            validate_l1_durability(&candidate.durability)
                .map_err(|error| format!("第 {} 条导入 L1 {error}", index + 1))?;
        }

        let now = chrono::Utc::now().to_rfc3339();
        let mut conn = self
            .conn
            .lock()
            .map_err(|_| "telemetry store lock poisoned".to_string())?;
        let transaction = conn.transaction().map_err(|e| e.to_string())?;
        transaction
            .execute(
                "DELETE FROM local_memory_items WHERE source_event_key = ?1",
                [import_key],
            )
            .map_err(|e| e.to_string())?;
        for (ordinal, candidate) in candidates.iter().enumerate() {
            let content = candidate.content.trim();
            let memory_type = normalize_l1_memory_type(&candidate.memory_type);
            let durability = validate_l1_durability(&candidate.durability)?;
            if memory_type == "summary" && durability != "session" {
                return Err(format!(
                    "第 {} 条导入 summary 的 durability 必须为 session",
                    ordinal + 1
                ));
            }
            let digest = Sha256::digest(format!("{import_key}:{ordinal}").as_bytes());
            let id = format!("local-import:{}", hex(&digest)[..24].to_string());
            transaction.execute(
                "INSERT INTO local_memory_items
                    (id, source_event_key, ordinal, memory, memory_type, durability, source, session_id, event_time, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'manual_import', ?7, ?8, ?8, ?8)",
                params![id, import_key, ordinal as i64, content, memory_type, durability, folder, now],
            ).map_err(|e| e.to_string())?;
        }
        transaction.commit().map_err(|e| e.to_string())?;
        Ok(candidates.len())
    }

    pub fn local_l1_memories(&self, limit: u32) -> Result<Vec<LocalMemory>, String> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| "telemetry store lock poisoned".to_string())?;
        let mut statement = conn
            .prepare(
                "SELECT id, memory, memory_type, durability, user_defined, updated_at, event_time FROM local_memory_items
                  WHERE user_defined = 1
                     OR id IN (
                        SELECT id FROM local_memory_items
                         WHERE user_defined = 0
                         ORDER BY updated_at DESC
                         LIMIT ?1
                     )
                 ORDER BY user_defined DESC, updated_at DESC",
            )
            .map_err(|e| e.to_string())?;
        let rows = statement
            .query_map([limit.min(500)], |row| {
                Ok(LocalMemory {
                    id: row.get(0)?,
                    memory: row.get(1)?,
                    memory_type: row.get(2)?,
                    durability: row.get(3)?,
                    user_defined: row.get::<_, i64>(4)? != 0,
                    last_update_at: row.get(5)?,
                    event_time: row.get(6)?,
                    score: None,
                })
            })
            .map_err(|e| e.to_string())?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())
    }

    pub fn local_l1_memory_stats(&self) -> Result<LocalMemoryStats, String> {
        self.conn.lock().map_err(|_| "telemetry store lock poisoned".to_string())?.query_row(
            "SELECT COUNT(*),
                    SUM(CASE WHEN lower(memory_type) IN ('preference', 'profile', 'constraint') THEN 0 ELSE 1 END),
                    SUM(CASE WHEN lower(memory_type) IN ('preference', 'profile', 'constraint') THEN 1 ELSE 0 END),
                    SUM(CASE WHEN lower(durability) = 'session' THEN 1 ELSE 0 END),
                    SUM(CASE WHEN lower(durability) = 'short_term' THEN 1 ELSE 0 END),
                    SUM(CASE WHEN lower(durability) = 'long_term' THEN 1 ELSE 0 END)
               FROM local_memory_items",
            [],
            |row| Ok(LocalMemoryStats {
                total: row.get(0)?,
                facts: row.get::<_, Option<i64>>(1)?.unwrap_or(0),
                preferences: row.get::<_, Option<i64>>(2)?.unwrap_or(0),
                session_only: row.get::<_, Option<i64>>(3)?.unwrap_or(0),
                short_term: row.get::<_, Option<i64>>(4)?.unwrap_or(0),
                long_term: row.get::<_, Option<i64>>(5)?.unwrap_or(0),
            }),
        ).map_err(|error| error.to_string())
    }

    /// Reset only the derived memory layers for a deliberate re-extraction.
    /// L0 conversations, transcript scan state, settings, skills and usage
    /// telemetry are left intact. Complete transcripts become pending again;
    /// partial receipts are not promoted because they lack reliable dialogue.
    pub fn reset_l1_for_reextraction(&self) -> Result<L1ResetResult, String> {
        let mut conn = self
            .conn
            .lock()
            .map_err(|_| "telemetry store lock poisoned".to_string())?;
        let tx = conn.transaction().map_err(|e| e.to_string())?;
        // 用户自定义记忆只有用户能删改：L1 重建只清理自动提取的条目。
        let cleared_l1 = tx
            .query_row(
                "SELECT COUNT(*) FROM local_memory_items WHERE user_defined = 0",
                [],
                |row| row.get(0),
            )
            .map_err(|e| e.to_string())?;
        let cleared_derived_documents = tx
            .query_row("SELECT COUNT(*) FROM memory_layer_documents", [], |row| {
                row.get(0)
            })
            .map_err(|e| e.to_string())?;
        tx.execute("DELETE FROM memory_layer_sources", [])
            .map_err(|e| e.to_string())?;
        tx.execute("DELETE FROM memory_layer_documents", [])
            .map_err(|e| e.to_string())?;
        tx.execute("DELETE FROM local_memory_items WHERE user_defined = 0", [])
            .map_err(|e| e.to_string())?;
        // 保留用户自定义条目的重要度记录，其余随被清理的记忆一并删除。
        tx.execute(
            "DELETE FROM memory_importance WHERE memory_id NOT IN (SELECT id FROM local_memory_items)",
            [],
        )
        .map_err(|e| e.to_string())?;
        // These are legacy compatibility mirrors/fallbacks. Leaving them
        // behind would inject pre-reset context while the new L1 library is
        // empty, which defeats a clean validation run.
        tx.execute(
            "DELETE FROM profile_summaries WHERE profile_id = 'local-user'",
            [],
        )
        .map_err(|e| e.to_string())?;
        tx.execute(
            "DELETE FROM workspace_summaries WHERE workspace_id = 'all-conversations'",
            [],
        )
        .map_err(|e| e.to_string())?;
        let requeued_conversations = tx.execute(
            "UPDATE agent_events
                SET l1_state = 'pending', l1_error_detail = NULL
              WHERE conversation_state = 'full' AND conversation_text IS NOT NULL AND conversation_text != ''",
            [],
        ).map_err(|e| e.to_string())? as i64;
        tx.execute(
            "UPDATE agent_events SET l1_state = 'unavailable', l1_error_detail = NULL
              WHERE conversation_state != 'full' AND l1_state = 'stored'",
            [],
        )
        .map_err(|e| e.to_string())?;
        tx.commit().map_err(|e| e.to_string())?;
        Ok(L1ResetResult {
            cleared_l1,
            cleared_derived_documents,
            requeued_conversations,
        })
    }

    pub fn local_l1_memory_snapshot(&self) -> Result<Vec<LocalMemorySnapshot>, String> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| "telemetry store lock poisoned".to_string())?;
        let mut statement = conn.prepare(
            "SELECT id, source_event_key, ordinal, memory, memory_type, durability, source, session_id, user_defined, event_time, created_at, updated_at
               FROM local_memory_items ORDER BY updated_at DESC",
        ).map_err(|e| e.to_string())?;
        let rows = statement
            .query_map([], |row| {
                Ok(LocalMemorySnapshot {
                    id: row.get(0)?,
                    source_event_key: row.get(1)?,
                    ordinal: row.get(2)?,
                    memory: row.get(3)?,
                    memory_type: row.get(4)?,
                    durability: row.get(5)?,
                    source: row.get(6)?,
                    session_id: row.get(7)?,
                    user_defined: row.get::<_, i64>(8)? != 0,
                    event_time: row.get(9)?,
                    created_at: row.get(10)?,
                    updated_at: row.get(11)?,
                })
            })
            .map_err(|e| e.to_string())?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())
    }

    /// Bounded, time-windowed L1 evidence for L2 consolidation.  The caller
    /// receives the source ids as well as the text, allowing the generated
    /// document to preserve an auditable provenance graph.
    pub fn recent_l1_memories(
        &self,
        days: i64,
        limit: u32,
    ) -> Result<Vec<LocalMemorySnapshot>, String> {
        let cutoff = (chrono::Utc::now() - chrono::Duration::days(days.clamp(1, 365))).to_rfc3339();
        let conn = self
            .conn
            .lock()
            .map_err(|_| "telemetry store lock poisoned".to_string())?;
        let mut statement = conn.prepare(
            "SELECT id, source_event_key, ordinal, memory, memory_type, durability, source, session_id, user_defined, event_time, created_at, updated_at
               FROM local_memory_items
              WHERE COALESCE(event_time, updated_at) >= ?1
              ORDER BY COALESCE(event_time, updated_at) DESC
              LIMIT ?2",
        ).map_err(|e| e.to_string())?;
        let rows = statement
            .query_map(params![cutoff, limit.min(600)], |row| {
                Ok(LocalMemorySnapshot {
                    id: row.get(0)?,
                    source_event_key: row.get(1)?,
                    ordinal: row.get(2)?,
                    memory: row.get(3)?,
                    memory_type: row.get(4)?,
                    durability: row.get(5)?,
                    source: row.get(6)?,
                    session_id: row.get(7)?,
                    user_defined: row.get::<_, i64>(8)? != 0,
                    event_time: row.get(9)?,
                    created_at: row.get(10)?,
                    updated_at: row.get(11)?,
                })
            })
            .map_err(|e| e.to_string())?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())
    }

    pub fn memory_layer_documents(
        &self,
        layer: &str,
        limit: u32,
    ) -> Result<Vec<MemoryLayerDocument>, String> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| "telemetry store lock poisoned".to_string())?;
        let mut statement = conn.prepare(
            "SELECT id, layer, scope, content, state, token_estimate, source_count, window_start, window_end, created_at, published_at
               FROM memory_layer_documents WHERE layer = ?1 ORDER BY created_at DESC LIMIT ?2",
        ).map_err(|e| e.to_string())?;
        let rows = statement
            .query_map(params![layer, limit.min(50)], |row| {
                Ok(MemoryLayerDocument {
                    id: row.get(0)?,
                    layer: row.get(1)?,
                    scope: row.get(2)?,
                    content: row.get(3)?,
                    state: row.get(4)?,
                    token_estimate: row.get(5)?,
                    source_count: row.get(6)?,
                    window_start: row.get(7)?,
                    window_end: row.get(8)?,
                    created_at: row.get(9)?,
                    published_at: row.get(10)?,
                })
            })
            .map_err(|e| e.to_string())?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())
    }

    pub fn active_memory_layer_document(
        &self,
        layer: &str,
    ) -> Result<Option<MemoryLayerDocument>, String> {
        self.conn.lock().map_err(|_| "telemetry store lock poisoned".to_string())?.query_row(
            "SELECT id, layer, scope, content, state, token_estimate, source_count, window_start, window_end, created_at, published_at
               FROM memory_layer_documents WHERE layer = ?1 AND state = 'published' ORDER BY published_at DESC, created_at DESC LIMIT 1",
            [layer],
            |row| Ok(MemoryLayerDocument {
                id: row.get(0)?, layer: row.get(1)?, scope: row.get(2)?, content: row.get(3)?, state: row.get(4)?,
                token_estimate: row.get(5)?, source_count: row.get(6)?, window_start: row.get(7)?, window_end: row.get(8)?,
                created_at: row.get(9)?, published_at: row.get(10)?,
            }),
        ).optional().map_err(|e| e.to_string())
    }

    pub fn save_memory_layer_document(
        &self,
        layer: &str,
        content: &str,
        state: &str,
        source_layer: &str,
        source_ids: &[String],
        window_start: Option<String>,
        window_end: Option<String>,
    ) -> Result<MemoryLayerDocument, String> {
        let source_references = source_ids
            .iter()
            .map(|source_id| (source_layer.to_string(), source_id.clone()))
            .collect::<Vec<_>>();
        self.save_memory_layer_document_with_sources(
            layer,
            content,
            state,
            &source_references,
            window_start,
            window_end,
        )
    }

    /// Persist a layered document with auditable evidence from one or more
    /// preceding layers. L3 combines published L2 summaries with a filtered
    /// selection of direct L1 evidence, so a single source layer is not enough.
    pub fn save_memory_layer_document_with_sources(
        &self,
        layer: &str,
        content: &str,
        state: &str,
        source_references: &[(String, String)],
        window_start: Option<String>,
        window_end: Option<String>,
    ) -> Result<MemoryLayerDocument, String> {
        let id = format!("{layer}:{}", uuid::Uuid::new_v4());
        let now = chrono::Utc::now().to_rfc3339();
        let published_at = (state == "published").then(|| now.clone());
        let content = strip_thinking_blocks(content);
        let token_estimate = estimate_transcript_tokens(&content);
        let mut conn = self
            .conn
            .lock()
            .map_err(|_| "telemetry store lock poisoned".to_string())?;
        let tx = conn.transaction().map_err(|e| e.to_string())?;
        if state == "published" {
            tx.execute("UPDATE memory_layer_documents SET state = 'archived' WHERE layer = ?1 AND scope = 'global' AND state = 'published'", [layer]).map_err(|e| e.to_string())?;
            mark_cloud_sync_dirty_tx(&tx, layer, &now);
        }
        tx.execute(
            "INSERT INTO memory_layer_documents
             (id, layer, scope, content, state, token_estimate, source_count, window_start, window_end, created_at, published_at)
             VALUES (?1, ?2, 'global', ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![id, layer, content.trim(), state, token_estimate, source_references.len() as i64, window_start, window_end, now, published_at],
        ).map_err(|e| e.to_string())?;
        for (source_layer, source_id) in source_references {
            tx.execute(
                "INSERT OR IGNORE INTO memory_layer_sources (document_id, source_layer, source_id) VALUES (?1, ?2, ?3)",
                params![id, source_layer, source_id],
            ).map_err(|e| e.to_string())?;
        }
        tx.commit().map_err(|e| e.to_string())?;
        Ok(MemoryLayerDocument {
            id,
            layer: layer.into(),
            scope: "global".into(),
            content: content.trim().into(),
            state: state.into(),
            token_estimate,
            source_count: source_references.len() as i64,
            window_start,
            window_end,
            created_at: now,
            published_at,
        })
    }

    /// Publishing L3 is the only operation allowed to replace the Profile
    /// used by MCP initialization. Drafts remain inert and reversible.
    /// 发布 L3 草案。`content_override` 携带用户在面板里编辑后的正文：
    /// 编辑本身不单独持久化（无版本/无中间保存），仅在发布这一刻覆盖
    /// 草案内容并随发布落盘。
    pub fn publish_memory_layer_document(
        &self,
        id: &str,
        content_override: Option<&str>,
    ) -> Result<MemoryLayerDocument, String> {
        let mut conn = self
            .conn
            .lock()
            .map_err(|_| "telemetry store lock poisoned".to_string())?;
        let tx = conn.transaction().map_err(|e| e.to_string())?;
        let layer: String = tx
            .query_row(
                "SELECT layer FROM memory_layer_documents WHERE id = ?1 AND state = 'draft'",
                [id],
                |row| row.get(0),
            )
            .map_err(|_| "未找到可发布的记忆草案".to_string())?;
        if layer != "l3" {
            return Err("只有 L3 Profile 草案需要人工发布".into());
        }
        if let Some(content) = content_override
            .map(str::trim)
            .filter(|text| !text.is_empty())
        {
            tx.execute(
                "UPDATE memory_layer_documents SET content = ?1 WHERE id = ?2",
                params![content, id],
            )
            .map_err(|e| e.to_string())?;
        }
        let now = chrono::Utc::now().to_rfc3339();
        tx.execute("UPDATE memory_layer_documents SET state = 'archived' WHERE layer = ?1 AND scope = 'global' AND state = 'published'", [&layer]).map_err(|e| e.to_string())?;
        tx.execute("UPDATE memory_layer_documents SET state = 'published', published_at = ?1 WHERE id = ?2", params![now, id]).map_err(|e| e.to_string())?;
        let document = tx.query_row(
            "SELECT id, layer, scope, content, state, token_estimate, source_count, window_start, window_end, created_at, published_at
               FROM memory_layer_documents WHERE id = ?1", [id], |row| Ok(MemoryLayerDocument {
                id: row.get(0)?, layer: row.get(1)?, scope: row.get(2)?, content: row.get(3)?, state: row.get(4)?, token_estimate: row.get(5)?, source_count: row.get(6)?, window_start: row.get(7)?, window_end: row.get(8)?, created_at: row.get(9)?, published_at: row.get(10)?,
            }),
        ).map_err(|e| e.to_string())?;
        tx.execute("INSERT INTO profile_summaries (profile_id, content, source_workspace_count, updated_at) VALUES ('local-user', ?1, ?2, ?3) ON CONFLICT(profile_id) DO UPDATE SET content=excluded.content, source_workspace_count=excluded.source_workspace_count, updated_at=excluded.updated_at", params![document.content, document.source_count, now]).map_err(|e| e.to_string())?;
        mark_cloud_sync_dirty_tx(&tx, &layer, &now);
        tx.commit().map_err(|e| e.to_string())?;
        Ok(document)
    }

    /// 手动编辑已发布的层级记忆文档正文（目前仅 L2 工作记忆走这条路径：
    /// L2 生成即发布、没有草案发布闸门，编辑确认后直接覆盖当前发布版，
    /// 下一次注入与 L3 草案立刻使用新内容；同步刷新 token_estimate）。
    pub fn update_published_memory_layer_content(
        &self,
        id: &str,
        content: &str,
    ) -> Result<MemoryLayerDocument, String> {
        let content = strip_thinking_blocks(content);
        if content.trim().is_empty() {
            return Err("记忆内容不能为空".into());
        }
        let token_estimate = estimate_transcript_tokens(&content);
        let conn = self
            .conn
            .lock()
            .map_err(|_| "telemetry store lock poisoned".to_string())?;
        let updated = conn
            .execute(
                "UPDATE memory_layer_documents SET content = ?1, token_estimate = ?2 WHERE id = ?3 AND state = 'published'",
                params![content, token_estimate, id],
            )
            .map_err(|e| e.to_string())?;
        if updated == 0 {
            return Err("未找到可编辑的已发布记忆文档".into());
        }
        // 已发布正文被人工编辑 → 云端同步水位置 dirty（同一连接内直接落库，避免重入锁）。
        let now = chrono::Utc::now().to_rfc3339();
        let layer: String = conn
            .query_row(
                "SELECT layer FROM memory_layer_documents WHERE id = ?1",
                [id],
                |r| r.get(0),
            )
            .map_err(|e| e.to_string())?;
        let _ = conn.execute(
            "INSERT INTO cloud_sync_meta (object_key, revision, content_hash, sync_state, updated_at)
             VALUES (?1, 0, NULL, 'dirty', ?2)
             ON CONFLICT(object_key) DO UPDATE SET sync_state = 'dirty', updated_at = ?2",
            params![format!("{layer}:published"), now],
        );
        conn.query_row(
            "SELECT id, layer, scope, content, state, token_estimate, source_count, window_start, window_end, created_at, published_at
               FROM memory_layer_documents WHERE id = ?1", [id], |row| Ok(MemoryLayerDocument {
                id: row.get(0)?, layer: row.get(1)?, scope: row.get(2)?, content: row.get(3)?, state: row.get(4)?, token_estimate: row.get(5)?, source_count: row.get(6)?, window_start: row.get(7)?, window_end: row.get(8)?, created_at: row.get(9)?, published_at: row.get(10)?,
            }),
        ).map_err(|e| e.to_string())
    }

    /// 云端记忆库：列出全部同步水位行（object_key, revision, content_hash, sync_state, updated_at）。
    pub fn cloud_sync_meta_rows(
        &self,
    ) -> Result<Vec<(String, i64, Option<String>, String, Option<String>)>, String> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| "telemetry store lock poisoned".to_string())?;
        let mut stmt = conn
            .prepare("SELECT object_key, revision, content_hash, sync_state, updated_at FROM cloud_sync_meta")
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            })
            .map_err(|e| e.to_string())?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())
    }

    /// 云端记忆库：按 id 查找用户自定义 L1（远端落地前的本地比对）。
    pub fn find_user_defined_l1(&self, id: &str) -> Result<Option<LocalMemorySnapshot>, String> {
        self.user_defined_l1_memories()
            .map(|items| items.into_iter().find(|item| item.id == id))
    }

    /// 云端记忆库：读取某对象 key 的同步水位行。
    pub fn cloud_sync_meta_row(
        &self,
        object_key: &str,
    ) -> Result<Option<(i64, Option<String>, String, Option<String>)>, String> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| "telemetry store lock poisoned".to_string())?;
        conn.query_row(
            "SELECT revision, content_hash, sync_state, updated_at FROM cloud_sync_meta WHERE object_key = ?1",
            [object_key],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .optional()
        .map_err(|e| e.to_string())
    }

    /// 云端记忆库：同步成功后回填水位（置 clean 或 conflict）。
    pub fn cloud_sync_meta_set(
        &self,
        object_key: &str,
        revision: i64,
        content_hash: Option<&str>,
        sync_state: &str,
    ) -> Result<(), String> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| "telemetry store lock poisoned".to_string())?;
        let now = chrono::Utc::now().to_rfc3339();
        conn.execute(
            "INSERT INTO cloud_sync_meta (object_key, revision, content_hash, sync_state, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(object_key) DO UPDATE SET revision = ?2, content_hash = ?3, sync_state = ?4, updated_at = ?5",
            params![object_key, revision, content_hash, sync_state, now],
        )
        .map_err(|e| e.to_string())?;
        Ok(())
    }

    /// Stores the last common version of an object locally.  This is never
    /// uploaded; it exists solely so a simultaneous edit can be resolved with
    /// an honest three-way comparison instead of a timestamp guess.
    pub fn cloud_sync_snapshot_set(
        &self,
        object_key: &str,
        revision: i64,
        content_hash: Option<&str>,
        content: Option<&str>,
    ) -> Result<(), String> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| "telemetry store lock poisoned".to_string())?;
        conn.execute(
            "INSERT INTO cloud_sync_snapshots (object_key, revision, content_hash, content, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(object_key) DO UPDATE SET revision = ?2, content_hash = ?3, content = ?4, updated_at = ?5",
            params![object_key, revision, content_hash, content, chrono::Utc::now().to_rfc3339()],
        )
        .map_err(|e| e.to_string())?;
        Ok(())
    }

    pub fn cloud_sync_snapshot_content(&self, object_key: &str) -> Result<Option<String>, String> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| "telemetry store lock poisoned".to_string())?;
        conn.query_row(
            "SELECT content FROM cloud_sync_snapshots WHERE object_key = ?1",
            [object_key],
            |row| row.get(0),
        )
        .optional()
        .map_err(|e| e.to_string())
    }

    pub fn cloud_sync_conflict_upsert(&self, conflict: &CloudSyncConflict) -> Result<(), String> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| "telemetry store lock poisoned".to_string())?;
        conn.execute(
            "INSERT INTO cloud_sync_conflicts
               (object_key, object_kind, base_content, local_content, remote_content, local_memory_type,
                remote_memory_type, remote_revision, remote_content_hash, local_updated_at, remote_updated_at, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
             ON CONFLICT(object_key) DO UPDATE SET
               object_kind = ?2, base_content = ?3, local_content = ?4, remote_content = ?5,
               local_memory_type = ?6, remote_memory_type = ?7, remote_revision = ?8,
               remote_content_hash = ?9, local_updated_at = ?10, remote_updated_at = ?11, created_at = ?12",
            params![
                conflict.object_key, conflict.object_kind, conflict.base_content, conflict.local_content,
                conflict.remote_content, conflict.local_memory_type, conflict.remote_memory_type,
                conflict.remote_revision, conflict.remote_content_hash, conflict.local_updated_at,
                conflict.remote_updated_at, conflict.created_at,
            ],
        )
        .map_err(|e| e.to_string())?;
        Ok(())
    }

    pub fn cloud_sync_conflicts(&self) -> Result<Vec<CloudSyncConflict>, String> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| "telemetry store lock poisoned".to_string())?;
        let mut stmt = conn.prepare(
            "SELECT object_key, object_kind, base_content, local_content, remote_content, local_memory_type,
                    remote_memory_type, remote_revision, remote_content_hash, local_updated_at,
                    remote_updated_at, created_at
               FROM cloud_sync_conflicts ORDER BY created_at DESC",
        ).map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |row| {
                Ok(CloudSyncConflict {
                    object_key: row.get(0)?,
                    object_kind: row.get(1)?,
                    base_content: row.get(2)?,
                    local_content: row.get(3)?,
                    remote_content: row.get(4)?,
                    local_memory_type: row.get(5)?,
                    remote_memory_type: row.get(6)?,
                    remote_revision: row.get(7)?,
                    remote_content_hash: row.get(8)?,
                    local_updated_at: row.get(9)?,
                    remote_updated_at: row.get(10)?,
                    created_at: row.get(11)?,
                })
            })
            .map_err(|e| e.to_string())?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())
    }

    pub fn cloud_sync_conflict_get(
        &self,
        object_key: &str,
    ) -> Result<Option<CloudSyncConflict>, String> {
        Ok(self
            .cloud_sync_conflicts()?
            .into_iter()
            .find(|item| item.object_key == object_key))
    }

    pub fn cloud_sync_conflict_delete(&self, object_key: &str) -> Result<(), String> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| "telemetry store lock poisoned".to_string())?;
        conn.execute(
            "DELETE FROM cloud_sync_conflicts WHERE object_key = ?1",
            [object_key],
        )
        .map_err(|e| e.to_string())?;
        Ok(())
    }

    /// 云端记忆库：本地最高对齐修订号，作为增量拉取水位线。
    pub fn cloud_sync_max_revision(&self) -> Result<i64, String> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| "telemetry store lock poisoned".to_string())?;
        conn.query_row(
            "SELECT COALESCE(MAX(revision), 0) FROM cloud_sync_meta",
            [],
            |r| r.get(0),
        )
        .map_err(|e| e.to_string())
    }

    /// 云端拉取落地：写入某层的发布版正文。本地已有发布版则覆盖；
    /// 没有则新建一条 published 文档（新设备首次拉取）。
    /// 与本地发布走同一语义：token_estimate 重算，L3 同步镜像 profile_summaries。
    pub fn upsert_published_layer_content(&self, layer: &str, content: &str) -> Result<(), String> {
        let content = strip_thinking_blocks(content);
        if content.trim().is_empty() {
            return Err("拉取到的记忆内容为空".into());
        }
        let token_estimate = estimate_transcript_tokens(&content);
        let now = chrono::Utc::now().to_rfc3339();
        let mut conn = self
            .conn
            .lock()
            .map_err(|_| "telemetry store lock poisoned".to_string())?;
        let tx = conn.transaction().map_err(|e| e.to_string())?;
        let existing_id: Option<String> = tx
            .query_row(
                "SELECT id FROM memory_layer_documents WHERE layer = ?1 AND state = 'published' ORDER BY published_at DESC LIMIT 1",
                [layer],
                |row| row.get(0),
            )
            .optional()
            .map_err(|e| e.to_string())?;
        match existing_id {
            Some(id) => {
                tx.execute(
                    "UPDATE memory_layer_documents SET content = ?1, token_estimate = ?2 WHERE id = ?3",
                    params![content, token_estimate, id],
                )
                .map_err(|e| e.to_string())?;
            }
            None => {
                tx.execute(
                    "INSERT INTO memory_layer_documents (id, layer, scope, content, state, token_estimate, source_count, created_at, published_at)
                     VALUES (?1, ?2, 'global', ?3, 'published', ?4, 0, ?5, ?5)",
                    params![format!("cloud-sync-{layer}"), layer, content, token_estimate, now],
                )
                .map_err(|e| e.to_string())?;
            }
        }
        if layer == "l3" {
            tx.execute(
                "INSERT INTO profile_summaries (profile_id, content, source_workspace_count, updated_at) VALUES ('local-user', ?1, 0, ?2)
                 ON CONFLICT(profile_id) DO UPDATE SET content=excluded.content, source_workspace_count=excluded.source_workspace_count, updated_at=excluded.updated_at",
                params![content, now],
            )
            .map_err(|e| e.to_string())?;
        }
        tx.commit().map_err(|e| e.to_string())?;
        Ok(())
    }

    /// Remove an un-published L3 document: drafts plus archived historical
    /// versions (e.g. leftovers from manual injection tests). Published
    /// Profiles are intentionally excluded so the active MCP context cannot
    /// be removed by the draft-management UI.
    pub fn delete_l3_draft(&self, id: &str) -> Result<(), String> {
        let mut conn = self
            .conn
            .lock()
            .map_err(|_| "telemetry store lock poisoned".to_string())?;
        let tx = conn.transaction().map_err(|e| e.to_string())?;
        let changed = tx.execute(
            "DELETE FROM memory_layer_documents WHERE id = ?1 AND layer = 'l3' AND state IN ('draft', 'archived')",
            [id],
        ).map_err(|e| e.to_string())?;
        if changed == 0 {
            return Err("未找到可删除的 L3 Profile 草案或归档版".into());
        }
        // 同步清理该草案的来源关联，避免残留孤儿记录。
        tx.execute(
            "DELETE FROM memory_layer_sources WHERE document_id = ?1",
            [id],
        )
        .map_err(|e| e.to_string())?;
        tx.commit().map_err(|e| e.to_string())?;
        Ok(())
    }

    pub fn restore_local_l1_memory_snapshot(
        &self,
        memories: &[LocalMemorySnapshot],
    ) -> Result<(), String> {
        let mut conn = self
            .conn
            .lock()
            .map_err(|_| "telemetry store lock poisoned".to_string())?;
        let transaction = conn.transaction().map_err(|e| e.to_string())?;
        transaction
            .execute("DELETE FROM local_memory_items", [])
            .map_err(|e| e.to_string())?;
        for memory in memories {
            transaction.execute(
                "INSERT INTO local_memory_items
                    (id, source_event_key, ordinal, memory, memory_type, durability, source, session_id, user_defined, event_time, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
                params![
                    memory.id, memory.source_event_key, memory.ordinal, memory.memory, memory.memory_type, memory.durability,
                    memory.source, memory.session_id, memory.user_defined, memory.event_time, memory.created_at, memory.updated_at,
                ],
            ).map_err(|e| e.to_string())?;
        }
        transaction.commit().map_err(|e| e.to_string())?;
        Ok(())
    }

    pub fn search_local_l1_memories(
        &self,
        query: &str,
        limit: u32,
    ) -> Result<Vec<LocalMemory>, String> {
        let terms = keyword_terms(query);
        if terms.is_empty() {
            return Ok(Vec::new());
        }
        // The SQL prefilter must see the same vocabulary the scorer can match:
        // keyword_score returns a positive alias-only score, so alias terms
        // join the prefilter or those rows would become unreachable.
        let mut filter_terms = terms.clone();
        filter_terms.extend(local_search_aliases(query));
        filter_terms.sort();
        filter_terms.dedup();
        let mut memories = self
            .keyword_prefilter_l1_memories(&filter_terms)?
            .into_iter()
            .filter_map(|mut memory| {
                let score = keyword_score(&memory.memory, query, &terms);
                (score > 0.0).then(|| {
                    memory.score = Some(score);
                    memory
                })
            })
            .collect::<Vec<_>>();
        // Reuse the evidence score already maintained by Vibe Assistant.  The
        // boost is intentionally small: relevance always wins over recency or
        // popularity, while durable/pinned memories break close ties.
        let ids = memories
            .iter()
            .map(|memory| memory.id.clone())
            .collect::<Vec<_>>();
        let importance = self
            .importance_records(&ids)?
            .into_iter()
            .map(|record| (record.memory_id.clone(), record))
            .collect::<HashMap<_, _>>();
        for memory in &mut memories {
            if let (Some(score), Some(record)) = (memory.score.as_mut(), importance.get(&memory.id))
            {
                *score += record.score as f64 / 10_000.0;
                if record.pinned {
                    *score += 0.015;
                }
            }
        }
        memories.sort_by(|left, right| {
            right
                .score
                .unwrap_or_default()
                .total_cmp(&left.score.unwrap_or_default())
        });
        memories.truncate(limit.min(100) as usize);
        Ok(memories)
    }

    /// Row cap for the SQL-side keyword prefilter.  It only guards pathological
    /// common-term queries (e.g. a frequent CJK bigram over a huge ledger);
    /// relevance ranking stays the Rust scorer's job.
    const KEYWORD_PREFILTER_ROW_CAP: i64 = 2_000;

    /// SQL-side candidate prefilter for L1 keyword search: every row whose
    /// content contains at least one term (or alias) as a substring, plus all
    /// user-defined memories.
    ///
    /// This replaced an in-memory scan of the most recent 500 auto-extracted
    /// rows, which silently made every older memory unreachable whenever the
    /// semantic sidecar was offline.  Substring LIKE keeps CJK matching exact
    /// (FTS5's default tokenizer does not segment Chinese, and the trigram
    /// tokenizer drops the common two-character words).
    fn keyword_prefilter_l1_memories(
        &self,
        terms: &[String],
    ) -> Result<Vec<LocalMemory>, String> {
        if terms.is_empty() {
            return Ok(Vec::new());
        }
        let conn = self
            .conn
            .lock()
            .map_err(|_| "telemetry store lock poisoned".to_string())?;
        let mut conditions = String::new();
        let mut patterns: Vec<String> = Vec::new();
        for term in terms {
            if !conditions.is_empty() {
                conditions.push_str(" OR ");
            }
            // Bound parameters keep quotes out of the pattern; ESCAPE handles
            // terms that legitimately contain LIKE wildcards ('_' survives
            // keyword_terms' latin filter).
            conditions.push_str(&format!(
                "memory LIKE ?{index} ESCAPE '\\'",
                index = patterns.len() + 1
            ));
            patterns.push(format!("%{}%", like_escape(term)));
        }
        let sql = format!(
            "SELECT id, memory, memory_type, durability, user_defined, updated_at, event_time
               FROM local_memory_items
              WHERE user_defined = 1 OR ({conditions})
              ORDER BY user_defined DESC, updated_at DESC
              LIMIT {cap}",
            conditions = conditions,
            cap = Self::KEYWORD_PREFILTER_ROW_CAP
        );
        let mut statement = conn.prepare(&sql).map_err(|e| e.to_string())?;
        let rows = statement
            .query_map(
                rusqlite::params_from_iter(patterns.iter().map(String::as_str)),
                |row| {
                Ok(LocalMemory {
                    id: row.get(0)?,
                    memory: row.get(1)?,
                    memory_type: row.get(2)?,
                    durability: row.get(3)?,
                    user_defined: row.get::<_, i64>(4)? != 0,
                    last_update_at: row.get(5)?,
                    event_time: row.get(6)?,
                    score: None,
                })
            })
            .map_err(|e| e.to_string())?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())
    }

    pub fn update_local_l1_memory(&self, id: &str, content: &str) -> Result<(), String> {
        if content.trim().is_empty() {
            return Err("记忆内容不能为空".into());
        }
        let changed = self
            .conn
            .lock()
            .map_err(|_| "telemetry store lock poisoned".to_string())?
            .execute(
                "UPDATE local_memory_items SET memory = ?1, updated_at = ?2 WHERE id = ?3",
                params![content.trim(), chrono::Utc::now().to_rfc3339(), id],
            )
            .map_err(|e| e.to_string())?;
        if changed == 0 {
            return Err("未找到本地记忆".into());
        }
        Ok(())
    }

    pub fn delete_local_l1_memory(&self, id: &str) -> Result<(), String> {
        let changed = self
            .conn
            .lock()
            .map_err(|_| "telemetry store lock poisoned".to_string())?
            .execute("DELETE FROM local_memory_items WHERE id = ?1", [id])
            .map_err(|e| e.to_string())?;
        if changed == 0 {
            return Err("未找到本地记忆".into());
        }
        Ok(())
    }

    /// 用户手动添加的自定义 L1 记忆：固定 long_term，标记 user_defined，
    /// 自动整理（BGE 巩固）与 L1 重建都必须跳过；删改只能由用户在面板
    /// 手动执行。scope 区分归属层级（l2 工作记忆 / l3 长期 Profile），
    /// id 用 `local-user-{scope}:` 前缀，与自动提取（local-l1:）和导入
    /// （local-import:）区分。
    pub fn add_user_defined_l1_memory(
        &self,
        content: &str,
        memory_type: &str,
        scope: &str,
    ) -> Result<LocalMemory, String> {
        let content = content.trim();
        if content.is_empty() {
            return Err("自定义记忆内容不能为空".into());
        }
        let memory_type = normalize_l1_memory_type(memory_type);
        if memory_type == "summary" {
            return Err("自定义记忆不支持 summary 类型，请选择 fact / decision / constraint / preference / open_item".into());
        }
        let scope = match scope {
            "l2" => "l2",
            _ => "l3",
        };
        let now = chrono::Utc::now().to_rfc3339();
        let digest = Sha256::digest(format!("user-defined:{scope}:{now}").as_bytes());
        let id = format!("local-user-{scope}:{}", hex(&digest)[..24].to_string());
        let source_event_key = format!("user-defined:{id}");
        let session_id = format!("user-defined:{scope}");
        self.conn
            .lock()
            .map_err(|_| "telemetry store lock poisoned".to_string())?
            .execute(
                "INSERT INTO local_memory_items
                    (id, source_event_key, ordinal, memory, memory_type, durability, source, session_id, user_defined, event_time, created_at, updated_at)
                 VALUES (?1, ?2, 0, ?3, ?4, 'long_term', 'user', ?5, 1, ?6, ?6, ?6)",
                params![id, source_event_key, content, memory_type, session_id, now],
            )
            .map_err(|e| e.to_string())?;
        Ok(LocalMemory {
            id,
            memory: content.to_string(),
            memory_type: memory_type.to_string(),
            durability: "long_term".to_string(),
            user_defined: true,
            last_update_at: now.clone(),
            event_time: Some(now),
            score: None,
        })
    }

    /// 按归属层级返回用户自定义 L1 记忆（不受时间窗限制）：注入与
    /// 整理证据都要强制携带，确保用户手写的长期约束不会被窗口或类型
    /// 筛选掉。旧版无层级前缀的 `local-user:` 条目视为 l3（长期 Profile）。
    pub fn user_defined_l1_memories_for_scope(
        &self,
        scope: &str,
    ) -> Result<Vec<LocalMemorySnapshot>, String> {
        self.user_defined_l1_memories().map(|items| {
            items
                .into_iter()
                .filter(|item| user_defined_memory_scope(&item.id) == scope)
                .collect()
        })
    }

    /// 全部用户自定义 L1 记忆（不受时间窗限制）：注入与整理证据都要
    /// 强制携带，确保用户手写的长期约束不会被窗口或类型筛选掉。
    pub fn user_defined_l1_memories(&self) -> Result<Vec<LocalMemorySnapshot>, String> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| "telemetry store lock poisoned".to_string())?;
        let mut statement = conn.prepare(
            "SELECT id, source_event_key, ordinal, memory, memory_type, durability, source, session_id, user_defined, event_time, created_at, updated_at
               FROM local_memory_items
              WHERE user_defined = 1
              ORDER BY created_at ASC",
        ).map_err(|e| e.to_string())?;
        let rows = statement
            .query_map([], |row| {
                Ok(LocalMemorySnapshot {
                    id: row.get(0)?,
                    source_event_key: row.get(1)?,
                    ordinal: row.get(2)?,
                    memory: row.get(3)?,
                    memory_type: row.get(4)?,
                    durability: row.get(5)?,
                    source: row.get(6)?,
                    session_id: row.get(7)?,
                    user_defined: row.get::<_, i64>(8)? != 0,
                    event_time: row.get(9)?,
                    created_at: row.get(10)?,
                    updated_at: row.get(11)?,
                })
            })
            .map_err(|e| e.to_string())?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())
    }

    /// 云端记忆库：按 id upsert 一条远端拉回的用户自定义 L1。
    /// 保持 user_defined=1 与 long_term 语义，与 add_user_defined_l1_memory
    /// 同一字段布局；id 来自源设备，本地已存在则仅更新内容与时间戳。
    pub fn upsert_synced_l1_memory(
        &self,
        id: &str,
        memory: &str,
        memory_type: &str,
        updated_at: &str,
    ) -> Result<(), String> {
        let memory_type = normalize_l1_memory_type(memory_type);
        if memory.trim().is_empty() {
            return Err("拉取到的自定义记忆内容为空".into());
        }
        let source_event_key = format!("user-defined:{id}");
        let conn = self
            .conn
            .lock()
            .map_err(|_| "telemetry store lock poisoned".to_string())?;
        conn.execute(
            "INSERT INTO local_memory_items
                (id, source_event_key, ordinal, memory, memory_type, durability, source, session_id, user_defined, event_time, created_at, updated_at)
             VALUES (?1, ?2, 0, ?3, ?4, 'long_term', 'user', ?5, 1, ?6, ?6, ?7)
             ON CONFLICT(id) DO UPDATE SET memory = ?3, memory_type = ?4, updated_at = ?7",
            params![id, source_event_key, memory.trim(), memory_type, format!("user-defined:{}", user_defined_memory_scope(id)), updated_at, updated_at],
        )
        .map_err(|e| e.to_string())?;
        Ok(())
    }

    /// 云端记忆库：墓碑落地——删除本地对应的用户自定义 L1。
    /// 仅 user_defined=1 条目可被远端删除传播，其余（自动提取/导入）不动。
    pub fn delete_synced_l1_memory(&self, id: &str) -> Result<bool, String> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| "telemetry store lock poisoned".to_string())?;
        let deleted = conn
            .execute(
                "DELETE FROM local_memory_items WHERE id = ?1 AND user_defined = 1",
                [id],
            )
            .map_err(|e| e.to_string())?;
        Ok(deleted > 0)
    }

    /// 云端记忆库：删除某对象 key 的本地水位行（本地删除已推成墓碑后清理）。
    pub fn cloud_sync_meta_delete(&self, object_key: &str) -> Result<(), String> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| "telemetry store lock poisoned".to_string())?;
        conn.execute(
            "DELETE FROM cloud_sync_meta WHERE object_key = ?1",
            [object_key],
        )
        .map_err(|e| e.to_string())?;
        Ok(())
    }

    /// 云端记忆库（Token 用量）：导出本机「天 × 来源」总量统计。
    /// 口径与用量分析页一致（usage_records 为主，无明细的会话用会话总量兜底），
    /// 按本地时区分天。
    pub fn usage_day_aggregates(&self) -> Result<Vec<UsageDayAggregate>, String> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| "telemetry store lock poisoned".to_string())?;
        let mut stmt = conn
            .prepare(
                "WITH feed AS (
                   SELECT occurred_at, source, input_tokens, output_tokens, cached_tokens FROM usage_records
                   UNION ALL
                   SELECT usage.refreshed_at, usage.source, usage.input_tokens, usage.output_tokens, usage.cached_tokens
                     FROM session_token_usage usage
                    WHERE NOT EXISTS (
                      SELECT 1 FROM usage_records record
                       WHERE record.source = usage.source AND record.session_id = usage.session_id
                    )
                 )
                 SELECT strftime('%Y-%m-%d', occurred_at, 'localtime'), source,
                        COALESCE(SUM(input_tokens), 0), COALESCE(SUM(output_tokens), 0),
                        COALESCE(SUM(cached_tokens), 0), COUNT(*)
                   FROM feed GROUP BY 1, 2 ORDER BY 1, 2",
            )
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |row| {
                Ok(UsageDayAggregate {
                    day: row.get(0)?,
                    source: row.get(1)?,
                    input_tokens: row.get(2)?,
                    output_tokens: row.get(3)?,
                    cached_tokens: row.get(4)?,
                    record_count: row.get(5)?,
                })
            })
            .map_err(|e| e.to_string())?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())
    }

    /// 云端记忆库（Token 用量）：整设备覆盖式落地远端总量统计。
    /// 返回写入的行数。occurred_at 用当天正午，保证日期过滤与按天分桶稳定。
    pub fn replace_remote_usage_totals(
        &self,
        device: &str,
        rows: &[UsageDayAggregate],
    ) -> Result<u64, String> {
        let mut conn = self
            .conn
            .lock()
            .map_err(|_| "telemetry store lock poisoned".to_string())?;
        let tx = conn.transaction().map_err(|e| e.to_string())?;
        tx.execute(
            "DELETE FROM usage_remote_totals WHERE device = ?1",
            [device],
        )
        .map_err(|e| e.to_string())?;
        for row in rows {
            tx.execute(
                "INSERT OR REPLACE INTO usage_remote_totals
                    (device, day, source, input_tokens, output_tokens, cached_tokens, record_count)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    device,
                    row.day,
                    row.source,
                    row.input_tokens,
                    row.output_tokens,
                    row.cached_tokens,
                    row.record_count
                ],
            )
            .map_err(|e| e.to_string())?;
        }
        tx.commit().map_err(|e| e.to_string())?;
        Ok(rows.len() as u64)
    }

    fn search_conversations(
        &self,
        query: &str,
        limit: u32,
    ) -> Result<Vec<ConversationSearchHit>, String> {
        let terms = keyword_terms(query);
        if terms.is_empty() {
            return Ok(Vec::new());
        }
        let conn = self
            .conn
            .lock()
            .map_err(|_| "telemetry store lock poisoned".to_string())?;
        let mut statement = conn
            .prepare(
                "SELECT id, source, session_id, occurred_at, conversation_text
                   FROM agent_events
                  WHERE event_type = 'Stop' AND conversation_text IS NOT NULL AND conversation_text != ''
                  ORDER BY id DESC LIMIT 300",
            )
            .map_err(|e| e.to_string())?;
        let rows = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                ))
            })
            .map_err(|e| e.to_string())?;
        let mut hits = rows
            .filter_map(|row| row.ok())
            .filter_map(|(id, source, session_id, occurred_at, text)| {
                let score = keyword_score(&text, query, &terms);
                (score > 0.0).then(|| ConversationSearchHit {
                    id,
                    source,
                    session_id,
                    occurred_at,
                    score,
                    preview: truncate_chars(&text, 2_400),
                })
            })
            .collect::<Vec<_>>();
        hits.sort_by(|left, right| {
            right
                .score
                .total_cmp(&left.score)
                .then_with(|| right.id.cmp(&left.id))
        });
        hits.truncate(limit.min(100) as usize);
        Ok(hits)
    }

    pub fn importance_records(
        &self,
        memory_ids: &[String],
    ) -> Result<Vec<MemoryImportance>, String> {
        if memory_ids.is_empty() {
            return Ok(Vec::new());
        }
        let conn = self
            .conn
            .lock()
            .map_err(|_| "telemetry store lock poisoned".to_string())?;
        let mut statement = conn.prepare(
            "SELECT memory_id, score, supporting_sessions, supporting_agents, duplicate_count, recall_count, pinned, updated_at
               FROM memory_importance WHERE memory_id = ?1",
        ).map_err(|e| e.to_string())?;
        let records = memory_ids
            .iter()
            .filter_map(|id| {
                statement
                    .query_row([id], |row| {
                        Ok(MemoryImportance {
                            memory_id: row.get(0)?,
                            score: row.get(1)?,
                            supporting_sessions: row.get(2)?,
                            supporting_agents: row.get(3)?,
                            duplicate_count: row.get(4)?,
                            recall_count: row.get(5)?,
                            pinned: row.get::<_, i64>(6)? != 0,
                            updated_at: row.get(7)?,
                        })
                    })
                    .optional()
                    .ok()
                    .flatten()
            })
            .collect::<Vec<_>>();
        Ok(records)
    }

    pub fn save_importance_records(&self, records: &[MemoryImportance]) -> Result<(), String> {
        let mut conn = self
            .conn
            .lock()
            .map_err(|_| "telemetry store lock poisoned".to_string())?;
        let transaction = conn.transaction().map_err(|e| e.to_string())?;
        for record in records {
            transaction.execute(
                "INSERT INTO memory_importance (memory_id, score, supporting_sessions, supporting_agents, duplicate_count, recall_count, pinned, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, COALESCE((SELECT recall_count FROM memory_importance WHERE memory_id = ?1), 0),
                         COALESCE((SELECT pinned FROM memory_importance WHERE memory_id = ?1), 0), ?6)
                 ON CONFLICT(memory_id) DO UPDATE SET score=excluded.score, supporting_sessions=excluded.supporting_sessions,
                   supporting_agents=excluded.supporting_agents, duplicate_count=excluded.duplicate_count, updated_at=excluded.updated_at",
                params![record.memory_id, record.score, record.supporting_sessions, record.supporting_agents, record.duplicate_count, record.updated_at],
            ).map_err(|e| e.to_string())?;
        }
        transaction.commit().map_err(|e| e.to_string())
    }

    pub fn set_memory_pinned(&self, memory_id: &str, pinned: bool) -> Result<(), String> {
        self.conn.lock().map_err(|_| "telemetry store lock poisoned".to_string())?.execute(
            "INSERT INTO memory_importance (memory_id, pinned, updated_at) VALUES (?1, ?2, ?3)
             ON CONFLICT(memory_id) DO UPDATE SET pinned=excluded.pinned, updated_at=excluded.updated_at",
            params![memory_id, pinned as i64, chrono::Utc::now().to_rfc3339()],
        ).map_err(|e| e.to_string())?;
        Ok(())
    }

    pub fn record_memory_recall(&self, memory_ids: &[String]) -> Result<(), String> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| "telemetry store lock poisoned".to_string())?;
        for memory_id in memory_ids.iter().filter(|id| !id.is_empty()) {
            conn.execute(
                "INSERT INTO memory_importance (memory_id, recall_count, updated_at) VALUES (?1, 1, ?2)
                 ON CONFLICT(memory_id) DO UPDATE SET recall_count=recall_count + 1, updated_at=excluded.updated_at",
                params![memory_id, chrono::Utc::now().to_rfc3339()],
            ).map_err(|e| e.to_string())?;
        }
        Ok(())
    }

    /// Best-effort recall telemetry for MCP child processes.  The desktop app
    /// can briefly hold the SQLite writer while persisting a Hook; never make
    /// an Agent wait behind that write just to increment a quality metric.
    pub fn try_record_memory_recall(&self, memory_ids: &[String]) -> Result<(), String> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| "telemetry store lock poisoned".to_string())?;
        conn.busy_timeout(std::time::Duration::from_millis(50))
            .map_err(|e| e.to_string())?;
        let result = (|| {
            for memory_id in memory_ids.iter().filter(|id| !id.is_empty()) {
                conn.execute(
                    "INSERT INTO memory_importance (memory_id, recall_count, updated_at) VALUES (?1, 1, ?2)
                     ON CONFLICT(memory_id) DO UPDATE SET recall_count=recall_count + 1, updated_at=excluded.updated_at",
                    params![memory_id, chrono::Utc::now().to_rfc3339()],
                ).map_err(|e| e.to_string())?;
            }
            Ok(())
        })();
        let _ = conn.busy_timeout(std::time::Duration::from_secs(5));
        result
    }

    /// Record an active Hook response after its HTTP write succeeds.
    pub fn try_record_hook_injection(
        &self,
        client_name: &str,
        event: &str,
        summary: &str,
        detail: Option<&str>,
    ) -> Result<(), String> {
        if !matches!(event, "prompt_inject" | "session_start_inject") {
            return Err("invalid Hook injection event".into());
        }
        self.record_memory_activity(client_name, event, summary, detail, true)
    }

    /// Record passive native MCP activity in the MCP ledger domain.
    pub fn try_record_mcp_access(
        &self,
        client_name: &str,
        tool_name: &str,
        summary: &str,
        detail: Option<&str>,
        success: bool,
    ) -> Result<(), String> {
        if matches!(tool_name, "prompt_inject" | "session_start_inject") {
            return Err("Hook injection is not an MCP tool call".into());
        }
        self.record_memory_activity(client_name, tool_name, summary, detail, success)
    }

    fn record_memory_activity(
        &self,
        client_name: &str,
        tool_name: &str,
        summary: &str,
        detail: Option<&str>,
        success: bool,
    ) -> Result<(), String> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| "telemetry store lock poisoned".to_string())?;
        conn.busy_timeout(std::time::Duration::from_millis(50))
            .map_err(|e| e.to_string())?;
        let occurred_at = chrono::Utc::now().to_rfc3339();
        let result = (|| {
            conn.execute(
                "INSERT INTO mcp_access_logs (occurred_at, client_name, tool_name, summary, detail, success) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![occurred_at, truncate_chars(client_name, 80), truncate_chars(tool_name, 80), truncate_chars(summary, 240), detail.map(|text| truncate_chars(text, 65_536)), success as i64],
            ).map_err(|e| e.to_string())?;
            let access_log_id = conn.last_insert_rowid();
            if let Some(detail) = detail {
                conn.execute(
                    "UPDATE activity_ledger SET payload_chars = ?1 WHERE access_log_id = ?2",
                    params![detail.chars().count() as i64, access_log_id],
                ).map_err(|e| e.to_string())?;
            }
            Ok(())
        })();
        let _ = conn.busy_timeout(std::time::Duration::from_secs(5));
        result
    }

    pub fn mcp_access_logs(&self, limit: u32) -> Result<Vec<McpAccessLog>, String> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| "telemetry store lock poisoned".to_string())?;
        let mut statement = conn
            .prepare(
                "SELECT ledger.id, ledger.occurred_at, COALESCE(ledger.client_name, ledger.session_id, ''),
                        ledger.tool_name, COALESCE(access.summary, ledger.error_reason, ledger.status),
                        access.detail, ledger.status = 'success'
                 FROM activity_ledger ledger
                 LEFT JOIN mcp_access_logs access ON access.id = ledger.access_log_id
                 WHERE ledger.domain = 'hook' AND access.id IS NOT NULL
                    OR (ledger.domain = 'mcp'
                        AND ledger.server_name = 'agent-manager-memory'
                        AND ledger.event_kind = 'mcp_tool_call')
                 ORDER BY ledger.id DESC LIMIT ?1",
            )
            .map_err(|e| e.to_string())?;
        let rows = statement
            .query_map([limit.min(100)], |row| {
                Ok(McpAccessLog {
                    id: row.get(0)?,
                    occurred_at: row.get(1)?,
                    client_name: row.get(2)?,
                    tool_name: row.get(3)?,
                    summary: row.get(4)?,
                    detail: row.get(5)?,
                    success: row.get::<_, i64>(6)? != 0,
                })
            })
            .map_err(|e| e.to_string())?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())
    }

    /// Return the newest shared-memory MCP handshake and tool-call evidence per
    /// client. This deliberately reports historical activity, not live status.
    pub fn mcp_access_activity(&self) -> Result<Vec<MemoryMcpActivity>, String> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| "telemetry store lock poisoned".to_string())?;
        let mut statement = conn.prepare(
            "SELECT client_name,
                    MAX(CASE WHEN event_kind = 'mcp_initialize' THEN occurred_at END),
                    MAX(CASE WHEN event_kind = 'mcp_tool_call' THEN occurred_at END)
             FROM activity_ledger
             WHERE domain = 'mcp' AND origin = 'memory_mcp'
               AND status = 'success' AND client_name IS NOT NULL
             GROUP BY client_name"
        ).map_err(|e| e.to_string())?;
        let rows = statement.query_map([], |row| Ok(MemoryMcpActivity {
            client_name: row.get(0)?,
            last_initialize_at: row.get(1)?,
            last_tool_call_at: row.get(2)?,
        })).map_err(|e| e.to_string())?;
        rows.collect::<Result<Vec<_>, _>>().map_err(|e| e.to_string())
    }

    /// Record a relay request without persisting the request key, arguments or payload.
    pub fn record_proxy_call(&self, key: &str, server: &str, tool: &str) -> Result<(), String> {
        let (observation_key, session_id) = proxy_call_identity(key)?;
        let server = sanitized_mcp_identifier(server);
        let tool = sanitized_mcp_identifier(tool);
        let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
        self.conn.lock().map_err(|_| "telemetry store lock poisoned".to_string())?
            .execute(
                "INSERT OR IGNORE INTO mcp_call_observations
                 (observation_key, source, origin, session_id, occurred_at, server_name, tool_name, status, result_observed)
                 VALUES (?1, 'stdio_proxy', 'stdio_proxy', ?2, ?3, ?4, ?5, 'pending', 0)",
                params![observation_key, session_id, now, server, tool],
            ).map_err(|e| e.to_string())?;
        Ok(())
    }

    pub fn record_proxy_system_failure(
        &self,
        connection_id: &str,
        server: Option<&str>,
        phase: &str,
        category: &str,
        kind: &str,
        reason: &str,
    ) -> Result<(), String> {
        if !matches!(phase, "control" | "assignment" | "startup" | "spawn" | "pipe" | "transport" | "wait" | "child")
            || !matches!(category, "relay_control" | "relay_assignment" | "relay_startup" | "relay_spawn" | "relay_transport" | "relay_child_exit")
            || !matches!(kind, "control_invalid" | "assignment_unavailable" | "telemetry_unavailable" | "command_invalid" | "handshake_failed" | "spawn_failed" | "pipe_unavailable" | "pump_failed" | "wait_failed" | "child_exit" | "transport_closed")
        {
            return Err("invalid relay failure metadata".into());
        }
        let server = server.unwrap_or("unknown");
        let server = sanitized_mcp_identifier(server);
        let connection_hash = hex(&Sha256::digest(connection_id.as_bytes()));
        let observation_key = format!("relay:{connection_hash}:{}", chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default());
        let session_id = format!("relay:{connection_hash}");
        let reason = truncate_chars(reason, 160);
        let occurred_at = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
        self.conn.lock().map_err(|_| "telemetry store lock poisoned".to_string())?
            .execute(
                "INSERT OR IGNORE INTO mcp_call_observations
                 (observation_key, source, origin, session_id, occurred_at, server_name, tool_name, status, error_category, result_observed, error_phase, failure_kind, error_reason)
                 VALUES (?1, 'stdio_proxy', 'stdio_relay', ?2, ?3, ?4, '__relay__', 'failed', ?5, 0, ?6, ?7, ?8)",
                params![observation_key, session_id, occurred_at, server, category, phase, kind, reason],
            ).map_err(|e| e.to_string())?;
        Ok(())
    }

    pub fn finish_proxy_call_with_reason(
        &self,
        key: &str,
        status: &str,
        category: Option<&str>,
        phase: Option<&str>,
        kind: Option<&str>,
        code: Option<i64>,
        reason: Option<&str>,
    ) -> Result<(), String> {
        let (observation_key, _) = proxy_call_identity(key)?;
        if !matches!(status, "success" | "failed") {
            return Err("invalid proxy call status".into());
        }
        let category = match (status, category) {
            ("success", _) | (_, None) => None,
            (_, Some(value @ ("protocol_error" | "mcp_error" | "disconnect"))) => Some(value),
            _ => return Err("invalid proxy error category".into()),
        };
        if !matches!(phase, None | Some("request" | "response" | "transport")) {
            return Err("invalid proxy error phase".into());
        }
        if !matches!(kind, None | Some("parse_error" | "rpc_error" | "tool_error" | "transport_closed" | "unknown")) {
            return Err("invalid proxy failure kind".into());
        }
        let result_observed = !matches!(category, Some("disconnect" | "protocol_error"));
        self.conn.lock().map_err(|_| "telemetry store lock poisoned".to_string())?
            .execute(
                "UPDATE mcp_call_observations SET status = ?2, error_category = ?3, result_observed = ?4,
                    error_phase = ?5, failure_kind = ?6, error_code = ?7, error_reason = ?8
                 WHERE observation_key = ?1 AND source = 'stdio_proxy' AND status = 'pending'",
                params![observation_key, status, category, result_observed as i64, phase, kind, code, reason.map(|value| truncate_chars(value, 160))],
            ).map_err(|e| e.to_string())?;
        Ok(())
    }

    pub fn finish_proxy_call(
        &self,
        key: &str,
        status: &str,
        category: Option<&str>,
        phase: Option<&str>,
        kind: Option<&str>,
        code: Option<i64>,
    ) -> Result<(), String> {
        self.finish_proxy_call_with_reason(key, status, category, phase, kind, code, None)
    }

    /// Record the start of a DSH plugin-observed MCP call. The cordis plugin
    /// reports only the server-qualified tool name; arguments and payloads
    /// never reach this store.
    pub fn record_dsh_plugin_call(
        &self,
        observation_key: &str,
        session_id: &str,
        occurred_at: &str,
        server: &str,
        tool: &str,
    ) -> Result<(), String> {
        self.conn.lock().map_err(|_| "telemetry store lock poisoned".to_string())?
            .execute(
                "INSERT OR IGNORE INTO mcp_call_observations
                 (observation_key, source, origin, session_id, occurred_at, server_name, tool_name, status, result_observed)
                 VALUES (?1, 'dsh', 'dsh_plugin', ?2, ?3, ?4, ?5, 'pending', 0)",
                params![observation_key, session_id, occurred_at, server, tool],
            ).map_err(|e| e.to_string())?;
        Ok(())
    }

    /// Complete a DSH plugin-observed call. A missing pending row means the
    /// start report was lost (plugin loaded mid-call, dropped POST), so the
    /// completed observation is inserted directly instead of being dropped.
    pub fn finish_dsh_plugin_call(
        &self,
        observation_key: &str,
        session_id: &str,
        occurred_at: &str,
        server: &str,
        tool: &str,
        status: &str,
        category: Option<&str>,
        phase: Option<&str>,
        kind: Option<&str>,
        code: Option<i64>,
        reason: Option<&str>,
    ) -> Result<(), String> {
        if !matches!(status, "success" | "failed") {
            return Err("invalid dsh observation status".into());
        }
        if !matches!(category, None | Some("tool_error" | "mcp_error" | "protocol_error" | "disconnect" | "timeout" | "unknown_error")) {
            return Err("invalid dsh observation category".into());
        }
        if !matches!(phase, None | Some("request" | "response" | "transport")) {
            return Err("invalid dsh observation phase".into());
        }
        if !matches!(kind, None | Some("parse_error" | "rpc_error" | "tool_error" | "transport_closed" | "unknown")) {
            return Err("invalid dsh observation failure kind".into());
        }
        let result_observed = !matches!(category, Some("disconnect" | "protocol_error" | "timeout"));
        let reason = reason.map(|value| truncate_chars(value, 160));
        let conn = self.conn.lock().map_err(|_| "telemetry store lock poisoned".to_string())?;
        let changed = conn.execute(
            "UPDATE mcp_call_observations SET status = ?2, error_category = ?3, result_observed = ?4,
                error_phase = ?5, failure_kind = ?6, error_code = ?7, error_reason = ?8
             WHERE observation_key = ?1 AND source = 'dsh' AND status = 'pending'",
            params![observation_key, status, category, result_observed as i64, phase, kind, code, reason],
        ).map_err(|e| e.to_string())?;
        if changed == 0 {
            conn.execute(
                "INSERT OR IGNORE INTO mcp_call_observations
                 (observation_key, source, origin, session_id, occurred_at, server_name, tool_name, status,
                  error_category, result_observed, error_phase, failure_kind, error_code, error_reason)
                 VALUES (?1, 'dsh', 'dsh_plugin', ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
                params![observation_key, session_id, occurred_at, server, tool, status, category,
                    result_observed as i64, phase, kind, code, reason],
            ).map_err(|e| e.to_string())?;
        }
        Ok(())
    }

    /// Mark missing evidence, not a tool failure. Late observed responses can
    /// still finish the legacy pending row and replace the unknown projection.
    pub fn mark_proxy_call_unknown(&self, key: &str, reason: &str) -> Result<(), String> {
        if !matches!(reason, "observer_metadata_limit" | "observer_framing_invalid" | "observer_frame_invalid"
            | "observer_queue_overflow" | "request_id_reused" | "connection_closed") {
            return Err("invalid observation gap reason".into());
        }
        let (observation_key, _) = proxy_call_identity(key)?;
        self.conn.lock().map_err(|_| "telemetry store lock poisoned".to_string())?.execute(
            "UPDATE mcp_call_observations SET error_category = 'observation_lost',
                error_phase = 'transport', failure_kind = 'unknown', error_reason = ?2, result_observed = 0
             WHERE observation_key = ?1 AND source = 'stdio_proxy' AND status = 'pending'
               AND error_category IS NOT 'observation_lost'", params![observation_key, reason]
        ).map_err(|e| e.to_string())?;
        Ok(())
    }

    /// A heartbeat proves the relay exists, not that a historical request is
    /// still executing. Fifteen minutes is an evidence horizon, not a timeout.
    pub fn reconcile_mcp_observations(&self) -> Result<(), String> {
        let now = chrono::Utc::now();
        let heartbeat_cutoff = (now - chrono::Duration::seconds(60))
            .to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
        let evidence_cutoff = (now - chrono::Duration::minutes(15))
            .to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
        self.conn.lock().map_err(|_| "telemetry store lock poisoned".to_string())?.execute(
            "UPDATE mcp_call_observations SET error_category = 'observation_lost', result_observed = 0,
                error_phase = 'transport', failure_kind = 'unknown',
                error_reason = CASE WHEN origin = 'transcript' THEN 'transcript_result_missing'
                  WHEN origin = 'dsh_plugin' THEN 'plugin_result_missing'
                  WHEN NOT EXISTS (SELECT 1 FROM mcp_proxy_connections c
                    WHERE c.connection_id = mcp_call_observations.session_id AND c.last_seen_at >= ?1)
                    THEN 'connection_lost' ELSE 'result_unconfirmed' END
             WHERE status = 'pending' AND error_category IS NOT 'observation_lost'
               AND (origin IN ('transcript', 'dsh_plugin') OR (origin = 'stdio_proxy' AND
                 (occurred_at < ?2 OR (occurred_at < ?1 AND NOT EXISTS
                    (SELECT 1 FROM mcp_proxy_connections c WHERE c.connection_id = mcp_call_observations.session_id
                     AND c.last_seen_at >= ?1)))))", params![heartbeat_cutoff, evidence_cutoff]
        ).map_err(|e| e.to_string())?;
        Ok(())
    }

    pub fn proxy_connection_open(&self, id: &str, server: &str, assignment_id: &str) -> Result<(), String> {
        let connection_id = opaque_proxy_id(id)?;
        let assignment_id = opaque_proxy_id(assignment_id)?;
        let server = sanitized_mcp_identifier(server);
        let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
        self.conn.lock().map_err(|_| "telemetry store lock poisoned".to_string())?
            .execute(
                "INSERT INTO mcp_proxy_connections (connection_id, server_name, assignment_id, last_seen_at)
                 VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(connection_id) DO UPDATE SET server_name = excluded.server_name,
                    assignment_id = excluded.assignment_id, last_seen_at = excluded.last_seen_at",
                params![connection_id, server, assignment_id, now],
            ).map_err(|e| e.to_string())?;
        Ok(())
    }

    pub fn proxy_connection_touch(&self, id: &str) -> Result<(), String> {
        let connection_id = opaque_proxy_id(id)?;
        let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
        self.conn.lock().map_err(|_| "telemetry store lock poisoned".to_string())?
            .execute("UPDATE mcp_proxy_connections SET last_seen_at = ?2 WHERE connection_id = ?1",
                params![connection_id, now]).map_err(|e| e.to_string())?;
        Ok(())
    }

    pub fn proxy_connection_close(&self, id: &str) -> Result<(), String> {
        let connection_id = opaque_proxy_id(id)?;
        let mut conn = self.conn.lock().map_err(|_| "telemetry store lock poisoned".to_string())?;
        let tx = conn.transaction().map_err(|e| e.to_string())?;
        tx.execute("UPDATE mcp_call_observations SET error_category = 'observation_lost', result_observed = 0,
                error_phase = 'transport', failure_kind = 'unknown', error_reason = 'connection_closed'
             WHERE session_id = ?1 AND origin = 'stdio_proxy' AND status = 'pending'
                AND error_category IS NOT 'observation_lost'", [&connection_id]).map_err(|e| e.to_string())?;
        tx.execute("DELETE FROM mcp_proxy_connections WHERE connection_id = ?1", [connection_id]).map_err(|e| e.to_string())?;
        tx.commit().map_err(|e| e.to_string())
    }

    pub fn proxy_active_connections(&self, assignment_id: &str) -> Result<i64, String> {
        let assignment_id = opaque_proxy_id(assignment_id)?;
        let cutoff = (chrono::Utc::now() - chrono::Duration::seconds(15))
            .to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
        self.conn.lock().map_err(|_| "telemetry store lock poisoned".to_string())?
            .query_row(
                "SELECT COUNT(*) FROM mcp_proxy_connections WHERE assignment_id = ?1 AND last_seen_at >= ?2",
                params![assignment_id, cutoff], |row| row.get(0),
            ).map_err(|e| e.to_string())
    }

    pub fn mcp_failed_calls(&self, offset: u32, limit: u32) -> Result<McpFailedCallPage, String> {
        self.mcp_failed_calls_filtered(offset, limit, None, None)
    }

    pub fn mcp_failed_calls_filtered(
        &self, offset: u32, limit: u32, server: Option<&str>, origin: Option<&str>,
    ) -> Result<McpFailedCallPage, String> {
        self.mcp_failed_calls_scoped(offset, limit, server, origin, None, None, None, None, None, None)
    }

    pub fn mcp_failed_calls_scoped(
        &self, offset: u32, limit: u32, server: Option<&str>, origin: Option<&str>,
        tool: Option<&str>, start_at: Option<&str>, end_at: Option<&str>, snapshot_id: Option<i64>,
        before_at: Option<&str>, before_id: Option<i64>,
    ) -> Result<McpFailedCallPage, String> {
        let conn = self.conn.lock().map_err(|_| "telemetry store lock poisoned".to_string())?;
        let mut conditions = vec!["domain = 'mcp'".to_string(), "event_kind = 'mcp_tool_call'".to_string(), "status = 'failed'".to_string()];
        let mut values = Vec::<String>::new();
        for (column, value, operator) in [
            ("server_name", server, "="), ("origin", origin, "="), ("tool_name", tool, "="),
            ("occurred_at", start_at, ">="), ("occurred_at", end_at, "<"),
        ] {
            if let Some(value) = value.filter(|value| !value.trim().is_empty()) {
                conditions.push(format!("{column} {operator} ?{}", values.len() + 1));
                values.push(value.to_string());
            }
        }
        let snapshot_id = match snapshot_id {
            Some(id) => id.max(0),
            None => conn.query_row("SELECT COALESCE(MAX(id), 0) FROM activity_ledger", [], |row| row.get::<_, i64>(0))
                .map_err(|e| e.to_string())?,
        };
        conditions.push(format!("id <= ?{}", values.len() + 1));
        values.push(snapshot_id.to_string());
        let where_sql = format!(" WHERE {}", conditions.join(" AND "));
        let total: i64 = conn.query_row(
            &format!("SELECT COUNT(*) FROM activity_ledger{where_sql}"),
            rusqlite::params_from_iter(values.iter()), |row| row.get(0),
        ).map_err(|e| e.to_string())?;
        let mut categories_stmt = conn.prepare(&format!(
            "SELECT COALESCE(error_category, 'unknown_error'), COUNT(*) FROM activity_ledger{where_sql}
             GROUP BY COALESCE(error_category, 'unknown_error') ORDER BY COUNT(*) DESC",
        )).map_err(|e| e.to_string())?;
        let categories = categories_stmt.query_map(rusqlite::params_from_iter(values.iter()), |row| Ok(McpFailureCategoryCount {
            category: row.get(0)?, count: row.get(1)?,
        })).map_err(|e| e.to_string())?.collect::<Result<Vec<_>, _>>().map_err(|e| e.to_string())?;
        let mut page_conditions = conditions.clone();
        if let (Some(before_at), Some(before_id)) = (before_at, before_id) {
            let time_index = values.len() + 1;
            let id_index = values.len() + 2;
            page_conditions.push(format!("(occurred_at < ?{time_index} OR (occurred_at = ?{time_index} AND id < ?{id_index}))"));
            values.push(before_at.to_string());
            values.push(before_id.to_string());
        }
        let page_where_sql = format!(" WHERE {}", page_conditions.join(" AND "));
        let mut statement = conn.prepare(&format!(
            "SELECT id, source, origin, session_id, occurred_at, server_name, tool_name, status, error_category, result_observed,
                error_phase, failure_kind, error_code, error_reason
             FROM activity_ledger{page_where_sql}
             ORDER BY occurred_at DESC, id DESC LIMIT ?{} OFFSET ?{}",
             values.len() + 1, values.len() + 2,
        )).map_err(|e| e.to_string())?;
        let limit = limit.clamp(1, 100) as i64;
        let offset = offset as i64;
        let mut query_values: Vec<&dyn rusqlite::ToSql> = values.iter()
            .map(|value| value as &dyn rusqlite::ToSql).collect();
        query_values.push(&limit);
        query_values.push(&offset);
        let calls = statement.query_map(rusqlite::params_from_iter(query_values), |row| Ok(McpCallObservation {
            id: row.get(0)?, source: row.get(1)?, origin: row.get(2)?, session_id: row.get(3)?,
            occurred_at: row.get(4)?, server_name: row.get(5)?, tool_name: row.get(6)?,
            status: row.get(7)?, error_category: row.get(8)?, result_observed: row.get::<_, i64>(9)? != 0,
            error_phase: row.get(10)?, failure_kind: row.get(11)?, error_code: row.get(12)?, error_reason: row.get(13)?,
        })).map_err(|e| e.to_string())?.collect::<Result<Vec<_>, _>>().map_err(|e| e.to_string())?;
        Ok(McpFailedCallPage { total, snapshot_id, categories, calls })
    }

    /// Global dashboard totals and a rolling seven-day window, independent of call-view filters.
    pub fn mcp_dashboard_stats(&self) -> Result<McpDashboardStats, String> {
        self.reconcile_mcp_observations()?;
        let now = chrono::Utc::now();
        let start = (now - chrono::Duration::days(7))
            .to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
        let end = now.to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
        let conn = self.conn.lock().map_err(|_| "telemetry store lock poisoned".to_string())?;
        let data_version: i64 = conn.query_row("PRAGMA data_version", [], |row| row.get(0)).map_err(|e| e.to_string())?;
        let total_changes: i64 = conn.query_row("SELECT total_changes()", [], |row| row.get(0)).map_err(|e| e.to_string())?;
        let mut cache = self.mcp_dashboard_cache.lock().map_err(|_| "MCP dashboard cache poisoned".to_string())?;
        if let Some(cached) = cache.as_ref() {
            if cached.data_version == data_version && cached.total_changes == total_changes
                && cached.captured_at.elapsed() < std::time::Duration::from_secs(60) {
                return Ok(cached.stats.clone());
            }
        }
        // Relay startup/transport diagnostics are not tool calls.
        let (total_calls, successful_calls_7d, failed_calls_7d) = conn.query_row(
            "SELECT COUNT(*),
                COALESCE(SUM(occurred_at >= ?1 AND occurred_at < ?2 AND status = 'success'), 0),
                COALESCE(SUM(occurred_at >= ?1 AND occurred_at < ?2 AND status = 'failed'), 0)
             FROM activity_ledger
             WHERE domain = 'mcp' AND event_kind = 'mcp_tool_call' AND tool_name <> '__relay__'",
            params![start, end],
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?, row.get::<_, i64>(2)?)),
        ).map_err(|e| e.to_string())?;
        // Group by both names: separate servers can expose identically named tools.
        // No top-N limit: even a rarely called tool must be shown above the threshold.
        let mut statement = conn.prepare(
            "SELECT server_name, tool_name, SUM(status = 'success'), SUM(status = 'failed')
             FROM activity_ledger
             WHERE domain = 'mcp' AND event_kind = 'mcp_tool_call' AND tool_name <> '__relay__'
               AND occurred_at >= ?1 AND occurred_at < ?2 AND status IN ('success', 'failed')
             GROUP BY server_name, tool_name
             HAVING SUM(status = 'failed') * 5 > COUNT(*)
             ORDER BY SUM(status = 'failed') * 1.0 / COUNT(*) DESC,
                      SUM(status = 'failed') DESC, server_name, tool_name"
        ).map_err(|e| e.to_string())?;
        let high_failure_tools = statement.query_map(params![start, end], |row| Ok(McpToolHealth {
            server_name: row.get(0)?, tool_name: row.get(1)?,
            successful_calls: row.get(2)?, failed_calls: row.get(3)?,
        })).map_err(|e| e.to_string())?.collect::<Result<Vec<_>, _>>().map_err(|e| e.to_string())?;
        let stats = McpDashboardStats {
            total_calls, health_start_at: start, health_end_at: end,
            successful_calls_7d, failed_calls_7d, high_failure_tools,
        };
        *cache = Some(McpDashboardCache { data_version, total_changes, captured_at: std::time::Instant::now(), stats: stats.clone() });
        Ok(stats)
    }

    pub fn mcp_call_stats(
        &self,
        start_at: Option<&str>,
        end_at: Option<&str>,
        source: Option<&str>,
        server: Option<&str>,
        tool: Option<&str>,
        recent_limit: u32,
    ) -> Result<McpCallStats, String> {
        self.reconcile_mcp_observations()?;
        let conn = self.conn.lock().map_err(|_| "telemetry store lock poisoned".to_string())?;
        let mut conditions = vec!["domain = 'mcp'".to_string(), "event_kind = 'mcp_tool_call'".to_string(), "tool_name <> '__relay__'".to_string()];
        let mut values: Vec<String> = Vec::new();
        for (column, value, operator) in [
            ("occurred_at", start_at, ">="), ("occurred_at", end_at, "<"),
            ("source", source, "="), ("server_name", server, "="), ("tool_name", tool, "="),
        ] {
            if let Some(value) = value {
                conditions.push(format!("{column} {operator} ?{}", values.len() + 1));
                values.push(value.to_string());
            }
        }
        let where_sql = if conditions.is_empty() { String::new() } else { format!(" WHERE {}", conditions.join(" AND ")) };
        let mut statement = conn.prepare(&format!(
            "SELECT COUNT(*), COALESCE(SUM(origin = 'stdio_proxy'), 0), COALESCE(SUM(origin = 'transcript'), 0),
                    COALESCE(SUM(origin = 'dsh_plugin'), 0),
                    COALESCE(SUM(status = 'success'), 0), COALESCE(SUM(status = 'failed'), 0),
                    COALESCE(SUM(status = 'pending'), 0), COALESCE(SUM(status = 'unknown'), 0), COUNT(DISTINCT server_name),
                    COUNT(DISTINCT tool_name), MAX(occurred_at)
             FROM activity_ledger{where_sql}"
        )).map_err(|e| e.to_string())?;
        let row = statement.query_row(rusqlite::params_from_iter(values.iter().map(|value| value as &dyn rusqlite::ToSql)), |row| Ok((
            row.get::<_, i64>(0)?, row.get::<_, i64>(1)?, row.get::<_, i64>(2)?, row.get::<_, i64>(3)?,
            row.get::<_, i64>(4)?, row.get::<_, i64>(5)?, row.get::<_, i64>(6)?,
            row.get::<_, i64>(7)?, row.get::<_, i64>(8)?, row.get::<_, i64>(9)?, row.get::<_, Option<String>>(10)?,
        ))).map_err(|e| e.to_string())?;
        let mut recent_stmt = conn.prepare(&format!(
            "SELECT id, source, origin, session_id, occurred_at, server_name, tool_name, status, error_category, result_observed,
                error_phase, failure_kind, error_code, error_reason
             FROM activity_ledger{where_sql} ORDER BY occurred_at DESC LIMIT ?{}",
            values.len() + 1
        )).map_err(|e| e.to_string())?;
        let limit = recent_limit.clamp(1, 200) as i64;
        let mut recent_values: Vec<&dyn rusqlite::ToSql> = values.iter()
            .map(|value| value as &dyn rusqlite::ToSql).collect();
        recent_values.push(&limit);
        let recent = recent_stmt.query_map(rusqlite::params_from_iter(recent_values), |row| Ok(McpCallObservation {
            id: row.get(0)?, source: row.get(1)?, origin: row.get(2)?, session_id: row.get(3)?, occurred_at: row.get(4)?,
            server_name: row.get(5)?, tool_name: row.get(6)?, status: row.get(7)?, error_category: row.get(8)?,
            result_observed: row.get::<_, i64>(9)? != 0, error_phase: row.get(10)?,
            failure_kind: row.get(11)?, error_code: row.get(12)?, error_reason: row.get(13)?,
        })).map_err(|e| e.to_string())?.collect::<Result<Vec<_>, _>>().map_err(|e| e.to_string())?;
        let groups = |column: &str| -> Result<Vec<McpCallGroup>, String> {
            let (grouping, server_column, limit) = if column == "tool_name" {
                ("server_name, tool_name", "server_name", "")
            } else { (column, "NULL", "LIMIT 50") };
            let sql = format!("SELECT {column}, COUNT(*), COALESCE(SUM(status = 'success'), 0), COALESCE(SUM(status = 'failed'), 0), COALESCE(SUM(status = 'pending'), 0), COALESCE(SUM(status = 'unknown'), 0), {server_column} FROM activity_ledger{where_sql} GROUP BY {grouping} ORDER BY COUNT(*) DESC, {column} {limit}");
            let mut stmt = conn.prepare(&sql).map_err(|e| e.to_string())?;
            let rows = stmt.query_map(rusqlite::params_from_iter(values.iter().map(|value| value as &dyn rusqlite::ToSql)), |row| Ok(McpCallGroup {
                name: row.get(0)?, calls: row.get(1)?, success: row.get(2)?, failed: row.get(3)?, pending: row.get(4)?, unknown: row.get(5)?, server_name: row.get(6)?,
            })).map_err(|e| e.to_string())?;
            rows.collect::<Result<Vec<_>, _>>().map_err(|e| e.to_string())
        };
        let servers = groups("server_name")?;
        let tools = groups("tool_name")?;
        let mut coverage_stmt = conn.prepare("SELECT source, state, transcript_count, call_count, scanned_at, reason FROM mcp_call_coverage ORDER BY source").map_err(|e| e.to_string())?;
        let coverage = coverage_stmt.query_map([], |row| Ok(McpCallCoverage { source: row.get(0)?, state: row.get(1)?, transcript_count: row.get(2)?, call_count: row.get(3)?, scanned_at: row.get(4)?, reason: row.get(5)? })).map_err(|e| e.to_string())?.collect::<Result<Vec<_>, _>>().map_err(|e| e.to_string())?;
        Ok(McpCallStats {
            total_calls: row.0, proxy_calls: row.1, transcript_calls: row.2, plugin_calls: row.3,
            native_calls: row.0 - row.1 - row.2 - row.3,
            successful_calls: row.4, failed_calls: row.5, pending_calls: row.6, unknown_calls: row.7,
            server_count: row.8, tool_count: tools.len() as i64, last_observed_at: row.10,
            servers, tools, recent, coverage,
        })
    }

    /// Hook-only delivery accounting. MCP tool results never enter this view.
    pub fn memory_injection_stats(&self, days: u32) -> Result<Vec<MemoryInjectionStats>, String> {
        let days = days.clamp(1, 90);
        let cutoff = (chrono::Utc::now() - chrono::Duration::days(days as i64))
            .to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
        let conn = self
            .conn
            .lock()
            .map_err(|_| "telemetry store lock poisoned".to_string())?;
        let mut statement = conn
            .prepare(
                "SELECT client_name,
                         SUM(CASE WHEN event_kind = 'hook_session_injection' THEN 1 ELSE 0 END),
                         SUM(CASE WHEN event_kind = 'hook_prompt_injection' THEN 1 ELSE 0 END),
                         SUM(CASE WHEN event_kind = 'hook_skip' THEN 1 ELSE 0 END),
                         SUM(payload_chars)
                  FROM activity_ledger
                  WHERE domain = 'hook' AND occurred_at >= ?1 AND status = 'success'
                 GROUP BY client_name
                  ORDER BY SUM(payload_chars) DESC",
            )
            .map_err(|e| e.to_string())?;
        let rows = statement
            .query_map([&cutoff], |row| {
                Ok(MemoryInjectionStats {
                    client_name: row.get(0)?,
                    session_injections: row
                        .get::<_, Option<i64>>(1)?
                        .unwrap_or_default()
                        .max(0) as u64,
                    prompt_injections: row
                        .get::<_, Option<i64>>(2)?
                        .unwrap_or_default()
                        .max(0) as u64,
                    prompt_skips: row.get::<_, Option<i64>>(3)?.unwrap_or_default().max(0) as u64,
                    injected_chars: row.get::<_, Option<i64>>(4)?.unwrap_or_default().max(0) as u64,
                })
            })
            .map_err(|e| e.to_string())?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())
    }

    pub fn importance_evidence(
        &self,
        candidates: &[MemoryImportanceCandidate],
    ) -> Result<HashMap<String, MemoryImportanceEvidence>, String> {
        if candidates.is_empty() {
            return Ok(HashMap::new());
        }
        let conn = self
            .conn
            .lock()
            .map_err(|_| "telemetry store lock poisoned".to_string())?;
        let mut statement = conn
            .prepare(
                "SELECT source, session_id, conversation_text FROM agent_events
              WHERE conversation_state = 'full' AND conversation_text IS NOT NULL
              ORDER BY id DESC LIMIT 500",
            )
            .map_err(|e| e.to_string())?;
        let rows = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })
            .map_err(|e| e.to_string())?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())?;
        let mut matches: HashMap<
            String,
            (
                std::collections::HashSet<String>,
                std::collections::HashSet<String>,
            ),
        > = HashMap::new();
        for candidate in candidates {
            let terms = keyword_terms(&candidate.content);
            if terms.len() < 2 {
                continue;
            }
            for (source, session_id, conversation) in &rows {
                let hit_count = terms
                    .iter()
                    .filter(|term| conversation.to_lowercase().contains(term.as_str()))
                    .count();
                let required = if terms.len() >= 8 { 3 } else { 2 };
                if hit_count >= required {
                    let entry = matches.entry(candidate.memory_id.clone()).or_default();
                    entry.0.insert(format!("{source}:{session_id}"));
                    entry.1.insert(source.clone());
                }
            }
        }
        Ok(matches
            .into_iter()
            .map(|(memory_id, (sessions, agents))| {
                (
                    memory_id,
                    MemoryImportanceEvidence {
                        supporting_sessions: sessions.len() as i64,
                        supporting_agents: agents.len() as i64,
                    },
                )
            })
            .collect())
    }

    fn summary(&self) -> Result<TelemetrySummary, String> {
        self.conn
            .lock()
            .map_err(|_| "telemetry store lock poisoned".to_string())?
            .query_row(
                // Totals come from the same detail-first feed as the usage
                // dashboard, so the Memory Center panel and the analytics
                // page can never show different numbers for one ledger.
                "WITH sessions AS (
                   SELECT DISTINCT source, session_id FROM agent_events WHERE session_id != 'unknown'
                   UNION
                   SELECT source, session_id FROM session_token_usage
                 ), usage AS (
                   SELECT source, session_id, input_tokens, output_tokens, cached_tokens, origin, refreshed_at
                     FROM session_token_usage
                 ), feed AS (
                   SELECT input_tokens, output_tokens, cached_tokens FROM usage_records
                   UNION ALL
                   SELECT usage.input_tokens, usage.output_tokens, usage.cached_tokens
                     FROM session_token_usage usage
                    WHERE NOT EXISTS (
                      SELECT 1 FROM usage_records record
                       WHERE record.source = usage.source AND record.session_id = usage.session_id
                    )
                   UNION ALL
                   SELECT input_tokens, output_tokens, cached_tokens FROM usage_remote_totals
                 ), feed_totals AS (
                   SELECT COALESCE(SUM(input_tokens), 0) AS input_tokens,
                          COALESCE(SUM(output_tokens), 0) AS output_tokens,
                          COALESCE(SUM(cached_tokens), 0) AS cached_tokens
                     FROM feed
                 )
                 SELECT
                   (SELECT COUNT(*) FROM agent_events),
                   (SELECT COUNT(*) FROM sessions),
                   (SELECT input_tokens FROM feed_totals),
                   (SELECT output_tokens FROM feed_totals),
                   (SELECT cached_tokens FROM feed_totals),
                   COUNT(usage.session_id),
                   COALESCE(SUM(CASE WHEN usage.origin LIKE 'estimated%' THEN 1 ELSE 0 END), 0),
                   (SELECT MAX(refreshed_at) FROM session_token_usage)
                 FROM sessions
                 LEFT JOIN usage ON usage.source = sessions.source AND usage.session_id = sessions.session_id",
                [],
                |row| {
                    let input: i64 = row.get(2)?;
                    let output: i64 = row.get(3)?;
                    let cached: i64 = row.get(4)?;
                    let usage_sessions: i64 = row.get(5)?;
                    let sessions: i64 = row.get(1)?;
                    let estimated_sessions: i64 = row.get(6)?;
                    Ok(TelemetrySummary {
                        events: row.get(0)?,
                        sessions,
                        input_tokens: input,
                        output_tokens: output,
                        cached_tokens: cached,
                        // Cache reads are a subset of the input for Codex and
                        // are shown separately. Adding them again inflated the
                        // old "total" counter.
                        total_tokens: input + output,
                        reported_events: usage_sessions - estimated_sessions,
                        estimated_events: estimated_sessions,
                        usage_sessions,
                        unavailable_usage_sessions: (sessions - usage_sessions).max(0),
                        usage_refreshed_at: row.get(7)?,
                    })
                },
            )
            .map_err(|e| e.to_string())
    }

    pub fn refresh_mcp_calls(&self) -> Result<McpCallRefresh, String> {
        const IMPORT_KEY: &str = "mcp_transcript_imported_v1";
        if self.app_setting_get::<bool>(IMPORT_KEY).unwrap_or(false) {
            return Ok(McpCallRefresh { imported: false, scanned_transcripts: 0, updated_transcripts: 0, observed_calls: 0,
                message: "历史 MCP 转录已导入；后续调用由连接观测记录".into() });
        }
        // Bound the one-time import before the first live relay event. This
        // prevents a previously observed call from appearing in both sources.
        let cutoff = {
            let conn = self.conn.lock().map_err(|_| "telemetry store lock poisoned".to_string())?;
            conn.query_row(
                "SELECT MIN(occurred_at) FROM mcp_call_observations WHERE origin = 'stdio_proxy'",
                [], |row| row.get::<_, Option<String>>(0),
            ).map_err(|e| e.to_string())?
                .unwrap_or_else(|| chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true))
        };
        let discovered = discover_all_native_transcripts();
        let mut scanned_transcripts = 0;
        let mut parsed = Vec::new();
        let mut coverage: HashMap<String, (i64, i64, String)> = HashMap::new();
        // 已验证转录 MCP 观测格式的来源：Claude Code 的
        // assistant/tool_use JSONL，DeepSeek Harness 的 tool/call 事件流。
        // 其他 Agent 可能用相同扩展名但不同的消息布局，不能据其建立空覆盖。
        let mcp_format_verified =
            |source: &str| matches!(source, "claude" | "dsh");
        for (source, session_id, path) in discovered {
            if !mcp_format_verified(&source) { continue; }
            let label = crate::agent_sources::agent_label(&source);
            let calls = match read_transcript_mcp_calls(&source, &session_id, &path) {
                Ok(calls) => calls,
                Err(_) => {
                    let entry = coverage.entry(source.clone()).or_insert((0, 0, String::new()));
                    entry.2 = format!("Some {label} transcripts could not be parsed or read");
                    continue;
                }
            };
            scanned_transcripts += 1;
            let calls: Vec<_> = calls.into_iter().filter(|call| call.occurred_at < cutoff).collect();
            let entry = coverage.entry(source.clone()).or_insert((0, 0, String::new()));
            entry.0 += 1;
            entry.1 += calls.len() as i64;
            if entry.2.is_empty() { entry.2 = format!("{label} tool_use/tool_result transcript scanned"); }
            parsed.extend(calls);
        }
        let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
        let mut conn = self.conn.lock().map_err(|_| "telemetry store lock poisoned".to_string())?;
        let tx = conn.transaction().map_err(|e| e.to_string())?;
        let already_imported: Option<String> = tx.query_row(
            "SELECT value_json FROM app_settings WHERE setting_key = ?1",
            [IMPORT_KEY], |row| row.get(0),
        ).optional().map_err(|e| e.to_string())?;
        if already_imported.as_deref() == Some("true") {
            return Ok(McpCallRefresh { imported: false, scanned_transcripts: 0, updated_transcripts: 0, observed_calls: 0,
                message: "历史 MCP 转录已导入；后续调用由连接观测记录".into() });
        }
        replace_transcript_mcp_calls(&tx, &parsed)?;
        tx.execute("DELETE FROM mcp_call_coverage", []).map_err(|e| e.to_string())?;
        for source in crate::agent_sources::AGENT_SOURCE_IDS {
            let (transcripts, calls, reason) = coverage.remove(source).unwrap_or_default();
            let verified = mcp_format_verified(source);
            let state = if !verified { "unsupported" }
                else if transcripts > 0 && reason.starts_with("Some ") && reason.contains("could not") { "partial" }
                else if transcripts > 0 { if calls > 0 { "observed" } else { "observed_empty" } }
                else { "unavailable" };
            let reason = if reason.is_empty() {
                if verified {
                    format!("No readable {} transcript was found", crate::agent_sources::agent_label(source))
                } else { "MCP call transcript format is not verified for this agent".to_string() }
            } else { reason };
            tx.execute("INSERT INTO mcp_call_coverage (source, state, transcript_count, call_count, scanned_at, reason) VALUES (?1, ?2, ?3, ?4, ?5, ?6)", params![source, state, transcripts, calls, now, reason]).map_err(|e| e.to_string())?;
        }
        tx.execute(
            "INSERT INTO app_settings (setting_key, value_json, updated_at) VALUES (?1, 'true', ?2)
             ON CONFLICT(setting_key) DO UPDATE SET value_json = excluded.value_json, updated_at = excluded.updated_at",
            params![IMPORT_KEY, now],
        ).map_err(|e| e.to_string())?;
        tx.commit().map_err(|e| e.to_string())?;
        Ok(McpCallRefresh { imported: true, scanned_transcripts, updated_transcripts: scanned_transcripts, observed_calls: parsed.len() as i64,
            message: format!("首次导入 {} 条历史 MCP 调用；后续由连接观测记录", parsed.len()) })
    }

    /// 用量估算口径版本。v2：Qoder 转录估算与账本估算改为「每请求全
    /// 上下文」口径（与 Claude/Codex 原生计数对齐）。v3：转录文本提取
    /// 补上思考块（`thinking` 键）与 tool_use 参数——此前输出被严重
    /// 低估。v4：Claude/WorkBuddy/MiniMax 读取器修复零 usage 占位行遮蔽
    /// 同 id 真实计数的去重缺陷，并把 Claude 子代理转录（`agent-*.jsonl`）
    /// 的用量归属回父会话。版本落后时把估算口径的存量行重置回未扫描
    /// 状态，让本次 refresh 全量重算；原生计数行不受影响。转录已被
    /// 宿主轮转走的会话保持原行——`has_durable_transcript_usage` 依旧
    /// 拦截降级估算，旧口径总比丢失或退回更粗的估计好。
    const USAGE_ESTIMATE_VERSION: i64 = 4;

    fn migrate_usage_estimate_version(&self) -> Result<(), String> {
        if self
            .app_setting_get::<i64>("usage_estimate_version")
            .unwrap_or(1)
            >= Self::USAGE_ESTIMATE_VERSION
        {
            return Ok(());
        }
        let conn = self
            .conn
            .lock()
            .map_err(|_| "telemetry store lock poisoned".to_string())?;
        conn.execute(
            "UPDATE session_token_usage SET refreshed_at = '1970-01-01T00:00:00.000Z'
              WHERE origin LIKE 'estimated%'
                 -- v4：Claude 原生行也要重扫，把此前被丢弃的子代理转录
                 -- （agent-*.jsonl）用量归属回父会话；转录已被轮转走的
                 -- 会话由 has_durable_transcript_usage 兜底，不会丢数据。
                 OR (source = 'claude' AND origin = 'native_transcript')",
            [],
        )
        .map_err(|e| e.to_string())?;
        drop(conn);
        self.app_setting_set("usage_estimate_version", &Self::USAGE_ESTIMATE_VERSION)
    }

    /// Reconcile token usage from native transcript files.  Hook callbacks are
    /// deliberately not summed: they are lifecycle notifications and can
    /// repeat cumulative values for the same turn.  Merge Hook hints with a
    /// lightweight metadata walk of every supported native transcript root.
    /// Content is read only for files that are new or whose mtime is newer than
    /// their durable SQLite usage projection.
    pub fn refresh_transcript_usage(&self) -> Result<TelemetryUsageRefresh, String> {
        self.migrate_usage_estimate_version()?;
        // Hook receipts are useful hints, but must never be the sole source of
        // truth: historical Codex sessions (especially archived ones) have no
        // Hook row to enqueue them.  Merge Hook hints with a lightweight
        // metadata walk of every supported native transcript root.  Content is
        // read only for files that are new or whose mtime is newer than their
        // durable SQLite usage projection.
        let mut candidates = self
            .usage_sessions_to_scan()?
            .into_iter()
            .map(|(source, session_id, path)| ((source, session_id), path))
            .collect::<HashMap<_, _>>();
        for (source, session_id, path) in discover_all_native_transcripts() {
            let key = (source.clone(), session_id.clone());
            match candidates.get_mut(&key) {
                Some(hinted_path) => {
                    if hinted_path
                        .as_ref()
                        .map(|value| !value.is_file())
                        .unwrap_or(true)
                    {
                        *hinted_path = Some(path);
                    }
                }
                None if self.transcript_changed_since_usage(&source, &session_id, &path) => {
                    candidates.insert(key, Some(path));
                }
                None => {}
            }
        }
        let sessions = candidates
            .into_iter()
            .map(|((source, session_id), path)| (source, session_id, path))
            .collect::<Vec<_>>();
        let scanned_sessions = sessions.len() as i64;
        if sessions.is_empty() {
            return Ok(TelemetryUsageRefresh {
                scanned_sessions: 0,
                updated_sessions: 0,
                unavailable_sessions: 0,
                message: "尚未捕获可统计的 Agent 会话".into(),
            });
        }

        let discovered = discover_native_transcripts(&sessions);
        let mut updates = Vec::new();
        let mut unavailable_sessions = 0;
        for (source, session_id, hinted_path) in sessions {
            let path = hinted_path.filter(|path| path.is_file()).or_else(|| {
                discovered
                    .get(&(source.clone(), session_id.clone()))
                    .cloned()
            });
            // The transcript was rotated away by its host Agent, but the
            // ledger already holds a transcript-derived projection.  Keep the
            // last known data: never overwrite it with a coarser estimate
            // and never wipe its per-response detail.
            if path.is_none() && self.has_durable_transcript_usage(&source, &session_id) {
                unavailable_sessions += 1;
                continue;
            }
            // CCSwitch's key insight is that display aggregates must be
            // built from idempotent request/response observations, not
            // from hook callbacks.  Session total and per-response detail
            // come from the SAME parse of the same file — the total is the
            // sum of the detail — so the two projections can never drift
            // apart when one of the two readers half-fails.
            let records = path
                .as_deref()
                .map(|path| read_transcript_usage_records(&source, &session_id, path))
                .unwrap_or_default();
            let (usage, origin) = if !records.is_empty() {
                (
                    summarize_usage_records(&records),
                    transcript_usage_origin(&source),
                )
            } else {
                match path
                    .as_deref()
                    .and_then(|path| match source.as_str() {
                        "codex" => read_codex_transcript_usage(path)
                            .map(|usage| (usage, "native_transcript")),
                        "claude" => read_claude_transcript_usage(path)
                            .map(|usage| (usage, "native_transcript")),
                        "workbuddy" => read_workbuddy_transcript_usage(path)
                            .map(|usage| (usage, "native_transcript")),
                        // Qoder currently persists dialogue but no provider usage
                        // fields. Keep its deterministic local estimate visible as an
                        // estimate instead of pretending it is billable usage.
                        "qoder" => read_qoder_transcript_estimate(path)
                            .map(|usage| (usage, "estimated_transcript")),
                        "minimax" => read_minimax_transcript_usage(path)
                            .map(|usage| (usage, "native_transcript")),
                        "kimi" => read_kimi_transcript_usage(path)
                            .map(|usage| (usage, "native_transcript")),
                        "dsh" => read_dsh_transcript_usage(path)
                            .map(|usage| (usage, "native_transcript")),
                        _ => None,
                    })
                    .or_else(|| {
                        self.estimate_usage_from_ledger(&source, &session_id)
                            .map(|usage| (usage, "estimated_ledger"))
                    })
                    .or_else(|| {
                        self.estimate_usage_from_event_payload(&source, &session_id)
                            .map(|usage| (usage, "estimated_event_payload"))
                    }) {
                    Some((usage, origin)) => (usage, origin),
                    None => {
                        unavailable_sessions += 1;
                        continue;
                    }
                }
            };
            updates.push((source.clone(), session_id.clone(), usage, origin, records));
        }

        if !updates.is_empty() {
            // Canonical millisecond UTC keeps lexical SQL comparisons aligned
            // with the ISO bounds the frontend sends.
            let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
            let pricing_providers =
                crate::provider_catalog::pricing_snapshot().unwrap_or_default();
            // 归一化备忘录：目录 alias 查询是全表扫描，同模型字符串必须复用。
            let mut normalize_memo: HashMap<String, (String, String)> = HashMap::new();
            let mut conn = self
                .conn
                .lock()
                .map_err(|_| "telemetry store lock poisoned".to_string())?;
            let transaction = conn.transaction().map_err(|e| e.to_string())?;
            for (source, session_id, usage, origin, records) in &updates {
                Self::upsert_session_usage_total(
                    &transaction, source, session_id, usage, origin, &now,
                )?;
                transaction
                    .execute(
                        "DELETE FROM usage_records WHERE source = ?1 AND session_id = ?2",
                        params![source, session_id],
                    )
                    .map_err(|e| e.to_string())?;
                Self::write_priced_usage_records(
                    &transaction,
                    &pricing_providers,
                    &now,
                    source,
                    session_id,
                    origin,
                    records,
                    &mut normalize_memo,
                )?;
            }
            transaction.commit().map_err(|e| e.to_string())?;
        }

        Ok(TelemetryUsageRefresh {
            scanned_sessions,
            updated_sessions: updates.len() as i64,
            unavailable_sessions,
            message: format!(
                "已写入 {} 个会话的 Token 用量（原始转录优先，缺失时自动估算）",
                updates.len()
            ),
        })
    }

    /// 会话总量投影 upsert（usage 投影的第一层；该会话无逐条明细时看板的
    /// 兜底行）。原 refresh_transcript_usage 内联事务体之一，提取为独立
    /// 方法供转录扫描与 dsh 插件上报共用同一写入口径。
    fn upsert_session_usage_total(
        transaction: &Transaction<'_>,
        source: &str,
        session_id: &str,
        usage: &TranscriptUsage,
        origin: &str,
        now: &str,
    ) -> Result<(), String> {
        transaction
            .execute(
                "INSERT INTO session_token_usage (source, session_id, input_tokens, output_tokens, cached_tokens, origin, refreshed_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
                 ON CONFLICT(source, session_id) DO UPDATE SET
                   input_tokens=excluded.input_tokens, output_tokens=excluded.output_tokens,
                   cached_tokens=excluded.cached_tokens, origin=excluded.origin, refreshed_at=excluded.refreshed_at",
                params![source, session_id, usage.input_tokens, usage.output_tokens, usage.cached_tokens, origin, now],
            )
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    /// usage_records 明细行的定价与写入内核（原 refresh_transcript_usage 的
    /// 内联事务体）。occurred_at 缺失时以 now 兜底；定价先经
    /// pricing::normalize_model 归一（normalize_memo 跨行复用，避免同模型
    /// 字符串重复做全表 alias 扫描），再经 pricing::price_usage 计算成本
    /// 快照（pricing_source 记录价格来源，支持后续追溯重算）。幂等性由
    /// record_id + INSERT OR REPLACE 保证，调用方负责让同一逻辑观测在
    /// 不同上报路径下命中同一 record_id。
    fn write_priced_usage_records(
        transaction: &Transaction<'_>,
        pricing_providers: &[crate::provider_catalog::ProviderConfig],
        now: &str,
        source: &str,
        session_id: &str,
        origin: &str,
        records: &[TranscriptUsageRecord],
        normalize_memo: &mut HashMap<String, (String, String)>,
    ) -> Result<(), String> {
        for record in records {
            let occurred_at = normalize_occurred_at(record.occurred_at.as_deref())
                .unwrap_or_else(|| now.to_string());
            let memo_key = format!("{source}|{}", record.model.as_deref().unwrap_or(""));
            let (normalized_provider, normalized_model) = normalize_memo
                .entry(memo_key)
                .or_insert_with(|| {
                    let normalized = crate::pricing::normalize_model(
                        transaction,
                        record.model.as_deref(),
                        source,
                    );
                    (normalized.provider_id, normalized.model_id)
                })
                .clone();
            let gateway = pricing_providers
                .iter()
                .find(|provider| provider.id == normalized_provider)
                .filter(|provider| !provider.base_url.is_empty())
                .map(|provider| {
                    provider
                        .base_url
                        .trim_start_matches("https://")
                        .trim_start_matches("http://")
                        .split('/')
                        .next()
                        .unwrap_or_default()
                        .to_string()
                })
                .filter(|host| !host.is_empty());
            let priced = crate::pricing::price_usage(
                transaction,
                pricing_providers,
                source,
                &normalized_provider,
                &normalized_model,
                record.input_tokens,
                record.output_tokens,
                record.cached_tokens,
            );
            transaction
                .execute(
                    "INSERT OR REPLACE INTO usage_records
                     (record_id, source, session_id, occurred_at, model, record_kind, input_tokens, output_tokens, cached_tokens, origin,
                      provider_id, gateway, route, cost_usd, pricing_source)
                     VALUES (?1, ?2, ?3, ?4, ?5, 'response', ?6, ?7, ?8, ?9, ?10, ?11, NULL, ?12, ?13)",
                    params![record.record_id, source, session_id, occurred_at, record.model,
                        record.input_tokens, record.output_tokens, record.cached_tokens, origin,
                        normalized_provider, gateway, priced.cost_usd, priced.pricing_source],
                )
                .map_err(|e| e.to_string())?;
        }
        Ok(())
    }

    /// dsh bridge 插件逐条 usage 上报（`POST /telemetry/usage?source=dsh`）
    /// 的幂等入库。与转录扫描口径（read_dsh_usage_records /
    /// refresh_transcript_usage）共用 record_id 命名空间：插件把
    /// assistant/message 的事件 seq 放进 message_id 时，同一消息无论先经
    /// 哪条路径入账都命中同一 record_id，INSERT OR REPLACE 原位覆盖、不
    /// 双计；转录扫描的 DELETE+重插则整体接管该会话明细（插件先报、扫描
    /// 后到时被替换）。只写 usage_records（origin=dsh_plugin），不动
    /// session_token_usage——看板聚合是 detail-first，会话总量行仅在无明
    /// 细时兜底，插件逐条数据天然成为该会话的权威口径。
    ///
    /// 载荷：`{session_id, model?, input_tokens, output_tokens, cached_tokens,
    /// occurred_at?, message_id?}`。session_id 接受裸 uuid 或 `session-<uuid>`
    /// 目录名，统一归一成扫描器口径；cached 按「缓存命中 ⊆ 输入」钳制。
    /// 校验失败返回可读 Err（HTTP 层映射 400）。成功返回 record_id。
    pub fn record_dsh_plugin_usage(&self, payload: &Value) -> Result<String, String> {
        let raw_session = payload
            .get("session_id")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or("session_id 不能为空")?;
        let session_id = Self::canonical_dsh_session_id(raw_session)?;
        // 插件转发 DeepSeek 原始字段：input_tokens 是未命中缓存的输入，
        // cached_tokens 是缓存命中。账本口径与 read_dsh_usage_records 对齐
        // （input 列 = 输入合计、含缓存命中；cached 列标记折价部分）——否则
        // 同一 record_id 被两条路径先后覆盖时，缓存命中部分会漏计。
        let raw_input = Self::usage_counter(payload, "input_tokens")?;
        let output_tokens = Self::usage_counter(payload, "output_tokens")?;
        let cached_tokens = Self::usage_counter(payload, "cached_tokens")?;
        let input_tokens = raw_input + cached_tokens;
        let cached_tokens = cached_tokens.min(input_tokens);
        if input_tokens <= 0 && output_tokens <= 0 {
            return Err("input_tokens 与 output_tokens 不能同时为零".into());
        }
        let model = payload
            .get("model")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(|value| value.chars().take(128).collect::<String>());
        let occurred_at = match payload.get("occurred_at").and_then(Value::as_str) {
            Some(raw) if !raw.trim().is_empty() => Some(
                normalize_occurred_at(Some(raw)).ok_or("occurred_at 不是合法的 RFC3339 时间")?,
            ),
            _ => None,
        };
        let message_id = payload
            .get("message_id")
            .and_then(Value::as_str)
            .map(|raw| {
                raw.chars()
                    .filter(|character| !character.is_control())
                    .collect::<String>()
                    .trim()
                    .to_string()
            })
            .filter(|value| !value.is_empty())
            .map(|value| value.chars().take(128).collect::<String>());
        // identity：message_id（插件传事件 seq）与 read_dsh_usage_records 的
        // seq identity 完全一致，是跨路径不双计的关键；缺失时退回
        // 「模型+计数+时间」稳定组合，同载荷重试仍幂等。
        let identity = match &message_id {
            Some(message_id) => message_id.clone(),
            None => format!(
                "{}|{input_tokens}|{output_tokens}|{cached_tokens}|{}",
                model.as_deref().unwrap_or(""),
                occurred_at.as_deref().unwrap_or(""),
            ),
        };
        let record_id = usage_record_id("dsh", &session_id, "message", &identity);
        let record = TranscriptUsageRecord {
            record_id: record_id.clone(),
            occurred_at,
            model,
            input_tokens,
            output_tokens,
            cached_tokens,
        };
        let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
        let pricing_providers = crate::provider_catalog::pricing_snapshot().unwrap_or_default();
        let mut conn = self
            .conn
            .lock()
            .map_err(|_| "telemetry store lock poisoned".to_string())?;
        let transaction = conn.transaction().map_err(|e| e.to_string())?;
        let mut normalize_memo = HashMap::new();
        Self::write_priced_usage_records(
            &transaction,
            &pricing_providers,
            &now,
            "dsh",
            &session_id,
            "dsh_plugin",
            std::slice::from_ref(&record),
            &mut normalize_memo,
        )?;
        transaction.commit().map_err(|e| e.to_string())?;
        Ok(record_id)
    }

    /// dsh 插件上报的 session_id 归一化：接受裸 uuid 或目录名
    /// `session-<uuid>`，统一为转录扫描口径（见 agent_sources::
    /// session_id_for_path 的 dsh 分支），保证 usage_record_id 与
    /// refresh_transcript_usage 同命名空间。仅认带连字符的规范 uuid 形态。
    fn canonical_dsh_session_id(raw: &str) -> Result<String, String> {
        let bare = raw.strip_prefix("session-").unwrap_or(raw);
        if bare.len() != 36 || uuid::Uuid::parse_str(bare).is_err() {
            return Err("session_id 必须是 dsh 会话 uuid 或 session-<uuid> 目录名".into());
        }
        Ok(format!("session-{bare}"))
    }

    /// 非负整数 usage 计数字段（JSON u64 → i64 安全收窄）。
    fn usage_counter(payload: &Value, key: &str) -> Result<i64, String> {
        let value = payload
            .get(key)
            .and_then(Value::as_u64)
            .ok_or_else(|| format!("{key} 缺失或不是非负整数"))?;
        i64::try_from(value).map_err(|_| format!("{key} 超出可记录范围"))
    }

    fn usage_sessions_to_scan(&self) -> Result<Vec<(String, String, Option<PathBuf>)>, String> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| "telemetry store lock poisoned".to_string())?;
        let mut statement = conn.prepare(
            "SELECT event.source, event.session_id, event.payload_json FROM agent_events event
               LEFT JOIN session_token_usage usage
                 ON usage.source = event.source AND usage.session_id = event.session_id
              WHERE event.source IN ('codex', 'claude', 'qoder', 'workbuddy', 'minimax', 'kimi', 'copilot') AND event.session_id != 'unknown'
                -- Historical sessions are already durable.  Revisit only a
                -- new Hook receipt (or a session with no previous result).
                AND (usage.refreshed_at IS NULL OR event.occurred_at > usage.refreshed_at)
              ORDER BY event.id DESC LIMIT 1000",
        ).map_err(|e| e.to_string())?;
        let rows = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })
            .map_err(|e| e.to_string())?;
        let mut sessions: HashMap<(String, String), Option<PathBuf>> = HashMap::new();
        for row in rows {
            let (source, session_id, raw_payload) = row.map_err(|e| e.to_string())?;
            let hinted_path =
                serde_json::from_str::<Value>(&raw_payload)
                    .ok()
                    .and_then(|payload| {
                        payload
                            .get("transcript_path")
                            .and_then(Value::as_str)
                            .map(PathBuf::from)
                    });
            sessions
                .entry((source, session_id))
                .and_modify(|current| {
                    if current.is_none() && hinted_path.is_some() {
                        *current = hinted_path.clone();
                    }
                })
                .or_insert(hinted_path);
        }
        Ok(sessions
            .into_iter()
            .map(|((source, session_id), path)| (source, session_id, path))
            .collect())
    }

    fn transcript_changed_since_usage(&self, source: &str, session_id: &str, path: &Path) -> bool {
        let modified = match std::fs::metadata(path).and_then(|metadata| metadata.modified()) {
            Ok(value) => chrono::DateTime::<chrono::Utc>::from(value),
            // A vanished transcript is not a change.  Report false so the
            // session is not rescanned (and its durable projection not
            // overwritten) on every single boot.
            Err(_) => return false,
        };
        let refreshed: Option<String> = self.conn.lock().ok().and_then(|conn| {
            conn.query_row(
            "SELECT refreshed_at FROM session_token_usage WHERE source = ?1 AND session_id = ?2",
            params![source, session_id], |row| row.get(0),
        ).optional().ok().flatten()
        });
        refreshed
            .and_then(|value| chrono::DateTime::parse_from_rfc3339(&value).ok())
            .map(|value| modified > value.with_timezone(&chrono::Utc))
            .unwrap_or(true)
    }

    /// True when the ledger already holds a transcript-derived projection
    /// for this session (native counters or a transcript estimate).  Such
    /// rows outlive the transcript file; coarser estimates must never
    /// overwrite them once the file is rotated away by its host Agent.
    fn has_durable_transcript_usage(&self, source: &str, session_id: &str) -> bool {
        self.conn
            .lock()
            .ok()
            .and_then(|conn| {
                conn.query_row(
                    "SELECT COUNT(*) FROM session_token_usage
                      WHERE source = ?1 AND session_id = ?2
                        AND origin IN ('native_transcript', 'estimated_transcript')",
                    params![source, session_id],
                    |row| row.get::<_, i64>(0),
                )
                .ok()
            })
            .map(|count| count > 0)
            .unwrap_or(false)
    }

    /// Last-resort local estimate when an Agent only supplied Hook receipts.
    /// It consumes the sanitized conversation ledger, never tool payloads, and
    /// therefore gives every captured conversation a stable usage entry.
    fn estimate_usage_from_ledger(
        &self,
        source: &str,
        session_id: &str,
    ) -> Option<TranscriptUsage> {
        let conn = self.conn.lock().ok()?;
        let conversation = conn.query_row(
            "SELECT conversation_text FROM agent_events
              WHERE source = ?1 AND session_id = ?2 AND conversation_text IS NOT NULL AND conversation_text != ''
              ORDER BY id DESC LIMIT 1",
            params![source, session_id],
            |row| row.get::<_, String>(0),
        ).optional().ok().flatten()?;
        estimate_ledger_conversation_usage(&conversation)
    }

    /// Final local fallback when an adapter only emitted lifecycle receipts.
    /// It is intentionally persisted as an estimate rather than leaving a
    /// visible zero/unknown session in the usage dashboard.
    fn estimate_usage_from_event_payload(
        &self,
        source: &str,
        session_id: &str,
    ) -> Option<TranscriptUsage> {
        let conn = self.conn.lock().ok()?;
        let payload: String = conn.query_row(
            "SELECT payload_json FROM agent_events WHERE source = ?1 AND session_id = ?2 ORDER BY id DESC LIMIT 1",
            params![source, session_id], |row| row.get(0),
        ).optional().ok().flatten()?;
        let input_tokens = estimate_transcript_tokens(&payload).max(1);
        Some(TranscriptUsage {
            input_tokens,
            output_tokens: 0,
            cached_tokens: 0,
        })
    }

    fn live_status(&self) -> Result<TelemetryLiveStatus, String> {
        let recent_cutoff = (chrono::Utc::now() - chrono::Duration::minutes(5)).to_rfc3339();
        let now = chrono::Utc::now().to_rfc3339();
        // Same latest-event window as the `active_sessions` counter below, so
        // the reported sources always match the reported session count.
        const ACTIVE_SESSION_FILTER: &str = "(
                  SELECT source, session_id, event_type, occurred_at,
                         ROW_NUMBER() OVER (PARTITION BY source, session_id ORDER BY id DESC) AS rank
                    FROM agent_events WHERE session_id != 'unknown'
                ) latest WHERE rank = 1
                    AND occurred_at >= ?1 AND occurred_at <= ?2
                    AND event_type NOT IN ('Stop', 'TranscriptSync', 'session_end', 'session_usage')";
        let conn = self
            .conn
            .lock()
            .map_err(|_| "telemetry store lock poisoned".to_string())?;
        let mut status = conn.query_row(
            &format!(
                "WITH sessions AS (
                     SELECT DISTINCT source, session_id FROM agent_events WHERE session_id != 'unknown'
                     UNION
                     SELECT source, session_id FROM session_token_usage
                 )
                 SELECT
                   (SELECT COUNT(*) FROM sessions),
                   (SELECT COUNT(*) FROM {ACTIVE_SESSION_FILTER}),
                   (SELECT COUNT(*) FROM agent_events WHERE conversation_state = 'full'),
                   (SELECT COUNT(*) FROM agent_events WHERE conversation_state = 'full' AND l1_state = 'stored' AND conversation_text IS NOT NULL AND conversation_text != ''),
                   (SELECT COUNT(*) FROM agent_events WHERE conversation_state = 'full' AND l1_state = 'pending' AND conversation_text IS NOT NULL AND conversation_text != ''),
                   (SELECT COUNT(*) FROM agent_events WHERE conversation_state = 'full' AND l1_state = 'retrying' AND conversation_text IS NOT NULL AND conversation_text != ''),
                   (SELECT COUNT(*) FROM agent_events WHERE conversation_state = 'full' AND l1_state = 'failed' AND conversation_text IS NOT NULL AND conversation_text != ''),
                   (SELECT COUNT(*) FROM sessions LEFT JOIN session_token_usage usage ON usage.source=sessions.source AND usage.session_id=sessions.session_id WHERE usage.session_id IS NULL),
                   (SELECT COUNT(*) FROM transcript_scan_states WHERE state = 'failed'),
                   (SELECT MAX(occurred_at) FROM agent_events)"
            ),
            [&recent_cutoff, &now],
            |row| Ok(TelemetryLiveStatus {
                captured_sessions: row.get(0)?, active_sessions: row.get(1)?, active_sources: Vec::new(),
                completed_conversations: row.get(2)?,
                organized_memory_conversations: row.get(3)?, pending_memory_sessions: row.get(4)?,
                retrying_memory_sessions: row.get(5)?, failed_memory_sessions: row.get(6)?, pending_usage_sessions: row.get(7)?,
                failed_transcript_scans: row.get(8)?, last_event_at: row.get(9)?,
            }),
        ).map_err(|e| e.to_string())?;
        if status.active_sessions > 0 {
            let mut statement = conn
                .prepare(&format!(
                    "SELECT DISTINCT source FROM {ACTIVE_SESSION_FILTER} ORDER BY source"
                ))
                .map_err(|e| e.to_string())?;
            status.active_sources = statement
                .query_map([&recent_cutoff, &now], |row| row.get::<_, String>(0))
                .map_err(|e| e.to_string())?
                .filter_map(|source| source.ok())
                .collect();
        }
        Ok(status)
    }

    fn recent_events(&self, limit: u32) -> Result<Vec<TelemetryEvent>, String> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| "telemetry store lock poisoned".to_string())?;
        let mut statement = conn
            .prepare(
                "SELECT id, source, session_id, event_type, occurred_at, token_source, input_tokens, output_tokens, cached_tokens,
                        conversation_state, conversation_message_count, conversation_text, l1_state
                   FROM agent_events ORDER BY id DESC LIMIT ?1",
            )
            .map_err(|e| e.to_string())?;
        let rows = statement
            .query_map([limit.min(200)], |row| {
                Ok(TelemetryEvent {
                    id: row.get(0)?,
                    source: row.get(1)?,
                    session_id: row.get(2)?,
                    event_type: row.get(3)?,
                    occurred_at: row.get(4)?,
                    token_source: row.get(5)?,
                    input_tokens: row.get(6)?,
                    output_tokens: row.get(7)?,
                    cached_tokens: row.get(8)?,
                    conversation_state: row.get(9)?,
                    conversation_message_count: row.get(10)?,
                    conversation_text: row.get(11)?,
                    l1_state: row.get(12)?,
                })
            })
            .map_err(|e| e.to_string())?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())
    }

    /// Usage dashboard feed. Native response rows are preferred; sessions
    /// without request-level provider counters are exposed as one explicit
    /// session-total fallback instead of zero-valued Hook rows.
    fn usage_records(&self, limit: u32) -> Result<Vec<TelemetryUsageRecord>, String> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| "telemetry store lock poisoned".to_string())?;
        let mut statement = conn.prepare(
            "SELECT record_id, source, session_id, occurred_at, model, record_kind,
                    input_tokens, output_tokens, cached_tokens, origin
               FROM (
                 SELECT record_id, source, session_id, occurred_at, model, record_kind,
                        input_tokens, output_tokens, cached_tokens, origin
                   FROM usage_records
                 UNION ALL
                 SELECT 'session-total:' || usage.source || ':' || usage.session_id,
                        usage.source, usage.session_id,
                        strftime('%Y-%m-%dT%H:%M:%fZ', COALESCE((SELECT MAX(event.occurred_at) FROM agent_events event
                                   WHERE event.source = usage.source AND event.session_id = usage.session_id), usage.refreshed_at)),
                        NULL, 'session_total', usage.input_tokens, usage.output_tokens,
                        usage.cached_tokens, usage.origin
                   FROM session_token_usage usage
                  WHERE NOT EXISTS (
                    SELECT 1 FROM usage_records record
                     WHERE record.source = usage.source AND record.session_id = usage.session_id
                  )
               )
              ORDER BY occurred_at DESC LIMIT ?1",
        ).map_err(|e| e.to_string())?;
        let rows = statement
            .query_map([limit.clamp(1, 1000)], |row| {
                Ok(TelemetryUsageRecord {
                    record_id: row.get(0)?,
                    source: row.get(1)?,
                    session_id: row.get(2)?,
                    occurred_at: row.get(3)?,
                    model: row.get(4)?,
                    record_kind: row.get(5)?,
                    input_tokens: row.get(6)?,
                    output_tokens: row.get(7)?,
                    cached_tokens: row.get(8)?,
                    origin: row.get(9)?,
                })
            })
            .map_err(|e| e.to_string())?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())
    }

    fn usage_analytics(
        &self,
        start_at: Option<&str>,
        end_at: Option<&str>,
        source: Option<&str>,
        bucket: &str,
        record_limit: u32,
    ) -> Result<TelemetryUsageAnalytics, String> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| "telemetry store lock poisoned".to_string())?;
        // Request-level records are primary. A session-total fallback is only
        // added when that session has no request records, so totals cannot be
        // inflated by counting both representations.
        const FEED: &str = "
            WITH usage_feed AS (
              SELECT record_id, source, session_id, occurred_at, model, record_kind,
                     input_tokens, output_tokens, cached_tokens, origin
                FROM usage_records
              UNION ALL
              SELECT 'session-total:' || usage.source || ':' || usage.session_id,
                     usage.source, usage.session_id,
                     strftime('%Y-%m-%dT%H:%M:%fZ', COALESCE((SELECT MAX(event.occurred_at) FROM agent_events event
                               WHERE event.source = usage.source AND event.session_id = usage.session_id), usage.refreshed_at)),
                     NULL, 'session_total', usage.input_tokens, usage.output_tokens,
                     usage.cached_tokens, usage.origin
                FROM session_token_usage usage
               WHERE NOT EXISTS (
                 SELECT 1 FROM usage_records record
                  WHERE record.source = usage.source AND record.session_id = usage.session_id
               )
              UNION ALL
              -- 其他设备经云端记忆库同步来的总量统计（无逐条明细）。
              SELECT 'remote:' || device || ':' || day || ':' || source,
                     source, device, day || 'T12:00:00',
                     NULL, 'remote_total', input_tokens, output_tokens, cached_tokens, 'remote'
                FROM usage_remote_totals
            ) ";
        const FILTER: &str = " WHERE (?1 IS NULL OR occurred_at >= ?1)
                                   AND (?2 IS NULL OR occurred_at < ?2)
                                   AND (?3 IS NULL OR source = ?3) ";
        let totals_sql = format!("{FEED} SELECT COUNT(*), COALESCE(SUM(input_tokens), 0), COALESCE(SUM(output_tokens), 0), COALESCE(SUM(cached_tokens), 0), COALESCE(SUM(CASE WHEN origin LIKE 'estimated%' THEN input_tokens + output_tokens ELSE 0 END), 0) FROM usage_feed {FILTER}");
        let (record_count, input_tokens, output_tokens, cached_tokens, estimated_tokens): (
            i64,
            i64,
            i64,
            i64,
            i64,
        ) = conn
            .query_row(&totals_sql, params![start_at, end_at, source], |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            })
            .map_err(|e| e.to_string())?;

        // 缓存命中率只对携带提供商计数（原生转录/适配器上报）的行有意义；
        // 估算与远端总量行没有缓存字段，混合展示会把命中率压成误导性低值。
        let capable_sql = format!(
            "{FEED} SELECT EXISTS(SELECT 1 FROM usage_feed {FILTER} \
              AND (origin LIKE 'native%' OR origin LIKE 'adapter%'))"
        );
        let cache_capable: bool = conn
            .query_row(&capable_sql, params![start_at, end_at, source], |row| {
                row.get(0)
            })
            .map_err(|e| e.to_string())?;

        let cap = record_limit.clamp(50, 1_000);
        let records_sql = format!("{FEED} SELECT record_id, source, session_id, occurred_at, model, record_kind, input_tokens, output_tokens, cached_tokens, origin FROM usage_feed {FILTER} ORDER BY occurred_at DESC LIMIT ?4");
        let mut statement = conn.prepare(&records_sql).map_err(|e| e.to_string())?;
        let records = statement
            .query_map(params![start_at, end_at, source, cap], |row| {
                Ok(TelemetryUsageRecord {
                    record_id: row.get(0)?,
                    source: row.get(1)?,
                    session_id: row.get(2)?,
                    occurred_at: row.get(3)?,
                    model: row.get(4)?,
                    record_kind: row.get(5)?,
                    input_tokens: row.get(6)?,
                    output_tokens: row.get(7)?,
                    cached_tokens: row.get(8)?,
                    origin: row.get(9)?,
                })
            })
            .map_err(|e| e.to_string())?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())?;

        // The group expression is chosen from a closed enum, never from UI
        // text, so it is safe to interpolate into the SQL statement.
        let group = if bucket == "hour" {
            "strftime('%Y-%m-%d %H:00', occurred_at, 'localtime')"
        } else {
            "strftime('%Y-%m-%d', occurred_at, 'localtime')"
        };
        let buckets_sql = format!("{FEED} SELECT {group}, COALESCE(SUM(input_tokens), 0), COALESCE(SUM(output_tokens), 0), COALESCE(SUM(cached_tokens), 0), COUNT(*) FROM usage_feed {FILTER} GROUP BY 1 ORDER BY 1");
        let mut bucket_statement = conn.prepare(&buckets_sql).map_err(|e| e.to_string())?;
        let buckets = bucket_statement
            .query_map(params![start_at, end_at, source], |row| {
                Ok(TelemetryUsageBucket {
                    label: row.get(0)?,
                    input_tokens: row.get(1)?,
                    output_tokens: row.get(2)?,
                    cached_tokens: row.get(3)?,
                    record_count: row.get(4)?,
                })
            })
            .map_err(|e| e.to_string())?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())?;

        let heatmap_sql = format!(
            "{FEED} SELECT CAST(strftime('%w', occurred_at, 'localtime') AS INTEGER), \
              CAST(strftime('%H', occurred_at, 'localtime') AS INTEGER), \
              COALESCE(SUM(input_tokens), 0), COALESCE(SUM(output_tokens), 0), COUNT(*) \
              FROM usage_feed {FILTER} GROUP BY 1, 2 ORDER BY 1, 2"
        );
        let mut heatmap_statement = conn.prepare(&heatmap_sql).map_err(|e| e.to_string())?;
        let heatmap = heatmap_statement
            .query_map(params![start_at, end_at, source], |row| {
                Ok(TelemetryUsageHeatCell {
                    weekday: row.get(0)?,
                    hour: row.get(1)?,
                    input_tokens: row.get(2)?,
                    output_tokens: row.get(3)?,
                    record_count: row.get(4)?,
                })
            })
            .map_err(|e| e.to_string())?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())?;

        let cost_groups_sql = format!(
            "{FEED} SELECT source, COALESCE(NULLIF(TRIM(model), ''), ''), \
              COALESCE(SUM(input_tokens), 0), COALESCE(SUM(output_tokens), 0), \
              COALESCE(SUM(cached_tokens), 0), \
              COALESCE(SUM(CASE WHEN origin LIKE 'estimated%' THEN input_tokens + output_tokens ELSE 0 END), 0) \
              FROM usage_feed {FILTER} GROUP BY 1, 2 ORDER BY 1, 2"
        );
        let mut cost_groups_statement = conn
            .prepare(&cost_groups_sql)
            .map_err(|e| e.to_string())?;
        let cost_groups = cost_groups_statement
            .query_map(params![start_at, end_at, source], |row| {
                let model: String = row.get(1)?;
                Ok(TelemetryUsageCostGroup {
                    source: row.get(0)?,
                    model: (!model.is_empty()).then_some(model),
                    input_tokens: row.get(2)?,
                    output_tokens: row.get(3)?,
                    cached_tokens: row.get(4)?,
                    estimated_tokens: row.get(5)?,
                })
            })
            .map_err(|e| e.to_string())?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())?;

        let sources_sql = format!("{FEED} SELECT DISTINCT source FROM usage_feed ORDER BY source");
        let mut sources_statement = conn.prepare(&sources_sql).map_err(|e| e.to_string())?;
        let sources = sources_statement
            .query_map([], |row| row.get(0))
            .map_err(|e| e.to_string())?
            .collect::<Result<Vec<String>, _>>()
            .map_err(|e| e.to_string())?;
        Ok(TelemetryUsageAnalytics {
            record_count,
            input_tokens,
            output_tokens,
            cached_tokens,
            estimated_tokens,
            cache_capable,
            truncated_records: record_count > records.len() as i64,
            records,
            buckets,
            heatmap,
            cost_groups,
            sources,
        })
    }

    /// Reprice pending rows during routine refreshes. An explicit forced run
    /// visits every row once, including rows with an existing price.
    pub fn recalculate_costs(&self, force: bool) -> Result<usize, String> {
        let providers = crate::provider_catalog::pricing_snapshot().unwrap_or_default();
        let mut processed = 0usize;
        let mut memo: HashMap<String, (String, String)> = HashMap::new();
        let batch = 500usize;
        let mut lock_retries = 0u32;
        let mut last_record_id = String::new();
        loop {
            let attempt = self.recalculate_costs_batch(
                &providers, &mut memo, force, batch, &last_record_id,
            );
            match attempt {
                Ok(Some((count, next_record_id))) => {
                    processed += count;
                    last_record_id = next_record_id;
                    lock_retries = 0;
                    // Let reads between write batches acquire the shared connection.
                    std::thread::yield_now();
                }
                Ok(None) => break,
                Err(error) if error.contains("database is locked") && lock_retries < 5 => {
                    lock_retries += 1;
                    std::thread::sleep(std::time::Duration::from_millis(1500));
                    continue;
                }
                Err(error) => return Err(error),
            }
        }
        Ok(processed)
    }

    /// Called after a successful catalog update so the next bounded background
    /// pass picks up existing automatically priced rows as well as new ones.
    pub fn invalidate_catalog_prices(&self) -> Result<usize, String> {
        let conn = self.conn.lock().map_err(|_| "telemetry store lock poisoned".to_string())?;
        conn.execute(
            "UPDATE usage_records SET pricing_source = NULL
              WHERE pricing_source IS NOT NULL AND pricing_source != 'user_override'",
            [],
        ).map_err(|e| e.to_string())
    }

    pub fn invalidate_provider_prices(&self, provider_id: &str) -> Result<usize, String> {
        let conn = self.conn.lock().map_err(|_| "telemetry store lock poisoned".to_string())?;
        conn.execute(
            "UPDATE usage_records SET pricing_source = NULL
              WHERE provider_id = ?1 AND pricing_source IS NOT NULL",
            params![provider_id],
        ).map_err(|e| e.to_string())
    }

    /// 单批重算：返回本批处理行数；0 表示没有剩余工作。
    fn recalculate_costs_batch(
        &self,
        providers: &[crate::provider_catalog::ProviderConfig],
        memo: &mut HashMap<String, (String, String)>,
        force: bool,
        batch: usize,
        after_record_id: &str,
    ) -> Result<Option<(usize, String)>, String> {
            let mut conn = self
                .conn
                .lock()
                .map_err(|_| "telemetry store lock poisoned".to_string())?;
            let transaction = conn
                .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
                .map_err(|e| e.to_string())?;
            let mut statement = transaction
                .prepare(
                    "SELECT record_id, source, model, input_tokens, output_tokens, cached_tokens
                       FROM usage_records
                      WHERE record_id > ?1 AND (pricing_source IS NULL OR ?2)
                      ORDER BY record_id LIMIT ?3",
                )
                .map_err(|e| e.to_string())?;
            let rows = statement
                .query_map(params![after_record_id, force, batch], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, Option<String>>(2)?,
                        row.get::<_, i64>(3)?,
                        row.get::<_, i64>(4)?,
                        row.get::<_, i64>(5)?,
                    ))
                })
                .map_err(|e| e.to_string())?
                .collect::<Result<Vec<_>, _>>()
                .map_err(|e| e.to_string())?;
            drop(statement);
            if rows.is_empty() {
                return Ok(None);
            }
            let last_record_id = rows.last().map(|row| row.0.clone()).unwrap();
            for (record_id, source, model, input, output, cached) in &rows {
                let memo_key = format!("{source}|{}", model.as_deref().unwrap_or(""));
                let (provider_id, model_id) = memo
                    .entry(memo_key)
                    .or_insert_with(|| {
                        let normalized =
                            crate::pricing::normalize_model(&transaction, model.as_deref(), source);
                        (normalized.provider_id, normalized.model_id)
                    })
                    .clone();
                let priced = crate::pricing::price_usage(
                    &transaction,
                    &providers,
                    source,
                    &provider_id,
                    &model_id,
                    *input,
                    *output,
                    *cached,
                );
                transaction
                    .execute(
                        "UPDATE usage_records
                            SET provider_id = ?2, cost_usd = ?3, pricing_source = ?4
                          WHERE record_id = ?1",
                        params![record_id, provider_id, priced.cost_usd, priced.pricing_source],
                    )
                    .map_err(|e| e.to_string())?;
            }
            transaction.commit().map_err(|e| e.to_string())?;
            Ok(Some((rows.len(), last_record_id)))
    }

    /// PRD 一期：按供应商聚合用量与成本（二期 UI 的数据源）。
    pub fn provider_usage_summary(
        &self,
        start_at: Option<&str>,
        end_at: Option<&str>,
    ) -> Result<Vec<crate::provider_catalog::ProviderUsageSummary>, String> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| "telemetry store lock poisoned".to_string())?;
        let mut statement = conn
            .prepare(
                "SELECT provider_id, gateway, model,
                        COALESCE(SUM(input_tokens), 0), COALESCE(SUM(output_tokens), 0),
                        COALESCE(SUM(cached_tokens), 0), COALESCE(SUM(cost_usd), 0.0),
                        COALESCE(MAX(pricing_source), ''), COUNT(*)
                   FROM usage_records
                  WHERE (?1 IS NULL OR occurred_at >= ?1)
                    AND (?2 IS NULL OR occurred_at < ?2)
                  GROUP BY 1, 2, 3 ORDER BY 7 DESC",
            )
            .map_err(|e| e.to_string())?;
        let rows = statement
            .query_map(params![start_at, end_at], |row| {
                let provider: Option<String> = row.get(0)?;
                let model: Option<String> = row.get(2)?;
                let pricing_source: String = row.get(7)?;
                Ok(crate::provider_catalog::ProviderUsageSummary {
                    provider_id: provider,
                    gateway: row.get(1)?,
                    model,
                    input_tokens: row.get(3)?,
                    output_tokens: row.get(4)?,
                    cached_tokens: row.get(5)?,
                    cost_usd: row.get(6)?,
                    pricing_source: (!pricing_source.is_empty()).then_some(pricing_source),
                    record_count: row.get(8)?,
                })
            })
            .map_err(|e| e.to_string())?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())?;
        Ok(rows)
    }
}

fn mark_cloud_sync_dirty_tx(tx: &Transaction, layer: &str, now: &str) {
    let _ = tx.execute(
        "INSERT INTO cloud_sync_meta (object_key, revision, content_hash, sync_state, updated_at)
         VALUES (?1, 0, NULL, 'dirty', ?2)
         ON CONFLICT(object_key) DO UPDATE SET sync_state = 'dirty', updated_at = ?2",
        params![format!("{layer}:published"), now],
    );
}

fn format_conversation(messages: &[(String, String)]) -> String {
    const MAX_CONVERSATION_CHARS: usize = 48_000;
    let text = messages
        .iter()
        .filter(|(role, _)| matches!(role.as_str(), "user" | "assistant"))
        .filter_map(|(role, content)| {
            // L0 清洗：先剥离宿主注入的上下文包裹（system-reminder/meta 等），
            // 再按角色剔除助手思考块；两个角色都只保留人说的话和 Agent 的回复。
            let cleaned = strip_injected_context(content);
            let content = if role == "assistant" {
                strip_thinking_blocks(&cleaned).trim().to_string()
            } else {
                cleaned.trim().to_string()
            };
            (!content.is_empty()).then(|| {
                format!(
                    "[{}]\n{}",
                    if role == "user" { "用户" } else { "助手" },
                    content
                )
            })
        })
        .collect::<Vec<_>>()
        .join("\n\n");
    if text.chars().count() <= MAX_CONVERSATION_CHARS {
        text
    } else {
        format!(
            "{}\n\n[对话过长，已截断]",
            text.chars()
                .take(MAX_CONVERSATION_CHARS)
                .collect::<String>()
        )
    }
}

/// 宿主环境注入的上下文包裹（Kimi 的 `<meta/>`、各 Agent 的
/// `<system-reminder>` / `<agent-context>` 等）是 harness 对模型说的话，
/// 不是用户对 Agent 说的话，一律不得进入 L0。只剥离成对闭合或自闭合的
/// 完整块；未闭合的标签保留原文，避免误删用户刻意引用的内容。
///
/// pub(crate)：转录读取器在计算内容指纹前就要应用同一规则，否则
/// 「指纹未变」的旧行永远不会被新清洗规则重录。
pub(crate) fn strip_injected_context(text: &str) -> String {
    const PAIRED_TAGS: [&str; 7] = [
        "system-reminder",
        "agent-context",
        "user_info",
        "identity_context",
        "environment_context",
        "relevant_memory_details",
        "relevant_memories",
    ];
    let mut cleaned = text.to_string();
    // 自闭合 <meta ... /> 注入。
    while let Some(start) = cleaned.find("<meta") {
        let boundary = cleaned[start + 5..].chars().next();
        if !matches!(boundary, Some(' ' | '\t' | '\n' | '\r' | '/')) {
            break;
        }
        let Some(end) = cleaned[start..].find("/>") else {
            break;
        };
        cleaned.replace_range(start..start + end + 2, "");
    }
    for tag in PAIRED_TAGS {
        let open = format!("<{tag}");
        let close = format!("</{tag}>");
        while let Some(start) = cleaned.find(&open) {
            let boundary = cleaned[start + open.len()..].chars().next();
            if !matches!(boundary, Some(' ' | '\t' | '\n' | '\r' | '>' | '/')) {
                break;
            }
            let Some(end) = cleaned[start..].find(&close) else {
                break;
            };
            cleaned.replace_range(start..start + end + close.len(), "");
        }
    }
    // 折叠剥离后残留的连续空行。
    let mut out = String::with_capacity(cleaned.len());
    let mut blank_lines = 0u32;
    for line in cleaned.lines() {
        if line.trim().is_empty() {
            blank_lines += 1;
            if blank_lines > 1 {
                continue;
            }
        } else {
            blank_lines = 0;
        }
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str(line);
    }
    out.trim().to_string()
}

fn truncate_chars(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    format!(
        "{}\n[内容已截断]",
        text.chars().take(max_chars).collect::<String>()
    )
}

/// Escape LIKE wildcards in a prefilter term; '%'/'_'/'\\' can legitimately
/// appear in latin terms ('_' survives the keyword_terms filter).
fn like_escape(term: &str) -> String {
    term.replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
}

fn keyword_terms(query: &str) -> Vec<String> {
    let lower = query.to_lowercase();
    let mut terms = Vec::new();
    let mut latin = String::new();
    let mut cjk = Vec::new();
    for character in lower.chars() {
        if character.is_ascii_alphanumeric() || character == '_' || character == '-' {
            latin.push(character);
            continue;
        }
        if latin.len() >= 2 {
            terms.push(std::mem::take(&mut latin));
        } else {
            latin.clear();
        }
        if ('\u{4e00}'..='\u{9fff}').contains(&character) {
            cjk.push(character);
        }
    }
    if latin.len() >= 2 {
        terms.push(latin);
    }
    for pair in cjk.windows(2) {
        terms.push(pair.iter().collect());
    }
    terms.sort();
    terms.dedup();
    terms
}

fn keyword_score(text: &str, query: &str, terms: &[String]) -> f64 {
    let haystack = text.to_lowercase();
    let matches = terms
        .iter()
        .filter(|term| haystack.contains(term.as_str()))
        .count();
    let coverage = if terms.is_empty() {
        0.0
    } else {
        matches as f64 / terms.len() as f64
    };
    let exact_bonus = haystack
        .contains(&query.to_lowercase())
        .then_some(0.35)
        .unwrap_or(0.0);
    // Keep the direct phrase match dominant, then add a bounded semantic-alias
    // signal.  This stays entirely offline while allowing natural switches
    // between common Vibe Assistant terms such as "偏好" / "约束" or "MCP" /
    // "共享记忆".
    let aliases = local_search_aliases(query);
    let alias_matches = aliases
        .iter()
        .filter(|term| haystack.contains(term.as_str()))
        .count();
    let alias_score = if aliases.is_empty() {
        0.0
    } else {
        alias_matches as f64 / aliases.len() as f64 * 0.42
    };
    if matches == 0 && alias_matches == 0 {
        return 0.0;
    }
    coverage + exact_bonus + alias_score
}

/// Small, transparent local equivalence set for the vocabulary users actually
/// use in Vibe Assistant. It is deliberately not a remote embedding service:
/// all matching happens against the SQLite ledger and never leaves the device.
fn local_search_aliases(query: &str) -> Vec<String> {
    let normalized = query.to_lowercase();
    let groups: [(&[&str], &[&str]); 7] = [
        (
            &["记忆", "memory", "上下文"],
            &["长期记忆", "共享记忆", "会话要点", "上下文"],
        ),
        (
            &["偏好", "约束", "习惯", "profile"],
            &["用户偏好", "长期偏好", "约束", "用户习惯"],
        ),
        (
            &["skill", "技能", "skill库"],
            &["共享 skill", "技能库", "skill library", "已发布 skill"],
        ),
        (
            &["mcp", "共享记忆", "注入"],
            &["模型上下文协议", "共享记忆", "recall_memory", "agent 注入"],
        ),
        (
            &["hook", "监听", "会话", "对话"],
            &["hook", "完整对话", "会话记录", "自动沉淀"],
        ),
        (
            &["token", "用量", "消耗"],
            &["token", "输入", "输出", "用量统计"],
        ),
        (&["检索", "搜索", "查找"], &["检索", "搜索", "召回", "匹配"]),
    ];
    let mut aliases = groups
        .into_iter()
        .filter(|(triggers, _)| {
            triggers
                .iter()
                .any(|trigger| normalized.contains(&trigger.to_lowercase()))
        })
        .flat_map(|(_, aliases)| aliases.iter().map(|alias| alias.to_lowercase()))
        .filter(|alias| !normalized.contains(alias))
        .collect::<Vec<_>>();
    aliases.sort();
    aliases.dedup();
    aliases
}

/// Persist every parseable external timestamp in one sortable UTC format.
/// The UI deliberately renders this instant in the user's local time zone;
/// raw `+00:00` values are never shown as if they were local wall-clock time.
fn canonical_timestamp(value: &str) -> String {
    chrono::DateTime::parse_from_rfc3339(value)
        .map(|time| {
            time.with_timezone(&chrono::Utc)
                .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
        })
        .unwrap_or_else(|_| chrono::Utc::now().to_rfc3339())
}

/// Returns (provenance, input, output, cached).  Only values supplied by an
/// adapter are labelled reported; absent values remain unavailable rather than
/// being presented as a misleading estimate.
fn extract_usage(payload: &Value) -> (&'static str, i64, i64, i64) {
    let usage = payload.get("usage").unwrap_or(payload);
    let input = get_i64(usage, &["input_tokens", "prompt_tokens"]);
    let output = get_i64(usage, &["output_tokens", "completion_tokens"]);
    let cached = get_i64(usage, &["cached_tokens", "cache_read_input_tokens"]);
    if input.is_some() || output.is_some() || cached.is_some() {
        (
            "reported",
            input.unwrap_or(0),
            output.unwrap_or(0),
            cached.unwrap_or(0),
        )
    } else {
        ("unavailable", 0, 0, 0)
    }
}

/// Index the native transcript locations once per manual refresh instead of
/// repeatedly walking the disk for every Hook event.  The traversal is bounded
/// to the native session folders and matches exact session identifiers.
fn replace_transcript_mcp_calls(tx: &Transaction<'_>, parsed: &[ParsedMcpCall]) -> Result<(), String> {
    tx.execute("DELETE FROM mcp_call_observations WHERE origin = 'transcript'", [])
        .map_err(|e| e.to_string())?;
    for call in parsed {
        tx.execute(
            "INSERT OR IGNORE INTO mcp_call_observations
             (observation_key, source, origin, session_id, occurred_at, server_name, tool_name, status, error_category, result_observed, parser_version)
             VALUES (?1, ?2, 'transcript', ?3, ?4, ?5, ?6, ?7, ?8, ?9, 1)",
            params![call.key, call.source, call.session_id, call.occurred_at, call.server_name,
                call.tool_name, call.status, call.error_category, call.result_observed as i64],
        ).map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Hash relay identifiers so client-supplied keys and connection ids are not
/// persisted. A call key is `session-id:sequence`; its session portion is
/// hashed separately to group observations without exposing a raw id.
#[tauri::command]
pub async fn telemetry_summary(
    state: tauri::State<'_, TelemetryStore>,
) -> Result<TelemetrySummary, String> {
    state.summary()
}

#[tauri::command]
pub fn telemetry_refresh_usage(
    app: tauri::AppHandle,
    state: tauri::State<'_, TelemetryStore>,
) -> Result<TelemetryUsageRefresh, String> {
    let result = state.refresh_transcript_usage();
    // Notify open dashboards even when the scan reports failures: the
    // ledger may still have been partially refreshed.
    let _ = app.emit("telemetry-updated", ());
    result
}

/// 常开应用的用量采集：周期性重扫转录（metadata walk 很轻，仅变化文件
/// 会重新解析）。启动时 lib.rs 已同步跑过一次，这里首次延迟一个周期，
/// 之后每当有会话更新就广播 telemetry-updated，让已打开的使用统计页
/// 自动补上新数据，而不需要用户手动点「重新扫描」。
pub fn start_usage_refresh_scheduler(app: tauri::AppHandle) {
    tauri::async_runtime::spawn(async move {
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(90)).await;
            let Some(store) = shared_store() else {
                continue;
            };
            let handle = app.clone();
            let refreshed = tauri::async_runtime::spawn_blocking(move || {
                let usage = store
                    .refresh_transcript_usage()
                    .map(|result| result.updated_sessions)
                    .unwrap_or(0);
                // 定价回填断点续跑：全部完成后是零成本空查询。
                let _ = store.recalculate_costs(false);
                usage
            })
            .await
            .unwrap_or(0);
            if refreshed > 0 {
                let _ = handle.emit("telemetry-updated", ());
            }
        }
    });
}

#[tauri::command]
pub async fn telemetry_live_status(
    state: tauri::State<'_, TelemetryStore>,
) -> Result<TelemetryLiveStatus, String> {
    state.live_status()
}

#[tauri::command]
pub async fn telemetry_recent_events(
    state: tauri::State<'_, TelemetryStore>,
    limit: Option<u32>,
) -> Result<Vec<TelemetryEvent>, String> {
    state.recent_events(limit.unwrap_or(50))
}

#[tauri::command]
pub async fn telemetry_usage_records(
    state: tauri::State<'_, TelemetryStore>,
    limit: Option<u32>,
) -> Result<Vec<TelemetryUsageRecord>, String> {
    state.usage_records(limit.unwrap_or(200))
}

/// PRD 一期：追溯重算成本（models.dev 同步或改价后调用）。
/// 分批后台执行，force=true 时连非 user_override 的行一起重算。
#[tauri::command]
pub async fn telemetry_recalculate_costs(
    app: tauri::AppHandle,
    force: Option<bool>,
) -> Result<usize, String> {
    let Some(store) = shared_store() else {
        return Err("telemetry store 未初始化".into());
    };
    let force = force.unwrap_or(false);
    let processed = tauri::async_runtime::spawn_blocking(move || {
        store.recalculate_costs(force)
    })
    .await
    .map_err(|e| e.to_string())?
    .map_err(|e| e.to_string())?;
    let _ = app.emit("telemetry-updated", ());
    Ok(processed)
}

#[tauri::command]
pub async fn telemetry_usage_analytics(
    state: tauri::State<'_, TelemetryStore>,
    start_at: Option<String>,
    end_at: Option<String>,
    source: Option<String>,
    bucket: Option<String>,
    record_limit: Option<u32>,
) -> Result<TelemetryUsageAnalytics, String> {
    state.usage_analytics(
        start_at.as_deref(),
        end_at.as_deref(),
        source
            .as_deref()
            .filter(|value| !value.trim().is_empty() && *value != "all"),
        bucket.as_deref().unwrap_or("day"),
        record_limit.unwrap_or(500),
    )
}

#[tauri::command]
pub async fn telemetry_search_conversations(
    state: tauri::State<'_, TelemetryStore>,
    query: String,
    limit: Option<u32>,
) -> Result<Vec<ConversationSearchHit>, String> {
    state.search_conversations(&query, limit.unwrap_or(20))
}

#[tauri::command]
pub fn memory_mcp_access_logs(
    state: tauri::State<'_, TelemetryStore>,
    limit: Option<u32>,
) -> Result<Vec<McpAccessLog>, String> {
    state.mcp_access_logs(limit.unwrap_or(30))
}

#[tauri::command]
pub fn telemetry_refresh_mcp_calls(
    app: tauri::AppHandle,
    state: tauri::State<'_, TelemetryStore>,
) -> Result<McpCallRefresh, String> {
    let result = state.refresh_mcp_calls();
    let _ = app.emit("telemetry-updated", ());
    result
}

#[tauri::command]
pub fn telemetry_mcp_dashboard(state: tauri::State<'_, TelemetryStore>) -> Result<McpDashboardStats, String> {
    state.mcp_dashboard_stats()
}

#[tauri::command]
pub fn telemetry_mcp_stats(
    state: tauri::State<'_, TelemetryStore>,
    start_at: Option<String>,
    end_at: Option<String>,
    source: Option<String>,
    server: Option<String>,
    tool: Option<String>,
    recent_limit: Option<u32>,
) -> Result<McpCallStats, String> {
    state.mcp_call_stats(start_at.as_deref(), end_at.as_deref(), source.as_deref(), server.as_deref(), tool.as_deref(), recent_limit.unwrap_or(50))
}

#[tauri::command]
pub fn telemetry_mcp_failed_calls(
    state: tauri::State<'_, TelemetryStore>,
    offset: Option<u32>,
    limit: Option<u32>,
    server: Option<String>,
    origin: Option<String>,
    tool: Option<String>,
    start_at: Option<String>,
    end_at: Option<String>,
    snapshot_id: Option<i64>,
    before_at: Option<String>,
    before_id: Option<i64>,
) -> Result<McpFailedCallPage, String> {
    state.mcp_failed_calls_scoped(offset.unwrap_or(0), limit.unwrap_or(25), server.as_deref(), origin.as_deref(),
        tool.as_deref(), start_at.as_deref(), end_at.as_deref(), snapshot_id, before_at.as_deref(), before_id)
}

#[tauri::command]
pub fn memory_injection_stats(
    state: tauri::State<'_, TelemetryStore>,
    days: Option<u32>,
) -> Result<Vec<MemoryInjectionStats>, String> {
    state.memory_injection_stats(days.unwrap_or(30))
}

#[cfg(test)]
mod tests;
