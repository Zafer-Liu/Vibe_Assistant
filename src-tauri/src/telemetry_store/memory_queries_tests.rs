use super::*;

fn test_connection() -> Connection {
    super::super::schema::initialize_database(Connection::open_in_memory().unwrap()).unwrap()
}

fn inventory_revision(conn: &Connection) -> i64 {
    conn.query_row(
        "SELECT revision FROM app_data_revisions WHERE domain = 'memory_inventory'",
        [], |row| row.get(0),
    ).unwrap()
}

fn insert_memory(conn: &Connection, id: &str, user_defined: bool, updated_at: &str) {
    conn.execute(
        "INSERT INTO local_memory_items
         (id, source_event_key, ordinal, memory, memory_type, durability, source, session_id,
          user_defined, event_time, created_at, updated_at)
         VALUES (?1, ?1, 0, '测试 memory', 'fact', 'long_term', 'test', 'session', ?2, ?3, ?3, ?3)",
        params![id, i64::from(user_defined), updated_at],
    ).unwrap();
}

fn insert_importance(
    conn: &Connection,
    id: &str,
    pinned: bool,
    pinned_at: Option<&str>,
    recall_count: i64,
) {
    conn.execute(
        "INSERT INTO memory_importance
         (memory_id, score, supporting_sessions, supporting_agents, duplicate_count,
          recall_count, pinned, updated_at, pinned_updated_at)
         VALUES (?1, 42, 3, 2, 1, ?2, ?3, '2026-01-01', ?4)",
        params![id, recall_count, i64::from(pinned), pinned_at],
    ).unwrap();
}

fn insert_peer(
    conn: &Connection,
    id: &str,
    device: &str,
    pinned: bool,
    pinned_at: Option<&str>,
    recall_count: i64,
) {
    conn.execute(
        "INSERT INTO memory_user_state_peer
         (device, memory_id, pinned, pinned_updated_at, recall_count, recall_updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, '2026-01-04')",
        params![device, id, i64::from(pinned), pinned_at, recall_count],
    ).unwrap();
}

#[test]
fn pages_follow_user_timestamp_and_id_order_across_ties() {
    let conn = test_connection();
    for (id, user_defined, updated_at) in [
        ("auto-a", false, "2026-01-02"),
        ("user-a", true, "2026-01-02"),
        ("auto-oldest", false, "2025-01-01"),
        ("auto-b", false, "2026-01-02"),
        ("user-old", true, "2026-01-01"),
        ("auto-old", false, "2026-01-01"),
        ("user-b", true, "2026-01-02"),
        ("auto-new", false, "2026-02-01"),
    ] {
        insert_memory(&conn, id, user_defined, updated_at);
    }
    let mut cursor = None;
    let mut ids = Vec::new();
    let mut page_count = 0;
    loop {
        let page = local_memory_page_on(&conn, cursor.as_ref(), Some(2)).unwrap();
        assert_eq!(page.items.len(), 2);
        for item in &page.items {
            assert_eq!(item.memory, "测试 memory");
            assert_eq!(item.memory_type, "fact");
            assert_eq!(item.durability, "long_term");
            assert_eq!(item.event_time.as_deref(), Some(item.last_update_at.as_str()));
            assert_eq!(item.score, None);
            ids.push(item.id.clone());
        }
        page_count += 1;
        assert!(page_count <= 4, "cursor must advance");
        let Some(next) = page.next_cursor else { break };
        let encoded = serde_json::to_value(&next).unwrap();
        assert_eq!(encoded["user_defined"], next.user_defined);
        assert_eq!(encoded["revision"].as_str(), Some(next.revision.as_str()));
        assert_eq!(next.revision, inventory_revision(&conn).to_string());
        cursor = Some(serde_json::from_value::<MemoryPageCursor>(encoded).unwrap());
    }
    assert_eq!(page_count, 4);
    assert_eq!(ids, [
        "user-b", "user-a", "user-old", "auto-new", "auto-b", "auto-a", "auto-old", "auto-oldest",
    ]);
}

#[test]
fn pages_are_bounded_and_validate_limits_and_cursor() {
    let conn = test_connection();
    for index in 0..1001 {
        insert_memory(&conn, &format!("memory-{index:04}"), false, "2026-01-01");
    }
    let first = local_memory_page_on(&conn, None, None).unwrap();
    assert_eq!(first.items.len(), 500);
    assert_eq!(first.items.first().unwrap().id, "memory-1000");
    let second = local_memory_page_on(&conn, first.next_cursor.as_ref(), None).unwrap();
    assert_eq!(second.items.len(), 500);
    assert_eq!(second.items.first().unwrap().id, "memory-0500");
    let third = local_memory_page_on(&conn, second.next_cursor.as_ref(), None).unwrap();
    assert_eq!(third.items.len(), 1);
    assert_eq!(third.items[0].id, "memory-0000");
    assert!(third.next_cursor.is_none());
    assert!(local_memory_page_on(&conn, None, Some(0)).is_err());
    assert!(local_memory_page_on(&conn, None, Some(501)).is_err());
    assert!(local_memory_page_on(&conn, None, Some(u32::MAX)).is_err());
    for (id, updated_at) in [("", "2026-01-01"), ("memory", "")] {
        let cursor = MemoryPageCursor {
            user_defined: false, updated_at: updated_at.into(), id: id.into(),
            revision: inventory_revision(&conn).to_string(),
        };
        assert!(local_memory_page_on(&conn, Some(&cursor), Some(10)).is_err());
    }
    for revision in ["", "-1", "not-an-integer", "9223372036854775808"] {
        let cursor = MemoryPageCursor {
            user_defined: false, updated_at: "2026-01-01".into(), id: "memory".into(), revision: revision.into(),
        };
        assert!(local_memory_page_on(&conn, Some(&cursor), Some(10)).is_err());
    }
    assert!(serde_json::from_value::<MemoryPageCursor>(serde_json::json!({
        "user_defined": false, "updated_at": "2026-01-01", "id": "memory", "revision": "1", "offset": 4,
    })).is_err());
    assert!(serde_json::from_value::<MemoryPageCursor>(serde_json::json!({
        "user_defined": false, "updated_at": "2026-01-01", "id": "memory",
    })).is_err());
    conn.execute("DELETE FROM local_memory_items", []).unwrap();
    let empty = local_memory_page_on(&conn, None, None).unwrap();
    assert!(empty.items.is_empty());
    assert!(empty.next_cursor.is_none());
}

#[test]
fn deleting_cursor_and_unvisited_rows_requires_restart_without_skipping_rows() {
    let conn = test_connection();
    for id in ["a", "b", "c", "d", "e", "f"] {
        insert_memory(&conn, id, false, "2026-01-01");
    }
    let first = local_memory_page_on(&conn, None, Some(2)).unwrap();
    assert_eq!(first.items.iter().map(|item| item.id.as_str()).collect::<Vec<_>>(), ["f", "e"]);
    conn.execute("DELETE FROM local_memory_items WHERE id IN ('e', 'd')", []).unwrap();
    let error = local_memory_page_on(&conn, first.next_cursor.as_ref(), Some(2)).err().unwrap();
    assert!(error.starts_with(MEMORY_INVENTORY_CHANGED_PREFIX));
    assert!(conn.is_autocommit(), "stale read must release its transaction");
    let restarted = local_memory_page_on(&conn, None, Some(2)).unwrap();
    assert_eq!(restarted.items.iter().map(|item| item.id.as_str()).collect::<Vec<_>>(), ["f", "c"]);
    let last = local_memory_page_on(&conn, restarted.next_cursor.as_ref(), Some(2)).unwrap();
    assert_eq!(last.items.iter().map(|item| item.id.as_str()).collect::<Vec<_>>(), ["b", "a"]);
    assert!(last.next_cursor.is_none());
    assert!(conn.is_autocommit());
}

#[test]
fn ordering_key_changes_and_inserts_reject_stale_cursors() {
    for mutation in [
        "UPDATE local_memory_items SET updated_at = '2026-02-01' WHERE id = 'a'",
        "UPDATE local_memory_items SET user_defined = 1 WHERE id = 'a'",
        "UPDATE local_memory_items SET id = 'z' WHERE id = 'a'",
        "INSERT INTO local_memory_items
         (id, source_event_key, ordinal, memory, memory_type, durability, source, session_id,
          user_defined, created_at, updated_at)
         VALUES ('z', 'z', 0, 'inserted', 'fact', 'long_term', 'test', 'session', 0, '2026-01-01', '2026-01-01')",
    ] {
        let conn = test_connection();
        for id in ["a", "b", "c", "d"] {
            insert_memory(&conn, id, false, "2026-01-01");
        }
        let first = local_memory_page_on(&conn, None, Some(2)).unwrap();
        assert_eq!(first.items.iter().map(|item| item.id.as_str()).collect::<Vec<_>>(), ["d", "c"]);
        conn.execute(mutation, []).unwrap();
        let error = local_memory_page_on(&conn, first.next_cursor.as_ref(), Some(2)).err().unwrap();
        assert!(error.starts_with(MEMORY_INVENTORY_CHANGED_PREFIX), "{mutation}");
        assert!(conn.is_autocommit());
        let mut cursor = None;
        let mut ids = Vec::new();
        loop {
            let page = local_memory_page_on(&conn, cursor.as_ref(), Some(2)).unwrap();
            ids.extend(page.items.into_iter().map(|item| item.id));
            if page.next_cursor.is_none() { break; }
            cursor = page.next_cursor;
        }
        assert_eq!(ids[0], if mutation.contains("'z'") { "z" } else { "a" });
        let expected_count: i64 = conn.query_row("SELECT COUNT(*) FROM local_memory_items", [], |row| row.get(0)).unwrap();
        assert_eq!(ids.len() as i64, expected_count);
        assert_eq!(ids.iter().collect::<HashSet<_>>().len(), ids.len());
    }
}

#[test]
fn inventory_revision_triggers_track_visible_changes_and_initialize_is_idempotent() {
    let conn = test_connection();
    let initial = inventory_revision(&conn);
    insert_memory(&conn, "memory", false, "2026-01-01");
    assert_eq!(inventory_revision(&conn), initial + 1);
    initialize(&conn).unwrap();
    assert_eq!(inventory_revision(&conn), initial + 1);
    conn.execute("UPDATE local_memory_items SET memory = memory WHERE id = 'memory'", []).unwrap();
    assert_eq!(inventory_revision(&conn), initial + 1);
    for (index, mutation) in [
        "UPDATE local_memory_items SET memory = 'new content' WHERE id = 'memory'",
        "UPDATE local_memory_items SET memory_type = 'preference' WHERE id = 'memory'",
        "UPDATE local_memory_items SET durability = 'short_term' WHERE id = 'memory'",
        "UPDATE local_memory_items SET event_time = '2026-01-02' WHERE id = 'memory'",
    ].iter().enumerate() {
        conn.execute(mutation, []).unwrap();
        assert_eq!(inventory_revision(&conn), initial + 2 + index as i64);
    }
    let before_delete = inventory_revision(&conn);
    insert_importance(&conn, "memory", true, Some("2026-01-03"), 1);
    insert_peer(&conn, "memory", "peer", false, Some("2026-01-04"), 2);
    assert_eq!(inventory_revision(&conn), before_delete);
    conn.execute("DELETE FROM local_memory_items WHERE id = 'memory'", []).unwrap();
    assert_eq!(inventory_revision(&conn), before_delete + 1);
}

#[test]
fn importance_batches_preserve_input_order_duplicates_pins_and_recall_totals() {
    let conn = test_connection();
    let count = IMPORTANCE_BATCH_LIMIT * 2 + 7;
    for index in 0..count {
        let id = format!("memory-{index:04}");
        insert_importance(&conn, &id, false, Some("2026-01-02"), 2);
        insert_peer(&conn, &id, "old", true, Some("2026-01-01"), 3);
        insert_peer(&conn, &id, "new", true, Some("2026-01-03"), 5);
        insert_peer(&conn, &id, "without-pin-decision", false, None, 7);
    }
    insert_peer(&conn, "missing-local", "peer-only", true, Some("2026-01-04"), 99);
    let mut ids = vec![format!("memory-{:04}", count - 1), "memory-0000".into(), "missing-local".into()];
    ids.extend((0..count).rev().map(|index| format!("memory-{index:04}")));
    ids.push("memory-0000".into());
    let records = importance_records_on(&conn, &ids).unwrap();
    let expected_ids: Vec<_> = ids.iter().filter(|id| id.as_str() != "missing-local").map(String::as_str).collect();
    assert_eq!(records.iter().map(|record| record.memory_id.as_str()).collect::<Vec<_>>(), expected_ids);
    assert_eq!(records.len(), count + 3);
    for record in records {
        assert_eq!(record.score, 42);
        assert_eq!(record.supporting_sessions, 3);
        assert_eq!(record.supporting_agents, 2);
        assert_eq!(record.duplicate_count, 1);
        assert_eq!(record.recall_count, 17);
        assert!(record.pinned);
        assert_eq!(record.pinned_updated_at.as_deref(), Some("2026-01-03"));
        assert_eq!(record.updated_at, "2026-01-01");
    }
    assert!(importance_records_on(&conn, &[]).unwrap().is_empty());
    assert!(importance_records_on(&conn, &["missing-local".into()]).unwrap().is_empty());
}

#[test]
fn importance_pin_precedence_keeps_local_ties_and_ignores_missing_timestamps() {
    let conn = test_connection();
    insert_importance(&conn, "local-newer", true, Some("2026-01-03"), 1);
    insert_peer(&conn, "local-newer", "peer", false, Some("2026-01-02"), 2);
    insert_importance(&conn, "local-tie", false, Some("2026-01-03"), 1);
    insert_peer(&conn, "local-tie", "peer", true, Some("2026-01-03"), 2);
    insert_importance(&conn, "no-decision", true, None, 1);
    insert_peer(&conn, "no-decision", "peer", false, None, 2);
    insert_importance(&conn, "peer-tie", false, None, 1);
    insert_peer(&conn, "peer-tie", "first", true, Some("2026-01-03"), 2);
    insert_peer(&conn, "peer-tie", "second", false, Some("2026-01-03"), 3);
    insert_importance(&conn, "peer-newer", false, Some("2026-01-01"), 1);
    insert_peer(&conn, "peer-newer", "first", false, Some("2026-01-02"), 2);
    insert_peer(&conn, "peer-newer", "second", true, Some("2026-01-03"), 3);
    let ids = ["local-newer", "local-tie", "no-decision", "peer-tie", "peer-newer"].map(String::from);
    let records = importance_records_on(&conn, &ids).unwrap();
    assert_eq!(records.iter().map(|record| record.pinned).collect::<Vec<_>>(), [true, false, true, true, true]);
    assert_eq!(records.iter().map(|record| record.recall_count).collect::<Vec<_>>(), [3, 3, 3, 6, 6]);
    assert_eq!(records[2].pinned_updated_at, None);
    assert_eq!(records[3].pinned_updated_at.as_deref(), Some("2026-01-03"));
    assert_eq!(records[4].pinned_updated_at.as_deref(), Some("2026-01-03"));
}
