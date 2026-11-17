use super::*;
use rusqlite::params;
static SERIAL: Mutex<()> = Mutex::new(());

fn budget() -> RecallBudget { RecallBudget::new(Duration::from_secs(5)) }
fn seed(store: &TelemetryStore) {
    let text = format!("{}ORBIT_KEY is the original source constraint.{}", "prefix filler ".repeat(1000), " suffix filler".repeat(1000));
    let conn = store.conn.lock().unwrap();
    conn.execute("INSERT INTO local_memory_items(id,source_event_key,ordinal,memory,memory_type,durability,source,session_id,user_defined,created_at,updated_at) VALUES('memory-one','event-one',0,'Read ORBIT_KEY source constraint','fact','short_term','claude','session-one',0,'2026-01-01','2026-01-01')", []).unwrap();
    conn.execute("INSERT INTO local_memory_domains VALUES('memory-one','work',1,'manual','2026-01-01','2026-01-01')", []).unwrap();
    conn.execute("INSERT INTO agent_events(event_key,source,session_id,event_type,occurred_at,payload_json,conversation_state,conversation_message_count,conversation_text) VALUES('event-one','claude','session-one','Stop','2026-01-01','{}','full',2,?1)", [&text]).unwrap();
}
fn fixture() -> TelemetryStore { let store = TelemetryStore::in_memory(); seed(&store); store }
fn issue(store: &TelemetryStore) -> EvidenceAccess {
    let resolved = store.with_read_connection(|conn| source::binding(conn, "memory-one", &budget())).unwrap().unwrap();
    let result = TraceResult { memory_id: "memory-one".into(), fingerprint: resolved.binding.memory_fingerprint,
        domains: vec!["work".into()], source_event_key: "event-one".into(), source_agent: "claude".into(),
        session_id: "session-one".into(), event_time: None, updated_at: "2026-01-01".into() };
    issue_for_results(store, "recall-one", &[result], Some(&["work".into()]), &budget())
}
fn request(access: &EvidenceAccess, max_chars: usize) -> EvidenceRequest {
    assert_eq!(access.status, "ready");
    EvidenceRequest { context_token: access.context_token.clone().unwrap(), memory_ids: vec!["memory-one".into()],
        query: Some("ORBIT_KEY".into()), max_chars: Some(max_chars), cursor: None }
}

#[test]
fn issued_tokens_are_hashed_and_mid_source_reads_are_bounded_and_paginated() {
    let _serial = SERIAL.lock().unwrap();
    let store = fixture();
    let access = issue(&store);
    let request = request(&access, 1000);
    let body = read_with_store(&store, &request, &budget());
    assert_eq!(body.status, "partial");
    assert!(body.partial && body.next_cursor.is_some());
    assert!(body.returned_chars <= 1000 && body.remaining_chars <= 7000);
    let excerpt = body.items[0].sources[0].excerpt.as_ref().unwrap();
    assert!(excerpt.char_start > 0);
    assert!(excerpt.text.contains("ORBIT_KEY"));
    assert_eq!(excerpt.char_end-excerpt.char_start, excerpt.text.chars().count());
    let conn = store.conn.lock().unwrap();
    let (token_hash, bindings, secret): (String,String,String) = conn.query_row(
        "SELECT token_hash,bindings_json,cursor_secret FROM memory_evidence_contexts",[],|row|Ok((row.get(0)?,row.get(1)?,row.get(2)?))).unwrap();
    assert_eq!(token_hash, hash(request.context_token.as_bytes()));
    assert_ne!(secret, request.context_token);
    assert!(!bindings.contains("original source constraint"));
    assert!(!bindings.contains(&request.context_token));
}

#[test]
fn wrong_ids_tampered_cursors_and_changed_queries_return_no_source_text() {
    let _serial = SERIAL.lock().unwrap();
    let store = fixture();
    let access = issue(&store);
    let mut input = request(&access, 2000);
    input.memory_ids = vec!["not-returned-by-recall".into()];
    let denied = read_with_store(&store, &input, &budget());
    assert_eq!(denied.status, "forbidden_scope");
    assert_eq!(denied.returned_chars, 0);
    input.memory_ids = vec!["memory-one".into()];
    let first = read_with_store(&store, &input, &budget());
    input.cursor = first.next_cursor;
    input.query = Some("different locator".into());
    let changed = read_with_store(&store, &input, &budget());
    assert_eq!(changed.status, "invalid_cursor");
    assert_eq!(changed.returned_chars, 0);
    input.query = Some("ORBIT_KEY".into());
    input.cursor.as_mut().unwrap().push('0');
    assert_eq!(read_with_store(&store, &input, &budget()).status, "invalid_cursor");
}

#[test]
fn expiry_memory_edits_domain_changes_and_source_revision_revoke_old_access() {
    let _serial = SERIAL.lock().unwrap();
    for mutation in [
        "UPDATE local_memory_items SET memory='edited body' WHERE id='memory-one'",
        "DELETE FROM local_memory_domains WHERE memory_id='memory-one'",
        "UPDATE agent_events SET conversation_text='changed source' WHERE event_key='event-one'",
        "DELETE FROM local_memory_items WHERE id='memory-one'",
    ] {
        let store = fixture();
        let access = issue(&store);
        store.conn.lock().unwrap().execute(mutation,[]).unwrap();
        let result = read_with_store(&store, &request(&access, 4000), &budget());
        assert!(matches!(result.status.as_str(), "stale_memory" | "scope_changed" | "source_changed"));
        assert_eq!(result.returned_chars, 0);
        assert!(result.items.iter().flat_map(|item| &item.sources).all(|source| source.excerpt.is_none()));
    }
    let store = fixture();
    let access = issue(&store);
    store.conn.lock().unwrap().execute("UPDATE memory_evidence_contexts SET expires_at='2000-01-01T00:00:00.000Z'",[]).unwrap();
    assert_eq!(read_with_store(&store,&request(&access,4000),&budget()).status,"context_expired");
}

#[test]
fn disabled_domain_is_rejected_even_if_membership_links_remain() {
    let _serial = SERIAL.lock().unwrap();
    let store = fixture();
    let access = issue(&store);
    store.conn.lock().unwrap().execute("UPDATE memory_domains SET enabled=0 WHERE id='work'",[]).unwrap();
    assert_eq!(read_with_store(&store,&request(&access,4000),&budget()).status,"scope_changed");
}

#[test]
fn separate_sqlite_connections_share_one_cumulative_character_quota() {
    let _serial = SERIAL.lock().unwrap();
    let temporary = tempfile::tempdir().unwrap();
    let path = temporary.path().join("evidence.sqlite3");
    let first = TelemetryStore::isolated_for_retrieval_evaluation(&path).unwrap();
    seed(&first);
    let access = issue(&first);
    let input = request(&access, TOTAL_CHARS);
    let second = TelemetryStore::from_connection(Connection::open(&path).unwrap());
    let barrier = Arc::new(std::sync::Barrier::new(2));
    let left_barrier = barrier.clone();
    let left_input = input.clone();
    let left = thread::spawn(move || { left_barrier.wait(); read_with_store(&first,&left_input,&budget()) });
    let right = thread::spawn(move || { barrier.wait(); read_with_store(&second,&input,&budget()) });
    let left = left.join().unwrap();
    let right = right.join().unwrap();
    assert!(left.returned_chars + right.returned_chars <= TOTAL_CHARS);
    assert!(left.returned_chars > 0 || right.returned_chars > 0);
}

#[test]
fn sql_deadline_interrupts_the_query_before_releasing_a_reader() {
    let _serial = SERIAL.lock().unwrap();
    let store = fixture();
    let deadline = RecallBudget::new(Duration::from_millis(20));
    let result = store.with_budgeted_read_connection(&deadline, |conn| {
        conn.query_row("WITH RECURSIVE n(x) AS (SELECT 1 UNION ALL SELECT x+1 FROM n WHERE x<1000000000) SELECT SUM(x) FROM n",[],|row|row.get::<_,i64>(0))
            .map_err(|_| "interrupted".into())
    });
    assert!(result.is_err());
    let next = store.with_read_connection(|conn|conn.query_row("SELECT 1",[],|row|row.get::<_,i64>(0)).map_err(|error|error.to_string())).unwrap();
    assert_eq!(next,1);
}

#[test]
fn evidence_audits_do_not_persist_bearers_locators_cursors_or_l0_text() {
    let token = "a".repeat(64);
    let arguments = json!({"context_token":token,"query":"private locator","cursor":"opaque cursor","memory_ids":["memory-one"]});
    let result = json!({"request_id":"read-one","status":"ok","returned_chars":20,"items":[{
        "memory_id":"memory-one","status":"ok","sources":[{"source_event_key":"event-one","status":"available",
        "excerpt":{"text":"private L0 source body","char_start":0,"char_end":20}}]}]});
    let detail = audit_detail("read_memory_evidence",&arguments,Some(&result),None);
    assert!(!detail.contains(&token));
    assert!(!detail.contains("private locator"));
    assert!(!detail.contains("opaque cursor"));
    assert!(!detail.contains("private L0 source body"));
    let recalled = json!({"evidence_access":{"context_token":token,"status":"ready"},"memories":[]});
    assert!(!audit_detail("recall_memory",&json!({"query":"normal"}),Some(&recalled),None).contains(&token));
}

#[test]
fn source_delete_and_reinsert_cannot_reuse_the_old_revision_fingerprint() {
    let _serial = SERIAL.lock().unwrap();
    let store = fixture();
    let access = issue(&store);
    let conn = store.conn.lock().unwrap();
    let original: String = conn.query_row("SELECT conversation_text FROM agent_events WHERE event_key='event-one'",[],|row|row.get(0)).unwrap();
    conn.execute("DELETE FROM agent_events WHERE event_key='event-one'",[]).unwrap();
    conn.execute("INSERT INTO agent_events(event_key,source,session_id,event_type,occurred_at,payload_json,conversation_state,conversation_message_count,conversation_text) VALUES('event-one','claude','session-one','Stop','2026-01-01','{}','full',2,?1)",params![original]).unwrap();
    drop(conn);
    let result = read_with_store(&store,&request(&access,1000),&budget());
    assert_eq!(result.status,"source_changed");
    assert_eq!(result.returned_chars,0);
}
