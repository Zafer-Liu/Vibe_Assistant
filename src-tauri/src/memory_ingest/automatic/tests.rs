use super::*;

const IDLE: Duration = Duration::from_secs(180);

fn buffer(text: &str, now: Instant) -> SessionBuffer {
    SessionBuffer {
        agent_id: "owner".into(),
        source: "fixture".into(),
        messages: vec![("user".into(), text.into())],
        event_key: None,
        last_active: now,
    }
}

fn queue(scheduler: &mut AutomaticScheduler, id: &str, text: &str, now: Instant) {
    let mut receipt = buffer(text, now);
    receipt.event_key = Some(format!("{id}:{text}"));
    scheduler.enqueue(id, receipt);
}

#[test]
fn backlog_claims_only_two_jobs_and_releases_capacity_on_completion() {
    let now = Instant::now();
    let mut scheduler = AutomaticScheduler::default();
    for index in 0..1_000 {
        queue(&mut scheduler, &format!("session-{index:04}"), "complete", now);
    }
    let first = scheduler.take_ready(now, IDLE).unwrap();
    let second = scheduler.take_ready(now, IDLE).unwrap();
    assert_ne!(first.session_id, second.session_id);
    assert!(scheduler.take_ready(now, IDLE).is_none());
    assert_eq!(scheduler.active, AUTOMATIC_CONCURRENCY);
    scheduler.finish(first, None, now);
    assert!(scheduler.take_ready(now, IDLE).is_some());
    assert_eq!(scheduler.active, AUTOMATIC_CONCURRENCY);
}

#[test]
fn failed_batch_stays_complete_and_serialized_against_later_hook_transcripts() {
    let now = Instant::now();
    let mut scheduler = AutomaticScheduler::default();
    let original = "完整消息".repeat(12_001);
    queue(&mut scheduler, "session", &original, now);
    let first = scheduler.take_ready(now, IDLE).unwrap();
    queue(&mut scheduler, "session", "next complete turn", now);
    assert!(scheduler.take_ready(now, IDLE).is_none());
    scheduler.finish(first, Some(RetryClass::Transient), now);
    // Repeated Stop and incoming replacement must not reset retry deadlines.
    queue(&mut scheduler, "session", &original, now);
    assert!(scheduler.take_ready(now + Duration::from_secs(14), IDLE).is_none());
    let retry = scheduler.take_ready(now + Duration::from_secs(15), IDLE).unwrap();
    assert_eq!(retry.buffer.event_key, Some(format!("session:{original}")));
    assert_eq!(retry.buffer.messages[0].1, original);
    scheduler.finish(retry, None, now + Duration::from_secs(15));
    let next = scheduler.take_ready(now + Duration::from_secs(15), IDLE).unwrap();
    assert_eq!(next.buffer.messages, vec![("user".into(), "next complete turn".into())]);
}

#[test]
fn successful_batch_cannot_erase_new_hooks_or_extract_them_without_a_receipt() {
    let now = Instant::now();
    let mut scheduler = AutomaticScheduler::default();
    queue(&mut scheduler, "session", "first", now);
    let first = scheduler.take_ready(now, IDLE).unwrap();
    scheduler.buffer("session", "owner", "fixture", now).messages.push(("user".into(), "later".into()));
    scheduler.finish(first, None, now);
    let later = now + Duration::from_secs(86_400);
    assert!(scheduler.take_ready(later, IDLE).is_none());
    assert_eq!(scheduler.next_delay(later, IDLE, Duration::from_secs(30)), Duration::from_secs(30));
    assert_eq!(scheduler.request_all(), 0);
    assert_eq!(scheduler.take_pending("session").unwrap().messages[0].1, "later");
    queue(&mut scheduler, "session", "later", later);
    let next = scheduler.take_ready(later, IDLE).unwrap();
    assert_eq!(next.buffer.event_key.as_deref(), Some("session:later"));
    assert_eq!(next.buffer.messages[0].1, "later");
}

#[test]
fn configuration_failure_gates_unattempted_sessions_until_change_or_cooldown() {
    let now = Instant::now();
    let mut scheduler = AutomaticScheduler::default();
    queue(&mut scheduler, "a", "first", now);
    queue(&mut scheduler, "b", "second", now);
    let first = scheduler.take_ready(now, IDLE).unwrap();
    scheduler.finish(first, Some(RetryClass::Configuration), now);
    assert!(scheduler.take_ready(now + Duration::from_secs(1_799), IDLE).is_none());
    assert_eq!(scheduler.next_delay(now, IDLE, Duration::from_secs(30)), Duration::from_secs(30));
    scheduler.configuration_changed(now);
    assert!(scheduler.take_ready(now, IDLE).is_some());
    assert!(scheduler.take_ready(now, IDLE).is_some());
    assert!(scheduler.take_ready(now, IDLE).is_none());
}

#[test]
fn changed_configuration_during_running_attempt_cannot_leave_stale_backoff() {
    let now = Instant::now();
    for class in [RetryClass::Configuration, RetryClass::Transient, RetryClass::Contract] {
        let mut scheduler = AutomaticScheduler::default();
        queue(&mut scheduler, "session", "complete", now);
        let work = scheduler.take_ready(now, IDLE).unwrap();
        scheduler.configuration_changed(now);
        scheduler.finish(work, Some(class), now);
        assert!(scheduler.take_ready(now, IDLE).is_some(), "{class:?}");
    }
}

#[test]
fn local_write_retry_keeps_candidates_and_ignores_model_config_changes() {
    let now = Instant::now();
    let mut scheduler = AutomaticScheduler::default();
    queue(&mut scheduler, "session", "original evidence", now);
    let mut work = scheduler.take_ready(now, IDLE).unwrap();
    work.prepared = Some(PreparedMemories {
        candidates: vec![L1MemoryCandidate {
            content: "retained model output".into(),
            memory_type: "summary".into(),
            durability: "session".into(),
            domains: vec![],
        }],
        provider_name: "original model".into(),
    });
    scheduler.configuration_changed(now);
    scheduler.finish(work, Some(RetryClass::LocalWrite), now);
    scheduler.configuration_changed(now);
    assert!(scheduler.take_ready(now + Duration::from_secs(29), IDLE).is_none());
    // Even a provider configuration outage must allow prepared ledger writes.
    scheduler.configuration_blocked_until = Some(now + Duration::from_secs(1_800));
    let retry = scheduler.take_ready(now + Duration::from_secs(30), IDLE).unwrap();
    assert_eq!(retry.buffer.messages[0].1, "original evidence");
    let prepared = retry.prepared.unwrap();
    assert_eq!(prepared.candidates[0].content, "retained model output");
    assert_eq!(prepared.provider_name, "original model");
}

#[test]
fn transient_retry_grows_and_caps_while_contract_is_slower() {
    assert_eq!(RetryClass::Transient.delay(1), Duration::from_secs(15));
    assert_eq!(RetryClass::Transient.delay(2), Duration::from_secs(30));
    assert_eq!(RetryClass::Transient.delay(u32::MAX), Duration::from_secs(900));
    assert_eq!(RetryClass::Contract.delay(1), Duration::from_secs(300));
    assert_eq!(RetryClass::Contract.delay(u32::MAX), Duration::from_secs(3_600));
    let mut scheduler = AutomaticScheduler::default();
    let mut now = Instant::now();
    queue(&mut scheduler, "session", "complete", now);
    for delay in [15, 30, 60, 120, 240, 480, 900, 900] {
        let work = scheduler.take_ready(now, IDLE).unwrap();
        scheduler.finish(work, Some(RetryClass::Transient), now);
        let due = now + Duration::from_secs(delay);
        assert!(scheduler.take_ready(due - Duration::from_millis(1), IDLE).is_none());
        assert_eq!(scheduler.next_delay(now, IDLE, Duration::from_secs(3_600)), Duration::from_secs(delay));
        now = due;
    }
}

#[test]
fn explicit_ui_flush_bypasses_backoff_without_bypassing_serialization() {
    let now = Instant::now();
    let mut scheduler = AutomaticScheduler::default();
    queue(&mut scheduler, "session", "complete", now);
    let first = scheduler.take_ready(now, IDLE).unwrap();
    scheduler.finish(first, Some(RetryClass::Configuration), now);
    assert_eq!(scheduler.request_all(), 1);
    let retry = scheduler.take_ready(now, IDLE).unwrap();
    scheduler.request_all();
    assert!(scheduler.take_ready(now, IDLE).is_none());
    assert_eq!(retry.buffer.messages[0].1, "complete");
}

#[test]
fn discarded_pending_hooks_do_not_remove_running_session_marker() {
    let now = Instant::now();
    let mut scheduler = AutomaticScheduler::default();
    queue(&mut scheduler, "session", "complete", now);
    let first = scheduler.take_ready(now, IDLE).unwrap();
    scheduler.buffer("session", "owner", "fixture", now).messages.push(("user".into(), "covered by native scanner".into()));
    let _covered = scheduler.take_pending("session");
    queue(&mut scheduler, "session", "new hook", now);
    assert!(scheduler.take_ready(now, IDLE).is_none());
    scheduler.finish(first, None, now);
    assert_eq!(scheduler.take_ready(now, IDLE).unwrap().buffer.messages[0].1, "new hook");
}

#[test]
fn request_error_classification_does_not_blame_model_for_trace_writes() {
    use crate::memory_ingest::L1ExtractError;
    for (message, class) in [
        ("模型返回 HTTP 401 Unauthorized: invalid key", RetryClass::Configuration),
        ("模型返回 HTTP 429 Too Many Requests: rate limit", RetryClass::Transient),
        ("模型返回 HTTP 503 Service Unavailable: busy", RetryClass::Transient),
        ("模型响应无法解析（HTTP 200 OK）", RetryClass::Contract),
        ("模型请求失败（fixture）: timeout", RetryClass::Transient),
        ("记忆提取模型未返回文本内容", RetryClass::Contract),
        ("database is locked", RetryClass::LocalWrite),
        ("attempt to write a readonly database", RetryClass::LocalWrite),
        ("model trace database lock poisoned", RetryClass::LocalWrite),
        ("模型调用已结束，但请求追踪保存失败；请勿自动重发请求", RetryClass::UncertainOutcome),
    ] {
        let error = L1ExtractError::from_request_error(message.into());
        assert_eq!(error.retry_class(), class, "{message}");
        assert_eq!(error.into_message(), message);
    }
}

#[test]
fn queued_completed_turns_keep_fifo_order_and_their_exact_receipt_keys() {
    let now = Instant::now();
    let mut scheduler = AutomaticScheduler::default();
    queue(&mut scheduler, "session", "first completed turn", now);
    scheduler.buffer("session", "owner", "fixture", now).messages.push(("user".into(), "second prompt".into()));
    let _second_hooks = scheduler.take_pending("session");
    queue(&mut scheduler, "session", "second completed turn", now);
    scheduler.buffer("session", "owner", "fixture", now).messages.push(("user".into(), "third active turn".into()));
    let first = scheduler.take_ready(now, IDLE).unwrap();
    assert_eq!(first.buffer.event_key.as_deref(), Some("session:first completed turn"));
    assert_eq!(first.buffer.messages, vec![("user".into(), "first completed turn".into())]);
    scheduler.finish(first, None, now);
    let second = scheduler.take_ready(now, IDLE).unwrap();
    assert_eq!(second.buffer.event_key.as_deref(), Some("session:second completed turn"));
    assert_eq!(second.buffer.messages, vec![("user".into(), "second completed turn".into())]);
    scheduler.finish(second, None, now);
    assert!(scheduler.take_ready(now + IDLE, IDLE).is_none());
    assert_eq!(scheduler.take_pending("session").unwrap().messages[0].1, "third active turn");
}

#[test]
fn uncertain_completed_model_call_requires_explicit_retry() {
    let now = Instant::now();
    let mut scheduler = AutomaticScheduler::default();
    queue(&mut scheduler, "session", "complete evidence", now);
    let work = scheduler.take_ready(now, IDLE).unwrap();
    scheduler.finish(work, Some(RetryClass::UncertainOutcome), now);
    let later = now + Duration::from_secs(86_400);
    scheduler.configuration_changed(later);
    queue(&mut scheduler, "session", "complete evidence", later);
    assert!(scheduler.take_ready(later, IDLE).is_none());
    assert_eq!(scheduler.buffered_sessions(), 1);
    assert_eq!(scheduler.request_all(), 1);
    assert_eq!(scheduler.take_ready(later, IDLE).unwrap().buffer.messages[0].1, "complete evidence");
}

#[test]
fn repeated_stop_keys_are_deduplicated_while_queued_running_and_retained() {
    let now = Instant::now();
    let mut scheduler = AutomaticScheduler::default();
    queue(&mut scheduler, "session", "first", now);
    queue(&mut scheduler, "session", "first", now);
    assert_eq!(scheduler.sessions["session"].completed.len(), 1);
    let first = scheduler.take_ready(now, IDLE).unwrap();
    queue(&mut scheduler, "session", "first", now);
    assert!(scheduler.sessions["session"].completed.is_empty());
    queue(&mut scheduler, "session", "second", now);
    scheduler.finish(first, Some(RetryClass::Transient), now);
    queue(&mut scheduler, "session", "first", now);
    assert_eq!(scheduler.sessions["session"].completed.len(), 1);
    let retry = scheduler.take_ready(now + Duration::from_secs(15), IDLE).unwrap();
    assert_eq!(retry.buffer.event_key.as_deref(), Some("session:first"));
    scheduler.finish(retry, None, now);
    assert_eq!(scheduler.take_ready(now, IDLE).unwrap().buffer.event_key.as_deref(), Some("session:second"));
}

#[test]
fn hook_buffer_preserves_existing_per_message_unicode_limit() {
    let store = crate::memory_ingest::IngestStore::new();
    let text = "中".repeat(12_001);
    store.push_message("session", "owner", "fixture", "user", &text);
    let hooks = store.inner.lock().unwrap().automatic.take_pending("session").unwrap();
    assert_eq!(hooks.messages[0].1, "中".repeat(12_000) + "\n[内容已截断]");
    // Full transcript receipts keep their existing, separate size policy.
    store.queue_session_messages("transcript", "owner", "fixture", vec![("user".into(), text.clone())], Some("full-receipt".into()));
    let full = store.inner.lock().unwrap().automatic.take_ready(Instant::now(), IDLE).unwrap();
    assert_eq!(full.buffer.messages[0].1, text);
}
