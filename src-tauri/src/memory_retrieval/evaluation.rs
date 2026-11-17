//! Offline ranking regression metrics. Semantic nominations in the fixture
//! are fixed inputs, not measured embedding-model quality or end-to-end latency.
use super::*;
use serde::Deserialize;

use super::benchmark::metrics::evaluate;

#[derive(Deserialize)]
struct Fixture { version: u32, cases: Vec<Case> }
#[derive(Deserialize)]
struct Case {
    name: String,
    query: String,
    limit: usize,
    memories: Vec<FixtureMemory>,
    semantic: Vec<String>,
    keyword: Vec<String>,
    #[serde(default)]
    domains: Option<Vec<String>>,
    expected: Vec<String>,
    #[serde(default)]
    forbidden: Vec<String>,
}
#[derive(Deserialize)]
struct FixtureMemory {
    id: String,
    content: String,
    #[serde(default)]
    domains: Vec<String>,
    #[serde(default)]
    event_time: Option<String>,
}

#[test]
fn fixed_ranking_fixture_protects_recall_scope_and_empty_results() {
    let fixture: Fixture = serde_json::from_str(include_str!("fixtures.json")).unwrap();
    assert_eq!(fixture.version, 1);
    let mut reports = Vec::new();
    for case in fixture.cases {
        let ledger: Vec<_> = case.memories.iter().map(|row| LocalMemorySnapshot {
            id: row.id.clone(), memory: row.content.clone(), memory_type: "fact".into(), durability: "short_term".into(),
            source_event_key: "fixture".into(), ordinal: 0, source: "fixture".into(), session_id: "fixture".into(),
            user_defined: false, event_time: row.event_time.clone(),
            created_at: "2026-01-01T00:00:00Z".into(), updated_at: "2026-01-01T00:00:00Z".into(),
        }).collect();
        let links = case.memories.iter().map(|row| (row.id.clone(), row.domains.clone())).collect();
        let content = |id: &str| ledger.iter().find(|row| row.id == id).map(|row| row.memory.clone()).unwrap_or_else(|| "deleted fixture".into());
        let semantic = case.semantic.iter().map(|id| SemanticMemory {
            id: id.clone(), memory: content(id), memory_type: "fact".into(), score: Some(0.8),
        }).collect();
        let keyword = case.keyword.iter().map(|id| LocalMemory {
            id: id.clone(), memory: content(id), memory_type: "fact".into(), durability: "short_term".into(),
            user_defined: false, event_time: None, last_update_at: "2026-01-01T00:00:00Z".into(), score: Some(1.0),
        }).collect();
        let selection = select_candidates(semantic, keyword, &ledger, &links, case.domains.as_deref(), case.limit);
        let ids: Vec<_> = selection.memories.iter().map(|row| row["id"].as_str().unwrap().to_string()).collect();
        let metrics = evaluate(&case.expected, &ids, &case.forbidden, case.limit);
        assert_eq!(metrics.duplicate_rate, 0.0, "{}", case.name);
        assert_eq!(metrics.forbidden_rate, 0.0, "{}", case.name);
        assert!(!metrics.false_positive_on_empty_query, "{}", case.name);
        if let Some(recall) = metrics.recall_at_k { assert_eq!(recall, 1.0, "{}: {:?}", case.name, ids); }
        reports.push(json!({ "case": case.name, "query": case.query, "returned": ids, "metrics": metrics }));
    }
    println!("{}", json!({ "fixture_version": 1, "ranking_version": RANKING_VERSION, "scope": "fixed_candidate_selection", "cases": reports }));
}

#[test]
fn metrics_use_all_relevant_ids_for_the_ideal_ranking_and_penalize_duplicates() {
    let metrics = evaluate(&["a".into(), "b".into(), "c".into()], &["a".into(), "a".into(), "x".into()], &["x".into()], 3);
    assert_eq!(metrics.recall_at_k, Some(1.0 / 3.0));
    assert_eq!(metrics.mrr, Some(1.0));
    assert!(metrics.ndcg_at_k.unwrap() < 0.5);
    assert_eq!(metrics.duplicate_rate, 1.0 / 3.0);
    assert_eq!(metrics.forbidden_rate, 1.0 / 3.0);
}

#[test]
fn empty_ground_truth_is_reported_separately_not_as_perfect_recall() {
    let metrics = evaluate(&[], &["irrelevant".into()], &[], 6);
    assert_eq!(metrics.recall_at_k, None);
    assert_eq!(metrics.ndcg_at_k, None);
    assert!(metrics.false_positive_on_empty_query);
}
