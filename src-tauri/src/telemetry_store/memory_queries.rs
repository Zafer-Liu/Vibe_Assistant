//! Bounded reads for the authoritative local memory inventory and its user state.

use super::{LocalMemory, MemoryImportance, TelemetryStore};
use rusqlite::{params, params_from_iter, Connection};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

const MEMORY_PAGE_LIMIT: u32 = 500;
const IMPORTANCE_BATCH_LIMIT: usize = 400;
const MEMORY_INVENTORY_CHANGED_PREFIX: &str = "MEMORY_INVENTORY_CHANGED:";

/// Called in the v2 migration transaction after projections initialize revisions.
pub(super) fn initialize(conn: &Connection) -> Result<(), String> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS app_data_revisions (
             domain TEXT PRIMARY KEY, revision INTEGER NOT NULL
         );
         INSERT OR IGNORE INTO app_data_revisions(domain, revision) VALUES ('memory_inventory', 0);
         CREATE TRIGGER IF NOT EXISTS revision_memory_inventory_insert AFTER INSERT ON local_memory_items BEGIN
             UPDATE app_data_revisions SET revision = revision + 1 WHERE domain = 'memory_inventory';
         END;
         CREATE TRIGGER IF NOT EXISTS revision_memory_inventory_update AFTER UPDATE ON local_memory_items
         WHEN OLD.id IS NOT NEW.id OR OLD.memory IS NOT NEW.memory
           OR OLD.memory_type IS NOT NEW.memory_type OR OLD.durability IS NOT NEW.durability
           OR OLD.user_defined IS NOT NEW.user_defined OR OLD.updated_at IS NOT NEW.updated_at
           OR OLD.event_time IS NOT NEW.event_time BEGIN
             UPDATE app_data_revisions SET revision = revision + 1 WHERE domain = 'memory_inventory';
         END;
         CREATE TRIGGER IF NOT EXISTS revision_memory_inventory_delete AFTER DELETE ON local_memory_items BEGIN
             UPDATE app_data_revisions SET revision = revision + 1 WHERE domain = 'memory_inventory';
         END;",
    ).map_err(|error| error.to_string())
}

/// The last emitted ordering key and the inventory revision of the traversal.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct MemoryPageCursor {
    pub user_defined: bool,
    pub updated_at: String,
    pub id: String,
    /// Decimal i64 encoded as a string so JavaScript preserves it exactly.
    pub revision: String,
}

#[derive(Serialize)]
pub struct MemoryPage {
    pub items: Vec<LocalMemory>,
    pub next_cursor: Option<MemoryPageCursor>,
}

/// Revision and rows share one short read transaction. An inventory mutation
/// between pages invalidates the cursor instead of silently moving rows past it.
#[tauri::command]
pub async fn local_memory_page(
    state: tauri::State<'_, TelemetryStore>,
    cursor: Option<MemoryPageCursor>,
    limit: Option<u32>,
) -> Result<MemoryPage, String> {
    let store = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        store.with_read_connection(|conn| local_memory_page_on(conn, cursor.as_ref(), limit))
    })
    .await
    .map_err(|error| error.to_string())?
}

fn local_memory_page_on(
    conn: &Connection,
    cursor: Option<&MemoryPageCursor>,
    limit: Option<u32>,
) -> Result<MemoryPage, String> {
    let limit = limit.unwrap_or(MEMORY_PAGE_LIMIT);
    if !(1..=MEMORY_PAGE_LIMIT).contains(&limit) {
        return Err(format!("memory page limit must be between 1 and {MEMORY_PAGE_LIMIT}"));
    }
    if cursor.is_some_and(|cursor| cursor.updated_at.is_empty() || cursor.id.is_empty()) {
        return Err("memory page cursor must contain a timestamp and id".into());
    }
    let cursor_revision = cursor.map(|cursor| cursor.revision.parse::<i64>()
        .map_err(|_| "memory page cursor revision must be a nonnegative integer".to_string()))
        .transpose()?;
    if cursor_revision.is_some_and(|revision| revision < 0) {
        return Err("memory page cursor revision must be a nonnegative integer".into());
    }
    // This helper receives an exclusively borrowed pooled connection (or the
    // locked in-memory connection), with no transaction active on entry.
    let transaction = conn.unchecked_transaction().map_err(|error| error.to_string())?;
    let revision: i64 = transaction.query_row(
        "SELECT revision FROM app_data_revisions WHERE domain = 'memory_inventory'",
        [], |row| row.get(0),
    ).map_err(|error| error.to_string())?;
    if cursor_revision.is_some_and(|expected| expected != revision) {
        return Err(format!("{MEMORY_INVENTORY_CHANGED_PREFIX} inventory changed; restart from the first page"));
    }
    let mut items = {
        let columns = "SELECT id, memory, memory_type, durability, user_defined, updated_at, event_time
                         FROM local_memory_items";
        let sql = if cursor.is_some() {
            format!("{columns}
                      WHERE (user_defined, updated_at, id) < (?1, ?2, ?3)
                      ORDER BY user_defined DESC, updated_at DESC, id DESC LIMIT ?4")
        } else {
            format!("{columns} ORDER BY user_defined DESC, updated_at DESC, id DESC LIMIT ?1")
        };
        let mut statement = transaction.prepare(&sql).map_err(|error| error.to_string())?;
        // One lookahead row distinguishes a final full page from a continuing page.
        let read_limit = i64::from(limit) + 1;
        let mut rows = match cursor {
            Some(cursor) => statement.query(params![
                i64::from(cursor.user_defined), cursor.updated_at, cursor.id, read_limit,
            ]),
            None => statement.query([read_limit]),
        }.map_err(|error| error.to_string())?;
        let mut items = Vec::with_capacity(limit as usize + 1);
        while let Some(row) = rows.next().map_err(|error| error.to_string())? {
            items.push(LocalMemory {
                id: row.get(0).map_err(|error| error.to_string())?,
                memory: row.get(1).map_err(|error| error.to_string())?,
                memory_type: row.get(2).map_err(|error| error.to_string())?,
                durability: row.get(3).map_err(|error| error.to_string())?,
                user_defined: row.get::<_, i64>(4).map_err(|error| error.to_string())? != 0,
                last_update_at: row.get(5).map_err(|error| error.to_string())?,
                event_time: row.get(6).map_err(|error| error.to_string())?,
                score: None,
            });
        }
        items
    };
    let next_cursor = if items.len() > limit as usize {
        items.pop();
        items.last().map(|item| MemoryPageCursor {
            user_defined: item.user_defined,
            updated_at: item.last_update_at.clone(),
            id: item.id.clone(),
            revision: revision.to_string(),
        })
    } else {
        None
    };
    transaction.commit().map_err(|error| error.to_string())?;
    Ok(MemoryPage { items, next_cursor })
}

/// Merge local importance and peer user state using bounded IN queries. Missing
/// local records remain absent; replay the input to preserve order and duplicates.
pub(super) fn importance_records_on(
    conn: &Connection,
    ids: &[String],
) -> Result<Vec<MemoryImportance>, String> {
    let mut seen = HashSet::new();
    let unique_ids: Vec<&String> = ids.iter().filter(|id| seen.insert(id.as_str())).collect();
    let mut records = HashMap::new();
    for batch in unique_ids.chunks(IMPORTANCE_BATCH_LIMIT) {
        let placeholders = vec!["?"; batch.len()].join(",");
        let mut statement = conn.prepare(&format!(
            "SELECT memory_id, score, supporting_sessions, supporting_agents, duplicate_count,
                    recall_count, pinned, updated_at, pinned_updated_at
               FROM memory_importance WHERE memory_id IN ({placeholders})",
        )).map_err(|error| error.to_string())?;
        let rows = statement.query_map(params_from_iter(batch.iter().map(|id| id.as_str())), |row| {
            Ok(MemoryImportance {
                memory_id: row.get(0)?,
                score: row.get(1)?,
                supporting_sessions: row.get(2)?,
                supporting_agents: row.get(3)?,
                duplicate_count: row.get(4)?,
                recall_count: row.get(5)?,
                pinned: row.get::<_, i64>(6)? != 0,
                updated_at: row.get(7)?,
                pinned_updated_at: row.get(8)?,
            })
        }).map_err(|error| error.to_string())?;
        // The previous per-id query omitted local rows it could not decode.
        for record in rows.filter_map(Result::ok) {
            records.insert(record.memory_id.clone(), record);
        }
        let peer_ids: Vec<&String> = batch.iter().copied().filter(|id| records.contains_key(*id)).collect();
        if peer_ids.is_empty() {
            continue;
        }
        let placeholders = vec!["?"; peer_ids.len()].join(",");
        let mut statement = conn.prepare(&format!(
            "SELECT memory_id, pinned, pinned_updated_at, recall_count, recall_updated_at
               FROM memory_user_state_peer WHERE memory_id IN ({placeholders})
              ORDER BY memory_id, rowid",
        )).map_err(|error| error.to_string())?;
        let rows = statement.query_map(params_from_iter(peer_ids.iter().map(|id| id.as_str())), |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)? != 0,
                row.get::<_, Option<String>>(2)?,
                row.get::<_, i64>(3)?,
                row.get::<_, Option<String>>(4)?,
            ))
        }).map_err(|error| error.to_string())?;
        for row in rows {
            let (memory_id, peer_pinned, peer_pinned_at, peer_recall, _) = row.map_err(|error| error.to_string())?;
            if let Some(record) = records.get_mut(&memory_id) {
                record.recall_count += peer_recall;
                if let Some(peer_at) = peer_pinned_at.as_deref() {
                    let local_newer = record.pinned_updated_at.as_deref()
                        .is_some_and(|local_at| local_at >= peer_at);
                    if !local_newer {
                        record.pinned = peer_pinned;
                        record.pinned_updated_at = peer_pinned_at;
                    }
                }
            }
        }
    }
    Ok(ids.iter().filter_map(|id| records.get(id).cloned()).collect())
}

#[cfg(test)]
#[path = "memory_queries_tests.rs"]
mod tests;
