//! Stable label identities, name collision checks and durable merge redirects.
use super::*;
use std::collections::BTreeMap;

pub(super) fn initialize(conn: &Connection) -> Result<(), String> {
    conn.execute_batch("CREATE TABLE IF NOT EXISTS memory_domain_merges (
        source_id TEXT PRIMARY KEY, target_id TEXT NOT NULL, merged_at TEXT NOT NULL,
        source_json TEXT NOT NULL DEFAULT '{}', CHECK(source_id != target_id));")
        .map_err(|error| error.to_string())
}

pub(super) fn name_key(name: &str) -> String { name.trim().to_lowercase() }

pub(super) fn ensure_unique_name(conn: &Connection, id: &str, name: &str) -> Result<(), String> {
    let mut statement = conn.prepare("SELECT id,name FROM memory_domains WHERE id!=?1").map_err(|error| error.to_string())?;
    let existing = statement.query_map([id], |row| Ok((row.get::<_,String>(0)?,row.get::<_,String>(1)?)))
        .map_err(|error| error.to_string())?;
    let key = name_key(name);
    for item in existing {
        let (_, old) = item.map_err(|error| error.to_string())?;
        if name_key(&old) == key { return Err("同名标签已存在，请使用已有标签或选择不同名称".into()); }
    }
    Ok(())
}

/// A deletion without a merge redirect remains deleted. Chains are bounded and
/// checked before memberships are written, never silently recreated by name.
fn terminal(conn: &Connection, id: &str) -> Result<String, String> {
    let mut current = id.to_string();
    let mut seen = HashSet::new();
    for _ in 0..32 {
        if !seen.insert(current.clone()) { return Err("标签合并映射存在循环，已保留原数据".into()); }
        let next: Option<String> = conn.query_row("SELECT target_id FROM memory_domain_merges WHERE source_id=?1", [&current], |row| row.get(0))
            .optional().map_err(|error| error.to_string())?;
        match next { Some(next) => current=next, None => return Ok(current) }
    }
    Err("标签合并链过长，已保留原数据".into())
}

pub(super) fn explicitly_deleted(conn: &Connection, id: &str) -> Result<bool, String> {
    let id = terminal(conn,id)?;
    conn.query_row("SELECT EXISTS(SELECT 1 FROM memory_domain_tombstones WHERE domain_id=?1)", [&id], |row|row.get(0))
        .map_err(|error|error.to_string())
}

pub(super) fn resolve(conn: &Connection, id: &str) -> Result<Option<String>, String> {
    let id = terminal(conn,id)?;
    let active: bool = conn.query_row("SELECT EXISTS(SELECT 1 FROM memory_domains d WHERE d.id=?1
        AND NOT EXISTS(SELECT 1 FROM memory_domain_tombstones t WHERE t.domain_id=d.id))", [&id], |row|row.get(0))
        .map_err(|error|error.to_string())?;
    Ok(active.then_some(id))
}

pub(super) fn record_redirect(conn: &Connection, source: &str, target: &str, at: &str) -> Result<(), String> {
    if source.trim().is_empty() || target.trim().is_empty() || source == target { return Err("标签合并目标无效".into()); }
    let old: Option<String> = conn.query_row("SELECT target_id FROM memory_domain_merges WHERE source_id=?1", [source], |row| row.get(0))
        .optional().map_err(|error| error.to_string())?;
    if old.as_deref().is_some_and(|old| old != target) {
        let old = resolve(conn, old.as_deref().unwrap())?;
        let new = resolve(conn, target)?;
        if old.is_none() || old != new { return Err("标签合并目标发生冲突，未覆盖已有映射".into()); }
        return Ok(());
    }
    let mut cursor = target.to_string();
    let mut seen = HashSet::from([source.to_string()]);
    for _ in 0..32 {
        if !seen.insert(cursor.clone()) { return Err("标签合并映射存在循环".into()); }
        let next: Option<String> = conn.query_row("SELECT target_id FROM memory_domain_merges WHERE source_id=?1", [&cursor], |row| row.get(0))
            .optional().map_err(|error| error.to_string())?;
        match next { Some(next) => cursor = next, None => {
            conn.execute("INSERT OR IGNORE INTO memory_domain_merges(source_id,target_id,merged_at) VALUES (?1,?2,?3)",
                params![source,target,at]).map_err(|error| error.to_string())?;
            return Ok(());
        } }
    }
    Err("标签合并链过长".into())
}

pub(super) fn merge(conn: &Connection, source: &str, target: &str, at: &str) -> Result<usize, String> {
    let target = resolve(conn, target)?.ok_or("保留标签不存在或已删除，未迁移关联")?;
    if source == target { return Ok(0); }
    let source_metadata: Option<String> = conn.query_row(
        "SELECT json_object('id',id,'name',name,'kind',kind,'description',description,'aliases_json',aliases_json,
            'enabled',enabled,'created_at',created_at,'updated_at',updated_at) FROM memory_domains WHERE id=?1", [source], |row| row.get(0),
    ).optional().map_err(|error| error.to_string())?;
    let affected: Vec<String> = {
        let mut statement = conn.prepare("SELECT DISTINCT memory_id FROM local_memory_domains WHERE domain_id=?1").map_err(|error| error.to_string())?;
        let rows = statement.query_map([source], |row| row.get(0)).map_err(|error| error.to_string())?;
        rows.collect::<Result<Vec<_>, _>>().map_err(|error| error.to_string())?
    };
    record_redirect(conn, source, &target, at)?;
    if let Some(metadata) = source_metadata {
        let links: String = conn.query_row("SELECT COALESCE(json_group_array(json_object('memory_id',memory_id,
            'domain_id',domain_id,'weight',weight,'source',source,'created_at',created_at,'updated_at',updated_at)),'[]')
            FROM local_memory_domains WHERE domain_id=?1", [source], |row| row.get(0)).map_err(|error| error.to_string())?;
        let backup = serde_json::json!({"domain":serde_json::from_str::<Value>(&metadata).map_err(|error| error.to_string())?,
            "memberships":serde_json::from_str::<Value>(&links).map_err(|error| error.to_string())?}).to_string();
        conn.execute("UPDATE memory_domain_merges SET source_json=?2 WHERE source_id=?1 AND source_json='{}'",
            params![source,backup]).map_err(|error| error.to_string())?;
    }
    conn.execute("INSERT INTO local_memory_domains(memory_id,domain_id,weight,source,created_at,updated_at)
        SELECT memory_id,?2,weight,source,created_at,?3 FROM local_memory_domains WHERE domain_id=?1
        ON CONFLICT(memory_id,domain_id,source) DO UPDATE SET weight=MAX(weight,excluded.weight),updated_at=excluded.updated_at",
        params![source,target,at]).map_err(|error| error.to_string())?;
    conn.execute("DELETE FROM local_memory_domains WHERE domain_id=?1", [source]).map_err(|error| error.to_string())?;
    for id in &affected {
        // Publish the final label set even for a locally owned memory: a peer
        // may own a same-ID local row and ignore mirror content replacements.
        let content: Option<(String,String,String)> = conn.query_row("SELECT memory,memory_type,durability FROM local_memory_items WHERE id=?1", [id],
            |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?))).optional().map_err(|error| error.to_string())?;
        if let Some((memory,kind,durability)) = content {
            let hash = memory_domain_content_hash(&memory,&kind,&durability);
            conn.execute("INSERT INTO memory_domain_classification_state(memory_id,content_hash,classified_at) VALUES (?1,?2,?3)
                ON CONFLICT(memory_id) DO UPDATE SET content_hash=excluded.content_hash,classified_at=excluded.classified_at",
                params![id,hash,at]).map_err(|error| error.to_string())?;
            conn.execute("INSERT INTO cloud_domain_retags(memory_id,classified_at) VALUES (?1,?2)
                ON CONFLICT(memory_id) DO UPDATE SET classified_at=excluded.classified_at", params![id,at]).map_err(|error| error.to_string())?;
        }
        conn.execute("DELETE FROM memory_refinement_reviews WHERE memory_id=?1", [id]).map_err(|error| error.to_string())?;
    }
    conn.execute("INSERT INTO memory_domain_tombstones(domain_id,deleted_at) VALUES (?1,?2)
        ON CONFLICT(domain_id) DO UPDATE SET deleted_at=CASE WHEN julianday(excluded.deleted_at)>julianday(deleted_at)
            THEN excluded.deleted_at ELSE deleted_at END", params![source,at]).map_err(|error| error.to_string())?;
    conn.execute("DELETE FROM memory_domains WHERE id=?1", [source]).map_err(|error| error.to_string())?;
    if !affected.is_empty() {
        conn.execute("UPDATE memory_domains SET updated_at=CASE WHEN julianday(?2)>julianday(updated_at)
            THEN ?2 ELSE updated_at END WHERE id=?1", params![target,at]).map_err(|error| error.to_string())?;
    }
    Ok(affected.len())
}

/// Deterministic winner: built-in identity first, then stable ID. All callers
/// hold a write transaction, so membership migration and retirement are atomic.
pub(super) fn merge_duplicate_names(conn: &Connection) -> Result<(), String> {
    let groups: BTreeMap<String,Vec<String>> = {
        let mut statement = conn.prepare("SELECT id,name FROM memory_domains d WHERE NOT EXISTS(SELECT 1 FROM memory_domain_tombstones t WHERE t.domain_id=d.id)
            ORDER BY CASE WHEN id IN ('general','work','study','job_search','life','unclassified') THEN 0
                WHEN kind='builtin' THEN 1 ELSE 2 END,id")
            .map_err(|error| error.to_string())?;
        let mut groups = BTreeMap::<String,Vec<String>>::new();
        for row in statement.query_map([], |row| Ok((row.get::<_,String>(0)?,row.get::<_,String>(1)?))).map_err(|error| error.to_string())? {
            let (id,name) = row.map_err(|error| error.to_string())?;
            groups.entry(name_key(&name)).or_default().push(id);
        }
        groups
    };
    let now = chrono::Utc::now().to_rfc3339();
    for ids in groups.values().filter(|ids| ids.len()>1) {
        for source in &ids[1..] { merge(conn,source,&ids[0],&now)?; }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn old_work(conn: &Connection) {
        conn.execute("INSERT INTO memory_domains(id,name,kind,description,created_at,updated_at)
            VALUES ('custom:old-work','工作','custom','Legacy work label','2026-11-01T00:00:00Z','2026-11-01T00:00:00Z')", []).unwrap();
    }

    fn memory(conn: &Connection, id: &str, domain: &str, weight: f64) {
        conn.execute("INSERT OR IGNORE INTO local_memory_items(id,source_event_key,ordinal,memory,memory_type,durability,
            source,session_id,created_at,updated_at) VALUES (?1,?1,0,?1,'fact','short_term','test',?1,'2026-11-01','2026-11-01')", [id]).unwrap();
        conn.execute("INSERT INTO local_memory_domains(memory_id,domain_id,weight,source,created_at,updated_at)
            VALUES (?1,?2,?3,'backfill','2026-11-01','2026-11-01')", params![id,domain,weight]).unwrap();
    }

    #[test]
    fn local_create_and_rename_cannot_collide_with_normalized_names() {
        let store = TelemetryStore::in_memory();
        assert!(store.save_memory_domain(None," 工作 ","",&[],true).is_err());
        let custom = store.save_memory_domain(None,"Project","",&[],true).unwrap();
        assert!(store.save_memory_domain(None," project ","",&[],true).is_err());
        assert!(store.save_memory_domain(Some(&custom.id),"工作","",&[],true).is_err());
        assert!(store.memory_domains().unwrap().iter().any(|domain| domain.id==custom.id && domain.name=="Project"));
        assert!(store.save_memory_domain(Some("work"),"工作","Updated description",&[],true).is_ok());
    }

    #[test]
    fn duplicate_merge_keeps_union_weights_source_backup_and_sync_intent() {
        let store = TelemetryStore::in_memory();
        {
            let mut conn = store.conn.lock().unwrap();
            let tx = conn.transaction().unwrap();
            old_work(&tx);
            memory(&tx,"a","custom:old-work",0.9);
            memory(&tx,"b","custom:old-work",0.7);
            memory(&tx,"b","work",0.8);
            memory(&tx,"c","work",0.6);
            merge_duplicate_names(&tx).unwrap();
            tx.commit().unwrap();
        }
        assert_eq!(store.local_l1_memory_snapshot().unwrap().len(),3);
        assert_eq!(store.memory_ids_for_domains(&["work".into()]).unwrap().len(),3);
        let conn = store.conn.lock().unwrap();
        assert_eq!(resolve(&conn,"custom:old-work").unwrap().as_deref(),Some("work"));
        assert_eq!(conn.query_row("SELECT weight FROM local_memory_domains WHERE memory_id='b' AND domain_id='work'",[],|row|row.get::<_,f64>(0)).unwrap(),0.8);
        let backup: String = conn.query_row("SELECT source_json FROM memory_domain_merges WHERE source_id='custom:old-work'",[],|row|row.get(0)).unwrap();
        let backup: Value = serde_json::from_str(&backup).unwrap();
        assert_eq!(backup["domain"]["description"],"Legacy work label");
        assert_eq!(backup["memberships"].as_array().unwrap().len(),2);
        drop(conn);
        let catalog = store.cloud_domain_catalog().unwrap();
        assert!(catalog.tombstones.iter().any(|item| item.domain_id=="custom:old-work" && item.merged_into.as_deref()==Some("work")));
        assert!(store.cloud_tag_overrides().unwrap().iter().any(|item| item.memory_id=="a" && item.domains.iter().any(|link| link.domain.id=="work")));
    }

    #[test]
    fn cloud_merge_moves_peer_memberships_before_deleting_old_label_and_maps_late_overrides() {
        let source = TelemetryStore::in_memory();
        let peer = TelemetryStore::in_memory();
        for store in [&source,&peer] {
            let conn = store.conn.lock().unwrap();
            old_work(&conn);
            memory(&conn,"peer-only","custom:old-work",0.9);
        }
        {
            let mut conn=source.conn.lock().unwrap();
            let tx=conn.transaction().unwrap();
            merge_duplicate_names(&tx).unwrap(); tx.commit().unwrap();
        }
        peer.merge_cloud_domain_catalog(&source.cloud_domain_catalog().unwrap()).unwrap();
        assert!(peer.memory_ids_for_domains(&["work".into()]).unwrap().contains("peer-only"));
        let old = CloudMemoryDomain { id:"custom:old-work".into(),name:"工作".into(),kind:"custom".into(),
            description:"Old offline copy".into(),aliases:vec![],enabled:true,created_at:"2026-11-01T00:00:00Z".into(),updated_at:"2026-11-01T00:00:00Z".into() };
        peer.merge_cloud_domain_catalog(&CloudDomainCatalog { domains:vec![old.clone()],tombstones:vec![] }).unwrap();
        peer.apply_peer_tag_overrides(&[CloudTagOverride { memory_id:"peer-only".into(),
            classified_at:"2027-01-01T00:00:00Z".into(),domains:vec![CloudMemoryDomainLink { domain:old,weight:0.9,source:"backfill".into() }] }]).unwrap();
        assert!(peer.memory_ids_for_domains(&["work".into()]).unwrap().contains("peer-only"));
        assert!(!peer.memory_domains().unwrap().iter().any(|domain| domain.id=="custom:old-work"));
        peer.delete_memory_domain("work").unwrap();
        assert!(resolve(&peer.conn.lock().unwrap(),"custom:old-work").unwrap().is_none());
    }

    #[test]
    fn v5_upgrade_merges_existing_duplicates_without_touching_memory_content() {
        let conn = schema::initialize_database(Connection::open_in_memory().unwrap()).unwrap();
        old_work(&conn); memory(&conn,"retained","custom:old-work",1.0);
        conn.execute_batch("DROP TABLE memory_domain_merges; PRAGMA user_version=5;").unwrap();
        let conn = schema::initialize_database(conn).unwrap();
        assert_eq!(conn.pragma_query_value(None,"user_version",|row|row.get::<_,i64>(0)).unwrap(),6);
        assert_eq!(resolve(&conn,"custom:old-work").unwrap().as_deref(),Some("work"));
        assert_eq!(conn.query_row("SELECT memory FROM local_memory_items WHERE id='retained'",[],|row|row.get::<_,String>(0)).unwrap(),"retained");
    }

    #[test]
    fn a_missing_transitive_merge_target_does_not_discard_existing_memberships() {
        let store = TelemetryStore::in_memory();
        { let conn=store.conn.lock().unwrap(); old_work(&conn); memory(&conn,"retained","custom:old-work",1.0); }
        let at="2026-11-15T00:00:00Z".to_string();
        let catalog=CloudDomainCatalog { domains:vec![],tombstones:vec![
            CloudDomainTombstone { domain_id:"custom:old-work".into(),deleted_at:at.clone(),merged_into:Some("custom:middle".into()) },
            CloudDomainTombstone { domain_id:"custom:middle".into(),deleted_at:at,merged_into:Some("custom:not-arrived".into()) },
        ] };
        assert!(store.merge_cloud_domain_catalog(&catalog).is_err());
        assert!(store.memory_ids_for_domains(&["custom:old-work".into()]).unwrap().contains("retained"));
        assert!(store.cloud_domain_catalog().unwrap().tombstones.is_empty());
    }

    #[test]
    fn malformed_cloud_redirects_roll_back_the_entire_catalog() {
        let store = TelemetryStore::in_memory();
        let at="2026-11-15T00:00:00Z".to_string();
        let catalog=CloudDomainCatalog { domains:vec![],tombstones:vec![
            CloudDomainTombstone { domain_id:"custom:a".into(),deleted_at:at.clone(),merged_into:Some("custom:b".into()) },
            CloudDomainTombstone { domain_id:"custom:b".into(),deleted_at:at,merged_into:Some("custom:a".into()) },
        ] };
        assert!(store.merge_cloud_domain_catalog(&catalog).is_err());
        assert!(store.cloud_domain_catalog().unwrap().tombstones.is_empty());
    }
}
