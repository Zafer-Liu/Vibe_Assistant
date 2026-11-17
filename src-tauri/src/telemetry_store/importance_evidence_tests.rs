//! Importance-scan regressions: keep matching semantics while normalizing the
//! conversation snapshot once, outside the writer connection's mutex.
use super::*;
use serde_json::json;

#[test]
fn importance_snapshot_preserves_case_substrings_thresholds_and_session_deduplication() {
    let store = TelemetryStore::in_memory();
    for (source, session, turn, text) in [
        ("fixture-a", "same-session", 1, "AUTH FILEPATH TOKEN 法律责任 apple bridge"),
        ("fixture-a", "same-session", 2, "Auth filepath token 法律和责任 apple bridge"),
        ("fixture-b", "other-session", 1, "auth token 法律责任 apple bridge copper"),
        ("fixture-c", "unmatched-session", 1, "auth only"),
    ] {
        let payload = json!({
            "session_id": session, "hook_event_name": "fixture", "turn": turn,
        });
        // Completed conversation text attaches to an existing Hook receipt.
        store.record_hook(source, &payload).unwrap();
        store.record_conversation(source, &payload, "full", &[("user".into(), text.into())]).unwrap();
    }
    let candidates: Vec<_> = [
        ("case", "AUTH TOKEN"),
        ("substring", "PATH TOKEN"),
        ("chinese", "法律责任"),
        ("threshold", "apple bridge copper drum eagle forest globe harbor"),
        ("missing", "absent unrelated"),
    ].into_iter().map(|(id, content)| MemoryImportanceCandidate {
        memory_id: id.into(), content: content.into(),
    }).collect();
    let evidence = store.importance_evidence(&candidates).unwrap();
    assert_eq!(evidence["case"].supporting_sessions, 2, "two turns in one session count once");
    assert_eq!(evidence["case"].supporting_agents, 2);
    assert_eq!(evidence["substring"].supporting_sessions, 1, "PATH still matches FILEPATH");
    assert_eq!(evidence["chinese"].supporting_sessions, 2);
    assert_eq!(evidence["threshold"].supporting_sessions, 1, "eight terms still require three hits");
    assert!(!evidence.contains_key("missing"));
}
