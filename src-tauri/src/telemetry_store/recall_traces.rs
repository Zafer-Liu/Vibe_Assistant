//! Body-free recall diagnostics. Detail views resolve only the current L1 ledger.
use super::{LocalMemorySnapshot, TelemetryStore};
use crate::memory_retrieval::trace::{
    fingerprint, RecallTrace, RecallTraceDetail, RecallTraceSummary, TraceMemoryView,
    MAX_TRACE_BYTES,
};
use chrono::{SecondsFormat, Utc};
use rusqlite::{params, types::ValueRef, Connection, OptionalExtension, Row, TransactionBehavior};
use std::collections::HashSet;
use std::sync::TryLockError;
use std::time::{Duration, Instant};

const RETENTION_DAYS: i64 = 14;
const MAX_LIST: u32 = 100;
const MAX_SQLITE_WAIT: Duration = Duration::from_millis(10);
// Refuse an exceptional current row rather than truncating its fingerprint input.
const MAX_MEMORY_BYTES: usize = 1024 * 1024;
const MAX_DETAIL_BYTES: usize = 8 * 1024 * 1024;
const MAX_DOMAINS: usize = 256;

pub(super) fn initialize(conn: &Connection) -> Result<(), String> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS memory_recall_traces (
             request_id TEXT PRIMARY KEY NOT NULL,
             created_at TEXT NOT NULL,
             entrypoint TEXT NOT NULL,
             query TEXT NOT NULL,
             trace_json TEXT NOT NULL
                 CHECK(typeof(trace_json) = 'text'
                       AND length(CAST(trace_json AS BLOB)) <= 65536)
         );
         CREATE INDEX IF NOT EXISTS idx_memory_recall_traces_created
             ON memory_recall_traces(created_at DESC, request_id DESC);",
    ).map_err(|error| error.to_string())
}

fn cutoff() -> String {
    (Utc::now() - chrono::Duration::days(RETENTION_DAYS))
        .to_rfc3339_opts(SecondsFormat::Nanos, true)
}

// Fixed-width UTC keys make indexed comparisons correct for offsets and varying
// RFC3339 precision. The original timestamp stays intact in the trace JSON.
fn timestamp_key(timestamp: &str) -> Result<String, String> {
    chrono::DateTime::parse_from_rfc3339(timestamp)
        .map(|value| value.with_timezone(&Utc).to_rfc3339_opts(SecondsFormat::Nanos, true))
        .map_err(|_| "invalid recall trace timestamp".to_string())
}

fn validate_trace(trace: &RecallTrace) -> Result<String, String> {
    if trace.schema_version != 1 {
        return Err("unsupported recall trace schema version".into());
    }
    if trace.request_id.trim().is_empty() {
        return Err("recall trace request_id is empty".into());
    }
    let mut ids = HashSet::new();
    for result in &trace.results {
        if result.memory_id.trim().is_empty() || !ids.insert(result.memory_id.as_str()) {
            return Err("recall trace contains an empty or duplicate result ID".into());
        }
        if result.fingerprint.len() != 64
            || !result.fingerprint.bytes().all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err("invalid recall trace fingerprint".into());
        }
    }
    timestamp_key(&trace.created_at)
}

/// Restores the exact connection setting on every return path, including rollback
/// and unwinding. Explicit restoration also surfaces a restoration failure.
pub(super) struct RestoreBusyTimeout<'a> {
    pub(super) conn: &'a mut Connection,
    previous: Duration,
    restored: bool,
}

impl<'a> RestoreBusyTimeout<'a> {
    pub(super) fn new(conn: &'a mut Connection) -> Result<Self, String> {
        let milliseconds: u64 = conn.pragma_query_value(None, "busy_timeout", |row| row.get(0))
            .map_err(|error| error.to_string())?;
        Ok(Self { conn, previous: Duration::from_millis(milliseconds), restored: false })
    }

    pub(super) fn restore(mut self) -> Result<(), String> {
        self.conn.busy_timeout(self.previous)
            .map_err(|error| format!("restore recall trace busy_timeout: {error}"))?;
        self.restored = true;
        Ok(())
    }
}

impl Drop for RestoreBusyTimeout<'_> {
    fn drop(&mut self) {
        if !self.restored {
            let _ = self.conn.busy_timeout(self.previous);
        }
    }
}

impl TelemetryStore {
    /// Ok reports the persisted trace's `truncated` flag, never a skipped write.
    /// Serialization and setup consume the caller's budget; SQLite lock waiting
    /// gets at most its remainder or 10ms. Never queue behind the writer mutex.
    pub(crate) fn try_save_recall_trace(
        &self,
        trace: &RecallTrace,
        lock_budget: Duration,
    ) -> Result<bool, String> {
        let started = Instant::now();
        let json = trace.bounded_json()?;
        if json.len() > MAX_TRACE_BYTES {
            return Err("recall trace exceeds storage limit".into());
        }
        let bounded: RecallTrace = serde_json::from_str(&json)
            .map_err(|_| "invalid serialized recall trace".to_string())?;
        let created_at = validate_trace(&bounded)?;
        let expires_before = cutoff();
        if created_at <= expires_before {
            return Err("recall trace is already expired".into());
        }
        let mut conn = match self.conn.try_lock() {
            Ok(conn) => conn,
            Err(TryLockError::WouldBlock) => return Err("recall trace writer is busy".into()),
            Err(TryLockError::Poisoned(_)) => return Err("telemetry store lock poisoned".into()),
        };
        let timeout = RestoreBusyTimeout::new(&mut conn)?;
        let saved = (|| -> Result<(), String> {
            let remaining = lock_budget.saturating_sub(started.elapsed()).min(MAX_SQLITE_WAIT);
            if remaining.is_zero() {
                return Err("recall trace lock budget exhausted".into());
            }
            timeout.conn.busy_timeout(remaining).map_err(|error| error.to_string())?;
            let transaction = timeout.conn.transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(|error| error.to_string())?;
            // BEGIN IMMEDIATE owns the writer slot. No later statement, COMMIT,
            // or rollback may start another lock wait with a fresh budget.
            transaction.busy_timeout(Duration::ZERO).map_err(|error| error.to_string())?;
            transaction.execute(
                "INSERT INTO memory_recall_traces
                     (request_id, created_at, entrypoint, query, trace_json)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![bounded.request_id, created_at, bounded.entrypoint, bounded.query, json],
            ).map_err(|error| error.to_string())?;
            transaction.execute(
                "DELETE FROM memory_recall_traces WHERE request_id IN (
                     SELECT request_id FROM memory_recall_traces
                      WHERE created_at <= ?1
                      ORDER BY created_at ASC, request_id ASC LIMIT 50
                 )",
                [&expires_before],
            ).map_err(|error| error.to_string())?;
            transaction.execute(
                "DELETE FROM memory_recall_traces WHERE request_id IN (
                     SELECT request_id FROM memory_recall_traces
                      ORDER BY created_at DESC, request_id DESC LIMIT 50 OFFSET 1000
                 )",
                [],
            ).map_err(|error| error.to_string())?;
            let retained: bool = transaction.query_row(
                "SELECT EXISTS(SELECT 1 FROM memory_recall_traces WHERE request_id = ?1)",
                [&bounded.request_id], |row| row.get(0),
            ).map_err(|error| error.to_string())?;
            if !retained {
                return Err("recall trace falls outside the retention window".into());
            }
            transaction.commit().map_err(|error| error.to_string())
        })();
        let restored = timeout.restore();
        match (saved, restored) {
            (Ok(()), Ok(())) => Ok(bounded.truncated),
            (Err(error), Ok(())) | (Ok(()), Err(error)) => Err(error),
            (Err(error), Err(restore_error)) => Err(format!("{error}; {restore_error}")),
        }
    }
}

#[tauri::command]
pub(crate) async fn memory_recall_trace_list(
    telemetry: tauri::State<'_, TelemetryStore>,
    limit: Option<u32>,
) -> Result<Vec<RecallTraceSummary>, String> {
    let store = telemetry.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        store.with_read_connection(|conn| list_on(conn, limit))
    }).await.map_err(|_| "recall trace list task failed".to_string())?
}

#[tauri::command]
pub(crate) async fn memory_recall_trace_get(
    telemetry: tauri::State<'_, TelemetryStore>,
    request_id: String,
) -> Result<Option<RecallTraceDetail>, String> {
    let store = telemetry.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        store.with_read_connection(|conn| get_on(conn, &request_id))
    }).await.map_err(|_| "recall trace detail task failed".to_string())?
}

fn text<'a>(row: &'a Row<'_>, index: usize, max_bytes: usize) -> Result<&'a str, String> {
    match row.get_ref(index).map_err(|error| error.to_string())? {
        ValueRef::Text(bytes) if bytes.len() <= max_bytes => std::str::from_utf8(bytes)
            .map_err(|_| "invalid UTF-8 in recall trace data".to_string()),
        _ => Err("invalid or oversized recall trace data".into()),
    }
}

fn decode_trace(row: &Row<'_>) -> Result<RecallTrace, String> {
    let json = text(row, 4, MAX_TRACE_BYTES)?;
    // Do not echo malformed JSON (which may contain sensitive data) into errors.
    let trace: RecallTrace = serde_json::from_str(json)
        .map_err(|_| "invalid stored recall trace JSON".to_string())?;
    let created_at = validate_trace(&trace)?;
    if trace.request_id != text(row, 0, MAX_TRACE_BYTES)?
        || created_at != text(row, 1, MAX_TRACE_BYTES)?
        || trace.entrypoint != text(row, 2, MAX_TRACE_BYTES)?
        || trace.query != text(row, 3, MAX_TRACE_BYTES)?
    {
        return Err("recall trace index metadata does not match its JSON".into());
    }
    Ok(trace)
}

fn list_on(conn: &Connection, limit: Option<u32>) -> Result<Vec<RecallTraceSummary>, String> {
    let mut statement = conn.prepare(
        "SELECT request_id, created_at, entrypoint, query,
                CASE WHEN typeof(trace_json) = 'text'
                           AND length(CAST(trace_json AS BLOB)) <= ?3
                     THEN trace_json ELSE NULL END
           FROM memory_recall_traces WHERE created_at > ?1
          ORDER BY created_at DESC, request_id DESC LIMIT ?2",
    ).map_err(|error| error.to_string())?;
    let mut rows = statement.query(params![cutoff(), limit.unwrap_or(MAX_LIST).min(MAX_LIST), MAX_TRACE_BYTES])
        .map_err(|error| error.to_string())?;
    let mut summaries = Vec::new();
    while let Some(row) = rows.next().map_err(|error| error.to_string())? {
        summaries.push(decode_trace(row)?.summary());
    }
    Ok(summaries)
}

fn snapshot_on(row: &Row<'_>) -> Result<LocalMemorySnapshot, String> {
    let event_time = match row.get_ref(9).map_err(|error| error.to_string())? {
        ValueRef::Null => None,
        _ => Some(text(row, 9, MAX_TRACE_BYTES)?.to_string()),
    };
    Ok(LocalMemorySnapshot {
        id: text(row, 0, MAX_TRACE_BYTES)?.to_string(),
        source_event_key: text(row, 1, MAX_TRACE_BYTES)?.to_string(),
        ordinal: row.get(2).map_err(|error| error.to_string())?,
        memory: text(row, 3, MAX_MEMORY_BYTES)?.to_string(),
        memory_type: text(row, 4, MAX_TRACE_BYTES)?.to_string(),
        durability: text(row, 5, MAX_TRACE_BYTES)?.to_string(),
        source: text(row, 6, MAX_TRACE_BYTES)?.to_string(),
        session_id: text(row, 7, MAX_TRACE_BYTES)?.to_string(),
        user_defined: row.get::<_, i64>(8).map_err(|error| error.to_string())? != 0,
        event_time,
        created_at: text(row, 10, MAX_TRACE_BYTES)?.to_string(),
        updated_at: text(row, 11, MAX_TRACE_BYTES)?.to_string(),
    })
}

fn get_on(conn: &mut Connection, request_id: &str) -> Result<Option<RecallTraceDetail>, String> {
    if request_id.trim().is_empty() || request_id.len() > MAX_TRACE_BYTES {
        return Err("invalid recall trace request_id".into());
    }
    // The trace and every current row/domain link share one SQLite snapshot.
    // On disk this is a pooled READ_ONLY connection; no writer mutex is needed.
    let transaction = conn.transaction().map_err(|error| error.to_string())?;
    let trace = {
        let mut statement = transaction.prepare(
            "SELECT request_id, created_at, entrypoint, query,
                    CASE WHEN typeof(trace_json) = 'text'
                               AND length(CAST(trace_json AS BLOB)) <= ?3
                         THEN trace_json ELSE NULL END
               FROM memory_recall_traces WHERE request_id = ?1 AND created_at > ?2",
        ).map_err(|error| error.to_string())?;
        let mut rows = statement.query(params![request_id, cutoff(), MAX_TRACE_BYTES])
            .map_err(|error| error.to_string())?;
        match rows.next().map_err(|error| error.to_string())? {
            Some(row) => decode_trace(row)?,
            None => return Ok(None),
        }
    };
    let mut current_memories = Vec::with_capacity(trace.results.len());
    {
        // Point lookups reuse the primary-key indexes. The body-free trace's
        // 64KiB cap bounds the number of IDs; never enumerate the whole ledger.
        let mut memories = transaction.prepare(
            "SELECT id, source_event_key, ordinal,
                    CASE WHEN length(CAST(memory AS BLOB)) <= ?2 THEN memory ELSE NULL END,
                    memory_type, durability, source, session_id, user_defined,
                    event_time, created_at, updated_at
               FROM local_memory_items WHERE id = ?1 LIMIT 1",
        ).map_err(|error| error.to_string())?;
        let mut links = transaction.prepare(
            "SELECT DISTINCT domain_id FROM local_memory_domains WHERE memory_id = ?1
              ORDER BY domain_id LIMIT ?2",
        ).map_err(|error| error.to_string())?;
        let mut remaining_bytes = MAX_DETAIL_BYTES;
        for result in &trace.results {
            let snapshot = memories.query_row(params![result.memory_id, MAX_MEMORY_BYTES], |row| {
                // The SQL CASE and borrowed text checks reject oversized fields
                // before copying them into an owned snapshot.
                Ok(snapshot_on(row))
            }).optional().map_err(|error| error.to_string())?.transpose()?;
            let Some(snapshot) = snapshot else {
                current_memories.push(TraceMemoryView {
                    memory_id: result.memory_id.clone(), state: "deleted".into(),
                    content: None, domains: Vec::new(), source_event_key: None,
                    source_agent: None, session_id: None,
                });
                continue;
            };
            let snapshot_bytes = snapshot.id.len() + snapshot.source_event_key.len()
                + snapshot.memory.len() + snapshot.memory_type.len() + snapshot.durability.len()
                + snapshot.source.len() + snapshot.session_id.len() + snapshot.created_at.len()
                + snapshot.updated_at.len() + snapshot.event_time.as_ref().map_or(0, String::len);
            remaining_bytes = remaining_bytes.checked_sub(snapshot_bytes)
                .ok_or("current recall memory detail exceeds size limit")?;
            let mut rows = links.query(params![result.memory_id, MAX_DOMAINS + 1])
                .map_err(|error| error.to_string())?;
            let mut domains = Vec::new();
            while let Some(row) = rows.next().map_err(|error| error.to_string())? {
                if domains.len() == MAX_DOMAINS {
                    return Err("current recall memory has too many domains".into());
                }
                let domain = text(row, 0, MAX_TRACE_BYTES)?;
                remaining_bytes = remaining_bytes.checked_sub(domain.len())
                    .ok_or("current recall memory detail exceeds size limit")?;
                domains.push(domain.to_string());
            }
            let state = if fingerprint(&snapshot, &domains) == result.fingerprint {
                "unchanged"
            } else {
                "changed"
            };
            current_memories.push(TraceMemoryView {
                memory_id: snapshot.id, state: state.into(), content: Some(snapshot.memory),
                domains, source_event_key: Some(snapshot.source_event_key),
                source_agent: Some(snapshot.source), session_id: Some(snapshot.session_id),
            });
        }
    }
    transaction.commit().map_err(|error| error.to_string())?;
    Ok(Some(RecallTraceDetail { trace, current_memories }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory_retrieval::trace::{TraceNomination, TraceResult};
    use serde_json::json;

    fn store() -> TelemetryStore {
        let store = TelemetryStore::in_memory();
        initialize(&store.conn.lock().unwrap()).unwrap();
        store
    }

    fn trace(id: &str) -> RecallTrace {
        RecallTrace {
            schema_version: 1, request_id: id.into(), created_at: Utc::now().to_rfc3339(),
            entrypoint: "desktop".into(), query: "a diagnostic query".into(),
            arguments: json!({"limit": 6}), routing: json!({"active_domains": ["work"]}),
            retrieval: json!({"mode": "hybrid", "timings_ms": {"total": 23}}),
            candidates: Vec::new(), ranking: Vec::new(), results: Vec::new(), truncated: false,
        }
    }

    // Fixture insertion bypasses lock timing to isolate read/retention assertions.
    fn insert(conn: &Connection, trace: &RecallTrace) {
        conn.execute(
            "INSERT INTO memory_recall_traces VALUES (?1, ?2, ?3, ?4, ?5)",
            params![trace.request_id, timestamp_key(&trace.created_at).unwrap(),
                trace.entrypoint, trace.query, trace.bounded_json().unwrap()],
        ).unwrap();
    }

    fn list(store: &TelemetryStore, limit: Option<u32>) -> Result<Vec<RecallTraceSummary>, String> {
        store.with_read_connection(|conn| list_on(conn, limit))
    }

    fn detail(store: &TelemetryStore, id: &str) -> RecallTraceDetail {
        store.with_read_connection(|conn| get_on(conn, id)).unwrap().unwrap()
    }

    fn busy_timeout(conn: &Connection) -> u64 {
        conn.pragma_query_value(None, "busy_timeout", |row| row.get(0)).unwrap()
    }

    #[test]
    fn save_is_body_free_and_duplicate_failure_restores_timeout() {
        let store = store();
        store.conn.lock().unwrap().busy_timeout(Duration::from_millis(1234)).unwrap();
        let trace = trace("saved");
        assert!(!store.try_save_recall_trace(&trace, MAX_SQLITE_WAIT).unwrap());
        assert!(store.try_save_recall_trace(&trace, MAX_SQLITE_WAIT).is_err());
        let conn = store.conn.lock().unwrap();
        assert_eq!(busy_timeout(&conn), 1234);
        let json: String = conn.query_row(
            "SELECT trace_json FROM memory_recall_traces WHERE request_id = 'saved'",
            [], |row| row.get(0),
        ).unwrap();
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert!(value.get("current_memories").is_none());
        assert_eq!(value["request_id"], "saved");
        assert_eq!(list_on(&conn, None).unwrap()[0].total_ms, 23);
    }

    #[test]
    fn writer_contention_and_exhausted_budget_never_report_a_save() {
        let store = store();
        let conn = store.conn.lock().unwrap();
        assert!(store.try_save_recall_trace(&trace("mutex-busy"), MAX_SQLITE_WAIT).is_err());
        drop(conn);
        assert!(store.try_save_recall_trace(&trace("no-budget"), Duration::ZERO).is_err());
        assert!(list(&store, None).unwrap().is_empty());
        assert_eq!(busy_timeout(&store.conn.lock().unwrap()), 8000);
    }

    #[test]
    fn oversized_diagnostic_is_bounded_and_oversized_scope_is_not_saved() {
        let store = store();
        let mut trace = trace("trimmed");
        trace.candidates.push(TraceNomination {
            route: "keyword".into(), memory_id: "x".repeat(MAX_TRACE_BYTES),
            input_rank: 1, eligible_rank: None, score: None, status: "selected".into(),
        });
        assert!(store.try_save_recall_trace(&trace, Duration::from_secs(1)).unwrap());
        let conn = store.conn.lock().unwrap();
        let (bytes, json): (usize, String) = conn.query_row(
            "SELECT length(CAST(trace_json AS BLOB)), trace_json
               FROM memory_recall_traces WHERE request_id = 'trimmed'",
            [], |row| Ok((row.get(0)?, row.get(1)?)),
        ).unwrap();
        assert!(bytes <= MAX_TRACE_BYTES);
        let saved: RecallTrace = serde_json::from_str(&json).unwrap();
        assert!(saved.truncated);
        assert!(saved.candidates.is_empty());
        drop(conn);
        trace.request_id = "too-large".into();
        trace.query = "界".repeat(MAX_TRACE_BYTES);
        assert!(store.try_save_recall_trace(&trace, MAX_SQLITE_WAIT).is_err());
        assert!(store.with_read_connection(|conn| get_on(conn, "too-large")).unwrap().is_none());
    }

    #[test]
    fn list_is_capped_and_stable_and_expired_details_are_hidden() {
        let store = store();
        let mut trace = trace("unused");
        let conn = store.conn.lock().unwrap();
        for index in 0..105 {
            trace.request_id = format!("request-{index:03}");
            insert(&conn, &trace);
        }
        trace.request_id = "expired".into();
        trace.created_at = (Utc::now() - chrono::Duration::days(15)).to_rfc3339();
        insert(&conn, &trace);
        drop(conn);
        let listed = list(&store, Some(u32::MAX)).unwrap();
        assert_eq!(listed.len(), 100);
        assert_eq!(listed[0].request_id, "request-104");
        assert_eq!(listed[99].request_id, "request-005");
        assert!(list(&store, Some(0)).unwrap().is_empty());
        assert!(store.with_read_connection(|conn| get_on(conn, "expired")).unwrap().is_none());
    }

    #[test]
    fn cleanup_is_bounded_and_newest_thousand_survive() {
        let store = store();
        let mut trace = trace("unused");
        let conn = store.conn.lock().unwrap();
        for index in 0..1100 {
            trace.request_id = format!("live-{index:04}");
            insert(&conn, &trace);
        }
        trace.created_at = (Utc::now() - chrono::Duration::days(15)).to_rfc3339();
        for index in 0..100 {
            trace.request_id = format!("old-{index:03}");
            insert(&conn, &trace);
        }
        drop(conn);
        // Use a tiny trace so this exercises pruning, not large serialization.
        let saved = self::trace("newest");
        store.try_save_recall_trace(&saved, MAX_SQLITE_WAIT).unwrap();
        let conn = store.conn.lock().unwrap();
        let (total, expired): (i64, i64) = conn.query_row(
            "SELECT COUNT(*), SUM(created_at <= ?1) FROM memory_recall_traces",
            [cutoff()], |row| Ok((row.get(0)?, row.get(1)?)),
        ).unwrap();
        assert_eq!(total, 1101, "one insert and at most 50+50 deletions");
        assert_eq!(expired, 50, "expiration removes exactly one bounded batch");
        let newest: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM memory_recall_traces WHERE request_id = 'newest')",
            [], |row| row.get(0),
        ).unwrap();
        assert!(newest);
    }

    fn memory() -> LocalMemorySnapshot {
        LocalMemorySnapshot {
            id: "current-memory".into(), source_event_key: "event-original".into(), ordinal: 0,
            memory: "original current body".into(), memory_type: "fact".into(),
            durability: "long_term".into(), source: "test-original".into(),
            session_id: "session-original".into(), user_defined: false, event_time: None,
            created_at: "2026-01-01T00:00:00Z".into(), updated_at: "2026-01-01T00:00:00Z".into(),
        }
    }

    #[test]
    fn details_follow_current_content_sources_domains_and_deletion() {
        let store = store();
        let memory = memory();
        let mut trace = trace("states");
        trace.results.push(TraceResult {
            memory_id: memory.id.clone(), fingerprint: fingerprint(&memory, &["work".into()]),
            domains: vec!["work".into()], source_event_key: memory.source_event_key.clone(),
            source_agent: memory.source.clone(), session_id: memory.session_id.clone(),
            event_time: None, updated_at: memory.updated_at.clone(),
        });
        let conn = store.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO local_memory_items
                 (id, source_event_key, ordinal, memory, memory_type, durability, source,
                  session_id, user_defined, event_time, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
            params![memory.id, memory.source_event_key, memory.ordinal, memory.memory,
                memory.memory_type, memory.durability, memory.source, memory.session_id,
                memory.user_defined, memory.event_time, memory.created_at, memory.updated_at],
        ).unwrap();
        conn.execute(
            "INSERT INTO local_memory_domains VALUES (?1, 'work', 1, 'manual', ?2, ?2)",
            params![memory.id, memory.updated_at],
        ).unwrap();
        insert(&conn, &trace);
        drop(conn);
        assert_eq!(detail(&store, "states").current_memories[0].state, "unchanged");
        store.conn.lock().unwrap().execute(
            "DELETE FROM local_memory_domains WHERE memory_id = ?1", [&memory.id],
        ).unwrap();
        let view = detail(&store, "states").current_memories.remove(0);
        assert_eq!(view.state, "changed", "domain-only changes invalidate the fingerprint");
        assert_eq!(view.content.as_deref(), Some(memory.memory.as_str()));
        assert!(view.domains.is_empty());
        // Multiple assignment sources for one domain do not change its identity.
        store.conn.lock().unwrap().execute(
            "INSERT INTO local_memory_domains VALUES
                 (?1, 'work', 1, 'manual', ?2, ?2), (?1, 'work', 1, 'auto', ?2, ?2)",
            params![memory.id, memory.updated_at],
        ).unwrap();
        assert_eq!(detail(&store, "states").current_memories[0].state, "unchanged");
        store.conn.lock().unwrap().execute(
            "UPDATE local_memory_items SET memory = 'edited body', source_event_key = 'event-current',
                    source = 'agent-current', session_id = 'session-current' WHERE id = ?1",
            [&memory.id],
        ).unwrap();
        let view = detail(&store, "states").current_memories.remove(0);
        assert_eq!(view.state, "changed");
        assert_eq!(view.content.as_deref(), Some("edited body"));
        assert_eq!(view.source_event_key.as_deref(), Some("event-current"));
        assert_eq!(view.source_agent.as_deref(), Some("agent-current"));
        assert_eq!(view.session_id.as_deref(), Some("session-current"));
        store.conn.lock().unwrap().execute(
            "UPDATE local_memory_items SET memory = ?2 WHERE id = ?1",
            params![memory.id, "x".repeat(MAX_MEMORY_BYTES + 1)],
        ).unwrap();
        assert!(store.with_read_connection(|conn| get_on(conn, "states")).is_err());
        store.conn.lock().unwrap().execute(
            "DELETE FROM local_memory_items WHERE id = ?1", [&memory.id],
        ).unwrap();
        let detail = detail(&store, "states");
        assert_eq!(detail.trace.results[0].source_agent, "test-original");
        let view = &detail.current_memories[0];
        assert_eq!(view.state, "deleted");
        assert!(view.content.is_none());
        assert!(view.domains.is_empty());
        assert!(view.source_event_key.is_none());
        assert!(view.source_agent.is_none());
        assert!(view.session_id.is_none());
    }

    #[test]
    fn malformed_mismatched_and_oversized_records_are_rejected() {
        let store = store();
        let trace = trace("corrupt");
        let conn = store.conn.lock().unwrap();
        insert(&conn, &trace);
        conn.execute("UPDATE memory_recall_traces SET trace_json = '{'", []).unwrap();
        assert!(list_on(&conn, None).is_err());
        drop(conn);
        assert!(store.with_read_connection(|conn| get_on(conn, "corrupt")).is_err());
        let conn = store.conn.lock().unwrap();
        conn.execute("UPDATE memory_recall_traces SET trace_json = ?1, query = 'wrong'",
            [trace.bounded_json().unwrap()]).unwrap();
        assert!(list_on(&conn, None).is_err());
        conn.execute_batch("PRAGMA ignore_check_constraints=ON").unwrap();
        conn.execute("UPDATE memory_recall_traces SET trace_json = ?1",
            ["x".repeat(MAX_TRACE_BYTES + 1)]).unwrap();
        assert!(list_on(&conn, None).is_err());
        drop(conn);
        assert!(store.with_read_connection(|conn| get_on(conn, "corrupt")).is_err());
    }

    #[test]
    fn cleanup_failure_rolls_back_the_insert_and_restores_timeout() {
        let store = store();
        let mut expired = trace("retained-on-failure");
        expired.created_at = (Utc::now() - chrono::Duration::days(15)).to_rfc3339();
        let conn = store.conn.lock().unwrap();
        insert(&conn, &expired);
        conn.busy_timeout(Duration::from_millis(777)).unwrap();
        conn.execute_batch(
            "CREATE TRIGGER reject_trace_cleanup BEFORE DELETE ON memory_recall_traces
             BEGIN SELECT RAISE(ABORT, 'cleanup failed'); END;",
        ).unwrap();
        drop(conn);
        assert!(store.try_save_recall_trace(&trace("rolled-back"), MAX_SQLITE_WAIT).is_err());
        let conn = store.conn.lock().unwrap();
        assert_eq!(busy_timeout(&conn), 777);
        let ids: String = conn.query_row(
            "SELECT group_concat(request_id) FROM memory_recall_traces", [], |row| row.get(0),
        ).unwrap();
        assert_eq!(ids, "retained-on-failure");
    }

    #[test]
    fn sqlite_writer_contention_restores_timeout_and_leaves_no_trace() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("recall-traces.sqlite3");
        let conn = Connection::open(&path).unwrap();
        conn.pragma_update(None, "journal_mode", "WAL").unwrap();
        initialize(&conn).unwrap();
        conn.busy_timeout(Duration::from_millis(4321)).unwrap();
        let store = TelemetryStore::from_connection(conn);
        let mut peer = Connection::open(&path).unwrap();
        let writer = peer.transaction_with_behavior(TransactionBehavior::Immediate).unwrap();
        assert!(store.try_save_recall_trace(&trace("locked"), Duration::from_secs(1)).is_err());
        writer.rollback().unwrap();
        assert_eq!(busy_timeout(&store.conn.lock().unwrap()), 4321);
        assert!(list(&store, None).unwrap().is_empty());
    }

    #[test]
    fn timestamp_keys_compare_instants_without_rewriting_history() {
        assert_eq!(timestamp_key("2026-11-01T08:00:00+08:00").unwrap(),
            timestamp_key("2026-11-01T00:00:00Z").unwrap());
        let store = store();
        let mut trace = trace("precision");
        trace.created_at = (Utc::now() - chrono::Duration::hours(1))
            .with_timezone(&chrono::FixedOffset::east_opt(8 * 3600).unwrap()).to_rfc3339();
        let conn = store.conn.lock().unwrap();
        insert(&conn, &trace);
        drop(conn);
        assert_eq!(detail(&store, "precision").trace.created_at, trace.created_at);
    }
}
