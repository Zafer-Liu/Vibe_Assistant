//! Reviewable L1 refinement. Model output is a proposal, never a database command.
use crate::{llm, telemetry_store::TelemetryStore};
use serde::{Deserialize, Serialize};
#[cfg(test)]
use serde_json::json;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::{atomic::{AtomicBool, Ordering}, Arc, Mutex};
use std::time::Duration;

mod review;
mod proposal_json;

pub(crate) const RULE_VERSION: &str = "l1-refinement-by-label-v2";
static ACTIVE: Mutex<Option<(String, Arc<AtomicBool>)>> = Mutex::new(None);
static SUGGESTING: Mutex<Vec<(String, String)>> = Mutex::new(Vec::new());

struct SuggestionGuard((String, String));
impl Drop for SuggestionGuard {
    fn drop(&mut self) {
        if let Ok(mut active) = SUGGESTING.lock() { active.retain(|key| key != &self.0); }
    }
}

struct ActiveGuard(String);

impl Drop for ActiveGuard {
    fn drop(&mut self) {
        if let Ok(mut active) = ACTIVE.lock() {
            if active.as_ref().is_some_and(|(id, _)| id == &self.0) { *active = None; }
        }
    }
}

#[derive(Clone, Serialize, Deserialize)]
pub struct RefinementMemory {
    pub id: String,
    pub memory: String,
    pub memory_type: String,
    pub durability: String,
    pub source: String,
    pub session_id: String,
    #[serde(default)]
    pub source_event_key: String,
    #[serde(default)]
    pub ordinal: i64,
    pub event_time: Option<String>,
    pub updated_at: String,
    pub domains: Vec<String>,
    pub source_excerpt: Option<String>,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct RefinementAction {
    pub id: String,
    pub kind: String,
    pub originals: Vec<RefinementMemory>,
    pub content: String,
    pub memory_type: String,
    pub durability: String,
    pub reason: String,
    pub evidence: Vec<String>,
    pub status: String,
    pub message: Option<String>,
    #[serde(default)]
    pub recommended_source_id: Option<String>,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct RefinementRun {
    pub id: String,
    pub created_at: String,
    pub updated_at: String,
    pub status: String,
    pub domain_id: Option<String>,
    pub incremental: bool,
    pub total: usize,
    pub processed: usize,
    pub protected: usize,
    pub remaining: usize,
    pub actions: Vec<RefinementAction>,
    pub warnings: Vec<String>,
    #[serde(default)]
    pub activity: Option<String>,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct RefinementHistorySnapshot {
    pub count: usize,
    pub revision: String,
}

#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct MemoryState {
    pub view: RefinementMemory,
    pub row: serde_json::Value,
    pub domains: Vec<serde_json::Value>,
    pub provenance: Vec<serde_json::Value>,
    pub layers: Vec<serde_json::Value>,
    pub importance: Vec<serde_json::Value>,
    pub signature: String,
    pub protected: bool,
}

#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct AppliedChange {
    pub action_id: String,
    pub before: Vec<MemoryState>,
    pub after: Vec<MemoryState>,
    pub removed_ids: Vec<String>,
}

#[derive(Clone, Default, Serialize, Deserialize)]
pub(crate) struct SemanticProgress {
    pub checks: usize,
    pub degraded: usize,
    pub legacy_history: bool,
}

#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct StoredRun {
    pub view: RefinementRun,
    #[serde(skip_serializing, default)]
    pub inventory: Vec<MemoryState>,
    #[serde(skip_serializing, default)]
    pub tasks: Vec<Vec<usize>>,
    #[serde(skip_serializing, default)]
    pub task_domains: Vec<String>,
    #[serde(skip_serializing, default)]
    pub task_targets: Vec<Vec<usize>>,
    #[serde(skip_serializing, default)]
    pub targets: HashSet<usize>,
    pub reviewed: HashSet<usize>,
    #[serde(default)]
    pub quality_reviewed: HashSet<usize>,
    #[serde(default)]
    pub seen_pairs: HashSet<(String, String)>,
    #[serde(default)]
    pub degraded_targets: HashSet<usize>,
    #[serde(default)]
    pub retry_tasks: HashSet<usize>,
    #[serde(default)]
    pub completed_tasks: HashSet<usize>,
    #[serde(skip_serializing, default)]
    pub review_version: String,
    pub cursor: usize,
    pub changes: Vec<AppliedChange>,
    #[serde(default)]
    pub semantic_progress: Option<SemanticProgress>,
}

#[derive(Deserialize)]
pub struct RefinementSelection {
    pub action_id: String,
    pub content: Option<String>,
    /// None inherits source labels; Some(empty) explicitly selects unclassified.
    #[serde(default)]
    pub domain_ids: Option<Vec<String>>,
    /// Submitted only on user confirmation; may accept the generated recommendation.
    #[serde(default)]
    pub conflict_source_id: Option<String>,
}

#[derive(Deserialize)]
pub struct RefinementDeleteSelection {
    pub action_id: String,
    pub expected_status: String,
}

#[derive(Deserialize)]
struct Proposal {
    kind: String,
    memory_ids: Vec<String>,
    #[serde(default)]
    content: String,
    #[serde(default)]
    memory_type: String,
    #[serde(default)]
    durability: String,
    reason: String,
    #[serde(default)]
    evidence: Vec<String>,
    #[serde(default)]
    recommended_source_id: Option<String>,
}

#[derive(Deserialize)]
struct ProposalResponse {
    actions: Vec<Proposal>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn saved_timeout_warning_clears_on_resume_or_completion_but_keeps_other_errors() {
        let mut run = empty_run();
        let warning = "精炼模型本批超时；已生成的建议保留，可继续分析";
        run.view.remaining = 1;
        run.view.status = "failed".into();
        run.view.warnings = vec![warning.into(), "HTTP 401".into()];
        normalize_notices(&mut run.view);
        assert_eq!(run.view.warnings.len(), 2);
        run.view.status = "analyzing".into();
        normalize_notices(&mut run.view);
        assert_eq!(run.view.warnings, vec!["HTTP 401"]);
        run.view.warnings.push(warning.into());
        run.view.status = "ready".into();
        run.view.remaining = 0;
        normalize_notices(&mut run.view);
        assert_eq!(run.view.warnings, vec!["HTTP 401"]);
    }

    #[test]
    fn resume_clears_old_errors_without_hiding_semantic_degradation() {
        let mut run = empty_run();
        run.view.status = "analyzing".into();
        run.view.remaining = 2;
        run.view.warnings = vec![
            "精炼模型没有返回有效的 JSON 建议".into(),
            "模型请求失败（GLM）: 无法建立 HTTPS 连接（error sending request）".into(),
            "语义候选检索未就绪或暂不可用，本批使用关键词候选；不会记为完整检查，下次增量分析会重试".into(),
            semantic_review_notice("semantic_partial_index").unwrap(),
            "模型返回 HTTP 401: invalid key".into(),
        ];
        normalize_notices(&mut run.view);
        assert_eq!(run.view.warnings.len(), 2);
        assert!(run.view.warnings.iter().any(|warning| warning.starts_with("语义候选当前批：")));
        assert!(run.view.warnings.iter().any(|warning| warning.contains("HTTP 401")));
        run.view.status = "ready".into();
        run.view.remaining = 0;
        normalize_notices(&mut run.view);
        assert_eq!(run.view.warnings.len(), 2);
        assert!(semantic_review_notice("semantic").is_none());
        assert!(semantic_review_notice("local_fallback_budget").unwrap().contains("30 秒"));
        assert!(semantic_review_notice("local_fallback_runtime").unwrap().contains("未运行"));
    }

    #[test]
    fn successful_semantic_query_clears_current_fault_but_keeps_retry_coverage_and_counts() {
        let mut run = empty_run();
        run.view.status = "analyzing".into();
        run.degraded_targets.insert(0);
        record_semantic_result(&mut run, "local_fallback_connect_timeout");
        assert!(run.view.warnings.iter().any(|warning| warning.starts_with("语义候选当前批：") && warning.contains("连接阶段")));
        record_semantic_result(&mut run, "semantic");
        normalize_notices(&mut run.view);
        assert!(!run.view.warnings.iter().any(|warning| warning.starts_with("语义候选当前批：")));
        assert!(run.view.warnings.iter().any(|warning| warning.starts_with("语义候选历史：") && warning.contains("已恢复")));
        let progress = run.semantic_progress.as_ref().unwrap();
        assert_eq!((progress.checks, progress.degraded), (2, 1));
        assert!(run.degraded_targets.contains(&0));
        assert_eq!(semantic_error_mode("embedding connection timed out"), "local_fallback_connect_timeout");
        assert_eq!(semantic_error_mode("embedding response timed out"), "local_fallback_response_timeout");
        assert_eq!(semantic_error_mode("refinement lease timed out"), "local_fallback_lease");
        assert_eq!(semantic_error_mode("recall_deadline"), "local_fallback_budget");
    }

    #[test]
    fn legacy_semantic_warning_is_explicitly_historical_without_claiming_live_status() {
        let mut run = empty_run();
        run.view.warnings.push("语义候选降级：语义查询超过 8 秒预算，已使用关键词；本批不记为完整关系检查，后续增量分析会重试".into());
        normalize_notices(&mut run.view);
        assert!(run.view.warnings[0].starts_with("语义候选历史（旧版记录）："));
        record_semantic_result(&mut run, "semantic");
        assert!(run.semantic_progress.as_ref().unwrap().legacy_history);
        assert!(run.view.warnings.iter().all(|warning| !warning.starts_with("语义候选当前批：")));
        assert!(run.view.warnings[0].contains("次数未记录"));
    }

    #[test]
    fn semantic_candidate_cancellation_precedes_configuration_or_network_work() {
        let store = TelemetryStore::in_memory();
        let budget = crate::memory_retrieval::RecallBudget::new(Duration::from_secs(8));
        budget.cancel();
        let error = crate::memory_backend::refinement_semantic_neighbours(&store, "id", "query", None, &budget).err().unwrap();
        assert_eq!(error, "recall_cancelled");
    }

    fn memory(id: &str, domains: &[&str]) -> MemoryState {
        MemoryState {
            view: RefinementMemory {
                id: id.into(), memory: "A project fact".into(), memory_type: "fact".into(),
                durability: "short_term".into(), source: "test".into(), session_id: id.into(),
                source_event_key: id.into(), ordinal: 0, event_time: None, updated_at: "now".into(),
                domains: domains.iter().map(|id| id.to_string()).collect(), source_excerpt: None,
            }, row: json!({}), domains: Vec::new(), provenance: Vec::new(), layers: Vec::new(),
            importance: Vec::new(), signature: id.into(), protected: false,
        }
    }

    fn proposal(kind: &str, ids: &[&str]) -> Proposal {
        Proposal {
            kind: kind.into(), memory_ids: ids.iter().map(|id| id.to_string()).collect(),
            content: "A concise supported project fact".into(), memory_type: "fact".into(),
            durability: "short_term".into(), reason: "Supported by the original records".into(),
            evidence: vec!["Source fact".into()],
            recommended_source_id: (kind == "conflict").then(|| ids[0].to_string()),
        }
    }

    fn empty_run() -> StoredRun {
        TelemetryStore::in_memory().refinement_prepare(None, false).unwrap()
    }

    #[tokio::test]
    async fn refinement_database_work_runs_off_the_calling_thread() {
        let caller = std::thread::current().id();
        let worker = refinement_io(|| Ok(std::thread::current().id())).await.unwrap();
        assert_ne!(caller, worker);
    }

    #[tokio::test]
    async fn refinement_worker_failure_is_returned_as_an_error() {
        let error = refinement_io(|| -> Result<(), String> { panic!("refinement worker fixture") }).await.unwrap_err();
        assert!(error.contains("精炼后台任务失败"));
    }

    #[test]
    fn reversed_json_delimiters_return_an_error() {
        assert!(parse_proposals("} text {").is_err());
    }

    #[test]
    fn length_retry_uses_pairs_before_individual_quality_and_never_repeats_a_unit() {
        let pairs = HashSet::from([pair_key("a", "b"), pair_key("a", "context")]);
        let quality = HashSet::from(["a".to_string(), "b".to_string()]);
        let mut attempted = HashSet::new();
        let first = next_retry_unit(&pairs, &quality, &attempted).unwrap();
        assert_eq!(first, ReviewUnit::Pair("a".into(), "b".into()));
        attempted.insert(first);
        let second = next_retry_unit(&pairs, &quality, &attempted).unwrap();
        assert_eq!(second, ReviewUnit::Pair("a".into(), "context".into()));
        attempted.insert(second);
        for id in ["a", "b"] {
            let unit = next_retry_unit(&pairs, &quality, &attempted).unwrap();
            assert_eq!(unit, ReviewUnit::Single(id.into()));
            assert!(attempted.insert(unit));
        }
        assert!(next_retry_unit(&pairs, &quality, &attempted).is_none());
    }

    #[test]
    fn accepted_partial_review_leaves_only_unseen_work_for_resume() {
        let mut run = empty_run();
        run.inventory = vec![memory("a", &["law"]), memory("b", &["law"]), memory("c", &["law"])];
        let candidates = run.inventory.clone();
        let pairs = HashSet::from([pair_key("a", "b")]);
        let quality = HashSet::from(["a".into(), "b".into()]);
        accept_review(&mut run, &candidates[..2], Vec::new(), "law", &pairs, &quality, &[0, 1, 2]);
        assert_eq!(run.quality_reviewed, HashSet::from([0, 1]));
        assert_eq!(pending_quality(&run, &[0, 1, 2]), HashSet::from(["c".into()]));
        let remaining = allowed_pairs(&run, &candidates, "law");
        assert!(!remaining.contains(&pair_key("a", "b")));
        assert_eq!(remaining.len(), 2);
    }

    #[test]
    fn a_pair_is_not_judged_again_under_its_second_label() {
        let mut run = empty_run();
        let mut candidates = vec![memory("a", &["alpha", "beta"]), memory("b", &["alpha", "beta"])];
        let first = allowed_pairs(&run, &candidates, "alpha");
        assert_eq!(first.len(), 1);
        run.seen_pairs.extend(first);
        assert!(allowed_pairs(&run, &candidates, "beta").is_empty());
        candidates.push(memory("c", &["beta"]));
        assert_eq!(allowed_pairs(&run, &candidates, "beta").len(), 2, "new relationships still need review");
    }

    #[test]
    fn proposal_validation_rejects_cross_label_members_even_if_whitelisted() {
        let mut run = empty_run();
        let candidates = vec![memory("a", &["alpha"]), memory("b", &["beta"])];
        let pairs = HashSet::from([pair_key("a", "b")]);
        accept_proposals(&mut run, &candidates, vec![proposal("merge", &["a", "b"])],
            "alpha", &pairs, &HashSet::new());
        assert!(run.view.actions.is_empty());
        let same_label = vec![memory("a", &["alpha"]), memory("b", &["alpha"])];
        accept_proposals(&mut run, &same_label, vec![proposal("merge", &["a", "b"])],
            "alpha", &pairs, &HashSet::new());
        assert_eq!(run.view.actions.len(), 1);
    }

    #[test]
    fn unclassified_records_only_allow_requested_single_record_quality_changes() {
        let mut run = empty_run();
        let candidates = vec![memory("a", &[]), memory("b", &["unclassified"])];
        assert!(allowed_pairs(&run, &candidates, "unclassified").is_empty());
        accept_proposals(&mut run, &candidates, vec![proposal("merge", &["a", "b"])],
            "unclassified", &HashSet::from([pair_key("a", "b")]), &HashSet::new());
        assert!(run.view.actions.is_empty());
        accept_proposals(&mut run, &candidates, vec![proposal("rewrite", &["a"])],
            "unclassified", &HashSet::new(), &HashSet::from(["a".into()]));
        assert_eq!(run.view.actions.len(), 1);
        accept_proposals(&mut run, &candidates, vec![proposal("rewrite", &["b"])],
            "unclassified", &HashSet::new(), &HashSet::new());
        assert_eq!(run.view.actions.len(), 1, "quality review is not repeated as label context");
    }

    #[test]
    fn conflict_keeps_generated_content_and_uses_the_recommended_existing_source() {
        let mut run = empty_run();
        let mut candidates = vec![memory("a", &["work"]), memory("b", &["work"])];
        candidates[0].view.memory = "Old footnote scheme".into();
        candidates[1].view.memory_type = "summary".into();
        candidates[1].view.durability = "session".into();
        let mut draft = proposal("conflict", &["a", "b"]);
        draft.content = "Footnotes were later withdrawn; the final report uses inline source links".into();
        draft.recommended_source_id = Some("b".into());
        accept_proposals(&mut run, &candidates, vec![draft], "work", &HashSet::from([pair_key("a", "b")]), &HashSet::new());
        assert_eq!(run.view.actions.len(), 1);
        let action = &run.view.actions[0];
        assert!(action.content.contains("inline source links"));
        assert_ne!(action.content, candidates[0].view.memory);
        assert_eq!(action.recommended_source_id.as_deref(), Some("b"));
        assert_eq!(action.memory_type, "summary");
        assert_eq!(action.durability, "session");
        let mut invalid = proposal("conflict", &["a", "b"]);
        invalid.recommended_source_id = Some("outside-group".into());
        accept_proposals(&mut run, &candidates, vec![invalid], "work", &HashSet::from([pair_key("a", "b")]), &HashSet::new());
        assert_eq!(run.view.actions.len(), 1);
    }

    #[test]
    fn a_memory_with_a_planned_change_is_excluded_from_later_merge_pairs() {
        let mut run = empty_run();
        let candidates = vec![memory("a", &["alpha"]), memory("b", &["alpha"])];
        accept_proposals(&mut run, &candidates, vec![proposal("rewrite", &["a"])],
            "alpha", &HashSet::new(), &HashSet::from(["a".into()]));
        assert!(allowed_pairs(&run, &candidates, "alpha").is_empty());
    }
}

fn parse_proposals(text: &str) -> Result<Vec<Proposal>, String> {
    proposal_json::parse(text)
}

// Lexical neighbours are a bounded fallback, not a duplicate decision. The
// configured semantic index supplies additional cross-wording neighbours later.
fn terms(text: &str) -> HashSet<String> {
    let mut result = HashSet::new();
    let mut latin = String::new();
    let mut previous = None;
    for ch in text.chars().take(4000) {
        if ch.is_ascii_alphanumeric() || ch == '_' {
            latin.push(ch.to_ascii_lowercase());
        } else if !latin.is_empty() {
            if latin.len() > 2 { result.insert(std::mem::take(&mut latin)); }
            latin.clear();
        }
        if ('\u{4e00}'..='\u{9fff}').contains(&ch) {
            if let Some(left) = previous { result.insert(format!("{left}{ch}")); }
            previous = Some(ch);
        } else { previous = None; }
    }
    if latin.len() > 2 { result.insert(latin); }
    result
}

#[derive(Default)]
pub(crate) struct TaskPlan {
    pub tasks: Vec<Vec<usize>>,
    pub domains: Vec<String>,
    pub targets: Vec<Vec<usize>>,
}

pub(crate) fn in_domain(memory: &RefinementMemory, domain: &str) -> bool {
    if domain == "unclassified" {
        !memory.domains.iter().any(|id| !id.is_empty() && id != "unclassified")
    } else {
        memory.domains.iter().any(|id| id == domain)
    }
}

pub(crate) fn build_tasks(inventory: &[MemoryState], targets: &HashSet<usize>, scope: Option<&str>) -> TaskPlan {
    let vocabulary: Vec<_> = inventory.iter().map(|memory| terms(&memory.view.memory)).collect();
    let mut groups = BTreeMap::<String, Vec<usize>>::new();
    for (index, memory) in inventory.iter().enumerate() {
        if let Some(domain) = scope {
            if in_domain(&memory.view, domain) { groups.entry(domain.to_string()).or_default().push(index); }
        } else {
            let labels: HashSet<_> = memory.view.domains.iter()
                .filter(|id| !id.is_empty() && id.as_str() != "unclassified").collect();
            if labels.is_empty() { groups.entry("unclassified".into()).or_default().push(index); }
            else { for label in labels { groups.entry(label.to_string()).or_default().push(index); } }
        }
    }
    let mut plan = TaskPlan::default();
    for (domain, members) in groups {
        let mut pending: HashSet<_> = members.iter().copied().filter(|index| targets.contains(index)).collect();
        if domain == "unclassified" {
            // Missing labels are not evidence of a shared topic. Review quality
            // one record at a time; never group unknown subjects for merging.
            for index in members.into_iter().filter(|index| targets.contains(index)) {
                plan.tasks.push(vec![index]);
                plan.targets.push(vec![index]);
                plan.domains.push(domain.clone());
            }
            continue;
        }
        let mut postings: HashMap<&str, Vec<usize>> = HashMap::new();
        for &index in &members {
            for word in &vocabulary[index] { postings.entry(word).or_default().push(index); }
        }
        for (position, &anchor) in members.iter().enumerate() {
            if !pending.remove(&anchor) { continue; }
            let mut scores = HashMap::<usize, usize>::new();
            for word in &vocabulary[anchor] {
                if let Some(indices) = postings.get(word.as_str()) {
                    if indices.len() > 256 { continue; }
                    for &index in indices {
                        if index != anchor { *scores.entry(index).or_default() += 1; }
                    }
                }
            }
            let mut neighbours: Vec<_> = scores.into_iter().filter(|(_, score)| *score >= 2).collect();
            neighbours.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
            let mut batch_targets = vec![anchor];
            let mut context = Vec::new();
            for (index, _) in neighbours {
                if batch_targets.len() < 4 && pending.remove(&index) { batch_targets.push(index); }
                else if !pending.contains(&index) && !batch_targets.contains(&index) && context.len() < 2 { context.push(index); }
            }
            for &index in members.iter().skip(position + 1) {
                if batch_targets.len() >= 4 { break; }
                if pending.remove(&index) { batch_targets.push(index); }
            }
            let mut batch = batch_targets.clone();
            batch.extend(context.into_iter().filter(|index| !batch_targets.contains(index)));
            plan.tasks.push(batch);
            plan.targets.push(batch_targets);
            plan.domains.push(domain.clone());
        }
    }
    plan
}

pub(crate) fn last_target_tasks(run: &StoredRun) -> HashMap<usize, usize> {
    let mut last = HashMap::new();
    for (task, targets) in run.task_targets.iter().enumerate() {
        for &index in targets { last.insert(index, task); }
    }
    last
}

pub(crate) fn finish_batch(run: &mut StoredRun, last_tasks: &HashMap<usize, usize>, semantic_covered: bool) -> Vec<usize> {
    let mut checked = Vec::new();
    for &index in &run.task_targets[run.cursor] {
        if !semantic_covered { run.degraded_targets.insert(index); }
        let retry_pending = run.retry_tasks.iter().any(|task|
            run.task_targets.get(*task).is_some_and(|targets| targets.contains(&index)));
        if last_tasks.get(&index) == Some(&run.cursor) && !retry_pending && run.reviewed.insert(index)
            && !run.degraded_targets.contains(&index) { checked.push(index); }
    }
    run.cursor += 1;
    run.view.processed = run.reviewed.len();
    run.view.remaining = run.view.total.saturating_sub(run.view.processed);
    run.view.updated_at = chrono::Utc::now().to_rfc3339();
    checked
}

fn pair_key(left: &str, right: &str) -> (String, String) {
    if left < right { (left.into(), right.into()) } else { (right.into(), left.into()) }
}

fn allowed_pairs(run: &StoredRun, candidates: &[MemoryState], domain: &str) -> HashSet<(String, String)> {
    let mut pairs = HashSet::new();
    if domain == "unclassified" { return pairs; }
    let occupied: HashSet<_> = run.view.actions.iter().filter(|action| action.kind != "conflict")
        .flat_map(|action| action.originals.iter().map(|memory| memory.id.as_str())).collect();
    for (index, left) in candidates.iter().enumerate() {
        if left.protected || occupied.contains(left.view.id.as_str()) || !in_domain(&left.view, domain) { continue; }
        for right in &candidates[index + 1..] {
            if right.protected || occupied.contains(right.view.id.as_str()) || !in_domain(&right.view, domain) { continue; }
            let pair = pair_key(&left.view.id, &right.view.id);
            if !run.seen_pairs.contains(&pair) { pairs.insert(pair); }
        }
    }
    pairs
}

fn accept_proposals(
    run: &mut StoredRun, candidates: &[MemoryState], proposals: Vec<Proposal>, domain: &str,
    pairs: &HashSet<(String, String)>, quality_ids: &HashSet<String>,
) {
    let by_id: HashMap<_, _> = candidates.iter().map(|state| (state.view.id.as_str(), state)).collect();
    let mut occupied: HashSet<String> = run.view.actions.iter()
        .filter(|action| action.kind != "conflict")
        .flat_map(|action| action.originals.iter().map(|memory| memory.id.clone())).collect();
    for proposal in proposals.into_iter().take(12) {
        if !matches!(proposal.kind.as_str(), "rewrite" | "merge" | "metadata" | "archive" | "conflict")
            || proposal.reason.trim().is_empty() || proposal.reason.chars().count() > 2000
            || proposal.evidence.is_empty()
            || proposal.content.chars().count() > 8000 || proposal.memory_ids.is_empty()
            || proposal.memory_ids.len() > 6 { continue; }
        let mut ids = HashSet::new();
        let originals: Vec<_> = proposal.memory_ids.iter().filter_map(|id| {
            if !ids.insert(id.as_str()) { return None; }
            by_id.get(id.as_str()).copied()
        }).collect();
        if originals.len() != proposal.memory_ids.len()
            || originals.iter().any(|memory| memory.protected || !in_domain(&memory.view, domain))
            || (proposal.kind != "conflict" && originals.iter().any(|memory| occupied.contains(&memory.view.id)))
            || (domain == "unclassified" && originals.len() != 1)
            || (matches!(proposal.kind.as_str(), "merge" | "conflict") && originals.len() < 2)
            || (matches!(proposal.kind.as_str(), "rewrite" | "metadata" | "archive") && originals.len() != 1) { continue; }
        if originals.len() == 1 && !quality_ids.contains(&originals[0].view.id) { continue; }
        if originals.iter().enumerate().any(|(index, left)| originals[index + 1..].iter()
            .any(|right| !pairs.contains(&pair_key(&left.view.id, &right.view.id)))) { continue; }
        let primary = if proposal.kind == "conflict" {
            let Some(source) = proposal.recommended_source_id.as_deref()
                .and_then(|id| originals.iter().find(|memory| memory.view.id == id)) else { continue; };
            &source.view
        } else { &originals[0].view };
        let content = if proposal.kind == "archive" || proposal.kind == "metadata" {
            primary.memory.clone()
        } else { proposal.content.trim().to_string() };
        if content.is_empty() { continue; }
        let memory_type = if proposal.kind == "conflict" || proposal.memory_type.is_empty() { primary.memory_type.clone() } else { proposal.memory_type.clone() };
        let durability = if proposal.kind == "conflict" || proposal.durability.is_empty() { primary.durability.clone() } else { proposal.durability.clone() };
        if !matches!(memory_type.as_str(), "summary" | "fact" | "decision" | "constraint" | "preference_candidate" | "open_item")
            || !matches!(durability.as_str(), "session" | "short_term" | "long_term")
            || (memory_type == "summary" && durability != "session")
            // A cleanup model cannot promote an inferred preference to a
            // durable instruction. A user can still edit memory explicitly.
            || (proposal.kind != "conflict" && durability == "long_term" && originals.iter().any(|memory| memory.view.durability != "long_term")) { continue; }
        if matches!(proposal.kind.as_str(), "rewrite" | "metadata") && content == primary.memory
            && memory_type == primary.memory_type && durability == primary.durability { continue; }
        if proposal.kind != "conflict" {
            occupied.extend(originals.iter().map(|memory| memory.view.id.clone()));
        }
        let recommended_source_id = if proposal.kind == "conflict" { proposal.recommended_source_id } else { None };
        run.view.actions.push(RefinementAction {
            id: uuid::Uuid::new_v4().to_string(), kind: proposal.kind,
            originals: originals.into_iter().map(|memory| memory.view.clone()).collect(),
            content, memory_type, durability, reason: proposal.reason.trim().to_string(),
            evidence: proposal.evidence.into_iter().take(8).map(|item| item.chars().take(800).collect()).collect(),
            status: "pending".into(), message: None, recommended_source_id,
        });
    }
}

pub(crate) fn normalize_notices(view: &mut RefinementRun) {
    let complete = view.remaining == 0;
    let running = matches!(view.status.as_str(), "analyzing" | "cancelling");
    let semantic_detail = view.warnings.iter().any(|warning|
        warning.starts_with("语义候选当前批：") || warning.starts_with("语义候选历史："));
    if semantic_detail {
        view.warnings.retain(|warning| !is_legacy_semantic_notice(warning));
    } else {
        for warning in &mut view.warnings {
            if is_legacy_semantic_notice(warning) {
                *warning = format!("语义候选历史（旧版记录）：{}；该记录未保存最近一批状态，不代表当前仍在降级", warning);
            }
        }
    }
    view.warnings.retain(|warning| {
        warning != "模型达到输出长度上限，已改用双条/单条与较短来源片段有限重试；成功小组会单独保存"
            && !((complete || running) && (warning == "精炼模型本批超时；已生成的建议保留，可继续分析"
                || warning == "精炼模型没有返回有效的 JSON 建议" || review::is_connection_error(warning)))
            && !(complete && (warning.starts_with("部分小组格式修复后仍未通过校验")
                || warning == "部分小组在缩批后仍超出长度或时间预算，已保留重试并继续后续批次；完成后可继续分析剩余记忆"))
    });
    if !running { view.activity = None; }
}

fn is_legacy_semantic_notice(warning: &str) -> bool {
    warning.starts_with("语义候选降级：")
        || warning == "语义候选检索未就绪或暂不可用，本批使用关键词候选；不会记为完整检查，下次增量分析会重试"
}

pub(crate) fn is_semantic_review_notice(warning: &str) -> bool {
    warning.starts_with("语义候选")
}

fn semantic_review_notice(mode: &str) -> Option<String> {
    let reason = match mode {
        "semantic" => return None,
        "semantic_partial_index" => "当前标签部分向量缺失，已使用已有有效向量和关键词",
        "local_fallback_cold_index" => "当前标签尚无可用向量，已使用关键词",
        "local_fallback_runtime" => "本地向量服务未运行，已使用关键词",
        "local_fallback_busy" => "向量缓存或服务正忙，已使用关键词",
        "local_fallback_budget" => "语义候选查询总预算 30 秒已耗尽，已使用关键词",
        "local_fallback_lease" => "等待向量服务租约达到本批截止时间，已使用关键词",
        "local_fallback_connect_timeout" => "远端向量连接阶段超时（连接上限 10 秒，受总预算约束），已使用关键词",
        "local_fallback_request_timeout" => "远端向量请求在收到响应头前超时，已使用关键词",
        "local_fallback_response_timeout" => "远端向量响应正文读取超时，已使用关键词",
        "local_fallback_source_changed" => "查询记忆正文或标签已变化，已使用当前批关键词候选",
        _ => "语义配置或查询暂不可用，已使用关键词",
    };
    Some(format!("语义候选当前批：{reason}；本批不记为完整关系检查，后续增量分析会重试"))
}

fn record_semantic_result(run: &mut StoredRun, mode: &str) {
    let legacy = run.semantic_progress.is_none() && run.view.warnings.iter().any(|warning| is_semantic_review_notice(warning));
    let notice = semantic_review_notice(mode);
    let progress = run.semantic_progress.get_or_insert_with(|| SemanticProgress { legacy_history: legacy, ..SemanticProgress::default() });
    progress.checks += 1;
    progress.degraded += usize::from(notice.is_some());
    let (checks, degraded, legacy) = (progress.checks, progress.degraded, progress.legacy_history);
    // The worker owns semantic status. Do not keep the previous batch's fault
    // as a current error after a later successful local/vector query.
    run.view.warnings.retain(|warning| !is_semantic_review_notice(warning));
    if let Some(notice) = notice { warn_once(run, notice); }
    if degraded > 0 || legacy {
        let state = if mode == "semantic" { "最近一批已恢复；" } else { "" };
        let old = if legacy { "另有旧版降级记录（次数未记录）；" } else { "" };
        warn_once(run, format!("语义候选历史：{state}本轮已记录 {checks} 次查询、{degraded} 次降级；{old}此前覆盖不足的批次仍待增量补查"));
    }
}

fn semantic_error_mode(error: &str) -> &str {
    match error {
        "recall_deadline" | "embedding total timed out" => "local_fallback_budget",
        "refinement lease timed out" => "local_fallback_lease",
        "embedding connection timed out" => "local_fallback_connect_timeout",
        "embedding request timed out" => "local_fallback_request_timeout",
        "embedding response timed out" => "local_fallback_response_timeout",
        "embedding cache unavailable" | "embedding service busy" | "memory snapshot busy" | "memory snapshot unavailable" | "unavailable" => "local_fallback_busy",
        "refinement source changed" => "local_fallback_source_changed",
        _ => "semantic_error",
    }
}

fn warn_once(run: &mut StoredRun, message: String) {
    if !run.view.warnings.contains(&message) && run.view.warnings.len() < 30 { run.view.warnings.push(message); }
}

#[derive(Clone, Debug, Hash, PartialEq, Eq)]
enum ReviewUnit {
    Pair(String, String),
    Single(String),
}

fn next_retry_unit(
    pairs: &HashSet<(String, String)>, quality_ids: &HashSet<String>, attempted: &HashSet<ReviewUnit>,
) -> Option<ReviewUnit> {
    let mut pending: Vec<_> = pairs.iter().filter(|(left, right)|
        !attempted.contains(&ReviewUnit::Pair(left.clone(), right.clone()))).collect();
    pending.sort_by(|a, b| {
        let quality = |pair: &(String, String)| usize::from(quality_ids.contains(&pair.0)) + usize::from(quality_ids.contains(&pair.1));
        quality(b).cmp(&quality(a)).then(a.cmp(b))
    });
    if let Some((left, right)) = pending.first() {
        return Some(ReviewUnit::Pair(left.clone(), right.clone()));
    }
    quality_ids.iter().filter(|id| !attempted.contains(&ReviewUnit::Single((*id).clone())))
        .min().map(|id| ReviewUnit::Single(id.clone()))
}

fn pending_quality(run: &StoredRun, indices: &[usize]) -> HashSet<String> {
    let occupied: HashSet<_> = run.view.actions.iter().filter(|action| action.kind != "conflict")
        .flat_map(|action| action.originals.iter().map(|memory| memory.id.as_str())).collect();
    indices.iter().filter(|&&index| !run.quality_reviewed.contains(&index)
        && !run.inventory[index].protected && !occupied.contains(run.inventory[index].view.id.as_str()))
        .map(|&index| run.inventory[index].view.id.clone()).collect()
}

fn pending_quality_in(run: &StoredRun, indices: &[usize], candidates: &[MemoryState]) -> HashSet<String> {
    let ids: HashSet<_> = candidates.iter().map(|memory| memory.view.id.as_str()).collect();
    let mut quality = pending_quality(run, indices);
    quality.retain(|id| ids.contains(id.as_str()));
    quality
}

fn publish_review(
    store: &TelemetryStore, run: &mut StoredRun, candidates: &[MemoryState], proposals: Vec<Proposal>, domain: &str,
    requested_pairs: &HashSet<(String, String)>, requested_quality: &HashSet<String>, quality_indices: &[usize],
) -> Result<(), String> {
    store.refinement_sync_reviews(run)?;
    let current = store.refinement_current_candidates(candidates)?;
    let allowed = allowed_pairs(run, &current, domain);
    let pairs = requested_pairs.intersection(&allowed).cloned().collect();
    let pending = pending_quality_in(run, quality_indices, &current);
    let quality = requested_quality.intersection(&pending).cloned().collect();
    accept_review(run, &current, proposals, domain, &pairs, &quality, quality_indices);
    store.refinement_publish(run, &[])
}

fn accept_review(
    run: &mut StoredRun, candidates: &[MemoryState], proposals: Vec<Proposal>, domain: &str,
    pairs: &HashSet<(String, String)>, quality_ids: &HashSet<String>, quality_indices: &[usize],
) {
    accept_proposals(run, candidates, proposals, domain, pairs, quality_ids);
    run.seen_pairs.extend(pairs.iter().cloned());
    let reviewed: Vec<_> = quality_indices.iter().copied()
        .filter(|&index| quality_ids.contains(&run.inventory[index].view.id)).collect();
    run.quality_reviewed.extend(reviewed);
}

pub(crate) fn record_task_result(run: &mut StoredRun, complete: bool) {
    if complete {
        run.retry_tasks.remove(&run.cursor);
        run.completed_tasks.insert(run.cursor);
    } else {
        run.completed_tasks.remove(&run.cursor);
        run.retry_tasks.insert(run.cursor);
    }
}

// None is cancellation; false keeps this batch retryable while later batches
// continue. Length and timeout failures share bounded smaller-group retries.
async fn review_batch(
    store: &TelemetryStore, run: &mut StoredRun, provider: &llm::LlmProvider,
    candidates: &[MemoryState], domain: &str, quality_indices: &[usize], cancel: &AtomicBool,
) -> Result<Option<bool>, String> {
    store.refinement_sync_reviews(run)?;
    let live_candidates = store.refinement_current_candidates(candidates)?;
    let pairs = allowed_pairs(run, &live_candidates, domain);
    let quality_ids = pending_quality_in(run, quality_indices, &live_candidates);
    if pairs.is_empty() && quality_ids.is_empty() { run.view.activity = None; return Ok(Some(true)); }
    let mut budget = review::BatchBudget::new();
    let mut format_error = None;
    let concise = review::prefers_compact_requests(provider);
    match review::request(provider, &live_candidates, domain, &pairs, &quality_ids, concise,
        review::timeout(provider, false), cancel, &mut budget).await {
        Ok(proposals) => {
            run.view.activity = None;
            publish_review(store, run, &live_candidates, proposals, domain, &pairs, &quality_ids, quality_indices)?;
            return Ok(Some(true));
        }
        Err(review::ReviewFailure::Cancelled) => return Ok(None),
        Err(review::ReviewFailure::Network(message) | review::ReviewFailure::Other(message)) => return Err(message),
        Err(review::ReviewFailure::Format(message)) => { format_error = Some(message); }
        Err(review::ReviewFailure::Length | review::ReviewFailure::Timeout | review::ReviewFailure::Budget) => {},
    }
    run.view.activity = Some("正在缩小检查范围并重试，已有建议可继续审阅和应用".into());
    store.refinement_publish(run, &[])?;
    let mut attempted = HashSet::new();
    loop {
        if cancel.load(Ordering::Acquire) { return Ok(None); }
        store.refinement_sync_reviews(run)?;
        let live_candidates = store.refinement_current_candidates(candidates)?;
        let pairs = allowed_pairs(run, &live_candidates, domain);
        let quality_ids = pending_quality_in(run, quality_indices, &live_candidates);
        if pairs.is_empty() && quality_ids.is_empty() { run.view.activity = None; return Ok(Some(true)); }
        if budget.exhausted() { break; }
        let Some(unit) = next_retry_unit(&pairs, &quality_ids, &attempted) else { break };
        attempted.insert(unit.clone());
        let (unit_pairs, ids) = match unit {
            ReviewUnit::Pair(left, right) => (HashSet::from([(left.clone(), right.clone())]), HashSet::from([left, right])),
            ReviewUnit::Single(id) => (HashSet::new(), HashSet::from([id])),
        };
        let unit_quality: HashSet<_> = quality_ids.intersection(&ids).cloned().collect();
        let unit_candidates: Vec<_> = live_candidates.iter().filter(|memory| ids.contains(&memory.view.id)).cloned().collect();
        run.view.activity = Some(format!("正在分析小组（本批第 {} 次请求），已生成建议可立即处理", budget.calls + 1));
        store.refinement_publish(run, &[])?;
        match review::request(provider, &unit_candidates, domain, &unit_pairs, &unit_quality, true,
            review::timeout(provider, true), cancel, &mut budget).await {
            Ok(proposals) => {
                run.view.activity = None;
                publish_review(store, run, &unit_candidates, proposals, domain, &unit_pairs, &unit_quality, quality_indices)?;
            }
            Err(review::ReviewFailure::Cancelled) => return Ok(None),
            Err(review::ReviewFailure::Network(message) | review::ReviewFailure::Other(message)) => return Err(message),
            Err(review::ReviewFailure::Budget) => break,
            Err(review::ReviewFailure::Format(message)) => { format_error = Some(message); }
            Err(review::ReviewFailure::Length | review::ReviewFailure::Timeout) => {
                // No seen-pair/quality checkpoint is written for a failed unit.
                // Try independent groups within the remaining request budget.
            }
        }
        run.view.updated_at = chrono::Utc::now().to_rfc3339();
        store.refinement_publish(run, &[])?;
    }
    run.view.activity = None;
    if let Some(message) = format_error {
        warn_once(run, format!("部分小组格式修复后仍未通过校验，已保留重试；{message}"));
    } else {
        warn_once(run, "部分小组在缩批后仍超出长度或时间预算，已保留重试并继续后续批次；完成后可继续分析剩余记忆".into());
    }
    Ok(Some(false))
}

async fn analyze(store: &TelemetryStore, run: &mut StoredRun, cancel: &AtomicBool) -> Result<(), String> {
    let provider = llm::memory_extraction_provider()?;
    let semantic_enabled = store.app_setting_get::<serde_json::Value>("memory_semantic_embedding")
        .is_some_and(|value| !value.is_null());
    if run.task_domains.len() != run.tasks.len() || run.task_targets.len() != run.tasks.len() {
        return Err("此记录使用旧版分析策略，请新建按标签分析；已有建议仍可审阅或撤销".into());
    }
    let last_tasks = last_target_tasks(run);
    while run.cursor < run.tasks.len() {
        if cancel.load(Ordering::Acquire) { break; }
        if run.completed_tasks.contains(&run.cursor) {
            // Revisit the cursor cheaply after an earlier failed batch retries;
            // successful model work must never be billed again on resume.
            let checked = finish_batch(run, &last_tasks, true);
            if !checked.is_empty() { store.refinement_publish(run, &checked)?; }
            continue;
        }
        let indices = run.tasks[run.cursor].clone();
        let domain = run.task_domains[run.cursor].clone();
        let mut candidates: Vec<_> = indices.iter().map(|&index| run.inventory[index].clone())
            .filter(|memory| in_domain(&memory.view, &domain)).collect();
        if candidates.is_empty() { return Err("标签分组与记忆快照不一致，请新建分析".into()); }
        let mut semantic_covered = true;
        if semantic_enabled && domain != "unclassified" {
            let query_id = candidates[0].view.id.clone();
            let query = candidates[0].view.memory.clone();
            let domains = Some(vec![domain.clone()]);
            let search_store = store.clone();
            let semantic_budget = crate::memory_backend::REFINEMENT_SEMANTIC_BUDGET;
            let search_budget = crate::memory_retrieval::RecallBudget::new(semantic_budget);
            let worker_budget = search_budget.clone();
            let search = tauri::async_runtime::spawn_blocking(move || {
                crate::memory_backend::refinement_semantic_neighbours(&search_store, &query_id, &query, domains.as_deref(), &worker_budget)
            });
            let neighbours = tokio::select! {
                result = tokio::time::timeout(semantic_budget, search) => result
                    .map_err(|_| "recall_deadline".to_string())
                    .and_then(|joined| joined.map_err(|_| "semantic_worker_failed".to_string()))
                    .and_then(|value| value),
                _ = async { while !cancel.load(Ordering::Acquire) { tokio::time::sleep(Duration::from_millis(200)).await; } } => {
                    search_budget.cancel();
                    break;
                },
            };
            search_budget.cancel();
            match neighbours {
                Ok(neighbours) => {
                    semantic_covered = neighbours.mode == "semantic";
                    record_semantic_result(run, neighbours.mode);
                    for neighbour in neighbours.memories {
                        if candidates.len() >= 6 { break; }
                        if candidates.iter().any(|memory| memory.view.id == neighbour.id) { continue; }
                        if let Some(memory) = run.inventory.iter().find(|memory| memory.view.id == neighbour.id
                            && in_domain(&memory.view, &domain)) {
                            candidates.push(memory.clone());
                        }
                    }
                }
                Err(error) => {
                    semantic_covered = false;
                    record_semantic_result(run, semantic_error_mode(&error));
                }
            }
        }
        if cancel.load(Ordering::Acquire) { break; }
        let quality_indices = run.task_targets[run.cursor].clone();
        // Source excerpts are bounded and included only for this review group.
        for state in &mut candidates {
            state.view.source_excerpt = store.refinement_source_excerpt(state)?;
        }
        let Some(complete) = review_batch(store, run, &provider, &candidates, &domain, &quality_indices, cancel).await? else { break };
        record_task_result(run, complete);
        let checked = finish_batch(run, &last_tasks, semantic_covered);
        store.refinement_publish(run, &checked)?;
    }
    Ok(())
}

fn activate(store: TelemetryStore, mut run: StoredRun, create: bool) -> Result<RefinementRun, String> {
    let mut active = ACTIVE.lock().map_err(|_| "精炼任务锁不可用".to_string())?;
    if active.is_some() { return Err("已有记忆精炼任务运行中，请等待完成或取消".into()); }
    let cancel = Arc::new(AtomicBool::new(false));
    run.view.status = "analyzing".into();
    run.view.updated_at = chrono::Utc::now().to_rfc3339();
    if create { store.refinement_create(&run)?; }
    else { store.refinement_publish(&mut run, &[])?; }
    let view = run.view.clone();
    *active = Some((run.view.id.clone(), cancel.clone()));
    tauri::async_runtime::spawn(async move {
        let _active_guard = ActiveGuard(run.view.id.clone());
        let result = analyze(&store, &mut run, &cancel).await;
        run.view.status = if cancel.load(Ordering::Acquire) { "cancelled" }
            else if result.is_err() || !run.retry_tasks.is_empty() { "failed" } else { "ready" }.into();
        if let Err(error) = result { warn_once(&mut run, error); }
        run.view.updated_at = chrono::Utc::now().to_rfc3339();
        if let Err(error) = store.refinement_publish(&mut run, &[]) { eprintln!("[memory-refinement] save failed: {error}"); }
    });
    Ok(view)
}

fn ensure_idle(run_id: &str) -> Result<(), String> {
    if ACTIVE.lock().map_err(|_| "精炼任务锁不可用".to_string())?.as_ref()
        .is_some_and(|(id, _)| id == run_id) { return Err("分析仍在进行；撤销或继续分析前，请先结束或取消当前分析".into()); }
    Ok(())
}

async fn refinement_io<T, F>(work: F) -> Result<T, String>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T, String> + Send + 'static,
{
    tauri::async_runtime::spawn_blocking(work).await
        .map_err(|error| format!("精炼后台任务失败：{error}"))?
}

#[tauri::command]
pub async fn memory_refinement_start(
    telemetry: tauri::State<'_, TelemetryStore>, domain_id: Option<String>, incremental: bool,
) -> Result<RefinementRun, String> {
    let store = telemetry.inner().clone();
    refinement_io(move || {
        llm::memory_extraction_provider()?;
        let run = store.refinement_prepare(domain_id, incremental)?;
        activate(store, run, true)
    }).await
}

#[tauri::command]
pub async fn memory_refinement_list(telemetry: tauri::State<'_, TelemetryStore>) -> Result<Vec<RefinementRun>, String> {
    let store = telemetry.inner().clone();
    refinement_io(move || {
        let active_id = ACTIVE.lock().map_err(|_| "精炼任务锁不可用".to_string())?.as_ref().map(|(id, _)| id.clone());
        store.refinement_history(active_id.as_deref())
    }).await
}

#[tauri::command]
pub async fn memory_refinement_history_snapshot(
    telemetry: tauri::State<'_, TelemetryStore>,
) -> Result<RefinementHistorySnapshot, String> {
    let store = telemetry.inner().clone();
    refinement_io(move || {
        let active = ACTIVE.lock().map_err(|_| "精炼任务锁不可用".to_string())?;
        if active.is_some() { return Err("仍有记忆分析运行中，请先停止后再清空历史".into()); }
        let suggesting = SUGGESTING.lock().map_err(|_| "建议生成锁不可用".to_string())?;
        if !suggesting.is_empty() { return Err("仍有处理建议生成中，请等待完成后再清空历史".into()); }
        store.refinement_history_snapshot()
    }).await
}

#[tauri::command]
pub async fn memory_refinement_clear(
    telemetry: tauri::State<'_, TelemetryStore>, expected_revision: String,
) -> Result<usize, String> {
    let store = telemetry.inner().clone();
    refinement_io(move || {
        let active = ACTIVE.lock().map_err(|_| "精炼任务锁不可用".to_string())?;
        if active.is_some() { return Err("仍有记忆分析运行中，请先停止后再清空历史".into()); }
        let suggesting = SUGGESTING.lock().map_err(|_| "建议生成锁不可用".to_string())?;
        if !suggesting.is_empty() { return Err("仍有处理建议生成中，请等待完成后再清空历史".into()); }
        store.refinement_clear(&expected_revision)
    }).await
}

#[tauri::command]
pub async fn memory_refinement_forget(
    telemetry: tauri::State<'_, TelemetryStore>, run_id: String, expected_updated_at: String,
) -> Result<(), String> {
    let store = telemetry.inner().clone();
    refinement_io(move || {
        // Use the same lock order as activation: runtime admission, then DB.
        // Holding the suggestion lock also prevents a generation starting in
        // the gap between the busy check and deletion.
        let active = ACTIVE.lock().map_err(|_| "精炼任务锁不可用".to_string())?;
        if active.as_ref().is_some_and(|(id, _)| id == &run_id) {
            return Err("该记录仍在分析中，请先取消分析并等待停止后再删除记录".into());
        }
        let suggesting = SUGGESTING.lock().map_err(|_| "建议生成锁不可用".to_string())?;
        if suggesting.iter().any(|(id, _)| id == &run_id) {
            return Err("该记录仍在生成建议，请等待完成后再删除记录".into());
        }
        store.refinement_forget(&run_id, &expected_updated_at)?;
        Ok(())
    }).await
}

#[tauri::command]
pub async fn memory_refinement_get(telemetry: tauri::State<'_, TelemetryStore>, run_id: String) -> Result<RefinementRun, String> {
    let store = telemetry.inner().clone();
    refinement_io(move || {
        let mut view = store.refinement_view(&run_id)?;
        if matches!(view.status.as_str(), "analyzing" | "cancelling") {
            let active = ACTIVE.lock().map_err(|_| "精炼任务锁不可用".to_string())?;
            if active.as_ref().is_none_or(|(id, _)| id != &run_id) { view.status = "cancelled".into(); }
        }
        normalize_notices(&mut view);
        Ok(view)
    }).await
}

#[tauri::command]
pub async fn memory_refinement_resume(telemetry: tauri::State<'_, TelemetryStore>, run_id: String) -> Result<RefinementRun, String> {
    let store = telemetry.inner().clone();
    refinement_io(move || {
        ensure_idle(&run_id)?;
        let mut run = store.refinement_load(&run_id)?;
        if let Some(&retry) = run.retry_tasks.iter().min() { run.cursor = run.cursor.min(retry); }
        if run.cursor >= run.tasks.len() { return Err("本轮全部条目已完成分析".into()); }
        if run.task_domains.len() != run.tasks.len() || run.task_targets.len() != run.tasks.len() {
            return Err("此记录使用旧版分析策略，请新建按标签分析；已有建议仍可审阅或撤销".into());
        }
        activate(store, run, false)
    }).await
}

#[tauri::command]
pub async fn memory_refinement_cancel(telemetry: tauri::State<'_, TelemetryStore>, run_id: String) -> Result<RefinementRun, String> {
    let store = telemetry.inner().clone();
    refinement_io(move || {
        let requested = {
            let active = ACTIVE.lock().map_err(|_| "精炼任务锁不可用".to_string())?;
            if let Some((_, flag)) = active.as_ref().filter(|(id, _)| id == &run_id) {
                flag.store(true, Ordering::Release);
                true
            } else { false }
        };
        let mut view = store.refinement_view(&run_id)?;
        if requested && matches!(view.status.as_str(), "analyzing" | "cancelling") { view.status = "cancelling".into(); }
        normalize_notices(&mut view);
        Ok(view)
    }).await
}

fn refresh_indexes(store: &TelemetryStore, run: StoredRun) -> RefinementRun {
    // Append notices to the latest progress instead of saving the mutation's
    // stale snapshot over a worker that has already published another batch.
    let mut warnings = Vec::new();
    if let Err(error) = crate::memory_backend::queue_refined_l1_index(store.clone()) {
        warnings.push(format!("记忆已更新，语义索引排队失败：{error}"));
    }
    warnings.push("关键词检索已更新；语义向量在后台按新内容增量更新，索引未就绪时检索会使用关键词及已有有效向量。相关工作记忆可重新生成，长期画像仍需人工发布。".into());
    match store.refinement_append_warnings(&run.view.id, warnings) {
        Ok(view) => view,
        Err(error) => { eprintln!("[memory-refinement] index notice: {error}"); run.view }
    }
}

#[tauri::command]
pub async fn memory_refinement_apply(
    telemetry: tauri::State<'_, TelemetryStore>, run_id: String, selections: Vec<RefinementSelection>,
) -> Result<RefinementRun, String> {
    let store = telemetry.inner().clone();
    refinement_io(move || {
        let run = store.refinement_apply(&run_id, selections)?;
        Ok(refresh_indexes(&store, run))
    }).await
}

#[tauri::command]
pub async fn memory_refinement_suggest(
    telemetry: tauri::State<'_, TelemetryStore>, run_id: String, action_id: String,
) -> Result<RefinementRun, String> {
    let key = (run_id.clone(), action_id.clone());
    {
        let mut active = SUGGESTING.lock().map_err(|_| "建议生成锁不可用".to_string())?;
        if active.contains(&key) { return Err("本项建议正在生成，请等待完成".into()); }
        active.push(key.clone());
    }
    let _guard = SuggestionGuard(key);
    let store = telemetry.inner().clone();
    let input_store = store.clone();
    let input_id = run_id.clone();
    let input_action = action_id.clone();
    let (mut run, candidates, domain, provider) = refinement_io(move || {
        let run = input_store.refinement_load(&input_id)?;
        let action = run.view.actions.iter().find(|action| action.id == input_action).ok_or("未找到冲突建议")?;
        if action.kind != "conflict" || action.status != "pending" { return Err("请选择待审阅的来源冲突项".into()); }
        if action.recommended_source_id.is_some() { return Ok((run, Vec::new(), String::new(), None)); }
        let mut candidates: Vec<_> = action.originals.iter().filter_map(|source|
            run.inventory.iter().find(|memory| memory.view.id == source.id).cloned()).collect();
        if candidates.len() != action.originals.len() || candidates.len() < 2
            || input_store.refinement_current_candidates(&candidates)?.len() != candidates.len()
            || candidates.iter().any(|memory| memory.protected) {
            return Err("原始记忆已变化或受到保护，请重新分析；已有内容未修改".into());
        }
        let mut domains = candidates[0].view.domains.clone();
        domains.retain(|id| id != "unclassified" && candidates.iter().all(|memory| in_domain(&memory.view, id)));
        domains.sort();
        domains.dedup();
        let domain = run.view.domain_id.as_ref().filter(|id| domains.contains(id)).cloned()
            .or_else(|| domains.into_iter().next()).ok_or("本组已没有共同标签，请重新分析")?;
        for memory in &mut candidates { memory.view.source_excerpt = input_store.refinement_source_excerpt(memory)?; }
        let provider = llm::memory_extraction_provider()?;
        Ok((run, candidates, domain, Some(provider)))
    }).await?;
    let Some(provider) = provider else { return Ok(run.view); };
    let pairs = candidates.iter().enumerate().flat_map(|(index, left)| candidates[index + 1..].iter()
        .map(move |right| pair_key(&left.view.id, &right.view.id))).collect();
    let mut budget = review::BatchBudget::new();
    let proposals = review::suggest_conflict(&provider, &candidates, &domain, &AtomicBool::new(false), &mut budget).await
        .map_err(|error| match error {
            review::ReviewFailure::Length => "处理建议达到输出上限，原记忆未修改，可重试生成".into(),
            review::ReviewFailure::Timeout | review::ReviewFailure::Budget => "处理建议生成超时，原记忆未修改，可重试生成".into(),
            review::ReviewFailure::Cancelled => "处理建议生成已取消".into(),
            review::ReviewFailure::Format(message) | review::ReviewFailure::Network(message) | review::ReviewFailure::Other(message) => message,
        })?;
    run.view.actions.clear();
    accept_proposals(&mut run, &candidates, proposals, &domain, &pairs, &HashSet::new());
    let ids: HashSet<_> = candidates.iter().map(|memory| memory.view.id.as_str()).collect();
    let recommendation = run.view.actions.into_iter().find(|action| action.kind == "conflict"
        && action.recommended_source_id.is_some() && action.originals.len() == ids.len()
        && action.originals.iter().all(|memory| ids.contains(memory.id.as_str())))
        .ok_or("模型未生成覆盖本组完整来源的处理建议，原记忆未修改，请重试")?;
    refinement_io(move || store.refinement_save_conflict_recommendation(&run_id, &action_id, &candidates, recommendation)).await
}

#[tauri::command]
pub async fn memory_refinement_skip(
    telemetry: tauri::State<'_, TelemetryStore>, run_id: String, action_ids: Vec<String>,
) -> Result<RefinementRun, String> {
    let store = telemetry.inner().clone();
    refinement_io(move || store.refinement_skip(&run_id, action_ids).map(|run| run.view)).await
}

#[tauri::command]
pub async fn memory_refinement_delete(
    telemetry: tauri::State<'_, TelemetryStore>, run_id: String, selections: Vec<RefinementDeleteSelection>,
) -> Result<RefinementRun, String> {
    let store = telemetry.inner().clone();
    // Queries validate against current L1; deleting records needs no new
    // embeddings or full-library importance calculation.
    refinement_io(move || store.refinement_delete(&run_id, selections).map(|run| run.view)).await
}

#[tauri::command]
pub async fn memory_refinement_undo(telemetry: tauri::State<'_, TelemetryStore>, run_id: String) -> Result<RefinementRun, String> {
    let store = telemetry.inner().clone();
    refinement_io(move || {
        let active = ACTIVE.lock().map_err(|_| "精炼任务锁不可用".to_string())?;
        if active.as_ref().is_some_and(|(id, _)| id == &run_id) {
            return Err("分析仍在进行；撤销前请先结束或取消当前分析".into());
        }
        let run = store.refinement_undo(&run_id)?;
        drop(active);
        Ok(refresh_indexes(&store, run))
    }).await
}
