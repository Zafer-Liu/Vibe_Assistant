use super::*;

#[test]
fn keyword_evaluation_uses_an_isolated_store_and_has_no_recall_side_effects() {
    let production_fixture = TelemetryStore::in_memory();
    production_fixture.app_setting_set("evaluation-isolation-marker", &"keep").unwrap();
    let before: i64 = production_fixture.conn.lock().unwrap()
        .query_row("SELECT COUNT(*) FROM local_memory_items", [], |row| row.get(0)).unwrap();
    let temporary = tempfile::tempdir().unwrap();
    let store = TelemetryStore::isolated_for_retrieval_evaluation(&temporary.path().join("corpus.sqlite3")).unwrap();
    let data = dataset::builtin().unwrap();
    seed(&store, &data).unwrap();
    let budget = RecallBudget::new(Duration::from_secs(2));
    let result = super::super::recall_for_evaluation(&store,
        &json!({"query":"SQLite","domain_mode":"manual","domains":["atlas"],"limit":3}),
        2000, &budget, None, None, &MemoryDomainConfig::default()).unwrap();
    assert_eq!(result["retrieval"]["mode"], "keyword_only_evaluation");
    assert_eq!(result["retrieval"]["trace_status"], "disabled");
    let conn = store.conn.lock().unwrap();
    assert_eq!(conn.query_row("SELECT COUNT(*) FROM memory_recall_traces", [], |row| row.get::<_, i64>(0)).unwrap(), 0);
    assert_eq!(conn.query_row("SELECT COALESCE(SUM(recall_count),0) FROM memory_importance", [], |row| row.get::<_, i64>(0)).unwrap(), 0);
    assert!(!temporary.path().join("vectors.sqlite3").exists());
    let after: i64 = production_fixture.conn.lock().unwrap()
        .query_row("SELECT COUNT(*) FROM local_memory_items", [], |row| row.get(0)).unwrap();
    assert_eq!(before, after);
    assert_eq!(production_fixture.app_setting_get::<String>("evaluation-isolation-marker").as_deref(), Some("keep"));
}

#[test]
fn hybrid_mock_http_retrieves_and_reads_actual_source_text_without_production_state() {
    struct MockServer {
        stop: Arc<AtomicBool>,
        requests: Arc<std::sync::atomic::AtomicUsize>,
        worker: Option<std::thread::JoinHandle<()>>,
    }
    impl Drop for MockServer {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::Release);
            if let Some(worker) = self.worker.take() {
                let result = worker.join();
                if !std::thread::panicking() { result.unwrap(); }
            }
        }
    }
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    listener.set_nonblocking(true).unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    let stopped = stop.clone();
    let requests = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let observed = requests.clone();
    let worker = std::thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(30);
        while !stopped.load(Ordering::Acquire) && Instant::now() < deadline {
            let (mut stream, _) = match listener.accept() {
                Ok(stream) => stream,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(2)); continue;
                }
                Err(error) => panic!("mock listener failed: {error}"),
            };
            // Windows inherits the listener's nonblocking mode on accept.
            stream.set_nonblocking(false).unwrap();
            stream.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
            let mut bytes = Vec::new();
            let (body_start, content_length) = loop {
                let mut chunk = [0u8; 4096];
                let count = stream.read(&mut chunk).unwrap();
                if count == 0 { break (0,0); } // Runtime readiness TCP probes carry no HTTP.
                bytes.extend_from_slice(&chunk[..count]);
                if let Some(end) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
                    let headers = String::from_utf8_lossy(&bytes[..end]);
                    assert!(headers.starts_with("POST /v1/embeddings "));
                    let length = headers.lines().find_map(|line| {
                        let (name,value) = line.split_once(':')?;
                        name.eq_ignore_ascii_case("content-length").then(|| value.trim().parse::<usize>().unwrap())
                    }).unwrap();
                    break (end+4,length);
                }
                assert!(bytes.len() < 128*1024);
            };
            if body_start == 0 { continue; }
            while bytes.len() < body_start+content_length {
                let mut chunk = [0u8;4096];
                let count = stream.read(&mut chunk).unwrap();
                assert!(count > 0);
                bytes.extend_from_slice(&chunk[..count]);
            }
            let payload: Value = serde_json::from_slice(&bytes[body_start..body_start+content_length]).unwrap();
            let data: Vec<_> = payload["input"].as_array().unwrap().iter().enumerate().map(|(index,input)| {
                let text = input.as_str().unwrap().to_ascii_lowercase();
                let kind = ["middle", "merged", "restored", "manual", "missing source", "remote mirror", "partial procedure", "final handoff"]
                    .iter().position(|word| text.contains(word)).unwrap_or(7);
                let mut vector = vec![0.0_f32;8]; vector[kind] = 1.0;
                json!({"index":index,"embedding":vector})
            }).collect();
            let response = serde_json::to_vec(&json!({"data":data})).unwrap();
            write!(stream,"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nConnection: close\r\nContent-Length: {}\r\n\r\n",response.len()).unwrap();
            stream.write_all(&response).unwrap();
            observed.fetch_add(1,Ordering::AcqRel);
        }
    });
    let server = MockServer { stop, requests, worker: Some(worker) };
    let temporary = tempfile::tempdir().unwrap();
    let store = TelemetryStore::isolated_for_retrieval_evaluation(&temporary.path().join("corpus.sqlite3")).unwrap();
    let dataset = dataset::builtin_evidence().unwrap();
    seed(&store,&dataset).unwrap();
    let config = json!({"base_url":format!("http://{address}/v1"),"model":"synthetic-http-fixture"});
    let cache = temporary.path().join("vectors.sqlite3");
    evaluation_embedding::prepare(config.clone(),&snapshots(&dataset),&cache,&RecallBudget::new(Duration::from_secs(5)), |_|{}).unwrap();
    let job = Job { status:Mutex::new(RunStatus {run_id:"mock".into(),status:"running".into(),completed:0,total:2,prepared:8,total_memories:8,error:None}),
        cancelled:AtomicBool::new(false),budget:Mutex::new(None) };
    let mut coverages = Vec::new();
    for case_id in ["evidence-middle","evidence-merged"] {
        let case = dataset.cases.iter().find(|case| case.case_id == case_id).unwrap();
        let budget = RecallBudget::new(Duration::from_secs(5));
        let response = super::super::recall_for_evaluation(&store,
            &json!({"query":case.query,"limit":case.limit,"domain_mode":case.domain_mode,"domains":case.domains}),
            5000,&budget,Some(&cache),Some(&config),&MemoryDomainConfig::default()).unwrap();
        assert_eq!(response["retrieval"]["mode"],"semantic");
        assert_eq!(response["retrieval"]["index"]["pending"],0);
        let returned:Vec<_> = response["memories"].as_array().unwrap().iter().map(|item|item["id"].as_str().unwrap().to_string()).collect();
        assert!(case.expected_ids.iter().all(|id|returned.contains(id)),"{case_id}: {returned:?}");
        let scores = evaluate_evidence(&store,case,&response,&returned,&case.domains,&budget,&job);
        assert_eq!(scores.0,Some(1.0),"actual original excerpts must cover every source annotation");
        coverages.push(scores.0);
    }
    let conn = store.conn.lock().unwrap();
    assert_eq!(conn.query_row("SELECT COUNT(*) FROM memory_recall_traces",[],|row|row.get::<_,i64>(0)).unwrap(),0);
    assert_eq!(conn.query_row("SELECT COALESCE(SUM(recall_count),0) FROM memory_importance",[],|row|row.get::<_,i64>(0)).unwrap(),0);
    assert_eq!(server.requests.load(Ordering::Acquire),3);
    println!("{}",json!({"fixture_http_requests":server.requests.load(Ordering::Acquire),"actual_excerpt_coverages":coverages,"production_side_effects":false}));
}

#[test]
fn confirmation_is_required_before_a_plan_can_be_consumed() {
    assert!(memory_retrieval_eval_start("not-a-plan".into(), false).err().unwrap().contains("confirmation"));
}

#[test]
fn cancelled_before_execution_lists_omitted_cases_without_scoring_them() {
    let dataset = dataset::builtin().unwrap();
    let backend = BackendInfo { embedding_model: None, embedding_location: "disabled".into(),
        routing_model: None, routing_provider: "disabled".into(), routing_remote: false };
    let plan = PendingPlan {
        public: EvaluationPlan {
            plan_id: "test".into(), dataset_id: dataset.dataset_id.clone(), dataset_name: dataset.name.clone(),
            synthetic: true, dataset_fingerprint: dataset::fingerprint(&dataset), split: "development".into(),
            mode: "keyword".into(), memory_count: dataset.memories.len(), case_count: 32,
            embedding_batches: 0, query_calls: 0, routing_calls: 0, remote_calls_estimate: 0,
            budget_ms: 1000, backend: backend.clone(), expires_at: now(),
        },
        dataset, embedding: None, routing: MemoryDomainConfig::default(), expires: Instant::now(),
    };
    let mut report = EvaluationReport {
        schema_version: 1, run_id: "test".into(), dataset_id: plan.public.dataset_id.clone(),
        dataset_name: plan.public.dataset_name.clone(), synthetic: true,
        dataset_fingerprint: plan.public.dataset_fingerprint.clone(), split: "development".into(),
        mode: "keyword".into(), status: "cancelled".into(), planned_count: 32,
        started_at: now(), finished_at: Some(now()), preparation_ms: 0,
        backend, cases: Vec::new(), summary: metrics::aggregate(&[], 32),
    };
    append_unexecuted(&plan, &mut report);
    assert_eq!(report.cases.len(), 32);
    assert!(report.cases.iter().all(|case| case.status == "not_run" && case.metrics.recall_at_k.is_none()));
    let summary = metrics::aggregate(&report.cases, 32);
    assert_eq!(summary.executed_count, 0);
    assert_eq!(summary.not_run_count, 32);
    assert_eq!(summary.recall_at_k, None);
    assert!(report.cases.iter().all(|case| case.evidence_coverage.is_none()
        && case.evidence_status_hit.is_none() && case.evidence_ms.is_none()));
}

#[test]
fn source_seed_preserves_manual_remote_and_merged_metadata_in_isolation() {
    let temporary = tempfile::tempdir().unwrap();
    let store = TelemetryStore::isolated_for_retrieval_evaluation(&temporary.path().join("corpus.sqlite3")).unwrap();
    seed(&store, &dataset::builtin_evidence().unwrap()).unwrap();
    let conn = store.conn.lock().unwrap();
    assert_eq!(conn.query_row("SELECT COUNT(*) FROM agent_events WHERE event_type='Evaluation' AND payload_json='{}' AND conversation_state='full'", [], |r| r.get::<_, i64>(0)).unwrap(), 3);
    assert_eq!(conn.query_row("SELECT user_defined FROM local_memory_items WHERE id='ev-manual'", [], |r| r.get::<_, bool>(0)).unwrap(), true);
    assert_eq!(conn.query_row("SELECT origin_device FROM local_memory_items WHERE id='ev-remote'", [], |r| r.get::<_, String>(0)).unwrap(), "synthetic-other-device");
    assert_eq!(conn.query_row("SELECT COUNT(*) FROM memory_refinement_provenance WHERE memory_id='ev-merged'", [], |r| r.get::<_, i64>(0)).unwrap(), 2);
}

#[test]
fn annotated_cancelled_and_unreturned_cases_score_zero_without_authorization() {
    let store = TelemetryStore::in_memory();
    let data = dataset::builtin_evidence().unwrap();
    let mut case = data.cases[0].clone();
    case.expected_evidence_status = Some("ok".into());
    let job = Job {
        status: Mutex::new(RunStatus { run_id: "synthetic".into(), status: "running".into(),
            completed: 0, total: 1, prepared: 0, total_memories: 0, error: None }),
        cancelled: AtomicBool::new(false), budget: Mutex::new(None),
    };
    let budget = RecallBudget::new(Duration::from_secs(1));
    let scores = evaluate_evidence(&store, &case, &Value::Null, &[], &[], &budget, &job);
    assert_eq!((scores.0, scores.1), (Some(0.0), Some(false)));
    job.cancelled.store(true, Ordering::Release);
    let scores = evaluate_evidence(&store, &case, &Value::Null, &case.expected_ids, &[], &budget, &job);
    assert_eq!((scores.0, scores.1), (Some(0.0), Some(false)));
    job.cancelled.store(false, Ordering::Release);
    let expired = RecallBudget::new(Duration::ZERO);
    let scores = evaluate_evidence(&store, &case, &Value::Null, &case.expected_ids, &[], &expired, &job);
    assert_eq!((scores.0, scores.1), (Some(0.0), Some(false)));
    case.expected_evidence.clear();
    case.expected_evidence_status = None;
    assert_eq!(evaluate_evidence(&store, &case, &Value::Null, &[], &[], &budget, &job), (None, None, None));
}
