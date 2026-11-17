use super::*;
use rusqlite::params;

const CUTOFF: &str = "2026-11-07T12:00:00.000Z";
const NOW: &str = "2026-11-07T12:05:00.000Z";
const EARLY: &str = "2026-11-07T12:01:00.000Z";
const LATE: &str = "2026-11-07T12:04:00.000Z";

// A retained v1 ledger, without projections. Keep these fixtures independent of
// schema::initialize_database so the backfill can be exercised before v2 exists.
fn legacy_schema(conn: &Connection) {
    conn.execute_batch(
        "CREATE TABLE agent_events (
             id INTEGER PRIMARY KEY AUTOINCREMENT, event_key TEXT NOT NULL UNIQUE,
             source TEXT NOT NULL, session_id TEXT NOT NULL, event_type TEXT NOT NULL,
             occurred_at TEXT NOT NULL, payload_json TEXT NOT NULL,
             conversation_state TEXT NOT NULL DEFAULT 'unavailable',
             conversation_text TEXT, l1_state TEXT NOT NULL DEFAULT 'unavailable'
         );
         CREATE INDEX idx_agent_events_occurred ON agent_events(occurred_at);
         CREATE TABLE session_token_usage (
             source TEXT NOT NULL, session_id TEXT NOT NULL,
             input_tokens INTEGER NOT NULL DEFAULT 0, output_tokens INTEGER NOT NULL DEFAULT 0,
             cached_tokens INTEGER NOT NULL DEFAULT 0, origin TEXT NOT NULL, refreshed_at TEXT NOT NULL,
             PRIMARY KEY(source, session_id)
         );
         CREATE TABLE usage_records (
             record_id TEXT PRIMARY KEY, source TEXT NOT NULL, session_id TEXT NOT NULL,
             occurred_at TEXT NOT NULL, model TEXT, record_kind TEXT NOT NULL,
             input_tokens INTEGER NOT NULL DEFAULT 0, output_tokens INTEGER NOT NULL DEFAULT 0,
             cached_tokens INTEGER NOT NULL DEFAULT 0, origin TEXT NOT NULL,
             provider_id TEXT, gateway TEXT, route TEXT, cost_usd REAL, pricing_source TEXT
         );
         CREATE INDEX idx_usage_records_session ON usage_records(source, session_id);
         CREATE TABLE usage_remote_totals (
             device TEXT NOT NULL, day TEXT NOT NULL, source TEXT NOT NULL,
             input_tokens INTEGER NOT NULL DEFAULT 0, output_tokens INTEGER NOT NULL DEFAULT 0,
             cached_tokens INTEGER NOT NULL DEFAULT 0, record_count INTEGER NOT NULL DEFAULT 0,
             PRIMARY KEY(device, day, source)
         );
         CREATE TABLE transcript_scan_states (
             source TEXT NOT NULL, transcript_path TEXT NOT NULL, state TEXT NOT NULL,
             PRIMARY KEY(source, transcript_path)
         );
         CREATE TABLE app_settings (setting_key TEXT PRIMARY KEY, value_json TEXT, updated_at TEXT);
         CREATE TABLE mcp_access_logs (id INTEGER PRIMARY KEY, detail TEXT);
         CREATE TABLE mcp_call_observations (observation_key TEXT PRIMARY KEY, status TEXT);",
    ).unwrap();
    super::super::cloud_usage::initialize(conn).unwrap();
}

fn connection() -> Connection {
    let conn = Connection::open_in_memory().unwrap();
    legacy_schema(&conn);
    conn.execute_batch("BEGIN IMMEDIATE").unwrap();
    initialize(&conn).unwrap();
    conn.execute_batch("COMMIT; PRAGMA recursive_triggers=OFF").unwrap();
    conn
}

fn event(conn: &Connection, key: &str, source: &str, session: &str, kind: &str, at: &str) -> i64 {
    conn.execute(
        "INSERT INTO agent_events(event_key, source, session_id, event_type, occurred_at, payload_json)
         VALUES (?1, ?2, ?3, ?4, ?5, '{}')",
        params![key, source, session, kind, at],
    ).unwrap();
    conn.last_insert_rowid()
}

fn revision(conn: &Connection) -> i64 {
    conn.query_row("SELECT revision FROM app_data_revisions WHERE domain = 'usage'", [], |row| row.get(0)).unwrap()
}

fn mutation(conn: &Connection, sql: &str, invalidates_usage: bool) {
    let before = revision(conn);
    conn.execute_batch(sql).unwrap();
    if invalidates_usage {
        assert!(revision(conn) > before, "missing usage invalidation: {sql}");
    } else {
        assert_eq!(revision(conn), before, "unrelated invalidation: {sql}");
    }
    assert_projections(conn);
}

// The original full-history queries are the oracle. This deliberately retains
// the window and independent counts that the production projection replaces.
fn reference_status(conn: &Connection) -> TelemetryLiveStatus {
    let (mut status, sources) = conn.query_row(
        "WITH sessions AS (
             SELECT DISTINCT source, session_id FROM agent_events WHERE session_id != 'unknown'
             UNION SELECT source, session_id FROM session_token_usage
         ), ranked AS (
             SELECT source, session_id, event_type, occurred_at,
                    ROW_NUMBER() OVER (PARTITION BY source, session_id ORDER BY id DESC) AS rank
               FROM agent_events WHERE session_id != 'unknown'
         ), active AS (
             SELECT source FROM ranked WHERE rank = 1 AND occurred_at >= ?1 AND occurred_at <= ?2
                AND event_type NOT IN ('Stop', 'TranscriptSync', 'session_end', 'session_usage')
         ) SELECT (SELECT COUNT(*) FROM sessions), (SELECT COUNT(*) FROM active),
             (SELECT COUNT(*) FROM agent_events WHERE conversation_state = 'full'),
             (SELECT COUNT(*) FROM agent_events WHERE conversation_state = 'full' AND l1_state = 'stored'
                AND conversation_text IS NOT NULL AND conversation_text != ''),
             (SELECT COUNT(*) FROM agent_events WHERE conversation_state = 'full' AND l1_state = 'pending'
                AND conversation_text IS NOT NULL AND conversation_text != ''),
             (SELECT COUNT(*) FROM agent_events WHERE conversation_state = 'full' AND l1_state = 'retrying'
                AND conversation_text IS NOT NULL AND conversation_text != ''),
             (SELECT COUNT(*) FROM agent_events WHERE conversation_state = 'full' AND l1_state = 'failed'
                AND conversation_text IS NOT NULL AND conversation_text != ''),
             (SELECT COUNT(*) FROM sessions s LEFT JOIN session_token_usage u
                ON u.source = s.source AND u.session_id = s.session_id WHERE u.session_id IS NULL),
             (SELECT COUNT(*) FROM transcript_scan_states WHERE state = 'failed'),
             (SELECT MAX(occurred_at) FROM agent_events),
             (SELECT json_group_array(source) FROM (SELECT DISTINCT source FROM active ORDER BY source))",
        [CUTOFF, NOW],
        |row| Ok((TelemetryLiveStatus {
            captured_sessions: row.get(0)?, active_sessions: row.get(1)?, active_sources: Vec::new(),
            completed_conversations: row.get(2)?, organized_memory_conversations: row.get(3)?,
            pending_memory_sessions: row.get(4)?, retrying_memory_sessions: row.get(5)?,
            failed_memory_sessions: row.get(6)?, pending_usage_sessions: row.get(7)?,
            failed_transcript_scans: row.get(8)?, last_event_at: row.get(9)?,
        }, row.get::<_, String>(10)?)),
    ).unwrap();
    status.active_sources = serde_json::from_str(&sources).unwrap();
    status
}

fn assert_projections(conn: &Connection) {
    let actual = live_status(conn, CUTOFF, NOW).unwrap();
    assert_eq!(serde_json::to_value(actual).unwrap(), serde_json::to_value(reference_status(conn)).unwrap());
    let actual_total: i64 = conn.query_row("SELECT total_events FROM event_counts WHERE singleton = 1", [], |row| row.get(0)).unwrap();
    let expected_total: i64 = conn.query_row("SELECT COUNT(*) FROM agent_events", [], |row| row.get(0)).unwrap();
    assert_eq!(actual_total, expected_total);
    let read_latest = |sql: &str| {
        let mut statement = conn.prepare(sql).unwrap();
        let rows = statement.query_map([], |row| Ok((
            row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, i64>(2)?,
            row.get::<_, String>(3)?, row.get::<_, String>(4)?, row.get::<_, String>(5)?,
        ))).unwrap();
        rows.collect::<Result<Vec<_>, _>>().unwrap()
    };
    assert_eq!(
        read_latest("SELECT source, session_id, event_id, event_type, occurred_at, max_occurred_at
                       FROM session_latest_events ORDER BY source, session_id"),
        read_latest("WITH ranked AS (
                         SELECT *, ROW_NUMBER() OVER (PARTITION BY source, session_id ORDER BY id DESC) AS rank,
                                MAX(occurred_at) OVER (PARTITION BY source, session_id) AS max_time FROM agent_events
                     ) SELECT source, session_id, id, event_type, occurred_at, max_time
                         FROM ranked WHERE rank = 1 ORDER BY source, session_id"),
    );
}

#[test]
fn backfill_retains_history_and_is_atomic_and_idempotent() {
    let conn = Connection::open_in_memory().unwrap();
    legacy_schema(&conn);
    event(&conn, "first", "claude", "session", "Prompt", LATE);
    event(&conn, "second", "claude", "session", "Stop", EARLY);
    conn.execute_batch("UPDATE agent_events SET conversation_state = 'full', l1_state = 'pending', conversation_text = 'retained'
                        WHERE event_key = 'second'; BEGIN IMMEDIATE;").unwrap();
    initialize(&conn).unwrap();
    assert_projections(&conn);
    conn.execute_batch("ROLLBACK").unwrap();
    let projections: i64 = conn.query_row("SELECT COUNT(*) FROM sqlite_schema WHERE name = 'event_counts'", [], |row| row.get(0)).unwrap();
    assert_eq!(projections, 0);
    let retained: i64 = conn.query_row("SELECT COUNT(*) FROM agent_events", [], |row| row.get(0)).unwrap();
    assert_eq!(retained, 2);
    conn.execute_batch("BEGIN IMMEDIATE").unwrap();
    initialize(&conn).unwrap();
    conn.execute_batch("COMMIT").unwrap();
    let before = revision(&conn);
    initialize(&conn).unwrap();
    assert_eq!(revision(&conn), before);
    assert_projections(&conn);
    let text: String = conn.query_row("SELECT conversation_text FROM agent_events WHERE event_key = 'second'", [], |row| row.get(0)).unwrap();
    assert_eq!(text, "retained");
}

#[test]
fn live_status_preserves_id_order_time_boundaries_sources_and_all_counters() {
    let conn = connection();
    event(&conn, "live", "claude", "live", "Prompt", CUTOFF);
    event(&conn, "qoder-stop", "qoder", "live", "Stop", LATE);
    event(&conn, "qoder-prompt", "qoder", "live", "Prompt", EARLY);
    event(&conn, "stopped-prompt", "claude", "stopped", "Prompt", LATE);
    event(&conn, "stopped-stop", "claude", "stopped", "Stop", "2026-11-07T11:00:00.000Z");
    event(&conn, "future", "codex", "future", "Prompt", "2026-11-07T13:00:00.000Z");
    event(&conn, "unknown", "qoder", "unknown", "Prompt", "2026-11-07T14:00:00.000Z");
    for (index, kind) in ["Stop", "TranscriptSync", "session_end", "session_usage"].iter().enumerate() {
        event(&conn, &format!("excluded-{index}"), "codex", &format!("excluded-{index}"), kind, NOW);
    }
    conn.execute_batch(
        "INSERT INTO session_token_usage VALUES
             ('codex', 'usage-only', 100, 20, 0, 'native_transcript', '2026-11-07T12:04:00.000Z'),
             ('qoder', 'unknown', 100, 20, 0, 'native_transcript', '2026-11-07T12:04:00.000Z');
         INSERT INTO transcript_scan_states VALUES ('codex', 'failed', 'failed'), ('codex', 'ok', 'ready');
         UPDATE agent_events SET conversation_state = 'full', conversation_text = 'text', l1_state = 'stored' WHERE event_key = 'live';
         UPDATE agent_events SET conversation_state = 'full', conversation_text = ' ', l1_state = 'pending' WHERE event_key = 'qoder-prompt';
         UPDATE agent_events SET conversation_state = 'full', conversation_text = 'text', l1_state = 'retrying' WHERE event_key = 'qoder-stop';
         UPDATE agent_events SET conversation_state = 'full', conversation_text = 'text', l1_state = 'failed' WHERE event_key = 'stopped-stop';
         UPDATE agent_events SET conversation_state = 'full', conversation_text = '', l1_state = 'pending' WHERE event_key = 'future';
         UPDATE agent_events SET conversation_state = 'full', conversation_text = NULL, l1_state = 'pending' WHERE event_key = 'unknown';",
    ).unwrap();
    assert_projections(&conn);
    let status = live_status(&conn, CUTOFF, NOW).unwrap();
    assert_eq!(status.active_sessions, 2);
    assert_eq!(status.active_sources, vec!["claude", "qoder"]);
    assert_eq!(status.completed_conversations, 6);
    assert_eq!(status.organized_memory_conversations, 1);
    assert_eq!(status.pending_memory_sessions, 1);
    assert_eq!(status.retrying_memory_sessions, 1);
    assert_eq!(status.failed_memory_sessions, 1);
    assert_eq!(status.failed_transcript_scans, 1);
    assert_eq!(status.last_event_at.as_deref(), Some("2026-11-07T14:00:00.000Z"));
    conn.execute("UPDATE agent_events SET occurred_at = ?1 WHERE event_key = 'live'", [NOW]).unwrap();
    assert_projections(&conn);
    assert_eq!(live_status(&conn, CUTOFF, NOW).unwrap().active_sessions, 2);
}

#[test]
fn event_updates_ignored_writes_replacements_and_deletes_keep_projections_exact() {
    for recursive in [false, true] {
        let conn = connection();
        conn.pragma_update(None, "recursive_triggers", recursive).unwrap();
        event(&conn, "survivor", "claude", "old", "Prompt", LATE);
        let replaced = event(&conn, "replace", "claude", "old", "Stop", EARLY);
        mutation(&conn, "UPDATE agent_events SET conversation_state = 'full', l1_state = 'pending', conversation_text = 'text' WHERE event_key = 'replace'", false);
        for state in ["stored", "retrying", "failed", "pending"] {
            mutation(&conn, &format!("UPDATE agent_events SET l1_state = '{state}' WHERE event_key = 'replace'"), false);
        }
        mutation(&conn, "UPDATE agent_events SET conversation_text = '' WHERE event_key = 'replace'", false);
        mutation(&conn, "UPDATE agent_events SET conversation_text = NULL WHERE event_key = 'replace'", false);
        mutation(&conn, "UPDATE agent_events SET conversation_text = ' ' WHERE event_key = 'replace'", false);
        mutation(&conn, "INSERT OR IGNORE INTO agent_events(event_key, source, session_id, event_type, occurred_at, payload_json)
                         VALUES ('replace', 'wrong', 'wrong', 'Prompt', '2099', '{}')", false);
        mutation(&conn, "INSERT OR REPLACE INTO agent_events(event_key, source, session_id, event_type, occurred_at, payload_json)
                         VALUES ('replace', 'qoder', 'new', 'Prompt', '2026-11-07T12:03:00.000Z', '{}')", false);
        let replacement_id: i64 = conn.query_row("SELECT id FROM agent_events WHERE event_key = 'replace'", [], |row| row.get(0)).unwrap();
        assert!(replacement_id > replaced, "REPLACE must be checked with a different generated id");
        mutation(&conn, "DELETE FROM agent_events WHERE event_key = 'survivor'", false);
        let older = event(&conn, "older", "qoder", "new", "Prompt", NOW);
        mutation(&conn, &format!("UPDATE agent_events SET id = {} WHERE event_key = 'replace'", older + 1), false);
        mutation(&conn, "DELETE FROM agent_events WHERE event_key = 'replace'", false);
        assert_eq!(live_status(&conn, CUTOFF, NOW).unwrap().active_sources, vec!["qoder"]);
        mutation(&conn, "DELETE FROM agent_events", false);
        let empty = live_status(&conn, CUTOFF, NOW).unwrap();
        assert_eq!(empty.captured_sessions, 0);
        assert!(empty.active_sources.is_empty());
        assert!(empty.last_event_at.is_none());
    }
}

#[test]
fn replace_can_displace_two_rows_and_update_replace_can_move_three_sessions() {
    for recursive in [false, true] {
        let conn = connection();
        conn.pragma_update(None, "recursive_triggers", recursive).unwrap();
        let first = event(&conn, "one", "claude", "one", "Prompt", EARLY);
        event(&conn, "two", "qoder", "two", "Stop", LATE);
        mutation(&conn, "UPDATE agent_events SET conversation_state = 'full', l1_state = 'stored', conversation_text = 'text'", false);
        mutation(&conn, &format!(
            "INSERT OR REPLACE INTO agent_events(id, event_key, source, session_id, event_type, occurred_at, payload_json)
             VALUES ({first}, 'two', 'codex', 'merged', 'Prompt', '2026-11-07T12:02:00.000Z', '{{}}')"
        ), false);
        assert_eq!(live_status(&conn, CUTOFF, NOW).unwrap().captured_sessions, 1);
        let target = event(&conn, "target-id", "claude", "id-victim", "Prompt", EARLY);
        event(&conn, "target-key", "qoder", "key-victim", "Prompt", EARLY);
        mutation(&conn, "UPDATE agent_events SET conversation_state = 'full', l1_state = 'failed', conversation_text = 'text'", false);
        mutation(&conn, &format!(
            "UPDATE OR REPLACE agent_events SET id = {target}, event_key = 'target-key',
                 source = 'dsh', session_id = 'moved' WHERE event_key = 'two'"
        ), false);
        assert_eq!(live_status(&conn, CUTOFF, NOW).unwrap().captured_sessions, 1);
    }
}

#[test]
fn automatic_id_does_not_treat_a_real_negative_id_as_a_replace_victim() {
    let conn = connection();
    conn.execute_batch("INSERT INTO agent_events(id, event_key, source, session_id, event_type, occurred_at, payload_json)
                        VALUES (-1, 'negative', 'claude', 'negative', 'Prompt', '2026-11-07T12:01:00.000Z', '{}')").unwrap();
    event(&conn, "auto", "qoder", "auto", "Prompt", LATE);
    assert_projections(&conn);
    assert_eq!(live_status(&conn, CUTOFF, NOW).unwrap().active_sessions, 2);
}

#[test]
fn usage_revision_tracks_semantic_mutations_in_each_usage_table() {
    let conn = connection();
    for (table, insert, update, key_update) in [
        ("usage_records", "INSERT INTO usage_records(record_id, source, session_id, occurred_at, record_kind, origin) VALUES ('record', 'claude', 'session', '2026-11-07T12:01:00.000Z', 'response', 'native_transcript')",
         "UPDATE usage_records SET input_tokens = 100", "UPDATE usage_records SET source = 'qoder', session_id = 'moved', record_id = 'new-record'"),
        ("session_token_usage", "INSERT INTO session_token_usage VALUES ('claude', 'session', 100, 20, 0, 'native_transcript', '2026-11-07T12:01:00.000Z')",
         "UPDATE session_token_usage SET refreshed_at = '2026-11-07T12:04:00.000Z'", "UPDATE session_token_usage SET source = 'qoder', session_id = 'moved'"),
        ("usage_remote_records", "INSERT INTO usage_remote_records VALUES ('peer', 'record', 'claude', 'session', '2026-11-07T12:01:00.000Z', NULL, 'response', 100, 20, 0, 'native_transcript', '2026-11-07T12:01:00.000Z')",
         "UPDATE usage_remote_records SET model = 'model'", "UPDATE usage_remote_records SET device = 'new-peer', record_id = 'new-record', source = 'qoder', session_id = 'moved'"),
        ("usage_remote_ledgers", "INSERT INTO usage_remote_ledgers VALUES ('peer', 1)",
         "UPDATE usage_remote_ledgers SET revision = 2", "UPDATE usage_remote_ledgers SET device = 'new-peer'"),
        ("usage_remote_totals", "INSERT INTO usage_remote_totals VALUES ('peer', '2026-11-07', 'claude', 100, 20, 0, 1)",
         "UPDATE usage_remote_totals SET record_count = 2", "UPDATE usage_remote_totals SET device = 'new-peer', day = '2026-11-06', source = 'qoder'"),
        ("usage_sync_local_state", "INSERT INTO usage_sync_local_state VALUES ('record', 'signature', '2026-11-07T12:01:00.000Z')",
         "UPDATE usage_sync_local_state SET updated_at = '2026-11-07T12:04:00.000Z'", "UPDATE usage_sync_local_state SET record_id = 'new-record'"),
    ] {
        mutation(&conn, insert, true);
        mutation(&conn, update, true);
        mutation(&conn, update, false);
        // An ignored duplicate whose proposed fields differ must not invalidate.
        mutation(&conn, &insert.replacen("INSERT INTO", "INSERT OR IGNORE INTO", 1), false);
        mutation(&conn, &format!("INSERT OR REPLACE INTO {table} SELECT * FROM {table}"), false);
        mutation(&conn, key_update, true);
        mutation(&conn, &format!("DELETE FROM {table}"), true);
        mutation(&conn, &format!("DELETE FROM {table}"), false);
    }
}

#[test]
fn pricing_and_unrelated_hook_mcp_log_settings_writes_do_not_invalidate_usage() {
    let conn = connection();
    mutation(&conn, "INSERT INTO usage_records(record_id, source, session_id, occurred_at, record_kind, origin)
                     VALUES ('record', 'claude', 'detail', '2026-11-07T12:01:00.000Z', 'response', 'native_transcript')", true);
    mutation(&conn, "UPDATE usage_records SET provider_id = 'provider', gateway = 'gateway', route = 'route', cost_usd = 1.5, pricing_source = 'catalog'", false);
    mutation(&conn, "INSERT OR REPLACE INTO usage_records
                     SELECT record_id, source, session_id, occurred_at, model, record_kind,
                            input_tokens, output_tokens, cached_tokens, origin,
                            'another-provider', 'another-gateway', 'another-route', 2.5, 'manual'
                       FROM usage_records", false);
    mutation(&conn, "INSERT INTO app_settings VALUES ('setting', 'true', 'now');
                     INSERT INTO mcp_access_logs VALUES (1, 'detail');
                     INSERT INTO mcp_call_observations VALUES ('call', 'pending');
                     UPDATE mcp_call_observations SET status = 'failed';
                     UPDATE mcp_access_logs SET detail = 'updated';
                     DELETE FROM app_settings; DELETE FROM mcp_access_logs; DELETE FROM mcp_call_observations;", false);
    event(&conn, "hook", "claude", "detail", "Prompt", LATE);
    mutation(&conn, "UPDATE agent_events SET payload_json = '{\"changed\":true}',
                         conversation_state = 'full', l1_state = 'stored', conversation_text = 'text'", false);
    mutation(&conn, "UPDATE agent_events SET occurred_at = '2099'", false);
    mutation(&conn, "DELETE FROM agent_events", false);
}

#[test]
fn fallback_invalidation_uses_max_timestamp_not_latest_id_and_requires_a_fallback() {
    let conn = connection();
    mutation(&conn, "INSERT INTO session_token_usage VALUES
                     ('claude', 'fallback', 100, 20, 0, 'native_transcript', '2026-11-07T12:05:00.000Z')", true);
    mutation(&conn, "INSERT INTO agent_events(event_key, source, session_id, event_type, occurred_at, payload_json)
                     VALUES ('max-time', 'claude', 'fallback', 'Prompt', '2026-11-07T12:04:00.000Z', '{}')", true);
    mutation(&conn, "INSERT INTO agent_events(event_key, source, session_id, event_type, occurred_at, payload_json)
                     VALUES ('max-id', 'claude', 'fallback', 'Stop', '2026-11-07T12:01:00.000Z', '{}')", false);
    mutation(&conn, "UPDATE agent_events SET event_type = 'Prompt', payload_json = '{\"other\":true}' WHERE event_key = 'max-id'", false);
    mutation(&conn, "UPDATE agent_events SET occurred_at = '2026-11-07T12:02:00.000Z' WHERE event_key = 'max-id'", false);
    mutation(&conn, "DELETE FROM agent_events WHERE event_key = 'max-time'", true);
    mutation(&conn, "INSERT OR REPLACE INTO agent_events(event_key, source, session_id, event_type, occurred_at, payload_json)
                     VALUES ('max-id', 'claude', 'fallback', 'Stop', '2026-11-07T12:02:00.000Z', '{}')", false);
    mutation(&conn, "INSERT INTO agent_events(event_key, source, session_id, event_type, occurred_at, payload_json)
                     VALUES ('other', 'qoder', 'unrelated', 'Prompt', '2099', '{}')", false);
    mutation(&conn, "UPDATE agent_events SET source = 'claude', session_id = 'fallback' WHERE event_key = 'other'", true);
    mutation(&conn, "UPDATE agent_events SET session_id = 'unrelated' WHERE event_key = 'other'", true);
    mutation(&conn, "INSERT OR REPLACE INTO agent_events(event_key, source, session_id, event_type, occurred_at, payload_json)
                     VALUES ('max-id', 'qoder', 'moved', 'Stop', '2026-11-07T12:02:00.000Z', '{}')", true);
    mutation(&conn, "INSERT INTO usage_records(record_id, source, session_id, occurred_at, record_kind, origin)
                     VALUES ('detail', 'claude', 'fallback', '2026-11-07T12:01:00.000Z', 'response', 'native_transcript')", true);
    mutation(&conn, "UPDATE agent_events SET source = 'claude', session_id = 'fallback', occurred_at = '2099' WHERE event_key = 'max-id'", false);
    mutation(&conn, "DELETE FROM agent_events", false);
    mutation(&conn, "DELETE FROM usage_records", true);
    // No events: the fallback is refreshed_at. Inserting an equal timestamp has
    // no semantic effect, and removing that event returns the same fallback.
    mutation(&conn, "INSERT INTO agent_events(event_key, source, session_id, event_type, occurred_at, payload_json)
                     VALUES ('equal', 'claude', 'fallback', 'Prompt', '2026-11-07T12:05:00.000Z', '{}')", false);
    mutation(&conn, "DELETE FROM agent_events", false);
}

#[test]
fn external_writer_maintains_durable_revisions_and_projections_with_default_pragmas() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("projections.sqlite3");
    let reader = Connection::open(&path).unwrap();
    legacy_schema(&reader);
    reader.execute_batch("BEGIN IMMEDIATE").unwrap();
    initialize(&reader).unwrap();
    reader.execute_batch("COMMIT").unwrap();
    let writer = Connection::open(&path).unwrap();
    let recursive: bool = writer.pragma_query_value(None, "recursive_triggers", |row| row.get(0)).unwrap();
    assert!(!recursive, "external connections must work without enabling recursive triggers");
    let before = revision(&reader);
    writer.execute_batch("INSERT INTO app_settings VALUES ('peer-setting', 'true', 'now');
                          INSERT INTO mcp_access_logs VALUES (1, 'peer access');
                          INSERT INTO mcp_call_observations VALUES ('peer-call', 'failed');").unwrap();
    assert_eq!(revision(&reader), before);
    mutation(&writer, "INSERT INTO usage_records(record_id, source, session_id, occurred_at, record_kind, origin)
                       VALUES ('peer-record', 'claude', 'peer', '2026-11-07T12:01:00.000Z', 'response', 'native_transcript')", true);
    assert!(revision(&reader) > before);
    let before = revision(&reader);
    mutation(&writer, "UPDATE usage_records SET cost_usd = 9, pricing_source = 'manual'", false);
    assert_eq!(revision(&reader), before);
    mutation(&writer, "INSERT INTO session_token_usage VALUES ('qoder', 'fallback', 100, 20, 0, 'native_transcript', '2026-11-07T12:05:00.000Z')", true);
    let before = revision(&reader);
    mutation(&writer, "INSERT INTO agent_events(event_key, source, session_id, event_type, occurred_at, payload_json)
                       VALUES ('peer-event', 'qoder', 'fallback', 'Prompt', '2026-11-07T12:01:00.000Z', '{}')", true);
    assert!(revision(&reader) > before);
    mutation(&writer, "INSERT OR REPLACE INTO agent_events(event_key, source, session_id, event_type, occurred_at, payload_json)
                       VALUES ('peer-event', 'claude', 'replacement', 'Stop', '2026-11-07T12:04:00.000Z', '{}')", true);
    assert_projections(&reader);
    mutation(&writer, "DELETE FROM agent_events", false);
    assert_projections(&reader);
}

#[test]
fn rolled_back_writes_restore_counts_latest_and_revision_together() {
    let conn = connection();
    event(&conn, "retained", "claude", "session", "Prompt", EARLY);
    let before = revision(&conn);
    let status = serde_json::to_value(live_status(&conn, CUTOFF, NOW).unwrap()).unwrap();
    conn.execute_batch("BEGIN IMMEDIATE").unwrap();
    mutation(&conn, "INSERT INTO session_token_usage VALUES ('claude', 'session', 100, 20, 0, 'native_transcript', '2026-11-07T12:05:00.000Z')", true);
    mutation(&conn, "DELETE FROM agent_events", true);
    conn.execute_batch("ROLLBACK").unwrap();
    assert_eq!(revision(&conn), before);
    assert_projections(&conn);
    assert_eq!(serde_json::to_value(live_status(&conn, CUTOFF, NOW).unwrap()).unwrap(), status);
}
