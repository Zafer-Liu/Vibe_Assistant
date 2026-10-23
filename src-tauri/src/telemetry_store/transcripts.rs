//! Native transcript discovery, token accounting, and MCP observation parsing.
use super::types::{TranscriptUsage, TranscriptUsageRecord};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

pub(super) fn discover_native_transcripts(
    sessions: &[(String, String, Option<PathBuf>)],
) -> HashMap<(String, String), PathBuf> {
    let wanted = |source: &str| {
        sessions
            .iter()
            .filter(|(kind, _, _)| kind == source)
            .map(|(_, session_id, _)| session_id.clone())
            .collect::<HashSet<_>>()
    };
    let mut found = HashMap::new();
    for source in crate::agent_sources::AGENT_SOURCE_IDS {
        let wanted = wanted(source);
        if wanted.is_empty() {
            continue;
        }
        for root in crate::agent_sources::transcript_roots(source) {
            scan_transcript_tree(&root, source, &wanted, &mut found);
        }
    }
    found
}

/// Enumerate native transcript files even if an Agent did not send a Hook.
/// This is a metadata-only discovery pass; callers decide whether a file has
/// changed and therefore needs its content parsed.  Codex keeps older session
/// files under `archived_sessions`, which is just as authoritative as its live
/// `sessions` tree for usage history.  Transcript roots come from the agent
/// source registry so per-device overrides apply to every supported Agent.
pub fn discover_all_native_transcripts() -> Vec<(String, String, PathBuf)> {
    let mut latest = HashMap::<(String, String), (std::time::SystemTime, PathBuf)>::new();
    for source in crate::agent_sources::AGENT_SOURCE_IDS {
        for root in crate::agent_sources::transcript_roots(source) {
            scan_all_transcript_tree(root.as_path(), source, &mut latest);
        }
    }
    latest
        .into_iter()
        .map(|((source, session_id), (_, path))| (source, session_id, path))
        .collect()
}

pub(super) fn scan_all_transcript_tree(
    root: &Path,
    source: &str,
    found: &mut HashMap<(String, String), (std::time::SystemTime, PathBuf)>,
) {
    let mut pending = vec![root.to_path_buf()];
    let mut visited = 0usize;
    while let Some(directory) = pending.pop() {
        if visited >= 20_000 {
            break;
        }
        let Ok(entries) = std::fs::read_dir(directory) else {
            continue;
        };
        for entry in entries.flatten() {
            visited += 1;
            let path = entry.path();
            if path.is_dir() {
                pending.push(path);
                continue;
            }
            // DeepSeek Harness 的转录是 `session.vN.jsonl.zstd`（扩展名 zstd）。
            let is_transcript = matches!(
                path.extension().and_then(|extension| extension.to_str()),
                Some("jsonl") | Some("zstd")
            );
            if !is_transcript {
                continue;
            }
            let Some(session_id) = transcript_session_id(source, &path) else {
                continue;
            };
            let modified = entry
                .metadata()
                .ok()
                .and_then(|metadata| metadata.modified().ok())
                .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
            let key = (source.to_string(), session_id);
            if found
                .get(&key)
                .map(|(current, _)| modified > *current)
                .unwrap_or(true)
            {
                found.insert(key, (modified, path));
            }
        }
    }
}

pub(super) fn transcript_session_id(source: &str, path: &Path) -> Option<String> {
    crate::agent_sources::session_id_for_path(source, path)
}

pub(super) fn scan_transcript_tree(
    root: &Path,
    source: &str,
    wanted: &HashSet<String>,
    found: &mut HashMap<(String, String), PathBuf>,
) {
    let mut pending = vec![root.to_path_buf()];
    // A safety ceiling prevents an unrelated, malformed profile directory
    // from making the Memory Center refresh indefinitely.
    let mut visited = 0usize;
    while let Some(dir) = pending.pop() {
        if visited >= 20_000
            || found.keys().filter(|(kind, _)| kind == source).count() >= wanted.len()
        {
            break;
        }
        let Ok(entries) = std::fs::read_dir(dir) else {
            continue;
        };
        for entry in entries.flatten() {
            visited += 1;
            let path = entry.path();
            if path.is_dir() {
                pending.push(path);
                continue;
            }
            // DeepSeek Harness 的转录是 `session.vN.jsonl.zstd`（扩展名 zstd）。
            let is_transcript = matches!(
                path.extension().and_then(|extension| extension.to_str()),
                Some("jsonl") | Some("zstd")
            );
            if !is_transcript {
                continue;
            }
            // 会话 id 推导统一走注册表：Claude/Qoder/WorkBuddy 用文件 stem，
            // Codex 用文件名末尾 UUID，MiniMax/Kimi 用会话目录名。
            let session_id = crate::agent_sources::session_id_for_path(source, &path)
                .filter(|id| wanted.contains(id));
            if let Some(session_id) = session_id {
                found
                    .entry((source.to_string(), session_id.to_string()))
                    .or_insert(path);
            }
        }
    }
}

/// Codex 的 `total_token_usage` / `last_token_usage` 里 `input_tokens` 不含
/// 缓存，缓存读取/写入单独成字段。为与 Claude/MiniMax/Kimi 统一「输入=真实
/// 输入上下文」口径，把 cache_read + cache_creation 并入 input，缓存命中仍
/// 单独保留为 cache_read 子集，供「缓存命中率」展示。这样 `cached <= input`
/// 恒成立，命中率始终落在 [0, 100%]。
pub(super) fn codex_usage(value: &Value) -> Option<TranscriptUsage> {
    let input = get_i64(value, &["input_tokens"]);
    let output = get_i64(value, &["output_tokens"]);
    if input.is_none() && output.is_none() {
        return None;
    }
    let cache_read =
        get_i64(value, &["cache_read_input_tokens", "cached_input_tokens"]).unwrap_or(0);
    let cache_write = get_i64(value, &["cache_creation_input_tokens"]).unwrap_or(0);
    Some(TranscriptUsage {
        input_tokens: input.unwrap_or(0) + cache_read + cache_write,
        output_tokens: output.unwrap_or(0),
        cached_tokens: cache_read,
    })
}

/// WorkBuddy 把 usage 嵌在转录行的 `message.usage`（蛇形，Chat Completions）
/// 或 `providerData.usage`（驼峰，Responses API），而不是顶层 `usage`。其
/// `input_tokens` 不含缓存，缓存读取单独在 `cache_read_input_tokens`；按
/// Claude 口径把 cache_read 并入 input，缓存命中单独保留，`cached <= input`
/// 恒成立。优先用 message.usage 的蛇形字段，缺失时回退 providerData.usage。
pub(super) fn workbuddy_usage(entry: &Value) -> Option<TranscriptUsage> {
    let snake = entry
        .get("message")
        .and_then(|message| message.get("usage"));
    if let Some(usage) = snake {
        let input = get_i64(usage, &["input_tokens", "prompt_tokens"]);
        let output = get_i64(usage, &["output_tokens", "completion_tokens"]);
        if input.is_some() || output.is_some() {
            let cache_read =
                get_i64(usage, &["cache_read_input_tokens", "cached_tokens"]).unwrap_or(0);
            return Some(TranscriptUsage {
                input_tokens: input.unwrap_or(0) + cache_read,
                output_tokens: output.unwrap_or(0),
                cached_tokens: cache_read,
            });
        }
    }
    // Responses API 回退：providerData.usage 的驼峰字段，缓存命中藏在
    // inputTokensDetails[0].cached_tokens。
    let camel = entry
        .get("providerData")
        .and_then(|provider| provider.get("usage"));
    if let Some(usage) = camel {
        let input = get_i64(usage, &["inputTokens"]);
        let output = get_i64(usage, &["outputTokens"]);
        if input.is_some() || output.is_some() {
            let cache_read = usage
                .get("inputTokensDetails")
                .and_then(Value::as_array)
                .and_then(|details| details.first())
                .and_then(|detail| get_i64(detail, &["cached_tokens"]))
                .unwrap_or(0);
            return Some(TranscriptUsage {
                input_tokens: input.unwrap_or(0) + cache_read,
                output_tokens: output.unwrap_or(0),
                cached_tokens: cache_read,
            });
        }
    }
    None
}

pub(super) fn usage_record_id(
    source: &str,
    session_id: &str,
    kind: &str,
    identity: &str,
) -> String {
    let digest = Sha256::digest(identity.as_bytes());
    format!(
        "{source}:v1:{session_id}:{kind}:{}",
        hex(&digest)[..20].to_string()
    )
}

/// Sum the per-response detail into the session total.  Deriving one
/// projection from the other guarantees the dashboard feed (detail-first)
/// and the session-level fallback can never disagree for the same session.
pub(super) fn summarize_usage_records(records: &[TranscriptUsageRecord]) -> TranscriptUsage {
    records.iter().fold(
        TranscriptUsage {
            input_tokens: 0,
            output_tokens: 0,
            cached_tokens: 0,
        },
        |mut total, record| {
            total.input_tokens += record.input_tokens;
            total.output_tokens += record.output_tokens;
            total.cached_tokens += record.cached_tokens;
            total
        },
    )
}

/// Provenance of a transcript-derived session total: native provider
/// counters, or — for Qoder, whose transcripts carry none — a transparent
/// transcript estimate.
pub(super) fn transcript_usage_origin(source: &str) -> &'static str {
    if source == "qoder" {
        "estimated_transcript"
    } else {
        "native_transcript"
    }
}

/// Parse any supported RFC3339 transcript timestamp and emit the canonical
/// millisecond-UTC form used across the usage ledger, so lexical SQL
/// comparisons and the frontend's ISO bounds stay consistent.
pub(super) fn normalize_occurred_at(value: Option<&str>) -> Option<String> {
    let value = value?.trim();
    if value.is_empty() {
        return None;
    }
    chrono::DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|time| time.to_rfc3339_opts(chrono::SecondsFormat::Millis, true))
}

pub(super) fn opaque_proxy_id(value: &str) -> Result<String, String> {
    if value.is_empty() || value.len() > 256 {
        return Err("invalid proxy identifier".into());
    }
    Ok(hex(&Sha256::digest(value.as_bytes())))
}

pub(super) fn proxy_call_identity(key: &str) -> Result<(String, String), String> {
    let (session, sequence) = key.rsplit_once(':').ok_or("invalid proxy call key")?;
    if session.is_empty() || sequence.is_empty() || key.len() > 256 {
        return Err("invalid proxy call key".into());
    }
    Ok((
        format!("proxy:{}", opaque_proxy_id(key)?),
        opaque_proxy_id(session)?,
    ))
}

pub(super) fn sanitized_mcp_identifier(value: &str) -> String {
    let value = value.trim();
    if value.is_empty() {
        return "unknown".into();
    }
    if value.len() > 80
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
    {
        return format!("invalid-{}", &hex(&Sha256::digest(value.as_bytes()))[..16]);
    }
    value.to_string()
}

/// Claude Code JSONL tool_use/tool_result observations, rebuilt by a manual
/// refresh while relay observations remain durable.
#[derive(Clone)]
pub(super) struct ParsedMcpCall {
    pub(super) key: String,
    pub(super) source: String,
    pub(super) session_id: String,
    pub(super) occurred_at: String,
    pub(super) server_name: String,
    pub(super) tool_name: String,
    pub(super) status: String,
    pub(super) error_category: Option<String>,
    pub(super) result_observed: bool,
}

/// Classify a failed MCP result into a fixed vocabulary; never retain result text.
/// A generic `is_error` flag alone cannot reveal the underlying cause.
pub(super) fn transcript_mcp_failure_category(block: &Value) -> &'static str {
    fn classify(text: &str) -> &'static str {
        let text = text
            .chars()
            .take(4096)
            .collect::<String>()
            .to_ascii_lowercase();
        let has = |terms: &[&str]| terms.iter().any(|term| text.contains(term));
        let status = |code: &str| {
            text.split(|ch: char| !ch.is_ascii_digit())
                .any(|part| part == code)
        };
        if has(&["rate limit", "rate_limit", "too many requests"]) || status("429") {
            "rate_limit"
        } else if has(&["unauthorized", "invalid api key", "authentication failed"])
            || status("401")
        {
            "authentication"
        } else if has(&["permission denied", "access denied", "forbidden"]) || status("403") {
            "permission_denied"
        } else if has(&["timed out", "timeout", "deadline exceeded"]) {
            "timeout"
        } else if has(&["not found", "enoent"]) || status("404") {
            "not_found"
        } else if has(&[
            "connection refused",
            "connection reset",
            "network error",
            "econnrefused",
            "econnreset",
            "dns error",
        ]) {
            "connection_error"
        } else if has(&[
            "invalid argument",
            "invalid input",
            "bad request",
            "validation error",
        ]) || status("400")
        {
            "invalid_request"
        } else if has(&[
            "internal server error",
            "service unavailable",
            "bad gateway",
        ]) || ["502", "503", "504"].iter().any(|code| status(code))
        {
            "server_error"
        } else {
            "unknown_error"
        }
    }
    if let Some(content) = block.get("content").and_then(Value::as_str) {
        return classify(content);
    }
    block
        .get("content")
        .and_then(Value::as_array)
        .and_then(|blocks| {
            blocks
                .iter()
                .take(16)
                .filter_map(|item| item.get("text").and_then(Value::as_str))
                .map(classify)
                .find(|category| *category != "unknown_error")
        })
        .unwrap_or("unknown_error")
}

pub(super) fn read_transcript_mcp_calls(
    source: &str,
    session_id: &str,
    path: &Path,
) -> Result<Vec<ParsedMcpCall>, String> {
    if source == "dsh" {
        return read_dsh_transcript_mcp_calls(session_id, path);
    }
    if source != "claude" {
        return Err("unsupported MCP transcript format".into());
    }
    let file = std::fs::File::open(path).map_err(|e| e.to_string())?;
    let mut pending: HashMap<String, ParsedMcpCall> = HashMap::new();
    let opaque_session_id = hex(&Sha256::digest(session_id.as_bytes()));
    let mut recognized_messages = 0;
    for line in BufReader::new(file).lines() {
        let line = line.map_err(|e| e.to_string())?;
        let Ok(entry) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        let role = entry.get("type").and_then(Value::as_str);
        if !matches!(role, Some("assistant" | "user")) {
            continue;
        }
        let Some(blocks) = entry
            .get("message")
            .and_then(|m| m.get("content"))
            .and_then(Value::as_array)
        else {
            continue;
        };
        recognized_messages += 1;
        let timestamp =
            normalize_occurred_at(get_str(&entry, &["timestamp"])).unwrap_or_else(|| {
                chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
            });
        for block in blocks {
            if role == Some("assistant")
                && block.get("type").and_then(Value::as_str) == Some("tool_use")
            {
                let Some(name) = block.get("name").and_then(Value::as_str) else {
                    continue;
                };
                let Some(rest) = name.strip_prefix("mcp__") else {
                    continue;
                };
                let Some((server, tool)) = rest.split_once("__") else {
                    continue;
                };
                let Some(id) = block.get("id").and_then(Value::as_str) else {
                    continue;
                };
                let identity = hex(&Sha256::digest(
                    format!("{source}:{session_id}:{id}").as_bytes(),
                ));
                pending.insert(
                    id.to_string(),
                    ParsedMcpCall {
                        key: format!("transcript:{identity}"),
                        source: source.to_string(),
                        session_id: opaque_session_id.clone(),
                        occurred_at: timestamp.clone(),
                        server_name: sanitized_mcp_identifier(server),
                        tool_name: sanitized_mcp_identifier(tool),
                        status: "pending".into(),
                        error_category: None,
                        result_observed: false,
                    },
                );
            }
            if role == Some("user")
                && block.get("type").and_then(Value::as_str) == Some("tool_result")
            {
                let Some(id) = block.get("tool_use_id").and_then(Value::as_str) else {
                    continue;
                };
                if let Some(call) = pending.get_mut(id) {
                    call.result_observed = true;
                    let failed = block
                        .get("is_error")
                        .and_then(Value::as_bool)
                        .unwrap_or(false);
                    call.status = if failed { "failed" } else { "success" }.into();
                    call.error_category =
                        failed.then(|| transcript_mcp_failure_category(block).to_string());
                }
            }
        }
    }
    if recognized_messages == 0 {
        return Err("no supported transcript messages".into());
    }
    Ok(pending.into_values().collect())
}

/// DeepSeek Harness 的 MCP 调用观测：`tool/call` 的 `data.name` 采用
/// `mcp__<server>__<tool>` 命名（与 Claude Code 同构），`tool/result` 的
/// `data.message.toolCallId` 配对回填结果与 `isError` 分类。工具入参
/// 与结果正文不落库，只保留成功/失败观测。
pub(super) fn read_dsh_transcript_mcp_calls(
    session_id: &str,
    path: &Path,
) -> Result<Vec<ParsedMcpCall>, String> {
    let reader = crate::dsh_transcript::open_lines(path).map_err(|e| e.to_string())?;
    let mut pending: HashMap<String, ParsedMcpCall> = HashMap::new();
    let opaque_session_id = hex(&Sha256::digest(session_id.as_bytes()));
    let mut recognized_calls = 0usize;
    for line in reader.lines().map_while(Result::ok) {
        let Ok(entry) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        let data = entry.get("data");
        let timestamp = entry
            .get("time")
            .and_then(Value::as_i64)
            .and_then(chrono::DateTime::<chrono::Utc>::from_timestamp_millis)
            .map(|time| time.to_rfc3339())
            .unwrap_or_else(|| {
                chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
            });
        match entry.get("type").and_then(Value::as_str) {
            Some("tool/call") => {
                let Some(name) = data
                    .and_then(|data| data.get("name"))
                    .and_then(Value::as_str)
                else {
                    continue;
                };
                let Some(call_id) = data
                    .and_then(|data| data.get("callId"))
                    .and_then(Value::as_str)
                else {
                    continue;
                };
                let Some(rest) = name.strip_prefix("mcp__") else {
                    continue;
                };
                let Some((server, tool)) = rest.split_once("__") else {
                    continue;
                };
                recognized_calls += 1;
                let identity = hex(&Sha256::digest(
                    format!("dsh:{session_id}:{call_id}").as_bytes(),
                ));
                pending.insert(
                    call_id.to_string(),
                    ParsedMcpCall {
                        key: format!("transcript:{identity}"),
                        source: "dsh".into(),
                        session_id: opaque_session_id.clone(),
                        occurred_at: timestamp,
                        server_name: sanitized_mcp_identifier(server),
                        tool_name: sanitized_mcp_identifier(tool),
                        status: "pending".into(),
                        error_category: None,
                        result_observed: false,
                    },
                );
            }
            Some("tool/result") => {
                let Some(message) = data.and_then(|data| data.get("message")) else {
                    continue;
                };
                let Some(call_id) = message.get("toolCallId").and_then(Value::as_str) else {
                    continue;
                };
                if let Some(call) = pending.get_mut(call_id) {
                    call.result_observed = true;
                    let failed = message
                        .get("isError")
                        .and_then(Value::as_bool)
                        .unwrap_or(false);
                    call.status = if failed { "failed" } else { "success" }.into();
                    call.error_category =
                        failed.then(|| transcript_mcp_failure_category(message).to_string());
                }
            }
            _ => {}
        }
    }
    if recognized_calls == 0 {
        return Err("no supported transcript messages".into());
    }
    Ok(pending.into_values().collect())
}

pub(super) fn read_transcript_usage_records(
    source: &str,
    session_id: &str,
    path: &Path,
) -> Vec<TranscriptUsageRecord> {
    match source {
        "codex" => read_codex_usage_records(session_id, path),
        "claude" => read_claude_usage_records(session_id, path),
        "workbuddy" => read_workbuddy_usage_records(session_id, path),
        // Qoder does not write provider counters today, but its user/assistant
        // rows can use the same transparent transcript estimate at a
        // message-level boundary. They remain explicitly marked estimated.
        "qoder" => read_qoder_usage_records(session_id, path),
        "minimax" => read_minimax_usage_records(session_id, path),
        "kimi" => read_kimi_usage_records(session_id, path),
        "dsh" => read_dsh_usage_records(session_id, path),
        _ => Vec::new(),
    }
}

pub(super) fn read_codex_usage_records(
    session_id: &str,
    path: &Path,
) -> Vec<TranscriptUsageRecord> {
    let Ok(file) = std::fs::File::open(path) else {
        return Vec::new();
    };
    let mut records = Vec::new();
    let mut current_model = None;
    let mut high_water: Option<TranscriptUsage> = None;
    let mut last_without_total: Option<TranscriptUsage> = None;
    for line in BufReader::new(file).lines().map_while(Result::ok) {
        let Ok(entry) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        let timestamp = get_str(&entry, &["timestamp", "occurred_at", "time"]).map(str::to_string);
        let Some(payload) = entry.get("payload") else {
            continue;
        };
        if entry.get("type").and_then(Value::as_str) == Some("turn_context") {
            current_model = get_str(payload, &["model", "model_name"])
                .map(str::to_string)
                .or_else(|| {
                    payload
                        .get("context")
                        .and_then(|context| get_str(context, &["model", "model_name"]))
                        .map(str::to_string)
                });
            continue;
        }
        if entry.get("type").and_then(Value::as_str) != Some("event_msg")
            || payload.get("type").and_then(Value::as_str) != Some("token_count")
        {
            continue;
        }
        let Some(info) = payload.get("info") else {
            continue;
        };
        let total = info.get("total_token_usage").and_then(codex_usage);
        let last = info.get("last_token_usage").and_then(codex_usage);
        let (delta, identity) = if let Some(total) = total {
            let previous = high_water.unwrap_or(TranscriptUsage {
                input_tokens: 0,
                output_tokens: 0,
                cached_tokens: 0,
            });
            // Cumulative counters may be repeated by rate-limit refreshes. A
            // non-increasing snapshot is never another billable response.
            if total.input_tokens + total.output_tokens
                <= previous.input_tokens + previous.output_tokens
            {
                continue;
            }
            high_water = Some(total);
            (
                TranscriptUsage {
                    input_tokens: total.input_tokens.saturating_sub(previous.input_tokens),
                    output_tokens: total.output_tokens.saturating_sub(previous.output_tokens),
                    cached_tokens: total.cached_tokens.saturating_sub(previous.cached_tokens),
                },
                format!(
                    "total:{}:{}:{}",
                    total.input_tokens, total.output_tokens, total.cached_tokens
                ),
            )
        } else if let Some(last) = last {
            // Last-token snapshots are only used when a cumulative counter is
            // absent. Consecutive identical snapshots are rate-limit repeats.
            if last_without_total
                .map(|previous| {
                    previous.input_tokens == last.input_tokens
                        && previous.output_tokens == last.output_tokens
                        && previous.cached_tokens == last.cached_tokens
                })
                .unwrap_or(false)
            {
                continue;
            }
            last_without_total = Some(last);
            (
                last,
                format!(
                    "last:{}:{}:{}:{}",
                    timestamp.as_deref().unwrap_or(""),
                    last.input_tokens,
                    last.output_tokens,
                    last.cached_tokens
                ),
            )
        } else {
            continue;
        };
        if delta.input_tokens == 0 && delta.output_tokens == 0 && delta.cached_tokens == 0 {
            continue;
        }
        records.push(TranscriptUsageRecord {
            record_id: usage_record_id("codex", session_id, "token_count", &identity),
            occurred_at: timestamp,
            model: current_model.clone(),
            input_tokens: delta.input_tokens,
            output_tokens: delta.output_tokens,
            cached_tokens: delta.cached_tokens,
        });
    }
    records
}

pub(super) fn read_claude_usage_records(
    session_id: &str,
    path: &Path,
) -> Vec<TranscriptUsageRecord> {
    let mut seen = HashSet::new();
    let mut records = Vec::new();
    let Ok(file) = std::fs::File::open(path) else {
        return records;
    };
    for line in BufReader::new(file).lines().map_while(Result::ok) {
        let Ok(entry) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        if entry.get("type").and_then(Value::as_str) != Some("assistant")
            || entry.get("isSidechain").and_then(Value::as_bool) == Some(true)
        {
            continue;
        }
        let Some(message) = entry.get("message") else {
            continue;
        };
        let Some(usage) = message.get("usage") else {
            continue;
        };
        let identity = message
            .get("id")
            .and_then(Value::as_str)
            .map(str::to_string)
            .unwrap_or_else(|| serde_json::to_string(usage).unwrap_or_default());
        if seen.contains(&identity) {
            continue;
        }
        let cache_read = get_i64(usage, &["cache_read_input_tokens"]).unwrap_or(0);
        let cache_write = get_i64(usage, &["cache_creation_input_tokens"]).unwrap_or(0);
        let input_tokens =
            get_i64(usage, &["input_tokens"]).unwrap_or(0) + cache_read + cache_write;
        let output_tokens = get_i64(usage, &["output_tokens"]).unwrap_or(0);
        if input_tokens <= 0 && output_tokens <= 0 {
            // 同一 message id 可能先以零 usage 的占位行出现、稍后才补写
            // 真实计数：占位行不占用去重名额，避免遮蔽后续真实数据。
            continue;
        }
        seen.insert(identity.clone());
        records.push(TranscriptUsageRecord {
            record_id: usage_record_id("claude", session_id, "message", &identity),
            occurred_at: get_str(&entry, &["timestamp", "occurred_at", "time"]).map(str::to_string),
            model: get_str(message, &["model", "model_name"]).map(str::to_string),
            input_tokens,
            output_tokens,
            cached_tokens: cache_read,
        });
    }
    records.extend(read_claude_sidechain_usage_records(session_id, path));
    records
}

/// Claude Code 把子代理（Task 工具）的完整转录写在
/// `<project>/<父会话id>/subagents/agent-*.jsonl`，其中 assistant 记录带
/// `isSidechain: true` 与父会话 `sessionId`。主会话文件不含这些行——
/// 按 sidechain 过滤的读取器会把子代理的真实 API 用量整份丢掉。
/// 这里按 sessionId 把子代理文件的用量归属回父会话，与主会话明细
/// 一起入账。
pub(super) fn read_claude_sidechain_usage_records(
    session_id: &str,
    main_path: &Path,
) -> Vec<TranscriptUsageRecord> {
    let Some(directory) = main_path.parent() else {
        return Vec::new();
    };
    // 会话级 subagents 树可能还有 workflows/ 等更深一层，递归收集。
    let mut agent_files = Vec::new();
    collect_claude_agent_transcripts(
        &directory.join(session_id).join("subagents"),
        &mut agent_files,
    );
    for candidate in [directory.join("subagents"), directory.to_path_buf()] {
        let Ok(entries) = std::fs::read_dir(&candidate) else {
            continue;
        };
        for sibling in entries.flatten() {
            let path = sibling.path();
            if is_claude_agent_transcript(&path) {
                agent_files.push(path);
            }
        }
    }
    let mut records = Vec::new();
    let mut seen = HashSet::new();
    for path in agent_files {
        let Ok(file) = std::fs::File::open(&path) else {
            continue;
        };
        for line in BufReader::new(file).lines().map_while(Result::ok) {
            let Ok(entry) = serde_json::from_str::<Value>(&line) else {
                continue;
            };
            if entry.get("type").and_then(Value::as_str) != Some("assistant")
                || entry.get("sessionId").and_then(Value::as_str) != Some(session_id)
            {
                continue;
            }
            let Some(message) = entry.get("message") else {
                continue;
            };
            let Some(usage) = message.get("usage") else {
                continue;
            };
            let identity = message
                .get("id")
                .and_then(Value::as_str)
                .map(str::to_string)
                .unwrap_or_else(|| serde_json::to_string(usage).unwrap_or_default());
            if seen.contains(&identity) {
                continue;
            }
            let cache_read = get_i64(usage, &["cache_read_input_tokens"]).unwrap_or(0);
            let cache_write = get_i64(usage, &["cache_creation_input_tokens"]).unwrap_or(0);
            let input_tokens =
                get_i64(usage, &["input_tokens"]).unwrap_or(0) + cache_read + cache_write;
            let output_tokens = get_i64(usage, &["output_tokens"]).unwrap_or(0);
            if input_tokens <= 0 && output_tokens <= 0 {
                continue;
            }
            seen.insert(identity.clone());
            records.push(TranscriptUsageRecord {
                record_id: usage_record_id("claude", session_id, "sidechain", &identity),
                occurred_at: get_str(&entry, &["timestamp", "occurred_at", "time"])
                    .map(str::to_string),
                model: get_str(message, &["model", "model_name"]).map(str::to_string),
                input_tokens,
                output_tokens,
                cached_tokens: cache_read,
            });
        }
    }
    records
}

pub(super) fn is_claude_agent_transcript(path: &Path) -> bool {
    path.extension().and_then(|extension| extension.to_str()) == Some("jsonl")
        && path
            .file_name()
            .and_then(|name| name.to_str())
            .map(|name| name.starts_with("agent-"))
            .unwrap_or(false)
}

/// 递归收集会话 subagents 树（含 workflows/ 子目录）里的 agent 转录；
/// 数量封顶防止异常目录拖垮刷新。
pub(super) fn collect_claude_agent_transcripts(directory: &Path, found: &mut Vec<PathBuf>) {
    if found.len() >= 500 {
        return;
    }
    let Ok(entries) = std::fs::read_dir(directory) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_claude_agent_transcripts(&path, found);
        } else if is_claude_agent_transcript(&path) {
            found.push(path);
        }
    }
}

pub(super) fn read_workbuddy_usage_records(
    session_id: &str,
    path: &Path,
) -> Vec<TranscriptUsageRecord> {
    let Ok(file) = std::fs::File::open(path) else {
        return Vec::new();
    };
    let mut seen = HashSet::new();
    BufReader::new(file)
        .lines()
        .map_while(Result::ok)
        .filter_map(|line| {
            let entry = serde_json::from_str::<Value>(&line).ok()?;
            let usage = workbuddy_usage(&entry)?;
            let identity = entry
                .get("id")
                .and_then(Value::as_str)
                .map(str::to_string)
                .unwrap_or_else(|| line.clone());
            if seen.contains(&identity) {
                return None;
            }
            if usage.input_tokens <= 0 && usage.output_tokens <= 0 {
                // 零 usage 的占位行不占用去重名额，避免遮蔽同一 id
                // 稍后补写的真实计数。
                return None;
            }
            seen.insert(identity.clone());
            Some(TranscriptUsageRecord {
                record_id: usage_record_id("workbuddy", session_id, "record", &identity),
                occurred_at: get_str(&entry, &["timestamp", "occurred_at", "time"])
                    .map(str::to_string),
                model: get_str(&entry, &["model", "model_name"])
                    .or_else(|| {
                        entry
                            .get("providerData")
                            .and_then(|provider| get_str(provider, &["requestModelName", "model"]))
                    })
                    .map(str::to_string),
                input_tokens: usage.input_tokens,
                output_tokens: usage.output_tokens,
                cached_tokens: usage.cached_tokens,
            })
        })
        .collect()
}

/// 口径与 `read_qoder_transcript_estimate` 一致：每条 user 条目触发一次
/// 请求，input = 此前累积的全部上下文 + 本条文本；assistant 条目的
/// output = 本条文本。逐条明细与会话总量来自同一次顺序重放，永远一致。
pub(super) fn read_qoder_usage_records(
    session_id: &str,
    path: &Path,
) -> Vec<TranscriptUsageRecord> {
    let Ok(file) = std::fs::File::open(path) else {
        return Vec::new();
    };
    let mut seen = HashSet::new();
    let mut records = Vec::new();
    let mut context_tokens: i64 = 0;
    for line in BufReader::new(file).lines().map_while(Result::ok) {
        let Ok(entry) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        let Some(role) = entry
            .get("type")
            .and_then(Value::as_str)
            .filter(|role| matches!(*role, "user" | "assistant"))
        else {
            continue;
        };
        let identity = entry
            .get("uuid")
            .and_then(Value::as_str)
            .map(str::to_string)
            .unwrap_or_else(|| line.clone());
        if !seen.insert(identity.clone()) {
            continue;
        }
        let tokens = estimate_transcript_tokens(&transcript_value_text(
            entry
                .get("message")
                .and_then(|message| message.get("content")),
        ));
        if tokens <= 0 {
            continue;
        }
        let is_user = role == "user";
        let input_tokens = if is_user { context_tokens + tokens } else { 0 };
        context_tokens += tokens;
        records.push(TranscriptUsageRecord {
            record_id: usage_record_id("qoder", session_id, "estimated_message", &identity),
            occurred_at: get_str(&entry, &["timestamp", "occurred_at", "time"]).map(str::to_string),
            model: None,
            input_tokens,
            output_tokens: if is_user { 0 } else { tokens },
            cached_tokens: 0,
        });
    }
    records
}

/// MiniMax Code 把每个模型响应写在 `~/.minimax/v2/sessions/**/messages.jsonl`
/// 的 assistant 记录里，`message.usage` 形如
/// `{input, output, cacheRead, cacheWrite, totalTokens}`。`input` 不含缓存，
/// 缓存读取/写入仍是真实输入上下文，与 Claude 口径一样并入输入总量、缓存
/// 命中单独作为明细。去重边界是响应级 `responseId`（缺失时退回 message_id）。
pub(super) fn read_minimax_transcript_usage(path: &Path) -> Option<TranscriptUsage> {
    let file = std::fs::File::open(path).ok()?;
    let mut response_ids = HashSet::new();
    let mut total = TranscriptUsage {
        input_tokens: 0,
        output_tokens: 0,
        cached_tokens: 0,
    };
    for line in BufReader::new(file).lines().map_while(Result::ok) {
        let Ok(entry) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        let Some(message) = entry.get("message") else {
            continue;
        };
        if message.get("role").and_then(Value::as_str) != Some("assistant") {
            continue;
        }
        let Some(usage) = message.get("usage") else {
            continue;
        };
        let response_id = get_str(message, &["responseId", "response_id", "id"])
            .map(str::to_string)
            .or_else(|| {
                entry
                    .get("message_id")
                    .and_then(Value::as_str)
                    .map(str::to_string)
            })
            .unwrap_or_else(|| serde_json::to_string(usage).unwrap_or_default());
        if !response_ids.insert(response_id) {
            continue;
        }
        let cache_read = get_i64(usage, &["cacheRead", "cache_read"]).unwrap_or(0);
        let cache_write = get_i64(usage, &["cacheWrite", "cache_write"]).unwrap_or(0);
        total.input_tokens +=
            get_i64(usage, &["input", "input_tokens"]).unwrap_or(0) + cache_read + cache_write;
        total.output_tokens += get_i64(usage, &["output", "output_tokens"]).unwrap_or(0);
        total.cached_tokens += cache_read;
    }
    (total.input_tokens > 0 || total.output_tokens > 0).then_some(total)
}

/// Kimi（Kimi Code / Kimi Work）的会话转录是
/// `<kimi-code home>/sessions/<workspace>/<conv>/agents/main/wire.jsonl`。
/// 每个 `usage.record` 事件携带一轮（turn）的
/// `{inputOther, output, inputCacheRead, inputCacheCreation}`；与 Claude 口径
/// 一致，缓存读取/创建并入输入总量，缓存命中单列。事件没有稳定 id，按
/// (时间, 各计数) 元组去重，避免文件重写时重复累计。
pub(super) fn read_kimi_transcript_usage(path: &Path) -> Option<TranscriptUsage> {
    let file = std::fs::File::open(path).ok()?;
    let mut seen = HashSet::new();
    let mut total = TranscriptUsage {
        input_tokens: 0,
        output_tokens: 0,
        cached_tokens: 0,
    };
    for line in BufReader::new(file).lines().map_while(Result::ok) {
        let Ok(entry) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        if entry.get("type").and_then(Value::as_str) != Some("usage.record") {
            continue;
        }
        let Some(usage) = entry.get("usage") else {
            continue;
        };
        let input_other = get_i64(usage, &["inputOther", "input_other"]).unwrap_or(0);
        let output = get_i64(usage, &["output", "output_tokens"]).unwrap_or(0);
        let cache_read = get_i64(usage, &["inputCacheRead", "input_cache_read"]).unwrap_or(0);
        let cache_write =
            get_i64(usage, &["inputCacheCreation", "input_cache_creation"]).unwrap_or(0);
        let identity = format!(
            "{}:{}:{}:{}:{}",
            get_str(&entry, &["time", "timestamp", "occurred_at"]).unwrap_or(""),
            input_other,
            output,
            cache_read,
            cache_write
        );
        if !seen.insert(identity) {
            continue;
        }
        total.input_tokens += input_other + cache_read + cache_write;
        total.output_tokens += output;
        total.cached_tokens += cache_read;
    }
    (total.input_tokens > 0 || total.output_tokens > 0).then_some(total)
}

pub(super) fn read_minimax_usage_records(
    session_id: &str,
    path: &Path,
) -> Vec<TranscriptUsageRecord> {
    let Ok(file) = std::fs::File::open(path) else {
        return Vec::new();
    };
    let mut seen = HashSet::new();
    BufReader::new(file)
        .lines()
        .map_while(Result::ok)
        .filter_map(|line| {
            let entry = serde_json::from_str::<Value>(&line).ok()?;
            let message = entry.get("message")?;
            if message.get("role").and_then(Value::as_str) != Some("assistant") {
                return None;
            }
            let usage = message.get("usage")?;
            let identity = get_str(message, &["responseId", "response_id", "id"])
                .map(str::to_string)
                .or_else(|| {
                    entry
                        .get("message_id")
                        .and_then(Value::as_str)
                        .map(str::to_string)
                })
                .unwrap_or_else(|| serde_json::to_string(usage).unwrap_or_default());
            if seen.contains(&identity) {
                return None;
            }
            let cache_read = get_i64(usage, &["cacheRead", "cache_read"]).unwrap_or(0);
            let cache_write = get_i64(usage, &["cacheWrite", "cache_write"]).unwrap_or(0);
            let input_tokens =
                get_i64(usage, &["input", "input_tokens"]).unwrap_or(0) + cache_read + cache_write;
            let output_tokens = get_i64(usage, &["output", "output_tokens"]).unwrap_or(0);
            if input_tokens <= 0 && output_tokens <= 0 {
                // 零 usage 的占位行不占用去重名额，避免遮蔽同一 id
                // 稍后补写的真实计数。
                return None;
            }
            seen.insert(identity.clone());
            Some(TranscriptUsageRecord {
                record_id: usage_record_id("minimax", session_id, "message", &identity),
                occurred_at: message
                    .get("timestamp")
                    .and_then(Value::as_i64)
                    .and_then(|millis| {
                        chrono::DateTime::<chrono::Utc>::from_timestamp_millis(millis)
                    })
                    .map(|time| time.to_rfc3339())
                    .or_else(|| {
                        get_str(&entry, &["timestamp", "occurred_at", "time"]).map(str::to_string)
                    }),
                model: get_str(message, &["model", "model_name"]).map(str::to_string),
                input_tokens,
                output_tokens,
                cached_tokens: cache_read.min(input_tokens),
            })
        })
        .collect()
}

pub(super) fn read_kimi_usage_records(session_id: &str, path: &Path) -> Vec<TranscriptUsageRecord> {
    let Ok(file) = std::fs::File::open(path) else {
        return Vec::new();
    };
    let mut seen = HashSet::new();
    BufReader::new(file)
        .lines()
        .map_while(Result::ok)
        .filter_map(|line| {
            let entry = serde_json::from_str::<Value>(&line).ok()?;
            if entry.get("type").and_then(Value::as_str) != Some("usage.record") {
                return None;
            }
            let usage = entry.get("usage")?;
            let input_other = get_i64(usage, &["inputOther", "input_other"]).unwrap_or(0);
            let output_tokens = get_i64(usage, &["output", "output_tokens"]).unwrap_or(0);
            let cache_read = get_i64(usage, &["inputCacheRead", "input_cache_read"]).unwrap_or(0);
            let cache_write =
                get_i64(usage, &["inputCacheCreation", "input_cache_creation"]).unwrap_or(0);
            let occurred_at = entry
                .get("time")
                .and_then(Value::as_i64)
                .and_then(chrono::DateTime::<chrono::Utc>::from_timestamp_millis)
                .map(|time| time.to_rfc3339());
            let identity = format!(
                "{}:{}:{}:{}:{}",
                occurred_at.as_deref().unwrap_or(""),
                input_other,
                output_tokens,
                cache_read,
                cache_write
            );
            if !seen.insert(identity.clone()) {
                return None;
            }
            let input_tokens = input_other + cache_read + cache_write;
            ((input_tokens > 0 || output_tokens > 0) as bool).then_some(TranscriptUsageRecord {
                record_id: usage_record_id("kimi", session_id, "usage_record", &identity),
                occurred_at,
                model: get_str(&entry, &["model", "model_name"]).map(str::to_string),
                input_tokens,
                output_tokens,
                cached_tokens: cache_read.min(input_tokens),
            })
        })
        .collect()
}

/// DeepSeek Harness v4 事件流：`model/selection` 携带当前模型（如
/// `zhipu/glm-5.3`），`assistant/message` 的 `data.usage` 是驼峰计数——
/// `inputTokens` 不含缓存，`cacheReadTokens` 单列。按 Claude 口径把缓存
/// 读取并入输入，缓存命中单列；事件 `seq` 单调且稳定，作为幂等标识。
/// 撕裂的末帧只贡献完整解出的行（map_while 在解压错误处停止）。
pub(super) fn read_dsh_usage_records(session_id: &str, path: &Path) -> Vec<TranscriptUsageRecord> {
    let Ok(reader) = crate::dsh_transcript::open_lines(path) else {
        return Vec::new();
    };
    let mut seen = HashSet::new();
    let mut current_model: Option<String> = None;
    let mut records = Vec::new();
    for line in reader.lines().map_while(Result::ok) {
        let Ok(entry) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        let data = entry.get("data");
        match entry.get("type").and_then(Value::as_str) {
            Some("model/selection") => {
                if let Some(model) =
                    data.and_then(|data| data.get("model")).and_then(Value::as_str)
                {
                    current_model = Some(model.to_string());
                }
            }
            Some("assistant/message") => {
                let Some(usage) = data.and_then(|data| data.get("usage")) else {
                    continue;
                };
                let input = get_i64(usage, &["inputTokens", "input_tokens"]).unwrap_or(0);
                let output = get_i64(usage, &["outputTokens", "output_tokens"]).unwrap_or(0);
                let cache_read = get_i64(usage, &["cacheReadTokens", "cache_read_tokens"]).unwrap_or(0);
                let input_tokens = input + cache_read;
                if input_tokens <= 0 && output <= 0 {
                    continue;
                }
                // seq 是文件内单调序号；缺失时退回 (turn, step) 元组。
                let identity = entry
                    .get("seq")
                    .and_then(Value::as_i64)
                    .map(|seq| seq.to_string())
                    .unwrap_or_else(|| {
                        format!(
                            "{}:{}",
                            data.and_then(|data| data.get("turn")).and_then(Value::as_i64).unwrap_or(0),
                            data.and_then(|data| data.get("step")).and_then(Value::as_i64).unwrap_or(0),
                        )
                    });
                if !seen.insert(identity.clone()) {
                    continue;
                }
                let occurred_at = entry
                    .get("time")
                    .and_then(Value::as_i64)
                    .and_then(chrono::DateTime::<chrono::Utc>::from_timestamp_millis)
                    .map(|time| time.to_rfc3339());
                records.push(TranscriptUsageRecord {
                    record_id: usage_record_id("dsh", session_id, "message", &identity),
                    occurred_at,
                    model: current_model.clone(),
                    input_tokens,
                    output_tokens: output,
                    cached_tokens: cache_read.min(input_tokens),
                });
            }
            _ => {}
        }
    }
    records
}

/// DSH 的会话级兜底总量：明细读取器产出为空时的降级路径，直接累计
/// `assistant/message` 的 usage（同样按 seq 去重、缓存并入输入）。
pub(super) fn read_dsh_transcript_usage(path: &Path) -> Option<TranscriptUsage> {
    let reader = crate::dsh_transcript::open_lines(path).ok()?;
    let mut seen = HashSet::new();
    let mut total = TranscriptUsage {
        input_tokens: 0,
        output_tokens: 0,
        cached_tokens: 0,
    };
    for line in reader.lines().map_while(Result::ok) {
        let Ok(entry) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        if entry.get("type").and_then(Value::as_str) != Some("assistant/message") {
            continue;
        }
        let Some(usage) = entry
            .get("data")
            .and_then(|data| data.get("usage"))
        else {
            continue;
        };
        let identity = entry
            .get("seq")
            .and_then(Value::as_i64)
            .map(|seq| seq.to_string())
            .unwrap_or_else(|| serde_json::to_string(usage).unwrap_or_default());
        if !seen.insert(identity) {
            continue;
        }
        let cache_read = get_i64(usage, &["cacheReadTokens", "cache_read_tokens"]).unwrap_or(0);
        total.input_tokens +=
            get_i64(usage, &["inputTokens", "input_tokens"]).unwrap_or(0) + cache_read;
        total.output_tokens += get_i64(usage, &["outputTokens", "output_tokens"]).unwrap_or(0);
        total.cached_tokens += cache_read;
    }
    (total.input_tokens > 0 || total.output_tokens > 0).then_some(total)
}

/// Codex emits `token_count` records containing a cumulative session total and
/// a per-response delta.  Only the greatest cumulative total is authoritative
/// for a session; summing every record was the old over-counting bug.
pub(super) fn read_codex_transcript_usage(path: &Path) -> Option<TranscriptUsage> {
    let file = std::fs::File::open(path).ok()?;
    let mut latest: Option<TranscriptUsage> = None;
    for line in BufReader::new(file).lines().map_while(Result::ok) {
        let Ok(entry) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        let Some(payload) = entry.get("payload") else {
            continue;
        };
        if payload.get("type").and_then(Value::as_str) != Some("token_count") {
            continue;
        }
        let candidate = payload
            .get("info")
            .and_then(|info| info.get("total_token_usage"))
            .and_then(codex_usage)?;
        if latest
            .map(|previous| {
                candidate.input_tokens + candidate.output_tokens
                    >= previous.input_tokens + previous.output_tokens
            })
            .unwrap_or(true)
        {
            latest = Some(candidate);
        }
    }
    latest
}

/// Claude writes one API usage object for each response, but represents tool
/// blocks of that response as duplicate JSONL rows. `message.id` is the stable
/// response id, so it is the only safe deduplication boundary.
pub(super) fn read_claude_transcript_usage(path: &Path) -> Option<TranscriptUsage> {
    let file = std::fs::File::open(path).ok()?;
    let mut response_ids = HashSet::new();
    let mut total = TranscriptUsage {
        input_tokens: 0,
        output_tokens: 0,
        cached_tokens: 0,
    };
    for line in BufReader::new(file).lines().map_while(Result::ok) {
        let Ok(entry) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        if entry.get("type").and_then(Value::as_str) != Some("assistant")
            || entry.get("isSidechain").and_then(Value::as_bool) == Some(true)
        {
            continue;
        }
        let Some(message) = entry.get("message") else {
            continue;
        };
        let Some(usage) = message.get("usage") else {
            continue;
        };
        let response_id = message
            .get("id")
            .and_then(Value::as_str)
            .map(str::to_string)
            .unwrap_or_else(|| serde_json::to_string(usage).unwrap_or_default());
        // Claude keeps cache reads outside input_tokens.  They still consume
        // context and therefore belong in the displayed input total, while
        // remaining visible separately as cache hits.
        let cache_read = get_i64(usage, &["cache_read_input_tokens"]).unwrap_or(0);
        let cache_write = get_i64(usage, &["cache_creation_input_tokens"]).unwrap_or(0);
        let input_tokens =
            get_i64(usage, &["input_tokens"]).unwrap_or(0) + cache_read + cache_write;
        let output_tokens = get_i64(usage, &["output_tokens"]).unwrap_or(0);
        if input_tokens <= 0 && output_tokens <= 0 {
            // 同一 message id 可能先以零 usage 占位、稍后补写真实计数：
            // 占位行不占用去重名额，避免遮蔽后续真实数据。
            continue;
        }
        if !response_ids.insert(response_id) {
            continue;
        }
        total.input_tokens += input_tokens;
        total.output_tokens += output_tokens;
        total.cached_tokens += cache_read;
    }
    (total.input_tokens > 0 || total.output_tokens > 0).then_some(total)
}

/// WorkBuddy keeps every model response in its native project JSONL. Usage is
/// nested under `message.usage` (snake_case, Chat Completions) or
/// `providerData.usage` (camelCase, Responses API), never at the top level, and
/// the record `id` is the deduplication boundary. Like Claude, `input_tokens`
/// excludes cache reads, so cache_read is merged into the displayed input total
/// while remaining a display-only subset.
pub(super) fn read_workbuddy_transcript_usage(path: &Path) -> Option<TranscriptUsage> {
    let file = std::fs::File::open(path).ok()?;
    let mut ids = HashSet::new();
    let mut total = TranscriptUsage {
        input_tokens: 0,
        output_tokens: 0,
        cached_tokens: 0,
    };
    for line in BufReader::new(file).lines().map_while(Result::ok) {
        let Ok(entry) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        let Some(usage) = workbuddy_usage(&entry) else {
            continue;
        };
        let record_id = entry
            .get("id")
            .and_then(Value::as_str)
            .map(str::to_string)
            .unwrap_or_else(|| line.clone());
        if !ids.insert(record_id) {
            continue;
        }
        total.input_tokens += usage.input_tokens;
        total.output_tokens += usage.output_tokens;
        total.cached_tokens += usage.cached_tokens;
    }
    (total.input_tokens > 0 || total.output_tokens > 0).then_some(total)
}

/// Qoder's current local transcript format has no provider usage field. This
/// deterministic fallback gives users complete activity coverage while keeping
/// provenance explicit (`estimated_transcript`) in the UI and SQLite ledger.
///
/// 口径与原生计数对齐：真实 API 每轮请求的输入是此前累积的全部上下文
/// （历史消息 + 工具结果）加本条消息，而不是每条消息自身的文本量。
pub(super) fn read_qoder_transcript_estimate(path: &Path) -> Option<TranscriptUsage> {
    let file = std::fs::File::open(path).ok()?;
    let mut message_ids = HashSet::new();
    let mut context_tokens: i64 = 0;
    let mut total = TranscriptUsage {
        input_tokens: 0,
        output_tokens: 0,
        cached_tokens: 0,
    };
    for line in BufReader::new(file).lines().map_while(Result::ok) {
        let Ok(entry) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        if !matches!(
            entry.get("type").and_then(Value::as_str),
            Some("user" | "assistant")
        ) {
            continue;
        }
        let id = entry
            .get("uuid")
            .and_then(Value::as_str)
            .map(str::to_string)
            .unwrap_or_else(|| line.clone());
        if !message_ids.insert(id) {
            continue;
        }
        let text = transcript_value_text(
            entry
                .get("message")
                .and_then(|message| message.get("content")),
        );
        let tokens = estimate_transcript_tokens(&text);
        if entry.get("type").and_then(Value::as_str) == Some("assistant") {
            total.output_tokens += tokens;
        } else {
            // 每条 user 条目（含工具结果）都对应一次请求，输入即全部上下文。
            total.input_tokens += context_tokens + tokens;
        }
        context_tokens += tokens;
    }
    (total.input_tokens > 0 || total.output_tokens > 0).then_some(total)
}

pub(super) fn transcript_value_text(value: Option<&Value>) -> String {
    match value {
        Some(Value::String(text)) => text.to_string(),
        Some(Value::Array(items)) => items
            .iter()
            .map(|item| transcript_value_text(Some(item)))
            .collect::<Vec<_>>()
            .join("\n"),
        Some(Value::Object(object)) => {
            // Claude 风格 content block：text 块读 `text`，思考块读
            // `thinking`（内容不在 `text` 键下，漏计会让输出严重低估），
            // tool_result 读 `content`；tool_use 的工具名与参数也是模型
            // 生成的输出 token，按 JSON 序列化计入。
            if let Some(text) = object
                .get("text")
                .or_else(|| object.get("thinking"))
                .and_then(Value::as_str)
            {
                return text.to_string();
            }
            if let Some(input) = object.get("input") {
                let name = object.get("name").and_then(Value::as_str).unwrap_or("");
                return format!("{name}{}", serde_json::to_string(input).unwrap_or_default());
            }
            object
                .get("content")
                .map(|item| transcript_value_text(Some(item)))
                .unwrap_or_default()
        }
        _ => String::new(),
    }
}

/// Tokenise locally with OpenAI's public `cl100k_base` vocabulary. This is an
/// exact tokenizer for compatible models and a stable, far better fallback
/// than character-counting for other providers; provider-native counters
/// always take precedence elsewhere in the ledger.
pub(super) fn estimate_transcript_tokens(text: &str) -> i64 {
    // Building the BPE ranks is expensive (~100ms); transcripts hold thousands
    // of messages, so the encoding must be constructed once and reused.
    static ENCODING: std::sync::OnceLock<Option<tiktoken_rs::CoreBPE>> = std::sync::OnceLock::new();
    let encoding = ENCODING.get_or_init(|| tiktoken_rs::cl100k_base().ok());
    if let Some(encoding) = encoding {
        return encoding.encode_with_special_tokens(text).len() as i64;
    }
    // The bundled vocabulary should always load. Retain a deterministic
    // fallback so a damaged local install never drops a session from metrics.
    let mut tokens = 0i64;
    let mut ascii_run = 0i64;
    let flush_ascii = |run: &mut i64, total: &mut i64| {
        if *run > 0 {
            *total += (*run + 3) / 4;
            *run = 0;
        }
    };
    for character in text.chars() {
        if character.is_ascii_alphanumeric() {
            ascii_run += 1;
        } else {
            flush_ascii(&mut ascii_run, &mut tokens);
            if !character.is_whitespace() {
                tokens += 1;
            }
        }
    }
    flush_ascii(&mut ascii_run, &mut tokens);
    tokens
}

/// 与转录估算同一「每请求全上下文」口径：遇到 [用户] 块即一次请求，
/// 输入为累积上下文加本块；[助手] 块只计入输出。两者都追加进上下文。
pub(super) fn estimate_ledger_conversation_usage(conversation: &str) -> Option<TranscriptUsage> {
    let mut total = TranscriptUsage {
        input_tokens: 0,
        output_tokens: 0,
        cached_tokens: 0,
    };
    let mut context_tokens: i64 = 0;
    for block in conversation.split("\n\n") {
        if let Some(text) = block.strip_prefix("[用户]\n") {
            let tokens = estimate_transcript_tokens(text);
            total.input_tokens += context_tokens + tokens;
            context_tokens += tokens;
        } else if let Some(text) = block.strip_prefix("[助手]\n") {
            let tokens = estimate_transcript_tokens(text);
            total.output_tokens += tokens;
            context_tokens += tokens;
        }
    }
    (total.input_tokens > 0 || total.output_tokens > 0).then_some(total)
}

pub(super) fn get_i64(payload: &Value, keys: &[&str]) -> Option<i64> {
    keys.iter()
        .find_map(|key| payload.get(*key).and_then(Value::as_i64))
}

pub(crate) fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

pub(super) fn get_str<'a>(payload: &'a Value, keys: &[&str]) -> Option<&'a str> {
    keys.iter()
        .find_map(|key| payload.get(*key).and_then(Value::as_str))
}
