//! Short-lived capabilities and atomic accounting. No conversation or memory
//! bodies, and never the plaintext bearer token, are stored in these tables.
use super::{recall_traces::RestoreBusyTimeout, TelemetryStore};
use crate::memory_evidence::types::MemoryBinding;
use crate::memory_retrieval::RecallBudget;
use rusqlite::{params, Connection, OpenFlags, OptionalExtension, TransactionBehavior};
use std::time::Duration;

pub(crate) const MAX_CONTEXT_BYTES: usize = 128 * 1024;
pub(crate) const TOTAL_CHARS: usize = 8000;

#[derive(Clone)]
pub(crate) struct StoredContext {
    pub token_hash: String,
    pub request_id: String,
    pub expires_at: String,
    pub bindings: Vec<MemoryBinding>,
    pub scope: Vec<String>,
    pub cursor_secret: String,
}

pub(crate) struct ReservedContext {
    pub context: StoredContext,
    pub granted: usize,
}

pub(super) fn initialize(conn: &Connection) -> Result<(), String> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS memory_evidence_contexts (
             token_hash TEXT PRIMARY KEY NOT NULL,
             request_id TEXT NOT NULL,
             created_at TEXT NOT NULL,
             expires_at TEXT NOT NULL,
             used_chars INTEGER NOT NULL DEFAULT 0 CHECK(used_chars BETWEEN 0 AND 8000),
             bindings_json TEXT NOT NULL CHECK(length(CAST(bindings_json AS BLOB)) <= 131072),
             scope_json TEXT NOT NULL,
             cursor_secret TEXT NOT NULL
         );
         CREATE INDEX IF NOT EXISTS idx_memory_evidence_expiry ON memory_evidence_contexts(expires_at);
         CREATE INDEX IF NOT EXISTS idx_memory_evidence_created ON memory_evidence_contexts(created_at DESC,token_hash DESC);
         CREATE TABLE IF NOT EXISTS memory_evidence_source_revisions (
             event_key TEXT PRIMARY KEY NOT NULL, revision INTEGER NOT NULL
         );
         CREATE TRIGGER IF NOT EXISTS evidence_source_insert AFTER INSERT ON agent_events
         WHEN NEW.conversation_state = 'full' AND NEW.conversation_text IS NOT NULL AND NEW.conversation_text != '' BEGIN
             INSERT INTO memory_evidence_source_revisions(event_key,revision) VALUES(NEW.event_key,1)
             ON CONFLICT(event_key) DO UPDATE SET revision=revision+1;
         END;
         CREATE TRIGGER IF NOT EXISTS evidence_source_update
         AFTER UPDATE OF conversation_text,conversation_state,source,session_id,occurred_at,event_key ON agent_events
         WHEN (OLD.conversation_state = 'full' OR NEW.conversation_state = 'full')
           AND (OLD.conversation_text IS NOT NEW.conversation_text OR OLD.conversation_state IS NOT NEW.conversation_state
             OR OLD.source IS NOT NEW.source OR OLD.session_id IS NOT NEW.session_id
             OR OLD.occurred_at IS NOT NEW.occurred_at OR OLD.event_key IS NOT NEW.event_key) BEGIN
             INSERT INTO memory_evidence_source_revisions(event_key,revision) VALUES(OLD.event_key,1)
             ON CONFLICT(event_key) DO UPDATE SET revision=revision+1;
             INSERT INTO memory_evidence_source_revisions(event_key,revision)
             SELECT NEW.event_key,1 WHERE OLD.event_key IS NOT NEW.event_key
             ON CONFLICT(event_key) DO UPDATE SET revision=revision+1;
         END;
         CREATE TRIGGER IF NOT EXISTS evidence_source_delete AFTER DELETE ON agent_events
         WHEN OLD.conversation_state = 'full' BEGIN
             INSERT INTO memory_evidence_source_revisions(event_key,revision) VALUES(OLD.event_key,1)
             ON CONFLICT(event_key) DO UPDATE SET revision=revision+1;
         END;",
    ).map_err(|_| "evidence_schema_unavailable".into())
}

pub(crate) fn timestamp() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

impl TelemetryStore {
    fn evidence_write<T>(&self, wait: Duration, work: impl FnOnce(&mut Connection) -> Result<T, String>) -> Result<T, String> {
        let wait = wait.min(Duration::from_millis(10));
        if let Some(path) = self.read_database_path() {
            let mut conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX)
                .map_err(|_| "evidence_unavailable".to_string())?;
            conn.busy_timeout(wait).map_err(|_| "evidence_unavailable".to_string())?;
            return work(&mut conn);
        }
        let mut conn = self.conn.try_lock().map_err(|_| "evidence_unavailable".to_string())?;
        let timeout = RestoreBusyTimeout::new(&mut conn).map_err(|_| "evidence_unavailable".to_string())?;
        timeout.conn.busy_timeout(wait).map_err(|_| "evidence_unavailable".to_string())?;
        let result = work(&mut *timeout.conn);
        let restored = timeout.restore().map_err(|_| "evidence_unavailable".to_string());
        match (result, restored) { (Ok(value), Ok(())) => Ok(value), (Err(error), _) | (_, Err(error)) => Err(error) }
    }

    pub(crate) fn evidence_context_insert(&self, context: &StoredContext, budget: &RecallBudget) -> Result<(), String> {
        let bindings = serde_json::to_string(&context.bindings).map_err(|_| "evidence_unavailable".to_string())?;
        if bindings.len() > MAX_CONTEXT_BYTES { return Err("too_many_sources".into()); }
        let scope = serde_json::to_string(&context.scope).map_err(|_| "evidence_unavailable".to_string())?;
        self.evidence_write(budget.remaining()?, |conn| {
            budget.remaining()?;
            let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate).map_err(|_| "evidence_unavailable".to_string())?;
            tx.busy_timeout(Duration::ZERO).map_err(|_| "evidence_unavailable".to_string())?;
            tx.execute("INSERT INTO memory_evidence_contexts(token_hash,request_id,created_at,expires_at,used_chars,bindings_json,scope_json,cursor_secret) VALUES(?1,?2,?3,?4,0,?5,?6,?7)",
                params![context.token_hash,context.request_id,timestamp(),context.expires_at,bindings,scope,context.cursor_secret])
                .map_err(|_| "evidence_unavailable".to_string())?;
            tx.execute("DELETE FROM memory_evidence_contexts WHERE token_hash IN (SELECT token_hash FROM memory_evidence_contexts WHERE expires_at <= ?1 ORDER BY expires_at LIMIT 50)", [timestamp()])
                .map_err(|_| "evidence_unavailable".to_string())?;
            tx.execute("DELETE FROM memory_evidence_contexts WHERE token_hash IN (SELECT token_hash FROM memory_evidence_contexts ORDER BY created_at DESC,token_hash DESC LIMIT 50 OFFSET 1000)", [])
                .map_err(|_| "evidence_unavailable".to_string())?;
            let retained: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM memory_evidence_contexts WHERE token_hash=?1)",
                [&context.token_hash], |row| row.get(0)).map_err(|_| "evidence_unavailable".to_string())?;
            if !retained { return Err("evidence_unavailable".into()); }
            budget.remaining()?;
            tx.commit().map_err(|_| "evidence_unavailable".into())
        })
    }

    pub(crate) fn evidence_context_reserve(&self, token_hash: &str, requested: usize, budget: &RecallBudget) -> Result<ReservedContext, String> {
        self.evidence_write(budget.remaining()?, |conn| {
            budget.remaining()?;
            let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate).map_err(|_| "unavailable".to_string())?;
            tx.busy_timeout(Duration::ZERO).map_err(|_| "unavailable".to_string())?;
            let row: Option<(String,String,usize,String,String,String)> = tx.query_row(
                "SELECT request_id,expires_at,used_chars,bindings_json,scope_json,cursor_secret FROM memory_evidence_contexts WHERE token_hash=?1",
                [token_hash], |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?,row.get(4)?,row.get(5)?)),
            ).optional().map_err(|_| "unavailable".to_string())?;
            let Some((request_id,expires_at,used_chars,bindings,scope,cursor_secret)) = row else { return Err("invalid_context".into()); };
            if expires_at <= timestamp() { return Err("context_expired".into()); }
            if bindings.len() > MAX_CONTEXT_BYTES || used_chars > TOTAL_CHARS || cursor_secret.len() != 64 {
                return Err("unavailable".into());
            }
            let bindings: Vec<MemoryBinding> = serde_json::from_str(&bindings).map_err(|_| "unavailable".to_string())?;
            let scope: Vec<String> = serde_json::from_str(&scope).map_err(|_| "unavailable".to_string())?;
            let granted = requested.min(TOTAL_CHARS - used_chars);
            if granted == 0 { return Err("budget_exhausted".into()); }
            let changed = tx.execute("UPDATE memory_evidence_contexts SET used_chars=used_chars+?1 WHERE token_hash=?2 AND used_chars=?3 AND used_chars+?1 <= 8000 AND expires_at>?4",
                params![granted,token_hash,used_chars,timestamp()]).map_err(|_| "unavailable".to_string())?;
            if changed != 1 { return Err("budget_exhausted".into()); }
            budget.remaining()?;
            tx.commit().map_err(|_| "unavailable".to_string())?;
            Ok(ReservedContext { context: StoredContext { token_hash: token_hash.into(), request_id,expires_at,
                bindings,scope,cursor_secret }, granted })
        })
    }

    /// Best-effort refund. On contention the reservation remains charged, so
    /// cancellation or a crashed caller can never increase the 8000-char grant.
    pub(crate) fn evidence_context_settle(&self, token_hash: &str, refund: usize) -> usize {
        self.evidence_write(Duration::ZERO, |conn| {
            let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate).map_err(|_| "unavailable".to_string())?;
            tx.busy_timeout(Duration::ZERO).map_err(|_| "unavailable".to_string())?;
            tx.execute("UPDATE memory_evidence_contexts SET used_chars=used_chars-?1 WHERE token_hash=?2 AND used_chars>=?1",
                params![refund,token_hash]).map_err(|_| "unavailable".to_string())?;
            let used: Option<usize> = tx.query_row("SELECT used_chars FROM memory_evidence_contexts WHERE token_hash=?1",[token_hash],|row|row.get(0))
                .optional().map_err(|_| "unavailable".to_string())?;
            tx.commit().map_err(|_| "unavailable".to_string())?;
            Ok(used.map(|used| TOTAL_CHARS.saturating_sub(used)).unwrap_or(0))
        }).unwrap_or(0)
    }
}
