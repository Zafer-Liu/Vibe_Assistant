use super::*;

fn record(id: &str, at: &str, origin: &str, model: &str) -> TelemetryUsageRecord {
    TelemetryUsageRecord {
        record_id: format!("claude:v1:{id}"), source: "claude".into(), session_id: id.into(),
        occurred_at: at.into(), model: Some(model.into()), record_kind: "response".into(),
        input_tokens: 100, output_tokens: 20, cached_tokens: 80, origin: origin.into(),
    }
}

fn write_record(store: &TelemetryStore, row: &TelemetryUsageRecord) {
    store.conn.lock().unwrap().execute(
        "INSERT OR REPLACE INTO usage_records
         (record_id, source, session_id, occurred_at, model, record_kind,
          input_tokens, output_tokens, cached_tokens, origin)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        params![row.record_id, row.source, row.session_id, row.occurred_at, row.model,
            row.record_kind, row.input_tokens, row.output_tokens, row.cached_tokens, row.origin],
    ).unwrap();
}

fn analytics(store: &TelemetryStore) -> TelemetryUsageAnalytics {
    store.usage_analytics(None, None, None, "day", 500).unwrap()
}

fn legacy() -> UsageDayAggregate {
    UsageDayAggregate {
        day: "2026-11-05".into(), source: "claude".into(), input_tokens: 1000,
        output_tokens: 200, cached_tokens: 800, record_count: 120,
    }
}

#[test]
fn two_devices_converge_on_hours_counts_models_and_provenance() {
    let a = TelemetryStore::in_memory();
    let b = TelemetryStore::in_memory();
    write_record(&a, &record("morning", "2026-11-05T01:00:00Z", "native_transcript", "model-a"));
    write_record(&b, &record("evening", "2026-11-05T14:00:00Z", "native_transcript", "model-b"));
    write_record(&a, &record("estimated", "2026-11-04T03:00:00Z", "estimated_transcript", "model-a"));
    let ledger_a = a.cloud_usage_ledger().unwrap();
    let ledger_b = b.cloud_usage_ledger().unwrap();
    a.replace_remote_usage_ledger("b", 1, &ledger_b).unwrap();
    b.replace_remote_usage_ledger("a", 1, &ledger_a).unwrap();
    let seen_a = analytics(&a);
    let seen_b = analytics(&b);
    assert_eq!(serde_json::to_value(&seen_a).unwrap(), serde_json::to_value(&seen_b).unwrap());
    assert_eq!(seen_a.record_count, 3);
    assert_eq!(seen_a.input_tokens + seen_a.output_tokens, 360);
    assert_eq!(seen_a.estimated_tokens, 120);
    assert!(seen_a.cache_capable);
    assert_eq!(seen_a.cost_groups.len(), 2);
    assert_eq!(seen_a.heatmap.iter().map(|cell| cell.record_count).sum::<i64>(), 3);
    assert_eq!(a.cloud_usage_ledger().unwrap().records.len(), 2, "远端记录不得二次发布");
    assert_eq!(b.cloud_usage_ledger().unwrap().records.len(), 1);
    assert_eq!(a.summary().unwrap().total_tokens, 360);
    assert_eq!(b.summary().unwrap().total_tokens, 360);
}

#[test]
fn a_restored_database_does_not_double_count_its_copied_requests() {
    let a = TelemetryStore::in_memory();
    let b = TelemetryStore::in_memory();
    write_record(&a, &record("copied", "2026-11-05T01:00:00Z", "native_transcript", "model-a"));
    let before = a.cloud_usage_ledger().unwrap();
    a.app_setting_set("cloud_vault.device_id", &"original-device").unwrap();
    {
        let src = a.conn.lock().unwrap();
        let mut dst = b.conn.lock().unwrap();
        let backup = rusqlite::backup::Backup::new(&src, &mut *dst).unwrap();
        backup.run_to_completion(100, std::time::Duration::from_millis(1), None).unwrap();
    }
    crate::cloud_sync::retire_cloud_device_identity(&b).unwrap();
    let copied = b.cloud_usage_ledger().unwrap();
    a.replace_remote_usage_ledger("restored-device", 1, &copied).unwrap();
    b.replace_remote_usage_ledger("original-device", 1, &before).unwrap();
    assert_eq!(analytics(&a).record_count, 1);
    assert_eq!(analytics(&a).input_tokens, 100);
    assert_eq!(analytics(&b).input_tokens, 100);
}

#[test]
fn fallback_export_uses_event_time_instead_of_rescan_time() {
    let store = TelemetryStore::in_memory();
    {
        let conn = store.conn.lock().unwrap();
        conn.execute_batch(
            "INSERT INTO agent_events (event_key, source, session_id, event_type, occurred_at, payload_json)
             VALUES ('old', 'qoder', 'old-session', 'Stop', '2026-11-01T01:00:00Z', '{}');
             INSERT INTO session_token_usage VALUES
             ('qoder', 'old-session', 100, 20, 0, 'estimated_transcript', '2026-11-05T01:00:00Z');",
        ).unwrap();
    }
    let ledger = store.cloud_usage_ledger().unwrap();
    assert_eq!(ledger.records[0].record.occurred_at, "2026-11-01T01:00:00.000Z");
    let days = store.usage_day_aggregates().unwrap();
    let expected_day: String = store.conn.lock().unwrap().query_row(
        "SELECT strftime('%Y-%m-%d', '2026-11-01T01:00:00Z', 'localtime')", [], |row| row.get(0),
    ).unwrap();
    assert_eq!(days[0].day, expected_day);
    let receiver = TelemetryStore::in_memory();
    receiver.replace_remote_usage_ledger("source", 1, &ledger).unwrap();
    assert_eq!(serde_json::to_value(analytics(&store)).unwrap(), serde_json::to_value(analytics(&receiver)).unwrap());
}

#[test]
fn remote_detail_replaces_a_local_session_total_globally() {
    let store = TelemetryStore::in_memory();
    store.conn.lock().unwrap().execute_batch(
        "INSERT INTO session_token_usage VALUES
         ('claude', 'shared-session', 500, 50, 0, 'estimated_transcript', '2026-11-05T01:00:00Z');",
    ).unwrap();
    let source = TelemetryStore::in_memory();
    let mut row = record("shared-session", "2026-11-05T01:00:00Z", "native_transcript", "model-a");
    row.record_id = "shared-response".into();
    write_record(&source, &row);
    store.replace_remote_usage_ledger("source", 1, &source.cloud_usage_ledger().unwrap()).unwrap();
    assert_eq!(analytics(&store).record_count, 1);
    assert_eq!(analytics(&store).input_tokens, 100);
    assert_eq!(analytics(&store).estimated_tokens, 0);
}

#[test]
fn legacy_days_keep_count_and_date_without_inventing_hours() {
    let store = TelemetryStore::in_memory();
    store.replace_remote_usage_totals("legacy", &[legacy()]).unwrap();
    let seen = analytics(&store);
    assert_eq!(seen.record_count, 120);
    assert_eq!(seen.buckets[0].label, "2026-11-05");
    assert_eq!(seen.buckets[0].record_count, 120);
    assert_eq!(seen.daily_only_tokens, 1200);
    assert!(seen.heatmap.is_empty());
    assert!(seen.records.is_empty());
    assert!(!seen.truncated_records);
    let hours = store.usage_analytics(None, None, None, "hour", 500).unwrap();
    assert!(hours.buckets.is_empty());
    let (start, end): (String, String) = store.conn.lock().unwrap().query_row(
        "SELECT strftime('%Y-%m-%dT%H:%M:%fZ', '2026-11-05 00:00:00', 'utc'),
                strftime('%Y-%m-%dT%H:%M:%fZ', '2026-11-06 00:00:00', 'utc')",
        [], |row| Ok((row.get(0)?, row.get(1)?)),
    ).unwrap();
    assert_eq!(store.usage_analytics(Some(&start), Some(&end), None, "day", 500).unwrap().input_tokens, 1000);
}

#[test]
fn a_precise_ledger_supersedes_legacy_totals_even_after_an_empty_update() {
    let store = TelemetryStore::in_memory();
    let source = TelemetryStore::in_memory();
    write_record(&source, &record("real", "2026-11-05T01:00:00Z", "native_transcript", "model-a"));
    store.replace_remote_usage_totals("source", &[legacy()]).unwrap();
    store.replace_remote_usage_ledger("source", 2, &source.cloud_usage_ledger().unwrap()).unwrap();
    assert_eq!(analytics(&store).input_tokens, 100);
    assert_eq!(analytics(&store).daily_only_tokens, 0);
    store.replace_remote_usage_ledger("source", 3, &CloudUsageLedger { format: 2, records: Vec::new() }).unwrap();
    // An old object arriving afterwards must not bring its daily counters back.
    store.replace_remote_usage_totals("source", &[legacy()]).unwrap();
    assert_eq!(analytics(&store).input_tokens, 0);
}

#[test]
fn timestamps_are_canonical_utc_and_remote_cache_capability_is_preserved() {
    let source = TelemetryStore::in_memory();
    let receiver = TelemetryStore::in_memory();
    write_record(&source, &record("offset", "2026-11-05T00:30:00+14:00", "native_transcript", "model-a"));
    let ledger = source.cloud_usage_ledger().unwrap();
    assert_eq!(ledger.records[0].record.occurred_at, "2026-11-04T10:30:00.000Z");
    let mut offset_wire = ledger.clone();
    offset_wire.records[0].record.occurred_at = "2026-11-05T00:30:00+14:00".into();
    offset_wire.records[0].updated_at = "2026-11-05T00:30:00+14:00".into();
    receiver.replace_remote_usage_ledger("source", 1, &offset_wire).unwrap();
    let seen = analytics(&receiver);
    assert_eq!(seen.records[0].occurred_at, "2026-11-04T10:30:00.000Z");
    assert!(seen.cache_capable);
    assert_eq!(seen.cached_tokens, 80);
    assert_eq!(serde_json::to_value(analytics(&source)).unwrap(), serde_json::to_value(seen).unwrap());
}

#[test]
fn unchanged_exports_keep_versions_and_new_corrections_can_reduce_counts() {
    let a = TelemetryStore::in_memory();
    let b = TelemetryStore::in_memory();
    let mut row = record("correction", "2026-11-05T01:00:00Z", "native_transcript", "model-a");
    for store in [&a, &b] {
        write_record(store, &row);
        store.cloud_usage_ledger().unwrap();
        store.conn.lock().unwrap().execute(
            "UPDATE usage_sync_local_state SET updated_at = '2000-01-01T00:00:00.000Z'", [],
        ).unwrap();
    }
    let original = a.cloud_usage_ledger().unwrap();
    assert_eq!(original.records[0].updated_at, "2000-01-01T00:00:00.000Z");
    assert_eq!(serde_json::to_vec(&original).unwrap(), serde_json::to_vec(&a.cloud_usage_ledger().unwrap()).unwrap());
    let old_copy = b.cloud_usage_ledger().unwrap();
    row.input_tokens = 10;
    row.output_tokens = 2;
    row.cached_tokens = 0;
    write_record(&a, &row);
    let corrected = a.cloud_usage_ledger().unwrap();
    a.replace_remote_usage_ledger("b", 1, &old_copy).unwrap();
    b.replace_remote_usage_ledger("a", 2, &corrected).unwrap();
    assert_eq!(analytics(&a).input_tokens, 10);
    assert_eq!(analytics(&b).input_tokens, 10);
    // A slower pull cannot replace a newer device snapshot.
    b.replace_remote_usage_ledger("a", 1, &original).unwrap();
    assert_eq!(analytics(&b).input_tokens, 10);
}

#[test]
fn codex_counter_corrections_roundtrip_without_clipping_or_inflating_totals() {
    let source = TelemetryStore::in_memory();
    let receiver = TelemetryStore::in_memory();
    let counts = [
        (1_000_000, 100_000, 300_000),
        (87_558, -28_341, 58_624),
        (270_736, -17_479, 140_416),
        (244_611, -9_973, 95_488),
        (123_058, 27_700, -16_768),
    ];
    for (index, (input, output, cached)) in counts.into_iter().enumerate() {
        let mut row = record(&format!("correction-{index}"), "2026-11-05T01:00:00Z", "native_transcript", "model-a");
        row.source = "codex".into();
        row.session_id = "corrected-session".into();
        row.input_tokens = input;
        row.output_tokens = output;
        row.cached_tokens = cached;
        write_record(&source, &row);
    }
    let ledger = source.cloud_usage_ledger().unwrap();
    assert_eq!(receiver.replace_remote_usage_ledger("source", 1, &ledger).unwrap(), 5);
    let seen = analytics(&receiver);
    assert_eq!(seen.input_tokens, 1_725_963);
    assert_eq!(seen.output_tokens, 71_907);
    assert_eq!(seen.cached_tokens, 577_760);
    assert_eq!(serde_json::to_value(analytics(&source)).unwrap(), serde_json::to_value(seen).unwrap());
    assert!(receiver.usage_records(20).unwrap().iter().any(|row| row.output_tokens < 0));
}

#[test]
fn codex_cache_delta_is_validated_against_the_session_total() {
    let source = TelemetryStore::in_memory();
    let receiver = TelemetryStore::in_memory();
    for (id, input, cached) in [("base", 100, 0), ("correction", 10, 50)] {
        let mut row = record(id, "2026-11-05T01:00:00Z", "native_transcript", "model-a");
        row.source = "codex".into();
        row.session_id = "corrected-session".into();
        row.input_tokens = input;
        row.cached_tokens = cached;
        write_record(&source, &row);
    }
    receiver.replace_remote_usage_ledger("source", 1, &source.cloud_usage_ledger().unwrap()).unwrap();
    assert_eq!(analytics(&receiver).input_tokens, 110);
    assert_eq!(analytics(&receiver).cached_tokens, 50);
}

#[test]
fn legacy_anonymous_sessions_have_the_same_stable_identity_on_both_devices() {
    let source = TelemetryStore::in_memory();
    let receiver = TelemetryStore::in_memory();
    source.conn.lock().unwrap().execute_batch(
        "INSERT INTO session_token_usage VALUES
         ('qoder', '', 100, 20, 0, 'estimated_event_payload', '2026-11-05T01:00:00Z');",
    ).unwrap();
    let exported = source.cloud_usage_ledger().unwrap();
    assert_eq!(exported.records[0].record.session_id, "legacy-session:session-total:qoder:");
    // Older senders already published the same record with an empty session_id.
    let mut old_payload = exported.clone();
    old_payload.records[0].record.session_id.clear();
    receiver.replace_remote_usage_ledger("source", 1, &old_payload).unwrap();
    receiver.replace_remote_usage_ledger("source", 2, &exported).unwrap();
    assert_eq!(analytics(&receiver).record_count, 1);
    assert_eq!(serde_json::to_value(analytics(&source)).unwrap(), serde_json::to_value(analytics(&receiver)).unwrap());
}

#[test]
fn anonymous_response_identity_is_stable_across_imports_too() {
    let source = TelemetryStore::in_memory();
    let receiver = TelemetryStore::in_memory();
    let mut row = record("anonymous", "2026-11-05T01:00:00Z", "native_transcript", "model-a");
    row.session_id.clear();
    write_record(&source, &row);
    let exported = source.cloud_usage_ledger().unwrap();
    assert_eq!(exported.records[0].record.session_id, format!("legacy-session:{}", row.record_id));
    receiver.replace_remote_usage_ledger("source", 1, &exported).unwrap();
    assert_eq!(serde_json::to_value(analytics(&source)).unwrap(), serde_json::to_value(analytics(&receiver)).unwrap());
}

#[test]
fn invalid_correction_totals_preserve_the_previous_remote_snapshot() {
    let source = TelemetryStore::in_memory();
    let receiver = TelemetryStore::in_memory();
    let mut row = record("invalid-correction", "2026-11-05T01:00:00Z", "native_transcript", "model-a");
    row.source = "codex".into();
    write_record(&source, &row);
    let mut ledger = source.cloud_usage_ledger().unwrap();
    receiver.replace_remote_usage_ledger("source", 1, &ledger).unwrap();
    ledger.records[0].record.output_tokens = -1;
    let error = receiver.replace_remote_usage_ledger("source", 2, &ledger).unwrap_err();
    assert!(error.contains("会话合计无效"), "{error}");
    assert_eq!(analytics(&receiver).output_tokens, 20);
    let saved_revision: i64 = receiver.conn.lock().unwrap().query_row(
        "SELECT revision FROM usage_remote_ledgers WHERE device = 'source'", [], |row| row.get(0),
    ).unwrap();
    assert_eq!(saved_revision, 1);
}

#[test]
fn validation_errors_identify_the_field_without_exposing_record_ids() {
    let source = TelemetryStore::in_memory();
    let receiver = TelemetryStore::in_memory();
    let row = record("private-record-id", "2026-11-05T01:00:00Z", "native_transcript", "model-a");
    write_record(&source, &row);
    let valid = source.cloud_usage_ledger().unwrap();
    let mut unsupported = valid.clone();
    unsupported.records[0].record.record_kind = "unexpected".into();
    let error = receiver.replace_remote_usage_ledger("source", 1, &unsupported).unwrap_err();
    assert!(error.contains("第 1 条") && error.contains("record_kind"), "{error}");
    assert!(!error.contains("private-record-id"));
    let mut duplicate = valid.clone();
    duplicate.records.push(valid.records[0].clone());
    let error = receiver.replace_remote_usage_ledger("source", 1, &duplicate).unwrap_err();
    assert!(error.contains("第 2 条") && error.contains("record_id 重复"), "{error}");
    let mut bad_time = valid;
    bad_time.records[0].updated_at = "not-a-timestamp".into();
    let error = receiver.replace_remote_usage_ledger("source", 1, &bad_time).unwrap_err();
    assert!(error.contains("updated_at"), "{error}");
}

#[test]
fn exporting_invalid_usage_does_not_commit_sync_versions() {
    let source = TelemetryStore::in_memory();
    let mut row = record("invalid-export", "2026-11-05T01:00:00Z", "native_transcript", "model-a");
    row.output_tokens = -1;
    write_record(&source, &row);
    let error = source.cloud_usage_ledger().unwrap_err();
    assert!(error.contains("不能为负数"), "{error}");
    let count: i64 = source.conn.lock().unwrap().query_row(
        "SELECT COUNT(*) FROM usage_sync_local_state", [], |row| row.get(0),
    ).unwrap();
    assert_eq!(count, 0);
}

#[test]
fn charts_and_summary_reuse_one_snapshot_without_rewriting_it() {
    let store = TelemetryStore::in_memory();
    write_record(&store, &record("snapshot", "2026-11-05T01:00:00Z", "native_transcript", "model-a"));
    let seen = analytics(&store);
    let changes = || -> i64 {
        store.conn.lock().unwrap().query_row("SELECT total_changes()", [], |row| row.get(0)).unwrap()
    };
    let after_build = changes();
    let days = store.usage_calendar(None, None, None).unwrap();
    assert_eq!(serde_json::to_value(days).unwrap(), serde_json::to_value(&seen.buckets).unwrap());
    assert_eq!(store.summary().unwrap().total_tokens, 120);
    assert_eq!(store.usage_records(200).unwrap().len(), 1);
    assert_eq!(analytics(&store).record_count, 1);
    assert_eq!(changes(), after_build, "图表、概览和明细应复用同一临时快照");
}

#[test]
fn snapshot_rebuilds_after_local_edits_and_remote_removal() {
    let source = TelemetryStore::in_memory();
    let store = TelemetryStore::in_memory();
    let mut row = record("local", "2026-11-05T01:00:00Z", "native_transcript", "model-a");
    write_record(&store, &row);
    write_record(&source, &record("remote", "2026-11-04T01:00:00Z", "native_transcript", "model-b"));
    store.replace_remote_usage_ledger("source", 1, &source.cloud_usage_ledger().unwrap()).unwrap();
    assert_eq!(analytics(&store).input_tokens, 200);
    row.input_tokens = 10;
    row.cached_tokens = 0;
    write_record(&store, &row);
    assert_eq!(analytics(&store).input_tokens, 110);
    store.replace_remote_usage_ledger("source", 2, &CloudUsageLedger { format: 2, records: Vec::new() }).unwrap();
    assert_eq!(analytics(&store).input_tokens, 10);
    assert_eq!(store.usage_calendar(None, None, None).unwrap().len(), 1);
}

#[test]
fn snapshot_observes_writes_from_an_independent_database_connection() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("usage.sqlite3");
    let conn = crate::telemetry_store::schema::initialize_database(Connection::open(&path).unwrap()).unwrap();
    let store = TelemetryStore::from_connection(conn);
    let row = record("external-edit", "2026-11-05T01:00:00Z", "native_transcript", "model-a");
    write_record(&store, &row);
    assert_eq!(analytics(&store).input_tokens, 100);
    let peer = Connection::open(&path).unwrap();
    peer.execute("UPDATE usage_records SET input_tokens = 200 WHERE record_id = ?1", [&row.record_id]).unwrap();
    assert_eq!(analytics(&store).input_tokens, 200);
    assert_eq!(store.usage_records(200).unwrap()[0].input_tokens, 200);
}

#[test]
fn calendar_only_query_preserves_filters_and_legacy_day_counts() {
    let store = TelemetryStore::in_memory();
    write_record(&store, &record("native-day", "2026-11-04T01:00:00Z", "native_transcript", "model-a"));
    let mut old = legacy();
    old.source = "qoder".into();
    store.replace_remote_usage_totals("legacy", &[old]).unwrap();
    let selected = store.usage_calendar(None, None, Some("qoder")).unwrap();
    assert_eq!(selected.len(), 1);
    assert_eq!(selected[0].label, "2026-11-05");
    assert_eq!(selected[0].record_count, 120);
    let (start, end): (String, String) = store.conn.lock().unwrap().query_row(
        "SELECT strftime('%Y-%m-%dT%H:%M:%fZ', '2026-11-05 00:00:00', 'utc'),
                strftime('%Y-%m-%dT%H:%M:%fZ', '2026-11-06 00:00:00', 'utc')",
        [], |row| Ok((row.get(0)?, row.get(1)?)),
    ).unwrap();
    let days = store.usage_calendar(Some(&start), Some(&end), None).unwrap();
    let full = store.usage_analytics(Some(&start), Some(&end), None, "day", 500).unwrap();
    assert_eq!(serde_json::to_value(days).unwrap(), serde_json::to_value(full.buckets).unwrap());
}

#[test]
fn invalid_remote_ledger_rolls_back_and_does_not_hide_legacy_data() {
    let store = TelemetryStore::in_memory();
    store.replace_remote_usage_totals("source", &[legacy()]).unwrap();
    let mut row = record("bad", "2026-11-05T01:00:00Z", "native_transcript", "model-a");
    row.cached_tokens = row.input_tokens + 1;
    let ledger = CloudUsageLedger {
        format: 2, records: vec![CloudUsageRecord { record: row, updated_at: "2026-11-05T01:00:00Z".into() }],
    };
    assert!(store.replace_remote_usage_ledger("source", 1, &ledger).is_err());
    assert!(!store.has_remote_usage_ledger("source").unwrap());
    assert_eq!(analytics(&store).input_tokens, 1000);
}
