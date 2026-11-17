//! Memory-to-memory neighbours: a warm query vector needs no network or lease.
use super::*;

const CONNECT_LIMIT: Duration = Duration::from_secs(10);

pub(super) fn search(
    telemetry: &TelemetryStore, value: Value, query: (&str, &str), limit: u32,
    domains: Option<&[String]>, budget: &RecallBudget, cache: Option<&std::path::Path>,
) -> Result<crate::memory_backend::SemanticRecall, String> {
    budget.remaining()?;
    let config = Config::parse(value)?;
    let (memory_id, text) = query;
    let documents = interactive_stage("refinement_snapshot", || interactive_documents(telemetry, domains, budget))?;
    let Some(position) = documents.iter().position(|document| document.id == memory_id && document.memory == text) else {
        return Err("refinement source changed".into());
    };
    let mut recall = interactive_recall("local_fallback_cold_index", documents.len(), 0);
    let opened = match cache {
        Some(path) => open_interactive_cache_at(path.to_path_buf(), budget),
        None => open_interactive_cache(budget),
    };
    let (conn, cache_path) = match opened {
        Ok(Some(cache)) => cache,
        Ok(None) => { budget.remaining()?; return Ok(recall); }
        Err(_) => { budget.remaining()?; return Ok(crate::memory_backend::SemanticRecall::fallback("local_fallback_busy")); }
    };
    let outcome = interactive_stage("refinement_cache_read", || interactive_cached_outcome(&conn, &config, &documents, budget))?;
    recall = interactive_index_recall(&outcome, budget)?;
    if outcome.vectors.is_empty() { return Ok(recall); }
    // Signatures bind the exact current text, endpoint, model and credential
    // configuration. A same-ID edit never reuses the old document vector.
    let key = &outcome.signatures[position];
    let query_vector = if let Some(vector) = outcome.vectors.get(key) {
        eprintln!("[memory-semantic] stage=refinement_query_vector source=cache");
        vector.clone()
    } else {
        // Only an uncached anchor needs HTTP; cold peer vectors are never built
        // here. Cached requests above do not enter either arbitration table.
        if !config.is_remote() {
            let timeout = budget.remaining()?.min(Duration::from_millis(50));
            if TcpStream::connect_timeout(&runtime_address(&config)?, timeout).is_err() {
                budget.remaining()?;
                recall.mode = "local_fallback_runtime";
                return Ok(recall);
            }
        }
        let lease_conn = Connection::open_with_flags(&cache_path, rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE)
            .map_err(|_| "embedding cache unavailable".to_string())?;
        let lease = BuildLease {
            conn: lease_conn, route: lease_route(&config), legacy_route: signature(&config, ""),
            owner: uuid::Uuid::new_v4().to_string(),
        };
        let _foreground = ForegroundRequest::register_interactive(&cache_path, &config, budget)?;
        if let Err(error) = interactive_stage("refinement_lease_wait", || lease.acquire_interactive(budget)) {
            return Err(if error == "recall_cancelled" { error } else if budget.remaining().is_err() {
                "refinement lease timed out".into()
            } else { "embedding service busy".into() });
        }
        eprintln!("[memory-semantic] stage=refinement_query_vector source=remote");
        interactive_stage("refinement_query_embedding", ||
            embed_query_with_budget(&config, text, outcome.dimension, budget, CONNECT_LIMIT, true))?
    };
    drop(conn);
    budget.remaining()?;
    let current = interactive_stage("refinement_revalidate", || interactive_documents(telemetry, domains, budget))?;
    if !current.iter().any(|document| document.id == memory_id && document.memory == text) {
        return Err("refinement source changed".into());
    }
    let indexed: HashMap<_, _> = documents.iter().zip(&outcome.signatures)
        .map(|(document, key)| (document.id.as_str(), (document.memory.as_str(), key))).collect();
    let mut results = Vec::new();
    for mut document in current {
        budget.remaining()?;
        if document.id == memory_id { continue; }
        let Some(&(content, key)) = indexed.get(document.id.as_str()) else { continue };
        if document.memory != content { continue; }
        let Some(vector) = outcome.vectors.get(key) else { continue };
        document.score = Some(interactive_cosine(&query_vector, vector, budget)?);
        results.push(document);
    }
    budget.remaining()?;
    results.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal).then_with(|| a.id.cmp(&b.id)));
    results.truncate(limit.clamp(1, 100) as usize);
    budget.remaining()?;
    recall.memories = results;
    Ok(recall)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> (TelemetryStore, Config, tempfile::TempDir, std::path::PathBuf) {
        let store = TelemetryStore::in_memory();
        {
            let conn = store.conn.lock().unwrap();
            for (id, text, domain) in [("a", "Exact query memory", "work"), ("b", "Near memory", "work"),
                ("c", "Other memory", "work"), ("x", "Outside scope", "study")] {
                conn.execute("INSERT OR IGNORE INTO memory_domains(id,name,kind,created_at,updated_at)
                    VALUES (?1,?1,'custom','2026-01-01','2026-01-01')", [domain]).unwrap();
                conn.execute("INSERT INTO local_memory_items(id,source_event_key,ordinal,memory,memory_type,durability,
                    source,session_id,created_at,updated_at) VALUES (?1,?1,0,?2,'fact','short_term','test',?1,'2026-01-01','2026-01-01')",
                    params![id,text]).unwrap();
                conn.execute("INSERT INTO local_memory_domains(memory_id,domain_id,weight,source,created_at,updated_at)
                    VALUES (?1,?2,1,'manual','2026-01-01','2026-01-01')", params![id,domain]).unwrap();
            }
        }
        // This endpoint is deliberately unreachable. Warm-cache success proves
        // no runtime probe, remote query or embedding lease was needed.
        let config = Config::parse(serde_json::json!({"base_url":"http://127.0.0.1:1/v1","model":"fixture"})).unwrap();
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("cache.sqlite3");
        let conn = Connection::open(&path).unwrap();
        initialize_cache(&conn).unwrap();
        (store, config, directory, path)
    }

    fn seed(path: &std::path::Path, config: &Config, records: &[(&str, [f32; 2])]) {
        let conn = Connection::open(path).unwrap();
        for (text, vector) in records {
            let bytes: Vec<_> = vector.iter().flat_map(|value| value.to_le_bytes()).collect();
            conn.execute("INSERT OR REPLACE INTO embeddings_v1(signature,dimension,vector) VALUES (?1,2,?2)",
                params![signature(config, text),bytes]).unwrap();
        }
    }

    #[test]
    fn cached_anchor_works_offline_ignores_embedding_lease_and_stays_in_scope() {
        let (store, config, _directory, path) = fixture();
        seed(&path, &config, &[("Exact query memory",[1.0,0.0]),("Near memory",[0.9,0.1]),
            ("Other memory",[0.0,1.0]),("Outside scope",[1.0,0.0])]);
        let conn = Connection::open(&path).unwrap();
        conn.execute("INSERT INTO build_leases_v1(route,owner,expires_at) VALUES (?1,'another-build',?2)",
            params![lease_route(&config),chrono::Utc::now().timestamp()+120]).unwrap();
        let result = search(&store, serde_json::to_value(&config).unwrap(), ("a","Exact query memory"), 5,
            Some(&["work".into()]), &RecallBudget::new(Duration::from_secs(2)), Some(&path)).unwrap();
        assert_eq!(result.mode, "semantic");
        assert_eq!(result.cached, Some(3));
        assert_eq!(result.memories.len(), 2);
        assert_eq!(result.memories[0].id, "b");
        assert!(result.memories.iter().all(|memory| memory.id != "a" && memory.id != "x"));
        assert_eq!(conn.query_row("SELECT count(*) FROM foreground_requests_v1", [], |row| row.get::<_, i64>(0)).unwrap(), 0);
        assert_eq!(conn.query_row("SELECT owner FROM build_leases_v1 WHERE route=?1", [lease_route(&config)], |row| row.get::<_,String>(0)).unwrap(), "another-build");
    }

    #[test]
    fn partial_scope_uses_valid_cached_neighbours_without_a_remote_query() {
        let (store, config, _directory, path) = fixture();
        seed(&path, &config, &[("Exact query memory",[1.0,0.0]),("Near memory",[0.9,0.1])]);
        let result = search(&store, serde_json::to_value(&config).unwrap(), ("a","Exact query memory"), 5,
            Some(&["work".into()]), &RecallBudget::new(Duration::from_secs(2)), Some(&path)).unwrap();
        assert_eq!(result.mode, "semantic_partial_index");
        assert_eq!(result.pending, Some(1));
        assert_eq!(result.memories.len(), 1);
        assert_eq!(result.memories[0].id, "b");
    }

    #[test]
    fn stale_anchor_and_peer_text_never_reuse_the_old_vectors() {
        let (store, config, _directory, path) = fixture();
        seed(&path, &config, &[("Exact query memory",[1.0,0.0]),("Near memory",[0.9,0.1]),("Other memory",[0.0,1.0])]);
        store.update_local_l1_memory("b", "A changed peer").unwrap();
        let result = search(&store, serde_json::to_value(&config).unwrap(), ("a","Exact query memory"), 5,
            Some(&["work".into()]), &RecallBudget::new(Duration::from_secs(2)), Some(&path)).unwrap();
        assert!(result.memories.iter().all(|memory| memory.id != "b"));
        store.update_local_l1_memory("a", "A changed anchor").unwrap();
        let error = search(&store, serde_json::to_value(&config).unwrap(), ("a","Exact query memory"), 5,
            Some(&["work".into()]), &RecallBudget::new(Duration::from_secs(2)), Some(&path)).err().unwrap();
        assert_eq!(error, "refinement source changed");
    }

    #[test]
    fn cancelled_or_cold_requests_never_create_an_index_or_start_the_runtime() {
        let (store, config, directory, _path) = fixture();
        let missing = directory.path().join("missing.sqlite3");
        let cancelled = RecallBudget::new(Duration::from_secs(1));
        cancelled.cancel();
        let error = search(&store, Value::Null, ("a","Exact query memory"), 5, None, &cancelled, Some(&missing)).err().unwrap();
        assert_eq!(error, "recall_cancelled");
        let result = search(&store, serde_json::to_value(&config).unwrap(), ("a","Exact query memory"), 5,
            Some(&["work".into()]), &RecallBudget::new(Duration::from_secs(2)), Some(&missing)).unwrap();
        assert_eq!(result.mode, "local_fallback_cold_index");
        assert!(!missing.exists());
    }
}
