use super::*;

fn memory(id: &str, content: &str) -> LocalMemorySnapshot {
    LocalMemorySnapshot {
        id: id.into(), source_event_key: "session:test".into(), ordinal: 0,
        memory: content.into(), memory_type: "fact".into(), durability: "short_term".into(),
        source: "test".into(), session_id: "test".into(), user_defined: false,
        event_time: None, created_at: "2026-01-01T00:00:00Z".into(), updated_at: "2026-01-01T00:00:00Z".into(),
    }
}
fn semantic(id: &str, content: &str) -> SemanticMemory {
    SemanticMemory { id: id.into(), memory: content.into(), memory_type: "fact".into(), score: Some(0.8) }
}
fn keyword(id: &str, content: &str) -> LocalMemory {
    LocalMemory { id: id.into(), memory: content.into(), memory_type: "fact".into(), durability: "short_term".into(),
        user_defined: false, last_update_at: "2026-01-01T00:00:00Z".into(), event_time: None, score: Some(1.0) }
}

#[test]
fn routing_budget_reserves_two_thirds_without_a_one_second_ceiling() {
    assert_eq!(routing_budget(12_000, Duration::from_secs(12)), Duration::from_secs(4));
    assert_eq!(routing_budget(6_000, Duration::from_secs(6)), Duration::from_secs(2));
    assert_eq!(routing_budget(30_000, Duration::from_secs(30)), Duration::from_secs(10));
}

#[test]
fn routing_budget_respects_short_and_exhausted_request_deadlines() {
    assert_eq!(routing_budget(1_500, Duration::from_millis(1_500)), Duration::from_millis(500));
    assert_eq!(routing_budget(12_000, Duration::from_millis(200)), Duration::from_millis(200));
    assert_eq!(routing_budget(12_000, Duration::ZERO), Duration::ZERO);
    assert_eq!(routing_budget(0, Duration::ZERO), Duration::ZERO);
}

#[test]
fn a_joint_hit_beyond_the_final_limit_participates_in_fusion() {
    let ledger: Vec<_> = (0..8).map(|i| memory(&format!("m{i}"), &format!("独立事实 {i}"))).collect();
    let semantic: Vec<_> = (0..7).map(|i| semantic(&ledger[i].id, &ledger[i].memory)).collect();
    let local: Vec<_> = [7, 6].into_iter().map(|i| keyword(&ledger[i].id, &ledger[i].memory)).collect();
    let result = select_candidates(semantic, local, &ledger, &HashMap::new(), None, 1);
    assert_eq!(result.memories[0]["id"], "m6");
    assert_eq!(result.memories[0]["source"], "semantic+keyword");
}

#[test]
fn a_top_keyword_hit_beats_a_weak_semantic_neighbour() {
    let mut ledger: Vec<_> = (0..10).map(|i| memory(&format!("s{i}"), &format!("语义候选 {i}"))).collect();
    ledger.push(memory("keyword", "准确关键词证据"));
    let semantic = ledger[..10].iter().map(|m| semantic(&m.id, &m.memory)).collect();
    let result = select_candidates(semantic, vec![keyword("keyword", "准确关键词证据")], &ledger, &HashMap::new(), None, 12);
    let keyword_rank = result.memories.iter().position(|row| row["id"] == "keyword").unwrap();
    let weak_rank = result.memories.iter().position(|row| row["id"] == "s9").unwrap();
    assert!(keyword_rank < weak_rank);
}

#[test]
fn repeated_rows_vote_once_and_do_not_claim_a_second_route() {
    let ledger = vec![memory("a", "同一事实")];
    let result = select_candidates(vec![semantic("a", "同一事实"); 8], Vec::new(), &ledger, &HashMap::new(), None, 6);
    assert_eq!(result.memories.len(), 1);
    assert_eq!(result.memories[0]["source"], "semantic");
    assert_eq!(result.memories[0]["fused_score"], 0.5);
    assert_eq!(result.discarded_candidates, 7);
}

#[test]
fn stale_content_and_deleted_identities_cannot_return() {
    let ledger = vec![memory("edited", "use recall_memory")];
    let result = select_candidates(
        vec![semantic("removed", "旧事实"), semantic("edited", "use recall-memory")],
        vec![keyword("edited", "use recall-memory")], &ledger, &HashMap::new(), None, 6,
    );
    assert!(result.memories.is_empty());
    assert_eq!(result.discarded_candidates, 3);
}

#[test]
fn equal_text_keeps_distinct_identities_and_scope_is_applied_before_selection() {
    let ledger = vec![memory("project-a", "使用本地缓存"), memory("project-b", "使用本地缓存")];
    let links = HashMap::from([("project-a".into(), vec!["a".into()]), ("project-b".into(), vec!["b".into()])]);
    let nominations = vec![semantic("project-a", "使用本地缓存"), semantic("project-b", "使用本地缓存")];
    let all = select_candidates(nominations.clone(), Vec::new(), &ledger, &links, None, 6);
    assert_eq!(all.memories.len(), 2);
    let scoped = select_candidates(nominations, Vec::new(), &ledger, &links, Some(&["b".into()]), 1);
    assert_eq!(scoped.memories[0]["id"], "project-b");
}

#[test]
fn tied_scores_have_a_stable_identity_order() {
    let ledger = vec![memory("b", "乙事实"), memory("a", "甲事实")];
    let result = select_candidates(vec![semantic("b", "乙事实")], vec![keyword("a", "甲事实")], &ledger, &HashMap::new(), None, 2);
    assert_eq!(result.memories[0]["id"], "a");
    assert_eq!(result.memories[1]["id"], "b");
}

#[test]
fn diversity_promotes_additional_evidence_without_deleting_memory() {
    let ledger = vec![memory("a", "本项目使用本地语义缓存"), memory("b", "本项目使用本地语义缓存"), memory("c", "保留用户手动发布的画像")];
    let nominations = ledger.iter().map(|m| semantic(&m.id, &m.memory)).collect();
    let result = select_candidates(nominations, Vec::new(), &ledger, &HashMap::new(), None, 3);
    assert_eq!(result.memories[1]["id"], "c");
    assert_eq!(result.memories.len(), 3);
}

#[test]
fn diversity_does_not_penalize_opposing_or_different_time_evidence() {
    let a = memory("a", "允许执行编译");
    let b = memory("b", "不允许执行编译");
    let mut c = memory("c", "允许执行编译");
    c.event_time = Some("2026-11-10".into());
    let ledger = vec![a, b, c];
    let nominations = ledger.iter().map(|m| semantic(&m.id, &m.memory)).collect();
    let result = select_candidates(nominations, Vec::new(), &ledger, &HashMap::new(), None, 3);
    assert!(result.memories.iter().all(|row| row["diversity_penalty"] == 0.0));
}

#[test]
fn candidate_snapshot_is_bounded_to_nominated_current_ids_and_links() {
    let store = TelemetryStore::in_memory();
    for (id, text) in [("a", "alpha current"), ("b", "beta current"), ("unrelated", "outside nominations")] {
        store.conn.lock().unwrap().execute(
            "INSERT INTO local_memory_items
             (id, source_event_key, ordinal, memory, memory_type, durability, source, session_id,
              user_defined, created_at, updated_at)
             VALUES (?1, ?1, 0, ?2, 'fact', 'short_term', 'test', 'test', 0, 'now', 'now')",
            rusqlite::params![id, text],
        ).unwrap();
    }
    for (id, domain) in [("a", "work"), ("b", "study"), ("unrelated", "life")] {
        store.replace_backfill_memory_domains(id, &[crate::telemetry_store::MemoryDomainAssignment {
            domain_id: domain.into(), weight: 1.0,
        }]).unwrap();
    }
    let (ledger, links) = candidate_snapshot(&store, &["b", "a", "missing", "a"]).unwrap();
    assert_eq!(ledger.iter().map(|memory| memory.id.as_str()).collect::<HashSet<_>>(), HashSet::from(["a", "b"]));
    assert_eq!(links.len(), 2);
    assert_eq!(links["a"], ["work"]);
    assert_eq!(links["b"], ["study"]);
    assert!(candidate_snapshot(&store, &[]).unwrap().0.is_empty());

    store.conn.lock().unwrap().execute("DELETE FROM local_memory_items WHERE id='a'", []).unwrap();
    store.conn.lock().unwrap().execute("UPDATE local_memory_items SET memory='beta edited' WHERE id='b'", []).unwrap();
    store.replace_backfill_memory_domains("b", &[crate::telemetry_store::MemoryDomainAssignment {
        domain_id: "work".into(), weight: 1.0,
    }]).unwrap();
    let (current, current_links) = candidate_snapshot(&store, &["a", "b"]).unwrap();
    assert_eq!(current.len(), 1);
    assert_eq!(current[0].memory, "beta edited");
    assert_eq!(current_links["b"], ["work"]);
    let result = select_candidates(vec![semantic("a", "alpha current"), semantic("b", "beta current")],
        vec![keyword("b", "beta current")], &current, &current_links, None, 4);
    assert!(result.memories.is_empty(), "candidate refresh must not restore deleted or edited nominations");
}

#[test]
fn adapters_share_keyword_results_when_the_semantic_budget_is_exhausted() {
    let store = TelemetryStore::in_memory();
    store.conn.lock().unwrap().execute(
        "INSERT INTO local_memory_items
         (id, source_event_key, ordinal, memory, memory_type, durability, source, session_id,
          user_defined, created_at, updated_at)
         VALUES ('test:recall', 'test:event', 0, '氪石缓存保留完整条件', 'fact', 'short_term',
                 'test', 'test', 0, '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z')", [],
    ).unwrap();
    let arguments = json!({ "query": "氪石缓存", "domain_mode": "off", "limit": 6 });
    let direct = recall(&store, &arguments, Some(0)).unwrap();
    let adapter = crate::memory_mcp::recall_with_store_budget(&store, &arguments, Some(0)).unwrap();
    assert_eq!(direct["memories"], adapter["memories"]);
    assert_eq!(direct["memories"][0]["id"], "test:recall");
    assert_eq!(direct["retrieval"]["mode"], "local_fallback_budget");
    assert_eq!(direct["routing"], adapter["routing"]);
}

#[test]
fn invalid_manual_scope_is_rejected_instead_of_searching_globally() {
    let store = TelemetryStore::in_memory();
    let arguments = json!({ "query": "缓存", "domain_mode": "manual", "domains": ["missing-scope"] });
    assert!(recall(&store, &arguments, Some(0)).is_err());
}

#[test]
fn cancellation_and_expired_budgets_stop_before_network_work() {
    let expired = RecallBudget::new(Duration::ZERO);
    assert_eq!(expired.remaining().unwrap_err(), "recall_deadline");
    let cancelled = RecallBudget::new(Duration::from_secs(60));
    cancelled.cancel();
    assert_eq!(cancelled.remaining().unwrap_err(), "recall_cancelled");
}

#[test]
fn trace_preserves_filter_order_effective_ranks_and_cross_route_votes() {
    let ledger = vec![memory("a", "当前事实"), memory("b", "领域外事实"), memory("c", "无效评分事实")];
    let links = HashMap::from([
        ("a".into(), vec!["scope".into()]), ("b".into(), vec!["other".into()]),
        ("c".into(), vec!["scope".into()]),
    ]);
    let mut invalid = semantic("c", "无效评分事实");
    invalid.score = Some(f64::NAN);
    let result = select_candidates(vec![
        semantic("gone", "旧正文"), semantic("b", "也是旧正文"),
        semantic("a", "旧正文"), invalid,
        semantic("a", "当前事实"), semantic("a", "当前事实"),
    ], vec![keyword("a", "当前事实")], &ledger, &links, Some(&["scope".into()]), 1);
    let statuses: Vec<_> = result.nominations.iter().map(|row| row.status.as_str()).collect();
    assert_eq!(statuses, ["missing_current_memory", "scope_mismatch", "content_mismatch",
        "invalid_score", "accepted", "duplicate_in_route", "accepted"]);
    assert_eq!(result.nominations[4].input_rank, 5);
    assert_eq!(result.nominations[4].eligible_rank, Some(1));
    assert_eq!(result.nominations[6].eligible_rank, Some(1));
    assert_eq!(result.nominations[3].score, None);
    assert_eq!(result.memories[0]["fused_score"], 1.0);
    assert_eq!(result.ranking[0].selected_rank, Some(1));
    assert_eq!(result.ranking[0].semantic_rank, Some(1));
    assert_eq!(result.ranking[0].keyword_rank, Some(1));
}

#[test]
fn trace_records_diversity_as_a_soft_penalty_not_a_filter() {
    let ledger = vec![memory("a", "本项目使用本地语义缓存"), memory("b", "本项目使用本地语义缓存"), memory("c", "独立来源的其他证据")];
    let result = select_candidates(ledger.iter().map(|row| semantic(&row.id, &row.memory)).collect(),
        Vec::new(), &ledger, &HashMap::new(), None, 2);
    let similar = result.ranking.iter().find(|row| row.memory_id == "b").unwrap();
    assert!(similar.diversity_penalty > 0.0);
    assert_eq!(similar.outcome, "not_selected_at_limit");
    assert_eq!(result.discarded_candidates, 0);
    let ordered: Vec<_> = result.results.iter().map(|row| row.memory_id.as_str()).collect();
    assert_eq!(ordered, ["a", "c"]);
}

#[test]
fn fingerprints_track_current_content_and_scope_but_ignore_domain_order() {
    let mut row = memory("a", "原始事实");
    let first = trace::fingerprint(&row, &["a".into(), "b".into()]);
    assert_eq!(first, trace::fingerprint(&row, &["b".into(), "a".into(), "a".into()]));
    assert_ne!(first, trace::fingerprint(&row, &["a".into()]));
    row.memory = "用户修改后的事实".into();
    assert_ne!(first, trace::fingerprint(&row, &["a".into(), "b".into()]));
}

#[test]
fn bounded_trace_keeps_result_identity_without_persisting_memory_text() {
    let row = memory("a", "private-memory-body-not-for-trace-storage");
    let selected = select_candidates(vec![semantic("a", &row.memory)], Vec::new(), &[row], &HashMap::new(), None, 1);
    let mut record = trace::RecallTrace {
        schema_version: 1, request_id: "request".into(), created_at: "2026-11-11T00:00:00.000Z".into(),
        entrypoint: "desktop".into(), query: "查询".into(),
        arguments: json!({ "query": "查询", "domain_mode": "off", "limit": 1 }),
        routing: json!({ "active_domains": [] }), retrieval: json!({}),
        candidates: selected.nominations, ranking: selected.ranking, results: selected.results,
        truncated: false,
    };
    let raw = record.bounded_json().unwrap();
    assert!(!raw.contains("private-memory-body-not-for-trace-storage"));
    for _ in 0..40 {
        record.candidates.push(trace::TraceNomination {
            route: "semantic".into(), memory_id: "oversized-id".repeat(500),
            input_rank: 2, eligible_rank: None, score: None, status: "missing_current_memory".into(),
        });
    }
    let bounded = record.bounded_json().unwrap();
    assert!(bounded.len() <= trace::MAX_TRACE_BYTES);
    let decoded: trace::RecallTrace = serde_json::from_str(&bounded).unwrap();
    assert!(decoded.truncated);
    assert_eq!(decoded.results[0].memory_id, "a");
    assert_eq!(decoded.retrieval["trace_truncated"], true);
}
