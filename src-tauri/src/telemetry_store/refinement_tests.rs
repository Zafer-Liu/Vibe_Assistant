//! Recovery and concurrency regressions. These tests do not call any model.
use super::*;
use crate::memory_refinement::RefinementAction;

fn insert(store: &TelemetryStore, id: &str, content: &str) {
    store.conn.lock().unwrap().execute(
        "INSERT INTO local_memory_items(id, source_event_key, ordinal, memory, memory_type,
             durability, source, session_id, event_time, created_at, updated_at)
         VALUES (?1, ?1, 0, ?2, 'fact', 'short_term', 'test', ?1,
             '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z')",
        params![id, content],
    ).unwrap();
}

fn tag(store: &TelemetryStore, memory_id: &str, domains: &[&str]) {
    let conn = store.conn.lock().unwrap();
    for domain in domains {
        conn.execute("INSERT OR IGNORE INTO memory_domains(id, name, kind, created_at, updated_at)
            VALUES (?1, ?1, 'custom', 'now', 'now')", [domain]).unwrap();
        conn.execute("INSERT OR IGNORE INTO local_memory_domains(memory_id, domain_id, weight, source, created_at, updated_at)
            VALUES (?1, ?2, 1, 'auto', 'now', 'now')", params![memory_id, domain]).unwrap();
    }
}

fn plan(store: &TelemetryStore, ids: &[&str], kind: &str) -> (String, String) {
    let mut run = store.refinement_prepare(None, false).unwrap();
    let originals: Vec<_> = ids.iter().map(|id| run.inventory.iter().find(|memory| memory.view.id == *id).unwrap().view.clone()).collect();
    let action_id = uuid::Uuid::new_v4().to_string();
    run.view.actions.push(RefinementAction {
        id: action_id.clone(), kind: kind.into(), content: "An exact retained project fact".into(),
        memory_type: "fact".into(), durability: "short_term".into(), originals,
        reason: "Supported by original project facts".into(), evidence: vec!["Original fact".into()],
        status: "pending".into(), message: None, recommended_source_id: None,
    });
    store.refinement_save(&run).unwrap();
    (run.view.id, action_id)
}

fn apply(store: &TelemetryStore, run_id: &str, action_id: &str) -> StoredRun {
    store.refinement_apply(run_id, vec![RefinementSelection { action_id: action_id.into(), content: None, domain_ids: None, conflict_source_id: None }]).unwrap()
}

fn delete_group(store: &TelemetryStore, run: &str, action: &str, status: &str) -> Result<StoredRun, String> {
    store.refinement_delete(run, vec![RefinementDeleteSelection { action_id: action.into(), expected_status: status.into() }])
}

#[test]
fn clearing_history_covers_more_than_twenty_runs_without_changing_the_ledger() {
    let store = TelemetryStore::in_memory();
    for id in ["a", "b", "untouched"] { insert(&store, id, id); tag(&store, id, &["work"]); }
    let (id, action) = plan(&store, &["a", "b"], "merge");
    let applied = apply(&store, &id, &action);
    let keeper = applied.changes[0].after[0].clone();
    let mut last = None;
    for _ in 0..24 {
        let run = store.refinement_prepare(None, false).unwrap();
        store.refinement_create(&run).unwrap();
        last = Some(run);
    }
    let mut stale_worker = last.unwrap();
    let checked = stale_worker.inventory.iter().position(|memory| memory.view.id == "untouched").unwrap();
    store.refinement_checkpoint(&stale_worker, &[checked]).unwrap();
    assert_eq!(store.refinement_history(None).unwrap().len(), 20);
    let before = store.refinement_history_snapshot().unwrap();
    assert_eq!(before.count, 25);
    let tombstones = store.tombstoned_l1_ids().unwrap();
    assert_eq!(store.refinement_clear(&before.revision).unwrap(), 25);
    assert_eq!(store.refinement_history_snapshot().unwrap().count, 0);
    assert!(store.refinement_history(None).unwrap().is_empty());
    assert!(store.refinement_publish(&mut stale_worker, &[]).is_err());
    assert_eq!(store.local_l1_memory_snapshot().unwrap().len(), 2);
    assert_eq!(store.tombstoned_l1_ids().unwrap(), tombstones);
    let current = snapshot(&store.conn.lock().unwrap(), &keeper.view.id).unwrap().unwrap();
    assert_eq!(current.signature, keeper.signature);
    assert_eq!(current.provenance.len(), 2);
    let conn = store.conn.lock().unwrap();
    assert_eq!(conn.query_row("SELECT count(*) FROM memory_refinement_inputs", [], |row| row.get::<_,i64>(0)).unwrap(), 0);
    assert_eq!(conn.query_row("SELECT count(*) FROM memory_refinement_reviews WHERE memory_id='untouched'", [], |row| row.get::<_,i64>(0)).unwrap(), 1);
}

#[test]
fn clearing_rejects_a_new_or_updated_record_after_confirmation() {
    let store = TelemetryStore::in_memory();
    let first = store.refinement_prepare(None, false).unwrap();
    store.refinement_create(&first).unwrap();
    let old = store.refinement_history_snapshot().unwrap();
    let later = store.refinement_prepare(None, false).unwrap();
    store.refinement_create(&later).unwrap();
    assert!(store.refinement_clear(&old.revision).is_err());
    assert_eq!(store.refinement_history_snapshot().unwrap().count, 2);
    let old = store.refinement_history_snapshot().unwrap();
    store.refinement_append_warnings(&first.view.id, vec!["A new progress note".into()]).unwrap();
    assert!(store.refinement_clear(&old.revision).is_err());
    assert_eq!(store.refinement_history_snapshot().unwrap().count, 2);
    let current = store.refinement_history_snapshot().unwrap();
    assert_eq!(store.refinement_clear(&current.revision).unwrap(), 2);
    let empty = store.refinement_history_snapshot().unwrap();
    assert_eq!(store.refinement_clear(&empty.revision).unwrap(), 0);
}

#[test]
fn clearing_legacy_history_is_atomic_until_sync_compatibility_is_preserved() {
    let store = TelemetryStore::in_memory();
    insert(&store, "local-l1:legacy", "Original fact");
    let legacy = legacy_applied_refinement(&store, "local-l1:legacy");
    let other = store.refinement_prepare(None, false).unwrap();
    store.refinement_create(&other).unwrap();
    let snapshot = store.refinement_history_snapshot().unwrap();
    assert!(store.refinement_clear(&snapshot.revision).is_err());
    assert!(store.refinement_load(&legacy).is_ok());
    assert!(store.refinement_load(&other.view.id).is_ok());
    store.prepare_refinements_for_cloud_sync(&[]).unwrap();
    let snapshot = store.refinement_history_snapshot().unwrap();
    assert_eq!(store.refinement_clear(&snapshot.revision).unwrap(), 2);
    assert_eq!(store.local_l1_memory_snapshot().unwrap()[0].memory, "Legacy refined content");
}

#[test]
fn forgetting_history_removes_inputs_but_keeps_memories_and_incremental_checkpoints() {
    let store = TelemetryStore::in_memory();
    insert(&store, "a", "Keep this actual memory");
    tag(&store, "a", &["work"]);
    let (id, _) = plan(&store, &["a"], "rewrite");
    let mut stale_worker = store.refinement_load(&id).unwrap();
    let checked: Vec<_> = stale_worker.targets.iter().copied().collect();
    store.refinement_checkpoint(&stale_worker, &checked).unwrap();
    let signature = snapshot(&store.conn.lock().unwrap(), "a").unwrap().unwrap().signature;
    assert!(store.refinement_forget(&id, &stale_worker.view.updated_at).unwrap());
    assert!(!store.refinement_forget(&id, &stale_worker.view.updated_at).unwrap());
    assert!(store.refinement_view(&id).is_err());
    assert!(store.refinement_load(&id).is_err());
    assert!(store.refinement_history(None).unwrap().iter().all(|run| run.id != id));
    assert_eq!(snapshot(&store.conn.lock().unwrap(), "a").unwrap().unwrap().signature, signature);
    assert!(store.tombstoned_l1_ids().unwrap().is_empty());
    let conn = store.conn.lock().unwrap();
    assert_eq!(conn.query_row("SELECT count(*) FROM memory_refinement_inputs WHERE run_id=?1", [&id], |row| row.get::<_,i64>(0)).unwrap(), 0);
    assert_eq!(conn.query_row("SELECT count(*) FROM memory_refinement_reviews WHERE memory_id='a'", [], |row| row.get::<_,i64>(0)).unwrap(), 1);
    drop(conn);
    assert!(store.refinement_sync_reviews(&mut stale_worker).is_err());
    assert!(store.refinement_publish(&mut stale_worker, &checked).is_err());
    assert!(store.refinement_history(None).unwrap().is_empty());
}

#[test]
fn forgetting_an_applied_run_preserves_result_provenance_protection_and_sync_markers() {
    let store = TelemetryStore::in_memory();
    for id in ["a", "b"] { insert(&store, id, id); tag(&store, id, &["work"]); }
    let (id, action) = plan(&store, &["a", "b"], "merge");
    let applied = apply(&store, &id, &action);
    let keeper = &applied.changes[0].after[0];
    let deleted_before = store.tombstoned_l1_ids().unwrap();
    assert!(store.refinement_forget(&id, &applied.view.updated_at).unwrap());
    let current = snapshot(&store.conn.lock().unwrap(), &keeper.view.id).unwrap().unwrap();
    assert_eq!(current.signature, keeper.signature);
    assert_eq!(current.provenance.len(), 2);
    assert!(current.protected);
    assert_eq!(current.view.domains, vec!["work"]);
    assert_eq!(store.local_l1_memory_snapshot().unwrap().len(), 1);
    assert_eq!(store.tombstoned_l1_ids().unwrap(), deleted_before);
    assert!(store.refinement_undo(&id).is_err());
}

#[test]
fn forgetting_requires_the_confirmed_record_version() {
    let store = TelemetryStore::in_memory();
    insert(&store, "a", "Unchanged memory");
    let (id, _) = plan(&store, &["a"], "rewrite");
    let old = store.refinement_view(&id).unwrap();
    store.refinement_append_warnings(&id, vec!["A later update".into()]).unwrap();
    assert!(store.refinement_forget(&id, &old.updated_at).is_err());
    assert!(store.refinement_load(&id).is_ok());
    assert_eq!(store.local_l1_memory_snapshot().unwrap().len(), 1);
}

#[test]
fn initial_creation_is_explicit_and_progress_cannot_create_or_recreate_a_run() {
    let store = TelemetryStore::in_memory();
    let mut run = store.refinement_prepare(None, false).unwrap();
    assert!(store.refinement_publish(&mut run, &[]).is_err());
    store.refinement_create(&run).unwrap();
    assert!(store.refinement_create(&run).is_err());
    run.view.status = "analyzing".into();
    store.refinement_publish(&mut run, &[]).unwrap();
    assert!(store.refinement_forget(&run.view.id, &run.view.updated_at).unwrap());
    assert!(store.refinement_publish(&mut run, &[]).is_err());
    assert!(store.refinement_history(None).unwrap().is_empty());
}

#[test]
fn old_sync_identities_keep_their_compatibility_journal_until_upgraded() {
    let store = TelemetryStore::in_memory();
    insert(&store, "local-l1:legacy", "Original fact");
    let id = legacy_applied_refinement(&store, "local-l1:legacy");
    let old = store.refinement_view(&id).unwrap();
    assert!(store.refinement_forget(&id, &old.updated_at).is_err());
    assert!(store.refinement_load(&id).is_ok());
    store.prepare_refinements_for_cloud_sync(&[]).unwrap();
    let upgraded = store.refinement_view(&id).unwrap();
    assert!(store.refinement_forget(&id, &upgraded.updated_at).unwrap());
    let memories = store.local_l1_memory_snapshot().unwrap();
    assert_eq!(memories.len(), 1);
    assert_eq!(memories[0].memory, "Legacy refined content");
    assert!(memories[0].id.starts_with("local-l1:refined:"));
}

fn recommendation_fixture(store: &TelemetryStore, id: &str) -> (Vec<MemoryState>, RefinementAction) {
    let run = store.refinement_load(id).unwrap();
    let mut action = run.view.actions[0].clone();
    action.content = "Final document uses inline links after the footnote scheme was withdrawn".into();
    action.recommended_source_id = Some("b".into());
    action.reason = "The source explicitly records withdrawal of the earlier scheme".into();
    action.evidence = vec!["b: explicit withdrawal and final verification".into()];
    let candidates = action.originals.iter().map(|source|
        run.inventory.iter().find(|memory| memory.view.id == source.id).unwrap().clone()).collect();
    (candidates, action)
}

#[test]
fn generated_conflict_draft_is_saved_without_mutating_memories_and_can_be_confirmed_unchanged() {
    let store = TelemetryStore::in_memory();
    insert(&store, "a", "Earlier footnote scheme");
    insert(&store, "b", "Final inline links after withdrawal");
    tag(&store, "a", &["work"]);
    tag(&store, "b", &["work"]);
    let (id, action_id) = plan(&store, &["a", "b"], "conflict");
    let mut worker = store.refinement_load(&id).unwrap();
    worker.view.status = "analyzing".into();
    store.refinement_save(&worker).unwrap();
    let (candidates, recommendation) = recommendation_fixture(&store, &id);
    let saved = store.refinement_save_conflict_recommendation(&id, &action_id, &candidates, recommendation).unwrap();
    assert_eq!(store.local_l1_memory_snapshot().unwrap().len(), 2);
    assert!(store.tombstoned_l1_ids().unwrap().is_empty());
    assert_eq!(saved.status, "analyzing");
    assert_eq!(saved.actions[0].status, "pending");
    assert_eq!(saved.actions[0].recommended_source_id.as_deref(), Some("b"));
    store.refinement_publish(&mut worker, &[]).unwrap();
    assert_eq!(worker.view.actions[0].content, saved.actions[0].content);
    assert_eq!(worker.view.actions[0].recommended_source_id, saved.actions[0].recommended_source_id);
    let accepted = store.refinement_apply(&id, vec![RefinementSelection {
        action_id, content: Some(saved.actions[0].content.clone()), domain_ids: None,
        conflict_source_id: saved.actions[0].recommended_source_id.clone(),
    }]).unwrap();
    assert_eq!(accepted.view.actions[0].status, "applied");
    let active = store.local_l1_memory_snapshot().unwrap();
    assert_eq!(active.len(), 1);
    assert_eq!(active[0].memory, saved.actions[0].content);
}

#[test]
fn late_conflict_draft_cannot_override_a_skip_or_a_changed_source() {
    let store = TelemetryStore::in_memory();
    for id in ["a", "b"] { insert(&store, id, id); }
    let (id, action_id) = plan(&store, &["a", "b"], "conflict");
    let (candidates, recommendation) = recommendation_fixture(&store, &id);
    store.refinement_skip(&id, vec![action_id.clone()]).unwrap();
    assert!(store.refinement_save_conflict_recommendation(&id, &action_id, &candidates, recommendation).is_err());
    assert_eq!(store.refinement_view(&id).unwrap().actions[0].status, "skipped");
    let (id, action_id) = plan(&store, &["a", "b"], "conflict");
    let (candidates, recommendation) = recommendation_fixture(&store, &id);
    store.update_local_l1_memory("b", "A newer user edit").unwrap();
    let saved = store.refinement_save_conflict_recommendation(&id, &action_id, &candidates, recommendation).unwrap();
    assert_eq!(saved.actions[0].status, "stale");
    assert!(saved.actions[0].recommended_source_id.is_none());
    assert!(store.tombstoned_l1_ids().unwrap().is_empty());
    assert!(store.local_l1_memory_snapshot().unwrap().iter().any(|memory| memory.memory == "A newer user edit"));
}

#[test]
fn generated_draft_cannot_substitute_a_foreign_or_duplicated_source() {
    let store = TelemetryStore::in_memory();
    for id in ["a", "b"] { insert(&store, id, id); }
    let (id, action_id) = plan(&store, &["a", "b"], "conflict");
    let (candidates, mut recommendation) = recommendation_fixture(&store, &id);
    recommendation.recommended_source_id = Some("foreign".into());
    assert!(store.refinement_save_conflict_recommendation(&id, &action_id, &candidates, recommendation.clone()).is_err());
    recommendation.recommended_source_id = Some("b".into());
    recommendation.originals[0] = recommendation.originals[1].clone();
    assert!(store.refinement_save_conflict_recommendation(&id, &action_id, &candidates, recommendation).is_err());
    assert!(store.refinement_view(&id).unwrap().actions[0].recommended_source_id.is_none());
}

#[test]
fn conflict_resolution_preserves_selected_source_metadata_all_provenance_and_undo() {
    let store = TelemetryStore::in_memory();
    insert(&store, "a", "Earlier decision: use document footnotes");
    insert(&store, "b", "Final session outcome: use inline source links");
    tag(&store, "a", &["work"]);
    tag(&store, "b", &["work", "law"]);
    store.conn.lock().unwrap().execute(
        "UPDATE local_memory_items SET memory_type='summary', durability='session', event_time='2026-01-01T00:30:00Z' WHERE id='b'", [],
    ).unwrap();
    let (id, action_id) = plan(&store, &["a", "b"], "conflict");
    let mut worker = store.refinement_load(&id).unwrap();
    let mut overlapping = worker.view.actions[0].clone();
    overlapping.id = "overlapping".into();
    worker.view.actions.push(overlapping);
    worker.view.status = "analyzing".into();
    store.refinement_save(&worker).unwrap();
    let selection = || RefinementSelection {
        action_id: action_id.clone(), content: Some("Final document uses inline links after footnotes were removed".into()),
        domain_ids: Some(vec!["law".into()]), conflict_source_id: Some("b".into()),
    };
    let applied = store.refinement_apply(&id, vec![selection()]).unwrap();
    assert_eq!(applied.view.status, "analyzing");
    assert_eq!(applied.view.actions[0].kind, "conflict");
    assert_eq!(applied.view.actions[0].status, "applied");
    assert_eq!(applied.view.actions[0].memory_type, "summary");
    assert_eq!(applied.view.actions[0].durability, "session");
    assert_eq!(applied.view.actions[1].status, "stale");
    let after = &applied.changes[0].after[0];
    assert_eq!(after.view.session_id, "b");
    assert_eq!(after.view.event_time.as_deref(), Some("2026-01-01T00:30:00Z"));
    assert_eq!(after.view.domains, vec!["law"]);
    assert_eq!(after.provenance.len(), 2);
    assert_eq!(applied.changes[0].before[0].view.id, "b");
    let exported = store.extracted_l1_sync_snapshot().unwrap();
    assert_eq!(exported.memories.len(), 1);
    assert_eq!(exported.memories[0].id, after.view.id);
    assert!(exported.deleted_ids.contains("a") && exported.deleted_ids.contains("b"));
    assert_eq!(exported.memories[0].domains.as_ref().unwrap()[0].domain.id, "law");
    assert_eq!(store.refinement_apply(&id, vec![selection()]).unwrap().changes.len(), 1);
    store.refinement_publish(&mut worker, &[]).unwrap();
    assert_eq!(worker.view.actions[0].status, "applied");
    assert_eq!(worker.view.actions[1].status, "stale");
    assert_eq!(worker.changes[0].after[0].signature, after.signature);
    store.refinement_undo(&id).unwrap();
    let restored = store.local_l1_memory_snapshot().unwrap();
    assert_eq!(restored.len(), 2);
    assert!(restored.iter().any(|memory| memory.memory == "Earlier decision: use document footnotes"));
    assert!(restored.iter().any(|memory| memory.memory == "Final session outcome: use inline source links"));
    assert!(restored.iter().all(|memory| memory.id != "a" && memory.id != "b" && memory.id != after.view.id));
}

#[test]
fn conflict_application_requires_explicit_source_and_nonempty_content() {
    let store = TelemetryStore::in_memory();
    for id in ["a", "b"] { insert(&store, id, id); }
    let (run, action_id) = plan(&store, &["a", "b"], "conflict");
    for (source, content) in [
        (None, Some("Unconfirmed content")),
        (Some("outside-this-group"), Some("A final decision")),
        (Some("b"), None),
        (Some("b"), Some("  ")),
    ] {
        let result = store.refinement_apply(&run, vec![RefinementSelection {
            action_id: action_id.clone(), content: content.map(str::to_string),
            domain_ids: None, conflict_source_id: source.map(str::to_string),
        }]);
        assert!(result.is_err());
        assert_eq!(store.local_l1_memory_snapshot().unwrap().len(), 2);
        assert!(store.tombstoned_l1_ids().unwrap().is_empty());
        assert!(store.refinement_load(&run).unwrap().changes.is_empty());
    }
}

#[test]
fn conflict_resolution_skips_sources_modified_after_analysis() {
    let store = TelemetryStore::in_memory();
    for id in ["a", "b"] { insert(&store, id, id); }
    let (run, action_id) = plan(&store, &["a", "b"], "conflict");
    store.update_local_l1_memory("b", "A newer manual correction").unwrap();
    let result = store.refinement_apply(&run, vec![RefinementSelection {
        action_id, content: Some("Reviewed decision".into()), domain_ids: None, conflict_source_id: Some("b".into()),
    }]).unwrap();
    assert_eq!(result.view.actions[0].status, "stale");
    assert!(result.changes.is_empty());
    assert!(store.tombstoned_l1_ids().unwrap().is_empty());
    assert!(store.local_l1_memory_snapshot().unwrap().iter().any(|memory| memory.memory == "A newer manual correction"));
}

#[test]
fn incomplete_conflict_rolls_back_preceding_application_in_the_same_batch() {
    let store = TelemetryStore::in_memory();
    for id in ["a", "b", "c"] { insert(&store, id, id); }
    let (id, first) = plan(&store, &["a"], "rewrite");
    let mut run = store.refinement_load(&id).unwrap();
    let mut conflict = run.view.actions[0].clone();
    conflict.id = "conflict".into();
    conflict.kind = "conflict".into();
    conflict.originals = ["b", "c"].into_iter().map(|id|
        run.inventory.iter().find(|memory| memory.view.id == id).unwrap().view.clone()).collect();
    run.view.actions.push(conflict);
    store.refinement_save(&run).unwrap();
    assert!(store.refinement_apply(&id, vec![
        RefinementSelection { action_id: first, content: None, domain_ids: None, conflict_source_id: None },
        RefinementSelection { action_id: "conflict".into(), content: Some("Unconfirmed".into()), domain_ids: None, conflict_source_id: None },
    ]).is_err());
    assert_eq!(store.local_l1_memory_snapshot().unwrap().len(), 3);
    assert!(store.tombstoned_l1_ids().unwrap().is_empty());
    assert!(store.refinement_load(&id).unwrap().changes.is_empty());
}

#[test]
fn reviewed_labels_replace_inherited_labels_and_survive_publication_and_undo() {
    let store = TelemetryStore::in_memory();
    insert(&store, "a", "First legal project fact");
    insert(&store, "b", "Second legal project fact");
    tag(&store, "a", &["law", "general"]);
    tag(&store, "b", &["law"]);
    let (id, action) = plan(&store, &["a", "b"], "merge");
    let mut worker = store.refinement_load(&id).unwrap();
    let applied = store.refinement_apply(&id, vec![RefinementSelection {
        action_id: action, content: None, domain_ids: Some(vec!["law".into(), "law".into()]), conflict_source_id: None,
    }]).unwrap();
    let after = &applied.changes[0].after[0];
    assert_eq!(after.view.domains, vec!["law"]);
    assert_eq!(after.domains.len(), 1);
    assert_eq!(after.domains[0]["source"], "manual");
    assert!(store.completed_memory_domain_classification_ids().unwrap().contains(&after.view.id));
    store.refinement_publish(&mut worker, &[]).unwrap();
    assert_eq!(worker.changes[0].after[0].signature, after.signature);
    let cloud = store.extracted_l1_sync_snapshot().unwrap().memories.into_iter()
        .find(|memory| memory.id == after.view.id).unwrap();
    let links = cloud.domains.unwrap();
    assert_eq!(links.len(), 1);
    assert_eq!(links[0].domain.id, "law");
    assert_eq!(links[0].source, "manual");
    assert!(cloud.classified_at.is_some());
    store.refinement_undo(&id).unwrap();
    let restored = store.local_l1_memory_snapshot().unwrap();
    assert_eq!(restored.len(), 2);
    let conn = store.conn.lock().unwrap();
    for memory in restored {
        let mut domains = snapshot(&conn, &memory.id).unwrap().unwrap().view.domains;
        domains.sort();
        assert_eq!(domains, if memory.session_id == "a" { vec!["general", "law"] } else { vec!["law"] });
    }
}

#[test]
fn cleared_refinement_labels_are_explicitly_unclassified() {
    let store = TelemetryStore::in_memory();
    insert(&store, "a", "A fact with an obsolete label");
    tag(&store, "a", &["law", "unclassified"]);
    let (id, action) = plan(&store, &["a"], "rewrite");
    let applied = store.refinement_apply(&id, vec![RefinementSelection {
        action_id: action, content: None, domain_ids: Some(Vec::new()), conflict_source_id: None,
    }]).unwrap();
    let after = &applied.changes[0].after[0];
    assert_eq!(after.view.domains, vec!["unclassified"]);
    assert_eq!(after.domains[0]["source"], "manual");
    assert!(store.completed_memory_domain_classification_ids().unwrap().contains(&after.view.id));
}

#[test]
fn missing_label_rolls_back_the_entire_application_batch() {
    let store = TelemetryStore::in_memory();
    insert(&store, "a", "First original fact");
    insert(&store, "b", "Second original fact");
    tag(&store, "a", &["law"]);
    let (id, action) = plan(&store, &["a"], "rewrite");
    let mut run = store.refinement_load(&id).unwrap();
    let mut second = run.view.actions[0].clone();
    second.id = "second".into();
    second.originals = vec![run.inventory.iter().find(|memory| memory.view.id == "b").unwrap().view.clone()];
    run.view.actions.push(second);
    store.refinement_save(&run).unwrap();
    assert!(store.refinement_apply(&id, vec![
        RefinementSelection { action_id: action, content: None, domain_ids: Some(vec!["law".into()]), conflict_source_id: None },
        RefinementSelection { action_id: "second".into(), content: None, domain_ids: Some(vec!["missing-label".into()]), conflict_source_id: None },
    ]).is_err());
    let remaining = store.local_l1_memory_snapshot().unwrap();
    assert_eq!(remaining.len(), 2);
    assert!(remaining.iter().any(|memory| memory.id == "a"));
    assert!(remaining.iter().any(|memory| memory.id == "b"));
    assert!(store.tombstoned_l1_ids().unwrap().is_empty());
    let stored = store.refinement_load(&id).unwrap();
    assert!(stored.changes.is_empty());
    assert!(stored.view.actions.iter().all(|action| action.status == "pending"));
}

#[test]
fn deleting_a_merge_group_removes_every_original_but_not_unrelated_memories() {
    let store = TelemetryStore::in_memory();
    for id in ["a", "b", "unrelated"] { insert(&store, id, &format!("Fact {id}")); }
    let (run, action) = plan(&store, &["a", "b"], "merge");
    let deleted = delete_group(&store, &run, &action, "pending").unwrap();
    assert_eq!(deleted.view.actions[0].status, "deleted");
    assert_eq!(deleted.changes.len(), 1);
    let rows = store.local_l1_memory_snapshot().unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].id, "unrelated");
    let tombstones = store.tombstoned_l1_ids().unwrap();
    assert!(tombstones.contains("a") && tombstones.contains("b"));
    store.refinement_undo(&run).unwrap();
    assert_eq!(store.local_l1_memory_snapshot().unwrap().len(), 1, "Undo applied changes must not restore deleted groups");
    insert(&store, "a", "A stale backup recreated this retired ID");
    let repeated = delete_group(&store, &run, &action, "pending").unwrap();
    assert_eq!(repeated.changes.len(), 1, "repeating deletion is idempotent");
    assert_eq!(store.local_l1_memory_snapshot().unwrap().len(), 1, "retry repairs stale retired rows without touching unrelated memories");
    assert_eq!(store.prepare_refinements_for_cloud_sync(&[]).unwrap(), 0);
}

#[test]
fn group_deletion_is_atomic_when_one_source_has_changed() {
    let store = TelemetryStore::in_memory();
    insert(&store, "a", "First original"); insert(&store, "b", "Second original");
    let (run, action) = plan(&store, &["a", "b"], "merge");
    store.update_local_l1_memory("b", "A newer user correction").unwrap();
    assert!(delete_group(&store, &run, &action, "pending").is_err());
    assert_eq!(store.local_l1_memory_snapshot().unwrap().len(), 2);
    assert!(store.tombstoned_l1_ids().unwrap().is_empty());
    assert_eq!(store.refinement_load(&run).unwrap().view.actions[0].status, "pending");
}

#[test]
fn deleting_an_applied_group_retires_its_current_result_and_prevents_undo_resurrection() {
    let store = TelemetryStore::in_memory();
    insert(&store, "a", "First original"); insert(&store, "b", "Second original");
    let (run, action) = plan(&store, &["a", "b"], "merge");
    let applied = apply(&store, &run, &action);
    let keeper = applied.changes[0].after[0].view.id.clone();
    let stale_cloud = store.exportable_extracted_l1_memories().unwrap();
    let deleted = delete_group(&store, &run, &action, "applied").unwrap();
    assert_eq!(deleted.view.actions[0].status, "deleted");
    assert!(store.tombstoned_l1_ids().unwrap().contains(&keeper));
    store.refinement_undo(&run).unwrap();
    assert!(store.local_l1_memory_snapshot().unwrap().is_empty());
    assert_eq!(store.sync_peer_extracted_l1("offline-peer", &stale_cloud).unwrap(), (0, 0));
    assert!(store.extracted_l1_sync_snapshot().unwrap().memories.is_empty());
    assert_eq!(store.prepare_refinements_for_cloud_sync(&[]).unwrap(), 0);
}

#[test]
fn a_stale_confirmation_cannot_delete_a_newly_applied_result() {
    let store = TelemetryStore::in_memory();
    insert(&store, "a", "An original fact");
    let (run, action) = plan(&store, &["a"], "rewrite");
    let applied = apply(&store, &run, &action);
    let keeper = &applied.changes[0].after[0].view.id;
    assert!(delete_group(&store, &run, &action, "pending").is_err());
    assert_eq!(&store.local_l1_memory_snapshot().unwrap()[0].id, keeper);
    assert!(!store.tombstoned_l1_ids().unwrap().contains(keeper));
}

#[test]
fn current_pin_and_custom_memory_protection_are_preserved_by_group_deletion() {
    let store = TelemetryStore::in_memory();
    insert(&store, "a", "An original fact");
    let (run, action) = plan(&store, &["a"], "rewrite");
    let applied = apply(&store, &run, &action);
    let keeper = &applied.changes[0].after[0].view.id;
    store.conn.lock().unwrap().execute("INSERT INTO memory_importance(memory_id, pinned, updated_at) VALUES (?1, 1, 'now')", [keeper]).unwrap();
    assert!(delete_group(&store, &run, &action, "applied").is_err());
    assert_eq!(&store.local_l1_memory_snapshot().unwrap()[0].id, keeper);
    assert!(!store.tombstoned_l1_ids().unwrap().contains(keeper));
    let custom = TelemetryStore::in_memory();
    insert(&custom, "custom", "A protected user memory");
    let (run, action) = plan(&custom, &["custom"], "rewrite");
    custom.conn.lock().unwrap().execute("UPDATE local_memory_items SET user_defined = 1", []).unwrap();
    assert!(delete_group(&custom, &run, &action, "pending").is_err());
    assert_eq!(custom.local_l1_memory_snapshot().unwrap().len(), 1);
    assert!(custom.tombstoned_l1_ids().unwrap().is_empty());
}

#[test]
fn peer_only_pin_state_protects_a_memory_without_a_local_importance_score() {
    let store = TelemetryStore::in_memory();
    insert(&store, "a", "An obsolete fact");
    let (run, action) = plan(&store, &["a"], "rewrite");
    store.conn.lock().unwrap().execute(
        "INSERT INTO memory_user_state_peer(device, memory_id, pinned, pinned_updated_at)
         VALUES ('peer', 'a', 1, '2026-11-02T11:00:00Z')", [],
    ).unwrap();
    assert!(delete_group(&store, &run, &action, "pending").is_err());
    assert_eq!(store.local_l1_memory_snapshot().unwrap().len(), 1);
    assert!(store.tombstoned_l1_ids().unwrap().is_empty());
}

#[test]
fn newest_unpin_wins_over_peer_pin_across_timestamp_formats() {
    let store = TelemetryStore::in_memory();
    insert(&store, "a", "An obsolete fact");
    store.conn.lock().unwrap().execute(
        "INSERT INTO memory_importance(memory_id, pinned, pinned_updated_at, updated_at)
         VALUES ('a', 0, '2026-11-02T11:00:00Z', 'now')", [],
    ).unwrap();
    let (run, action) = plan(&store, &["a"], "rewrite");
    store.conn.lock().unwrap().execute(
        "INSERT INTO memory_user_state_peer(device, memory_id, pinned, pinned_updated_at)
         VALUES ('peer', 'a', 1, '2026-11-02T11:00:00.500+00:00')", [],
    ).unwrap();
    assert!(delete_group(&store, &run, &action, "pending").is_err());
    store.conn.lock().unwrap().execute(
        "UPDATE memory_importance SET pinned = 0, pinned_updated_at = '2026-11-02T11:00:01+00:00'", [],
    ).unwrap();
    assert_eq!(delete_group(&store, &run, &action, "pending").unwrap().view.actions[0].status, "deleted");
    assert!(store.local_l1_memory_snapshot().unwrap().is_empty());
}

#[test]
fn background_progress_cannot_restore_a_deleted_group_or_its_review_checkpoint() {
    let store = TelemetryStore::in_memory();
    insert(&store, "a", "First original"); insert(&store, "b", "Second original");
    let (run, action) = plan(&store, &["a", "b"], "merge");
    let mut worker = store.refinement_load(&run).unwrap();
    worker.view.status = "analyzing".into();
    store.refinement_save(&worker).unwrap();
    assert_eq!(delete_group(&store, &run, &action, "pending").unwrap().view.status, "analyzing");
    let checked: Vec<_> = worker.targets.iter().copied().collect();
    store.refinement_publish(&mut worker, &checked).unwrap();
    assert_eq!(worker.view.actions[0].status, "deleted");
    assert_eq!(worker.changes.len(), 1);
    assert!(store.refinement_current_candidates(&worker.inventory).unwrap().is_empty());
    let checkpoints: i64 = store.conn.lock().unwrap().query_row("SELECT count(*) FROM memory_refinement_reviews", [], |row| row.get(0)).unwrap();
    assert_eq!(checkpoints, 0);
}

#[test]
fn skipped_groups_can_be_explicitly_deleted_later() {
    let store = TelemetryStore::in_memory();
    insert(&store, "a", "An obsolete fact");
    let (run, action) = plan(&store, &["a"], "rewrite");
    store.refinement_skip(&run, vec![action.clone()]).unwrap();
    assert_eq!(delete_group(&store, &run, &action, "skipped").unwrap().view.actions[0].status, "deleted");
    assert!(store.local_l1_memory_snapshot().unwrap().is_empty());
}

fn legacy_applied_refinement(store: &TelemetryStore, id: &str) -> String {
    let (run_id, action_id) = plan(store, &[id], "rewrite");
    let mut run = store.refinement_load(&run_id).unwrap();
    let before = run.inventory.iter().find(|memory| memory.view.id == id).unwrap().clone();
    store.update_local_l1_memory(id, "Legacy refined content").unwrap();
    let after = snapshot(&store.conn.lock().unwrap(), id).unwrap().unwrap();
    run.changes.push(AppliedChange { action_id: action_id.clone(), before: vec![before], after: vec![after], removed_ids: Vec::new() });
    run.view.actions[0].status = "applied".into();
    run.view.actions[0].content = "Legacy refined content".into();
    run.view.status = "applied".into();
    store.refinement_save(&run).unwrap();
    run_id
}

#[test]
fn legacy_applied_keeper_gets_a_sync_identity_without_losing_undo() {
    let store = TelemetryStore::in_memory();
    insert(&store, "local-l1:legacy", "Original legacy fact");
    let run = legacy_applied_refinement(&store, "local-l1:legacy");
    assert_eq!(store.prepare_refinements_for_cloud_sync(&[]).unwrap(), 1);
    assert_eq!(store.prepare_refinements_for_cloud_sync(&[]).unwrap(), 0, "upgrade is idempotent");
    let upgraded = store.refinement_load(&run).unwrap();
    let keeper = upgraded.changes[0].after[0].view.id.clone();
    assert!(keeper.starts_with("local-l1:refined:"));
    assert!(upgraded.changes[0].removed_ids.contains(&"local-l1:legacy".to_string()));
    let snapshot = store.extracted_l1_sync_snapshot().unwrap();
    assert!(snapshot.deleted_ids.contains("local-l1:legacy"));
    assert_eq!(snapshot.memories[0].memory, "Legacy refined content");
    store.refinement_undo(&run).unwrap();
    let restored = store.local_l1_memory_snapshot().unwrap();
    assert_eq!(restored.len(), 1);
    assert_eq!(restored[0].memory, "Original legacy fact");
    assert!(restored[0].id.starts_with("local-l1:restored:"));
    assert!(store.tombstoned_l1_ids().unwrap().contains(&keeper));
}

#[test]
fn legacy_upgrade_cannot_republish_an_identity_already_deleted_by_the_cloud() {
    let store = TelemetryStore::in_memory();
    insert(&store, "local-l1:legacy", "Original legacy fact");
    let run = legacy_applied_refinement(&store, "local-l1:legacy");
    let deleted = vec!["local-l1:legacy".to_string()];
    assert_eq!(store.prepare_refinements_for_cloud_sync(&deleted).unwrap(), 0);
    store.apply_l1_deletion_tombstones(&deleted).unwrap();
    assert!(store.local_l1_memory_snapshot().unwrap().is_empty());
    assert!(store.extracted_l1_sync_snapshot().unwrap().memories.is_empty());
    assert!(store.refinement_load(&run).unwrap().view.actions[0].message.as_deref().unwrap().contains("云端删除"));
}

#[test]
fn legacy_sync_upgrade_preserves_later_corrections_and_keeps_undo_guarded() {
    let store = TelemetryStore::in_memory();
    insert(&store, "local-l1:legacy", "Original legacy fact");
    let run = legacy_applied_refinement(&store, "local-l1:legacy");
    store.update_local_l1_memory("local-l1:legacy", "A later manual correction").unwrap();
    assert_eq!(store.prepare_refinements_for_cloud_sync(&[]).unwrap(), 1);
    let result = store.refinement_undo(&run).unwrap();
    assert_eq!(result.view.actions[0].status, "applied");
    let active = store.local_l1_memory_snapshot().unwrap();
    assert_eq!(active.len(), 1);
    assert_eq!(active[0].memory, "A later manual correction");
}

#[test]
fn already_reverted_legacy_content_also_gets_a_publishable_version() {
    let store = TelemetryStore::in_memory();
    insert(&store, "local-l1:legacy", "Original legacy fact");
    let run_id = legacy_applied_refinement(&store, "local-l1:legacy");
    // Simulate an older release's stable-ID undo.
    store.update_local_l1_memory("local-l1:legacy", "Original legacy fact").unwrap();
    let mut run = store.refinement_load(&run_id).unwrap();
    run.view.actions[0].status = "reverted".into();
    run.view.status = "reverted".into();
    store.refinement_save(&run).unwrap();
    assert_eq!(store.prepare_refinements_for_cloud_sync(&[]).unwrap(), 1);
    let current = store.local_l1_memory_snapshot().unwrap();
    assert_eq!(current.len(), 1);
    assert_eq!(current[0].memory, "Original legacy fact");
    assert_ne!(current[0].id, "local-l1:legacy");
    assert_eq!(store.refinement_load(&run_id).unwrap().view.actions[0].status, "reverted");
    assert!(store.tombstoned_l1_ids().unwrap().contains("local-l1:legacy"));
}

#[test]
fn undoing_an_unmigrated_legacy_journal_also_retires_the_old_identity() {
    let store = TelemetryStore::in_memory();
    insert(&store, "local-l1:legacy", "Original legacy fact");
    let run = legacy_applied_refinement(&store, "local-l1:legacy");
    store.refinement_undo(&run).unwrap();
    let active = store.local_l1_memory_snapshot().unwrap();
    assert_eq!(active.len(), 1);
    assert_eq!(active[0].memory, "Original legacy fact");
    assert!(active[0].id.starts_with("local-l1:restored:"));
    assert!(store.tombstoned_l1_ids().unwrap().contains("local-l1:legacy"));
}

#[test]
fn semantic_recovery_status_survives_saved_warning_merge_and_reload() {
    let store = TelemetryStore::in_memory();
    let mut run = store.refinement_prepare(None, false).unwrap();
    run.view.status = "analyzing".into();
    run.semantic_progress = Some(crate::memory_refinement::SemanticProgress { checks: 1, degraded: 1, legacy_history: false });
    run.view.warnings = vec!["语义候选当前批：连接阶段超时".into(), "Keep this unrelated notice".into()];
    store.refinement_save(&run).unwrap();
    run.semantic_progress.as_mut().unwrap().checks = 2;
    run.view.warnings = vec!["语义候选历史：最近一批已恢复；此前覆盖不足仍待补查".into()];
    store.refinement_publish(&mut run, &[]).unwrap();
    let loaded = store.refinement_load(&run.view.id).unwrap();
    assert_eq!(loaded.semantic_progress.as_ref().unwrap().checks, 2);
    assert_eq!(loaded.semantic_progress.as_ref().unwrap().degraded, 1);
    assert!(!loaded.view.warnings.iter().any(|warning| warning.starts_with("语义候选当前批：")));
    assert!(loaded.view.warnings.iter().any(|warning| warning.contains("已恢复")));
    assert!(loaded.view.warnings.iter().any(|warning| warning == "Keep this unrelated notice"));
}

#[test]
fn background_publication_preserves_live_apply_skip_and_recovery_journal() {
    let store = TelemetryStore::in_memory();
    for id in ["a", "b", "c"] { insert(&store, id, &format!("Original {id}")); }
    let (id, first) = plan(&store, &["a"], "rewrite");
    let mut worker = store.refinement_load(&id).unwrap();
    let mut second = worker.view.actions[0].clone();
    second.id = "second".into();
    second.originals = vec![worker.inventory.iter().find(|memory| memory.view.id == "b").unwrap().view.clone()];
    worker.view.actions.push(second);
    worker.view.status = "analyzing".into();
    worker.view.activity = Some("Analyzing another group".into());
    store.refinement_save(&worker).unwrap();
    let applied = store.refinement_apply(&id, vec![RefinementSelection {
        action_id: first.clone(), content: Some("Reviewed wording for a".into()), domain_ids: None, conflict_source_id: None,
    }]).unwrap();
    assert_eq!(applied.view.status, "analyzing");
    store.refinement_skip(&id, vec!["second".into()]).unwrap();
    let mut third = worker.view.actions[0].clone();
    third.id = "third".into();
    third.originals = vec![worker.inventory.iter().find(|memory| memory.view.id == "c").unwrap().view.clone()];
    worker.view.actions.push(third);
    worker.cursor = 1;
    worker.view.processed = 1;
    worker.view.remaining = 2;
    store.refinement_publish(&mut worker, &[]).unwrap();
    assert_eq!(worker.view.actions.iter().find(|action| action.id == first).unwrap().status, "applied");
    assert_eq!(worker.view.actions.iter().find(|action| action.id == first).unwrap().content, "Reviewed wording for a");
    assert_eq!(worker.view.actions.iter().find(|action| action.id == "second").unwrap().status, "skipped");
    assert_eq!(worker.view.actions.iter().find(|action| action.id == "third").unwrap().status, "pending");
    assert_eq!(worker.changes.len(), 1);
    let notice = store.refinement_append_warnings(&id, vec!["Index refresh queued".into()]).unwrap();
    assert_eq!(notice.status, "analyzing");
    assert_eq!(notice.processed, 1);
    assert_eq!(store.refinement_load(&id).unwrap().cursor, 1);
    worker.view.status = "ready".into();
    worker.view.processed = 3;
    worker.view.remaining = 0;
    store.refinement_publish(&mut worker, &[]).unwrap();
    assert!(worker.view.activity.is_none());
    store.refinement_undo(&id).unwrap();
    let restored = store.local_l1_memory_snapshot().unwrap();
    assert!(restored.iter().any(|memory| memory.memory == "Original a"));
    assert!(restored.iter().all(|memory| memory.id != "a"));
}

#[test]
fn newly_generated_proposals_are_stale_if_sources_change_during_the_request() {
    let store = TelemetryStore::in_memory();
    insert(&store, "a", "Original fact");
    let (id, _) = plan(&store, &["a"], "rewrite");
    let mut in_flight = store.refinement_load(&id).unwrap();
    let mut published = in_flight.clone();
    published.view.actions.clear();
    published.view.status = "analyzing".into();
    store.refinement_save(&published).unwrap();
    store.update_local_l1_memory("a", "A newer manual correction").unwrap();
    in_flight.view.status = "analyzing".into();
    store.refinement_publish(&mut in_flight, &[]).unwrap();
    assert_eq!(in_flight.view.actions[0].status, "stale");
    assert!(store.refinement_current_candidates(&in_flight.inventory).unwrap().is_empty());
    assert_eq!(store.local_l1_memory_snapshot().unwrap()[0].memory, "A newer manual correction");
}

#[test]
fn recovered_length_notice_is_not_a_persistent_review_warning() {
    let store = TelemetryStore::in_memory();
    let mut run = store.refinement_prepare(None, false).unwrap();
    run.view.warnings = vec![
        "模型达到输出长度上限，已改用双条/单条与较短来源片段有限重试；成功小组会单独保存".into(),
        "A separate unresolved source issue".into(),
    ];
    run.view.activity = Some("Old retry activity".into());
    store.refinement_save(&run).unwrap();
    let view = store.refinement_view(&run.view.id).unwrap();
    assert_eq!(view.warnings, vec!["A separate unresolved source issue"]);
    assert!(view.activity.is_none());
    assert_eq!(store.refinement_history(None).unwrap()[0].warnings.len(), 1);
    assert_eq!(store.refinement_load(&run.view.id).unwrap().view.warnings.len(), 1);
}

#[test]
fn stale_plan_cannot_overwrite_a_later_user_edit() {
    let store = TelemetryStore::in_memory();
    insert(&store, "local-l1:a", "Original project fact");
    let (run, action) = plan(&store, &["local-l1:a"], "rewrite");
    store.update_local_l1_memory("local-l1:a", "User's later correction").unwrap();
    let result = apply(&store, &run, &action);
    assert_eq!(result.view.actions[0].status, "stale");
    assert_eq!(store.local_l1_memory_snapshot().unwrap()[0].memory, "User's later correction");
}

#[test]
fn archive_undo_preserves_new_memories_and_uses_a_new_sync_identity() {
    let store = TelemetryStore::in_memory();
    insert(&store, "local-l1:a", "Original project fact");
    let (run, action) = plan(&store, &["local-l1:a"], "archive");
    apply(&store, &run, &action);
    assert!(store.local_l1_memory_snapshot().unwrap().is_empty());
    assert!(store.tombstoned_l1_ids().unwrap().contains("local-l1:a"));
    insert(&store, "local-l1:new", "A later unrelated memory");
    let restored = store.refinement_undo(&run).unwrap();
    assert_eq!(restored.view.actions[0].status, "reverted");
    let memories = store.local_l1_memory_snapshot().unwrap();
    assert_eq!(memories.len(), 2);
    assert!(memories.iter().any(|memory| memory.id == "local-l1:new"));
    assert!(memories.iter().any(|memory| memory.id != "local-l1:a" && memory.memory == "Original project fact"));
    // Old remote tombstones cannot erase the newly restored identity.
    store.apply_l1_deletion_tombstone("local-l1:a").unwrap();
    assert_eq!(store.local_l1_memory_snapshot().unwrap().len(), 2);
    store.refinement_undo(&run).unwrap();
    assert_eq!(store.local_l1_memory_snapshot().unwrap().len(), 2, "undo is idempotent");
}

#[test]
fn merge_preserves_independent_sources_and_restores_removed_members() {
    let store = TelemetryStore::in_memory();
    insert(&store, "local-l1:a", "Project uses the same endpoint");
    insert(&store, "local-l1:b", "The same project endpoint is confirmed");
    let (run, action) = plan(&store, &["local-l1:a", "local-l1:b"], "merge");
    let applied = apply(&store, &run, &action);
    assert_eq!(store.local_l1_memory_snapshot().unwrap().len(), 1);
    let keeper = &applied.changes[0].after[0].view.id;
    assert_ne!(keeper.as_str(), "local-l1:a");
    let support = store.refinement_support(&[keeper.clone()]).unwrap();
    assert_eq!(support[keeper].0.len(), 2);
    assert_eq!(support[keeper].1.len(), 1);
    store.refinement_undo(&run).unwrap();
    let memories = store.local_l1_memory_snapshot().unwrap();
    assert_eq!(memories.len(), 2);
    assert!(memories.iter().any(|memory| memory.memory == "Project uses the same endpoint"));
    assert!(memories.iter().any(|memory| memory.memory == "The same project endpoint is confirmed"));
}

#[test]
fn undo_refuses_to_replace_content_edited_after_apply() {
    let store = TelemetryStore::in_memory();
    insert(&store, "local-l1:a", "Original project fact");
    let (run, action) = plan(&store, &["local-l1:a"], "rewrite");
    let applied = apply(&store, &run, &action);
    let keeper = &applied.changes[0].after[0].view.id;
    store.update_local_l1_memory(keeper, "Later manual correction").unwrap();
    let result = store.refinement_undo(&run).unwrap();
    assert_eq!(result.view.actions[0].status, "applied");
    assert!(result.view.actions[0].message.is_some());
    assert_eq!(store.local_l1_memory_snapshot().unwrap()[0].memory, "Later manual correction");
}

#[test]
fn undo_keeps_post_refinement_pin_and_recall_state_on_the_restored_version() {
    let store = TelemetryStore::in_memory();
    insert(&store, "local-l1:a", "Original project fact");
    let (run, action) = plan(&store, &["local-l1:a"], "rewrite");
    let applied = apply(&store, &run, &action);
    let keeper = &applied.changes[0].after[0].view.id;
    let now = chrono::Utc::now().to_rfc3339();
    store.conn.lock().unwrap().execute(
        "INSERT INTO memory_importance(memory_id, pinned, recall_count, pinned_updated_at, updated_at)
         VALUES (?1, 1, 7, ?2, ?2)", params![keeper, now],
    ).unwrap();
    store.refinement_undo(&run).unwrap();
    let restored = store.local_l1_memory_snapshot().unwrap();
    let state = store.importance_records(&[restored[0].id.clone()]).unwrap();
    assert!(state[0].pinned);
    assert_eq!(state[0].recall_count, 7);
    assert!(store.tombstoned_l1_ids().unwrap().contains(keeper));
}

#[test]
fn protected_memory_is_excluded_and_cannot_be_forced_into_apply() {
    let store = TelemetryStore::in_memory();
    insert(&store, "local-l1:a", "Pinned fact");
    let (run, action) = plan(&store, &["local-l1:a"], "archive");
    store.conn.lock().unwrap().execute(
        "INSERT INTO memory_importance(memory_id, pinned, updated_at) VALUES ('local-l1:a', 1, 'now')", [],
    ).unwrap();
    let protected = store.refinement_prepare(None, false).unwrap();
    assert_eq!(protected.view.total, 0);
    assert_eq!(protected.view.protected, 1);
    assert_eq!(apply(&store, &run, &action).view.actions[0].status, "stale");
    assert_eq!(store.local_l1_memory_snapshot().unwrap().len(), 1);
}

#[test]
fn refining_a_peer_mirror_publishes_a_new_keeper_and_undo_removes_it() {
    let store = TelemetryStore::in_memory();
    insert(&store, "local-l1:peer-a", "Peer project fact");
    insert(&store, "local-l1:peer-b", "Supporting peer project fact");
    store.conn.lock().unwrap().execute(
        "UPDATE local_memory_items SET origin_device = 'peer-device'", [],
    ).unwrap();
    let (run, action) = plan(&store, &["local-l1:peer-a", "local-l1:peer-b"], "merge");
    let applied = apply(&store, &run, &action);
    let keeper = &applied.changes[0].after[0].view.id;
    assert!(keeper.starts_with("local-l1:refined:"));
    assert_eq!(store.local_l1_memory_snapshot().unwrap().len(), 1);
    let deleted = store.tombstoned_l1_ids().unwrap();
    assert!(deleted.contains("local-l1:peer-a") && deleted.contains("local-l1:peer-b"));
    store.refinement_undo(&run).unwrap();
    let memories = store.local_l1_memory_snapshot().unwrap();
    assert_eq!(memories.len(), 2);
    assert!(memories.iter().all(|memory| &memory.id != keeper));
    assert!(store.tombstoned_l1_ids().unwrap().contains(keeper));
}

#[test]
fn domain_review_does_not_claim_global_comparison_coverage() {
    let store = TelemetryStore::in_memory();
    insert(&store, "local-l1:a", "A project-specific fact");
    {
        let conn = store.conn.lock().unwrap();
        conn.execute("INSERT INTO memory_domains(id, name, kind, created_at, updated_at)
            VALUES ('custom:scope', 'Scope test', 'custom', 'now', 'now')", []).unwrap();
        conn.execute("INSERT INTO local_memory_domains(memory_id, domain_id, weight, source, created_at, updated_at)
            VALUES ('local-l1:a', 'custom:scope', 1, 'auto', 'now', 'now')", []).unwrap();
    }
    let run = store.refinement_prepare(Some("custom:scope".into()), true).unwrap();
    store.refinement_checkpoint(&run, &[0]).unwrap();
    assert_eq!(store.refinement_prepare(Some("custom:scope".into()), true).unwrap().view.total, 0);
    assert_eq!(store.refinement_prepare(None, true).unwrap().view.total, 1);
}

#[test]
fn checkpoints_keep_inventory_separate_and_history_reads_public_views() {
    let store = TelemetryStore::in_memory();
    insert(&store, "local-l1:a", "A source fact");
    let run = store.refinement_prepare(None, false).unwrap();
    store.refinement_save(&run).unwrap();
    let loaded = store.refinement_load(&run.view.id).unwrap();
    assert_eq!(loaded.inventory.len(), 1);
    assert_eq!(loaded.review_version, run.review_version);
    assert_eq!(loaded.task_domains, run.task_domains);
    assert_eq!(loaded.task_targets, run.task_targets);
    let payload: String = store.conn.lock().unwrap().query_row(
        "SELECT payload FROM memory_refinement_runs WHERE id = ?1", [&run.view.id], |row| row.get(0),
    ).unwrap();
    let payload: Value = serde_json::from_str(&payload).unwrap();
    assert!(payload.get("inventory").is_none());
    assert_eq!(store.refinement_history(None).unwrap()[0].id, run.view.id);
}

#[test]
fn incremental_tasks_count_each_new_target_once_with_old_context() {
    let store = TelemetryStore::in_memory();
    for index in 0..12 {
        let id = format!("local-l1:{index}");
        insert(&store, &id, "Shared project endpoint is documented");
        tag(&store, &id, &["custom:review"]);
    }
    let mut run = store.refinement_prepare(None, true).unwrap();
    let last_tasks = memory_refinement::last_target_tasks(&run);
    while run.cursor < run.tasks.len() {
        let checked = memory_refinement::finish_batch(&mut run, &last_tasks, true);
        store.refinement_checkpoint(&run, &checked).unwrap();
    }
    assert_eq!(run.view.processed, 12);
    let incremental = store.refinement_prepare(None, true).unwrap();
    assert_eq!(incremental.view.total, 0);
    insert(&store, "local-l1:new", "Shared project endpoint is documented again");
    tag(&store, "local-l1:new", &["custom:review"]);
    let incremental = store.refinement_prepare(None, true).unwrap();
    assert_eq!(incremental.view.total, 1);
    assert_eq!(incremental.tasks.len(), 1);
    assert!(incremental.tasks[0].len() > 1, "old memory is retained as comparison context");
}

#[test]
fn failed_batch_keeps_remaining_work_while_later_success_is_saved() {
    let store = TelemetryStore::in_memory();
    for (id, domain) in [("a", "custom:a"), ("b", "custom:b")] {
        insert(&store, id, "A scoped review fact");
        tag(&store, id, &[domain]);
    }
    let mut run = store.refinement_prepare(None, true).unwrap();
    assert_eq!(run.tasks.len(), 2);
    let last_tasks = memory_refinement::last_target_tasks(&run);
    memory_refinement::record_task_result(&mut run, false);
    let checked = memory_refinement::finish_batch(&mut run, &last_tasks, true);
    assert!(checked.is_empty());
    store.refinement_checkpoint(&run, &checked).unwrap();
    memory_refinement::record_task_result(&mut run, true);
    let checked = memory_refinement::finish_batch(&mut run, &last_tasks, true);
    store.refinement_checkpoint(&run, &checked).unwrap();
    assert_eq!(run.view.processed, 1);
    assert_eq!(run.view.remaining, 1);
    let mut resumed = store.refinement_load(&run.view.id).unwrap();
    assert_eq!(resumed.retry_tasks, HashSet::from([0]));
    assert_eq!(resumed.completed_tasks, HashSet::from([1]));
    resumed.cursor = *resumed.retry_tasks.iter().min().unwrap();
    let last_tasks = memory_refinement::last_target_tasks(&resumed);
    memory_refinement::record_task_result(&mut resumed, true);
    let checked = memory_refinement::finish_batch(&mut resumed, &last_tasks, true);
    store.refinement_checkpoint(&resumed, &checked).unwrap();
    assert!(resumed.completed_tasks.contains(&resumed.cursor));
    let checked = memory_refinement::finish_batch(&mut resumed, &last_tasks, true);
    store.refinement_checkpoint(&resumed, &checked).unwrap();
    assert!(resumed.retry_tasks.is_empty());
    assert_eq!(resumed.view.processed, 2);
    assert_eq!(resumed.view.remaining, 0);
    assert_eq!(store.refinement_prepare(None, true).unwrap().view.total, 0);
}

#[test]
fn failed_earlier_label_blocks_completion_until_retry_succeeds() {
    let store = TelemetryStore::in_memory();
    insert(&store, "a", "A fact shared by two labels");
    tag(&store, "a", &["custom:a", "custom:b"]);
    let mut run = store.refinement_prepare(None, true).unwrap();
    let last_tasks = memory_refinement::last_target_tasks(&run);
    memory_refinement::record_task_result(&mut run, false);
    assert!(memory_refinement::finish_batch(&mut run, &last_tasks, true).is_empty());
    memory_refinement::record_task_result(&mut run, true);
    assert!(memory_refinement::finish_batch(&mut run, &last_tasks, true).is_empty());
    assert_eq!(run.view.remaining, 1);
    run.cursor = 0;
    memory_refinement::record_task_result(&mut run, true);
    assert!(memory_refinement::finish_batch(&mut run, &last_tasks, true).is_empty());
    assert!(run.completed_tasks.contains(&run.cursor));
    let checked = memory_refinement::finish_batch(&mut run, &last_tasks, true);
    assert_eq!(checked.len(), 1);
    assert_eq!(run.view.remaining, 0);
}

#[test]
fn old_runs_without_label_inputs_remain_readable_for_review() {
    let store = TelemetryStore::in_memory();
    insert(&store, "old", "An existing run's source fact");
    let run = store.refinement_prepare(None, false).unwrap();
    store.refinement_save(&run).unwrap();
    {
        let conn = store.conn.lock().unwrap();
        let payload: String = conn.query_row("SELECT payload FROM memory_refinement_inputs WHERE run_id = ?1",
            [&run.view.id], |row| row.get(0)).unwrap();
        let mut payload: Value = serde_json::from_str(&payload).unwrap();
        payload.as_object_mut().unwrap().remove("task_domains");
        payload.as_object_mut().unwrap().remove("task_targets");
        conn.execute("UPDATE memory_refinement_inputs SET payload = ?1 WHERE run_id = ?2",
            params![serde_json::to_string(&payload).unwrap(), run.view.id]).unwrap();
    }
    let loaded = store.refinement_load(&run.view.id).unwrap();
    assert_eq!(loaded.view.id, run.view.id);
    assert!(!loaded.tasks.is_empty());
    assert!(loaded.task_domains.is_empty());
    assert!(loaded.task_targets.is_empty());
    assert_eq!(store.refinement_history(None).unwrap().len(), 1);
}

#[test]
fn global_tasks_stay_in_one_label_and_keep_multilabel_memberships() {
    let store = TelemetryStore::in_memory();
    for id in ["a", "b", "both", "untagged", "fallback"] {
        insert(&store, &format!("local-l1:{id}"), "Identical words must not bypass label boundaries");
    }
    tag(&store, "local-l1:a", &["custom:a"]);
    tag(&store, "local-l1:b", &["custom:b"]);
    tag(&store, "local-l1:both", &["custom:a", "custom:b", "unclassified"]);
    tag(&store, "local-l1:fallback", &["unclassified"]);
    let run = store.refinement_prepare(None, false).unwrap();
    assert_eq!(run.view.total, 5);
    for (indices, domain) in run.tasks.iter().zip(&run.task_domains) {
        assert!(indices.iter().all(|&index| memory_refinement::in_domain(&run.inventory[index].view, domain)));
        if domain == "unclassified" { assert_eq!(indices.len(), 1); }
    }
    let both = run.inventory.iter().position(|memory| memory.view.id == "local-l1:both").unwrap();
    assert_eq!(run.task_targets.iter().filter(|targets| targets.contains(&both)).count(), 2);
    assert!(run.tasks.iter().zip(&run.task_domains).all(|(task, domain)|
        domain != "unclassified" || !task.contains(&both)));
}

#[test]
fn selected_label_is_the_only_comparison_group() {
    let store = TelemetryStore::in_memory();
    for id in ["a", "b", "both"] { insert(&store, id, "A shared project fact"); }
    tag(&store, "a", &["custom:a"]);
    tag(&store, "b", &["custom:b"]);
    tag(&store, "both", &["custom:a", "custom:b"]);
    let run = store.refinement_prepare(Some("custom:a".into()), false).unwrap();
    assert_eq!(run.view.total, 2);
    assert!(run.task_domains.iter().all(|domain| domain == "custom:a"));
    assert!(run.inventory.iter().all(|memory| memory.view.id != "b"));
}

#[test]
fn unclassified_scope_includes_missing_labels_but_excludes_classified_fallbacks() {
    let store = TelemetryStore::in_memory();
    for id in ["missing", "fallback", "classified"] { insert(&store, id, "Unclassified content"); }
    tag(&store, "fallback", &["unclassified"]);
    tag(&store, "classified", &["unclassified", "custom:a"]);
    let run = store.refinement_prepare(Some("unclassified".into()), false).unwrap();
    assert_eq!(run.view.total, 2);
    assert!(run.tasks.iter().all(|task| task.len() == 1));
    assert!(run.inventory.iter().all(|memory| memory.view.id != "classified"));
}

#[test]
fn multilabel_progress_and_resume_wait_for_every_label() {
    let store = TelemetryStore::in_memory();
    for id in ["a", "b"] {
        insert(&store, id, "One fact in two labels");
        tag(&store, id, &["custom:a", "custom:b"]);
    }
    let mut run = store.refinement_prepare(None, true).unwrap();
    let last_tasks = memory_refinement::last_target_tasks(&run);
    let checked = memory_refinement::finish_batch(&mut run, &last_tasks, true);
    assert!(checked.is_empty());
    assert_eq!(run.view.processed, 0);
    assert_eq!(run.view.remaining, 2);
    store.refinement_checkpoint(&run, &checked).unwrap();
    let mut resumed = store.refinement_load(&run.view.id).unwrap();
    let last_tasks = memory_refinement::last_target_tasks(&resumed);
    while resumed.cursor < resumed.tasks.len() {
        let checked = memory_refinement::finish_batch(&mut resumed, &last_tasks, true);
        store.refinement_checkpoint(&resumed, &checked).unwrap();
    }
    assert_eq!(resumed.view.processed, 2);
    assert_eq!(resumed.view.remaining, 0);
    assert_eq!(store.refinement_prepare(None, true).unwrap().view.total, 0);
}

#[test]
fn successful_second_label_does_not_hide_degraded_first_label() {
    let store = TelemetryStore::in_memory();
    insert(&store, "a", "A fact with two relevant labels");
    tag(&store, "a", &["custom:a", "custom:b"]);
    let mut run = store.refinement_prepare(None, true).unwrap();
    let last_tasks = memory_refinement::last_target_tasks(&run);
    let checked = memory_refinement::finish_batch(&mut run, &last_tasks, false);
    store.refinement_checkpoint(&run, &checked).unwrap();
    let mut resumed = store.refinement_load(&run.view.id).unwrap();
    let last_tasks = memory_refinement::last_target_tasks(&resumed);
    let checked = memory_refinement::finish_batch(&mut resumed, &last_tasks, true);
    assert!(checked.is_empty());
    store.refinement_checkpoint(&resumed, &checked).unwrap();
    assert_eq!(resumed.view.remaining, 0);
    assert_eq!(store.refinement_prepare(None, true).unwrap().view.total, 1);
}
