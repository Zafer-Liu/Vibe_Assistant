//! Shared ranking semantics and macro averages over executed benchmark cases.
use super::types::{CaseResult, EvaluationCase, EvaluationReport};
use crate::memory_evidence::EvidenceResponse;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::fmt::Write;

#[derive(Clone, Serialize, Deserialize, Debug)]
pub(crate) struct RankingMetrics {
    pub recall_at_k: Option<f64>,
    pub mrr: Option<f64>,
    pub ndcg_at_k: Option<f64>,
    pub duplicate_rate: f64,
    pub forbidden_rate: f64,
    pub false_positive_on_empty_query: bool,
}

pub(crate) fn evaluate(expected: &[String], returned: &[String], forbidden: &[String], k: usize) -> RankingMetrics {
    let expected: HashSet<_> = expected.iter().collect();
    let forbidden: HashSet<_> = forbidden.iter().collect();
    let rows: Vec<_> = returned.iter().take(k).collect();
    let mut seen = HashSet::new();
    let mut relevant = 0;
    let mut dcg = 0.0;
    let mut reciprocal = 0.0;
    let mut duplicates = 0;
    let mut wrong = 0;
    for (i, id) in rows.iter().enumerate() {
        let unique = seen.insert(*id);
        if !unique { duplicates += 1; }
        if forbidden.contains(*id) { wrong += 1; }
        if unique && expected.contains(*id) {
            relevant += 1;
            dcg += 1.0 / ((i + 2) as f64).log2();
            if reciprocal == 0.0 { reciprocal = 1.0 / (i + 1) as f64; }
        }
    }
    let ideal: f64 = (0..expected.len().min(k)).map(|i| 1.0 / ((i + 2) as f64).log2()).sum();
    let count = rows.len().max(1) as f64;
    RankingMetrics {
        recall_at_k: (!expected.is_empty()).then(|| relevant as f64 / expected.len() as f64),
        mrr: (!expected.is_empty()).then_some(reciprocal),
        ndcg_at_k: (ideal > 0.0).then(|| dcg / ideal),
        duplicate_rate: duplicates as f64 / count,
        forbidden_rate: wrong as f64 / count,
        false_positive_on_empty_query: expected.is_empty() && !rows.is_empty(),
    }
}

/// An annotation is covered only by an actual excerpt from its named source.
/// Status labels may target the response status or the uniform per-memory status;
/// every expected memory must have been returned and read for a status hit.
pub(crate) fn evidence_scores(case: &EvaluationCase, requested: &[String], response: &EvidenceResponse) -> (Option<f64>, Option<bool>) {
    let items: Vec<_> = response.items.iter().filter(|item| requested.contains(&item.memory_id)).collect();
    let coverage = (!case.expected_evidence.is_empty()).then(|| {
        if case.expected_ids.iter().any(|id| !requested.contains(id)) { return 0.0; }
        let hits = case.expected_evidence.iter().filter(|expected| items.iter().any(|item| {
            item.sources.iter().any(|source| source.source.source_event_key == expected.source_event_key
                && source.excerpt.as_ref().is_some_and(|excerpt| excerpt.text.contains(&expected.contains)))
        })).count();
        hits as f64 / case.expected_evidence.len() as f64
    });
    let status = case.expected_evidence_status.as_ref().map(|expected| {
        !case.expected_ids.is_empty() && case.expected_ids.iter().all(|id| {
            requested.contains(id) && items.iter().any(|item| &item.memory_id == id)
        }) && (response.status == *expected || items.iter().all(|item| item.status == *expected))
    });
    (coverage, status)
}

#[derive(Clone, Serialize, Deserialize, Default)]
pub(crate) struct AggregateMetrics {
    pub planned_count: usize,
    pub executed_count: usize,
    pub successful_count: usize,
    pub failed_count: usize,
    pub degraded_count: usize,
    pub partial_index_count: usize,
    pub not_run_count: usize,
    pub answerable_count: usize,
    pub unanswerable_count: usize,
    pub recall_at_k: Option<f64>,
    pub mrr: Option<f64>,
    pub ndcg_at_k: Option<f64>,
    pub duplicate_rate: Option<f64>,
    pub forbidden_rate: Option<f64>,
    pub false_positive_rate: Option<f64>,
    pub domain_accuracy: Option<f64>,
    pub p50_ms: Option<f64>,
    pub p95_ms: Option<f64>,
    pub mean_returned_chars: Option<f64>,
    #[serde(default)]
    pub evidence_coverage: Option<f64>,
    #[serde(default)]
    pub evidence_status_accuracy: Option<f64>,
}

fn ratio(sum: f64, count: usize) -> Option<f64> {
    (count > 0).then(|| sum / count as f64)
}

// Nearest-rank percentiles; preparation time is excluded.
fn percentile(sorted: &[u64], percent: usize) -> Option<f64> {
    if sorted.is_empty() { return None; }
    let rank = (sorted.len() * percent + 99) / 100;
    Some(sorted[rank.saturating_sub(1)] as f64)
}

/// Status contract: success / failed / not_run. Unknown executed statuses fail
/// closed. Explicit not_run rows and omitted planned rows are never successes.
/// The runner supplies evaluate(expected, [], forbidden, k) on failed cases.
pub(crate) fn aggregate(cases: &[CaseResult], planned: usize) -> AggregateMetrics {
    let executed: Vec<_> = cases.iter().filter(|c| c.status != "not_run").collect();
    let answerable: Vec<_> = executed.iter().copied().filter(|c| !c.expected_ids.is_empty()).collect();
    let unanswerable: Vec<_> = executed.iter().copied().filter(|c| c.expected_ids.is_empty()).collect();
    let domains: Vec<_> = executed.iter().filter_map(|c| c.domain_hit).collect();
    let evidence: Vec<_> = executed.iter().filter_map(|c| c.evidence_coverage).collect();
    let evidence_statuses: Vec<_> = executed.iter().filter_map(|c| c.evidence_status_hit).collect();
    let mut times: Vec<_> = executed.iter().map(|c| c.elapsed_ms).collect();
    times.sort_unstable();
    let successful = executed.iter().filter(|c| c.status == "success").count();
    // None from an answerable executed case contributes zero, never shrinks its denominator.
    AggregateMetrics {
        planned_count: planned,
        executed_count: executed.len(),
        successful_count: successful,
        failed_count: executed.len() - successful,
        degraded_count: executed.iter().filter(|c| c.degraded).count(),
        partial_index_count: executed.iter().filter(|c| !c.index_complete).count(),
        not_run_count: planned.saturating_sub(executed.len())
            .max(cases.iter().filter(|c| c.status == "not_run").count()),
        answerable_count: answerable.len(),
        unanswerable_count: unanswerable.len(),
        recall_at_k: ratio(answerable.iter().map(|c| c.metrics.recall_at_k.unwrap_or(0.0)).sum(), answerable.len()),
        mrr: ratio(answerable.iter().map(|c| c.metrics.mrr.unwrap_or(0.0)).sum(), answerable.len()),
        ndcg_at_k: ratio(answerable.iter().map(|c| c.metrics.ndcg_at_k.unwrap_or(0.0)).sum(), answerable.len()),
        duplicate_rate: ratio(executed.iter().map(|c| c.metrics.duplicate_rate).sum(), executed.len()),
        forbidden_rate: ratio(executed.iter().map(|c| c.metrics.forbidden_rate).sum(), executed.len()),
        false_positive_rate: ratio(unanswerable.iter().filter(|c| c.metrics.false_positive_on_empty_query).count() as f64, unanswerable.len()),
        domain_accuracy: ratio(domains.iter().filter(|hit| **hit).count() as f64, domains.len()),
        p50_ms: percentile(&times, 50),
        p95_ms: percentile(&times, 95),
        mean_returned_chars: ratio(executed.iter().map(|c| c.returned_chars as f64).sum(), executed.len()),
        evidence_coverage: ratio(evidence.iter().sum(), evidence.len()),
        evidence_status_accuracy: ratio(evidence_statuses.iter().filter(|hit| **hit).count() as f64, evidence_statuses.len()),
    }
}

/// Escape imported text as one Markdown table cell. Keep the actual question,
/// including technical URLs; model credentials are absent from the report DTO.
fn cell(text: &str) -> String {
    let mut result = String::new();
    for (i, word) in text.split_whitespace().enumerate() {
        if i > 0 { result.push(' '); }
        for c in word.chars() {
            match c {
                '&' => result.push_str("&amp;"),
                '<' => result.push_str("&lt;"),
                '>' => result.push_str("&gt;"),
                '|' => result.push_str("&#124;"),
                '\\' | '`' | '*' | '_' | '[' | ']' | '(' | ')' | '#' | '!' | '~' => {
                    let _ = write!(result, "&#{};", c as u32);
                }
                c if c.is_control() => result.push(' '),
                c => result.push(c),
            }
        }
    }
    result
}

fn number(value: Option<f64>) -> String {
    value.filter(|n| n.is_finite()).map(|n| format!("{n:.4}")).unwrap_or_else(|| "—".into())
}

fn ids(values: &[String]) -> String { cell(&values.join(", ")) }

pub(crate) fn markdown(report: &EvaluationReport) -> String {
    // Recompute to keep exported totals tied to the actual case rows.
    let s = aggregate(&report.cases, report.planned_count);
    let mut out = String::from("# Memory retrieval evaluation\n\n");
    let _ = writeln!(out, "Dataset: {} ({}) · synthetic={}\n", cell(&report.dataset_name), cell(&report.dataset_id), report.synthetic);
    if report.synthetic {
        out.push_str("人工构造演示数据，不含真实用户信息；试点结果不能代表真实用户检索质量。\n\n");
    }
    let _ = writeln!(out, "Run: {} · status: {} · split: {} · mode: {}\n", cell(&report.run_id), cell(&report.status), cell(&report.split), cell(&report.mode));
    let _ = writeln!(out, "Snapshot SHA256: {} · schema: {}\n", cell(&report.dataset_fingerprint), report.schema_version);
    let _ = writeln!(out, "Started: {} · finished: {} · preparation: {} ms\n", cell(&report.started_at), cell(report.finished_at.as_deref().unwrap_or("unfinished")), report.preparation_ms);
    let b = &report.backend;
    let _ = writeln!(out, "Embedding model: {} · location: {}\n\nRouting model: {} · provider: {} · remote={}\n", cell(b.embedding_model.as_deref().unwrap_or("unspecified")), cell(&b.embedding_location), cell(b.routing_model.as_deref().unwrap_or("disabled/unspecified")), cell(&b.routing_provider), b.routing_remote);
    out.push_str("## Overall\n\n| Planned | Executed | Success | Failed | Not run | Degraded | Partial index | Answerable | Unanswerable |\n| --- | --- | --- | --- | --- | --- | --- | --- | --- |\n");
    let _ = writeln!(out, "| {} | {} | {} | {} | {} | {} | {} | {} | {} |\n", s.planned_count, s.executed_count, s.successful_count, s.failed_count, s.not_run_count, s.degraded_count, s.partial_index_count, s.answerable_count, s.unanswerable_count);
    out.push_str("| Recall@K | MRR | NDCG@K | Duplicate rate | Forbidden rate | Unanswerable FP rate | Domain accuracy | p50 ms | p95 ms | Mean returned chars |\n| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |\n");
    let _ = writeln!(out, "| {} | {} | {} | {} | {} | {} | {} | {} | {} | {} |\n", number(s.recall_at_k), number(s.mrr), number(s.ndcg_at_k), number(s.duplicate_rate), number(s.forbidden_rate), number(s.false_positive_rate), number(s.domain_accuracy), number(s.p50_ms), number(s.p95_ms), number(s.mean_returned_chars));
    out.push_str("Counts refer to executed cases except planned/not run. Failed cases remain in executed denominators. Recall/MRR/NDCG average answerable cases; false positives average unanswerable cases. Domain accuracy uses only domain_hit=Some. Rates are macro averages. Latency includes failures, excludes preparation, and uses nearest-rank percentiles. — means no eligible observations. Success indicates execution completion, not a quality pass.\n\n");
    let _ = writeln!(out, "| Evidence coverage | Evidence status accuracy |\n| --- | --- |\n| {} | {} |\n", number(s.evidence_coverage), number(s.evidence_status_accuracy));
    out.push_str("来源覆盖只按已返回原文片段中的来源键和 contains 实际匹配计分；来源状态独立计分。仅状态题不参与原文覆盖率。已执行标注题失败、超时或缺失证据记零；无标注或未执行表示未测（—）。状态可标注响应状态或所有预期记忆一致的条目状态，缺少任一预期记忆时不命中。延迟含追加取证，准备耗时独立。报告不包含原文、token 或 cursor。\n\n## Cases\n\n| Case | Category / split | Query | Status | Expected | Forbidden | Returned | Expected domains | Active domains | Domain hit | Recall | MRR | NDCG | Duplicate | Forbidden rate | Empty-query FP | Degraded | Index complete | ms | Chars | Evidence coverage | Evidence status hit | Evidence ms |\n| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |\n");
    for c in &report.cases {
        let ran = c.status != "not_run";
        let m = &c.metrics;
        let _ = write!(out, "| {} | {} / {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} |", cell(&c.case_id), cell(&c.category), cell(&c.split), cell(&c.query), cell(&c.status), ids(&c.expected_ids), ids(&c.forbidden_ids), ids(&c.returned_ids), ids(&c.expected_domain_ids), ids(&c.active_domains), if ran { c.domain_hit.map(|h| h.to_string()).unwrap_or_else(|| "—".into()) } else { "—".into() }, number(if ran { m.recall_at_k } else { None }), number(if ran { m.mrr } else { None }), number(if ran { m.ndcg_at_k } else { None }), number(ran.then_some(m.duplicate_rate)), number(ran.then_some(m.forbidden_rate)), if ran { m.false_positive_on_empty_query.to_string() } else { "—".into() }, if ran { c.degraded.to_string() } else { "—".into() }, if ran { c.index_complete.to_string() } else { "—".into() }, number(ran.then_some(c.elapsed_ms as f64)), number(ran.then_some(c.returned_chars as f64)));
        let _ = writeln!(out, " {} | {} | {} |", number(if ran { c.evidence_coverage } else { None }),
            if ran { c.evidence_status_hit.map(|hit| hit.to_string()).unwrap_or_else(|| "—".into()) } else { "—".into() },
            number(if ran { c.evidence_ms.map(|ms| ms as f64) } else { None }));
    }
    out.push_str("\n## Failures, degradation and omissions\n\n");
    for c in &report.cases {
        if c.status != "success" || c.degraded || !c.index_complete {
            let _ = writeln!(out, "- {}: status={}, degraded={}, index_complete={}. Raw diagnostics omitted.", cell(&c.case_id), cell(&c.status), c.degraded, c.index_complete);
        }
    }
    let _ = writeln!(out, "\nNot run: {} (includes planned cases without result rows).", s.not_run_count);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn case(status: &str, expected: &[String], returned: &[String]) -> CaseResult {
        CaseResult {
            case_id: "case".into(), category: "demo".into(), split: "development".into(),
            query: "demo".into(), status: status.into(), error: None,
            expected_ids: expected.to_vec(), forbidden_ids: vec![], returned_ids: returned.to_vec(),
            expected_domain_ids: vec![], active_domains: vec![], metrics: evaluate(expected, returned, &[], 3),
            domain_hit: None, degraded: false, index_complete: true,
            retrieval: serde_json::Value::Null, elapsed_ms: 10, returned_chars: 5, evidence_coverage: None,
            evidence_status_hit: None, evidence_ms: None,
        }
    }

    #[test]
    fn failures_and_omissions_keep_distinct_denominators() {
        let mut failed = case("failed", &["a".into()], &[]);
        failed.domain_hit = Some(false);
        failed.degraded = true;
        failed.index_complete = false;
        let rows = vec![case("success", &["a".into()], &["a".into()]), failed,
            case("success", &[], &["x".into()]), case("not_run", &["a".into()], &[])];
        let s = aggregate(&rows, 5);
        assert_eq!((s.executed_count, s.successful_count, s.failed_count, s.not_run_count), (3, 2, 1, 2));
        assert_eq!(s.recall_at_k, Some(0.5));
        assert_eq!(s.false_positive_rate, Some(1.0));
        assert_eq!(s.domain_accuracy, Some(0.0));
        assert_eq!((s.degraded_count, s.partial_index_count), (1, 1));
        assert_eq!(aggregate(&[], 3).recall_at_k, None);
        assert_eq!(aggregate(&[], 3).p50_ms, None);
    }

    #[test]
    fn evidence_counts_actual_excerpts_and_independent_item_statuses() {
        let data = super::super::dataset::builtin_evidence().unwrap();
        let mut case = data.cases[0].clone();
        case.expected_evidence_status = Some("ok".into());
        let mut response: EvidenceResponse = serde_json::from_value(serde_json::json!({
            "request_id":"synthetic", "context_request_id":null, "status":"ok", "expires_at":null,
            "returned_chars":5, "remaining_chars":0, "partial":false, "next_cursor":null, "instruction":"",
            "items":[{"memory_id":"ev-middle", "status":"ok", "sources":[{
                "source_event_key":"synthetic-source-a", "source_agent":"claude", "session_id":"synthetic-session-a",
                "occurred_at":null, "source_fingerprint":null, "status":"available", "excerpt":{
                    "text":"unrelated text", "char_start":0, "char_end":14, "total_chars":14,
                    "match_method":"query", "excerpt_fingerprint":"synthetic"
                }
            }]}]
        })).unwrap();
        assert_eq!(evidence_scores(&case, &case.expected_ids, &response), (Some(0.0), Some(true)));
        response.items[0].sources[0].excerpt.as_mut().unwrap().text = case.expected_evidence[0].contains.clone();
        assert_eq!(evidence_scores(&case, &case.expected_ids, &response), (Some(1.0), Some(true)));
        response.items[0].sources[0].source.source_event_key = "unrelated-source".into();
        assert_eq!(evidence_scores(&case, &case.expected_ids, &response).0, Some(0.0));
        assert_eq!(evidence_scores(&case, &[], &response), (Some(0.0), Some(false)));
        let merged = &data.cases[1];
        response.items[0].memory_id = merged.expected_ids[0].clone();
        response.items[0].sources[0].source.source_event_key = merged.expected_evidence[0].source_event_key.clone();
        response.items[0].sources[0].excerpt.as_mut().unwrap().text = merged.expected_evidence[0].contains.clone();
        assert_eq!(evidence_scores(merged, &merged.expected_ids, &response), (Some(0.5), None));
        response.items[0].memory_id = case.expected_ids[0].clone();
        case.expected_evidence.clear();
        case.expected_evidence_status = Some("missing_source".into());
        response.status = "source_unavailable".into();
        response.items[0].status = "missing_source".into();
        response.items[0].sources.clear();
        assert_eq!(evidence_scores(&case, &case.expected_ids, &response), (None, Some(true)));
    }

    #[test]
    fn evidence_averages_only_annotations_and_old_reports_deserialize() {
        let mut covered = case("success", &["a".into()], &["a".into()]);
        covered.evidence_coverage = Some(1.0);
        let mut failed = case("failed", &["a".into()], &[]);
        failed.evidence_coverage = Some(0.0);
        failed.evidence_status_hit = Some(false);
        let mut status_only = case("success", &["a".into()], &["a".into()]);
        status_only.evidence_status_hit = Some(true);
        let rows = vec![covered.clone(), failed, status_only, case("not_run", &["a".into()], &[]), case("success", &[], &[])];
        let summary = aggregate(&rows, rows.len());
        assert_eq!(summary.evidence_coverage, Some(0.5));
        assert_eq!(summary.evidence_status_accuracy, Some(0.5));
        let mut old = serde_json::to_value(&covered).unwrap();
        for field in ["evidence_status_hit", "evidence_ms"] { old.as_object_mut().unwrap().remove(field); }
        let decoded: CaseResult = serde_json::from_value(old).unwrap();
        assert_eq!(decoded.evidence_status_hit, None);
        let mut old = serde_json::to_value(summary).unwrap();
        for field in ["evidence_coverage", "evidence_status_accuracy"] { old.as_object_mut().unwrap().remove(field); }
        let decoded: AggregateMetrics = serde_json::from_value(old).unwrap();
        assert_eq!((decoded.evidence_coverage, decoded.evidence_status_accuracy), (None, None));
    }

    #[test]
    fn export_cells_escape_markup_without_losing_the_actual_question() {
        let escaped = cell("a|b\n<script> [x](https://example.invalid/key) `code`");
        assert!(!escaped.contains('|'));
        assert!(!escaped.contains('\n'));
        assert!(!escaped.contains('<'));
        assert!(escaped.contains("https://example.invalid/key"));
        assert_eq!(percentile(&[1, 2, 3, 100], 95), Some(100.0));
    }
}
