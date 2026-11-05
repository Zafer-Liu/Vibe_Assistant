use super::*;

fn domain_link(id: &str, name: &str) -> CloudMemoryDomainLink {
    CloudMemoryDomainLink {
        domain: CloudMemoryDomain {
            id: id.into(), name: name.into(), kind: "custom".into(),
            description: format!("{name}相关记忆"), aliases: vec!["topic-alias".into()],
            enabled: true,
            created_at: "2026-11-01T00:00:00Z".into(),
            updated_at: "2026-11-01T00:00:00Z".into(),
        },
        weight: 0.8, source: "backfill".into(),
    }
}

fn cloud_record(id: &str, domains: Option<Vec<CloudMemoryDomainLink>>) -> CloudL1Memory {
    CloudL1Memory {
        item: LocalMemorySnapshot {
            id: id.into(), source_event_key: format!("event:{id}"), ordinal: 0,
            memory: "需要保留分类的会话摘要".into(), memory_type: "summary".into(),
            durability: "session".into(), source: "claude_code".into(),
            session_id: "source-session".into(), user_defined: false,
            event_time: None,
            created_at: "2026-11-01T00:00:00Z".into(),
            updated_at: "2026-11-01T00:00:00Z".into(),
        },
        domains,
        classified_at: None,
    }
}

#[test]
fn cloud_tags_backfill_existing_mirrors_and_preserve_domain_metadata() {
    let store = TelemetryStore::in_memory();
    let mut record = cloud_record("local-l1:tag-backfill", None);
    store.sync_peer_extracted_l1("source-device", &[record.clone()]).unwrap();
    assert!(store.cloud_l1_domains(&record.id).unwrap().is_empty());
    let link = domain_link("custom:sync-topic", "同步分类");
    record.domains = Some(vec![link.clone()]);
    store.sync_peer_extracted_l1("source-device", &[record.clone()]).unwrap();
    assert_eq!(store.cloud_l1_domains(&record.id).unwrap(), vec![link]);
    let topic = store.memory_domains().unwrap().into_iter()
        .find(|domain| domain.id == "custom:sync-topic").unwrap();
    assert_eq!(topic.name, "同步分类");
    assert_eq!(topic.memory_count, 1);
    assert!(store.completed_memory_domain_classification_ids().unwrap().contains(&record.id));
    assert!(store.exportable_extracted_l1_memories().unwrap().is_empty(), "对端标签和记忆不得二次广播");
}

#[test]
fn legacy_missing_tags_preserve_assignments_but_empty_tags_remove_them() {
    let store = TelemetryStore::in_memory();
    let mut record = cloud_record("local-l1:tag-clear", Some(vec![domain_link("custom:clear", "可移除分类")]));
    store.sync_peer_extracted_l1("source-device", &[record.clone()]).unwrap();
    record.domains = None;
    store.sync_peer_extracted_l1("source-device", &[record.clone()]).unwrap();
    assert_eq!(store.cloud_l1_domains(&record.id).unwrap().len(), 1);
    record.domains = Some(Vec::new());
    store.sync_peer_extracted_l1("source-device", &[record.clone()]).unwrap();
    assert!(store.cloud_l1_domains(&record.id).unwrap().is_empty());
}

#[test]
fn local_tags_survive_an_id_collision_with_a_remote_memory() {
    let store = TelemetryStore::in_memory();
    let local_link = domain_link("custom:local-owner", "本机分类");
    let mut record = cloud_record("local-user:collision", Some(vec![domain_link("custom:remote-owner", "远端分类")]));
    store.upsert_synced_l1_memory_with_domains(
        &record.id, "本机手写记忆", "fact", &record.updated_at, Some(&[local_link.clone()]),
    ).unwrap();
    record.item.memory = "不应覆盖本机的远端记忆".into();
    store.sync_peer_extracted_l1("remote-device", &[record.clone()]).unwrap();
    assert_eq!(store.cloud_l1_domains(&record.id).unwrap(), vec![local_link]);
    assert_eq!(store.find_user_defined_l1(&record.id).unwrap().unwrap().memory, "本机手写记忆");
}

#[test]
fn invalid_cloud_tags_roll_back_memory_and_registry_writes() {
    let store = TelemetryStore::in_memory();
    let valid = domain_link("custom:rollback-first", "不应残留的分类");
    let mut invalid = domain_link("custom:rollback-second", "非法分类");
    invalid.source = "unknown-source".into();
    let record = cloud_record("local-l1:invalid-tags", Some(vec![valid, invalid]));
    assert!(store.sync_peer_extracted_l1("source-device", &[record]).is_err());
    assert!(store.local_l1_memory_snapshot().unwrap().is_empty());
    assert!(!store.memory_domains().unwrap().iter().any(|domain| domain.id.starts_with("custom:rollback-")));
}

#[test]
fn exporting_tag_only_changes_updates_the_cloud_payload() {
    let store = TelemetryStore::in_memory();
    let record = cloud_record("local-l1:export-tags", None);
    store.sync_peer_extracted_l1("source-device", &[record.clone()]).unwrap();
    {
        let conn = store.conn.lock().unwrap();
        conn.execute("UPDATE local_memory_items SET origin_device = NULL WHERE id = ?1", [&record.id]).unwrap();
    }
    let before = store.exportable_extracted_l1_memories().unwrap();
    let link = domain_link("custom:export", "导出分类");
    {
        let mut conn = store.conn.lock().unwrap();
        let transaction = conn.transaction().unwrap();
        apply_cloud_domains(&transaction, &record.id, Some(&[link.clone()]), &record.memory, &record.memory_type, &record.durability).unwrap();
        transaction.commit().unwrap();
    }
    let after = store.exportable_extracted_l1_memories().unwrap();
    assert_ne!(serde_json::to_vec(&before).unwrap(), serde_json::to_vec(&after).unwrap());
    assert_eq!(before[0].item.memory, after[0].item.memory);
    assert_eq!(after[0].domains, Some(vec![link]));
    let legacy = serde_json::to_vec(&record.item).unwrap();
    let decoded: CloudL1Memory = serde_json::from_slice(&legacy).unwrap();
    assert!(decoded.domains.is_none());
}

#[test]
fn identical_untagged_local_record_can_receive_missing_remote_tags() {
    let store = TelemetryStore::in_memory();
    let mut record = cloud_record("local-l1:identical", None);
    store.sync_peer_extracted_l1("initial-device", &[record.clone()]).unwrap();
    {
        let conn = store.conn.lock().unwrap();
        conn.execute("UPDATE local_memory_items SET origin_device = NULL WHERE id = ?1", [&record.id]).unwrap();
    }
    let link = domain_link("custom:identical", "同源分类");
    record.domains = Some(vec![link.clone()]);
    store.sync_peer_extracted_l1("second-device", &[record.clone()]).unwrap();
    assert_eq!(store.cloud_l1_domains(&record.id).unwrap(), vec![link]);
    assert_eq!(store.exportable_extracted_l1_memories().unwrap().len(), 1, "本机记录不应被转为镜像");
}

#[test]
fn builtin_tags_with_sqlite_timestamps_are_accepted() {
    let store = TelemetryStore::in_memory();
    let mut link = domain_link("work", "工作");
    link.domain.kind = "builtin".into();
    link.domain.created_at = "2026-11-01 00:00:00".into();
    link.domain.updated_at = "2026-11-01 00:00:00".into();
    let record = cloud_record("local-l1:builtin", Some(vec![link]));
    store.sync_peer_extracted_l1("source-device", &[record.clone()]).unwrap();
    assert_eq!(store.cloud_l1_domains(&record.id).unwrap()[0].domain.id, "work");
}

#[test]
fn catalog_sync_includes_unused_disabled_custom_domains() {
    let source = TelemetryStore::in_memory();
    let receiver = TelemetryStore::in_memory();
    let label = source.save_memory_domain(None, "空标签", "尚未用于记忆", &["别名".into()], false).unwrap();
    receiver.merge_cloud_domain_catalog(&source.cloud_domain_catalog().unwrap()).unwrap();
    let received = receiver.memory_domains().unwrap().into_iter().find(|item| item.id == label.id).unwrap();
    assert_eq!(received.name, "空标签");
    assert_eq!(received.description, "尚未用于记忆");
    assert_eq!(received.aliases, vec!["别名".to_string()]);
    assert!(!received.enabled);
    assert_eq!(received.memory_count, 0);
}

#[test]
fn catalog_deletion_prevents_resurrection_from_older_memory_tags() {
    let source = TelemetryStore::in_memory();
    let receiver = TelemetryStore::in_memory();
    let link = domain_link("custom:deleted-label", "已删除分类");
    let record = cloud_record("local-l1:deleted-label", Some(vec![link.clone()]));
    source.merge_cloud_domain_catalog(&CloudDomainCatalog {
        domains: vec![link.domain.clone()], tombstones: Vec::new(),
    }).unwrap();
    receiver.sync_peer_extracted_l1("source-device", &[record.clone()]).unwrap();
    source.delete_memory_domain(&link.domain.id).unwrap();
    receiver.merge_cloud_domain_catalog(&source.cloud_domain_catalog().unwrap()).unwrap();
    assert!(receiver.cloud_l1_domains(&record.id).unwrap().is_empty());
    receiver.sync_peer_extracted_l1("source-device", &[record.clone()]).unwrap();
    assert!(receiver.cloud_l1_domains(&record.id).unwrap().is_empty());
    assert!(!receiver.memory_domains().unwrap().iter().any(|item| item.id == link.domain.id));
    assert_eq!(receiver.local_l1_memory_snapshot().unwrap().len(), 1, "删除分类不得删除记忆");
    assert!(receiver.cloud_domain_catalog().unwrap().tombstones.iter().any(|item| item.domain_id == link.domain.id));
}

#[test]
fn simultaneous_domain_renames_converge_in_either_merge_order() {
    let a = TelemetryStore::in_memory();
    let b = TelemetryStore::in_memory();
    let first = domain_link("custom:rename", "Alpha").domain;
    let mut second = first.clone();
    second.name = "Beta".into();
    let catalog_a = CloudDomainCatalog { domains: vec![first], tombstones: Vec::new() };
    let catalog_b = CloudDomainCatalog { domains: vec![second], tombstones: Vec::new() };
    a.merge_cloud_domain_catalog(&catalog_a).unwrap();
    a.merge_cloud_domain_catalog(&catalog_b).unwrap();
    b.merge_cloud_domain_catalog(&catalog_b).unwrap();
    b.merge_cloud_domain_catalog(&catalog_a).unwrap();
    let result_a = a.memory_domains().unwrap().into_iter().find(|item| item.id == "custom:rename").unwrap();
    let result_b = b.memory_domains().unwrap().into_iter().find(|item| item.id == "custom:rename").unwrap();
    assert_eq!(result_a.name, result_b.name);
    assert_eq!(result_a.name, "Beta");
}

#[test]
fn receiver_retag_publishes_and_survives_source_refresh() {
    let source = TelemetryStore::in_memory();
    let receiver = TelemetryStore::in_memory();
    let link = domain_link("custom:origin", "来源分类");
    let record = cloud_record("local-l1:retagged", Some(vec![link.clone()]));
    source.sync_peer_extracted_l1("unused", &[record.clone()]).unwrap();
    {
        let conn = source.conn.lock().unwrap();
        conn.execute("UPDATE local_memory_items SET origin_device = NULL WHERE id = ?1", [&record.id]).unwrap();
    }
    // 来源端导出（带检查点），接收端镜像。
    let exported = source.exportable_extracted_l1_memories().unwrap();
    assert_eq!(exported.len(), 1);
    receiver.sync_peer_extracted_l1("source-device", &exported).unwrap();
    assert_eq!(receiver.cloud_l1_domains(&record.id).unwrap(), vec![link.clone()]);

    // 接收端显式重分类：登记回传，且不被来源端旧聚合覆盖。
    let new_link = domain_link("custom:retagged", "接收端分类");
    receiver
        .merge_cloud_domain_catalog(&CloudDomainCatalog {
            domains: vec![new_link.domain.clone()],
            tombstones: Vec::new(),
        })
        .unwrap();
    receiver
        .save_memory_domain_classification(
            &record.item,
            &[crate::telemetry_store::MemoryDomainAssignment {
                domain_id: new_link.domain.id.clone(),
                weight: 0.9,
            }],
        )
        .unwrap();
    assert_eq!(receiver.cloud_tag_overrides().unwrap().len(), 1);
    receiver.sync_peer_extracted_l1("source-device", &exported).unwrap();
    let tags = receiver.cloud_l1_domains(&record.id).unwrap();
    assert_eq!(tags.len(), 1);
    assert_eq!(tags[0].domain.id, new_link.domain.id, "接收端重分类不应被来源端旧聚合覆盖");

    // 来源端应用回传后收敛到同一分类。
    source.apply_peer_tag_overrides(&receiver.cloud_tag_overrides().unwrap()).unwrap();
    let source_tags = source.cloud_l1_domains(&record.id).unwrap();
    assert_eq!(source_tags.len(), 1);
    assert_eq!(source_tags[0].domain.id, new_link.domain.id);
}

#[test]
fn user_state_merges_pins_and_recalls_across_devices() {
    let a = TelemetryStore::in_memory();
    let b = TelemetryStore::in_memory();
    a.set_memory_pinned("mem-1", true).unwrap();
    a.record_memory_recall(&["mem-1".to_string()]).unwrap();
    b.record_memory_recall(&["mem-1".to_string(), "mem-1".to_string()]).unwrap();
    let state_a = a.cloud_user_state().unwrap();
    let state_b = b.cloud_user_state().unwrap();
    assert_eq!(state_a.len(), 1);
    a.apply_peer_user_state("device-b", &state_b).unwrap();
    b.apply_peer_user_state("device-a", &state_a).unwrap();
    let seen_a = a.importance_records(&["mem-1".to_string()]).unwrap().remove(0);
    let seen_b = b.importance_records(&["mem-1".to_string()]).unwrap().remove(0);
    assert_eq!(seen_a.recall_count, 3, "召回按设备累加");
    assert_eq!(seen_b.recall_count, 3);
    assert!(seen_a.pinned && seen_b.pinned, "置顶跨设备可见");
    b.set_memory_pinned("mem-1", false).unwrap();
    let state_b2 = b.cloud_user_state().unwrap();
    a.apply_peer_user_state("device-b", &state_b2).unwrap();
    let seen_a2 = a.importance_records(&["mem-1".to_string()]).unwrap().remove(0);
    assert!(!seen_a2.pinned, "较新的取消置顶应覆盖旧置顶");
}

#[test]
fn layer_provenance_travels_with_published_content() {
    let store = TelemetryStore::in_memory();
    store
        .save_memory_layer_document(
            "l2", "工作记忆正文", "published", "l1",
            &["local-l1:src-1".to_string()],
            Some("2026-10-01T00:00:00Z".into()),
            Some("2026-10-31T00:00:00Z".into()),
        )
        .unwrap();
    let provenance = store.cloud_layer_provenance("l2").unwrap().unwrap();
    assert_eq!(provenance.source_count, 1);
    assert_eq!(provenance.sources.len(), 1);
    assert_eq!(provenance.sources[0].id, "local-l1:src-1");
    let receiver = TelemetryStore::in_memory();
    receiver
        .upsert_published_layer_content("l2", "工作记忆正文")
        .unwrap();
    receiver.apply_layer_provenance("l2", &provenance).unwrap();
    let received = receiver.cloud_layer_provenance("l2").unwrap().unwrap();
    assert_eq!(received.sources, provenance.sources);
    assert_eq!(received.window_start, provenance.window_start);
}

#[test]
fn a_newer_submillisecond_rename_is_not_lost() {
    let store = TelemetryStore::in_memory();
    let mut old = domain_link("custom:precise-rename", "Zeta").domain;
    old.updated_at = "2026-11-01T00:00:00.000100Z".into();
    let mut new = old.clone();
    new.name = "Alpha".into();
    new.updated_at = "2026-11-01T00:00:00.000200Z".into();
    store.merge_cloud_domain_catalog(&CloudDomainCatalog { domains: vec![old], tombstones: Vec::new() }).unwrap();
    store.merge_cloud_domain_catalog(&CloudDomainCatalog { domains: vec![new], tombstones: Vec::new() }).unwrap();
    let label = store.memory_domains().unwrap().into_iter().find(|item| item.id == "custom:precise-rename").unwrap();
    assert_eq!(label.name, "Alpha");
}

#[test]
fn user_defined_memory_sync_restores_tags_atomically() {
    let store = TelemetryStore::in_memory();
    let link = domain_link("custom:user-memory", "手写分类");
    store.upsert_synced_l1_memory_with_domains(
        "local-user:tagged", "手写记忆", "fact", "2026-11-01T00:00:00Z", Some(&[link.clone()]),
    ).unwrap();
    let exported = store.cloud_user_defined_l1_memories().unwrap();
    assert_eq!(exported.len(), 1);
    assert_eq!(exported[0].domains, Some(vec![link]));
    store.upsert_synced_l1_memory(
        "local-user:tagged", "旧版客户端写回正文", "fact", "2026-11-02T00:00:00Z",
    ).unwrap();
    assert_eq!(store.cloud_l1_domains("local-user:tagged").unwrap().len(), 1);
}
