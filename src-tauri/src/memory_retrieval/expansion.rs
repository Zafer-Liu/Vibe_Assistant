//! Local, conditional lexical backfill. Variants never invoke an embedding or
//! generation model, change scope, or create extra RRF votes for the same ID.
use super::RecallBudget;
use crate::telemetry_store::{LocalMemory, TelemetryStore};
use serde::Serialize;
use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

const MAX_VARIANTS: usize = 3;
const MAX_ROWS_PER_VARIANT: usize = 24;
const MAX_BUDGET: Duration = Duration::from_millis(200);

#[derive(Serialize)]
pub(super) struct Diagnostics {
    pub status: &'static str,
    pub variants: usize,
    pub searched: usize,
    pub added_candidates: usize,
    pub elapsed_ms: u64,
}
impl Diagnostics {
    pub(super) fn skipped(status: &'static str) -> Self {
        Self { status, variants: 0, searched: 0, added_candidates: 0, elapsed_ms: 0 }
    }
}

#[derive(Debug, PartialEq, Eq)]
struct Variant {
    replacement: String,
    required: Vec<String>,
}

// Bounded, transparent vocabulary. The original query still owns ranking;
// replacements only nominate additional candidates when that pool is short.
const GROUPS: &[&[&str]] = &[
    &["缓存", "cache"], &["超时", "timeout"], &["嵌入", "embedding"],
    &["数据库", "database"], &["备份", "backup"], &["快照", "snapshot"],
    &["回滚", "rollback"], &["并发", "concurrency"], &["取消", "cancel"],
    &["权限", "permission"], &["编译", "compile"], &["构建", "build"],
    &["配置", "config", "configuration"], &["崩溃", "crash"],
];

fn cjk(c: char) -> bool { ('\u{4e00}'..='\u{9fff}').contains(&c) }
fn identifier(c: char) -> bool { c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.') }

fn anchors(residual: &str) -> Vec<String> {
    const FILLER: &[&str] = &[
        "the", "a", "an", "how", "what", "where", "why", "does", "do", "is", "are", "was", "were",
        "can", "could", "would", "should", "please", "find", "show", "about", "for", "of", "to", "in",
        "and", "or", "with", "me", "my", "如何", "怎么", "怎样", "哪里", "什么",
        "请", "请问", "帮我", "查找", "查一下", "搜索", "一下", "的", "了", "吗", "呢",
    ];
    let mut result = Vec::new();
    let mut token = String::new();
    let mut previous_cjk = false;
    for c in residual.chars().chain(std::iter::once(' ')) {
        let is_cjk = cjk(c);
        let word = identifier(c) || is_cjk;
        if !token.is_empty() && (!word || is_cjk != previous_cjk) {
            if !FILLER.contains(&token.as_str()) { result.push(std::mem::take(&mut token)); }
            else { token.clear(); }
        }
        if word { token.push(c); previous_cjk = is_cjk; }
    }
    result.sort();
    result.dedup();
    result
}

fn variants(query: &str) -> Vec<Variant> {
    let query = query.trim().to_lowercase();
    // URLs, paths and quoted identifiers must not be interpreted as prose.
    if query.contains("://") || query.contains(['/', '\\', '`', '"', '\'']) { return Vec::new(); }
    let mut variants = Vec::new();
    for group in GROUPS {
        for &trigger in *group {
            let Some((start, _)) = query.match_indices(trigger).find(|&(start, _)| {
                if trigger.chars().all(|c| c.is_ascii()) {
                    !query[..start].chars().next_back().is_some_and(identifier)
                        && !query[start + trigger.len()..].chars().next().is_some_and(identifier)
                } else { true }
            }) else { continue };
            let mut residual = query.clone();
            residual.replace_range(start..start + trigger.len(), " ");
            let required = anchors(&residual);
            // Avoid dynamic SQL growth for paragraph-sized queries. Dropping
            // anchors to fit would widen the user's question, so skip instead.
            if required.len() > 12 { continue; }
            for &replacement in *group {
                if replacement == trigger || query.contains(replacement) { continue; }
                let variant = Variant { replacement: replacement.into(), required: required.clone() };
                if !variants.contains(&variant) { variants.push(variant); }
                if variants.len() >= MAX_VARIANTS { return variants; }
            }
        }
    }
    variants
}

fn contains_anchor(haystack: &str, anchor: &str) -> bool {
    if !anchor.is_ascii() { return haystack.contains(anchor); }
    haystack.match_indices(anchor).any(|(start, _)| {
        let before = &haystack[..start];
        let after = &haystack[start + anchor.len()..];
        let left = before.chars().next_back();
        let right = after.chars().next();
        // A terminal full stop is punctuation; dots inside versions or names
        // still belong to the literal (64 must not broaden into 640 or 64.5).
        let left_word = left.is_some_and(|c| identifier(c) && c != '.')
            || (left == Some('.') && before.chars().rev().nth(1).is_some_and(identifier));
        let right_word = right.is_some_and(|c| identifier(c) && c != '.')
            || (right == Some('.') && after.chars().nth(1).is_some_and(identifier));
        !left_word && !right_word
    })
}

fn pattern(term: &str) -> String {
    format!("%{}%", term.replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_"))
}

fn search(
    store: &TelemetryStore, variant: &Variant, domains: Option<&[String]>, budget: &RecallBudget,
) -> Result<Vec<LocalMemory>, String> {
    budget.remaining()?;
    if domains.is_some_and(|scope| scope.is_empty()) { return Ok(Vec::new()); }
    let mut parameters = vec![pattern(&variant.replacement)];
    let mut conditions = vec!["m.memory LIKE ? ESCAPE '\\'".to_string()];
    for anchor in &variant.required {
        conditions.push("m.memory LIKE ? ESCAPE '\\'".into());
        parameters.push(pattern(anchor));
    }
    if let Some(scope) = domains {
        let placeholders = vec!["?"; scope.len()].join(",");
        conditions.push(format!("EXISTS (SELECT 1 FROM local_memory_domains md WHERE md.memory_id=m.id AND md.domain_id IN ({placeholders}))"));
        parameters.extend(scope.iter().cloned());
    }
    let sql = format!(
        "SELECT m.id,m.memory,m.memory_type,m.durability,m.user_defined,m.updated_at,m.event_time
         FROM local_memory_items m WHERE {} ORDER BY m.updated_at DESC,m.id ASC LIMIT {MAX_ROWS_PER_VARIANT}",
        conditions.join(" AND "),
    );
    store.with_budgeted_read_connection(budget, |conn| {
        let mut statement = conn.prepare(&sql).map_err(|error| error.to_string())?;
        let mut rows = statement.query(rusqlite::params_from_iter(parameters.iter())).map_err(|error| error.to_string())?;
        let mut results = Vec::new();
        loop {
            budget.remaining()?;
            let Some(row) = rows.next().map_err(|error| error.to_string())? else { break };
            let memory: String = row.get(1).map_err(|error| error.to_string())?;
            // LIKE only prefilters. Every retained condition must match its
            // own identity, including numbers, negation and technical names.
            let lower = memory.to_lowercase();
            if !contains_anchor(&lower, &variant.replacement)
                || !variant.required.iter().all(|anchor| contains_anchor(&lower, anchor)) { continue; }
            results.push(LocalMemory {
                id: row.get(0).map_err(|error| error.to_string())?, memory,
                memory_type: row.get(2).map_err(|error| error.to_string())?,
                durability: row.get(3).map_err(|error| error.to_string())?,
                user_defined: row.get::<_, i64>(4).map_err(|error| error.to_string())? != 0,
                last_update_at: row.get(5).map_err(|error| error.to_string())?,
                event_time: row.get(6).map_err(|error| error.to_string())?,
                score: Some(0.35),
            });
        }
        Ok(results)
    })
}

pub(super) fn backfill(
    store: &TelemetryStore, query: &str, domains: Option<&[String]>, existing: &mut Vec<LocalMemory>,
    candidate_limit: usize, budget: &RecallBudget,
) -> Diagnostics {
    let variants = variants(query);
    if variants.is_empty() { return Diagnostics::skipped("no_variants"); }
    let Ok(remaining) = budget.remaining() else { return Diagnostics::skipped("budget_exhausted") };
    let mut bounded = budget.clone();
    let started = Instant::now();
    bounded.deadline = bounded.deadline.min(started + remaining.min(MAX_BUDGET));
    let mut diagnostics = Diagnostics { status: "completed", variants: variants.len(), searched: 0, added_candidates: 0, elapsed_ms: 0 };
    let mut additions: HashMap<String, LocalMemory> = HashMap::new();
    let existing_ids: HashSet<_> = existing.iter().map(|memory| memory.id.as_str()).collect();
    for variant in variants {
        if bounded.remaining().is_err() { diagnostics.status = "budget_exhausted"; break; }
        diagnostics.searched += 1;
        match search(store, &variant, domains, &bounded) {
            Ok(results) => for memory in results {
                if !existing_ids.contains(memory.id.as_str()) { additions.entry(memory.id.clone()).or_insert(memory); }
            },
            Err(_) => {
                diagnostics.status = if bounded.remaining().is_err() { "budget_exhausted" } else { "unavailable" };
                break;
            }
        }
    }
    // Original candidates retain their ordering and vote. Cross-variant hits
    // contribute once and have stable relative ranks, independent of query count.
    let mut additions: Vec<_> = additions.into_values().collect();
    additions.sort_by(|a, b| b.last_update_at.cmp(&a.last_update_at).then_with(|| a.id.cmp(&b.id)));
    additions.truncate(candidate_limit.saturating_sub(existing.len()));
    diagnostics.added_candidates = additions.len();
    diagnostics.elapsed_ms = started.elapsed().as_millis() as u64;
    existing.extend(additions);
    diagnostics
}

#[cfg(test)]
mod tests;
