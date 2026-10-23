//! SQLite initialization and idempotent schema migrations.
use rusqlite::Connection;
use std::path::PathBuf;

pub(super) fn open_database() -> Result<Connection, String> {
    let dir = dirs_next::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("agent-manager");
    std::fs::create_dir_all(&dir).map_err(|e| format!("create data directory {}: {e}", dir.display()))?;
    let path = dir.join("telemetry.sqlite3");
    let conn = Connection::open(&path).map_err(|e| format!("open {}: {e}", path.display()))?;
    ensure_writable_connection(&conn, &path)?;
    // Probe write access up front with a real page allocation: acquiring the
    // write lock alone also succeeds when the process is confined to writing
    // inside a sandbox, so a lock check would let the failure surface later as
    // the much vaguer "attempt to write a readonly database".
    // Concurrent writers (e.g. an MCP relay) may hold the write lock, so wait
    // for it and retry the probe.
    conn.pragma_update(None, "busy_timeout", 8000)
        .map_err(|e| format!("set busy_timeout on {}: {e}", path.display()))?;
    let mut last_err = None;
    for attempt in 0..3 {
        let probe = conn.execute_batch(
            "BEGIN IMMEDIATE; \
             CREATE TABLE IF NOT EXISTS __write_probe (x INTEGER); \
             DROP TABLE IF EXISTS __write_probe; \
             ROLLBACK;",
        );
        match probe {
            Ok(()) => {
                last_err = None;
                break;
            }
            Err(e) => {
                last_err = Some(e);
                // A failed probe can leave the half-open transaction behind;
                // roll it back so the retry can BEGIN again.
                let _ = conn.execute_batch("ROLLBACK;");
                if attempt < 2 {
                    std::thread::sleep(std::time::Duration::from_millis(1500));
                }
            }
        }
    }
    if let Some(e) = last_err {
        return Err(format!("{} is not writable: {e} ({e:?})", path.display()));
    }
    initialize_database(conn).map_err(|e| format!("initialize schema in {}: {e}", path.display()))
}

fn ensure_writable_connection(conn: &Connection, path: &std::path::Path) -> Result<(), String> {
    // SQLite may silently fall back to a read-only connection after the OS
    // denies write access. Retrying that same connection cannot fix it.
    if conn
        .is_readonly(rusqlite::DatabaseName::Main)
        .map_err(|e| e.to_string())?
    {
        let os_error = std::fs::OpenOptions::new().write(true).open(path).err();
        let detail = os_error
            .map(|e| format!("; OS write access: {e}"))
            .unwrap_or_default();
        let hint = if cfg!(windows) {
            "; check the executable's Windows integrity label (Low integrity cannot write normal AppData); run npm run dev to prepare the build directory"
        } else {
            "; check database and parent directory permissions"
        };
        return Err(format!("{} opened read-only{detail}{hint}", path.display()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_read_only_connections_without_changing_existing_data() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("telemetry.sqlite3");
        let writable = Connection::open(&path).unwrap();
        writable
            .execute_batch("CREATE TABLE evidence (value TEXT); INSERT INTO evidence VALUES ('retained');")
            .unwrap();
        ensure_writable_connection(&writable, &path).unwrap();
        drop(writable);

        let readonly =
            Connection::open_with_flags(&path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
        let error = ensure_writable_connection(&readonly, &path).unwrap_err();
        assert!(error.contains("opened read-only"), "{error}");
        assert!(error.contains(&path.display().to_string()), "{error}");
        let value: String = readonly
            .query_row("SELECT value FROM evidence", [], |row| row.get(0))
            .unwrap();
        assert_eq!(value, "retained");
    }
}

/// Shared by production and isolated tests; includes every schema migration.
pub(super) fn initialize_database(mut conn: Connection) -> Result<Connection, String> {
    conn.execute_batch(
        "PRAGMA journal_mode=WAL;
         PRAGMA busy_timeout=8000;
         PRAGMA foreign_keys=ON;
         CREATE TABLE IF NOT EXISTS agent_events (
           id INTEGER PRIMARY KEY AUTOINCREMENT,
           event_key TEXT NOT NULL UNIQUE,
           source TEXT NOT NULL,
           session_id TEXT NOT NULL,
           project_path TEXT,
           event_type TEXT NOT NULL,
           occurred_at TEXT NOT NULL,
           token_source TEXT NOT NULL DEFAULT 'unavailable',
           input_tokens INTEGER NOT NULL DEFAULT 0,
           output_tokens INTEGER NOT NULL DEFAULT 0,
           cached_tokens INTEGER NOT NULL DEFAULT 0,
           payload_json TEXT NOT NULL
         );
         CREATE INDEX IF NOT EXISTS idx_agent_events_occurred ON agent_events(occurred_at);
         CREATE INDEX IF NOT EXISTS idx_agent_events_source_session ON agent_events(source, session_id);
         CREATE TABLE IF NOT EXISTS session_token_usage (
           source TEXT NOT NULL,
           session_id TEXT NOT NULL,
           input_tokens INTEGER NOT NULL DEFAULT 0,
           output_tokens INTEGER NOT NULL DEFAULT 0,
           cached_tokens INTEGER NOT NULL DEFAULT 0,
           origin TEXT NOT NULL,
           refreshed_at TEXT NOT NULL,
           PRIMARY KEY(source, session_id)
         );
         CREATE TABLE IF NOT EXISTS usage_records (
           record_id TEXT PRIMARY KEY,
           source TEXT NOT NULL,
           session_id TEXT NOT NULL,
           occurred_at TEXT NOT NULL,
           model TEXT,
           record_kind TEXT NOT NULL,
           input_tokens INTEGER NOT NULL DEFAULT 0,
           output_tokens INTEGER NOT NULL DEFAULT 0,
           cached_tokens INTEGER NOT NULL DEFAULT 0,
           origin TEXT NOT NULL
         );
         CREATE INDEX IF NOT EXISTS idx_usage_records_time ON usage_records(occurred_at DESC);
         CREATE INDEX IF NOT EXISTS idx_usage_records_session ON usage_records(source, session_id);
         CREATE TABLE IF NOT EXISTS mcp_call_observations (
           id INTEGER PRIMARY KEY AUTOINCREMENT,
           observation_key TEXT NOT NULL UNIQUE,
           source TEXT NOT NULL,
           session_id TEXT NOT NULL,
           occurred_at TEXT NOT NULL,
           server_name TEXT NOT NULL,
           tool_name TEXT NOT NULL,
           status TEXT NOT NULL CHECK(status IN ('success', 'failed', 'pending')),
           error_category TEXT,
           result_observed INTEGER NOT NULL DEFAULT 0,
           parser_version INTEGER NOT NULL DEFAULT 1,
           origin TEXT NOT NULL DEFAULT 'transcript',
           error_phase TEXT,
           failure_kind TEXT,
           error_code INTEGER,
           error_reason TEXT
         );
         CREATE INDEX IF NOT EXISTS idx_mcp_call_observations_time ON mcp_call_observations(occurred_at DESC);
         CREATE INDEX IF NOT EXISTS idx_mcp_call_observations_source_session ON mcp_call_observations(source, session_id);
         CREATE INDEX IF NOT EXISTS idx_mcp_call_observations_server ON mcp_call_observations(server_name, occurred_at DESC);
         CREATE INDEX IF NOT EXISTS idx_mcp_call_observations_tool ON mcp_call_observations(tool_name, occurred_at DESC);
         CREATE INDEX IF NOT EXISTS idx_mcp_failed_calls_time ON mcp_call_observations(occurred_at DESC, id DESC) WHERE status = 'failed';
         CREATE TABLE IF NOT EXISTS mcp_call_coverage (
           source TEXT PRIMARY KEY,
           state TEXT NOT NULL,
           transcript_count INTEGER NOT NULL DEFAULT 0,
           call_count INTEGER NOT NULL DEFAULT 0,
           scanned_at TEXT,
           reason TEXT
         );
         CREATE TABLE IF NOT EXISTS mcp_proxy_connections (
           connection_id TEXT PRIMARY KEY,
           server_name TEXT NOT NULL,
           assignment_id TEXT NOT NULL,
           last_seen_at TEXT NOT NULL
         );
         CREATE TABLE IF NOT EXISTS workspace_summaries (
           workspace_id TEXT PRIMARY KEY,
           content TEXT NOT NULL,
           source_event_count INTEGER NOT NULL DEFAULT 0,
           updated_at TEXT NOT NULL
         );
         CREATE TABLE IF NOT EXISTS profile_summaries (
           profile_id TEXT PRIMARY KEY,
           content TEXT NOT NULL,
           source_workspace_count INTEGER NOT NULL DEFAULT 0,
           updated_at TEXT NOT NULL
         );
         CREATE TABLE IF NOT EXISTS memory_importance (
           memory_id TEXT PRIMARY KEY,
           score INTEGER NOT NULL DEFAULT 0,
           supporting_sessions INTEGER NOT NULL DEFAULT 0,
           supporting_agents INTEGER NOT NULL DEFAULT 0,
           duplicate_count INTEGER NOT NULL DEFAULT 0,
           recall_count INTEGER NOT NULL DEFAULT 0,
           pinned INTEGER NOT NULL DEFAULT 0,
           updated_at TEXT NOT NULL
         );
         CREATE TABLE IF NOT EXISTS local_memory_items (
           id TEXT PRIMARY KEY,
           source_event_key TEXT NOT NULL,
           ordinal INTEGER NOT NULL,
           memory TEXT NOT NULL,
           memory_type TEXT NOT NULL DEFAULT 'conversation',
           durability TEXT NOT NULL DEFAULT 'short_term',
           source TEXT NOT NULL,
           session_id TEXT NOT NULL,
           user_defined INTEGER NOT NULL DEFAULT 0,
           event_time TEXT,
           created_at TEXT NOT NULL,
           updated_at TEXT NOT NULL,
           UNIQUE(source_event_key, ordinal)
         );
         CREATE TABLE IF NOT EXISTS mcp_access_logs (
           id INTEGER PRIMARY KEY AUTOINCREMENT,
           occurred_at TEXT NOT NULL,
           client_name TEXT NOT NULL,
           tool_name TEXT NOT NULL,
           summary TEXT NOT NULL,
           detail TEXT,
          success INTEGER NOT NULL DEFAULT 1
        );
        -- Application-owned configuration is kept with the durable
         -- memory/telemetry ledger.  Values stay JSON so settings can
         -- evolve without a schema migration for every UI field.
         CREATE TABLE IF NOT EXISTS app_settings (
           setting_key TEXT PRIMARY KEY,
           value_json TEXT NOT NULL,
           updated_at TEXT NOT NULL
         );
         CREATE TABLE IF NOT EXISTS transcript_scan_states (
           source TEXT NOT NULL,
           transcript_path TEXT NOT NULL,
           file_size INTEGER NOT NULL,
           modified_at TEXT NOT NULL,
           state TEXT NOT NULL,
           error_detail TEXT,
           session_id TEXT,
           last_scanned_at TEXT NOT NULL,
           PRIMARY KEY(source, transcript_path)
         );
         -- Hierarchical memory is intentionally in the same durable
         -- SQLite ledger as L0/L1.  `state` makes generated Profile
         -- changes reviewable before they can be injected into an Agent.
         CREATE TABLE IF NOT EXISTS memory_layer_documents (
           id TEXT PRIMARY KEY,
           layer TEXT NOT NULL CHECK(layer IN ('l2', 'l3')),
           scope TEXT NOT NULL DEFAULT 'global',
           content TEXT NOT NULL,
           state TEXT NOT NULL CHECK(state IN ('draft', 'published', 'archived', 'failed')),
           token_estimate INTEGER NOT NULL DEFAULT 0,
           source_count INTEGER NOT NULL DEFAULT 0,
           window_start TEXT,
           window_end TEXT,
           created_at TEXT NOT NULL,
           published_at TEXT
         );
         CREATE INDEX IF NOT EXISTS idx_memory_layer_documents_active
           ON memory_layer_documents(layer, scope, state, created_at DESC);
         CREATE TABLE IF NOT EXISTS memory_layer_sources (
           document_id TEXT NOT NULL REFERENCES memory_layer_documents(id) ON DELETE CASCADE,
           source_layer TEXT NOT NULL CHECK(source_layer IN ('l1', 'l2')),
           source_id TEXT NOT NULL,
           PRIMARY KEY(document_id, source_layer, source_id)
         );
         -- 云端记忆库同步水位：本地发布/编辑后置 dirty，同步成功回填对齐。
         CREATE TABLE IF NOT EXISTS cloud_sync_meta (
           object_key TEXT PRIMARY KEY,
           revision INTEGER NOT NULL DEFAULT 0,
           content_hash TEXT,
           sync_state TEXT NOT NULL DEFAULT 'clean',
           updated_at TEXT
         );
         CREATE TABLE IF NOT EXISTS usage_remote_totals (
           device TEXT NOT NULL,
           day TEXT NOT NULL,
           source TEXT NOT NULL,
           input_tokens INTEGER NOT NULL DEFAULT 0,
           output_tokens INTEGER NOT NULL DEFAULT 0,
           cached_tokens INTEGER NOT NULL DEFAULT 0,
           record_count INTEGER NOT NULL DEFAULT 0,
           PRIMARY KEY (device, day, source)
         );
         -- Last common plaintext is retained only on this device.  It is
         -- the base for a human-readable three-way merge when a CAS
         -- conflict occurs; the cloud Vault still stores ciphertext only.
         CREATE TABLE IF NOT EXISTS cloud_sync_snapshots (
           object_key TEXT PRIMARY KEY,
           revision INTEGER NOT NULL,
           content_hash TEXT,
           content TEXT,
           updated_at TEXT NOT NULL
         );
         CREATE TABLE IF NOT EXISTS cloud_sync_conflicts (
           object_key TEXT PRIMARY KEY,
           object_kind TEXT NOT NULL,
           base_content TEXT,
           local_content TEXT,
           remote_content TEXT,
           local_memory_type TEXT,
           remote_memory_type TEXT,
           remote_revision INTEGER NOT NULL,
           remote_content_hash TEXT,
           local_updated_at TEXT,
           remote_updated_at TEXT,
           created_at TEXT NOT NULL
         );",
    )
    .map_err(|e| e.to_string())?;
    add_column_if_missing(
        &conn,
        "conversation_state",
        "TEXT NOT NULL DEFAULT 'unavailable'",
    )?;
    add_column_if_missing(
        &conn,
        "conversation_message_count",
        "INTEGER NOT NULL DEFAULT 0",
    )?;
    add_column_if_missing(&conn, "conversation_text", "TEXT")?;
    add_column_if_missing(&conn, "l1_state", "TEXT NOT NULL DEFAULT 'unavailable'")?;
    add_column_if_missing(&conn, "l1_error_detail", "TEXT")?;
    let has_observation_origin = conn
        .prepare("PRAGMA table_info(mcp_call_observations)")
        .map_err(|e| e.to_string())?
        .query_map([], |row| row.get::<_, String>(1))
        .map_err(|e| e.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?
        .iter()
        .any(|name| name == "origin");
    if !has_observation_origin {
        conn.execute_batch("ALTER TABLE mcp_call_observations ADD COLUMN origin TEXT NOT NULL DEFAULT 'transcript'")
                .map_err(|e| e.to_string())?;
    }
    conn.execute_batch("CREATE INDEX IF NOT EXISTS idx_mcp_failed_calls_server_origin
            ON mcp_call_observations(server_name, origin, occurred_at DESC) WHERE status = 'failed'")
            .map_err(|e| e.to_string())?;
    // 失败详情仅保存有界元数据：阶段、固定类别、可验证的数字错误码。
    // 绝不保存原始参数、结果正文或任意服务错误文本。
    add_column_if_missing_in(&conn, "mcp_call_observations", "error_phase", "TEXT")?;
    add_column_if_missing_in(&conn, "mcp_call_observations", "failure_kind", "TEXT")?;
    add_column_if_missing_in(&conn, "mcp_call_observations", "error_code", "INTEGER")?;
    add_column_if_missing_in(&conn, "mcp_call_observations", "error_reason", "TEXT")?;
    let has_connection_assignment = conn
        .prepare("PRAGMA table_info(mcp_proxy_connections)")
        .map_err(|e| e.to_string())?
        .query_map([], |row| row.get::<_, String>(1))
        .map_err(|e| e.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?
        .iter()
        .any(|name| name == "assignment_id");
    if !has_connection_assignment {
        conn.execute_batch(
            "ALTER TABLE mcp_proxy_connections ADD COLUMN assignment_id TEXT NOT NULL DEFAULT ''",
        )
        .map_err(|e| e.to_string())?;
    }
    conn.execute_batch("CREATE INDEX IF NOT EXISTS idx_mcp_proxy_connections_assignment ON mcp_proxy_connections(assignment_id, last_seen_at)")
            .map_err(|e| e.to_string())?;
    // mcp_access_logs 增加 detail 列：记忆注入摘要需要回放完整内容
    // （查询参数、返回的记忆正文、Skill 正文、注入上下文）。
    let access_log_has_detail = conn
        .prepare("PRAGMA table_info(mcp_access_logs)")
        .map_err(|e| e.to_string())?
        .query_map([], |row| row.get::<_, String>(1))
        .map_err(|e| e.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?
        .iter()
        .any(|name| name == "detail");
    if !access_log_has_detail {
        conn.execute_batch("ALTER TABLE mcp_access_logs ADD COLUMN detail TEXT")
            .map_err(|e| e.to_string())?;
    }
    // Hook delivery and passive MCP traffic share one fact ledger. Legacy
    // tables remain as evidence/replay stores; triggers keep every writer in
    // sync, including the standalone stdio server and transcript importer.
    conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS activity_ledger (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                event_key TEXT NOT NULL UNIQUE,
                domain TEXT NOT NULL CHECK(domain IN ('hook', 'mcp')),
                event_kind TEXT NOT NULL,
                occurred_at TEXT NOT NULL,
                source TEXT NOT NULL,
                origin TEXT NOT NULL,
                session_id TEXT NOT NULL DEFAULT '',
                client_name TEXT,
                server_name TEXT,
                tool_name TEXT,
                status TEXT NOT NULL,
                payload_chars INTEGER NOT NULL DEFAULT 0,
                detail_present INTEGER NOT NULL DEFAULT 0,
                error_category TEXT,
                result_observed INTEGER NOT NULL DEFAULT 0,
                error_phase TEXT,
                failure_kind TEXT,
                error_code INTEGER,
                error_reason TEXT,
                access_log_id INTEGER,
                observation_id INTEGER
            );
            CREATE INDEX IF NOT EXISTS idx_activity_ledger_domain_kind_time
                ON activity_ledger(domain, event_kind, occurred_at DESC);
            CREATE INDEX IF NOT EXISTS idx_activity_ledger_mcp_server_time
                ON activity_ledger(server_name, occurred_at DESC) WHERE domain = 'mcp';
            CREATE INDEX IF NOT EXISTS idx_activity_ledger_client_time
                ON activity_ledger(client_name, occurred_at DESC) WHERE origin = 'memory_mcp';
            CREATE TRIGGER IF NOT EXISTS activity_access_insert AFTER INSERT ON mcp_access_logs BEGIN
                INSERT OR IGNORE INTO activity_ledger
                    (event_key, domain, event_kind, occurred_at, source, origin, session_id,
                     client_name, server_name, tool_name, status, payload_chars, detail_present,
                     error_category, result_observed, error_reason, access_log_id)
                VALUES (
                    'access:' || NEW.id,
                    CASE WHEN NEW.tool_name IN ('session_start_inject', 'prompt_inject') THEN 'hook' ELSE 'mcp' END,
                    CASE WHEN NEW.tool_name = 'session_start_inject' THEN 'hook_session_injection'
                         WHEN NEW.tool_name = 'prompt_inject' AND NEW.detail IS NULL THEN 'hook_skip'
                         WHEN NEW.tool_name = 'prompt_inject' THEN 'hook_prompt_injection'
                         WHEN NEW.tool_name = 'initialize' THEN 'mcp_initialize'
                         WHEN NEW.tool_name = 'tools/list' THEN 'mcp_tools_list'
                         ELSE 'mcp_tool_call' END,
                    COALESCE(strftime('%Y-%m-%dT%H:%M:%fZ', NEW.occurred_at), NEW.occurred_at),
                    CASE WHEN NEW.tool_name IN ('session_start_inject', 'prompt_inject') THEN NEW.client_name ELSE 'memory_mcp' END,
                    CASE WHEN NEW.tool_name IN ('session_start_inject', 'prompt_inject') THEN 'hook' ELSE 'memory_mcp' END,
                    CASE WHEN NEW.tool_name IN ('session_start_inject', 'prompt_inject') THEN '' ELSE NEW.client_name END,
                    NEW.client_name,
                    CASE WHEN NEW.tool_name IN ('session_start_inject', 'prompt_inject') THEN NULL ELSE 'agent-manager-memory' END,
                    NEW.tool_name,
                    CASE WHEN NEW.success != 0 THEN 'success' ELSE 'failed' END,
                    COALESCE(LENGTH(NEW.detail), 0), CASE WHEN NEW.detail IS NULL THEN 0 ELSE 1 END,
                    CASE WHEN NEW.success != 0 THEN NULL ELSE 'mcp_error' END,
                    NEW.success, CASE WHEN NEW.success != 0 THEN NULL ELSE substr(NEW.summary, 1, 240) END,
                    NEW.id
                );
            END;
            CREATE TRIGGER IF NOT EXISTS activity_observation_insert
            AFTER INSERT ON mcp_call_observations WHEN NEW.origin != 'memory_mcp' BEGIN
                INSERT OR IGNORE INTO activity_ledger
                    (event_key, domain, event_kind, occurred_at, source, origin, session_id,
                     server_name, tool_name, status, error_category, result_observed,
                     error_phase, failure_kind, error_code, error_reason, observation_id)
                VALUES ('observation:' || NEW.observation_key, 'mcp', 'mcp_tool_call',
                    COALESCE(strftime('%Y-%m-%dT%H:%M:%fZ', NEW.occurred_at), NEW.occurred_at),
                    NEW.source, NEW.origin, NEW.session_id,
                    NEW.server_name, NEW.tool_name, NEW.status, NEW.error_category,
                    NEW.result_observed, NEW.error_phase, NEW.failure_kind, NEW.error_code,
                    NEW.error_reason, NEW.id);
            END;
            CREATE TRIGGER IF NOT EXISTS activity_observation_update
            AFTER UPDATE ON mcp_call_observations WHEN NEW.origin != 'memory_mcp' BEGIN
                UPDATE activity_ledger SET status = NEW.status, error_category = NEW.error_category,
                    result_observed = NEW.result_observed, error_phase = NEW.error_phase,
                    failure_kind = NEW.failure_kind, error_code = NEW.error_code,
                    error_reason = NEW.error_reason
                WHERE event_key = 'observation:' || NEW.observation_key;
            END;
            CREATE TRIGGER IF NOT EXISTS activity_observation_delete
            AFTER DELETE ON mcp_call_observations WHEN OLD.origin != 'memory_mcp' BEGIN
                DELETE FROM activity_ledger WHERE event_key = 'observation:' || OLD.observation_key;
            END;
            INSERT OR IGNORE INTO activity_ledger
                (event_key, domain, event_kind, occurred_at, source, origin, session_id,
                 client_name, server_name, tool_name, status, payload_chars, detail_present,
                 error_category, result_observed, error_reason, access_log_id)
            SELECT 'access:' || id,
                CASE WHEN tool_name IN ('session_start_inject', 'prompt_inject') THEN 'hook' ELSE 'mcp' END,
                CASE WHEN tool_name = 'session_start_inject' THEN 'hook_session_injection'
                     WHEN tool_name = 'prompt_inject' AND detail IS NULL THEN 'hook_skip'
                     WHEN tool_name = 'prompt_inject' THEN 'hook_prompt_injection'
                     WHEN tool_name = 'initialize' THEN 'mcp_initialize'
                     WHEN tool_name = 'tools/list' THEN 'mcp_tools_list'
                     ELSE 'mcp_tool_call' END,
                COALESCE(strftime('%Y-%m-%dT%H:%M:%fZ', occurred_at), occurred_at),
                CASE WHEN tool_name IN ('session_start_inject', 'prompt_inject') THEN client_name ELSE 'memory_mcp' END,
                CASE WHEN tool_name IN ('session_start_inject', 'prompt_inject') THEN 'hook' ELSE 'memory_mcp' END,
                CASE WHEN tool_name IN ('session_start_inject', 'prompt_inject') THEN '' ELSE client_name END,
                client_name,
                CASE WHEN tool_name IN ('session_start_inject', 'prompt_inject') THEN NULL ELSE 'agent-manager-memory' END,
                tool_name, CASE WHEN success != 0 THEN 'success' ELSE 'failed' END,
                COALESCE(LENGTH(detail), 0), CASE WHEN detail IS NULL THEN 0 ELSE 1 END,
                CASE WHEN success != 0 THEN NULL ELSE 'mcp_error' END,
                success, CASE WHEN success != 0 THEN NULL ELSE substr(summary, 1, 240) END, id
            FROM mcp_access_logs
            WHERE NOT EXISTS (SELECT 1 FROM app_settings WHERE setting_key = 'activity_ledger_backfilled_v1');
            INSERT OR IGNORE INTO activity_ledger
                (event_key, domain, event_kind, occurred_at, source, origin, session_id,
                 server_name, tool_name, status, error_category, result_observed,
                 error_phase, failure_kind, error_code, error_reason, observation_id)
            SELECT 'observation:' || observation_key, 'mcp', 'mcp_tool_call',
                COALESCE(strftime('%Y-%m-%dT%H:%M:%fZ', occurred_at), occurred_at),
                source, origin, session_id, server_name, tool_name, status, error_category,
                result_observed, error_phase, failure_kind, error_code, error_reason, id
            FROM mcp_call_observations
            WHERE origin != 'memory_mcp'
              AND NOT EXISTS (SELECT 1 FROM app_settings WHERE setting_key = 'activity_ledger_backfilled_v1');
            INSERT OR IGNORE INTO app_settings (setting_key, value_json, updated_at)
            VALUES ('activity_ledger_backfilled_v1', 'true', strftime('%Y-%m-%dT%H:%M:%fZ', 'now'));"
        ).map_err(|e| e.to_string())?;
    conn.execute(
        "UPDATE mcp_call_coverage SET state = 'partial'
             WHERE source = 'claude' AND state = 'unavailable' AND transcript_count > 0
               AND reason LIKE 'Some Claude Code transcripts could not%'",
        [],
    )
    .map_err(|e| e.to_string())?;
    add_local_memory_column_if_missing(&conn, "durability", "TEXT NOT NULL DEFAULT 'short_term'")?;
    // 用户自定义记忆标记：旧库升级后默认 0（自动提取），仅手动添加置 1。
    add_local_memory_column_if_missing(&conn, "user_defined", "INTEGER NOT NULL DEFAULT 0")?;
    // Older releases created one TranscriptSync event for every append to
    // the same native session. Keep only the newest revision's memories;
    // its transcript already contains the prior turns.
    conn.execute(
        "DELETE FROM local_memory_items WHERE source_event_key IN (
               SELECT event_key FROM agent_events
                WHERE event_type = 'TranscriptSync'
                  AND id NOT IN (
                    SELECT MAX(id) FROM agent_events WHERE event_type = 'TranscriptSync'
                     GROUP BY source, session_id
                  )
             )",
        [],
    )
    .map_err(|error| error.to_string())?;
    // Historical rows mixed `Z`, `+00:00` and nanosecond-precision
    // suffixes; lexical SQL comparisons need one canonical UTC format.
    // The non-NULL guard skips (idempotently) rows SQLite cannot parse.
    let _ = conn.execute_batch(
        "UPDATE usage_records
                SET occurred_at = strftime('%Y-%m-%dT%H:%M:%fZ', occurred_at)
              WHERE strftime('%Y-%m-%dT%H:%M:%fZ', occurred_at) IS NOT NULL
                AND occurred_at != strftime('%Y-%m-%dT%H:%M:%fZ', occurred_at);
             UPDATE session_token_usage
                SET refreshed_at = strftime('%Y-%m-%dT%H:%M:%fZ', refreshed_at)
              WHERE strftime('%Y-%m-%dT%H:%M:%fZ', refreshed_at) IS NOT NULL
                AND refreshed_at != strftime('%Y-%m-%dT%H:%M:%fZ', refreshed_at);",
    );
    // PRD 一期：usage_records 增列（幂等）+ 模型目录表。
    for (column, declaration) in [
        ("provider_id", "TEXT"),
        ("gateway", "TEXT"),
        ("route", "TEXT"),
        ("cost_usd", "REAL"),
        ("pricing_source", "TEXT"),
    ] {
        let _ = add_column_if_missing_in(&conn, "usage_records", column, declaration);
    }
    let _ = crate::provider_catalog::ensure_catalog_table(&conn);
    migrate_mcp_unknown_projection(&mut conn)?;
    crate::llm_trace::initialize(&conn)?;
    crate::model_registry::initialize(&conn)?;
    Ok(conn)
}

/// Keep the legacy evidence-table CHECK compatible with already-running relays.
/// Its pending+observation_lost rows project to a distinct unknown ledger state.
pub(super) fn migrate_mcp_unknown_projection(conn: &mut Connection) -> Result<(), String> {
    let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate).map_err(|e| e.to_string())?;
    let done: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM app_settings WHERE setting_key = 'mcp_unknown_projection_v1')", [], |row| row.get(0)).map_err(|e| e.to_string())?;
    if !done {
        tx.execute_batch(
            "DROP TRIGGER IF EXISTS activity_observation_insert;
             DROP TRIGGER IF EXISTS activity_observation_update;
             CREATE TRIGGER activity_observation_insert
             AFTER INSERT ON mcp_call_observations WHEN NEW.origin != 'memory_mcp' BEGIN
                INSERT OR IGNORE INTO activity_ledger
                  (event_key, domain, event_kind, occurred_at, source, origin, session_id,
                   server_name, tool_name, status, error_category, result_observed,
                   error_phase, failure_kind, error_code, error_reason, observation_id)
                VALUES ('observation:' || NEW.observation_key, 'mcp', 'mcp_tool_call',
                  COALESCE(strftime('%Y-%m-%dT%H:%M:%fZ', NEW.occurred_at), NEW.occurred_at),
                  NEW.source, NEW.origin, NEW.session_id, NEW.server_name, NEW.tool_name,
                  CASE WHEN NEW.status = 'pending' AND NEW.error_category = 'observation_lost' THEN 'unknown' ELSE NEW.status END,
                  NEW.error_category, NEW.result_observed, NEW.error_phase, NEW.failure_kind,
                  NEW.error_code, NEW.error_reason, NEW.id);
             END;
             CREATE TRIGGER activity_observation_update
             AFTER UPDATE ON mcp_call_observations WHEN NEW.origin != 'memory_mcp' BEGIN
                UPDATE activity_ledger SET
                  status = CASE WHEN NEW.status = 'pending' AND NEW.error_category = 'observation_lost' THEN 'unknown' ELSE NEW.status END,
                  error_category = NEW.error_category, result_observed = NEW.result_observed,
                  error_phase = NEW.error_phase, failure_kind = NEW.failure_kind,
                  error_code = NEW.error_code, error_reason = NEW.error_reason
                WHERE event_key = 'observation:' || NEW.observation_key;
             END;
             UPDATE activity_ledger SET status = 'unknown'
             WHERE observation_id IN (SELECT id FROM mcp_call_observations
                 WHERE status = 'pending' AND error_category = 'observation_lost');
             CREATE INDEX IF NOT EXISTS idx_mcp_unresolved_calls
                 ON mcp_call_observations(occurred_at, session_id) WHERE status = 'pending';
             INSERT INTO app_settings (setting_key, value_json, updated_at)
                 VALUES ('mcp_unknown_projection_v1', 'true', strftime('%Y-%m-%dT%H:%M:%fZ', 'now'));"
        ).map_err(|e| e.to_string())?;
    }
    tx.commit().map_err(|e| e.to_string())
}

fn add_column_if_missing(conn: &Connection, column: &str, declaration: &str) -> Result<(), String> {
    add_column_if_missing_in(conn, "agent_events", column, declaration)
}

/// 通用幂等加列（PRD 一期给 usage_records 加定价维度用）。
fn add_column_if_missing_in(
    conn: &Connection,
    table: &str,
    column: &str,
    declaration: &str,
) -> Result<(), String> {
    let exists = conn
        .prepare(&format!("PRAGMA table_info({table})"))
        .map_err(|e| e.to_string())?
        .query_map([], |row| row.get::<_, String>(1))
        .map_err(|e| e.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?
        .iter()
        .any(|name| name == column);
    if !exists {
        conn.execute_batch(&format!(
            "ALTER TABLE {table} ADD COLUMN {column} {declaration}"
        ))
        .map_err(|e| e.to_string())?;
    }
    Ok(())
}

fn add_local_memory_column_if_missing(
    conn: &Connection,
    column: &str,
    declaration: &str,
) -> Result<(), String> {
    let exists = conn
        .prepare("PRAGMA table_info(local_memory_items)")
        .map_err(|e| e.to_string())?
        .query_map([], |row| row.get::<_, String>(1))
        .map_err(|e| e.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?
        .iter()
        .any(|name| name == column);
    if !exists {
        conn.execute_batch(&format!(
            "ALTER TABLE local_memory_items ADD COLUMN {column} {declaration}"
        ))
        .map_err(|e| e.to_string())?;
    }
    Ok(())
}
