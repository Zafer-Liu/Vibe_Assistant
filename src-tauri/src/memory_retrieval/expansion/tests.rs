use super::*;
use serde_json::json;

fn insert(store: &TelemetryStore, id: &str, content: &str, domain: &str) {
    store.conn.lock().unwrap().execute(
        "INSERT INTO local_memory_items
         (id,source_event_key,ordinal,memory,memory_type,durability,source,session_id,user_defined,created_at,updated_at)
         VALUES (?1,?1,0,?2,'fact','short_term','test','test',0,'2026-01-01T00:00:00Z','2026-01-01T00:00:00Z')",
        rusqlite::params![id, content],
    ).unwrap();
    store.replace_backfill_memory_domains(id, &[crate::telemetry_store::MemoryDomainAssignment {
        domain_id: domain.into(), weight: 1.0,
    }]).unwrap();
}

fn recall(store: &TelemetryStore, query: &str, limit: usize) -> serde_json::Value {
    let budget = RecallBudget::new(Duration::from_secs(2));
    super::super::recall_for_evaluation(store,
        &json!({ "query": query, "limit": limit, "domain_mode": "manual", "domains": ["work"] }),
        2_000, &budget, None, None, &crate::memory_domain::MemoryDomainConfig::default(),
    ).unwrap()
}

#[test]
fn low_recall_backfills_bilingual_terms_inside_the_original_scope() {
    let store = TelemetryStore::in_memory();
    insert(&store, "work-cache", "cache.", "work");
    insert(&store, "other-cache", "cache", "life");
    assert!(store.search_local_l1_memories_scoped("缓存", 24, Some(&["work".into()])).unwrap().is_empty());
    let response = recall(&store, "缓存", 6);
    assert_eq!(response["memories"].as_array().unwrap().len(), 1);
    assert_eq!(response["memories"][0]["id"], "work-cache");
    assert_eq!(response["retrieval"]["keyword_expansion"]["added_candidates"], 1);
    assert_eq!(response["retrieval"]["initial_eligible_candidates"], 0);
    assert_eq!(response["routing"]["active_domains"], json!(["work"]));
}

#[test]
fn enough_valid_candidates_skip_all_extra_queries() {
    let store = TelemetryStore::in_memory();
    insert(&store, "direct", "缓存", "work");
    insert(&store, "alternate", "cache", "work");
    let response = recall(&store, "缓存", 1);
    assert_eq!(response["memories"][0]["id"], "direct");
    assert_eq!(response["retrieval"]["keyword_expansion"]["status"], "enough_candidates");
    assert_eq!(response["retrieval"]["keyword_expansion"]["searched"], 0);
}

#[test]
fn several_aliases_do_not_create_multiple_votes_or_reorder_direct_hits() {
    let store = TelemetryStore::in_memory();
    insert(&store, "direct", "配置", "work");
    insert(&store, "alternate", "config configuration", "work");
    let response = recall(&store, "配置", 6);
    let rows = response["memories"].as_array().unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0]["id"], "direct");
    assert_eq!(rows[1]["id"], "alternate");
    assert_eq!(rows[1]["keyword_rank"], 2);
    assert_eq!(rows[1]["semantic_rank"], serde_json::Value::Null);
    assert_eq!(rows[1]["source"], "local_keyword_fallback");
    assert_eq!(response["retrieval"]["keyword_expansion"]["added_candidates"], 1);
}

#[test]
fn expansion_preserves_project_number_and_negative_anchors() {
    let store = TelemetryStore::in_memory();
    insert(&store, "match", "project-a not 缓存 64", "work");
    insert(&store, "wrong-number", "project-a not 缓存 128", "work");
    insert(&store, "wrong-project", "project-b not 缓存 64", "work");
    insert(&store, "positive", "project-a 缓存 64", "work");
    insert(&store, "number-prefix", "project-a not 缓存 640", "work");
    insert(&store, "decimal", "project-a not 缓存 64.5", "work");
    insert(&store, "project-prefix", "project-a1 not 缓存 64", "work");
    insert(&store, "negative-prefix", "project-a nothing 缓存 64", "work");
    let mut results = Vec::new();
    backfill(&store, "project-a not cache 64", Some(&["work".into()]), &mut results, 24,
        &RecallBudget::new(Duration::from_secs(2)));
    assert_eq!(results.iter().map(|row| row.id.as_str()).collect::<Vec<_>>(), ["match"]);
}

#[test]
fn wildcard_anchors_are_literals_and_ascii_aliases_are_whole_words() {
    let store = TelemetryStore::in_memory();
    insert(&store, "literal", "cache x_1", "work");
    insert(&store, "wildcard", "cache xy1", "work");
    insert(&store, "substring", "cachebuster x_1", "work");
    insert(&store, "anchor-substring", "cache x_10", "work");
    let mut results = Vec::new();
    backfill(&store, "缓存 x_1", Some(&["work".into()]), &mut results, 24,
        &RecallBudget::new(Duration::from_secs(2)));
    assert_eq!(results.iter().map(|row| row.id.as_str()).collect::<Vec<_>>(), ["literal"]);
}

#[test]
fn expired_budget_and_empty_scope_preserve_existing_candidates() {
    let store = TelemetryStore::in_memory();
    insert(&store, "alternate", "cache", "work");
    let mut results = Vec::new();
    let diagnostics = backfill(&store, "缓存", None, &mut results, 24, &RecallBudget::new(Duration::ZERO));
    assert_eq!(diagnostics.status, "budget_exhausted");
    assert_eq!(diagnostics.searched, 0);
    assert!(results.is_empty());
    backfill(&store, "缓存", Some(&[]), &mut results, 24, &RecallBudget::new(Duration::from_secs(2)));
    assert!(results.is_empty());
}

#[test]
fn technical_literals_and_unrelated_queries_are_not_expanded() {
    for query in ["cachebuster", "cache_line", "https://example.test/cache", "C:\\cache", "`cache`", "海洋潮汐"] {
        assert!(variants(query).is_empty(), "{query}");
    }
    assert!(variants("cache timeout config build").len() <= MAX_VARIANTS);
    assert_eq!(variants("work cache")[0].required, ["work"], "topic words must not be treated as grammatical filler");
    let store = TelemetryStore::in_memory();
    insert(&store, "unrelated", "cache", "work");
    let result = recall(&store, "海洋潮汐", 6);
    assert!(result["memories"].as_array().unwrap().is_empty());
    assert_eq!(result["retrieval"]["keyword_expansion"]["status"], "no_variants");
}
