use super::*;

#[cfg(test)]
#[path = "cloud_domains_tests.rs"]
mod tests;

fn read_domain_links(
    conn: &Connection,
    memory_id: Option<&str>,
) -> Result<HashMap<String, Vec<CloudMemoryDomainLink>>, String> {
    let mut statement = conn.prepare(
        "SELECT md.memory_id, d.id, d.name, d.kind, d.description, d.aliases_json,
                d.enabled, d.created_at, d.updated_at, md.weight, md.source
           FROM local_memory_domains md
           JOIN memory_domains d ON d.id = md.domain_id
          WHERE (?1 IS NULL OR md.memory_id = ?1)
          ORDER BY md.memory_id, d.id, md.source",
    ).map_err(|error| error.to_string())?;
    let rows = statement.query_map([memory_id], |row| {
        let aliases: String = row.get(5)?;
        Ok((row.get::<_, String>(0)?, CloudMemoryDomainLink {
            domain: CloudMemoryDomain {
                id: row.get(1)?, name: row.get(2)?, kind: row.get(3)?,
                description: row.get(4)?,
                aliases: serde_json::from_str(&aliases).unwrap_or_default(),
                enabled: row.get::<_, i64>(6)? != 0,
                created_at: row.get(7)?, updated_at: row.get(8)?,
            },
            weight: row.get(9)?, source: row.get(10)?,
        }))
    }).map_err(|error| error.to_string())?;
    let mut links: HashMap<String, Vec<CloudMemoryDomainLink>> = HashMap::new();
    for row in rows {
        let (id, link) = row.map_err(|error| error.to_string())?;
        links.entry(id).or_default().push(link);
    }
    Ok(links)
}

pub(super) fn with_cloud_domains(
    conn: &Connection,
    items: Vec<LocalMemorySnapshot>,
) -> Result<Vec<CloudL1Memory>, String> {
    let mut links = read_domain_links(conn, None)?;
    let mut checkpoints: HashMap<String, String> = HashMap::new();
    let mut statement = conn.prepare(
        "SELECT memory_id, classified_at FROM memory_domain_classification_state",
    ).map_err(|error| error.to_string())?;
    let rows = statement.query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)))
        .map_err(|error| error.to_string())?;
    for row in rows {
        let (id, at) = row.map_err(|error| error.to_string())?;
        checkpoints.insert(id, at);
    }
    Ok(items.into_iter().map(|item| {
        let domains = Some(links.remove(&item.id).unwrap_or_default());
        let classified_at = checkpoints.remove(&item.id);
        CloudL1Memory { item, domains, classified_at }
    }).collect())
}

pub(super) fn domain_timestamp(value: &str) -> Result<chrono::DateTime<chrono::Utc>, String> {
    if let Ok(time) = chrono::DateTime::parse_from_rfc3339(value) {
        return Ok(time.with_timezone(&chrono::Utc));
    }
    // Builtin seeds use SQLite datetime('now'); user edits use RFC3339 with subsecond precision.
    chrono::NaiveDateTime::parse_from_str(value, "%Y-%m-%d %H:%M:%S%.f")
        .map(|time| time.and_utc())
        .map_err(|_| "云端领域标签的时间戳无效".to_string())
}

/// Definition updates converge by timestamp, with a deterministic tie-break for equal timestamps.
/// Deletion is permanent for an ID; creating a new label gives it a new UUID.
fn upsert_cloud_domain(transaction: &Transaction, domain: &CloudMemoryDomain) -> Result<bool, String> {
    if domain.id.trim().is_empty() || domain.name.trim().is_empty()
        || !matches!(domain.kind.as_str(), "builtin" | "project" | "custom")
    {
        return Err("云端领域标签无效".into());
    }
    domain_timestamp(&domain.created_at)?;
    let incoming_time = domain_timestamp(&domain.updated_at)?;
    let deleted: bool = transaction.query_row(
        "SELECT EXISTS(SELECT 1 FROM memory_domain_tombstones WHERE domain_id = ?1)",
        [&domain.id], |row| row.get(0),
    ).map_err(|error| error.to_string())?;
    if deleted { return Ok(false) }
    let aliases = serde_json::to_string(&domain.aliases).map_err(|error| error.to_string())?;
    let existing: Option<(String, String, String, String, bool, String, String)> = transaction.query_row(
        "SELECT name, kind, description, aliases_json, enabled, created_at, updated_at
           FROM memory_domains WHERE id = ?1",
        [&domain.id],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?, row.get(6)?)),
    ).optional().map_err(|error| error.to_string())?;
    if let Some((name, kind, description, old_aliases, enabled, created_at, updated_at)) = existing {
        let old_time = domain_timestamp(&updated_at)?;
        let incoming_order = (&domain.name, &domain.kind, &domain.description, &aliases,
            domain.enabled, &domain.created_at, &domain.updated_at);
        let old_order = (&name, &kind, &description, &old_aliases, enabled, &created_at, &updated_at);
        if incoming_time < old_time || (incoming_time == old_time && incoming_order <= old_order) {
            return Ok(true);
        }
    }
    transaction.execute(
        "INSERT INTO memory_domains
            (id, name, kind, description, aliases_json, enabled, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
         ON CONFLICT(id) DO UPDATE SET
            name = excluded.name, kind = excluded.kind, description = excluded.description,
            aliases_json = excluded.aliases_json, enabled = excluded.enabled,
            created_at = excluded.created_at, updated_at = excluded.updated_at",
        params![domain.id, domain.name, domain.kind, domain.description, aliases,
            domain.enabled, domain.created_at, domain.updated_at],
    ).map_err(|error| error.to_string())?;
    Ok(true)
}

/// Missing tags are legacy data; an explicit empty list removes old assignments.
/// Memory and label writes share a transaction. Stale payloads cannot resurrect deleted labels.
pub(super) fn apply_cloud_domains(
    transaction: &Transaction,
    memory_id: &str,
    domains: Option<&[CloudMemoryDomainLink]>,
    memory: &str,
    memory_type: &str,
    durability: &str,
) -> Result<(), String> {
    let Some(domains) = domains else { return Ok(()) };
    let now = chrono::Utc::now().to_rfc3339();
    let mut active = Vec::new();
    for link in domains {
        if !matches!(link.source.as_str(), "auto" | "manual" | "backfill")
            || !link.weight.is_finite() || !(0.0..=1.0).contains(&link.weight)
        {
            return Err("云端记忆的领域标签无效".into());
        }
        if upsert_cloud_domain(transaction, &link.domain)? { active.push(link); }
    }
    transaction.execute("DELETE FROM local_memory_domains WHERE memory_id = ?1", [memory_id])
        .map_err(|error| error.to_string())?;
    for link in &active {
        transaction.execute(
            "INSERT INTO local_memory_domains
                (memory_id, domain_id, weight, source, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?5)
             ON CONFLICT(memory_id, domain_id, source) DO UPDATE SET
                weight = excluded.weight, updated_at = excluded.updated_at",
            params![memory_id, link.domain.id, link.weight, link.source, now],
        ).map_err(|error| error.to_string())?;
    }
    if !active.iter().any(|link| link.source == "backfill") {
        // Extraction-time auto labels are not a completed classification decision.
        transaction.execute("DELETE FROM memory_domain_classification_state WHERE memory_id = ?1", [memory_id])
            .map_err(|error| error.to_string())?;
    } else {
        let hash = memory_domain_content_hash(memory, memory_type, durability);
        transaction.execute(
            "INSERT INTO memory_domain_classification_state (memory_id, content_hash, classified_at)
             VALUES (?1, ?2, ?3)
             ON CONFLICT(memory_id) DO UPDATE SET
                content_hash = excluded.content_hash, classified_at = excluded.classified_at",
            params![memory_id, hash, now],
        ).map_err(|error| error.to_string())?;
    }
    Ok(())
}

/// Restore the source checkpoint even for a completed decision with no active tags.
pub(super) fn apply_cloud_l1_domains(
    transaction: &Transaction,
    row: &CloudL1Memory,
) -> Result<(), String> {
    apply_cloud_domains(
        transaction, &row.id, row.domains.as_deref(),
        &row.memory, &row.memory_type, &row.durability,
    )?;
    if let Some(at) = row.classified_at.as_deref() {
        domain_timestamp(at)?;
        let hash = memory_domain_content_hash(&row.memory, &row.memory_type, &row.durability);
        transaction.execute(
            "INSERT INTO memory_domain_classification_state (memory_id, content_hash, classified_at)
             VALUES (?1, ?2, ?3)
             ON CONFLICT(memory_id) DO UPDATE SET
                content_hash = excluded.content_hash, classified_at = excluded.classified_at",
            params![row.id, hash, at],
        ).map_err(|error| error.to_string())?;
    }
    Ok(())
}

impl TelemetryStore {
    /// 本机显式用户状态（置顶/召回）：按设备发布，跨设备合并。
    pub fn cloud_user_state(&self) -> Result<Vec<CloudUserStateEntry>, String> {
        let conn = self.conn.lock().map_err(|_| "telemetry store lock poisoned".to_string())?;
        let mut statement = conn.prepare(
            "SELECT memory_id, pinned, pinned_updated_at, recall_count, recall_updated_at
               FROM memory_importance
              WHERE recall_count > 0 OR pinned_updated_at IS NOT NULL
              ORDER BY memory_id",
        ).map_err(|error| error.to_string())?;
        let rows = statement.query_map([], |row| Ok(CloudUserStateEntry {
            memory_id: row.get(0)?,
            pinned: row.get::<_, i64>(1)? != 0,
            pinned_updated_at: row.get(2)?,
            recall_count: row.get(3)?,
            recall_updated_at: row.get(4)?,
        })).map_err(|error| error.to_string())?;
        rows.collect::<Result<Vec<_>, _>>().map_err(|error| error.to_string())
    }

    /// 对端用户状态整表替换：只影响该设备的贡献，本机决策留在 memory_importance。
    pub fn apply_peer_user_state(&self, device: &str, entries: &[CloudUserStateEntry]) -> Result<(), String> {
        let mut conn = self.conn.lock().map_err(|_| "telemetry store lock poisoned".to_string())?;
        let transaction = conn.transaction().map_err(|error| error.to_string())?;
        transaction.execute("DELETE FROM memory_user_state_peer WHERE device = ?1", [device])
            .map_err(|error| error.to_string())?;
        for entry in entries {
            if entry.memory_id.trim().is_empty() || entry.recall_count < 0 {
                return Err("云端用户状态无效".into());
            }
            transaction.execute(
                "INSERT INTO memory_user_state_peer
                    (device, memory_id, pinned, pinned_updated_at, recall_count, recall_updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![device, entry.memory_id, entry.pinned as i64, entry.pinned_updated_at,
                    entry.recall_count, entry.recall_updated_at],
            ).map_err(|error| error.to_string())?;
        }
        transaction.commit().map_err(|error| error.to_string())
    }

    /// 接收端对镜像记忆的显式重分类，发布给来源端与其他设备收敛。
    pub fn cloud_tag_overrides(&self) -> Result<Vec<CloudTagOverride>, String> {
        let conn = self.conn.lock().map_err(|_| "telemetry store lock poisoned".to_string())?;
        let mut statement = conn.prepare(
            "SELECT r.memory_id, r.classified_at FROM cloud_domain_retags r ORDER BY r.memory_id",
        ).map_err(|error| error.to_string())?;
        let rows = statement.query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)))
            .map_err(|error| error.to_string())?;
        let mut overrides = Vec::new();
        for row in rows {
            let (memory_id, classified_at) = row.map_err(|error| error.to_string())?;
            let links = read_domain_links(&conn, Some(&memory_id))?
                .remove(&memory_id)
                .unwrap_or_default();
            overrides.push(CloudTagOverride { memory_id, domains: links, classified_at });
        }
        Ok(overrides)
    }

    /// 应用对端重分类：检查点较新者胜；镜像与本机记录都可被更新的分类收敛。
    pub fn apply_peer_tag_overrides(&self, overrides: &[CloudTagOverride]) -> Result<(), String> {
        let mut conn = self.conn.lock().map_err(|_| "telemetry store lock poisoned".to_string())?;
        let transaction = conn.transaction().map_err(|error| error.to_string())?;
        for override_item in overrides {
            if override_item.memory_id.trim().is_empty()
                || domain_timestamp(&override_item.classified_at).is_err()
            {
                return Err("云端重分类记录无效".into());
            }
            // 检查点绑定当前正文：正文再变时该分类会重新排队，不会被旧标签钉住。
            let local: Option<(String, String, String)> = transaction.query_row(
                "SELECT memory, memory_type, durability FROM local_memory_items WHERE id = ?1",
                [&override_item.memory_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            ).optional().map_err(|error| error.to_string())?;
            let Some((memory, memory_type, durability)) = local else { continue };
            let local_at = transaction.query_row(
                "SELECT classified_at FROM memory_domain_classification_state WHERE memory_id = ?1",
                [&override_item.memory_id], |row| row.get::<_, String>(0),
            ).optional().map_err(|error| error.to_string())?;
            if let Some(local_at) = local_at {
                if domain_timestamp(&local_at)? >= domain_timestamp(&override_item.classified_at)? {
                    continue;
                }
            }
            let assignments: Vec<MemoryDomainAssignment> = override_item.domains.iter().map(|link| {
                MemoryDomainAssignment { domain_id: link.domain.id.clone(), weight: link.weight }
            }).collect();
            for link in &override_item.domains {
                upsert_cloud_domain(&transaction, &link.domain)?;
            }
            transaction.execute(
                "DELETE FROM local_memory_domains WHERE memory_id = ?1 AND source IN ('auto', 'backfill')",
                [&override_item.memory_id],
            ).map_err(|error| error.to_string())?;
            insert_memory_domain_assignments(&transaction, &override_item.memory_id, &assignments, "backfill", &override_item.classified_at)
                .map_err(|error| error.to_string())?;
            let content_hash = memory_domain_content_hash(&memory, &memory_type, &durability);
            transaction.execute(
                "INSERT INTO memory_domain_classification_state (memory_id, content_hash, classified_at)
                 VALUES (?1, ?2, ?3)
                 ON CONFLICT(memory_id) DO UPDATE SET
                    content_hash = excluded.content_hash, classified_at = excluded.classified_at",
                params![override_item.memory_id, content_hash, override_item.classified_at],
            ).map_err(|error| error.to_string())?;
            transaction.execute(
                "INSERT INTO cloud_domain_retags (memory_id, classified_at) VALUES (?1, ?2)
                 ON CONFLICT(memory_id) DO UPDATE SET classified_at = excluded.classified_at
                 WHERE julianday(excluded.classified_at) > julianday(cloud_domain_retags.classified_at)",
                params![override_item.memory_id, override_item.classified_at],
            ).map_err(|error| error.to_string())?;
        }
        transaction.commit().map_err(|error| error.to_string())
    }

    pub fn cloud_classification_checkpoint(&self, memory_id: &str) -> Result<Option<String>, String> {
        let conn = self.conn.lock().map_err(|_| "telemetry store lock poisoned".to_string())?;
        conn.query_row(
            "SELECT classified_at FROM memory_domain_classification_state WHERE memory_id = ?1",
            [memory_id], |row| row.get::<_, String>(0),
        ).optional().map_err(|error| error.to_string())
    }

    /// 已发布文档的来源证据：随正文一起跨设备，避免接收端显示错误来源。
    pub fn cloud_layer_provenance(&self, layer: &str) -> Result<Option<CloudLayerProvenance>, String> {
        let conn = self.conn.lock().map_err(|_| "telemetry store lock poisoned".to_string())?;
        let doc = conn.query_row(
            "SELECT source_count, window_start, window_end, created_at, published_at
               FROM memory_layer_documents
              WHERE layer = ?1 AND state = 'published'
              ORDER BY published_at DESC, created_at DESC LIMIT 1",
            [layer],
            |row| Ok(CloudLayerProvenance {
                source_count: row.get(0)?, window_start: row.get(1)?, window_end: row.get(2)?,
                created_at: row.get(3)?, published_at: row.get(4)?, sources: Vec::new(),
            }),
        ).optional().map_err(|error| error.to_string())?;
        let Some(mut provenance) = doc else { return Ok(None) };
        let mut statement = conn.prepare(
            "SELECT s.source_layer, s.source_id FROM memory_layer_sources s
               JOIN memory_layer_documents d ON d.id = s.document_id
              WHERE d.layer = ?1 AND d.state = 'published'
              ORDER BY s.source_layer, s.source_id",
        ).map_err(|error| error.to_string())?;
        let rows = statement.query_map([layer], |row| Ok(CloudLayerSourceRef {
            layer: row.get(0)?, id: row.get(1)?,
        })).map_err(|error| error.to_string())?;
        provenance.sources = rows.collect::<Result<Vec<_>, _>>().map_err(|error| error.to_string())?;
        Ok(Some(provenance))
    }

    pub fn cloud_user_defined_l1_memories(&self) -> Result<Vec<CloudL1Memory>, String> {
        let items = self.user_defined_l1_memories()?;
        let conn = self.conn.lock().map_err(|_| "telemetry store lock poisoned".to_string())?;
        with_cloud_domains(&conn, items)
    }

    pub fn cloud_l1_domains(&self, id: &str) -> Result<Vec<CloudMemoryDomainLink>, String> {
        let conn = self.conn.lock().map_err(|_| "telemetry store lock poisoned".to_string())?;
        Ok(read_domain_links(&conn, Some(id))?.remove(id).unwrap_or_default())
    }

    /// Independent of memory membership, so unused and disabled custom labels are included.
    pub fn cloud_domain_catalog(&self) -> Result<CloudDomainCatalog, String> {
        let conn = self.conn.lock().map_err(|_| "telemetry store lock poisoned".to_string())?;
        let mut statement = conn.prepare(
            "SELECT id, name, kind, description, aliases_json, enabled, created_at, updated_at
               FROM memory_domains ORDER BY id",
        ).map_err(|error| error.to_string())?;
        let rows = statement.query_map([], |row| {
            let aliases: String = row.get(4)?;
            Ok(CloudMemoryDomain {
                id: row.get(0)?, name: row.get(1)?, kind: row.get(2)?, description: row.get(3)?,
                aliases: serde_json::from_str(&aliases).unwrap_or_default(),
                enabled: row.get::<_, i64>(5)? != 0, created_at: row.get(6)?, updated_at: row.get(7)?,
            })
        }).map_err(|error| error.to_string())?;
        let domains = rows.collect::<Result<Vec<_>, _>>().map_err(|error| error.to_string())?;
        let mut statement = conn.prepare(
            "SELECT domain_id, deleted_at FROM memory_domain_tombstones ORDER BY domain_id",
        ).map_err(|error| error.to_string())?;
        let rows = statement.query_map([], |row| Ok(CloudDomainTombstone {
            domain_id: row.get(0)?, deleted_at: row.get(1)?,
        })).map_err(|error| error.to_string())?;
        let tombstones = rows.collect::<Result<Vec<_>, _>>().map_err(|error| error.to_string())?;
        Ok(CloudDomainCatalog { domains, tombstones })
    }

    pub fn merge_cloud_domain_catalog(&self, catalog: &CloudDomainCatalog) -> Result<(), String> {
        let mut conn = self.conn.lock().map_err(|_| "telemetry store lock poisoned".to_string())?;
        let transaction = conn.transaction().map_err(|error| error.to_string())?;
        // Apply deletion first: older memory snapshots may still carry the removed definition.
        for tombstone in &catalog.tombstones {
            let valid_date: bool = transaction.query_row(
                "SELECT julianday(?1) IS NOT NULL", [&tombstone.deleted_at], |row| row.get(0),
            ).map_err(|error| error.to_string())?;
            if tombstone.domain_id.trim().is_empty() || !valid_date {
                return Err("云端领域标签的删除标记无效".into());
            }
            transaction.execute(
                "INSERT INTO memory_domain_tombstones (domain_id, deleted_at) VALUES (?1, ?2)
                 ON CONFLICT(domain_id) DO UPDATE SET deleted_at = excluded.deleted_at
                 WHERE julianday(excluded.deleted_at) > julianday(memory_domain_tombstones.deleted_at)
                    OR (julianday(excluded.deleted_at) = julianday(memory_domain_tombstones.deleted_at)
                        AND excluded.deleted_at > memory_domain_tombstones.deleted_at)",
                params![tombstone.domain_id, tombstone.deleted_at],
            ).map_err(|error| error.to_string())?;
            transaction.execute("DELETE FROM memory_domains WHERE id = ?1", [&tombstone.domain_id])
                .map_err(|error| error.to_string())?;
        }
        for domain in &catalog.domains { upsert_cloud_domain(&transaction, domain)?; }
        transaction.commit().map_err(|error| error.to_string())
    }
}
