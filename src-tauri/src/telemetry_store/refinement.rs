//! Transactional refinement plans and per-action recovery. Raw L0 is untouched.
#[cfg(test)]
#[path = "refinement_tests.rs"]
mod tests;
use super::{MemoryImportance, TelemetryStore};
use crate::memory_refinement::{self, AppliedChange, MemoryState, RefinementAction, RefinementDeleteSelection, RefinementHistorySnapshot, RefinementMemory, RefinementRun, RefinementSelection, StoredRun};
use rusqlite::{params, types::ValueRef, Connection, OptionalExtension};
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};

pub(super) fn initialize(conn: &Connection) -> Result<(), String> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS memory_refinement_runs (
             id TEXT PRIMARY KEY, payload TEXT NOT NULL, view_json TEXT NOT NULL, updated_at TEXT NOT NULL
         );
         CREATE TABLE IF NOT EXISTS memory_refinement_inputs (
             run_id TEXT PRIMARY KEY REFERENCES memory_refinement_runs(id) ON DELETE CASCADE,
             payload TEXT NOT NULL
         );
         CREATE TABLE IF NOT EXISTS memory_refinement_reviews (
             memory_id TEXT PRIMARY KEY, signature TEXT NOT NULL,
             rule_version TEXT NOT NULL, reviewed_at TEXT NOT NULL
         );
         CREATE TABLE IF NOT EXISTS memory_refinement_edits (
             memory_id TEXT PRIMARY KEY, edited_at TEXT NOT NULL
         );
         CREATE TABLE IF NOT EXISTS memory_refinement_provenance (
             memory_id TEXT NOT NULL REFERENCES local_memory_items(id) ON DELETE CASCADE,
             source_id TEXT NOT NULL, payload TEXT NOT NULL,
             PRIMARY KEY(memory_id, source_id)
         );
         CREATE INDEX IF NOT EXISTS idx_memory_refinement_history
             ON memory_refinement_runs(updated_at DESC);",
    ).map_err(|error| error.to_string())
}

fn rows(conn: &Connection, sql: &str, id: &str) -> Result<Vec<Value>, String> {
    let mut statement = conn.prepare(sql).map_err(|error| error.to_string())?;
    let names: Vec<_> = statement.column_names().iter().map(|name| name.to_string()).collect();
    let mut cursor = statement.query([id]).map_err(|error| error.to_string())?;
    let mut result = Vec::new();
    while let Some(row) = cursor.next().map_err(|error| error.to_string())? {
        let mut object = Map::new();
        for (index, name) in names.iter().enumerate() {
            let value = match row.get_ref(index).map_err(|error| error.to_string())? {
                ValueRef::Null => Value::Null,
                ValueRef::Integer(value) => json!(value),
                ValueRef::Real(value) => json!(value),
                ValueRef::Text(value) => json!(std::str::from_utf8(value).map_err(|error| error.to_string())?),
                ValueRef::Blob(_) => return Err("精炼记录不支持二进制字段".into()),
            };
            object.insert(name.clone(), value);
        }
        result.push(Value::Object(object));
    }
    Ok(result)
}

fn string(row: &Value, field: &str) -> String {
    row.get(field).and_then(Value::as_str).unwrap_or_default().to_string()
}

fn digest(value: &impl serde::Serialize) -> Result<String, String> {
    Ok(super::hex(&Sha256::digest(serde_json::to_vec(value).map_err(|error| error.to_string())?)))
}

fn snapshot(conn: &Connection, id: &str) -> Result<Option<MemoryState>, String> {
    let Some(row) = rows(conn, "SELECT * FROM local_memory_items WHERE id = ?1", id)?.pop() else { return Ok(None) };
    let domains = rows(conn, "SELECT * FROM local_memory_domains WHERE memory_id = ?1 ORDER BY domain_id, source", id)?;
    let importance = rows(conn, "SELECT * FROM memory_importance WHERE memory_id = ?1", id)?;
    let manual: bool = conn.query_row("SELECT EXISTS(SELECT 1 FROM memory_refinement_edits WHERE memory_id = ?1)", [id], |row| row.get(0))
        .map_err(|error| error.to_string())?;
    let effective_importance = super::memory_queries::importance_records_on(conn, &[id.to_string()])?;
    let protected = row["user_defined"].as_i64().unwrap_or(0) != 0 || manual
        || effective_importance.iter().any(|item| item.pinned)
        // Before explicit edit tracking existed, conservative protection of
        // locally modified rows is preferable to rewriting a user's edit.
        || row["created_at"] != row["updated_at"]
        || id.starts_with("local-l1:refined:") || id.starts_with("local-l1:restored:");
    let signature = digest(&(&row, &domains, manual, protected))?;
    let domain_ids = domains.iter().map(|domain| string(domain, "domain_id")).collect::<HashSet<_>>().into_iter().collect();
    Ok(Some(MemoryState {
        view: RefinementMemory {
            id: id.to_string(), memory: string(&row, "memory"), memory_type: string(&row, "memory_type"),
            durability: string(&row, "durability"), source: string(&row, "source"), session_id: string(&row, "session_id"),
            source_event_key: string(&row, "source_event_key"), ordinal: row["ordinal"].as_i64().unwrap_or_default(),
            event_time: row["event_time"].as_str().map(str::to_string), updated_at: string(&row, "updated_at"),
            domains: domain_ids, source_excerpt: None,
        },
        row, domains, importance, signature, protected,
        provenance: rows(conn, "SELECT * FROM memory_refinement_provenance WHERE memory_id = ?1 ORDER BY source_id", id)?,
        layers: rows(conn, "SELECT * FROM memory_layer_sources WHERE source_layer = 'l1' AND source_id = ?1 ORDER BY document_id", id)?,
    }))
}

fn save(conn: &Connection, run: &StoredRun) -> Result<(), String> {
    conn.execute(
        "INSERT INTO memory_refinement_runs(id, payload, view_json, updated_at) VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT(id) DO UPDATE SET payload = excluded.payload, view_json = excluded.view_json, updated_at = excluded.updated_at",
        params![run.view.id, serde_json::to_string(run).map_err(|error| error.to_string())?,
            serde_json::to_string(&run.view).map_err(|error| error.to_string())?, run.view.updated_at],
    ).map_err(|error| error.to_string())?;
    let has_inputs: bool = conn.query_row("SELECT EXISTS(SELECT 1 FROM memory_refinement_inputs WHERE run_id = ?1)",
        [&run.view.id], |row| row.get(0)).map_err(|error| error.to_string())?;
    if !has_inputs {
        // Immutable whole-library data is serialized once, not every four-item
        // checkpoint. Polling/history reads only the public view projection.
        let inputs = json!({"inventory": run.inventory, "tasks": run.tasks,
            "task_domains": run.task_domains, "task_targets": run.task_targets,
            "targets": run.targets, "review_version": run.review_version});
        conn.execute("INSERT INTO memory_refinement_inputs(run_id, payload) VALUES (?1, ?2)",
            params![run.view.id, serde_json::to_string(&inputs).map_err(|error| error.to_string())?])
            .map_err(|error| error.to_string())?;
    }
    Ok(())
}

fn load(conn: &Connection, id: &str) -> Result<StoredRun, String> {
    #[derive(serde::Deserialize)]
    struct Inputs {
        inventory: Vec<MemoryState>, tasks: Vec<Vec<usize>>, targets: HashSet<usize>, review_version: String,
        #[serde(default)]
        task_domains: Vec<String>,
        #[serde(default)]
        task_targets: Vec<Vec<usize>>,
    }
    let (payload, inputs): (String, String) = conn.query_row(
        "SELECT r.payload, i.payload FROM memory_refinement_runs r
         JOIN memory_refinement_inputs i ON i.run_id = r.id WHERE r.id = ?1", [id], |row| Ok((row.get(0)?, row.get(1)?)),
    ).optional().map_err(|error| error.to_string())?.ok_or("未找到完整的记忆精炼记录")?;
    let mut run: StoredRun = serde_json::from_str(&payload).map_err(|error| format!("精炼记录损坏：{error}"))?;
    let inputs: Inputs = serde_json::from_str(&inputs).map_err(|error| format!("精炼输入损坏：{error}"))?;
    run.inventory = inputs.inventory;
    run.tasks = inputs.tasks;
    run.task_domains = inputs.task_domains;
    run.task_targets = inputs.task_targets;
    run.targets = inputs.targets;
    run.review_version = inputs.review_version;
    memory_refinement::normalize_notices(&mut run.view);
    Ok(run)
}

// All names passed here are internal constants, never model or WebView input.
fn insert_row(conn: &Connection, table: &str, value: &Value) -> Result<(), String> {
    let object = value.as_object().ok_or("无效的精炼恢复记录")?;
    let columns: Vec<_> = object.keys().map(|key| format!("\"{key}\"")).collect();
    let placeholders: Vec<_> = (1..=columns.len()).map(|index| format!("?{index}")).collect();
    let values: Vec<rusqlite::types::Value> = object.values().map(|value| match value {
        Value::Null => rusqlite::types::Value::Null,
        Value::Number(number) if number.is_i64() => rusqlite::types::Value::Integer(number.as_i64().unwrap_or_default()),
        Value::Number(number) => rusqlite::types::Value::Real(number.as_f64().unwrap_or_default()),
        Value::String(text) => rusqlite::types::Value::Text(text.clone()),
        _ => rusqlite::types::Value::Text(value.to_string()),
    }).collect();
    conn.execute(&format!("INSERT OR REPLACE INTO {table} ({}) VALUES ({})", columns.join(","), placeholders.join(",")),
        rusqlite::params_from_iter(values)).map_err(|error| error.to_string())?;
    Ok(())
}

pub(super) fn mark_edited(conn: &Connection, id: &str, now: &str) -> Result<(), String> {
    conn.execute("INSERT OR REPLACE INTO memory_refinement_edits(memory_id, edited_at) VALUES (?1, ?2)", params![id, now])
        .map_err(|error| error.to_string())?;
    Ok(())
}

fn remove(conn: &Connection, id: &str, now: &str) -> Result<(), String> {
    super::l1_sync::mark_deleted(conn, id, now).map(|_| ())
}

fn pinned_memory_ids(conn: &Connection, ids: &[String]) -> Result<HashSet<String>, String> {
    if ids.is_empty() { return Ok(HashSet::new()); }
    let placeholders = (1..=ids.len()).map(|index| format!("?{index}")).collect::<Vec<_>>().join(",");
    // Include peer-only states when no local score exists. Parse actual instants
    // so Z/offset and fractional-second formats cannot reverse pin/unpin order.
    let mut statement = conn.prepare(&format!(
        "SELECT i.memory_id, i.pinned, i.pinned_updated_at, 1 AS local_state, i.rowid AS entry_order
           FROM memory_importance i JOIN local_memory_items m ON m.id = i.memory_id
          WHERE i.memory_id IN ({placeholders})
         UNION ALL
         SELECT p.memory_id, p.pinned, p.pinned_updated_at, 0, p.rowid
           FROM memory_user_state_peer p JOIN local_memory_items m ON m.id = p.memory_id
          WHERE p.memory_id IN ({placeholders})
         ORDER BY local_state DESC, entry_order ASC",
    )).map_err(|error| error.to_string())?;
    let rows = statement.query_map(rusqlite::params_from_iter(ids.iter()), |row| Ok((
        row.get::<_, String>(0)?, row.get::<_, i64>(1)? != 0, row.get::<_, Option<String>>(2)?,
    ))).map_err(|error| error.to_string())?;
    let mut decisions = HashMap::<String, (bool, Option<chrono::DateTime<chrono::FixedOffset>>)>::new();
    for row in rows {
        let (id, pinned, timestamp) = row.map_err(|error| error.to_string())?;
        let at = timestamp.as_deref().map(chrono::DateTime::parse_from_rfc3339).transpose()
            .map_err(|_| "置顶状态时间无效，删除未执行，请重新确认置顶状态".to_string())?;
        let replace = decisions.get(&id).is_none_or(|(current_pin, current_at)| match (&at, current_at) {
            (Some(new), Some(old)) => new > old,
            (Some(_), None) => true,
            (None, Some(_)) => false,
            (None, None) => pinned && !*current_pin,
        });
        if replace { decisions.insert(id, (pinned, at)); }
    }
    Ok(decisions.into_iter().filter_map(|(id, (pinned, _))| pinned.then_some(id)).collect())
}

fn copy_user_state(conn: &Connection, id: &str, state: &MemoryImportance, now: &str) -> Result<(), String> {
    conn.execute(
        "INSERT INTO memory_importance(memory_id, pinned, pinned_updated_at, recall_count, recall_updated_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?5)
         ON CONFLICT(memory_id) DO UPDATE SET pinned=excluded.pinned, pinned_updated_at=excluded.pinned_updated_at,
             recall_count=excluded.recall_count, recall_updated_at=excluded.recall_updated_at, updated_at=excluded.updated_at",
        params![id, state.pinned, state.pinned_updated_at.as_deref().unwrap_or(now), state.recall_count, now],
    ).map_err(|error| error.to_string())?;
    Ok(())
}

fn create_refined_copy(
    conn: &Connection, original: &MemoryState, content: &str, memory_type: &str, durability: &str, now: &str,
) -> Result<String, String> {
    let id = format!("local-l1:refined:{}", uuid::Uuid::new_v4());
    let mut row = original.row.clone();
    row["id"] = json!(id);
    row["source_event_key"] = json!(format!("refinement:{}", uuid::Uuid::new_v4()));
    row["ordinal"] = json!(0);
    row["origin_device"] = Value::Null;
    row["memory"] = json!(content);
    row["memory_type"] = json!(memory_type);
    row["durability"] = json!(durability);
    row["updated_at"] = json!(now);
    insert_row(conn, "local_memory_items", &row)?;
    for importance in &original.importance {
        let mut importance = importance.clone(); importance["memory_id"] = json!(id);
        insert_row(conn, "memory_importance", &importance)?;
    }
    if let Some(effective) = super::memory_queries::importance_records_on(conn, &[original.view.id.clone()])?.into_iter().next() {
        copy_user_state(conn, &id, &effective, now)?;
    }
    mark_edited(conn, &id, now)?;
    Ok(id)
}

fn inherit_sources(conn: &Connection, keep_id: &str, memories: &[MemoryState]) -> Result<(), String> {
    for memory in memories {
        conn.execute(
            "INSERT OR IGNORE INTO memory_refinement_provenance(memory_id, source_id, payload) VALUES (?1, ?2, ?3)",
            params![keep_id, memory.view.id, serde_json::to_string(&memory.view).map_err(|error| error.to_string())?],
        ).map_err(|error| error.to_string())?;
        for provenance in &memory.provenance {
            let mut provenance = provenance.clone(); provenance["memory_id"] = json!(keep_id);
            insert_row(conn, "memory_refinement_provenance", &provenance)?;
        }
        for domain in &memory.domains {
            conn.execute(
                "INSERT OR IGNORE INTO local_memory_domains(memory_id, domain_id, weight, source, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![keep_id, string(domain, "domain_id"), domain["weight"].as_f64().unwrap_or(0.0),
                    string(domain, "source"), string(domain, "created_at"), string(domain, "updated_at")],
            ).map_err(|error| error.to_string())?;
        }
        for layer in &memory.layers {
            conn.execute("INSERT OR IGNORE INTO memory_layer_sources(document_id, source_layer, source_id) VALUES (?1, 'l1', ?2)",
                params![string(layer, "document_id"), keep_id]).map_err(|error| error.to_string())?;
        }
    }
    Ok(())
}

fn replace_refinement_domains(conn: &Connection, memory_id: &str, domain_ids: &[String], now: &str) -> Result<(), String> {
    let mut ids = domain_ids.iter().map(|id| super::domain_merge::resolve(conn,id)?
        .ok_or_else(|| "选择的标签已不存在，请刷新标签列表后重试".to_string())).collect::<Result<Vec<_>,String>>()?;
    ids.sort();
    ids.dedup();
    if ids.contains(&"unclassified".to_string()) && ids.len() > 1 {
        return Err("未分类不能与其他标签同时选择，请重新选择标签".into());
    }
    for id in &ids {
        let exists: bool = conn.query_row("SELECT EXISTS(SELECT 1 FROM memory_domains WHERE id = ?1)", [id], |row| row.get(0))
            .map_err(|error| error.to_string())?;
        if !exists { return Err("选择的标签已不存在，请刷新标签列表后重试".into()); }
    }
    if ids.is_empty() {
        let fallback: bool = conn.query_row("SELECT EXISTS(SELECT 1 FROM memory_domains WHERE id = 'unclassified')", [], |row| row.get(0))
            .map_err(|error| error.to_string())?;
        if fallback { ids.push("unclassified".into()); }
    }
    conn.execute("DELETE FROM local_memory_domains WHERE memory_id = ?1", [memory_id]).map_err(|error| error.to_string())?;
    for id in ids {
        conn.execute(
            "INSERT INTO local_memory_domains(memory_id, domain_id, weight, source, created_at, updated_at)
             VALUES (?1, ?2, ?3, 'manual', ?4, ?4)",
            params![memory_id, id, if id == "unclassified" { 0.0 } else { 1.0 }, now],
        ).map_err(|error| error.to_string())?;
    }
    let (memory, memory_type, durability): (String, String, String) = conn.query_row(
        "SELECT memory, memory_type, durability FROM local_memory_items WHERE id = ?1", [memory_id],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    ).map_err(|error| error.to_string())?;
    let hash = super::memory_domain_content_hash(&memory, &memory_type, &durability);
    conn.execute(
        "INSERT INTO memory_domain_classification_state(memory_id, content_hash, classified_at) VALUES (?1, ?2, ?3)
         ON CONFLICT(memory_id) DO UPDATE SET content_hash = excluded.content_hash, classified_at = excluded.classified_at",
        params![memory_id, hash, now],
    ).map_err(|error| error.to_string())?;
    Ok(())
}

fn saved_progress(conn: &Connection, id: &str) -> Result<Option<StoredRun>, String> {
    let payload: Option<String> = conn.query_row("SELECT payload FROM memory_refinement_runs WHERE id = ?1", [id], |row| row.get(0))
        .optional().map_err(|error| error.to_string())?;
    payload.map(|payload| serde_json::from_str(&payload).map_err(|error| error.to_string())).transpose()
}

fn merge_review_decisions(worker: &mut StoredRun, saved: &StoredRun) {
    for reviewed in &saved.view.actions {
        if let Some(action) = worker.view.actions.iter_mut().find(|action| action.id == reviewed.id) {
            if reviewed.status != "pending" || reviewed.recommended_source_id.is_some() { *action = reviewed.clone(); }
        } else {
            worker.view.actions.push(reviewed.clone());
        }
    }
    // Only review transactions create recovery journals. Their latest version
    // must survive any stale background snapshot, including final publication.
    worker.changes = saved.changes.clone();
    for warning in &saved.view.warnings {
        // Analysis owns the current semantic outcome and history counters.
        // An apply/index notice based on an older view cannot revive a fault
        // that the latest query has already recovered from.
        if worker.semantic_progress.is_some() && memory_refinement::is_semantic_review_notice(warning) { continue; }
        if !worker.view.warnings.contains(warning) && worker.view.warnings.len() < 30 {
            worker.view.warnings.push(warning.clone());
        }
    }
}

fn publish_background(conn: &Connection, run: &mut StoredRun, checked: &[usize]) -> Result<(), String> {
    let saved = saved_progress(conn, &run.view.id)?.ok_or("精炼记录已删除或不存在，停止保存旧任务进度")?;
    let published_ids: HashSet<_> = saved.view.actions.iter().map(|action| action.id.clone()).collect();
    merge_review_decisions(run, &saved);
    // The request may have been in flight while another review edited/deleted
    // its source. Publish those new proposals as stale instead of actionable.
    let inventory = &run.inventory;
    for action in &mut run.view.actions {
        if action.status != "pending" || published_ids.contains(&action.id) { continue; }
        let mut valid = true;
        for original in &action.originals {
            let expected = inventory.iter().find(|memory| memory.view.id == original.id);
            match snapshot(conn, &original.id)? {
                Some(current) if !current.protected && expected.is_some_and(|memory| memory.signature == current.signature) => {},
                _ => { valid = false; break; }
            }
        }
        if !valid {
            action.status = "stale".into();
            action.message = Some("来源在分析期间已修改或删除，本建议未应用".into());
        }
    }
    for &index in checked {
        let memory = &run.inventory[index];
        conn.execute(
            "INSERT OR REPLACE INTO memory_refinement_reviews(memory_id, signature, rule_version, reviewed_at)
             SELECT ?1, ?2, ?3, ?4 WHERE EXISTS(SELECT 1 FROM local_memory_items WHERE id = ?1)",
            params![memory.view.id, memory.signature, run.review_version, chrono::Utc::now().to_rfc3339()],
        ).map_err(|error| error.to_string())?;
    }
    run.view.updated_at = chrono::Utc::now().to_rfc3339();
    memory_refinement::normalize_notices(&mut run.view);
    save(conn, run)
}

fn history_snapshot(conn: &Connection) -> Result<RefinementHistorySnapshot, String> {
    let mut statement = conn.prepare("SELECT id, updated_at FROM memory_refinement_runs ORDER BY id")
        .map_err(|error| error.to_string())?;
    let rows = statement.query_map([], |row| Ok((row.get::<_,String>(0)?, row.get::<_,String>(1)?)))
        .map_err(|error| error.to_string())?.collect::<Result<Vec<_>, _>>().map_err(|error| error.to_string())?;
    Ok(RefinementHistorySnapshot { count: rows.len(), revision: digest(&rows)? })
}

fn ensure_history_sync_compatibility(conn: &Connection, id: Option<&str>) -> Result<(), String> {
    // Journals may be the only source of a pre-versioned result's identity
    // upgrade. Clearing history cannot silently discard that sync information.
    let legacy: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM memory_refinement_runs r,
             json_each(r.payload, '$.changes') c, json_each(c.value, '$.after') a
             JOIN local_memory_items m ON m.id=json_extract(a.value, '$.view.id')
         WHERE (?1 IS NULL OR r.id=?1) AND m.user_defined=0 AND m.origin_device IS NULL
           AND m.id NOT LIKE 'local-l1:refined:%' AND m.id NOT LIKE 'local-l1:restored:%'
           AND NOT EXISTS(SELECT 1 FROM l1_deletion_tombstones t WHERE t.memory_id=m.id))",
        [id], |row| row.get(0),
    ).map_err(|error| error.to_string())?;
    if legacy { return Err("历史包含尚未完成同步升级的旧版精炼结果；请先完成一次云同步再清理记录，避免丢失兼容信息".into()); }
    Ok(())
}

impl TelemetryStore {
    pub(crate) fn refinement_history_snapshot(&self) -> Result<RefinementHistorySnapshot, String> {
        let conn = self.conn.lock().map_err(|_| "telemetry store lock poisoned".to_string())?;
        history_snapshot(&conn)
    }

    pub(crate) fn refinement_clear(&self, expected_revision: &str) -> Result<usize, String> {
        let mut conn = self.conn.lock().map_err(|_| "telemetry store lock poisoned".to_string())?;
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate).map_err(|error| error.to_string())?;
        if history_snapshot(&tx)?.revision != expected_revision {
            return Err("历史记录在确认期间已更新，未清空；请重新确认全部记录范围".into());
        }
        ensure_history_sync_compatibility(&tx, None)?;
        tx.execute("DELETE FROM memory_refinement_inputs", []).map_err(|error| error.to_string())?;
        let deleted = tx.execute("DELETE FROM memory_refinement_runs", []).map_err(|error| error.to_string())?;
        tx.commit().map_err(|error| error.to_string())?;
        Ok(deleted)
    }

    /// Creation is explicit. Progress saves cannot recreate a forgotten run.
    pub(crate) fn refinement_create(&self, run: &StoredRun) -> Result<(), String> {
        let mut conn = self.conn.lock().map_err(|_| "telemetry store lock poisoned".to_string())?;
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate).map_err(|error| error.to_string())?;
        let exists: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM memory_refinement_runs WHERE id=?1)", [&run.view.id], |row| row.get(0))
            .map_err(|error| error.to_string())?;
        if exists { return Err("同一精炼记录已存在，请刷新历史记录".into()); }
        save(&tx, run)?;
        tx.commit().map_err(|error| error.to_string())
    }

    pub(crate) fn refinement_forget(&self, id: &str, expected_updated_at: &str) -> Result<bool, String> {
        let mut conn = self.conn.lock().map_err(|_| "telemetry store lock poisoned".to_string())?;
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate).map_err(|error| error.to_string())?;
        let current: Option<String> = tx.query_row("SELECT updated_at FROM memory_refinement_runs WHERE id=?1", [id], |row| row.get(0))
            .optional().map_err(|error| error.to_string())?;
        let Some(current) = current else { return Ok(false) };
        if current != expected_updated_at { return Err("记录在确认期间已更新，未删除；请刷新后重新确认".into()); }
        ensure_history_sync_compatibility(&tx, Some(id))?;
        tx.execute("DELETE FROM memory_refinement_inputs WHERE run_id=?1", [id]).map_err(|error| error.to_string())?;
        let deleted = tx.execute("DELETE FROM memory_refinement_runs WHERE id=?1", [id]).map_err(|error| error.to_string())?;
        tx.commit().map_err(|error| error.to_string())?;
        Ok(deleted > 0)
    }

    pub(crate) fn refinement_publish(&self, run: &mut StoredRun, checked: &[usize]) -> Result<(), String> {
        let mut conn = self.conn.lock().map_err(|_| "telemetry store lock poisoned".to_string())?;
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate).map_err(|error| error.to_string())?;
        publish_background(&tx, run, checked)?;
        tx.commit().map_err(|error| error.to_string())
    }

    pub(crate) fn refinement_sync_reviews(&self, run: &mut StoredRun) -> Result<(), String> {
        let conn = self.conn.lock().map_err(|_| "telemetry store lock poisoned".to_string())?;
        let saved = saved_progress(&conn, &run.view.id)?.ok_or("精炼记录已删除或不存在，停止旧任务")?;
        merge_review_decisions(run, &saved);
        memory_refinement::normalize_notices(&mut run.view);
        Ok(())
    }

    pub(crate) fn refinement_current_candidates(&self, candidates: &[MemoryState]) -> Result<Vec<MemoryState>, String> {
        let conn = self.conn.lock().map_err(|_| "telemetry store lock poisoned".to_string())?;
        let mut current = Vec::new();
        for candidate in candidates {
            if snapshot(&conn, &candidate.view.id)?.is_some_and(|state| state.signature == candidate.signature) {
                current.push(candidate.clone());
            }
        }
        Ok(current)
    }

    pub(crate) fn refinement_save_conflict_recommendation(
        &self, id: &str, action_id: &str, candidates: &[MemoryState], mut recommendation: RefinementAction,
    ) -> Result<RefinementRun, String> {
        let mut conn = self.conn.lock().map_err(|_| "telemetry store lock poisoned".to_string())?;
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate).map_err(|error| error.to_string())?;
        let mut run = load(&tx, id)?;
        let index = run.view.actions.iter().position(|action| action.id == action_id).ok_or("未找到冲突建议，请刷新记录")?;
        let action = &run.view.actions[index];
        if action.kind != "conflict" || action.status != "pending" { return Err("该冲突项已处理，未覆盖已有决定".into()); }
        if action.recommended_source_id.is_some() { return Ok(run.view); }
        let expected: HashSet<_> = action.originals.iter().map(|memory| memory.id.as_str()).collect();
        if candidates.len() != expected.len() || candidates.iter().any(|memory| !expected.contains(memory.view.id.as_str()))
            || recommendation.kind != "conflict" || recommendation.status != "pending"
            || recommendation.originals.len() != expected.len()
            || recommendation.originals.iter().map(|memory| memory.id.as_str()).collect::<HashSet<_>>() != expected
            || !recommendation.recommended_source_id.as_deref().is_some_and(|id| expected.contains(id)) {
            return Err("处理建议未覆盖本组完整来源，原记忆保持不变".into());
        }
        let mut current = true;
        for memory in candidates {
            if snapshot(&tx, &memory.view.id)?.is_none_or(|state| state.protected || state.signature != memory.signature) {
                current = false;
                break;
            }
        }
        if current {
            recommendation.id = action_id.to_string();
            recommendation.originals = action.originals.clone();
            run.view.actions[index] = recommendation;
        } else {
            run.view.actions[index].status = "stale".into();
            run.view.actions[index].message = Some("来源在建议生成期间已修改或删除，未覆盖新内容，请重新分析".into());
        }
        run.view.updated_at = chrono::Utc::now().to_rfc3339();
        save(&tx, &run)?;
        tx.commit().map_err(|error| error.to_string())?;
        Ok(run.view)
    }

    pub(crate) fn refinement_append_warnings(&self, id: &str, warnings: Vec<String>) -> Result<RefinementRun, String> {
        let mut conn = self.conn.lock().map_err(|_| "telemetry store lock poisoned".to_string())?;
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate).map_err(|error| error.to_string())?;
        let mut run = saved_progress(&tx, id)?.ok_or("未找到记忆精炼记录")?;
        for warning in warnings {
            if !run.view.warnings.contains(&warning) && run.view.warnings.len() < 30 { run.view.warnings.push(warning); }
        }
        run.view.updated_at = chrono::Utc::now().to_rfc3339();
        memory_refinement::normalize_notices(&mut run.view);
        save(&tx, &run)?;
        tx.commit().map_err(|error| error.to_string())?;
        Ok(run.view)
    }

    #[cfg(test)]
    pub(crate) fn refinement_save(&self, run: &StoredRun) -> Result<(), String> {
        let mut conn = self.conn.lock().map_err(|_| "telemetry store lock poisoned".to_string())?;
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate).map_err(|error| error.to_string())?;
        save(&tx, run)?;
        tx.commit().map_err(|error| error.to_string())
    }

    pub(crate) fn refinement_view(&self, id: &str) -> Result<RefinementRun, String> {
        let conn = self.conn.lock().map_err(|_| "telemetry store lock poisoned".to_string())?;
        let payload: String = conn.query_row("SELECT view_json FROM memory_refinement_runs WHERE id = ?1", [id], |row| row.get(0))
            .optional().map_err(|error| error.to_string())?.ok_or("未找到记忆精炼记录")?;
        let mut view: RefinementRun = serde_json::from_str(&payload).map_err(|error| error.to_string())?;
        memory_refinement::normalize_notices(&mut view);
        Ok(view)
    }

    pub(crate) fn refinement_load(&self, id: &str) -> Result<StoredRun, String> {
        let conn = self.conn.lock().map_err(|_| "telemetry store lock poisoned".to_string())?;
        load(&conn, id)
    }

    pub(crate) fn refinement_history(&self, active_id: Option<&str>) -> Result<Vec<RefinementRun>, String> {
        let conn = self.conn.lock().map_err(|_| "telemetry store lock poisoned".to_string())?;
        let mut statement = conn.prepare("SELECT view_json FROM memory_refinement_runs ORDER BY updated_at DESC LIMIT 20")
            .map_err(|error| error.to_string())?;
        let payloads = statement.query_map([], |row| row.get::<_, String>(0)).map_err(|error| error.to_string())?;
        let mut result = Vec::new();
        for payload in payloads {
            let mut view: RefinementRun = serde_json::from_str(&payload.map_err(|error| error.to_string())?).map_err(|error| error.to_string())?;
            if matches!(view.status.as_str(), "analyzing" | "cancelling") && active_id != Some(view.id.as_str()) {
                view.status = "cancelled".into();
            }
            memory_refinement::normalize_notices(&mut view);
            result.push(view);
        }
        Ok(result)
    }

    pub(crate) fn refinement_prepare(&self, domain_id: Option<String>, incremental: bool) -> Result<StoredRun, String> {
        let conn = self.conn.lock().map_err(|_| "telemetry store lock poisoned".to_string())?;
        if let Some(domain) = domain_id.as_deref() {
            let exists: bool = conn.query_row("SELECT EXISTS(SELECT 1 FROM memory_domains WHERE id = ?1)", [domain], |row| row.get(0))
                .map_err(|error| error.to_string())?;
            if !exists { return Err("选择的记忆领域已不存在，请刷新领域列表".into()); }
        }
        let ids = {
            let mut statement = conn.prepare(
                "SELECT id FROM local_memory_items m
                 WHERE (?1 IS NULL
                    OR EXISTS(SELECT 1 FROM local_memory_domains d WHERE d.memory_id = m.id AND d.domain_id = ?1)
                    OR (?1 = 'unclassified' AND NOT EXISTS(
                        SELECT 1 FROM local_memory_domains d WHERE d.memory_id = m.id AND d.domain_id != 'unclassified')))
                 ORDER BY m.updated_at DESC, m.id",
            ).map_err(|error| error.to_string())?;
            let result = statement.query_map([domain_id.as_deref()], |row| row.get::<_, String>(0))
                .map_err(|error| error.to_string())?.collect::<Result<Vec<_>, _>>().map_err(|error| error.to_string())?;
            result
        };
        let embedding_config: Option<String> = conn.query_row(
            "SELECT value_json FROM app_settings WHERE setting_key = 'memory_semantic_embedding'", [], |row| row.get(0),
        ).optional().map_err(|error| error.to_string())?;
        // A domain-only pass does not establish global comparison coverage;
        // embedding configuration changes also invalidate relation checkpoints.
        let review_version = digest(&(memory_refinement::RULE_VERSION, &domain_id, embedding_config))?;
        let mut inventory = Vec::new();
        let mut targets = HashSet::new();
        let mut protected = 0;
        for id in ids {
            if let Some(state) = snapshot(&conn, &id)? {
                if domain_id.as_deref().is_some_and(|domain| !memory_refinement::in_domain(&state.view, domain)) { continue; }
                if state.protected { protected += 1; }
                else {
                    let checked: bool = incremental && conn.query_row(
                        "SELECT EXISTS(SELECT 1 FROM memory_refinement_reviews WHERE memory_id = ?1 AND signature = ?2 AND rule_version = ?3)",
                        params![id, state.signature, review_version], |row| row.get::<_, bool>(0),
                    ).map_err(|error| error.to_string())?;
                    if !checked { targets.insert(inventory.len()); }
                }
                inventory.push(state);
            }
        }
        let plan = memory_refinement::build_tasks(&inventory, &targets, domain_id.as_deref());
        let now = chrono::Utc::now().to_rfc3339();
        Ok(StoredRun {
            view: RefinementRun {
                id: uuid::Uuid::new_v4().to_string(), created_at: now.clone(), updated_at: now,
                status: "ready".into(), domain_id, incremental, total: targets.len(), processed: 0,
                protected, remaining: targets.len(), actions: Vec::new(), warnings: Vec::new(), activity: None,
            }, inventory, tasks: plan.tasks, task_domains: plan.domains, task_targets: plan.targets,
            targets, reviewed: HashSet::new(), quality_reviewed: HashSet::new(), seen_pairs: HashSet::new(),
            degraded_targets: HashSet::new(), retry_tasks: HashSet::new(), completed_tasks: HashSet::new(),
            review_version, cursor: 0, changes: Vec::new(), semantic_progress: None,
        })
    }

    #[cfg(test)]
    pub(crate) fn refinement_checkpoint(&self, run: &StoredRun, indices: &[usize]) -> Result<(), String> {
        let mut conn = self.conn.lock().map_err(|_| "telemetry store lock poisoned".to_string())?;
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate).map_err(|error| error.to_string())?;
        for &index in indices {
            let memory = &run.inventory[index];
            tx.execute(
                "INSERT OR REPLACE INTO memory_refinement_reviews(memory_id, signature, rule_version, reviewed_at)
             SELECT ?1, ?2, ?3, ?4 WHERE EXISTS(SELECT 1 FROM local_memory_items WHERE id = ?1)",
                params![memory.view.id, memory.signature, run.review_version, run.view.updated_at],
            ).map_err(|error| error.to_string())?;
        }
        save(&tx, run)?;
        tx.commit().map_err(|error| error.to_string())
    }

    pub(crate) fn refinement_skip(&self, id: &str, action_ids: Vec<String>) -> Result<StoredRun, String> {
        let mut conn = self.conn.lock().map_err(|_| "telemetry store lock poisoned".to_string())?;
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate).map_err(|error| error.to_string())?;
        let mut run = load(&tx, id)?;
        let selected: HashSet<_> = action_ids.into_iter().collect();
        for action in &mut run.view.actions {
            if action.status == "pending" && selected.contains(&action.id) { action.status = "skipped".into(); }
        }
        run.view.updated_at = chrono::Utc::now().to_rfc3339();
        save(&tx, &run)?;
        tx.commit().map_err(|error| error.to_string())?;
        Ok(run)
    }

    /// Upgrade applied/reverted pre-versioned refinements before envelope sync.
    /// Content is copied verbatim; matching recovery journals follow the new ID.
    pub(crate) fn prepare_refinements_for_cloud_sync(&self, remote_deleted_ids: &[String]) -> Result<usize, String> {
        let mut conn = self.conn.lock().map_err(|_| "telemetry store lock poisoned".to_string())?;
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate).map_err(|error| error.to_string())?;
        super::l1_sync::prune_deleted_rows(&tx)?;
        let remote_deleted: HashSet<_> = remote_deleted_ids.iter().map(String::as_str).collect();
        let payloads: Vec<String> = {
            let mut statement = tx.prepare(
                "SELECT r.payload FROM memory_refinement_runs r WHERE EXISTS(
                     SELECT 1 FROM json_each(r.payload, '$.changes') c
                     JOIN json_each(c.value, '$.after') a
                     JOIN local_memory_items m ON m.id = json_extract(a.value, '$.view.id')
                     WHERE m.user_defined = 0 AND m.origin_device IS NULL
                       AND m.id NOT LIKE 'local-l1:refined:%' AND m.id NOT LIKE 'local-l1:restored:%'
                       AND NOT EXISTS(SELECT 1 FROM l1_deletion_tombstones t WHERE t.memory_id = m.id))",
            ).map_err(|error| error.to_string())?;
            let rows = statement.query_map([], |row| row.get(0)).map_err(|error| error.to_string())?;
            rows.collect::<Result<Vec<_>, _>>().map_err(|error| error.to_string())?
        };
        let now = chrono::Utc::now().to_rfc3339();
        let mut promoted = HashMap::<String, (String, MemoryState)>::new();
        for payload in payloads {
            let mut run: StoredRun = serde_json::from_str(&payload).map_err(|error| error.to_string())?;
            let handled: HashSet<_> = run.view.actions.iter().filter(|action| matches!(action.status.as_str(), "applied" | "reverted"))
                .map(|action| action.id.clone()).collect();
            let reverted: HashSet<_> = run.view.actions.iter().filter(|action| action.status == "reverted")
                .map(|action| action.id.clone()).collect();
            let mut notices = Vec::new();
            for change in &mut run.changes {
                if !handled.contains(&change.action_id) { continue; }
                for expected in &mut change.after {
                    let old_id = expected.view.id.clone();
                    if remote_deleted.contains(old_id.as_str()) {
                        notices.push((change.action_id.clone(), "该旧版本已被云端删除，未重新发布；原文保留在审阅记录中".to_string()));
                        continue;
                    }
                    if old_id.starts_with("local-l1:refined:") || old_id.starts_with("local-l1:restored:") { continue; }
                    if !promoted.contains_key(&old_id) {
                        let Some(current) = snapshot(&tx, &old_id)? else { continue };
                        let deleted: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM l1_deletion_tombstones WHERE memory_id = ?1)",
                            [&old_id], |row| row.get(0)).map_err(|error| error.to_string())?;
                        if current.row["user_defined"].as_i64().unwrap_or(0) != 0 || !current.row["origin_device"].is_null() || deleted { continue; }
                        let new_id = create_refined_copy(&tx, &current, &current.view.memory, &current.view.memory_type,
                            &current.view.durability, &now)?;
                        inherit_sources(&tx, &new_id, std::slice::from_ref(&current))?;
                        remove(&tx, &old_id, &now)?;
                        let after = snapshot(&tx, &new_id)?.ok_or("旧精炼结果的同步版本不可用")?;
                        promoted.insert(old_id.clone(), (current.signature, after));
                    }
                    let (signature, after) = promoted.get(&old_id).ok_or("精炼同步版本缺失")?;
                    if reverted.contains(&change.action_id) {
                        notices.push((change.action_id.clone(), "已补齐历史撤销结果的云同步版本，内容与来源保留".to_string()));
                    } else if &expected.signature == signature {
                        *expected = after.clone();
                        if !change.removed_ids.contains(&old_id) { change.removed_ids.push(old_id); }
                        notices.push((change.action_id.clone(), "已补齐云同步版本标识，原内容、来源和撤销能力保留".to_string()));
                    } else {
                        // Preserve the user's later correction. Keeping the old
                        // expected snapshot makes automatic undo refuse it.
                        notices.push((change.action_id.clone(), "已为当前内容补齐云同步版本；该条存在后续修改，旧撤销需要人工核对".to_string()));
                    }
                }
            }
            if !notices.is_empty() {
                for (id, message) in notices {
                    if let Some(action) = run.view.actions.iter_mut().find(|action| action.id == id) { action.message = Some(message); }
                }
                run.view.updated_at = now.clone();
                memory_refinement::normalize_notices(&mut run.view);
                save(&tx, &run)?;
            }
        }
        tx.commit().map_err(|error| error.to_string())?;
        Ok(promoted.len())
    }

    pub(crate) fn refinement_apply(&self, id: &str, selections: Vec<RefinementSelection>) -> Result<StoredRun, String> {
        if selections.is_empty() || selections.len() > 200 { return Err("请每次选择 1–200 条精炼建议".into()); }
        let mut conn = self.conn.lock().map_err(|_| "telemetry store lock poisoned".to_string())?;
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate).map_err(|error| error.to_string())?;
        let mut run = load(&tx, id)?;
        let now = chrono::Utc::now().to_rfc3339();
        let mut selected = HashSet::new();
        for selection in selections {
            if !selected.insert(selection.action_id.clone()) { continue; }
            let Some(index) = run.view.actions.iter().position(|action| action.id == selection.action_id) else {
                return Err("精炼建议不存在，请重新加载记录".into());
            };
            let action = run.view.actions[index].clone();
            if action.status != "pending" { continue; }
            let mut before = Vec::new();
            for original in &action.originals {
                let Some(current) = snapshot(&tx, &original.id)? else { break; };
                let expected = run.inventory.iter().find(|state| state.view.id == original.id);
                if current.protected || expected.is_none_or(|state| state.signature != current.signature) { break; }
                before.push(current);
            }
            if before.len() != action.originals.len() {
                run.view.actions[index].status = "stale".into();
                run.view.actions[index].message = Some("原记忆已修改、置顶或删除，已跳过；请重新分析".into());
                continue;
            }
            let (memory_type, durability) = if action.kind == "conflict" {
                let source_id = selection.conflict_source_id.as_deref().ok_or("冲突建议需要先选择原始依据并确认最终内容，不能直接应用模型提示")?;
                if before.len() < 2 || selection.content.is_none() { return Err("请为冲突建议明确填写最终内容".into()); }
                let position = before.iter().position(|memory| memory.view.id == source_id)
                    .ok_or("冲突裁决依据不属于本组原始记忆，未执行修改")?;
                // The selected source supplies time/session/type/retention as
                // well as primary user state. Undo retains every original.
                before.swap(0, position);
                (before[0].view.memory_type.clone(), before[0].view.durability.clone())
            } else {
                if selection.conflict_source_id.is_some() { return Err("只有来源冲突建议可以指定裁决依据".into()); }
                (action.memory_type.clone(), action.durability.clone())
            };
            if action.kind == "archive" && selection.domain_ids.is_some() { return Err("归档建议不生成新记忆，不能同时修改标签".into()); }
            let content = selection.content.as_deref().unwrap_or(&action.content).trim();
            if content.is_empty() || content.chars().count() > 8000 { return Err("精炼内容不能为空，且不能超过 8000 字符".into()); }
            // Even a locally owned ID may also be locally owned on a restored
            // or independently extracted peer. Publish a fresh identity in all
            // cases and globally retire every superseded original ID.
            let removed_ids: Vec<_> = before.iter().map(|memory| memory.view.id.clone()).collect();
            let keep_id = if action.kind == "archive" { None } else {
                let id = create_refined_copy(&tx, &before[0], content, &memory_type, &durability, &now)?;
                inherit_sources(&tx, &id, &before)?;
                if let Some(domain_ids) = selection.domain_ids.as_deref() {
                    replace_refinement_domains(&tx, &id, domain_ids, &now)?;
                }
                Some(id)
            };
            for removed in &before { remove(&tx, &removed.view.id, &now)?; }
            let after = match keep_id {
                Some(id) => vec![snapshot(&tx, &id)?.ok_or("精炼后记忆不可用")?],
                None => Vec::new(),
            };
            run.changes.push(AppliedChange { action_id: action.id.clone(), before, after, removed_ids });
            run.view.actions[index].content = content.to_string();
            run.view.actions[index].memory_type = memory_type;
            run.view.actions[index].durability = durability;
            run.view.actions[index].status = "applied".into();
            run.view.actions[index].message = selection.conflict_source_id.as_deref().map(|source_id|
                format!("已人工确认冲突并保存最终内容，依据：{source_id}；全部原始来源保留，可撤销"));
        }
        let applied_ids: HashSet<_> = run.view.actions.iter().filter(|action| action.status == "applied")
            .map(|action| action.id.clone()).collect();
        let retired: HashSet<_> = run.changes.iter().filter(|change| applied_ids.contains(change.action_id.as_str()))
            .flat_map(|change| change.removed_ids.iter().cloned()).collect();
        for action in &mut run.view.actions {
            if action.status == "pending" && action.originals.iter().any(|memory| retired.contains(memory.id.as_str())) {
                action.status = "stale".into();
                action.message = Some("部分来源已由已确认的建议替代，本建议已失效，未覆盖新内容".into());
            }
        }
        if !matches!(run.view.status.as_str(), "analyzing" | "cancelling") {
            run.view.status = "applied".into();
        }
        run.view.updated_at = now;
        save(&tx, &run)?;
        tx.commit().map_err(|error| error.to_string())?;
        Ok(run)
    }

    pub(crate) fn refinement_delete(&self, id: &str, selections: Vec<RefinementDeleteSelection>) -> Result<StoredRun, String> {
        if selections.is_empty() || selections.len() > 200 { return Err("请每次选择 1–200 组关联记忆".into()); }
        let mut conn = self.conn.lock().map_err(|_| "telemetry store lock poisoned".to_string())?;
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate).map_err(|error| error.to_string())?;
        let mut run = load(&tx, id)?;
        let mut selected = HashSet::new();
        let mut plans = Vec::new();
        // Validate every group before deleting any row. A confirmation based on
        // a pending proposal must never delete a newer applied result silently.
        for selection in selections {
            if !selected.insert(selection.action_id.clone()) { continue; }
            let index = run.view.actions.iter().position(|action| action.id == selection.action_id)
                .ok_or("精炼建议不存在，删除未执行，请刷新记录")?;
            let action = &run.view.actions[index];
            if action.status == "deleted" {
                // A retried request also repairs a stale client that recreated
                // an already-retired ID. It never expands to a new identity.
                let mut ids: HashSet<_> = action.originals.iter().map(|memory| memory.id.clone()).collect();
                for change in run.changes.iter().filter(|change| change.action_id == action.id) {
                    ids.extend(change.removed_ids.iter().cloned());
                }
                let mut ids: Vec<_> = ids.into_iter().collect(); ids.sort();
                plans.push((index, ids, Vec::new()));
                continue;
            }
            if action.status != selection.expected_status || !matches!(action.status.as_str(), "pending" | "skipped" | "applied") {
                return Err("建议在确认期间已被处理，删除未执行，请重新选择并确认".into());
            }
            let mut expected = HashMap::<String, MemoryState>::new();
            for original in &action.originals {
                let state = run.inventory.iter().find(|memory| memory.view.id == original.id)
                    .ok_or("无法确定完整关联范围，删除未执行，请重新分析")?;
                expected.insert(original.id.clone(), state.clone());
            }
            if expected.is_empty() { return Err("关联记忆范围为空，删除未执行".into()); }
            let required: HashSet<String> = if action.status == "applied" {
                let change = run.changes.iter().rev().find(|change| change.action_id == action.id)
                    .ok_or("当前精炼版本不可用，删除未执行，请核对记忆列表")?;
                if action.kind != "archive" && change.after.is_empty() { return Err("当前精炼版本不可用，删除未执行".into()); }
                for state in &change.after { expected.insert(state.view.id.clone(), state.clone()); }
                change.after.iter().map(|state| state.view.id.clone()).collect()
            } else { expected.keys().cloned().collect() };
            let mut ids: Vec<_> = expected.keys().cloned().collect();
            ids.sort();
            let pinned = pinned_memory_ids(&tx, &ids)?;
            let mut before = Vec::new();
            for memory_id in &ids {
                match snapshot(&tx, memory_id)? {
                    Some(current) => {
                        if current.row["user_defined"].as_i64().unwrap_or(0) != 0 {
                            return Err("包含用户自定义记忆，删除未执行；请在记忆列表逐条确认".into());
                        }
                        if pinned.contains(memory_id) { return Err("包含置顶记忆，删除未执行；请先取消置顶再确认".into()); }
                        if expected.get(memory_id).is_none_or(|state| state.signature != current.signature) {
                            return Err("关联记忆在分析后已变化，删除未执行；请重新核对或分析".into());
                        }
                        before.push(current);
                    }
                    None if required.contains(memory_id) => return Err("关联记忆已不存在或生成了新版本，删除未执行；请重新核对记忆列表".into()),
                    None => {}, // Previously retired originals of an applied group.
                }
            }
            plans.push((index, ids, before));
        }
        let now = chrono::Utc::now().to_rfc3339();
        let mut deleted = HashSet::new();
        for (index, ids, before) in plans {
            for memory_id in &ids {
                if deleted.insert(memory_id.clone()) { remove(&tx, memory_id, &now)?; }
            }
            let action_id = run.view.actions[index].id.clone();
            if let Some(change) = run.changes.iter_mut().rev().find(|change| change.action_id == action_id) {
                for memory_id in &ids {
                    if !change.removed_ids.contains(memory_id) { change.removed_ids.push(memory_id.clone()); }
                }
            } else {
                run.changes.push(AppliedChange { action_id, before, after: Vec::new(), removed_ids: ids });
            }
            run.view.actions[index].status = "deleted".into();
            run.view.actions[index].message = Some("本组关联记忆已删除，删除标记将随云同步传播；原始会话与审阅记录保留".into());
        }
        for action in &mut run.view.actions {
            if action.status == "pending" && action.originals.iter().any(|memory| deleted.contains(&memory.id)) {
                action.status = "stale".into();
                action.message = Some("部分来源已随关联组删除，本建议已失效；未选择的其他记忆保持不变".into());
            }
        }
        if !matches!(run.view.status.as_str(), "analyzing" | "cancelling") { run.view.status = "applied".into(); }
        run.view.updated_at = now;
        save(&tx, &run)?;
        tx.commit().map_err(|error| error.to_string())?;
        Ok(run)
    }

    pub(crate) fn refinement_undo(&self, id: &str) -> Result<StoredRun, String> {
        let mut conn = self.conn.lock().map_err(|_| "telemetry store lock poisoned".to_string())?;
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate).map_err(|error| error.to_string())?;
        let mut run = load(&tx, id)?;
        let now = chrono::Utc::now().to_rfc3339();
        for change in run.changes.iter().rev() {
            let Some(index) = run.view.actions.iter().position(|action| action.id == change.action_id && action.status == "applied") else { continue; };
            let mut safe = true;
            for expected in &change.after {
                if snapshot(&tx, &expected.view.id)?.is_none_or(|state| state.signature != expected.signature) { safe = false; }
            }
            for removed in &change.removed_ids {
                if snapshot(&tx, removed)?.is_some() { safe = false; }
            }
            if !safe {
                run.view.actions[index].message = Some("精炼后记忆已有后续修改，本组未撤销，避免覆盖新内容".into());
                continue;
            }
            let current_user_state = match change.after.first() {
                Some(after) => super::memory_queries::importance_records_on(&tx, &[after.view.id.clone()])?.into_iter().next(),
                None => None,
            };
            // Retire the applied version even for legacy journals that kept
            // the original ID. Undo is a new global version, never an un-delete.
            for current in &change.after {
                remove(&tx, &current.view.id, &now)?;
                tx.execute("DELETE FROM memory_layer_sources WHERE source_layer = 'l1' AND source_id = ?1", [&current.view.id])
                    .map_err(|error| error.to_string())?;
            }
            let mut mapping = HashMap::new();
            for (position, memory) in change.before.iter().enumerate() {
                let restored_id = format!("local-l1:restored:{}", uuid::Uuid::new_v4());
                mapping.insert(memory.view.id.clone(), restored_id.clone());
                let mut row = memory.row.clone();
                row["id"] = json!(restored_id);
                row["origin_device"] = Value::Null;
                row["updated_at"] = json!(now);
                // Cloud auto-memory tombstones are append-only. A fresh ID
                // works even when a peer owns a local copy of the original ID.
                row["source_event_key"] = json!(format!("refinement-restore:{}", uuid::Uuid::new_v4()));
                row["ordinal"] = json!(0);
                row["created_at"] = json!(now);
                insert_row(&tx, "local_memory_items", &row)?;
                tx.execute("DELETE FROM local_memory_domains WHERE memory_id = ?1", [&restored_id]).map_err(|error| error.to_string())?;
                for domain in &memory.domains {
                    // A domain deliberately deleted since this run stays deleted.
                    if let Some(id) = super::domain_merge::resolve(&tx, &string(domain, "domain_id"))? {
                        tx.execute("INSERT INTO local_memory_domains(memory_id,domain_id,weight,source,created_at,updated_at)
                            VALUES (?1,?2,?3,?4,?5,?6) ON CONFLICT(memory_id,domain_id,source)
                            DO UPDATE SET weight=MAX(weight,excluded.weight),updated_at=excluded.updated_at",
                            params![restored_id,id,domain["weight"].as_f64().unwrap_or(0.0),string(domain,"source"),
                                string(domain,"created_at"),now]).map_err(|error| error.to_string())?;
                    }
                }
                tx.execute("DELETE FROM memory_refinement_provenance WHERE memory_id = ?1", [&restored_id]).map_err(|error| error.to_string())?;
                for provenance in &memory.provenance {
                    let mut provenance = provenance.clone(); provenance["memory_id"] = json!(restored_id);
                    insert_row(&tx, "memory_refinement_provenance", &provenance)?;
                }
                // Always preserve the original source event/session, even for a
                // new active identity whose synthetic event key protects it
                // from future raw-conversation extraction replacement.
                tx.execute("INSERT OR IGNORE INTO memory_refinement_provenance(memory_id, source_id, payload) VALUES (?1, ?2, ?3)",
                    params![restored_id, memory.view.id, serde_json::to_string(&memory.view).map_err(|error| error.to_string())?])
                    .map_err(|error| error.to_string())?;
                for importance in &memory.importance {
                    let mut importance = importance.clone(); importance["memory_id"] = json!(restored_id);
                    insert_row(&tx, "memory_importance", &importance)?;
                }
                if position == 0 {
                    if let Some(state) = &current_user_state { copy_user_state(&tx, &restored_id, state, &now)?; }
                }
                for layer in &memory.layers {
                    let document_exists: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM memory_layer_documents WHERE id = ?1)",
                        [string(layer, "document_id")], |row| row.get(0)).map_err(|error| error.to_string())?;
                    if document_exists {
                        let mut layer = layer.clone(); layer["source_id"] = json!(restored_id);
                        insert_row(&tx, "memory_layer_sources", &layer)?;
                    }
                }
                tx.execute("DELETE FROM memory_domain_classification_state WHERE memory_id = ?1", [&restored_id]).map_err(|error| error.to_string())?;
                tx.execute("DELETE FROM memory_refinement_reviews WHERE memory_id = ?1", [&restored_id]).map_err(|error| error.to_string())?;
                mark_edited(&tx, &restored_id, &now)?;
            }
            for (old, new) in &mapping {
                if old != new {
                    tx.execute("DELETE FROM memory_layer_sources WHERE source_layer = 'l1' AND source_id = ?1", [old])
                        .map_err(|error| error.to_string())?;
                }
            }
            run.view.actions[index].status = "reverted".into();
            run.view.actions[index].message = Some("已撤销本组修改，原始内容以新版本恢复；旧 ID 的删除标记继续保留".into());
        }
        run.view.status = if run.view.actions.iter().any(|action| action.status == "applied") { "applied" } else { "reverted" }.into();
        run.view.updated_at = now;
        save(&tx, &run)?;
        tx.commit().map_err(|error| error.to_string())?;
        Ok(run)
    }

    pub(crate) fn refinement_source_excerpt(&self, memory: &MemoryState) -> Result<Option<String>, String> {
        let payloads: Vec<_> = memory.provenance.iter().map(|provenance| string(provenance, "payload")).collect();
        let keys = crate::memory_evidence::source::source_keys(&memory.view.source_event_key, &payloads)?;
        let mut excerpts = Vec::new();
        for key in keys.into_iter().filter(|key| !key.is_empty()).take(4) {
            if let Some(detail) = self.conversation_detail_by_event_key(&key)? {
                excerpts.push(format!("[{} · {} · {}]\n{}", detail.source, detail.session_id, detail.occurred_at,
                    detail.conversation_text.chars().take(1200).collect::<String>()));
            }
        }
        Ok((!excerpts.is_empty()).then(|| excerpts.join("\n\n").chars().take(3600).collect()))
    }

    pub(crate) fn refinement_support(&self, ids: &[String]) -> Result<HashMap<String, (HashSet<String>, HashSet<String>)>, String> {
        let conn = self.conn.lock().map_err(|_| "telemetry store lock poisoned".to_string())?;
        let mut result = HashMap::new();
        let mut statement = conn.prepare("SELECT payload FROM memory_refinement_provenance WHERE memory_id = ?1")
            .map_err(|error| error.to_string())?;
        for id in ids {
            let mut sessions = HashSet::new();
            let mut agents = HashSet::new();
            let payloads = statement.query_map([id], |row| row.get::<_, String>(0)).map_err(|error| error.to_string())?;
            for payload in payloads {
                let memory: RefinementMemory = serde_json::from_str(&payload.map_err(|error| error.to_string())?).map_err(|error| error.to_string())?;
                if !memory.session_id.is_empty() { sessions.insert(format!("{}:{}", memory.source, memory.session_id)); }
                if !memory.source.is_empty() { agents.insert(memory.source); }
            }
            if !sessions.is_empty() || !agents.is_empty() { result.insert(id.clone(), (sessions, agents)); }
        }
        Ok(result)
    }
}
