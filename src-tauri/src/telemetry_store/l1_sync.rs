//! L1 synchronization primitives. Callers never hold a database transaction
//! across encryption or network I/O.
use super::{cloud_domains, CloudL1Memory, LocalMemorySnapshot, TelemetryStore};
use rusqlite::{params, Connection, TransactionBehavior};
use std::collections::HashSet;

pub(crate) struct ExtractedL1Snapshot {
    pub memories: Vec<CloudL1Memory>,
    pub deleted_ids: HashSet<String>,
}

pub(super) fn tombstoned_ids(conn: &Connection) -> Result<HashSet<String>, String> {
    let mut statement = conn.prepare("SELECT memory_id FROM l1_deletion_tombstones")
        .map_err(|error| error.to_string())?;
    let ids = statement.query_map([], |row| row.get(0)).map_err(|error| error.to_string())?;
    ids.collect::<Result<HashSet<_>, _>>().map_err(|error| error.to_string())
}

// This primitive must run inside its caller's write transaction. An existing
// marker still requires DELETE, so an old interrupted application is repairable.
pub(super) fn mark_deleted(conn: &Connection, id: &str, now: &str) -> Result<bool, String> {
    let inserted = conn.execute(
        "INSERT OR IGNORE INTO l1_deletion_tombstones(memory_id, deleted_at) VALUES (?1, ?2)",
        params![id, now],
    ).map_err(|error| error.to_string())?;
    let deleted = conn.execute("DELETE FROM local_memory_items WHERE id = ?1 AND user_defined = 0", [id])
        .map_err(|error| error.to_string())?;
    if deleted > 0 {
        // Domain/classification/provenance rows cascade. Legacy user-state
        // tables have no FK and must not keep publishing state for a dead ID.
        for table in ["memory_importance", "memory_user_state_peer", "cloud_domain_retags", "memory_refinement_reviews", "memory_refinement_edits"] {
            conn.execute(&format!("DELETE FROM {table} WHERE memory_id = ?1"), [id])
                .map_err(|error| error.to_string())?;
        }
    }
    Ok(inserted > 0 || deleted > 0)
}

pub(super) fn prune_deleted_rows(conn: &Connection) -> Result<usize, String> {
    let ids: Vec<String> = {
        let mut statement = conn.prepare(
            "SELECT m.id FROM local_memory_items m JOIN l1_deletion_tombstones t ON t.memory_id = m.id
             WHERE m.user_defined = 0",
        ).map_err(|error| error.to_string())?;
        let rows = statement.query_map([], |row| row.get(0)).map_err(|error| error.to_string())?;
        rows.collect::<Result<Vec<_>, _>>().map_err(|error| error.to_string())?
    };
    let now = chrono::Utc::now().to_rfc3339();
    for id in &ids { mark_deleted(conn, id, &now)?; }
    Ok(ids.len())
}

fn exportable_memories(conn: &Connection) -> Result<Vec<CloudL1Memory>, String> {
    let items = {
        let mut statement = conn.prepare(
            "SELECT id, source_event_key, ordinal, memory, memory_type, durability, source, session_id,
                    user_defined, event_time, created_at, updated_at
             FROM local_memory_items m
             WHERE user_defined = 0 AND origin_device IS NULL
               AND NOT EXISTS(SELECT 1 FROM l1_deletion_tombstones t WHERE t.memory_id = m.id)
             ORDER BY id",
        ).map_err(|error| error.to_string())?;
        let rows = statement.query_map([], |row| Ok(LocalMemorySnapshot {
            id: row.get(0)?, source_event_key: row.get(1)?, ordinal: row.get(2)?, memory: row.get(3)?,
            memory_type: row.get(4)?, durability: row.get(5)?, source: row.get(6)?, session_id: row.get(7)?,
            user_defined: row.get(8)?, event_time: row.get(9)?, created_at: row.get(10)?, updated_at: row.get(11)?,
        })).map_err(|error| error.to_string())?;
        rows.collect::<Result<Vec<_>, _>>().map_err(|error| error.to_string())?
    };
    cloud_domains::with_cloud_domains(conn, items)
}

fn capture_snapshot(conn: &Connection) -> Result<ExtractedL1Snapshot, String> {
    // Both reads, including tag/provenance projection, share a SQLite snapshot.
    // An apply/undo either precedes both of them or follows both of them.
    let deleted_ids = tombstoned_ids(conn)?;
    let memories = exportable_memories(conn)?;
    Ok(ExtractedL1Snapshot { memories, deleted_ids })
}

impl TelemetryStore {
    pub(crate) fn extracted_l1_sync_snapshot(&self) -> Result<ExtractedL1Snapshot, String> {
        let mut conn = self.conn.lock().map_err(|_| "telemetry store lock poisoned".to_string())?;
        let tx = conn.transaction().map_err(|error| error.to_string())?;
        let snapshot = capture_snapshot(&tx)?;
        tx.commit().map_err(|error| error.to_string())?;
        Ok(snapshot)
    }

    pub(crate) fn apply_l1_deletion_tombstones(&self, ids: &[String]) -> Result<usize, String> {
        let mut conn = self.conn.lock().map_err(|_| "telemetry store lock poisoned".to_string())?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate).map_err(|error| error.to_string())?;
        let now = chrono::Utc::now().to_rfc3339();
        let mut seen = HashSet::new();
        let mut applied = 0;
        for id in ids {
            if seen.insert(id.as_str()) && mark_deleted(&tx, id, &now)? { applied += 1; }
        }
        tx.commit().map_err(|error| error.to_string())?;
        Ok(applied)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn insert(conn: &Connection, id: &str, text: &str) {
        conn.execute("INSERT INTO local_memory_items
            (id, source_event_key, ordinal, memory, memory_type, durability, source, session_id, created_at, updated_at)
            VALUES (?1, ?1, 0, ?2, 'fact', 'short_term', 'test', 'session', '2026-11-02T00:00:00Z', '2026-11-02T00:00:00Z')",
            params![id, text]).unwrap();
    }

    #[test]
    fn deletion_batch_is_atomic_retryable_and_repairs_existing_markers() {
        let store = TelemetryStore::in_memory();
        {
            let conn = store.conn.lock().unwrap();
            insert(&conn, "a", "A"); insert(&conn, "b", "B");
            conn.execute_batch("CREATE TRIGGER fail_delete BEFORE DELETE ON local_memory_items
                WHEN OLD.id = 'b' BEGIN SELECT RAISE(ABORT, 'delete failed'); END;").unwrap();
        }
        let ids = vec!["a".into(), "b".into()];
        assert!(store.apply_l1_deletion_tombstones(&ids).is_err());
        assert!(store.tombstoned_l1_ids().unwrap().is_empty());
        assert_eq!(store.local_l1_memory_snapshot().unwrap().len(), 2);
        store.conn.lock().unwrap().execute_batch("DROP TRIGGER fail_delete;").unwrap();
        assert_eq!(store.apply_l1_deletion_tombstones(&ids).unwrap(), 2);
        assert_eq!(store.apply_l1_deletion_tombstones(&ids).unwrap(), 0);
        // Simulate an old client's interrupted DELETE after writing its marker.
        insert(&store.conn.lock().unwrap(), "a", "Residual old row");
        assert!(store.apply_l1_deletion_tombstone("a").unwrap());
        assert!(store.local_l1_memory_snapshot().unwrap().is_empty());
    }

    #[test]
    fn legacy_tombstoned_rows_cannot_be_exported_or_imported_again() {
        let store = TelemetryStore::in_memory();
        insert(&store.conn.lock().unwrap(), "old", "Old fact");
        let old = store.exportable_extracted_l1_memories().unwrap();
        store.apply_l1_deletion_tombstone("old").unwrap();
        insert(&store.conn.lock().unwrap(), "old", "Legacy residual");
        let snapshot = store.extracted_l1_sync_snapshot().unwrap();
        assert!(snapshot.memories.is_empty());
        assert!(snapshot.deleted_ids.contains("old"));
        assert_eq!(store.sync_peer_extracted_l1("peer", &old).unwrap(), (0, 1));
        assert!(store.local_l1_memory_snapshot().unwrap().is_empty());
    }

    std::thread_local! {
        static BLOCKED_IMPORT: std::cell::RefCell<Option<std::sync::mpsc::Sender<()>>> = std::cell::RefCell::new(None);
    }

    fn report_blocked_import(attempt: i32) -> bool {
        BLOCKED_IMPORT.with(|slot| {
            if let Some(sender) = slot.borrow_mut().take() { let _ = sender.send(()); }
        });
        if attempt >= 5000 { return false; }
        std::thread::sleep(std::time::Duration::from_millis(1));
        true
    }

    #[test]
    fn concurrent_import_reads_deletion_intent_after_acquiring_its_writer() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("l1-import-race.sqlite3");
        let mut writer = super::super::schema::initialize_database(Connection::open(&path).unwrap()).unwrap();
        insert(&writer, "old", "Do not resurrect this fact");
        let incoming = capture_snapshot(&writer).unwrap().memories;
        let importer = Connection::open(&path).unwrap();
        importer.busy_handler(Some(report_blocked_import)).unwrap();
        let write = writer.transaction_with_behavior(TransactionBehavior::Immediate).unwrap();
        mark_deleted(&write, "old", "2026-11-02T01:00:00Z").unwrap();
        let (signal, blocked) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            BLOCKED_IMPORT.with(|slot| *slot.borrow_mut() = Some(signal));
            let store = TelemetryStore::from_connection(importer);
            let result = store.sync_peer_extracted_l1("peer", &incoming);
            (result, store)
        });
        let waited = blocked.recv_timeout(std::time::Duration::from_secs(2));
        // Always release the writer and join, even when the diagnostic wait
        // failed, so the test cannot leave a background SQLite worker behind.
        write.commit().unwrap();
        let (result, store) = worker.join().unwrap();
        waited.expect("import must contend with the concurrent deletion");
        assert_eq!(result.unwrap(), (0, 0));
        assert!(store.local_l1_memory_snapshot().unwrap().is_empty());
        assert!(store.tombstoned_l1_ids().unwrap().contains("old"));
    }

    #[test]
    fn export_reads_deleted_ids_and_memories_from_one_wal_snapshot() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("l1-snapshot.sqlite3");
        let mut reader = super::super::schema::initialize_database(Connection::open(&path).unwrap()).unwrap();
        let mut writer = Connection::open(&path).unwrap();
        insert(&writer, "old", "Before refinement");
        let read = reader.transaction().unwrap();
        // Anchor the same transaction used by the production snapshot reader.
        assert!(tombstoned_ids(&read).unwrap().is_empty());
        let write = writer.transaction_with_behavior(TransactionBehavior::Immediate).unwrap();
        mark_deleted(&write, "old", "later").unwrap();
        insert(&write, "new", "After refinement");
        write.commit().unwrap();
        let before = capture_snapshot(&read).unwrap();
        assert!(before.deleted_ids.is_empty());
        assert_eq!(before.memories[0].id, "old");
        read.commit().unwrap();
        let read = reader.transaction().unwrap();
        let after = capture_snapshot(&read).unwrap();
        assert!(after.deleted_ids.contains("old"));
        assert_eq!(after.memories.len(), 1);
        assert_eq!(after.memories[0].id, "new");
    }
}
