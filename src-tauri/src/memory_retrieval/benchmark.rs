//! Explicit, user-started evaluations against a frozen corpus. Credentials stay
//! in pending plans and workers, never in the dataset database or report files.
pub(crate) mod dataset;
pub(crate) mod metrics;
pub(crate) mod types;
#[cfg(test)]
mod tests;

use super::RecallBudget;
use crate::memory_backend::evaluation_embedding;
use crate::memory_domain::MemoryDomainConfig;
use crate::telemetry_store::{LocalMemorySnapshot, TelemetryStore};
use rusqlite::params;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{atomic::{AtomicBool, Ordering}, Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};
use types::*;

const MAX_FILE_BYTES: u64 = 10 * 1024 * 1024;
struct PendingPlan {
    public: EvaluationPlan,
    dataset: Dataset,
    embedding: Option<Value>,
    routing: MemoryDomainConfig,
    expires: Instant,
}
struct Job {
    status: Mutex<RunStatus>,
    cancelled: AtomicBool,
    budget: Mutex<Option<RecallBudget>>,
}
#[derive(Default)]
struct State {
    plans: HashMap<String, PendingPlan>,
    jobs: HashMap<String, Arc<Job>>,
    last_run: Option<String>,
}
static STATE: OnceLock<Mutex<State>> = OnceLock::new();
fn state() -> &'static Mutex<State> { STATE.get_or_init(|| Mutex::new(State::default())) }
fn now() -> String { chrono::Utc::now().to_rfc3339() }
fn root() -> Result<PathBuf, String> {
    Ok(dirs_next::config_dir().ok_or("application data directory unavailable")?
        .join("agent-manager").join("evaluations").join("memory"))
}
fn run_path(id: &str) -> Result<PathBuf, String> {
    uuid::Uuid::parse_str(id).map_err(|_| "invalid evaluation run ID")?;
    Ok(root()?.join("runs").join(id))
}
fn read_text(path: &Path) -> Result<String, String> {
    let mut bytes = Vec::new();
    File::open(path).map_err(|_| "evaluation file unavailable")?.take(MAX_FILE_BYTES + 1)
        .read_to_end(&mut bytes).map_err(|_| "evaluation file could not be read")?;
    if bytes.len() as u64 > MAX_FILE_BYTES { return Err("evaluation file exceeds 10 MiB".into()); }
    String::from_utf8(bytes).map_err(|_| "evaluation file must be UTF-8".into())
}
fn write_json(path: &Path, value: &impl serde::Serialize) -> Result<(), String> {
    let parent = path.parent().ok_or("evaluation output directory unavailable")?;
    let bytes = serde_json::to_vec_pretty(value).map_err(|_| "evaluation report serialization failed")?;
    if bytes.len() as u64 > MAX_FILE_BYTES { return Err("evaluation output exceeds 10 MiB".into()); }
    let mut temporary = tempfile::NamedTempFile::new_in(parent).map_err(|_| "evaluation output unavailable")?;
    temporary.write_all(&bytes).map_err(|_| "evaluation output write failed")?;
    temporary.persist(path).map_err(|_| "evaluation output publication failed")?;
    Ok(())
}
fn load_dataset(id: &str) -> Result<Dataset, String> {
    if id == "example-memory-v1" { return dataset::builtin(); }
    if id == "evidence-example-v1" { return dataset::builtin_evidence(); }
    if !dataset::valid_id(id) { return Err("invalid evaluation dataset ID".into()); }
    let value = dataset::parse_and_validate(&read_text(&root()?.join("datasets").join(format!("{id}.json")))?)?;
    if value.dataset_id != id { return Err("dataset ID does not match its file".into()); }
    Ok(value)
}
fn snapshots(value: &Dataset) -> Vec<LocalMemorySnapshot> {
    value.memories.iter().enumerate().map(|(ordinal, row)| LocalMemorySnapshot {
        id: row.id.clone(), source_event_key: row.source_event_key.clone(), ordinal: ordinal as i64,
        memory: row.content.clone(), memory_type: row.memory_type.clone(), durability: row.durability.clone(),
        source: row.source_agent.clone(), session_id: row.session_id.clone(), user_defined: row.user_defined,
        event_time: row.event_time.clone(), created_at: "2026-01-01T00:00:00.000Z".into(),
        updated_at: "2026-01-01T00:00:00.000Z".into(),
    }).collect()
}
fn seed(store: &TelemetryStore, value: &Dataset) -> Result<(), String> {
    let rows = snapshots(value);
    let mut conn = store.conn.lock().map_err(|_| "evaluation database lock poisoned")?;
    let transaction = conn.transaction().map_err(|_| "evaluation snapshot transaction failed")?;
    transaction.execute("DELETE FROM memory_domains", []).map_err(|_| "evaluation domains could not be initialized")?;
    for domain in &value.domains {
        transaction.execute("INSERT INTO memory_domains(id,name,kind,description,enabled,created_at,updated_at) VALUES (?1,?2,'custom',?3,1,?4,?4)",
            params![domain.id, domain.name, domain.description, "2026-01-01T00:00:00.000Z"])
            .map_err(|_| "evaluation domain could not be inserted")?;
    }
    for source in &value.sources {
        transaction.execute("INSERT INTO agent_events(event_key,source,session_id,event_type,occurred_at,payload_json,conversation_state,conversation_message_count,conversation_text) VALUES (?1,?2,?3,'Evaluation',?4,'{}','full',1,?5)",
            params![source.event_key,source.source_agent,source.session_id,source.occurred_at,source.text])
            .map_err(|_| "evaluation source could not be inserted")?;
    }
    for (row, original) in rows.iter().zip(&value.memories) {
        transaction.execute("INSERT INTO local_memory_items(id,source_event_key,ordinal,memory,memory_type,durability,source,session_id,user_defined,event_time,created_at,updated_at,origin_device) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13)",
            params![row.id,row.source_event_key,row.ordinal,row.memory,row.memory_type,row.durability,row.source,row.session_id,row.user_defined,row.event_time,row.created_at,row.updated_at,original.origin_device])
            .map_err(|_| "evaluation memory could not be inserted")?;
        for provenance in &original.provenance {
            let payload = json!({"id":provenance.source_id,"source_event_key":provenance.source_event_key,
                "source":provenance.source_agent,"session_id":provenance.session_id}).to_string();
            transaction.execute("INSERT INTO memory_refinement_provenance(memory_id,source_id,payload) VALUES (?1,?2,?3)",
                params![row.id,provenance.source_id,payload]).map_err(|_| "evaluation provenance could not be inserted")?;
        }
        for domain in &original.domains {
            transaction.execute("INSERT INTO local_memory_domains(memory_id,domain_id,weight,source,created_at,updated_at) VALUES (?1,?2,1,'manual',?3,?3)",
                params![row.id, domain, row.updated_at]).map_err(|_| "evaluation domain link could not be inserted")?;
        }
    }
    transaction.commit().map_err(|_| "evaluation snapshot commit failed".into())
}
fn selected_cases(plan: &PendingPlan) -> Vec<EvaluationCase> {
    plan.dataset.cases.iter().filter(|case| plan.public.split == "all" || case.split == plan.public.split).cloned().collect()
}
fn append_unexecuted(plan: &PendingPlan, report: &mut EvaluationReport) {
    let completed: HashSet<_> = report.cases.iter().map(|case| case.case_id.clone()).collect();
    for case in selected_cases(plan).into_iter().filter(|case| !completed.contains(&case.case_id)) {
        let mut scores = metrics::evaluate(&case.expected_ids, &[], &case.forbidden_ids, case.limit);
        scores.recall_at_k = None; scores.mrr = None; scores.ndcg_at_k = None;
        report.cases.push(CaseResult {
            case_id: case.case_id, category: case.category, split: case.split, query: case.query,
            status: "not_run".into(), error: None, expected_ids: case.expected_ids, forbidden_ids: case.forbidden_ids,
            returned_ids: Vec::new(), expected_domain_ids: case.expected_domain_ids, active_domains: Vec::new(),
            metrics: scores, domain_hit: None, degraded: false, index_complete: false,
            retrieval: json!({}), elapsed_ms: 0, returned_chars: 0, evidence_coverage: None,
            evidence_status_hit: None, evidence_ms: None,
        });
    }
}

/// Only evaluation metadata for IDs that really reached RecallResponse.memories
/// may authorize a read. Neither production capabilities nor a database fallback
/// are accepted here. Raw responses and capabilities never enter CaseResult.
fn evaluate_evidence(
    store: &TelemetryStore, case: &EvaluationCase, response: &Value,
    returned_ids: &[String], active_domains: &[String], budget: &RecallBudget, job: &Job,
) -> (Option<f64>, Option<bool>, Option<u64>) {
    let coverage = (!case.expected_evidence.is_empty()).then_some(0.0);
    let status = case.expected_evidence_status.as_ref().map(|_| false);
    if coverage.is_none() && status.is_none() { return (None, None, None); }
    let started = Instant::now();
    let measured = (|| {
        if job.cancelled.load(Ordering::Acquire) { return None; }
        let remaining = budget.remaining().ok()?;
        let mut evidence_budget = budget.clone();
        evidence_budget.deadline = evidence_budget.deadline.min(started + remaining.min(Duration::from_secs(1)));
        set_budget(job, &evidence_budget);
        let ids: Vec<_> = case.expected_ids.iter().filter(|id| returned_ids.contains(id)).take(2).cloned().collect();
        if ids.is_empty() { return None; }
        let results: Vec<super::trace::TraceResult> = serde_json::from_value(response.get("_evidence_results")?.clone()).ok()?;
        let results: Vec<_> = results.into_iter().filter(|result| ids.contains(&result.memory_id)).collect();
        if ids.iter().any(|id| !results.iter().any(|result| &result.memory_id == id)) { return None; }
        if job.cancelled.load(Ordering::Acquire) || evidence_budget.remaining().is_err() { return None; }
        let access = crate::memory_evidence::issue_for_results(store, &uuid::Uuid::new_v4().to_string(), &results,
            (!active_domains.is_empty()).then_some(active_domains), &evidence_budget);
        if job.cancelled.load(Ordering::Acquire) || evidence_budget.remaining().is_err() { return None; }
        let request = crate::memory_evidence::EvidenceRequest {
            context_token: access.context_token?, memory_ids: ids.clone(), query: Some(case.query.chars().take(512).collect()),
            max_chars: Some(access.max_chars), cursor: None,
        };
        let evidence = crate::memory_evidence::read_with_store(store, &request, &evidence_budget);
        if job.cancelled.load(Ordering::Acquire) || evidence_budget.remaining().is_err() { return None; }
        Some(metrics::evidence_scores(case, &ids, &evidence))
    })();
    let (coverage, status) = measured.unwrap_or((coverage, status));
    (coverage, status, Some(started.elapsed().as_millis() as u64))
}

fn is_active(status: &str) -> bool { matches!(status, "preparing" | "running" | "cancelling") }
fn snapshot_status(job: &Job) -> Result<RunStatus, String> {
    job.status.lock().map(|value| value.clone()).map_err(|_| "evaluation status unavailable".into())
}
fn set_budget(job: &Job, budget: &RecallBudget) {
    if let Ok(mut current) = job.budget.lock() { *current = Some(budget.clone()); }
    if job.cancelled.load(Ordering::Acquire) { budget.cancel(); }
}
fn read_report(id: &str) -> Result<EvaluationReport, String> {
    let mut report: EvaluationReport = serde_json::from_str(&read_text(&run_path(id)?.join("report.json"))?)
        .map_err(|_| "evaluation report is invalid")?;
    if report.run_id != id || report.schema_version != 1 { return Err("evaluation report identity mismatch".into()); }
    let live = state().lock().ok().and_then(|state| state.jobs.get(id)
        .map(|job| snapshot_status(job).is_ok_and(|status| is_active(&status.status)))).unwrap_or(false);
    if is_active(&report.status) && !live { report.status = "failed".into(); }
    Ok(report)
}

#[tauri::command]
pub(crate) async fn memory_retrieval_eval_datasets() -> Result<Vec<DatasetSummary>, String> {
    tauri::async_runtime::spawn_blocking(|| {
        let mut result = vec![dataset::summary(&dataset::builtin()?), dataset::summary(&dataset::builtin_evidence()?)];
        let directory = root()?.join("datasets");
        if directory.exists() {
            for entry in fs::read_dir(directory).map_err(|_| "evaluation datasets unavailable")?.take(200) {
                let Ok(entry) = entry else { continue };
                let path = entry.path();
                let Some(id) = path.file_stem().and_then(|name| name.to_str()) else { continue };
                if path.extension().and_then(|value| value.to_str()) != Some("json") || dataset::is_builtin(id) { continue; }
                if let Ok(value) = load_dataset(id) { result.push(dataset::summary(&value)); }
            }
        }
        result.sort_by(|left, right| dataset::is_builtin(&right.dataset_id).cmp(&dataset::is_builtin(&left.dataset_id))
            .then_with(|| left.name.cmp(&right.name)).then_with(|| left.dataset_id.cmp(&right.dataset_id)));
        Ok(result)
    }).await.map_err(|_| "dataset listing interrupted".to_string())?
}

#[tauri::command]
pub(crate) async fn memory_retrieval_eval_import(path: String) -> Result<DatasetSummary, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let value = dataset::parse_and_validate(&read_text(Path::new(&path))?)?;
        if dataset::is_builtin(&value.dataset_id) { return Err("choose a new ID for the imported dataset".into()); }
        let directory = root()?.join("datasets");
        fs::create_dir_all(&directory).map_err(|_| "evaluation dataset directory unavailable")?;
        let target = directory.join(format!("{}.json", value.dataset_id));
        let bytes = serde_json::to_vec_pretty(&value).map_err(|_| "dataset serialization failed")?;
        if bytes.len() as u64 > MAX_FILE_BYTES { return Err("formatted dataset exceeds 10 MiB".into()); }
        // Publish only complete files, without replacing an earlier labelled corpus.
        let mut temporary = tempfile::NamedTempFile::new_in(&directory).map_err(|_| "dataset output unavailable")?;
        temporary.write_all(&bytes).map_err(|_| "dataset could not be saved")?;
        temporary.persist_noclobber(target).map_err(|_| "dataset ID already exists or cannot be saved")?;
        Ok(dataset::summary(&value))
    }).await.map_err(|_| "dataset import interrupted".to_string())?
}

#[tauri::command]
pub(crate) async fn memory_retrieval_eval_plan(
    telemetry: tauri::State<'_, TelemetryStore>, dataset_id: String, split: String,
    mode: String, budget_ms: Option<u64>,
) -> Result<EvaluationPlan, String> {
    let store = telemetry.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        if !matches!(split.as_str(), "development" | "holdout" | "all") || !matches!(mode.as_str(), "hybrid" | "keyword") {
            return Err("invalid evaluation split or mode".into());
        }
        let dataset = load_dataset(&dataset_id)?;
        let cases: Vec<_> = dataset.cases.iter().filter(|case| split == "all" || case.split == split).collect();
        if cases.is_empty() { return Err("the selected split has no cases".into()); }
        let routing = crate::memory_domain::load_config(&store)?;
        let embedding = if mode == "hybrid" {
            Some(crate::memory_backend::semantic_config_read_raw(&store)?
                .ok_or("native embedding is not configured; configure it or explicitly choose keyword evaluation")?)
        } else { None };
        let embedding_plan = embedding.as_ref().map(|value| evaluation_embedding::plan(value.clone(), &snapshots(&dataset))).transpose()?;
        let routing_remote = routing.enabled && reqwest::Url::parse(&routing.endpoint).ok().and_then(|url| url.host_str().map(str::to_string))
            .is_some_and(|host| host != "localhost" && !host.trim_matches(['[', ']']).parse::<std::net::IpAddr>().is_ok_and(|ip| ip.is_loopback()));
        let routing_calls = if routing.enabled { cases.iter().filter(|case| case.domain_mode == "auto" && case.domains.is_empty()).count() } else { 0 };
        let embedding_batches = embedding_plan.as_ref().map_or(0, |value| value.batches);
        let query_calls = if embedding.is_some() { cases.len() } else { 0 };
        let public = EvaluationPlan {
            plan_id: uuid::Uuid::new_v4().to_string(), dataset_id: dataset.dataset_id.clone(), dataset_name: dataset.name.clone(),
            synthetic: dataset.synthetic, dataset_fingerprint: dataset::fingerprint(&dataset), split, mode,
            memory_count: dataset.memories.len(), case_count: cases.len(), embedding_batches, query_calls, routing_calls,
            remote_calls_estimate: (if embedding_plan.as_ref().is_some_and(|value| value.remote) { embedding_batches + query_calls } else { 0 })
                + (if routing_remote { routing_calls } else { 0 }),
            budget_ms: budget_ms.unwrap_or(super::DEFAULT_BUDGET_MS).clamp(100, super::DEFAULT_BUDGET_MS),
            backend: BackendInfo {
                embedding_model: embedding_plan.as_ref().map(|value| value.model.clone()),
                embedding_location: embedding_plan.as_ref().map(|value| if value.remote { "remote" } else { "loopback" }).unwrap_or("disabled").into(),
                routing_model: routing.enabled.then(|| routing.model.clone()),
                routing_provider: if routing.enabled { routing.provider.clone() } else { "disabled".into() }, routing_remote,
            },
            expires_at: (chrono::Utc::now() + chrono::Duration::minutes(10)).to_rfc3339(),
        };
        let mut state = state().lock().map_err(|_| "evaluation planner unavailable")?;
        state.plans.retain(|_, plan| plan.expires > Instant::now());
        if state.plans.len() >= 4 { state.plans.clear(); }
        state.plans.insert(public.plan_id.clone(), PendingPlan { public: public.clone(), dataset, embedding, routing, expires: Instant::now() + Duration::from_secs(600) });
        Ok(public)
    }).await.map_err(|_| "evaluation planning interrupted".to_string())?
}

#[tauri::command]
pub(crate) fn memory_retrieval_eval_start(plan_id: String, confirmed: bool) -> Result<RunStatus, String> {
    if !confirmed { return Err("explicit confirmation is required before model calls".into()); }
    let mut state = state().lock().map_err(|_| "evaluation service unavailable")?;
    if state.jobs.values().any(|job| snapshot_status(job).map_or(true, |status| is_active(&status.status))) {
        return Err("an evaluation is already running".into());
    }
    let plan = state.plans.remove(&plan_id).ok_or("evaluation plan expired; preview it again")?;
    if plan.expires <= Instant::now() { return Err("evaluation plan expired; preview it again".into()); }
    let run_id = uuid::Uuid::new_v4().to_string();
    let status = RunStatus { run_id: run_id.clone(), status: "preparing".into(), completed: 0,
        total: plan.public.case_count, prepared: 0, total_memories: plan.public.memory_count, error: None };
    let job = Arc::new(Job { status: Mutex::new(status.clone()), cancelled: AtomicBool::new(false), budget: Mutex::new(None) });
    state.jobs.clear();
    state.jobs.insert(run_id.clone(), job.clone());
    state.last_run = Some(run_id.clone());
    drop(state);
    let monitor = job.clone();
    tauri::async_runtime::spawn(async move {
        let result = tauri::async_runtime::spawn_blocking(move || run(plan, &job)).await;
        let error = match result { Ok(Ok(())) => None, Ok(Err(error)) => Some(error), Err(_) => Some("evaluation worker interrupted".into()) };
        if let Some(error) = error {
            if let Ok(mut status) = monitor.status.lock() { status.status = "failed".into(); status.error = Some(error); }
        }
    });
    Ok(status)
}

fn run(plan: PendingPlan, job: &Job) -> Result<(), String> {
    let run_id = snapshot_status(job)?.run_id;
    let directory = run_path(&run_id)?;
    fs::create_dir_all(&directory).map_err(|_| "evaluation run directory unavailable")?;
    write_json(&root()?.join("last-run.json"), &run_id)?;
    write_json(&directory.join("dataset.json"), &plan.dataset)?;
    let mut report = EvaluationReport {
        schema_version: 1, run_id, dataset_id: plan.public.dataset_id.clone(), dataset_name: plan.public.dataset_name.clone(),
        synthetic: plan.public.synthetic, dataset_fingerprint: plan.public.dataset_fingerprint.clone(), split: plan.public.split.clone(), mode: plan.public.mode.clone(),
        status: "preparing".into(), planned_count: plan.public.case_count, started_at: now(), finished_at: None,
        preparation_ms: 0, backend: plan.public.backend.clone(), cases: Vec::new(), summary: metrics::aggregate(&[], plan.public.case_count),
    };
    write_json(&directory.join("report.json"), &report)?;
    let preparation = Instant::now();
    let mut preparation_finished = false;
    let execution = (|| -> Result<(), String> {
        let store = TelemetryStore::isolated_for_retrieval_evaluation(&directory.join("corpus.sqlite3"))?;
        seed(&store, &plan.dataset)?;
        let cache = directory.join("vectors.sqlite3");
        if let Some(config) = &plan.embedding {
            let budget = RecallBudget::new(Duration::from_secs(600));
            set_budget(job, &budget);
            evaluation_embedding::prepare(config.clone(), &snapshots(&plan.dataset), &cache, &budget, |prepared| {
                if let Ok(mut status) = job.status.lock() { status.prepared = prepared; }
            })?;
        } else if let Ok(mut status) = job.status.lock() { status.prepared = plan.public.memory_count; }
        report.preparation_ms = preparation.elapsed().as_millis() as u64;
        preparation_finished = true;
        if job.cancelled.load(Ordering::Acquire) { return Ok(()); }
        if let Ok(mut status) = job.status.lock() { status.status = "running".into(); }
        report.status = "running".into();
        for case in selected_cases(&plan) {
            if job.cancelled.load(Ordering::Acquire) { break; }
            let budget = RecallBudget::new(Duration::from_millis(plan.public.budget_ms));
            set_budget(job, &budget);
            let mut arguments = json!({"query":case.query,"limit":case.limit,"domain_mode":case.domain_mode});
            if !case.domains.is_empty() { arguments["domains"] = json!(case.domains); }
            let started = Instant::now();
            let response = super::recall_for_evaluation(&store, &arguments, plan.public.budget_ms, &budget,
                plan.embedding.as_ref().map(|_| cache.as_path()), plan.embedding.as_ref(), &plan.routing);
            // A request already sent may finish while cancellation is pending.
            // Preserve its observed result, then stop before another model call.
            let success = response.is_ok();
            let response = response.unwrap_or(Value::Null);
            let returned_ids: Vec<_> = response["memories"].as_array().into_iter().flatten()
                .filter_map(|item| item["id"].as_str().map(str::to_string)).collect();
            let active_domains: Vec<_> = response["routing"]["active_domains"].as_array().into_iter().flatten()
                .filter_map(|item| item.as_str().map(str::to_string)).collect();
            let (evidence_coverage, evidence_status_hit, evidence_ms) = evaluate_evidence(
                &store, &case, &response, &returned_ids, &active_domains, &budget, job);
            let elapsed_ms = started.elapsed().as_millis() as u64;
            let domain_hit = if case.expected_domain_ids.is_empty() { None } else {
                let expected: HashSet<_> = case.expected_domain_ids.iter().collect();
                let actual: HashSet<_> = active_domains.iter().collect();
                Some(success && expected == actual)
            };
            let mut retrieval = if response["retrieval"].is_object() { response["retrieval"].clone() } else { json!({}) };
            retrieval["routing_fallback"] = response["routing"]["fallback"].clone();
            let routing_degraded = retrieval["routing_fallback"].as_str().is_some_and(|reason| reason != "low_confidence");
            let mode = retrieval["mode"].as_str().unwrap_or("");
            let index_complete = plan.embedding.is_none() || (mode == "semantic" && retrieval["index"]["pending"].as_u64() == Some(0));
            let returned_chars = retrieval["injected_chars"].as_u64().unwrap_or(0) as usize;
            report.cases.push(CaseResult {
                case_id: case.case_id, category: case.category, split: case.split, query: case.query,
                status: if success { "success" } else { "failed" }.into(), error: (!success).then(|| "retrieval_failed".into()),
                metrics: metrics::evaluate(&case.expected_ids, &returned_ids, &case.forbidden_ids, case.limit),
                expected_ids: case.expected_ids, forbidden_ids: case.forbidden_ids, returned_ids,
                expected_domain_ids: case.expected_domain_ids, active_domains, domain_hit,
                degraded: (plan.embedding.is_some() && mode != "semantic") || routing_degraded, index_complete, retrieval,
                elapsed_ms, returned_chars, evidence_coverage, evidence_status_hit, evidence_ms,
            });
            report.summary = metrics::aggregate(&report.cases, report.planned_count);
            write_json(&directory.join("report.json"), &report)?;
            if let Ok(mut status) = job.status.lock() { status.completed = report.cases.len(); }
        }
        Ok(())
    })();
    if !preparation_finished { report.preparation_ms = preparation.elapsed().as_millis() as u64; }
    let cancelled = job.cancelled.load(Ordering::Acquire);
    report.status = if cancelled { "cancelled" } else if execution.is_err() { "failed" } else { "completed" }.into();
    report.finished_at = Some(now());
    append_unexecuted(&plan, &mut report);
    report.summary = metrics::aggregate(&report.cases, report.planned_count);
    write_json(&directory.join("report.json"), &report)?;
    if let Ok(mut current) = job.budget.lock() { *current = None; }
    if let Ok(mut status) = job.status.lock() {
        status.status = report.status.clone(); status.completed = report.summary.executed_count;
        status.error = if cancelled { None } else { execution.err() };
    }
    Ok(())
}

#[tauri::command]
pub(crate) async fn memory_retrieval_eval_status(run_id: Option<String>) -> Result<Option<RunStatus>, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let requested = {
            let state = state().lock().map_err(|_| "evaluation status unavailable")?;
            let id = run_id.or_else(|| state.last_run.clone());
            if let Some(job) = id.as_ref().and_then(|id| state.jobs.get(id)) { return snapshot_status(job).map(Some); }
            id
        };
        let id = match requested {
            Some(id) => id,
            None => {
                let pointer = root()?.join("last-run.json");
                if !pointer.exists() { return Ok(None); }
                serde_json::from_str::<String>(&read_text(&pointer)?).map_err(|_| "last evaluation pointer is invalid")?
            }
        };
        // A crash before the first report publication must not permanently
        // disable the next explicit run through a dangling last-run pointer.
        if !run_path(&id)?.join("report.json").exists() { return Ok(None); }
        let report = read_report(&id)?;
        let interrupted = report.finished_at.is_none() && report.status == "failed";
        Ok(Some(RunStatus { run_id: id, status: if interrupted { "failed".into() } else { report.status },
            completed: report.summary.executed_count, total: report.planned_count, prepared: 0, total_memories: 0,
            error: interrupted.then(|| "evaluation interrupted; completed cases were retained".into()),
        }))
    }).await.map_err(|_| "evaluation status read interrupted".to_string())?
}

#[tauri::command]
pub(crate) fn memory_retrieval_eval_cancel(run_id: String) -> Result<RunStatus, String> {
    let state = state().lock().map_err(|_| "evaluation service unavailable")?;
    let job = state.jobs.get(&run_id).ok_or("evaluation is no longer active")?;
    job.cancelled.store(true, Ordering::Release);
    if let Ok(current) = job.budget.lock() { if let Some(budget) = current.as_ref() { budget.cancel(); } }
    let mut status = job.status.lock().map_err(|_| "evaluation status unavailable")?;
    if is_active(&status.status) { status.status = "cancelling".into(); }
    Ok(status.clone())
}

#[tauri::command]
pub(crate) async fn memory_retrieval_eval_report(run_id: String) -> Result<EvaluationReport, String> {
    tauri::async_runtime::spawn_blocking(move || read_report(&run_id)).await
        .map_err(|_| "evaluation report read interrupted".to_string())?
}

#[tauri::command]
pub(crate) async fn memory_retrieval_eval_export(id: String, kind: String, path: String) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || {
        let content = match kind.as_str() {
            "dataset" => serde_json::to_string_pretty(&load_dataset(&id)?).map_err(|_| "dataset export failed")?,
            "json" => serde_json::to_string_pretty(&read_report(&id)?).map_err(|_| "report export failed")?,
            "markdown" => metrics::markdown(&read_report(&id)?),
            _ => return Err("unsupported evaluation export format".into()),
        };
        fs::write(Path::new(&path), content).map_err(|_| "evaluation export could not be saved".into())
    }).await.map_err(|_| "evaluation export interrupted".to_string())?
}
