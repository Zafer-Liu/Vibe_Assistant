use super::*;

fn test_activity_ledger(conn: &Connection) {
    conn.execute_batch(
        "CREATE TABLE activity_ledger (
                id INTEGER PRIMARY KEY AUTOINCREMENT, event_key TEXT UNIQUE,
                domain TEXT, event_kind TEXT, occurred_at TEXT, source TEXT, origin TEXT,
                session_id TEXT, client_name TEXT, server_name TEXT, tool_name TEXT,
                status TEXT, payload_chars INTEGER DEFAULT 0, detail_present INTEGER DEFAULT 0,
                error_category TEXT, result_observed INTEGER DEFAULT 0,
                error_phase TEXT, failure_kind TEXT, error_code INTEGER, error_reason TEXT,
                access_log_id INTEGER, observation_id INTEGER
             );
             CREATE TRIGGER activity_observation_insert AFTER INSERT ON mcp_call_observations BEGIN
                INSERT INTO activity_ledger
                  (event_key, domain, event_kind, occurred_at, source, origin, session_id,
                   server_name, tool_name, status, error_category, result_observed,
                   error_phase, failure_kind, error_code, error_reason, observation_id)
                VALUES ('observation:' || NEW.observation_key, 'mcp', 'mcp_tool_call',
                  NEW.occurred_at, NEW.source, NEW.origin, NEW.session_id,
                  NEW.server_name, NEW.tool_name, CASE WHEN NEW.status = 'pending' AND NEW.error_category = 'observation_lost' THEN 'unknown' ELSE NEW.status END, NEW.error_category,
                  NEW.result_observed, NEW.error_phase, NEW.failure_kind, NEW.error_code,
                  NEW.error_reason, NEW.id);
             END;
             CREATE TRIGGER activity_observation_update AFTER UPDATE ON mcp_call_observations BEGIN
                UPDATE activity_ledger SET status = CASE WHEN NEW.status = 'pending' AND NEW.error_category = 'observation_lost' THEN 'unknown' ELSE NEW.status END, error_category = NEW.error_category,
                  result_observed = NEW.result_observed, error_phase = NEW.error_phase,
                  failure_kind = NEW.failure_kind, error_code = NEW.error_code,
                  error_reason = NEW.error_reason
                WHERE event_key = 'observation:' || NEW.observation_key;
             END;
             CREATE TRIGGER activity_observation_delete AFTER DELETE ON mcp_call_observations BEGIN
                DELETE FROM activity_ledger WHERE event_key = 'observation:' || OLD.observation_key;
             END;",
    )
    .unwrap();
}

#[test]
fn cost_recalculation_batches_advance_and_stop() {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(
        "CREATE TABLE usage_records (
                record_id TEXT PRIMARY KEY, source TEXT NOT NULL, model TEXT,
                input_tokens INTEGER NOT NULL, output_tokens INTEGER NOT NULL,
                cached_tokens INTEGER NOT NULL, provider_id TEXT, cost_usd REAL,
                pricing_source TEXT
             );
             INSERT INTO usage_records VALUES
                ('a', 'codex', 'gpt-test', 100, 10, 0, NULL, NULL, NULL),
                ('b', 'codex', 'gpt-test', 100, 10, 0, NULL, 1.0, 'models_dev'),
                ('c', 'codex', 'gpt-test', 100, 10, 0, NULL, NULL, NULL);",
    )
    .unwrap();
    crate::provider_catalog::ensure_catalog_table(&conn).unwrap();
    let store = TelemetryStore::from_connection(conn);
    let mut memo = HashMap::new();
    let mut cursor = String::new();
    let mut visited = 0;
    for _ in 0..4 {
        match store
            .recalculate_costs_batch(&[], &mut memo, false, 1, &cursor)
            .unwrap()
        {
            Some((count, next)) => {
                visited += count;
                cursor = next;
            }
            None => break,
        }
    }
    assert_eq!(visited, 2);
    assert_eq!(cursor, "c");
    assert!(store
        .recalculate_costs_batch(&[], &mut memo, false, 1, &cursor)
        .unwrap()
        .is_none());
    assert_eq!(
        store
            .recalculate_costs_batch(&[], &mut memo, true, 1, "")
            .unwrap()
            .unwrap()
            .1,
        "a"
    );

    assert_eq!(store.invalidate_provider_prices("openai").unwrap(), 2);
    assert_eq!(store.invalidate_catalog_prices().unwrap(), 1);
    let conn = store.conn.lock().unwrap();
    let pending: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM usage_records WHERE pricing_source IS NULL",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(pending, 3);
}

fn mcp_test_store() -> TelemetryStore {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(
        "CREATE TABLE mcp_call_observations (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                observation_key TEXT NOT NULL UNIQUE,
                source TEXT NOT NULL,
                origin TEXT NOT NULL DEFAULT 'transcript',
                session_id TEXT NOT NULL,
                occurred_at TEXT NOT NULL,
                server_name TEXT NOT NULL,
                tool_name TEXT NOT NULL,
                status TEXT NOT NULL CHECK(status IN ('success', 'failed', 'pending')),
                error_category TEXT,
                result_observed INTEGER NOT NULL DEFAULT 0,
                parser_version INTEGER NOT NULL DEFAULT 1,
                error_phase TEXT, failure_kind TEXT, error_code INTEGER, error_reason TEXT
             );
             CREATE TABLE mcp_call_coverage (
                source TEXT PRIMARY KEY, state TEXT NOT NULL, transcript_count INTEGER NOT NULL,
                call_count INTEGER NOT NULL, scanned_at TEXT, reason TEXT
             );
             CREATE TABLE mcp_proxy_connections (
                connection_id TEXT PRIMARY KEY, server_name TEXT NOT NULL,
                assignment_id TEXT NOT NULL, last_seen_at TEXT NOT NULL
             );",
    )
    .unwrap();
    test_activity_ledger(&conn);
    TelemetryStore::from_connection(conn)
}

#[test]
fn transcript_failure_categories_do_not_keep_result_text() {
    let response = serde_json::json!({"content": [{"type": "text", "text": "HTTP 429: rate limit exceeded; token=private-value"}]});
    assert_eq!(transcript_mcp_failure_category(&response), "rate_limit");
    assert_eq!(
        transcript_mcp_failure_category(
            &serde_json::json!({"content": [{"type": "text", "text": "Unexpected tool failure"}]})
        ),
        "unknown_error"
    );
    assert_eq!(
        transcript_mcp_failure_category(&serde_json::json!({"content": []})),
        "unknown_error"
    );
    assert_eq!(
        transcript_mcp_failure_category(
            &serde_json::json!({"content": [{"text": "Generic error"}, {"text": "Request timed out"}]})
        ),
        "timeout"
    );
}

#[test]
fn failed_call_page_paginates_and_groups_categories() {
    let store = mcp_test_store();
    for (key, category) in [
        ("session:1", "disconnect"),
        ("session:2", "disconnect"),
        ("session:3", "mcp_error"),
    ] {
        store.record_proxy_call(key, "relay", "tool").unwrap();
        store
            .finish_proxy_call(key, "failed", Some(category), None, None, None)
            .unwrap();
    }
    let page = store.mcp_failed_calls(0, 2).unwrap();
    assert_eq!(page.total, 3);
    assert_eq!(page.calls.len(), 2);
    assert_eq!(
        page.categories
            .iter()
            .map(|item| (item.category.as_str(), item.count))
            .collect::<Vec<_>>(),
        vec![("disconnect", 2), ("mcp_error", 1)]
    );
    assert_eq!(store.mcp_failed_calls(2, 2).unwrap().calls.len(), 1);
}

#[test]
fn proxy_calls_are_opaque_bounded_and_idempotent() {
    let store = mcp_test_store();
    let key = "session-uuid:1";
    store
        .record_proxy_call(key, "a\\nserver", "tool<script>")
        .unwrap();
    store.record_proxy_call(key, "different", "other").unwrap();
    assert!(store
        .finish_proxy_call(
            key,
            "failed",
            Some("secret error response"),
            None,
            None,
            None
        )
        .is_err());
    store
        .finish_proxy_call(key, "failed", Some("mcp_error"), None, None, None)
        .unwrap();
    store
        .finish_proxy_call(key, "success", None, None, None, None)
        .unwrap();
    let stats = store
        .mcp_call_stats(None, None, Some("stdio_proxy"), None, None, 5)
        .unwrap();
    assert_eq!(
        (stats.total_calls, stats.proxy_calls, stats.failed_calls),
        (1, 1, 1)
    );
    let call = &stats.recent[0];
    assert!(call.result_observed);
    assert!(call.server_name.starts_with("invalid-"));
    assert!(call.tool_name.starts_with("invalid-"));
    assert_eq!(call.origin, "stdio_proxy");
    assert_eq!(call.error_category.as_deref(), Some("mcp_error"));
    assert_eq!(call.session_id.len(), 64);
    assert!(!call.session_id.contains("session-uuid"));
    let conn = store.conn.lock().unwrap();
    let key_on_disk: String = conn
        .query_row(
            "SELECT observation_key FROM mcp_call_observations",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(!key_on_disk.contains("session-uuid"));
    drop(conn);
    store
        .record_proxy_call("session-uuid:2", "safe", "tool")
        .unwrap();
    store
        .finish_proxy_call(
            "session-uuid:2",
            "failed",
            Some("disconnect"),
            None,
            None,
            None,
        )
        .unwrap();
    assert!(
        !store
            .mcp_call_stats(None, None, None, Some("safe"), None, 5)
            .unwrap()
            .recent[0]
            .result_observed
    );
}

#[test]
fn proxy_stats_optional_filters_and_transcript_refresh_preserve_relay() {
    let store = mcp_test_store();
    store
        .record_proxy_call("session:1", "relay", "first")
        .unwrap();
    store
        .finish_proxy_call("session:1", "success", None, None, None, None)
        .unwrap();
    let call = ParsedMcpCall {
        key: "claude:session:tool-1".into(),
        source: "claude".into(),
        session_id: "session".into(),
        occurred_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
        server_name: "native".into(),
        tool_name: "second".into(),
        status: "pending".into(),
        error_category: None,
        result_observed: false,
    };
    {
        let mut conn = store.conn.lock().unwrap();
        let tx = conn.transaction().unwrap();
        replace_transcript_mcp_calls(&tx, &[call]).unwrap();
        tx.commit().unwrap();
    }
    let all = store
        .mcp_call_stats(None, None, None, None, None, 10)
        .unwrap();
    assert_eq!(
        (all.total_calls, all.proxy_calls, all.transcript_calls),
        (2, 1, 1)
    );
    let filtered = store
        .mcp_call_stats(None, None, None, Some("relay"), Some("first"), 10)
        .unwrap();
    assert_eq!((filtered.total_calls, filtered.successful_calls), (1, 1));
    assert_eq!(
        store
            .mcp_call_stats(None, None, Some("absent"), None, None, 10)
            .unwrap()
            .total_calls,
        0
    );
    assert_eq!(
        store
            .mcp_call_stats(Some("2000-01-01T00:00:00.000Z"), None, None, None, None, 10)
            .unwrap()
            .total_calls,
        2
    );
    assert_eq!(
        store
            .mcp_call_stats(None, Some("2000-01-01T00:00:00.000Z"), None, None, None, 10)
            .unwrap()
            .total_calls,
        0
    );
    {
        let mut conn = store.conn.lock().unwrap();
        let tx = conn.transaction().unwrap();
        replace_transcript_mcp_calls(&tx, &[]).unwrap();
        tx.commit().unwrap();
    }
    assert_eq!(
        store
            .mcp_call_stats(None, None, None, None, None, 10)
            .unwrap()
            .proxy_calls,
        1
    );
}

#[test]
fn unknown_observation_is_not_failure_and_late_response_corrects_it() {
    let store = mcp_test_store();
    store.proxy_connection_open("connection", "pov", "assignment").unwrap();
    store.record_proxy_call("connection:1", "pov", "screenshot").unwrap();
    store.mark_proxy_call_unknown("connection:1", "observer_metadata_limit").unwrap();
    let stats = store.mcp_call_stats(None, None, None, None, None, 10).unwrap();
    assert_eq!((stats.pending_calls, stats.unknown_calls, stats.failed_calls), (0, 1, 0));
    assert_eq!(stats.recent[0].status, "unknown");
    assert!(!stats.recent[0].result_observed);
    assert_eq!(stats.tools[0].unknown, 1);
    store.finish_proxy_call("connection:1", "success", None, None, None, None).unwrap();
    let stats = store.mcp_call_stats(None, None, None, None, None, 10).unwrap();
    assert_eq!((stats.successful_calls, stats.unknown_calls), (1, 0));
    assert!(stats.recent[0].result_observed);
    store.record_proxy_call("connection:2", "pov", "tool-error").unwrap();
    store.mark_proxy_call_unknown("connection:2", "observer_queue_overflow").unwrap();
    store.finish_proxy_call("connection:2", "failed", Some("mcp_error"), Some("response"), Some("tool_error"), None).unwrap();
    let stats = store.mcp_call_stats(None, None, None, None, None, 10).unwrap();
    assert_eq!((stats.successful_calls, stats.failed_calls, stats.unknown_calls), (1, 1, 0));
}

#[test]
fn heartbeat_loss_and_evidence_age_have_different_unknown_reasons() {
    let store = mcp_test_store();
    store.proxy_connection_open("active", "pov", "assignment").unwrap();
    store.proxy_connection_open("lost", "pov", "assignment").unwrap();
    store.record_proxy_call("active:1", "pov", "old-active").unwrap();
    store.record_proxy_call("lost:1", "pov", "old-lost").unwrap();
    store.record_proxy_call("active:2", "pov", "fresh").unwrap();
    {
        let conn = store.conn.lock().unwrap();
        conn.execute("UPDATE mcp_call_observations SET occurred_at = '2000-01-01T00:00:00.000Z' WHERE tool_name != 'fresh'", []).unwrap();
        conn.execute("UPDATE mcp_proxy_connections SET last_seen_at = '2000-01-01T00:00:00.000Z' WHERE connection_id = ?1", [opaque_proxy_id("lost").unwrap()]).unwrap();
    }
    let stats = store.mcp_call_stats(None, None, None, None, None, 10).unwrap();
    assert_eq!((stats.pending_calls, stats.unknown_calls, stats.failed_calls), (1, 2, 0));
    assert_eq!(stats.recent.iter().find(|call| call.tool_name == "old-active").unwrap().error_reason.as_deref(), Some("result_unconfirmed"));
    assert_eq!(stats.recent.iter().find(|call| call.tool_name == "old-lost").unwrap().error_reason.as_deref(), Some("connection_lost"));
    store.proxy_connection_touch("lost").unwrap();
    assert_eq!(store.proxy_active_connections("assignment").unwrap(), 2);
}

#[test]
fn closing_a_connection_finalizes_waiting_calls_as_unknown() {
    let store = mcp_test_store();
    store.proxy_connection_open("close", "pov", "assignment").unwrap();
    store.record_proxy_call("close:1", "pov", "tool").unwrap();
    store.proxy_connection_close("close").unwrap();
    let stats = store.mcp_call_stats(None, None, None, None, None, 10).unwrap();
    assert_eq!((stats.pending_calls, stats.unknown_calls, stats.failed_calls), (0, 1, 0));
    assert_eq!(stats.recent[0].error_reason.as_deref(), Some("connection_closed"));
}

#[test]
fn unknown_projection_migration_is_idempotent_and_preserves_observations() {
    let store = mcp_test_store();
    store.record_proxy_call("migration:1", "pov", "tool").unwrap();
    let mut conn = store.conn.lock().unwrap();
    conn.execute_batch("CREATE TABLE app_settings (setting_key TEXT PRIMARY KEY, value_json TEXT, updated_at TEXT);
        UPDATE mcp_call_observations SET error_category = 'observation_lost', error_reason = 'connection_closed';
        UPDATE activity_ledger SET status = 'pending';").unwrap();
    schema::migrate_mcp_unknown_projection(&mut conn).unwrap();
    schema::migrate_mcp_unknown_projection(&mut conn).unwrap();
    assert_eq!(conn.query_row("SELECT status FROM activity_ledger", [], |row| row.get::<_, String>(0)).unwrap(), "unknown");
    assert_eq!(conn.query_row("SELECT COUNT(*) FROM mcp_call_observations", [], |row| row.get::<_, i64>(0)).unwrap(), 1);
}

#[test]
fn proxy_heartbeat_expires_and_close_removes_connection() {
    let store = mcp_test_store();
    store
        .proxy_connection_open("private-connection", "relay", "assignment-one")
        .unwrap();
    assert_eq!(store.proxy_active_connections("assignment-one").unwrap(), 1);
    assert_eq!(store.proxy_active_connections("assignment-two").unwrap(), 0);
    {
        let conn = store.conn.lock().unwrap();
        let id: String = conn
            .query_row(
                "SELECT connection_id FROM mcp_proxy_connections",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(!id.contains("private-connection"));
        conn.execute(
            "UPDATE mcp_proxy_connections SET last_seen_at = '2000-01-01T00:00:00.000Z'",
            [],
        )
        .unwrap();
    }
    assert_eq!(store.proxy_active_connections("assignment-one").unwrap(), 0);
    store.proxy_connection_touch("private-connection").unwrap();
    assert_eq!(store.proxy_active_connections("assignment-one").unwrap(), 1);
    store.proxy_connection_close("private-connection").unwrap();
    assert_eq!(store.proxy_active_connections("assignment-one").unwrap(), 0);
}

#[test]
fn local_memory_list_keeps_all_custom_items_outside_the_recent_limit() {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(
        "CREATE TABLE local_memory_items (
                id TEXT PRIMARY KEY,
                memory TEXT NOT NULL,
                memory_type TEXT NOT NULL,
                durability TEXT NOT NULL,
                user_defined INTEGER NOT NULL,
                updated_at TEXT NOT NULL,
                event_time TEXT
            );",
    )
    .unwrap();
    conn.execute(
        "INSERT INTO local_memory_items VALUES (?1, ?2, 'fact', 'long_term', 1, ?3, ?3)",
        params![
            "local-user-l3:old",
            "older custom memory",
            "2020-01-01T00:00:00Z"
        ],
    )
    .unwrap();
    for index in 1..=3 {
        conn.execute(
            "INSERT INTO local_memory_items VALUES (?1, ?2, 'fact', 'short_term', 0, ?3, ?3)",
            params![
                format!("local-l1:{index}"),
                format!("automatic memory {index}"),
                format!("2026-08-2{index}T00:00:00Z")
            ],
        )
        .unwrap();
    }
    let store = TelemetryStore::from_connection(conn);

    let memories = store.local_l1_memories(2).unwrap();
    assert_eq!(memories.len(), 3);
    assert!(memories
        .iter()
        .any(|memory| memory.id == "local-user-l3:old" && memory.user_defined));
    assert_eq!(
        memories
            .iter()
            .filter(|memory| !memory.user_defined)
            .count(),
        2
    );
}

#[test]
fn keyword_prefilter_escapes_like_wildcards() {
    assert_eq!(like_escape("a_b%c\\d"), "a\\_b\\%c\\\\d");
}

#[test]
fn keyword_search_reaches_memories_beyond_the_recent_window() {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(
        "CREATE TABLE local_memory_items (
                id TEXT PRIMARY KEY,
                memory TEXT NOT NULL,
                memory_type TEXT NOT NULL,
                durability TEXT NOT NULL,
                user_defined INTEGER NOT NULL,
                updated_at TEXT NOT NULL,
                event_time TEXT
            );
            CREATE TABLE memory_importance (
               memory_id TEXT PRIMARY KEY,
               score INTEGER NOT NULL DEFAULT 0,
               supporting_sessions INTEGER NOT NULL DEFAULT 0,
               supporting_agents INTEGER NOT NULL DEFAULT 0,
               duplicate_count INTEGER NOT NULL DEFAULT 0,
               recall_count INTEGER NOT NULL DEFAULT 0,
               pinned INTEGER NOT NULL DEFAULT 0,
               updated_at TEXT NOT NULL
            );",
    )
    .unwrap();
    // 600 auto-extracted rows: the previous implementation only ever
    // scored the most recent 500, leaving the oldest row unreachable
    // whenever the semantic sidecar was offline.
    for index in 0..600 {
        conn.execute(
            "INSERT INTO local_memory_items VALUES (?1, ?2, 'fact', 'short_term', 0, ?3, ?3)",
            params![
                format!("local-l1:{index}"),
                format!("日常填充记忆 {index}"),
                format!("2026-09-{:02}T00:00:00Z", (index % 28) + 1)
            ],
        )
        .unwrap();
    }
    conn.execute(
            "INSERT INTO local_memory_items VALUES (?1, ?2, 'fact', 'long_term', 0, '2020-01-01T00:00:00Z', '2020-01-01T00:00:00Z')",
            params!["local-l1:ancient", "上古遗留的氪石冶炼约束"],
        )
        .unwrap();
    let store = TelemetryStore::from_connection(conn);

    let hits = store.search_local_l1_memories("氪石冶炼", 5).unwrap();
    assert!(hits.iter().any(|memory| memory.id == "local-l1:ancient"));
    // "日常填充记忆" 不含查询词的任何 bigram，不得混入结果。
    assert!(hits
        .iter()
        .all(|memory| !memory.memory.contains("日常填充")));
}

#[test]
fn injection_stats_separate_real_injections_from_gate_skips() {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(
            "CREATE TABLE mcp_access_logs (
               id INTEGER PRIMARY KEY AUTOINCREMENT,
               occurred_at TEXT NOT NULL,
               client_name TEXT NOT NULL,
               tool_name TEXT NOT NULL,
               summary TEXT NOT NULL,
               detail TEXT,
               success INTEGER NOT NULL DEFAULT 1
             );
             CREATE TABLE activity_ledger (
                id INTEGER PRIMARY KEY AUTOINCREMENT, access_log_id INTEGER,
                domain TEXT, event_kind TEXT, occurred_at TEXT, client_name TEXT,
                status TEXT, payload_chars INTEGER
             );
             CREATE TRIGGER test_hook_access_insert AFTER INSERT ON mcp_access_logs BEGIN
                INSERT INTO activity_ledger
                  (access_log_id, domain, event_kind, occurred_at, client_name, status, payload_chars)
                VALUES (NEW.id,
                  CASE WHEN NEW.tool_name IN ('session_start_inject', 'prompt_inject') THEN 'hook' ELSE 'mcp' END,
                  CASE WHEN NEW.tool_name = 'session_start_inject' THEN 'hook_session_injection'
                       WHEN NEW.tool_name = 'prompt_inject' AND NEW.detail IS NULL THEN 'hook_skip'
                       WHEN NEW.tool_name = 'prompt_inject' THEN 'hook_prompt_injection'
                       ELSE 'mcp_tool_call' END,
                  NEW.occurred_at, NEW.client_name,
                  CASE WHEN NEW.success != 0 THEN 'success' ELSE 'failed' END,
                  COALESCE(LENGTH(NEW.detail), 0));
             END;",
        )
        .unwrap();
    let insert = |occurred_at: &str, client: &str, tool: &str, detail: Option<&str>| {
        conn.execute(
                "INSERT INTO mcp_access_logs (occurred_at, client_name, tool_name, summary, detail, success)
                 VALUES (?1, ?2, ?3, '审计摘要', ?4, 1)",
                params![occurred_at, client, tool, detail],
            )
            .unwrap();
    };
    // Keep fixtures inside the moving 30-day window.
    let recent_day = (chrono::Utc::now() - chrono::Duration::days(1))
        .date_naive()
        .to_string();
    let recent = |hour: u8| format!("{recent_day}T{hour:02}:00:00Z");
    // Claude：两次真实注入（含 CJK 正文）+ 三次指纹跳过 + 一次启动注入。
    insert(
        &recent(0),
        "Claude Code",
        "session_start_inject",
        Some("L3 长期记忆正文"),
    );
    insert(
        &recent(1),
        "Claude Code",
        "prompt_inject",
        Some("每轮注入正文"),
    );
    insert(
        &recent(2),
        "Claude Code",
        "prompt_inject",
        Some("每轮注入正文"),
    );
    insert(&recent(3), "Claude Code", "prompt_inject", None);
    insert(&recent(4), "Claude Code", "prompt_inject", None);
    insert(&recent(5), "Claude Code", "prompt_inject", None);
    // Codex：仅一次真实注入，字符数更多，应排在 Claude 之前。
    insert(
        &recent(6),
        "Codex",
        "prompt_inject",
        Some("更长的注入正文内容"),
    );
    // 窗口外与无关工具行不计入。
    insert(
        "2020-01-01T00:00:00Z",
        "Claude Code",
        "prompt_inject",
        Some("远古记录不应计入"),
    );
    insert(
        &recent(7),
        "Claude Code",
        "recall_memory",
        Some("检索正文不算注入"),
    );
    let store = TelemetryStore::from_connection(conn);

    let stats = store.memory_injection_stats(30).unwrap();
    assert_eq!(stats.len(), 2, "两个 agent 各占一行");
    // 注入字符按 LENGTH 口径（Unicode 字符数，与写入侧 chars().count() 一致）。
    let codex = stats.iter().find(|row| row.client_name == "Codex").unwrap();
    assert_eq!(codex.prompt_injections, 1);
    assert_eq!(codex.prompt_skips, 0);
    assert_eq!(codex.session_injections, 0);
    assert_eq!(
        codex.injected_chars,
        "更长的注入正文内容".chars().count() as u64
    );
    let claude = stats
        .iter()
        .find(|row| row.client_name == "Claude Code")
        .unwrap();
    assert_eq!(claude.session_injections, 1);
    assert_eq!(claude.prompt_injections, 2);
    assert_eq!(claude.prompt_skips, 3);
    let expected = "L3 长期记忆正文".chars().count() + 2 * "每轮注入正文".chars().count();
    assert_eq!(claude.injected_chars, expected as u64);
    assert_eq!(claude.prompt_skips, 3, "窗口外的远古行不得混入统计");
}

#[test]
fn l0_conversation_omits_assistant_thinking_before_extraction() {
    let messages = vec![
        ("user".to_string(), "记住项目使用 SQLite".to_string()),
        (
            "assistant".to_string(),
            "<thinking>先分析</thinking>已记录。".to_string(),
        ),
    ];
    let conversation = format_conversation(&messages);
    assert!(conversation.contains("已记录。"));
    assert!(!conversation.contains("先分析"));
}

#[test]
fn usage_parser_distinguishes_missing_from_reported_values() {
    assert_eq!(
        extract_usage(&serde_json::json!({})),
        ("unavailable", 0, 0, 0)
    );
    assert_eq!(
        extract_usage(&serde_json::json!({"usage": {"input_tokens": 5, "output_tokens": 7}})),
        ("reported", 5, 7, 0)
    );
}

#[test]
fn archived_codex_rollout_name_keeps_its_session_id() {
    let path = Path::new("C:/Users/test/.codex/archived_sessions/rollout-2026-07-23T18-40-43-019f8e90-36f5-71a2-bddf-f0fe9ee23b57.jsonl");
    assert_eq!(
        transcript_session_id("codex", path).as_deref(),
        Some("019f8e90-36f5-71a2-bddf-f0fe9ee23b57"),
    );
    assert_eq!(
        transcript_session_id(
            "claude",
            Path::new("C:/Users/test/.claude/projects/demo/session-1.jsonl")
        )
        .as_deref(),
        Some("session-1"),
    );
}

#[test]
fn metadata_discovery_finds_an_archived_codex_transcript_without_a_hook() {
    let root =
        std::env::temp_dir().join(format!("agent-manager-archived-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(root.join("nested")).unwrap();
    let session_id = "019f8e90-36f5-71a2-bddf-f0fe9ee23b57";
    let path = root
        .join("nested")
        .join(format!("rollout-2026-07-23T18-40-43-{session_id}.jsonl"));
    std::fs::write(&path, "{}").unwrap();
    let mut found = HashMap::new();
    scan_all_transcript_tree(&root, "codex", &mut found);
    assert_eq!(
        found
            .get(&("codex".to_string(), session_id.to_string()))
            .map(|(_, found_path)| found_path),
        Some(&path),
    );
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn native_transcript_usage_uses_one_codex_session_total() {
    let path = std::env::temp_dir().join(format!(
        "agent-manager-usage-{}.jsonl",
        uuid::Uuid::new_v4()
    ));
    let rows = [
        serde_json::json!({"type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":100,"cached_input_tokens":40,"output_tokens":10}}}}),
        serde_json::json!({"type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":240,"cached_input_tokens":100,"output_tokens":35}}}}),
    ];
    std::fs::write(
        &path,
        rows.iter()
            .map(Value::to_string)
            .collect::<Vec<_>>()
            .join("\n"),
    )
    .unwrap();
    let usage = read_codex_transcript_usage(&path).unwrap();
    assert_eq!(
        (usage.input_tokens, usage.output_tokens, usage.cached_tokens),
        (340, 35, 100)
    );
    let _ = std::fs::remove_file(path);
}

#[test]
fn codex_usage_records_turn_cumulative_snapshots_into_idempotent_deltas() {
    let path = std::env::temp_dir().join(format!(
        "agent-manager-usage-records-{}.jsonl",
        uuid::Uuid::new_v4()
    ));
    let rows = [
        serde_json::json!({"timestamp":"2026-08-16T08:00:00Z","type":"turn_context","payload":{"model":"gpt-5.6-terra"}}),
        serde_json::json!({"timestamp":"2026-08-16T08:00:01Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":100,"cached_input_tokens":50,"output_tokens":10}}}}),
        serde_json::json!({"timestamp":"2026-08-16T08:00:02Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":100,"cached_input_tokens":50,"output_tokens":10}}}}),
        serde_json::json!({"timestamp":"2026-08-16T08:00:03Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":140,"cached_input_tokens":80,"output_tokens":25}}}}),
    ];
    std::fs::write(
        &path,
        rows.iter()
            .map(Value::to_string)
            .collect::<Vec<_>>()
            .join("\n"),
    )
    .unwrap();
    let records = read_codex_usage_records("session-1", &path);
    assert_eq!(records.len(), 2);
    assert_eq!(
        records
            .iter()
            .map(|record| record.input_tokens)
            .sum::<i64>(),
        220
    );
    assert_eq!(
        records
            .iter()
            .map(|record| record.output_tokens)
            .sum::<i64>(),
        25
    );
    assert_eq!(
        records
            .iter()
            .map(|record| record.cached_tokens)
            .sum::<i64>(),
        80
    );
    assert!(records
        .iter()
        .all(|record| record.model.as_deref() == Some("gpt-5.6-terra")));
    let _ = std::fs::remove_file(path);
}

#[test]
fn minimax_transcript_usage_deduplicates_by_response_id() {
    let path = std::env::temp_dir().join(format!(
        "agent-manager-minimax-usage-{}.jsonl",
        uuid::Uuid::new_v4()
    ));
    let rows = [
        serde_json::json!({"message_id":"msg-1","message":{"role":"user","content":[{"type":"text","text":"你好"}]}}),
        serde_json::json!({"message_id":"msg-2","message":{"role":"assistant","model":"MiniMax-M3","responseId":"r-1","usage":{"input":100,"output":20,"cacheRead":40,"cacheWrite":10}}}),
        serde_json::json!({"message_id":"msg-2","message":{"role":"assistant","model":"MiniMax-M3","responseId":"r-1","usage":{"input":100,"output":20,"cacheRead":40,"cacheWrite":10}}}),
        serde_json::json!({"message_id":"msg-3","message":{"role":"assistant","model":"MiniMax-M3","responseId":"r-2","usage":{"input":7,"output":3,"cacheRead":0,"cacheWrite":0}}}),
    ];
    std::fs::write(
        &path,
        rows.iter()
            .map(Value::to_string)
            .collect::<Vec<_>>()
            .join("\n"),
    )
    .unwrap();
    let usage = read_minimax_transcript_usage(&path).unwrap();
    // 输入 = input + cacheRead + cacheWrite；重复 responseId 只计一次。
    assert_eq!(
        (usage.input_tokens, usage.output_tokens, usage.cached_tokens),
        (157, 23, 40)
    );
    let records = read_minimax_usage_records("session-1", &path);
    assert_eq!(records.len(), 2);
    assert!(records
        .iter()
        .all(|record| record.model.as_deref() == Some("MiniMax-M3")));
    let _ = std::fs::remove_file(path);
}

#[test]
fn kimi_transcript_usage_sums_turn_records_once() {
    let path = std::env::temp_dir().join(format!(
        "agent-manager-kimi-usage-{}.jsonl",
        uuid::Uuid::new_v4()
    ));
    let rows = [
        serde_json::json!({"type":"usage.record","model":"k3-agent","usageScope":"turn","time":1787051603421i64,"usage":{"inputOther":100,"output":20,"inputCacheRead":40,"inputCacheCreation":10}}),
        // 文件重写产生的完全重复事件不得再次累计。
        serde_json::json!({"type":"usage.record","model":"k3-agent","usageScope":"turn","time":1787051603421i64,"usage":{"inputOther":100,"output":20,"inputCacheRead":40,"inputCacheCreation":10}}),
        serde_json::json!({"type":"usage.record","model":"k3-agent","usageScope":"turn","time":1787051609500i64,"usage":{"inputOther":7,"output":3,"inputCacheRead":0,"inputCacheCreation":0}}),
    ];
    std::fs::write(
        &path,
        rows.iter()
            .map(Value::to_string)
            .collect::<Vec<_>>()
            .join("\n"),
    )
    .unwrap();
    let usage = read_kimi_transcript_usage(&path).unwrap();
    assert_eq!(
        (usage.input_tokens, usage.output_tokens, usage.cached_tokens),
        (157, 23, 40)
    );
    let records = read_kimi_usage_records("conv-1", &path);
    assert_eq!(records.len(), 2);
    assert!(records
        .iter()
        .all(|record| record.model.as_deref() == Some("k3-agent")));
    assert!(records.iter().all(|record| record.occurred_at.is_some()));
    let _ = std::fs::remove_file(path);
}

#[test]
fn native_transcript_usage_deduplicates_claude_tool_rows_by_message_id() {
    let path = std::env::temp_dir().join(format!(
        "agent-manager-usage-{}.jsonl",
        uuid::Uuid::new_v4()
    ));
    let rows = [
        serde_json::json!({"type":"assistant","message":{"id":"response-1","usage":{"input_tokens":5,"cache_read_input_tokens":20,"output_tokens":3}}}),
        serde_json::json!({"type":"assistant","message":{"id":"response-1","usage":{"input_tokens":5,"cache_read_input_tokens":20,"output_tokens":3}}}),
        serde_json::json!({"type":"assistant","message":{"id":"response-2","usage":{"input_tokens":7,"cache_creation_input_tokens":2,"output_tokens":4}}}),
    ];
    std::fs::write(
        &path,
        rows.iter()
            .map(Value::to_string)
            .collect::<Vec<_>>()
            .join("\n"),
    )
    .unwrap();
    let usage = read_claude_transcript_usage(&path).unwrap();
    assert_eq!(
        (usage.input_tokens, usage.output_tokens, usage.cached_tokens),
        (34, 7, 20)
    );
    let _ = std::fs::remove_file(path);
}

#[test]
fn claude_usage_records_deduplicate_tool_rows_by_message_id() {
    let path = std::env::temp_dir().join(format!(
        "agent-manager-usage-records-{}.jsonl",
        uuid::Uuid::new_v4()
    ));
    let rows = [
        serde_json::json!({"timestamp":"2026-08-16T08:00:00Z","type":"assistant","message":{"id":"response-1","model":"claude-sonnet","usage":{"input_tokens":5,"cache_read_input_tokens":20,"output_tokens":3}}}),
        serde_json::json!({"timestamp":"2026-08-16T08:00:00Z","type":"assistant","message":{"id":"response-1","model":"claude-sonnet","usage":{"input_tokens":5,"cache_read_input_tokens":20,"output_tokens":3}}}),
    ];
    std::fs::write(
        &path,
        rows.iter()
            .map(Value::to_string)
            .collect::<Vec<_>>()
            .join("\n"),
    )
    .unwrap();
    let records = read_claude_usage_records("session-1", &path);
    assert_eq!(records.len(), 1);
    assert_eq!(
        (
            records[0].input_tokens,
            records[0].output_tokens,
            records[0].cached_tokens
        ),
        (25, 3, 20)
    );
    let _ = std::fs::remove_file(path);
}

#[test]
fn claude_zero_usage_placeholder_does_not_shadow_real_usage() {
    // 同一 message id 先以零 usage 占位、稍后补写真实计数：占位行
    // 不应占用去重名额，否则整个会话会被降级成粗估甚至计零。
    let dir = std::env::temp_dir().join(format!(
        "agent-manager-claude-shadow-{}",
        uuid::Uuid::new_v4()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("session.jsonl");
    let rows = [
        serde_json::json!({"timestamp":"2026-08-31T12:00:00Z","type":"assistant","message":{"id":"resp-1","model":"claude-sonnet","usage":{"input_tokens":0,"output_tokens":0}}}),
        serde_json::json!({"timestamp":"2026-08-31T12:00:00Z","type":"assistant","message":{"id":"resp-1","model":"claude-sonnet","usage":{"input_tokens":7309,"cache_read_input_tokens":25280,"output_tokens":434}}}),
    ];
    std::fs::write(
        &path,
        rows.iter()
            .map(Value::to_string)
            .collect::<Vec<_>>()
            .join("\n"),
    )
    .unwrap();
    let records = read_claude_usage_records("session-1", &path);
    assert_eq!(records.len(), 1);
    assert_eq!(
        (records[0].input_tokens, records[0].output_tokens),
        (32589, 434)
    );
    let total = read_claude_transcript_usage(&path).unwrap();
    assert_eq!((total.input_tokens, total.output_tokens), (32589, 434));
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn claude_sidechain_agent_files_are_attributed_to_parent_session() {
    // Claude Code 把子代理转录写在 <project>/<父会话id>/subagents/
    // agent-*.jsonl，其中 assistant 记录带 isSidechain 与父会话
    // sessionId。用量必须归属回父会话，且不误收其他父会话的
    // agent 文件。
    let dir = std::env::temp_dir().join(format!(
        "agent-manager-claude-sidechain-{}",
        uuid::Uuid::new_v4()
    ));
    std::fs::create_dir_all(dir.join("session-1").join("subagents")).unwrap();
    let main = dir.join("session.jsonl");
    std::fs::write(
            &main,
            serde_json::json!({"timestamp":"2026-09-19T08:00:00Z","type":"assistant","message":{"id":"resp-1","model":"claude-sonnet","usage":{"input_tokens":100,"output_tokens":10}}}).to_string(),
        )
        .unwrap();
    // 会话级布局：<project>/<父会话id>/subagents/agent-*.jsonl
    let own_agent = dir
        .join("session-1")
        .join("subagents")
        .join("agent-own.jsonl");
    std::fs::write(
            &own_agent,
            [
                serde_json::json!({"timestamp":"2026-09-19T08:01:00Z","type":"assistant","sessionId":"session-1","isSidechain":true,"message":{"id":"sub-1","model":"claude-haiku","usage":{"input_tokens":1000,"cache_read_input_tokens":500,"output_tokens":200}}}),
                // 同 id 零 usage 占位行不遮蔽真实计数
                serde_json::json!({"timestamp":"2026-09-19T08:02:00Z","type":"assistant","sessionId":"session-1","isSidechain":true,"message":{"id":"sub-1","model":"claude-haiku","usage":{"input_tokens":0,"output_tokens":0}}}),
                // 无 sessionId 的行不归属任何会话
                serde_json::json!({"timestamp":"2026-09-19T08:03:00Z","type":"assistant","isSidechain":true,"message":{"id":"sub-2","model":"claude-haiku","usage":{"input_tokens":999,"output_tokens":999}}}),
            ]
            .iter()
            .map(Value::to_string)
            .collect::<Vec<_>>()
            .join("\n"),
        )
        .unwrap();
    let other_agent = dir.join("agent-other.jsonl");
    std::fs::write(
            &other_agent,
            serde_json::json!({"timestamp":"2026-09-19T08:04:00Z","type":"assistant","sessionId":"session-2","isSidechain":true,"message":{"id":"sub-3","model":"claude-haiku","usage":{"input_tokens":7777,"output_tokens":77}}}).to_string(),
        )
        .unwrap();
    let records = read_claude_usage_records("session-1", &main);
    // 主会话 1 条 + 会话级 subagents/ 里的子代理 1 条；session-2 的
    // agent 文件与无 sessionId 的行都不计入。
    assert_eq!(records.len(), 2);
    let sidechain = records
        .iter()
        .find(|record| record.record_id.contains("sidechain"))
        .unwrap();
    assert_eq!(
        (
            sidechain.input_tokens,
            sidechain.output_tokens,
            sidechain.cached_tokens
        ),
        (1500, 200, 500)
    );
    let summary = summarize_usage_records(&records);
    assert_eq!((summary.input_tokens, summary.output_tokens), (1600, 210));
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn usage_feed_prefers_response_records_and_falls_back_to_session_totals() {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(
        "CREATE TABLE agent_events (source TEXT, session_id TEXT, occurred_at TEXT);
             CREATE TABLE session_token_usage (
               source TEXT NOT NULL, session_id TEXT NOT NULL, input_tokens INTEGER NOT NULL,
               output_tokens INTEGER NOT NULL, cached_tokens INTEGER NOT NULL, origin TEXT NOT NULL,
               refreshed_at TEXT NOT NULL, PRIMARY KEY(source, session_id)
             );
             CREATE TABLE usage_records (
               record_id TEXT PRIMARY KEY, source TEXT NOT NULL, session_id TEXT NOT NULL,
               occurred_at TEXT NOT NULL, model TEXT, record_kind TEXT NOT NULL,
               input_tokens INTEGER NOT NULL, output_tokens INTEGER NOT NULL,
               cached_tokens INTEGER NOT NULL, origin TEXT NOT NULL
             );
             CREATE TABLE usage_remote_totals (
               device TEXT NOT NULL, day TEXT NOT NULL, source TEXT NOT NULL,
               input_tokens INTEGER NOT NULL DEFAULT 0, output_tokens INTEGER NOT NULL DEFAULT 0,
               cached_tokens INTEGER NOT NULL DEFAULT 0, record_count INTEGER NOT NULL DEFAULT 0,
               PRIMARY KEY (device, day, source)
             );",
    )
    .unwrap();
    conn.execute(
            "INSERT INTO session_token_usage VALUES ('codex', 'with-details', 100, 10, 80, 'native_transcript', '2026-08-16T08:00:00Z')",
            [],
        ).unwrap();
    conn.execute(
            "INSERT INTO session_token_usage VALUES ('qoder', 'fallback', 40, 5, 0, 'estimated_transcript', '2026-08-16T08:01:00Z')",
            [],
        ).unwrap();
    conn.execute(
            "INSERT INTO usage_records VALUES ('codex:1', 'codex', 'with-details', '2026-08-16T08:00:02Z', 'gpt-5.6-terra', 'response', 100, 10, 80, 'native_transcript')",
            [],
        ).unwrap();
    let store = TelemetryStore::from_connection(conn);
    let rows = store.usage_records(20).unwrap();
    assert_eq!(rows.len(), 2);
    assert!(rows
        .iter()
        .any(|row| row.record_id == "codex:1" && row.record_kind == "response"));
    assert!(rows
        .iter()
        .any(|row| row.session_id == "fallback" && row.record_kind == "session_total"));
    let analytics = store
        .usage_analytics(
            Some("2026-08-16T08:00:00Z"),
            Some("2026-08-16T09:00:00Z"),
            None,
            "hour",
            50,
        )
        .unwrap();
    assert_eq!(analytics.record_count, 2);
    assert_eq!(
        (
            analytics.input_tokens,
            analytics.output_tokens,
            analytics.cached_tokens
        ),
        (140, 15, 80)
    );
    assert_eq!(
        analytics
            .buckets
            .iter()
            .map(|bucket| bucket.record_count)
            .sum::<i64>(),
        2
    );
    assert_eq!(
        analytics
            .heatmap
            .iter()
            .map(|cell| cell.record_count)
            .sum::<i64>(),
        2
    );
    assert_eq!(analytics.cost_groups.len(), 2);
    assert_eq!(
        analytics
            .cost_groups
            .iter()
            .map(|group| group.input_tokens)
            .sum::<i64>(),
        140
    );
    let codex = analytics
        .cost_groups
        .iter()
        .find(|group| group.source == "codex")
        .unwrap();
    assert_eq!(codex.model.as_deref(), Some("gpt-5.6-terra"));
}

#[test]
fn native_transcript_conversation_enters_the_same_l1_queue_without_a_hook() {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(
            "CREATE TABLE agent_events (
                id INTEGER PRIMARY KEY AUTOINCREMENT, event_key TEXT NOT NULL UNIQUE,
                source TEXT NOT NULL, session_id TEXT NOT NULL, project_path TEXT,
                event_type TEXT NOT NULL, occurred_at TEXT NOT NULL,
                token_source TEXT NOT NULL, input_tokens INTEGER NOT NULL, output_tokens INTEGER NOT NULL,
                cached_tokens INTEGER NOT NULL, payload_json TEXT NOT NULL,
                conversation_state TEXT NOT NULL DEFAULT 'unavailable',
                conversation_message_count INTEGER NOT NULL DEFAULT 0, conversation_text TEXT,
                l1_state TEXT NOT NULL DEFAULT 'unavailable'
             );
             CREATE TABLE session_token_usage (
                source TEXT NOT NULL, session_id TEXT NOT NULL, input_tokens INTEGER NOT NULL,
                output_tokens INTEGER NOT NULL, cached_tokens INTEGER NOT NULL, origin TEXT NOT NULL,
                refreshed_at TEXT NOT NULL, PRIMARY KEY(source, session_id)
             );
             CREATE TABLE local_memory_items (
                id TEXT PRIMARY KEY, source_event_key TEXT NOT NULL, ordinal INTEGER NOT NULL,
                memory TEXT NOT NULL, memory_type TEXT NOT NULL, durability TEXT NOT NULL DEFAULT 'short_term', user_defined INTEGER NOT NULL DEFAULT 0, source TEXT NOT NULL,
                session_id TEXT NOT NULL, event_time TEXT, created_at TEXT NOT NULL, updated_at TEXT NOT NULL,
                UNIQUE(source_event_key, ordinal)
             );",
        ).unwrap();
    let store = TelemetryStore::from_connection(conn);
    let transcript = std::env::temp_dir().join("qoder-session-1.jsonl");
    let messages = vec![
        ("user".to_string(), "保留这个决定".to_string()),
        ("assistant".to_string(), "已使用 SQLite。".to_string()),
    ];
    assert!(store
        .record_native_conversation(
            "qoder",
            "qoder-1",
            &transcript,
            "2026-08-16T08:00:00Z",
            &messages
        )
        .unwrap());
    let pending = store.pending_l1_conversations(10).unwrap();
    assert_eq!(pending.len(), 1);
    store
        .store_typed_l1_memories(
            &pending[0].event_key,
            &[L1MemoryCandidate {
                content: "项目决定：使用 SQLite。".into(),
                memory_type: "decision".into(),
                durability: "short_term".into(),
            }],
        )
        .unwrap();
    assert!(!store
        .record_native_conversation(
            "qoder",
            "qoder-1",
            &transcript,
            "2026-08-16T08:00:00Z",
            &messages
        )
        .unwrap());
    assert_eq!(store.pending_l1_conversations(10).unwrap().len(), 0);
}

#[test]
fn native_transcript_usage_reads_workbuddy_nested_usage_once_per_record() {
    let path = std::env::temp_dir().join(format!(
        "agent-manager-usage-{}.jsonl",
        uuid::Uuid::new_v4()
    ));
    let rows = [
        // 蛇形 message.usage，且缓存读取需并入输入。
        serde_json::json!({"id":"call-1","type":"function_call","message":{"usage":{"input_tokens":100,"output_tokens":10,"cache_read_input_tokens":70}}}),
        // 同 id 跨 type 重复，只计一次。
        serde_json::json!({"id":"call-1","type":"message","message":{"usage":{"input_tokens":100,"output_tokens":10,"cache_read_input_tokens":70}}}),
        serde_json::json!({"id":"call-2","type":"function_call","message":{"usage":{"input_tokens":120,"output_tokens":20,"cache_read_input_tokens":90}}}),
        // 只有驼峰 providerData.usage，回退读取并取 inputTokensDetails 缓存。
        serde_json::json!({"id":"call-3","type":"function_call","providerData":{"usage":{"inputTokens":50,"outputTokens":5,"inputTokensDetails":[{"cached_tokens":30}]}}}),
    ];
    std::fs::write(
        &path,
        rows.iter()
            .map(Value::to_string)
            .collect::<Vec<_>>()
            .join("\n"),
    )
    .unwrap();
    let usage = read_workbuddy_transcript_usage(&path).unwrap();
    assert_eq!(
        (usage.input_tokens, usage.output_tokens, usage.cached_tokens),
        (460, 35, 190)
    );
    let _ = std::fs::remove_file(path);
}

#[test]
fn workbuddy_usage_records_read_nested_usage_and_model_from_provider() {
    let path = std::env::temp_dir().join(format!(
        "agent-manager-wb-records-{}.jsonl",
        uuid::Uuid::new_v4()
    ));
    let rows = [
        serde_json::json!({"id":"call-1","type":"function_call","timestamp":"2026-08-16T08:00:00Z","message":{"usage":{"input_tokens":100,"output_tokens":10,"cache_read_input_tokens":70}},"providerData":{"requestModelName":"gpt-5"}}),
        serde_json::json!({"id":"call-1","type":"message","message":{"usage":{"input_tokens":100,"output_tokens":10,"cache_read_input_tokens":70}},"providerData":{"requestModelName":"gpt-5"}}),
        serde_json::json!({"id":"call-2","type":"function_call","message":{"usage":{"input_tokens":120,"output_tokens":20,"cache_read_input_tokens":90}},"providerData":{"requestModelName":"gpt-5"}}),
    ];
    std::fs::write(
        &path,
        rows.iter()
            .map(Value::to_string)
            .collect::<Vec<_>>()
            .join("\n"),
    )
    .unwrap();
    let records = read_workbuddy_usage_records("session-1", &path);
    assert_eq!(records.len(), 2);
    assert_eq!(records.iter().map(|r| r.input_tokens).sum::<i64>(), 380);
    assert_eq!(records.iter().map(|r| r.output_tokens).sum::<i64>(), 30);
    assert_eq!(records.iter().map(|r| r.cached_tokens).sum::<i64>(), 160);
    assert!(records.iter().all(|r| r.model.as_deref() == Some("gpt-5")));
    let _ = std::fs::remove_file(path);
}

#[test]
fn qoder_transcript_estimate_separates_user_input_and_assistant_output() {
    let path = std::env::temp_dir().join(format!(
        "agent-manager-usage-{}.jsonl",
        uuid::Uuid::new_v4()
    ));
    let rows = [
        serde_json::json!({"type":"user","uuid":"u-1","message":{"content":[{"content":"你好，build api"}]}}),
        serde_json::json!({"type":"assistant","uuid":"a-1","message":{"content":[{"content":"已完成"}]}}),
        serde_json::json!({"type":"assistant","uuid":"a-1","message":{"content":[{"content":"已完成"}]}}),
    ];
    std::fs::write(
        &path,
        rows.iter()
            .map(Value::to_string)
            .collect::<Vec<_>>()
            .join("\n"),
    )
    .unwrap();
    let usage = read_qoder_transcript_estimate(&path).unwrap();
    assert!(usage.input_tokens > 0 && usage.output_tokens > 0);
    assert_eq!(usage.output_tokens, estimate_transcript_tokens("已完成"));
    let _ = std::fs::remove_file(path);
}

#[test]
fn qoder_estimate_counts_cumulative_context_per_request() {
    // 与 Claude/Codex 原生计数同一口径：每条 user 条目（含工具结果）
    // 都触发一次请求，输入为累积的全部上下文加本条，因此多轮会话
    // 的输入总量大于会话文本本身的 token 数。
    let path = std::env::temp_dir().join(format!(
        "agent-manager-qoder-cumulative-{}.jsonl",
        uuid::Uuid::new_v4()
    ));
    let rows = [
        serde_json::json!({"type":"user","uuid":"u-1","message":{"content":[{"content":"第一轮问题"}]}}),
        serde_json::json!({"type":"assistant","uuid":"a-1","message":{"content":[{"content":"回答一"}]}}),
        serde_json::json!({"type":"user","uuid":"u-2","message":{"content":[{"content":"第二轮问题"}]}}),
    ];
    std::fs::write(
        &path,
        rows.iter()
            .map(Value::to_string)
            .collect::<Vec<_>>()
            .join("\n"),
    )
    .unwrap();
    let usage = read_qoder_transcript_estimate(&path).unwrap();
    let first = estimate_transcript_tokens("第一轮问题");
    let reply = estimate_transcript_tokens("回答一");
    let second = estimate_transcript_tokens("第二轮问题");
    assert_eq!(usage.input_tokens, first + (first + reply + second));
    assert_eq!(usage.output_tokens, reply);
    let records = read_qoder_usage_records("qoder-1", &path);
    assert_eq!(
        records
            .iter()
            .map(|record| record.input_tokens)
            .sum::<i64>(),
        usage.input_tokens
    );
    assert_eq!(
        records
            .iter()
            .map(|record| record.output_tokens)
            .sum::<i64>(),
        usage.output_tokens
    );
    let _ = std::fs::remove_file(path);
}

#[test]
fn qoder_estimate_counts_thinking_and_tool_use_as_output() {
    // 思考块内容在 `thinking` 键下、tool_use 参数在 `input` 键下，
    // 都是模型生成的输出 token；漏计会让输出被严重低估。
    let path = std::env::temp_dir().join(format!(
        "agent-manager-qoder-blocks-{}.jsonl",
        uuid::Uuid::new_v4()
    ));
    let rows = [
        serde_json::json!({"type":"user","uuid":"u-1","message":{"content":[{"type":"text","text":"开始"}]}}),
        serde_json::json!({"type":"assistant","uuid":"a-1","message":{"content":[{"type":"thinking","thinking":"先分析需求","signature":"sig"}]}}),
        serde_json::json!({"type":"assistant","uuid":"a-2","message":{"content":[{"type":"tool_use","id":"t-1","name":"Read","input":{"path":"a.rs"}}]}}),
        serde_json::json!({"type":"user","uuid":"u-2","message":{"content":[{"type":"tool_result","content":"文件内容"}]}}),
    ];
    std::fs::write(
        &path,
        rows.iter()
            .map(Value::to_string)
            .collect::<Vec<_>>()
            .join("\n"),
    )
    .unwrap();
    let usage = read_qoder_transcript_estimate(&path).unwrap();
    let thinking = estimate_transcript_tokens("先分析需求");
    let tool_call = estimate_transcript_tokens("Read{\"path\":\"a.rs\"}");
    let tool_result = estimate_transcript_tokens("文件内容");
    assert!(usage.output_tokens >= thinking + tool_call);
    // 工具结果作为 user 条目计入输入，且带累积上下文。
    assert!(usage.input_tokens >= tool_result);
    let _ = std::fs::remove_file(path);
}

#[test]
fn qoder_usage_records_keep_the_same_estimate_per_message() {
    let path = std::env::temp_dir().join(format!(
        "agent-manager-qoder-records-{}.jsonl",
        uuid::Uuid::new_v4()
    ));
    let rows = [
        serde_json::json!({"type":"user","uuid":"u-1","message":{"content":[{"content":"你好，build api"}]}}),
        serde_json::json!({"type":"assistant","uuid":"a-1","message":{"content":[{"content":"已完成"}]}}),
        serde_json::json!({"type":"assistant","uuid":"a-1","message":{"content":[{"content":"重复"}]}}),
    ];
    std::fs::write(
        &path,
        rows.iter()
            .map(Value::to_string)
            .collect::<Vec<_>>()
            .join("\n"),
    )
    .unwrap();
    let summary = read_qoder_transcript_estimate(&path).unwrap();
    let records = read_qoder_usage_records("qoder-1", &path);
    assert_eq!(records.len(), 2);
    assert_eq!(
        records
            .iter()
            .map(|record| record.input_tokens)
            .sum::<i64>(),
        summary.input_tokens
    );
    assert_eq!(
        records
            .iter()
            .map(|record| record.output_tokens)
            .sum::<i64>(),
        summary.output_tokens
    );
    let _ = std::fs::remove_file(path);
}

#[test]
fn final_adapter_usage_overwrites_one_session_instead_of_accumulating() {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(
        "CREATE TABLE session_token_usage (
                source TEXT NOT NULL, session_id TEXT NOT NULL,
                input_tokens INTEGER NOT NULL, output_tokens INTEGER NOT NULL,
                cached_tokens INTEGER NOT NULL, origin TEXT NOT NULL,
                refreshed_at TEXT NOT NULL, PRIMARY KEY(source, session_id)
            );",
    )
    .unwrap();
    let store = TelemetryStore::from_connection(conn);
    let initial = serde_json::json!({"session_id":"qoder-1","event":"session_usage","usage":{"input_tokens":100,"output_tokens":20,"cached_tokens":80}});
    let corrected = serde_json::json!({"session_id":"qoder-1","event":"session_usage","usage":{"input_tokens":140,"output_tokens":30,"cached_tokens":110}});
    assert!(store.record_final_session_usage("qoder", &initial).unwrap());
    assert!(store
        .record_final_session_usage("qoder", &corrected)
        .unwrap());
    let usage: (i64, i64, i64) = store.conn.lock().unwrap().query_row(
            "SELECT input_tokens, output_tokens, cached_tokens FROM session_token_usage WHERE source='qoder' AND session_id='qoder-1'",
            [], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        ).unwrap();
    assert_eq!(usage, (140, 30, 110));
}

#[test]
fn keyword_recall_covers_cjk_and_latin_queries() {
    assert!(
        keyword_score(
            "[用户]\n记忆中心需要宽松检索",
            "记忆检索",
            &keyword_terms("记忆检索")
        ) > 0.0
    );
    assert!(
        keyword_score(
            "Use shared skill library",
            "skill library",
            &keyword_terms("skill library")
        ) > 0.0
    );
    assert_eq!(
        keyword_score("无关内容", "记忆检索", &keyword_terms("记忆检索")),
        0.0
    );
}

#[test]
fn local_search_aliases_cover_product_vocabulary() {
    assert!(
        keyword_score(
            "已向 Codex 配置共享记忆 MCP",
            "注入记忆",
            &keyword_terms("注入记忆")
        ) > 0.0
    );
    assert!(keyword_score("用户长期偏好：使用中文输出", "约束", &keyword_terms("约束")) > 0.0);
}

#[test]
fn pending_memory_session_list_orders_failures_first_and_hides_stored_rows() {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(
            "CREATE TABLE agent_events (
                id INTEGER PRIMARY KEY AUTOINCREMENT, event_key TEXT NOT NULL UNIQUE,
                source TEXT NOT NULL, session_id TEXT NOT NULL, project_path TEXT,
                event_type TEXT NOT NULL, occurred_at TEXT NOT NULL,
                token_source TEXT NOT NULL, input_tokens INTEGER NOT NULL, output_tokens INTEGER NOT NULL,
                cached_tokens INTEGER NOT NULL, payload_json TEXT NOT NULL,
                conversation_state TEXT NOT NULL DEFAULT 'unavailable',
                conversation_message_count INTEGER NOT NULL DEFAULT 0, conversation_text TEXT,
                l1_state TEXT NOT NULL DEFAULT 'unavailable', l1_error_detail TEXT
             );",
        )
        .unwrap();
    for (key, state, text) in [
        ("k-pending", "pending", "[用户]\n待整理会话"),
        ("k-failed", "failed", "[用户]\n失败会话"),
        ("k-stored", "stored", "[用户]\n已整理会话"),
        ("k-empty", "pending", ""),
    ] {
        conn.execute(
                "INSERT INTO agent_events (event_key, source, session_id, event_type, occurred_at, token_source, input_tokens, output_tokens, cached_tokens, payload_json, conversation_state, conversation_message_count, conversation_text, l1_state, l1_error_detail)
                 VALUES (?1, 'qoder', ?1, 'TranscriptSync', '2026-08-18T08:00:00Z', 'unavailable', 0, 0, 0, '{}', 'full', 3, ?2, ?3, CASE WHEN ?3 = 'failed' THEN '模型超时' ELSE NULL END)",
                rusqlite::params![key, text, state],
            )
            .unwrap();
    }
    let store = TelemetryStore::from_connection(conn);
    let rows = store.pending_memory_session_list(50).unwrap();
    let keys = rows
        .iter()
        .map(|row| row.event_key.as_str())
        .collect::<Vec<_>>();
    // stored 与空正文不进入面板；失败排在待整理之前。
    assert_eq!(keys, vec!["k-failed", "k-pending"]);
    assert_eq!(rows[0].error.as_deref(), Some("模型超时"));
    assert_eq!(rows[0].message_count, 3);
    assert!(rows[1].excerpt.contains("待整理会话"));
    let one = store
        .conversation_by_event_key("k-pending")
        .unwrap()
        .unwrap();
    assert!(one.conversation_text.contains("待整理会话"));
    // 已整理完成的会话不允许通过单会话整理重复提取。
    assert!(store
        .conversation_by_event_key("k-stored")
        .unwrap()
        .is_none());
    assert!(store
        .conversation_by_event_key("k-empty")
        .unwrap()
        .is_none());
}

#[test]
fn format_conversation_strips_injected_context_but_keeps_dialogue() {
    let messages = vec![
        (
            "user".to_string(),
            "<meta awareness=\"low\" timestamp=\"2026-08-18 19:13\" />\n这个项目的记忆中心有问题"
                .to_string(),
        ),
        (
            "user".to_string(),
            "<system-reminder>\n<tools_added>\nWidget\n</tools_added>\n</system-reminder>"
                .to_string(),
        ),
        (
            "assistant".to_string(),
            "好的，我来修复。<system-reminder>内部提示</system-reminder>马上开始。".to_string(),
        ),
        (
            "user".to_string(),
            "<system-reminder>未闭合的块，用户可能是在引用这个语法".to_string(),
        ),
    ];
    let text = format_conversation(&messages);
    assert!(text.contains("[用户]\n这个项目的记忆中心有问题"));
    assert!(text.contains("[助手]\n好的，我来修复。马上开始。"));
    // 纯注入消息整块消失；未闭合的标签按用户原文保留。
    assert!(!text.contains("tools_added"));
    assert!(!text.contains("内部提示"));
    assert!(!text.contains("meta awareness"));
    assert!(text.contains("未闭合的块"));
}

#[test]
fn dsh_plugin_usage_is_idempotent_and_matches_transcript_namespace() {
    let store = TelemetryStore::in_memory();
    let session = "session-019f8e90-36f5-71a2-bddf-f0fe9ee23b57";
    let payload = serde_json::json!({
        // 插件可能上报裸 uuid，入库前统一归一成目录名口径。
        "session_id": "019f8e90-36f5-71a2-bddf-f0fe9ee23b57",
        "model": "zhipu/glm-5.3",
        "input_tokens": 1200,
        "output_tokens": 80,
        "cached_tokens": 300,
        "occurred_at": "2026-10-28T10:00:00Z",
        "message_id": "42",
    });
    let first = store.record_dsh_plugin_usage(&payload).unwrap();
    let second = store.record_dsh_plugin_usage(&payload).unwrap();
    assert_eq!(first, second);
    // message_id 即事件 seq：与 read_dsh_usage_records 的 identity 完全一致，
    // 同一条 assistant/message 在两条路径下命中同一 record_id。
    assert_eq!(first, usage_record_id("dsh", session, "message", "42"));
    let conn = store.conn.lock().unwrap();
    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM usage_records WHERE source='dsh' AND session_id=?1",
            params![session],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(count, 1);
    let row: (String, i64, i64, i64, Option<String>) = conn
        .query_row(
            "SELECT origin, input_tokens, output_tokens, cached_tokens, model
               FROM usage_records WHERE source='dsh' AND session_id=?1",
            params![session],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            },
        )
        .unwrap();
    assert_eq!(row.0, "dsh_plugin");
    // input 列为输入合计（1200 未命中 + 300 缓存命中），与转录扫描口径一致。
    assert_eq!((row.1, row.2, row.3), (1500, 80, 300));
    assert_eq!(row.4.as_deref(), Some("zhipu/glm-5.3"));
}

#[test]
fn dsh_plugin_usage_replaces_transcript_record_with_same_identity() {
    let store = TelemetryStore::in_memory();
    let session = "session-019f8e90-36f5-71a2-bddf-f0fe9ee23b57";
    // 模拟转录扫描先写入同一条 assistant/message（identity=seq="42"）。
    let record_id = usage_record_id("dsh", session, "message", "42");
    let transcript_record = TranscriptUsageRecord {
        record_id: record_id.clone(),
        occurred_at: Some("2026-10-28T10:00:00.000Z".to_string()),
        model: Some("zhipu/glm-5.3".to_string()),
        input_tokens: 1000,
        output_tokens: 60,
        cached_tokens: 0,
    };
    {
        let mut conn = store.conn.lock().unwrap();
        let transaction = conn.transaction().unwrap();
        TelemetryStore::write_priced_usage_records(
            &transaction,
            &[],
            "2026-10-28T10:00:01.000Z",
            "dsh",
            session,
            "native_transcript",
            std::slice::from_ref(&transcript_record),
            &mut HashMap::new(),
        )
        .unwrap();
        transaction.commit().unwrap();
    }
    // 插件随后重报同一条消息：record_id 命中，INSERT OR REPLACE 原位覆盖
    // 而非叠加计费。
    let payload = serde_json::json!({
        "session_id": session,
        "model": "zhipu/glm-5.3",
        "input_tokens": 1200,
        "output_tokens": 80,
        "cached_tokens": 300,
        "message_id": "42",
    });
    assert_eq!(store.record_dsh_plugin_usage(&payload).unwrap(), record_id);
    let conn = store.conn.lock().unwrap();
    let row: (i64, i64, i64, String) = conn
        .query_row(
            "SELECT input_tokens, output_tokens, cached_tokens, origin
               FROM usage_records WHERE source='dsh' AND session_id=?1",
            params![session],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .unwrap();
    assert_eq!(row, (1500, 80, 300, "dsh_plugin".to_string()));
    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM usage_records WHERE source='dsh' AND session_id=?1",
            params![session],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(count, 1);
}
