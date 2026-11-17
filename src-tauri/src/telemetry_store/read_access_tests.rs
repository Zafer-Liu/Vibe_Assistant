use super::*;

fn disk_store() -> (tempfile::TempDir, TelemetryStore) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("read-isolation.sqlite3");
    let conn = schema::initialize_database(Connection::open(&path).unwrap()).unwrap();
    let store = TelemetryStore::from_connection(conn);
    store.conn.lock().unwrap().execute_batch(
        "INSERT INTO usage_records (record_id, source, session_id, occurred_at,
          record_kind, input_tokens, output_tokens, cached_tokens, origin)
         VALUES ('response', 'claude', 'session', '2026-11-07T01:00:00Z',
                 'response', 100, 20, 80, 'native_transcript');",
    ).unwrap();
    (directory, store)
}

#[test]
fn ordinary_reads_do_not_need_the_main_writer_mutex() {
    let (_directory, store) = disk_store();
    let writer = store.conn.lock().unwrap();
    writer.execute_batch("BEGIN IMMEDIATE;
        INSERT INTO app_settings (setting_key, value_json, updated_at)
        VALUES ('pending-writer', 'true', '2026-11-07T01:00:00Z');").unwrap();
    let reader = store.clone();
    let (sender, receiver) = std::sync::mpsc::channel();
    let task = std::thread::spawn(move || {
        let result = reader.with_read_connection(|conn| {
            conn.query_row("SELECT COUNT(*) FROM usage_records", [], |row| row.get::<_, i64>(0))
                .map_err(|error| error.to_string())
        });
        sender.send(result).unwrap();
    });
    let result = receiver.recv_timeout(std::time::Duration::from_secs(3));
    writer.execute_batch("ROLLBACK").unwrap();
    drop(writer);
    task.join().unwrap();
    assert_eq!(result.unwrap().unwrap(), 1);
}

#[test]
fn recall_settings_and_keywords_do_not_wait_for_the_writer() {
    let (_directory, store) = disk_store();
    store.app_setting_set("recall-read-setting", &true).unwrap();
    store.conn.lock().unwrap().execute_batch(
        "INSERT INTO local_memory_items
         (id, source_event_key, ordinal, memory, memory_type, durability, source, session_id, user_defined, created_at, updated_at)
         VALUES ('recall', 'recall-event', 0, 'flintkeyword committed memory', 'fact', 'short_term', 'test', 'test', 0, 'now', 'now');",
    ).unwrap();
    let writer = store.conn.lock().unwrap();
    writer.execute_batch("BEGIN IMMEDIATE;
        UPDATE local_memory_items SET memory='uncommitted edit' WHERE id='recall';
        UPDATE app_settings SET value_json='false' WHERE setting_key='recall-read-setting';").unwrap();
    let reader = store.clone();
    let (sender, receiver) = std::sync::mpsc::channel();
    let task = std::thread::spawn(move || {
        let setting = reader.app_setting_get_readonly::<bool>("recall-read-setting");
        let memories = reader.search_local_l1_memories_scoped("flintkeyword", 4, None);
        sender.send((setting, memories)).unwrap();
    });
    let result = receiver.recv_timeout(std::time::Duration::from_secs(2));
    writer.execute_batch("ROLLBACK").unwrap();
    drop(writer);
    task.join().unwrap();
    let (setting, memories) = result.unwrap();
    assert_eq!(setting, Some(true));
    let memories = memories.unwrap();
    assert_eq!(memories.len(), 1);
    assert_eq!(memories[0].memory, "flintkeyword committed memory");
    store.app_setting_set("recall-read-setting", &false).unwrap();
    assert_eq!(store.app_setting_get_readonly::<bool>("recall-read-setting"), Some(false));
    assert!(store.read_database_path().is_some());
}

#[test]
fn readonly_settings_keep_in_memory_and_decode_fallback_behavior() {
    let store = TelemetryStore::in_memory();
    assert!(store.read_database_path().is_none());
    assert_eq!(store.app_setting_get_readonly::<bool>("missing"), None);
    store.app_setting_set("value", &true).unwrap();
    assert_eq!(store.app_setting_get_readonly::<bool>("value"), Some(true));
    store.app_setting_set("value", &"not a boolean").unwrap();
    assert_eq!(store.app_setting_get_readonly::<bool>("value"), None);
}

#[test]
fn analytics_temp_snapshot_works_without_the_writer_mutex() {
    let (_directory, store) = disk_store();
    let writer = store.conn.lock().unwrap();
    let reader = store.clone();
    let (sender, receiver) = std::sync::mpsc::channel();
    let task = std::thread::spawn(move || {
        sender.send(reader.usage_analytics(None, None, None, "day", 500)).unwrap();
    });
    let result = receiver.recv_timeout(std::time::Duration::from_secs(3));
    drop(writer);
    task.join().unwrap();
    assert_eq!(result.unwrap().unwrap().input_tokens, 100);
}

#[test]
fn pooled_readers_cannot_write_the_durable_database() {
    let (_directory, store) = disk_store();
    let result = store.with_read_connection(|conn| {
        conn.execute("DELETE FROM usage_records", []).map_err(|error| error.to_string())
    });
    assert!(result.is_err());
    assert_eq!(store.usage_analytics(None, None, None, "day", 500).unwrap().record_count, 1);
}

#[test]
fn unrelated_writes_and_price_changes_do_not_rebuild_usage_snapshot() {
    let store = TelemetryStore::in_memory();
    store.conn.lock().unwrap().execute_batch(
        "INSERT INTO usage_records (record_id, source, session_id, occurred_at,
          record_kind, input_tokens, output_tokens, cached_tokens, origin)
         VALUES ('response', 'claude', 'session', '2026-11-07T01:00:00Z',
                 'response', 100, 20, 80, 'native_transcript');",
    ).unwrap();
    store.usage_analytics(None, None, None, "day", 500).unwrap();
    store.app_setting_set("unrelated-setting", &true).unwrap();
    store.conn.lock().unwrap().execute("UPDATE usage_records SET cost_usd = 0.5, pricing_source = 'catalog'", []).unwrap();
    let changes = || -> i64 {
        store.conn.lock().unwrap().query_row("SELECT total_changes()", [], |row| row.get(0)).unwrap()
    };
    let after_writes = changes();
    assert_eq!(store.summary().unwrap().total_tokens, 120);
    store.usage_analytics(None, None, None, "day", 500).unwrap();
    assert_eq!(changes(), after_writes, "无关设置和定价不能重建 TEMP 用量快照");
}

#[test]
fn explicit_reader_invalidation_rebuilds_after_restore() {
    let (_directory, store) = disk_store();
    store.usage_analytics(None, None, None, "day", 500).unwrap();
    assert!(store.usage_query_cache.lock().unwrap().is_some());
    store.invalidate_read_connections().unwrap();
    assert!(store.usage_query_cache.lock().unwrap().is_none());
    assert_eq!(store.usage_analytics(None, None, None, "day", 500).unwrap().record_count, 1);
}
