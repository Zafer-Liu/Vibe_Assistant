//! Read-only evidence resolution. The caller owns the read transaction and interrupt watchdog.
use super::types::{EvidenceExcerpt, MemoryBinding, ResolvedMemory, SourceBinding};
use crate::memory_retrieval::{trace, RecallBudget};
use crate::telemetry_store::LocalMemorySnapshot;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

const MAX_MEMORY_BYTES: i64 = 1024 * 1024;
const MAX_PAYLOAD_BYTES: i64 = 64 * 1024;
const MAX_SOURCES: usize = 32;
const MAX_DOMAINS: usize = 256;
const MAX_ANCHORS: usize = 24;
const MAX_ANCHOR_INPUT: usize = 2048;

// Never include a database error, SQL, or a payload in a public error.
fn sql_error(error: rusqlite::Error) -> String {
    match error {
        rusqlite::Error::SqliteFailure(ref code, _)
            if code.code == rusqlite::ErrorCode::OperationInterrupted => "source_interrupted".into(),
        _ => "evidence_sql_error".into(),
    }
}

// Only owned query results cross this closure boundary. Statement/Rows/Row
// borrows are created directly in the calling scope instead.
fn sql<T>(budget: &RecallBudget, action: impl FnOnce() -> rusqlite::Result<T>) -> Result<T, String> {
    budget.remaining()?;
    let result = action();
    budget.remaining()?;
    result.map_err(sql_error)
}

fn digest(value: &impl Serialize) -> Result<String, String> {
    let bytes = serde_json::to_vec(value).map_err(|_| "evidence_encoding_error".to_string())?;
    Ok(format!("{:x}", Sha256::digest(bytes)))
}

#[derive(Deserialize)]
struct Relation {
    #[serde(default)]
    source_event_key: String,
    #[serde(default)]
    source: String,
    #[serde(default)]
    session_id: String,
}

fn parse_relation(payload: &str) -> Result<Relation, String> {
    serde_json::from_str(payload).map_err(|_| "invalid_provenance".into())
}

fn actual_key(key: &str) -> bool {
    !key.trim().is_empty() && !key.starts_with("refinement-restore:") && !key.starts_with("refinement:")
}

/// This helper deliberately has no source-count limit; each caller owns its limit.
pub(crate) fn source_keys(primary: &str, payloads: &[String]) -> Result<Vec<String>, String> {
    let mut keys = Vec::new();
    if actual_key(primary) {
        keys.push(primary.to_string());
    }
    for payload in payloads {
        let relation = parse_relation(payload)?;
        if actual_key(&relation.source_event_key) && !keys.contains(&relation.source_event_key) {
            keys.push(relation.source_event_key);
        }
    }
    Ok(keys)
}

struct Event {
    id: i64,
    key: String,
    source: String,
    session: String,
    occurred_at: Option<String>,
    state: String,
    message_count: i64,
    revision: i64,
    has_text: bool,
}

impl Event {
    fn fingerprint(&self) -> Result<String, String> {
        digest(&("memory-evidence-source-v1", self.id, &self.key, self.revision,
            &self.source, &self.session, &self.occurred_at, &self.state,
            self.message_count, self.has_text))
    }
}

fn event(conn: &Connection, key: &str, budget: &RecallBudget) -> Result<Option<Event>, String> {
    // No conversation text is materialized here. The revision is maintained by schema v5.
    sql(budget, || conn.query_row(
        "SELECT e.id, e.event_key, e.source, e.session_id, e.occurred_at,
                COALESCE(e.conversation_state, 'unavailable'), COALESCE(e.conversation_message_count, 0),
                COALESCE(r.revision, 0),
                e.conversation_text IS NOT NULL AND e.conversation_text != ''
           FROM agent_events e
           LEFT JOIN memory_evidence_source_revisions r ON r.event_key = e.event_key
          WHERE e.event_key = ?1",
        [key], |row| Ok(Event {
            id: row.get(0)?, key: row.get(1)?, source: row.get(2)?, session: row.get(3)?,
            occurred_at: row.get(4)?, state: row.get(5)?, message_count: row.get(6)?,
            revision: row.get(7)?, has_text: row.get(8)?,
        }),
    ).optional())
}

pub(crate) fn binding(
    conn: &Connection, memory_id: &str, budget: &RecallBudget,
) -> Result<Option<ResolvedMemory>, String> {
    let snapshot = sql(budget, || conn.query_row(
        "SELECT id, source_event_key, ordinal,
                CASE WHEN length(CAST(memory AS BLOB)) <= ?2 THEN memory ELSE NULL END,
                memory_type, durability, source, session_id, user_defined,
                event_time, created_at, updated_at, origin_device,
                length(CAST(memory AS BLOB))
           FROM local_memory_items WHERE id = ?1",
        params![memory_id, MAX_MEMORY_BYTES], |row| {
            let bytes: i64 = row.get(13)?;
            Ok((LocalMemorySnapshot {
                id: row.get(0)?, source_event_key: row.get(1)?, ordinal: row.get(2)?,
                memory: row.get::<_, Option<String>>(3)?.unwrap_or_default(),
                memory_type: row.get(4)?, durability: row.get(5)?, source: row.get(6)?,
                session_id: row.get(7)?, user_defined: row.get(8)?, event_time: row.get(9)?,
                created_at: row.get(10)?, updated_at: row.get(11)?,
            }, row.get::<_, Option<String>>(12)?, bytes))
        },
    ).optional())?;
    let Some((memory, origin_device, bytes)) = snapshot else { return Ok(None) };
    if bytes > MAX_MEMORY_BYTES { return Err("memory_too_large".into()); }

    let mut domains = Vec::new();
    {
        budget.remaining()?;
        let prepared = conn.prepare(
            "SELECT DISTINCT domain_id FROM local_memory_domains
              WHERE memory_id = ?1 ORDER BY domain_id LIMIT 257",
        );
        budget.remaining()?;
        let mut statement = prepared.map_err(sql_error)?;
        budget.remaining()?;
        let queried = statement.query([memory_id]);
        budget.remaining()?;
        let mut rows = queried.map_err(sql_error)?;
        loop {
            budget.remaining()?;
            let next = rows.next();
            budget.remaining()?;
            let Some(row) = next.map_err(sql_error)? else { break };
            if domains.len() == MAX_DOMAINS { return Err("too_many_domains".into()); }
            domains.push(sql(budget, || row.get::<_, String>(0))?);
        }
    }

    let mut relations = vec![Relation {
        source_event_key: memory.source_event_key.clone(),
        source: memory.source.clone(), session_id: memory.session_id.clone(),
    }];
    // Canonical, ordered relationship tuples only: old L1 bodies are never retained.
    let mut provenance = Vec::<(String, String, String, String)>::new();
    {
        budget.remaining()?;
        let prepared = conn.prepare(
            "SELECT source_id,
                    CASE WHEN length(CAST(payload AS BLOB)) <= ?2 THEN payload ELSE NULL END,
                    length(CAST(payload AS BLOB))
               FROM memory_refinement_provenance WHERE memory_id = ?1
              ORDER BY source_id LIMIT 33",
        );
        budget.remaining()?;
        let mut statement = prepared.map_err(sql_error)?;
        budget.remaining()?;
        let queried = statement.query(params![memory_id, MAX_PAYLOAD_BYTES]);
        budget.remaining()?;
        let mut rows = queried.map_err(sql_error)?;
        loop {
            budget.remaining()?;
            let next = rows.next();
            budget.remaining()?;
            let Some(row) = next.map_err(sql_error)? else { break };
            if provenance.len() == MAX_SOURCES { return Err("too_many_provenance_rows".into()); }
            let bytes: i64 = sql(budget, || row.get(2))?;
            if bytes > MAX_PAYLOAD_BYTES { return Err("provenance_too_large".into()); }
            let source_id: String = sql(budget, || row.get(0))?;
            let payload: String = sql(budget, || row.get(1))?;
            let relation = parse_relation(&payload)?;
            budget.remaining()?;
            provenance.push((source_id, relation.source_event_key.clone(),
                relation.source.clone(), relation.session_id.clone()));
            relations.push(relation);
        }
    }
    let mut keys = Vec::new();
    for relation in &relations {
        budget.remaining()?;
        if actual_key(&relation.source_event_key) && !keys.contains(&relation.source_event_key) {
            if keys.len() == MAX_SOURCES { return Err("too_many_sources".into()); }
            keys.push(relation.source_event_key.clone());
        }
    }

    let remote = origin_device.is_some();
    let mut sources = Vec::new();
    let mut has_local_row = false;
    for key in keys {
        budget.remaining()?;
        let mut expected = None;
        for relation in &relations {
            budget.remaining()?;
            if relation.source_event_key == key {
                expected = Some(relation);
                break;
            }
        }
        let expected = expected.ok_or("invalid_provenance")?;
        let current = event(conn, &key, budget)?;
        let mut source = SourceBinding {
            source_event_key: key.clone(), source_agent: expected.source.clone(),
            session_id: expected.session_id.clone(), occurred_at: None,
            source_fingerprint: None,
            status: if remote { "remote_source_missing" } else { "missing_source" }.into(),
        };
        if let Some(current) = current {
            has_local_row = true;
            source.occurred_at = current.occurred_at.clone();
            source.source_fingerprint = Some(current.fingerprint()?);
            let mut identity_matches = true;
            // Conflicting relationships for the same event must not be silently discarded.
            for relation in &relations {
                budget.remaining()?;
                if relation.source_event_key == key
                    && (relation.source != current.source || relation.session_id != current.session)
                {
                    identity_matches = false;
                }
            }
            source.status = if !identity_matches { "source_identity_mismatch" }
                else if current.state == "full" && current.has_text { "available" }
                else { "source_unavailable" }.into();
        }
        sources.push(source);
    }
    let manual_no_source = memory.user_defined && !remote && !has_local_row;
    if manual_no_source {
        for source in &mut sources {
            budget.remaining()?;
            source.status = "manual_no_source".into();
        }
    }
    if sources.is_empty() {
        sources.push(SourceBinding {
            source_event_key: String::new(), source_agent: memory.source.clone(),
            session_id: memory.session_id.clone(), occurred_at: None, source_fingerprint: None,
            status: if remote { "remote_source_missing" } else if manual_no_source {
                "manual_no_source"
            } else { "missing_source" }.into(),
        });
    }
    budget.remaining()?;
    let memory_fingerprint = trace::fingerprint(&memory, &domains);
    let provenance_fingerprint = digest(&("memory-evidence-provenance-v1",
        (&memory.id, &memory.source_event_key, &memory.source, &memory.session_id, &origin_device), provenance))?;
    budget.remaining()?;
    Ok(Some(ResolvedMemory {
        binding: MemoryBinding {
            memory_id: memory.id, memory_fingerprint, domains, provenance_fingerprint,
            kind: if remote { "remote" } else if manual_no_source { "manual_no_source" }
                else { "local" }.into(),
            sources,
        },
        memory_text: memory.memory,
    }))
}

fn is_cjk(ch: char) -> bool {
    matches!(ch as u32, 0x3400..=0x4dbf | 0x4e00..=0x9fff | 0x20000..=0x323af)
}

fn push_anchor(anchors: &mut Vec<String>, anchor: String, limit: usize) {
    if anchors.len() < limit && !anchors.contains(&anchor) { anchors.push(anchor); }
}

fn anchors(query: &str, memory: &str, budget: &RecallBudget) -> Result<Vec<String>, String> {
    let mut result = Vec::new();
    for (index, text) in [query, memory].iter().enumerate() {
        budget.remaining()?;
        let mut chars = Vec::new();
        for ch in text.chars().take(MAX_ANCHOR_INPUT) {
            budget.remaining()?;
            chars.push(ch);
        }
        // Reserve half the probes for L1, even when the query has many words.
        let limit = if index == 0 { MAX_ANCHORS / 2 } else { MAX_ANCHORS };
        let mut start = 0;
        while start < chars.len() && result.len() < limit {
            budget.remaining()?;
            let mut end = start;
            if chars[start].is_ascii_alphabetic() || chars[start] == '_' {
                while end < chars.len() && (chars[end].is_ascii_alphanumeric() || chars[end] == '_') {
                    budget.remaining()?;
                    end += 1;
                }
                if (3..=64).contains(&(end - start)) {
                    push_anchor(&mut result, chars[start..end].iter().collect(), limit);
                }
            } else if is_cjk(chars[start]) {
                while end < chars.len() && is_cjk(chars[end]) {
                    budget.remaining()?;
                    end += 1;
                }
                // Longer fragments first; each probe remains a lexical fragment, not a sentence.
                for size in [4, 3, 2] {
                    budget.remaining()?;
                    for position in start..end.saturating_sub(size - 1) {
                        budget.remaining()?;
                        if result.len() >= limit { break; }
                        push_anchor(&mut result, chars[position..position + size].iter().collect(), limit);
                    }
                    if result.len() >= limit { break; }
                }
            }
            start = end.max(start + 1);
        }
    }
    Ok(result)
}

pub(crate) fn excerpt(
    conn: &Connection, source: &SourceBinding, query: &str, memory_text: &str,
    offset: Option<usize>, max_chars: usize, budget: &RecallBudget,
) -> Result<EvidenceExcerpt, String> {
    budget.remaining()?;
    let current = event(conn, &source.source_event_key, budget)?.ok_or("source_changed")?;
    let current_fingerprint = current.fingerprint()?;
    budget.remaining()?;
    if source.source_fingerprint.as_deref() != Some(current_fingerprint.as_str()) {
        return Err("source_changed".into());
    }
    if source.status != "available" || current.state != "full" || !current.has_text {
        return Err("source_unavailable".into());
    }
    if source.source_agent != current.source || source.session_id != current.session {
        return Err("source_identity_mismatch".into());
    }
    let total: i64 = sql(budget, || conn.query_row(
        "SELECT length(conversation_text) FROM agent_events WHERE event_key = ?1",
        [&source.source_event_key], |row| row.get(0),
    ))?;
    let total_chars = usize::try_from(total).map_err(|_| "invalid_source_length".to_string())?;
    let max_chars = max_chars.clamp(1, 8000);
    let (start, method) = if let Some(offset) = offset {
        if offset > total_chars { return Err("invalid_offset".into()); }
        (offset, "continuation")
    } else {
        let mut matched = None;
        for anchor in anchors(query, memory_text, budget)? {
            budget.remaining()?;
            let position: i64 = sql(budget, || conn.query_row(
                "SELECT instr(lower(conversation_text), lower(?2)) FROM agent_events WHERE event_key = ?1",
                params![source.source_event_key, anchor], |row| row.get(0),
            ))?;
            if position > 0 {
                let position = usize::try_from(position - 1).map_err(|_| "invalid_source_length".to_string())?;
                matched = Some(position.saturating_sub(max_chars / 3));
                break;
            }
        }
        match matched { Some(start) => (start, "lexical"), None => (0, "prefix_fallback") }
    };
    let count = max_chars.min(total_chars.saturating_sub(start));
    let sqlite_start = i64::try_from(start).ok().and_then(|n| n.checked_add(1))
        .ok_or("invalid_offset")?;
    let text: String = sql(budget, || conn.query_row(
        "SELECT substr(conversation_text, ?2, ?3) FROM agent_events WHERE event_key = ?1",
        params![source.source_event_key, sqlite_start, count as i64], |row| row.get(0),
    ))?;
    let mut returned_chars = 0;
    for _ in text.chars() {
        budget.remaining()?;
        returned_chars += 1;
    }
    let excerpt_fingerprint = format!("{:x}", Sha256::digest(text.as_bytes()));
    budget.remaining()?;
    Ok(EvidenceExcerpt {
        text, char_start: start, char_end: start + returned_chars, total_chars,
        match_method: method.into(), excerpt_fingerprint,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn budget() -> RecallBudget { RecallBudget::new(Duration::from_secs(30)) }

    fn fixture() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE local_memory_items (
                id TEXT PRIMARY KEY, source_event_key TEXT, ordinal INTEGER, memory TEXT,
                memory_type TEXT, durability TEXT, source TEXT, session_id TEXT,
                user_defined INTEGER, event_time TEXT, created_at TEXT, updated_at TEXT,
                origin_device TEXT);
             CREATE TABLE local_memory_domains (memory_id TEXT, domain_id TEXT);
             CREATE TABLE memory_refinement_provenance (memory_id TEXT, source_id TEXT, payload TEXT,
                PRIMARY KEY(memory_id, source_id));
             CREATE TABLE agent_events (id INTEGER PRIMARY KEY, event_key TEXT UNIQUE,
                source TEXT, session_id TEXT, occurred_at TEXT, conversation_state TEXT,
                conversation_message_count INTEGER, conversation_text TEXT);
             CREATE TABLE memory_evidence_source_revisions (event_key TEXT PRIMARY KEY, revision INTEGER);
             INSERT INTO local_memory_items VALUES
                ('m', 'original', 0, 'L1 brief', 'fact', 'long_term', 'client', 'session',
                 0, NULL, 'created', 'updated', NULL);
             INSERT INTO agent_events VALUES
                (1, 'original', 'client', 'session', 'time', 'full', 2, 'original transcript');",
        ).unwrap();
        conn
    }

    fn payload(key: &str) -> String {
        serde_json::json!({"source_event_key": key, "source": "client", "session_id": "session",
            "memory": "old body must not affect provenance fingerprint"}).to_string()
    }

    fn resolve(conn: &Connection) -> ResolvedMemory {
        binding(conn, "m", &budget()).unwrap().unwrap()
    }

    #[test]
    fn original_merged_restored_keys_and_invalid_payloads() {
        assert_eq!(source_keys("original", &[payload("other"), payload("original"),
            payload("refinement-restore:uuid"), payload(" ")]).unwrap(), vec!["original", "other"]);
        assert_eq!(source_keys("refinement-restore:uuid", &[payload("original")]).unwrap(), vec!["original"]);
        assert_eq!(source_keys("unknown-prefix:keep", &[]).unwrap(), vec!["unknown-prefix:keep"]);
        assert_eq!(source_keys("", &["{broken".into()]).err().as_deref(), Some("invalid_provenance"));
        let many: Vec<_> = (0..40).map(|i| payload(&format!("key-{i}"))).collect();
        assert_eq!(source_keys("", &many).unwrap().len(), 40);
    }

    #[test]
    fn restored_memory_uses_real_provenance_and_canonical_metadata() {
        let conn = fixture();
        conn.execute("UPDATE local_memory_items SET source_event_key = 'refinement-restore:uuid'", []).unwrap();
        conn.execute("INSERT INTO memory_refinement_provenance VALUES ('m', 'old', ?1)", [payload("original")]).unwrap();
        conn.execute_batch("INSERT INTO local_memory_domains VALUES ('m','z'), ('m','a'), ('m','a');").unwrap();
        let first = resolve(&conn);
        assert_eq!(first.binding.sources.len(), 1);
        assert_eq!(first.binding.sources[0].status, "available");
        assert_eq!(first.binding.domains, vec!["a", "z"]);
        let changed = serde_json::json!({"memory": "different body", "session_id": "session",
            "source": "client", "source_event_key": "original"}).to_string();
        conn.execute("UPDATE memory_refinement_provenance SET payload = ?1", [changed]).unwrap();
        assert_eq!(first.binding.provenance_fingerprint, resolve(&conn).binding.provenance_fingerprint);
        conn.execute("UPDATE memory_refinement_provenance SET payload = ?1", [payload("other")]).unwrap();
        assert_ne!(first.binding.provenance_fingerprint, resolve(&conn).binding.provenance_fingerprint);
    }

    #[test]
    fn merged_sources_survive_missing_primary_and_enforce_identity() {
        let conn = fixture();
        conn.execute("UPDATE local_memory_items SET source_event_key = 'missing'", []).unwrap();
        conn.execute("INSERT INTO memory_refinement_provenance VALUES ('m', 'old', ?1)", [payload("original")]).unwrap();
        let resolved = resolve(&conn);
        assert_eq!(resolved.binding.sources[0].status, "missing_source");
        assert_eq!(resolved.binding.sources[1].status, "available");
        conn.execute("UPDATE agent_events SET session_id = 'wrong'", []).unwrap();
        assert_eq!(resolve(&conn).binding.sources[1].status, "source_identity_mismatch");
        conn.execute("UPDATE agent_events SET session_id = 'session', conversation_state = 'partial'", []).unwrap();
        assert_eq!(resolve(&conn).binding.sources[1].status, "source_unavailable");
    }

    #[test]
    fn english_and_chinese_middle_windows_use_unicode_offsets() {
        for (query, middle) in [("check target_identifier please", "target_identifier evidence"),
            ("请核对数据库迁移的处理", "数据库迁移过程中的真实证据")] {
            let conn = fixture();
            let text = format!("{}{}{}", "前🙂".repeat(500), middle, "后".repeat(500));
            conn.execute("UPDATE agent_events SET conversation_text = ?1", [&text]).unwrap();
            let resolved = resolve(&conn);
            let excerpt = excerpt(&conn, &resolved.binding.sources[0], query, "", None, 120, &budget()).unwrap();
            assert_eq!(excerpt.match_method, "lexical");
            assert!(excerpt.char_start > 500);
            assert!(excerpt.text.contains(if query.is_ascii() { "target_identifier" } else { "数据库迁移" }));
            assert_eq!(excerpt.total_chars, text.chars().count());
            assert_eq!(excerpt.text, text.chars().skip(excerpt.char_start).take(120).collect::<String>());
            assert_eq!(excerpt.char_end - excerpt.char_start, excerpt.text.chars().count());
            assert_eq!(excerpt.excerpt_fingerprint, format!("{:x}", Sha256::digest(excerpt.text.as_bytes())));
        }
    }

    #[test]
    fn fallback_continuation_and_window_limits() {
        let conn = fixture();
        let text = "🙂甲乙丙丁戊己庚辛壬癸".repeat(900);
        conn.execute("UPDATE agent_events SET conversation_text = ?1", [&text]).unwrap();
        let resolved = resolve(&conn);
        let source = &resolved.binding.sources[0];
        let first = excerpt(&conn, source, "absent_identifier", "", None, 7, &budget()).unwrap();
        assert_eq!(first.match_method, "prefix_fallback");
        assert_eq!(first.char_start, 0);
        let next = excerpt(&conn, source, "", "", Some(first.char_end), 7, &budget()).unwrap();
        assert_eq!(next.match_method, "continuation");
        assert_eq!(next.text, text.chars().skip(7).take(7).collect::<String>());
        assert_eq!(excerpt(&conn, source, "", "", None, 0, &budget()).unwrap().text.chars().count(), 1);
        assert_eq!(excerpt(&conn, source, "", "", None, usize::MAX, &budget()).unwrap().text.chars().count(), 8000);
        let end = text.chars().count();
        let terminal = excerpt(&conn, source, "", "", Some(end), 7, &budget()).unwrap();
        assert!(terminal.text.is_empty());
        assert_eq!(terminal.char_start, terminal.char_end);
        assert_eq!(excerpt(&conn, source, "", "", Some(end + 1), 7, &budget()).err().as_deref(), Some("invalid_offset"));
    }

    #[test]
    fn l1_anchors_and_revision_or_metadata_changes() {
        let conn = fixture();
        conn.execute("UPDATE agent_events SET conversation_text = ?1", [format!("{}target_identifier", "前".repeat(500))]).unwrap();
        let resolved = resolve(&conn);
        let source = &resolved.binding.sources[0];
        assert_eq!(excerpt(&conn, source, "unmatched", "remember target_identifier", None, 80, &budget()).unwrap().match_method, "lexical");
        conn.execute("INSERT INTO memory_evidence_source_revisions VALUES ('original', 1)", []).unwrap();
        assert_eq!(excerpt(&conn, source, "", "", None, 80, &budget()).err().as_deref(), Some("source_changed"));
        let refreshed = resolve(&conn);
        conn.execute("UPDATE agent_events SET conversation_message_count = 3", []).unwrap();
        assert_eq!(excerpt(&conn, &refreshed.binding.sources[0], "", "", None, 80, &budget()).err().as_deref(), Some("source_changed"));
    }

    #[test]
    fn manual_remote_missing_and_expired_budget() {
        let conn = fixture();
        conn.execute("DELETE FROM agent_events", []).unwrap();
        assert_eq!(resolve(&conn).binding.sources[0].status, "missing_source");
        conn.execute("UPDATE local_memory_items SET user_defined = 1", []).unwrap();
        assert_eq!(resolve(&conn).binding.kind, "manual_no_source");
        assert_eq!(resolve(&conn).binding.sources[0].status, "manual_no_source");
        conn.execute("UPDATE local_memory_items SET origin_device = 'peer'", []).unwrap();
        let remote = resolve(&conn);
        assert_eq!(remote.binding.kind, "remote");
        assert_eq!(remote.binding.sources[0].status, "remote_source_missing");
        let expired = RecallBudget::new(Duration::ZERO);
        assert_eq!(binding(&conn, "m", &expired).err().as_deref(), Some("recall_deadline"));
        assert_eq!(excerpt(&conn, &remote.binding.sources[0], "", "", None, 1, &expired).err().as_deref(), Some("recall_deadline"));
        let cancelled = budget();
        cancelled.cancel();
        assert_eq!(binding(&conn, "m", &cancelled).err().as_deref(), Some("recall_cancelled"));
    }

    #[test]
    fn provenance_limits_fail_without_truncating_relationships() {
        let conn = fixture();
        for i in 0..32 {
            conn.execute("INSERT INTO memory_refinement_provenance VALUES ('m', ?1, ?2)",
                params![format!("source-{i:02}"), payload(&format!("event-{i}"))]).unwrap();
        }
        assert_eq!(binding(&conn, "m", &budget()).err().as_deref(), Some("too_many_sources"));
        conn.execute("UPDATE local_memory_items SET source_event_key = 'refinement-restore:uuid'", []).unwrap();
        assert_eq!(resolve(&conn).binding.sources.len(), 32);
        conn.execute("INSERT INTO memory_refinement_provenance VALUES ('m', 'source-33', ?1)", [payload("extra")]).unwrap();
        assert_eq!(binding(&conn, "m", &budget()).err().as_deref(), Some("too_many_provenance_rows"));
        conn.execute("DELETE FROM memory_refinement_provenance", []).unwrap();
        conn.execute("INSERT INTO memory_refinement_provenance VALUES ('m', 'oversize', ?1)", ["x".repeat(64 * 1024 + 1)]).unwrap();
        assert_eq!(binding(&conn, "m", &budget()).err().as_deref(), Some("provenance_too_large"));
        conn.execute("DELETE FROM memory_refinement_provenance", []).unwrap();
        conn.execute("UPDATE local_memory_items SET memory = ?1", ["x".repeat(1024 * 1024 + 1)]).unwrap();
        assert_eq!(binding(&conn, "m", &budget()).err().as_deref(), Some("memory_too_large"));
    }
}
