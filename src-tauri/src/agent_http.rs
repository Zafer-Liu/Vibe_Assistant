//! Local memory context, capture hooks and usage telemetry API.
//! The legacy module name is retained for installed hook URLs; task dispatch is removed.

use serde_json::{json, Value};
use std::sync::Arc;

// ── HTTP server 固定配置 ─────────────────────────────────────────────────────

/// Agent HTTP server 固定监听端口（记忆沉淀、遥测、agent 回调等共用）
pub const AGENT_HTTP_PORT: u16 = 9420;

// The server no longer owns agent task or execution state.
#[derive(Default)]
pub struct AgentHttpStore;
impl AgentHttpStore {
    pub fn new() -> Self { Self }
}
pub fn init_store() -> Arc<AgentHttpStore> { Arc::new(AgentHttpStore::new()) }

// ── HTTP Server（极简手写，不引入框架）──────────────────────────────────────

/// 启动 HTTP server。在 Tauri setup 里 tokio::spawn 调用。
pub async fn start_agent_http_server(port: u16, store: Arc<AgentHttpStore>) {
    let addr = format!("127.0.0.1:{}", port);
    // A development restart or a previous app instance can briefly keep the
    // port occupied.  Endpoints must recover once that process exits instead of
    // silently remaining offline for the lifetime of this desktop instance.
    let listener = loop {
        match tokio::net::TcpListener::bind(&addr).await {
            Ok(listener) => {
                eprintln!("[agent-http] listening on http://{}", addr);
                break listener;
            }
            Err(error) => {
                eprintln!("[agent-http] failed to bind {addr}: {error}; retrying in 5 seconds");
                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
            }
        }
    };

    loop {
        match listener.accept().await {
            Ok((stream, _)) => {
                let store = Arc::clone(&store);
                tokio::spawn(handle_connection(stream, store));
            }
            Err(e) => {
                eprintln!("[agent-http] accept error: {}", e);
            }
        }
    }
}

async fn handle_connection(mut stream: tokio::net::TcpStream, store: Arc<AgentHttpStore>) {
    use tokio::io::AsyncWriteExt;

    // Read the complete HTTP body.  Payloads often include tool output and
    // exceed a single TCP frame; the old 8 KiB one-shot read silently truncated
    // such events and turned them into ignored JSON.
    const MAX_REQUEST_BYTES: usize = 1024 * 1024;
    let raw = match read_http_request(&mut stream, MAX_REQUEST_BYTES).await {
        Ok(raw) => raw,
        Err(message) => {
            let status = if message.contains("too large") {
                "413 Payload Too Large"
            } else {
                "400 Bad Request"
            };
            let response = json!({"error": message}).to_string();
            let http = format!(
                "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
                response.len(), response
            );
            let _ = stream.write_all(http.as_bytes()).await;
            return;
        }
    };

    // 解析 HTTP 请求行
    let first_line = raw.lines().next().unwrap_or("");
    let parts: Vec<&str> = first_line.split_whitespace().collect();
    if parts.len() < 2 {
        let _ = stream.write_all(b"HTTP/1.1 400 Bad Request\r\n\r\n").await;
        return;
    }
    let method = parts[0];
    let path = parts[1];

    // 提取 body（\r\n\r\n 之后）
    let body = if let Some(idx) = raw.find("\r\n\r\n") {
        &raw[idx + 4..]
    } else {
        ""
    };

    // 路由
    let (status, response, hook_audit) = if matches!(method, "GET" | "POST")
        && path.starts_with("/memory/context")
    {
        prepare_hook_context(path, body)
    } else {
        let (status, response) = route(method, path, body, &store).await;
        (status, response, None)
    };

    let http_response = format!(
        "HTTP/1.1 {}\r\nContent-Type: application/json\r\nAccess-Control-Allow-Origin: *\r\nAccess-Control-Allow-Methods: GET, POST, OPTIONS\r\nAccess-Control-Allow-Headers: Content-Type, Authorization\r\nContent-Length: {}\r\n\r\n{}",
        status,
        response.len(),
        response
    );

    if stream.write_all(http_response.as_bytes()).await.is_ok() && stream.flush().await.is_ok() {
        if let Some(audit) = hook_audit {
            if let Some(ledger) = crate::telemetry_store::shared_store() {
                let _ = ledger.try_record_hook_injection(
                    crate::agent_sources::agent_label(&audit.source),
                    audit.tool_name,
                    &audit.summary,
                    audit.detail.as_deref(),
                );
            }
            if audit.commit_gate {
                crate::memory_mcp::hook_commit_prompt_fingerprint(
                    &audit.source, audit.session_id.as_deref(), &audit.fingerprint,
                );
            }
        }
    }
}

struct HookAudit {
    source: String,
    session_id: Option<String>,
    tool_name: &'static str,
    summary: String,
    detail: Option<String>,
    fingerprint: String,
    commit_gate: bool,
}

fn prepare_hook_context(path: &str, body: &str) -> (&'static str, String, Option<HookAudit>) {
    let query = path.split_once('?').map(|(_, query)| query).unwrap_or("");
    let source = query.split('&').find_map(|part| part.strip_prefix("source="))
        // dsh 的注入由 cordis pre-step 插件发起（记忆注入插件），采集走
        // 本地转录扫描而非 Hook payload。
        .filter(|source| matches!(*source, "claude" | "qoder" | "codex" | "workbuddy" | "dsh"))
        .unwrap_or("unknown");
    let event = query.split('&').find_map(|part| part.strip_prefix("event="))
        .filter(|event| matches!(*event, "SessionStart" | "UserPromptSubmit"))
        .unwrap_or("SessionStart");
    let payload = serde_json::from_str::<Value>(body).ok();
    let session_id = payload.as_ref().and_then(|value| value.get("session_id"))
        .and_then(Value::as_str).filter(|id| !id.is_empty() && id.len() <= 256)
        .map(str::to_owned);
    // dsh 的对话沉淀由转录扫描负责，不把插件请求当 Hook 采集事件入账。
    if event == "UserPromptSubmit" && payload.is_some() && source != "dsh" {
        if let (Some(ingest), Some(backend)) = (
            crate::memory_ingest::ingest_store(), crate::memory_backend::shared_backend(),
        ) {
            let _ = ingest.handle_hook(backend, body, source);
        }
    }
    if event == "UserPromptSubmit" {
        let injection = crate::memory_mcp::hook_prompt_context_gated(source, session_id.as_deref());
        let (response, summary, detail, commit_gate) = match injection.context {
            Some(context) => {
                let summary = format!("每轮提问注入 L3 长期记忆（{} 字符，指纹 {}）",
                    context.chars().count(), injection.fingerprint);
                (hook_inject_body(source, event, context.clone()), summary, Some(context), true)
            }
            None => (String::new(), format!("跳过未变化的 L3 注入（指纹 {}）", injection.fingerprint), None, false),
        };
        return ("200 OK", response, Some(HookAudit {
            source: source.into(), session_id, tool_name: "prompt_inject", summary,
            detail, fingerprint: injection.fingerprint, commit_gate,
        }));
    }
    let (context, fingerprint) = crate::memory_mcp::hook_session_start_context(source);
    let summary = format!("会话启动注入 L3 长期记忆 + L2 工作记忆（{} 字符，指纹 {}）",
        context.chars().count(), crate::memory_mcp::context_fingerprint(&context));
    // 智管在线时顺带刷新 dsh 注入插件的离线快照，保持回退内容与实时一致。
    if source == "dsh" && !context.trim().is_empty() {
        let _ = crate::dsh_injection::refresh_snapshot(&context);
    }
    let response = hook_inject_body(source, event, context.clone());
    ("200 OK", response, Some(HookAudit {
        source: source.into(), session_id, tool_name: "session_start_inject", summary,
        detail: Some(context), fingerprint, commit_gate: true,
    }))
}

/// Read one HTTP/1.1 request without assuming that headers and body arrive in
/// one `read`.  The hook endpoint only needs JSON bodies, so a bounded
/// Content-Length parser is sufficient and keeps this lightweight server safe.
async fn read_http_request(
    stream: &mut tokio::net::TcpStream,
    max_request_bytes: usize,
) -> Result<String, String> {
    use tokio::io::AsyncReadExt;

    let mut bytes = Vec::with_capacity(4096);
    let mut chunk = [0u8; 4096];
    let header_end;
    loop {
        let count = stream.read(&mut chunk).await.map_err(|e| e.to_string())?;
        if count == 0 {
            return Err("connection closed before HTTP headers".into());
        }
        bytes.extend_from_slice(&chunk[..count]);
        if bytes.len() > max_request_bytes {
            return Err("request too large".into());
        }
        if let Some(pos) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
            header_end = pos + 4;
            break;
        }
    }

    let headers =
        std::str::from_utf8(&bytes[..header_end]).map_err(|_| "HTTP headers are not UTF-8")?;
    let content_length = headers
        .lines()
        .find_map(|line| {
            line.split_once(':')
                .filter(|(name, _)| name.eq_ignore_ascii_case("content-length"))
        })
        .map(|(_, value)| {
            value
                .trim()
                .parse::<usize>()
                .map_err(|_| "invalid Content-Length".to_string())
        })
        .transpose()?
        .unwrap_or(0);
    if header_end.saturating_add(content_length) > max_request_bytes {
        return Err("request too large".into());
    }

    let required = header_end + content_length;
    while bytes.len() < required {
        let count = stream.read(&mut chunk).await.map_err(|e| e.to_string())?;
        if count == 0 {
            return Err("connection closed before HTTP body".into());
        }
        bytes.extend_from_slice(&chunk[..count]);
        if bytes.len() > max_request_bytes {
            return Err("request too large".into());
        }
    }
    String::from_utf8(bytes[..required].to_vec()).map_err(|_| "HTTP request is not UTF-8".into())
}

async fn route(
    method: &str,
    path: &str,
    body: &str,
    _store: &Arc<AgentHttpStore>,
) -> (&'static str, String) {
    // CORS preflight
    if method == "OPTIONS" {
        return ("204 No Content", String::new());
    }

    match (method, path) {
        ("GET", "/health") => (
            "200 OK",
            json!({"status": "ok", "service": "agent-manager-http"}).to_string(),
        ),

        // ── 记忆沉淀 Agent Hook 回调 ──────────────────────────────────────
        ("POST", p) if p == "/memory/hook" || p.starts_with("/memory/hook") => {
            // Agent hook 回调：自动沉淀记忆与 skill（异步处理，不阻塞）
            let source = p
                .split_once('?')
                .and_then(|(_, query)| {
                    query
                        .split('&')
                        .find_map(|part| part.strip_prefix("source="))
                })
                .filter(|source| matches!(*source, "claude" | "qoder" | "codex" | "workbuddy"))
                .unwrap_or("unknown");
            if let Some(ingest) = crate::memory_ingest::ingest_store() {
                if let Some(backend) = crate::memory_backend::shared_backend() {
                    return match ingest.handle_hook(backend, body, source) {
                        Ok(resp) => ("200 OK", resp.to_string()),
                        Err(error) => ("400 Bad Request", json!({"error": error}).to_string()),
                    };
                }
            }
            (
                "200 OK",
                json!({"status": "ok", "note": "ingest not ready"}).to_string(),
            )
        }

        // Normalized telemetry endpoint for adapters that do not implement the
        // Claude/Qoder Hook payload.  Any supported Agent adapter can submit
        // a final `session_usage` record here without being coerced into a
        // conversation-memory event.  Native transcript import remains an
        // optional enrichment for the four locally integrated Agents.
        ("POST", p) if p.starts_with("/telemetry/events/") => {
            let source = p
                .trim_start_matches("/telemetry/events/")
                .split('?')
                .next()
                .unwrap_or("");
            if !matches!(
                source,
                "codex"
                    | "workbuddy"
                    | "claude"
                    | "qoder"
                    | "gemini"
                    | "opencode"
                    | "openclaw"
                    | "pi"
                    | "grokbuild"
            ) {
                return (
                    "400 Bad Request",
                    json!({"error": "unsupported telemetry source"}).to_string(),
                );
            }
            let payload: Value = match serde_json::from_str(body) {
                Ok(payload) => payload,
                Err(error) => {
                    return (
                        "400 Bad Request",
                        json!({"error": format!("invalid JSON: {error}")}).to_string(),
                    )
                }
            };
            match crate::telemetry_store::shared_store() {
                Some(store) => match store.record_hook(source, &payload).and_then(|_| store.record_final_session_usage(source, &payload)) {
                    Ok(true) => ("202 Accepted", json!({"status": "confirmed_session_usage", "source": source}).to_string()),
                    Ok(false) => ("202 Accepted", json!({"status": "recorded_unverified", "source": source, "note": "event retained but not included in token totals; send event=session_usage for a final session total"}).to_string()),
                    Err(error) => ("500 Internal Server Error", json!({"error": error}).to_string()),
                },
                None => ("503 Service Unavailable", json!({"error": "telemetry store not ready"}).to_string()),
            }
        }

        // ── DSH 插件 MCP 观测上报 ────────────────────────────────────────
        // The cordis plugin reports tool/call and tool/result pairs; status,
        // failure classification and "result missing" semantics stay in the
        // telemetry store so the plugin never decides what counts as lost.
        ("POST", p) if p.starts_with("/mcp/observation") => {
            let observation = match crate::dsh_observe::parse_observation(body) {
                Ok(observation) => observation,
                Err(error) => {
                    return ("400 Bad Request", json!({ "error": error }).to_string());
                }
            };
            match crate::telemetry_store::shared_store() {
                Some(store) => match crate::dsh_observe::record_observation(&store, &observation) {
                    Ok(()) => (
                        "202 Accepted",
                        json!({ "status": "recorded", "phase": if matches!(observation.phase, crate::dsh_observe::DshObservationPhase::Start) { "start" } else { "end" } }).to_string(),
                    ),
                    Err(error) => ("500 Internal Server Error", json!({ "error": error }).to_string()),
                },
                None => (
                    "503 Service Unavailable",
                    json!({ "error": "telemetry store not ready" }).to_string(),
                ),
            }
        }

        _ => (
            "404 Not Found",
            json!({"error": "not found", "path": path}).to_string(),
        ),
    }
}

/// Hook 注入的 stdout 响应体（SessionStart / UserPromptSubmit 同构）。Claude
/// 形态 harness（Qoder / Claude Code / WorkBuddy）要求结构化 JSON，注入正文放
/// 在 `hookSpecificOutput.additionalContext`，hookEventName 需与事件同名；纯文
/// 本 stdout 会被丢弃（日志中的 "winner=none (no parsed output)"）。Codex 的
/// hook 协议不同且未验证，保持纯文本以免破坏其现有行为。
fn hook_inject_body(source: &str, event: &str, context: String) -> String {
    if matches!(source, "claude" | "qoder" | "workbuddy") {
        json!({
            "hookSpecificOutput": {
                "hookEventName": event,
                "additionalContext": context
            }
        })
        .to_string()
    } else {
        context
    }
}

#[cfg(test)]
mod tests {
    use super::hook_inject_body;

    #[tokio::test]
    async fn removed_dispatch_and_task_routes_are_unavailable() {
        let store = std::sync::Arc::new(super::AgentHttpStore::new());
        for (method, path) in [("POST", "/agent/dispatch"), ("POST", "/agent/submit"), ("GET", "/agent/tasks"), ("GET", "/agent/results")] {
            let (status, _) = super::route(method, path, "{}", &store).await;
            assert_eq!(status, "404 Not Found", "{method} {path}");
        }
        assert_eq!(super::route("GET", "/health", "", &store).await.0, "200 OK");
    }


    #[test]
    fn claude_shape_sources_get_structured_hook_envelope() {
        for source in ["claude", "qoder", "workbuddy"] {
            let body = hook_inject_body(source, "SessionStart", "记忆正文".to_string());
            let value: serde_json::Value =
                serde_json::from_str(&body).expect("envelope must be valid JSON");
            assert_eq!(value["hookSpecificOutput"]["hookEventName"], "SessionStart");
            assert_eq!(value["hookSpecificOutput"]["additionalContext"], "记忆正文");
        }
    }

    #[test]
    fn prompt_submit_envelope_names_its_own_event() {
        let body = hook_inject_body("qoder", "UserPromptSubmit", "每轮注入正文".to_string());
        let value: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(
            value["hookSpecificOutput"]["hookEventName"],
            "UserPromptSubmit"
        );
        assert_eq!(
            value["hookSpecificOutput"]["additionalContext"],
            "每轮注入正文"
        );
    }

    #[test]
    fn codex_and_unknown_sources_keep_plain_text() {
        for source in ["codex", "unknown"] {
            let body = hook_inject_body(source, "SessionStart", "plain body".to_string());
            assert_eq!(body, "plain body");
        }
    }

    #[test]
    fn envelope_escapes_special_characters() {
        let body = hook_inject_body("qoder", "SessionStart", "line1\n\"quoted\"\t端".to_string());
        let value: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(
            value["hookSpecificOutput"]["additionalContext"],
            "line1\n\"quoted\"\t端"
        );
    }
}
