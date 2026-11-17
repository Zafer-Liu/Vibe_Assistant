//! Durable overview projections and usage-only cache invalidation.
//!
//! The schema migration owns the transaction. Ordinary opens never rebuild these
//! projections. All maintenance lives in SQLite so external writers participate.
use super::TelemetryLiveStatus;
use rusqlite::Connection;

#[cfg(test)]
#[path = "projections_tests.rs"]
mod tests;

/// Called after all v1 tables/columns exist, inside the v2 migration transaction.
pub(super) fn initialize(conn: &Connection) -> Result<(), String> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS app_data_revisions (
             domain TEXT PRIMARY KEY, revision INTEGER NOT NULL
         );
         INSERT OR IGNORE INTO app_data_revisions(domain, revision) VALUES ('usage', 0);
         CREATE INDEX IF NOT EXISTS idx_agent_events_session_latest
             ON agent_events(source, session_id, id DESC);
         CREATE INDEX IF NOT EXISTS idx_agent_events_session_occurred
             ON agent_events(source, session_id, occurred_at DESC);
         CREATE TABLE IF NOT EXISTS session_latest_events (
             source TEXT NOT NULL, session_id TEXT NOT NULL,
             event_id INTEGER NOT NULL, event_type TEXT NOT NULL,
             occurred_at TEXT NOT NULL, max_occurred_at TEXT NOT NULL,
             PRIMARY KEY(source, session_id)
         );
         CREATE TABLE IF NOT EXISTS event_counts (
             singleton INTEGER PRIMARY KEY CHECK(singleton = 1),
             total_events INTEGER NOT NULL,
             completed_conversations INTEGER NOT NULL,
             organized_memory_conversations INTEGER NOT NULL,
             pending_memory_sessions INTEGER NOT NULL,
             retrying_memory_sessions INTEGER NOT NULL,
             failed_memory_sessions INTEGER NOT NULL
         );
         -- Bounded scratch space, not a second event ledger. A BEFORE trigger
         -- captures up to three possible victims; only a successful AFTER
         -- trigger applies deltas. An ignored insert may leave scratch rows,
         -- which the next INSERT/UPDATE replaces before consulting them.
         CREATE TABLE IF NOT EXISTS event_projection_replacements (
             event_id INTEGER PRIMARY KEY, event_key TEXT NOT NULL,
             source TEXT NOT NULL, session_id TEXT NOT NULL,
             conversation_state TEXT NOT NULL, l1_state TEXT NOT NULL,
             has_text INTEGER NOT NULL
         );
         CREATE TABLE IF NOT EXISTS usage_revision_pending (
             table_name TEXT PRIMARY KEY, changed INTEGER NOT NULL
         );
         INSERT INTO session_latest_events
             (source, session_id, event_id, event_type, occurred_at, max_occurred_at)
         SELECT e.source, e.session_id, e.id, e.event_type, e.occurred_at, grouped.max_time
           FROM (SELECT source, session_id, MAX(id) AS latest_id,
                        MAX(occurred_at) AS max_time
                   FROM agent_events GROUP BY source, session_id) grouped
           JOIN agent_events e ON e.id = grouped.latest_id
          WHERE NOT EXISTS (SELECT 1 FROM event_counts WHERE singleton = 1);
         INSERT INTO event_counts
         SELECT 1, COUNT(*),
                COALESCE(SUM(conversation_state = 'full'), 0),
                COALESCE(SUM(conversation_state = 'full' AND l1_state = 'stored'
                    AND conversation_text IS NOT NULL AND conversation_text != ''), 0),
                COALESCE(SUM(conversation_state = 'full' AND l1_state = 'pending'
                    AND conversation_text IS NOT NULL AND conversation_text != ''), 0),
                COALESCE(SUM(conversation_state = 'full' AND l1_state = 'retrying'
                    AND conversation_text IS NOT NULL AND conversation_text != ''), 0),
                COALESCE(SUM(conversation_state = 'full' AND l1_state = 'failed'
                    AND conversation_text IS NOT NULL AND conversation_text != ''), 0)
           FROM agent_events
         HAVING NOT EXISTS (SELECT 1 FROM event_counts WHERE singleton = 1);",
    ).map_err(|error| error.to_string())?;

    // Compare the feed's semantic fields, excluding all local pricing metadata.
    // BEFORE snapshots distinguish an identical REPLACE from a changed insert
    // even when SQLite omits REPLACE's implicit DELETE triggers (the default).
    usage_revision_triggers(conn, "usage_records", &["record_id"], &[
        "record_id", "source", "session_id", "occurred_at", "model", "record_kind",
        "input_tokens", "output_tokens", "cached_tokens", "origin",
    ])?;
    usage_revision_triggers(conn, "session_token_usage", &["source", "session_id"], &[
        "source", "session_id", "input_tokens", "output_tokens", "cached_tokens", "origin", "refreshed_at",
    ])?;
    usage_revision_triggers(conn, "usage_remote_records", &["device", "record_id"], &[
        "device", "record_id", "source", "session_id", "occurred_at", "model", "record_kind",
        "input_tokens", "output_tokens", "cached_tokens", "origin", "updated_at",
    ])?;
    usage_revision_triggers(conn, "usage_remote_ledgers", &["device"], &["device", "revision"])?;
    usage_revision_triggers(conn, "usage_remote_totals", &["device", "day", "source"], &[
        "device", "day", "source", "input_tokens", "output_tokens", "cached_tokens", "record_count",
    ])?;
    usage_revision_triggers(conn, "usage_sync_local_state", &["record_id"], &[
        "record_id", "signature", "updated_at",
    ])?;
    event_triggers(conn)
}

fn usage_revision_triggers(
    conn: &Connection, table: &str, keys: &[&str], columns: &[&str],
) -> Result<(), String> {
    // Identifiers come only from the fixed schema lists in initialize(). Use =
    // for keys: nullable legacy TEXT PRIMARY KEY values do not conflict on NULL.
    let identity = keys.iter().map(|key| format!("previous.{key} = NEW.{key}"))
        .collect::<Vec<_>>().join(" AND ");
    let same = columns.iter().map(|column| format!("previous.{column} IS NEW.{column}"))
        .collect::<Vec<_>>().join(" AND ");
    let changed = columns.iter().map(|column| format!("OLD.{column} IS NOT NEW.{column}"))
        .collect::<Vec<_>>().join(" OR ");
    conn.execute_batch(&format!(
        "INSERT OR IGNORE INTO usage_revision_pending(table_name, changed) VALUES ('{table}', 0);
         CREATE TRIGGER IF NOT EXISTS revision_{table}_before_insert BEFORE INSERT ON {table} BEGIN
             UPDATE usage_revision_pending SET changed = NOT EXISTS (
                 SELECT 1 FROM {table} previous WHERE {identity} AND {same}
             ) WHERE table_name = '{table}';
         END;
         CREATE TRIGGER IF NOT EXISTS revision_{table}_insert AFTER INSERT ON {table} BEGIN
             UPDATE app_data_revisions SET revision = revision + 1 WHERE domain = 'usage'
                 AND (SELECT changed FROM usage_revision_pending WHERE table_name = '{table}') != 0;
         END;
         CREATE TRIGGER IF NOT EXISTS revision_{table}_update AFTER UPDATE ON {table}
         WHEN {changed} BEGIN
             UPDATE app_data_revisions SET revision = revision + 1 WHERE domain = 'usage';
         END;
         CREATE TRIGGER IF NOT EXISTS revision_{table}_delete AFTER DELETE ON {table} BEGIN
             UPDATE app_data_revisions SET revision = revision + 1 WHERE domain = 'usage';
         END;"
    )).map_err(|error| error.to_string())
}

const COUNTERS: &[(&str, &str)] = &[
    ("total_events", "1"),
    ("completed_conversations", "{row}.conversation_state = 'full'"),
    ("organized_memory_conversations", "{row}.conversation_state = 'full' AND {row}.l1_state = 'stored' AND {text}"),
    ("pending_memory_sessions", "{row}.conversation_state = 'full' AND {row}.l1_state = 'pending' AND {text}"),
    ("retrying_memory_sessions", "{row}.conversation_state = 'full' AND {row}.l1_state = 'retrying' AND {text}"),
    ("failed_memory_sessions", "{row}.conversation_state = 'full' AND {row}.l1_state = 'failed' AND {text}"),
];

fn counter_expression(expression: &str, row: &str, text: &str) -> String {
    expression.replace("{row}", row).replace("{text}", text)
}

/// Refresh only the affected sessions, using index seeks for both kinds of
/// latest: greatest id for liveness, greatest timestamp for usage fallbacks.
fn refresh_sessions(affected: &str) -> String {
    format!(
        "UPDATE app_data_revisions SET revision = revision + 1
          WHERE domain = 'usage' AND EXISTS (
             SELECT 1 FROM ({affected}) affected
             CROSS JOIN session_token_usage u
              WHERE u.source = affected.source AND u.session_id = affected.session_id
                AND NOT EXISTS (SELECT 1 FROM usage_records r
                                 WHERE r.source = u.source AND r.session_id = u.session_id)
                AND COALESCE((SELECT max_occurred_at FROM session_latest_events previous
                               WHERE previous.source = u.source AND previous.session_id = u.session_id),
                             u.refreshed_at)
                    IS NOT COALESCE((SELECT MAX(e.occurred_at) FROM agent_events e
                                      WHERE e.source = u.source AND e.session_id = u.session_id),
                                    u.refreshed_at)
         );
         DELETE FROM session_latest_events
          WHERE (source, session_id) IN ({affected});
         INSERT INTO session_latest_events
             (source, session_id, event_id, event_type, occurred_at, max_occurred_at)
         SELECT e.source, e.session_id, e.id, e.event_type, e.occurred_at,
                (SELECT MAX(t.occurred_at) FROM agent_events t
                  WHERE t.source = e.source AND t.session_id = e.session_id)
           FROM ({affected}) affected
           JOIN agent_events e ON e.id = (
                SELECT id FROM agent_events latest
                 WHERE latest.source = affected.source AND latest.session_id = affected.session_id
                 ORDER BY id DESC LIMIT 1
           );"
    )
}

fn event_triggers(conn: &Connection) -> Result<(), String> {
    let capture_columns = "event_id, event_key, source, session_id, conversation_state, l1_state, has_text";
    let capture_values = "id, event_key, source, session_id, conversation_state, l1_state,
                          conversation_text IS NOT NULL AND conversation_text != ''";
    let affected = "SELECT source, session_id FROM event_projection_replacements
                    UNION SELECT NEW.source, NEW.session_id";
    let after_write = format!(
        "UPDATE event_counts SET {} WHERE singleton = 1;
         {}
         DELETE FROM event_projection_replacements;",
        COUNTERS.iter().map(|(column, expression)| {
            let old = counter_expression(expression, "previous", "previous.has_text != 0");
            let new = counter_expression(expression, "NEW", "NEW.conversation_text IS NOT NULL AND NEW.conversation_text != ''");
            format!("{column} = {column} - (SELECT COALESCE(SUM(({old})), 0)
                     FROM event_projection_replacements previous) + ({new})")
        }).collect::<Vec<_>>().join(", "),
        refresh_sessions(affected),
    );
    let after_delete = format!(
        "UPDATE event_counts SET {} WHERE singleton = 1;
         {}
         -- With recursive_triggers=ON, REPLACE already ran this DELETE trigger.
         -- Remove its captured victim so AFTER INSERT/UPDATE cannot subtract twice.
         DELETE FROM event_projection_replacements WHERE event_id = OLD.id;",
        COUNTERS.iter().map(|(column, expression)| {
            let old = counter_expression(expression, "OLD", "OLD.conversation_text IS NOT NULL AND OLD.conversation_text != ''");
            format!("{column} = {column} - ({old})")
        }).collect::<Vec<_>>().join(", "),
        refresh_sessions("SELECT OLD.source AS source, OLD.session_id AS session_id"),
    );
    // Transcript/error payload updates and nonempty-to-nonempty text edits do
    // not change either projection. Avoid scratch writes/index seeks for them.
    let update_changed = "OLD.id IS NOT NEW.id OR OLD.event_key IS NOT NEW.event_key
        OR OLD.source IS NOT NEW.source OR OLD.session_id IS NOT NEW.session_id
        OR OLD.event_type IS NOT NEW.event_type OR OLD.occurred_at IS NOT NEW.occurred_at
        OR OLD.conversation_state IS NOT NEW.conversation_state OR OLD.l1_state IS NOT NEW.l1_state
        OR (OLD.conversation_text IS NOT NULL AND OLD.conversation_text != '')
           IS NOT (NEW.conversation_text IS NOT NULL AND NEW.conversation_text != '')";
    conn.execute_batch(&format!(
        "CREATE TRIGGER IF NOT EXISTS event_projection_before_insert BEFORE INSERT ON agent_events BEGIN
             DELETE FROM event_projection_replacements;
             INSERT INTO event_projection_replacements ({capture_columns})
             SELECT {capture_values} FROM agent_events
              WHERE event_key = NEW.event_key OR id = NEW.id;
         END;
         CREATE TRIGGER IF NOT EXISTS event_projection_insert AFTER INSERT ON agent_events BEGIN
             -- NEW.id is undefined before an auto-id insert (usually -1).
             -- Discard any row captured only by that provisional id.
             DELETE FROM event_projection_replacements
              WHERE event_key != NEW.event_key AND event_id != NEW.id;
             {after_write}
         END;
         CREATE TRIGGER IF NOT EXISTS event_projection_before_update BEFORE UPDATE ON agent_events
         WHEN {update_changed} BEGIN
             DELETE FROM event_projection_replacements;
             INSERT INTO event_projection_replacements ({capture_columns})
             SELECT {capture_values} FROM agent_events
              WHERE id = OLD.id OR id = NEW.id OR event_key = NEW.event_key;
         END;
         CREATE TRIGGER IF NOT EXISTS event_projection_update AFTER UPDATE ON agent_events
         WHEN {update_changed} BEGIN
             {after_write}
         END;
         CREATE TRIGGER IF NOT EXISTS event_projection_delete AFTER DELETE ON agent_events BEGIN
             {after_delete}
         END;"
    )).map_err(|error| error.to_string())
}

/// Same counters and lexical timestamp boundaries as the original overview.
/// All fields, including active_sources, come from one SQLite read snapshot.
pub(super) fn live_status(
    conn: &Connection, recent_cutoff: &str, now: &str,
) -> Result<TelemetryLiveStatus, String> {
    let (mut status, sources) = conn.query_row(
        "WITH sessions AS (
             SELECT source, session_id FROM session_latest_events WHERE session_id != 'unknown'
             UNION SELECT source, session_id FROM session_token_usage
         ), active AS MATERIALIZED (
             SELECT source FROM session_latest_events
              WHERE session_id != 'unknown' AND occurred_at >= ?1 AND occurred_at <= ?2
                AND event_type NOT IN ('Stop', 'TranscriptSync', 'session_end', 'session_usage')
         )
         SELECT (SELECT COUNT(*) FROM sessions), (SELECT COUNT(*) FROM active),
                counts.completed_conversations, counts.organized_memory_conversations,
                counts.pending_memory_sessions, counts.retrying_memory_sessions,
                counts.failed_memory_sessions,
                (SELECT COUNT(*) FROM sessions s WHERE NOT EXISTS (
                     SELECT 1 FROM session_token_usage u
                      WHERE u.source = s.source AND u.session_id = s.session_id)),
                (SELECT COUNT(*) FROM transcript_scan_states WHERE state = 'failed'),
                (SELECT MAX(occurred_at) FROM agent_events),
                (SELECT json_group_array(source) FROM (SELECT DISTINCT source FROM active ORDER BY source))
           FROM event_counts counts WHERE counts.singleton = 1",
        [recent_cutoff, now],
        |row| Ok((TelemetryLiveStatus {
            captured_sessions: row.get(0)?, active_sessions: row.get(1)?, active_sources: Vec::new(),
            completed_conversations: row.get(2)?, organized_memory_conversations: row.get(3)?,
            pending_memory_sessions: row.get(4)?, retrying_memory_sessions: row.get(5)?,
            failed_memory_sessions: row.get(6)?, pending_usage_sessions: row.get(7)?,
            failed_transcript_scans: row.get(8)?, last_event_at: row.get(9)?,
        }, row.get::<_, String>(10)?)),
    ).map_err(|error| error.to_string())?;
    status.active_sources = serde_json::from_str(&sources).map_err(|error| error.to_string())?;
    Ok(status)
}
