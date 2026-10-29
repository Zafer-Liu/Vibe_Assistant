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

        // ── dsh bridge 插件六端点 ────────────────────────────────────────
        // 契约：docs/30-dsh-bridge-plugin-plan.md §4.1（POST、CORS *、仅本机、
        // 无鉴权）。检索/画像/Skill 与 MCP 通道共用同一实现（memory_mcp），
        // 保证工具直连与 MCP 输出同构；usage 与 collect 是插件专属通道。
        ("POST", p) if p == "/memory/recall" || p.starts_with("/memory/recall?") => {
            // 语义检索（memory_backend / local_semantic）内部使用
            // reqwest::blocking 客户端：在 tokio worker 线程上创建/drop 其
            // 内嵌 runtime 会 panic（"Cannot drop a runtime in a context
            // where blocking is not allowed"）。把整个处理器挪到 blocking
            // 池执行，与其原生同步上下文（MCP stdio）对齐。
            let payload = body.to_string();
            match tauri::async_runtime::spawn_blocking(move || {
                handle_memory_recall(&payload, crate::telemetry_store::shared_store().as_deref())
            })
            .await
            {
                Ok(result) => result,
                Err(_) => (
                    "500 Internal Server Error",
                    json!({"error": "recall task interrupted"}).to_string(),
                ),
            }
        }

        ("POST", p) if p == "/memory/profile" || p.starts_with("/memory/profile?") => {
            handle_memory_profile(crate::telemetry_store::shared_store().as_deref())
        }

        ("POST", p) if p == "/skills/list" || p.starts_with("/skills/list?") => {
            handle_skills_list()
        }

        ("POST", p) if p == "/skills/read" || p.starts_with("/skills/read?") => {
            handle_skills_read(body)
        }

        ("POST", p) if p.starts_with("/telemetry/usage") => {
            // 逐条 usage 是 dsh 插件专属通道，不复用 /telemetry/events/<source>
            // （后者是整会话报告语义，白名单刻意排除 dsh）。
            if telemetry_source(p) != Some("dsh") {
                return (
                    "400 Bad Request",
                    json!({"error": "unsupported telemetry source"}).to_string(),
                );
            }
            handle_telemetry_usage(body, crate::telemetry_store::shared_store().as_deref())
        }

        ("POST", p) if p.starts_with("/collect/trigger") => {
            if telemetry_source(p) != Some("dsh") {
                return (
                    "400 Bad Request",
                    json!({"error": "unsupported collect source"}).to_string(),
                );
            }
            handle_collect_trigger(body)
        }

        _ => (
            "404 Not Found",
            json!({"error": "not found", "path": path}).to_string(),
        ),
    }
}

/// 从路径中取 `?source=` 参数（无查询串时 None）。
fn telemetry_source(path: &str) -> Option<&str> {
    path.split_once('?')
        .map(|(_, query)| query)
        .and_then(|query| {
            query
                .split('&')
                .find_map(|part| part.strip_prefix("source="))
        })
}

// ── dsh bridge 六端点处理器 ─────────────────────────────────────────────────
// store 以参数注入而非直接取全局，route() 直测时可传内存账本；Err 映射沿
// 用既有 route 风格（校验/语义错误 400，账本层错误 500，账本未就绪 503）。

fn bad_request(error: String) -> (&'static str, String) {
    ("400 Bad Request", json!({ "error": error }).to_string())
}

fn parse_json_body(body: &str) -> Result<Value, String> {
    serde_json::from_str(body).map_err(|error| format!("invalid JSON: {error}"))
}

fn store_unavailable() -> (&'static str, String) {
    (
        "503 Service Unavailable",
        json!({"error": "telemetry store not ready"}).to_string(),
    )
}

/// `POST /memory/recall`：`{query, limit? 1-12 默认6}` → recall 同构 JSON。
fn handle_memory_recall(
    body: &str,
    store: Option<&crate::telemetry_store::TelemetryStore>,
) -> (&'static str, String) {
    let arguments = match parse_json_body(body) {
        Ok(value) => value,
        Err(error) => return bad_request(error),
    };
    let Some(store) = store else {
        return store_unavailable();
    };
    // 预算 3000ms：略低于桥接插件默认 2s 请求超时的服务端一侧必须更快——
    // 插件超时是 2s，这里给语义检索 3s 预算看似更慢，但预算只限制语义检索
    // 这一步，本地关键词检索与 RRF 融合是毫秒级；总耗时稳定在预算量级内，
    // 远程嵌入慢时按本地降级返回，不再让调用整段超过插件 deadline。
    match crate::memory_mcp::recall_with_store_budget(store, &arguments, Some(3_000)) {
        Ok(value) => ("200 OK", value.to_string()),
        Err(error) => bad_request(error),
    }
}

/// `POST /memory/profile`：`{}` → `{profile: string|null}`。
fn handle_memory_profile(
    store: Option<&crate::telemetry_store::TelemetryStore>,
) -> (&'static str, String) {
    let Some(store) = store else {
        return store_unavailable();
    };
    match crate::memory_mcp::user_profile_with_store(store) {
        Ok(value) => ("200 OK", value.to_string()),
        Err(error) => ("500 Internal Server Error", json!({ "error": error }).to_string()),
    }
}

/// `POST /skills/list`：`{}` → `{skills:[{source,name,description,version}]}`。
fn handle_skills_list() -> (&'static str, String) {
    match crate::memory_mcp::shared_skills_list() {
        Ok(value) => ("200 OK", value.to_string()),
        Err(error) => ("500 Internal Server Error", json!({ "error": error }).to_string()),
    }
}

/// `POST /skills/read`：`{source, name}` → `{source,name,content}`。
fn handle_skills_read(body: &str) -> (&'static str, String) {
    let payload = match parse_json_body(body) {
        Ok(value) => value,
        Err(error) => return bad_request(error),
    };
    let (source, name) = match (
        payload.get("source").and_then(Value::as_str),
        payload.get("name").and_then(Value::as_str),
    ) {
        (Some(source), Some(name))
            if !source.trim().is_empty() && !name.trim().is_empty() =>
        {
            (source.trim().to_string(), name.trim().to_string())
        }
        _ => return bad_request("source 与 name 不能为空".into()),
    };
    match crate::memory_mcp::shared_skill_read(&source, &name) {
        Ok(value) => ("200 OK", value.to_string()),
        // 标识非法与未发布都是调用方可修正的语义错误，随 fs 失败一并 400，
        // 与 /memory/hook 的处理口径一致。
        Err(error) => bad_request(error),
    }
}

/// `POST /telemetry/usage?source=dsh`：逐条 token 用量幂等入库。
fn handle_telemetry_usage(
    body: &str,
    store: Option<&crate::telemetry_store::TelemetryStore>,
) -> (&'static str, String) {
    let payload = match parse_json_body(body) {
        Ok(value) => value,
        Err(error) => return bad_request(error),
    };
    let Some(store) = store else {
        return store_unavailable();
    };
    match store.record_dsh_plugin_usage(&payload) {
        Ok(record_id) => (
            "202 Accepted",
            json!({"status": "recorded", "record_id": record_id}).to_string(),
        ),
        Err(error) => bad_request(error),
    }
}

/// `POST /collect/trigger?source=dsh`：校验会话标识后立即 202，收集（L0
/// 入账 + L1 提取）在后台任务中执行——插件的 turn-stopping 钩子不能被
/// 记忆模型阻塞。结果与错误只进日志（eprintln + IngestLog）。
fn handle_collect_trigger(body: &str) -> (&'static str, String) {
    let payload = match parse_json_body(body) {
        Ok(value) => value,
        Err(error) => return bad_request(error),
    };
    let raw_session = payload.get("session_id").and_then(Value::as_str).unwrap_or("");
    let session_uuid = match crate::memory_ingest::dsh_session_uuid(raw_session) {
        Ok(session_uuid) => session_uuid,
        Err(error) => return bad_request(error),
    };
    tokio::spawn(async move {
        match crate::memory_ingest::collect_dsh_session(&session_uuid).await {
            Ok(outcome) => eprintln!(
                "[agent-http] dsh collect {}（{}）：{}",
                outcome.session_id, outcome.l1, outcome.detail
            ),
            Err(error) => {
                eprintln!("[agent-http] dsh collect failed（session-{session_uuid}）：{error}")
            }
        }
    });
    ("202 Accepted", json!({"status": "scheduled"}).to_string())
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

    // ── dsh bridge 六端点（docs/30 §4.1 契约）────────────────────────────
    // route() 直测沿用既有模式；需要账本的 happy path 走处理器直调并注入
    // 内存账本（in_memory），避免触碰 shared_store 全局与真实数据库——
    // TelemetryStore::new() 会打开生产库并做写探测，单测绝不能触达。

    const DSH_SESSION_UUID: &str = "019f8e90-36f5-71a2-bddf-f0fe9ee23b57";

    #[tokio::test]
    async fn dsh_plugin_routes_dispatch_validate_and_schedule() {
        let store = std::sync::Arc::new(super::AgentHttpStore::new());
        // 全局账本未初始化（单测环境 shared_store() 为 None）时，
        // 依赖账本的端点一律 503，插件可据此降级。
        for (path, body) in [
            ("/memory/recall", r#"{"query":"部署"}"#.to_string()),
            ("/memory/profile", "{}".to_string()),
            (
                "/telemetry/usage?source=dsh",
                format!(
                    r#"{{"session_id":"{DSH_SESSION_UUID}","input_tokens":10,"output_tokens":2,"cached_tokens":0}}"#
                ),
            ),
        ] {
            let (status, _) = super::route("POST", path, &body, &store).await;
            assert_eq!(status, "503 Service Unavailable", "{path}");
        }
        // 端点只收 POST。
        for path in [
            "/memory/recall",
            "/memory/profile",
            "/skills/list",
            "/skills/read",
            "/telemetry/usage?source=dsh",
            "/collect/trigger?source=dsh",
        ] {
            let (status, _) = super::route("GET", path, "", &store).await;
            assert_eq!(status, "404 Not Found", "GET {path}");
        }
        // 非法 JSON 一律 400（在账本检查之前）。
        for path in [
            "/memory/recall",
            "/skills/read",
            "/telemetry/usage?source=dsh",
            "/collect/trigger?source=dsh",
        ] {
            let (status, _) = super::route("POST", path, "{bad", &store).await;
            assert_eq!(status, "400 Bad Request", "POST {path}");
        }
        // source 白名单：usage 与 collect 仅接受 dsh。
        assert_eq!(
            super::route("POST", "/telemetry/usage?source=claude", "{}", &store).await.0,
            "400 Bad Request"
        );
        assert_eq!(
            super::route("POST", "/telemetry/usage", "{}", &store).await.0,
            "400 Bad Request"
        );
        assert_eq!(
            super::route("POST", "/collect/trigger?source=qoder", "{}", &store).await.0,
            "400 Bad Request"
        );
        // collect：session_id 缺失/非 uuid → 400；合法 uuid 立即 202 scheduled
        //（收集在后台任务中执行）。
        assert_eq!(
            super::route("POST", "/collect/trigger?source=dsh", "{}", &store).await.0,
            "400 Bad Request"
        );
        assert_eq!(
            super::route(
                "POST",
                "/collect/trigger?source=dsh",
                r#"{"session_id":"not-a-uuid"}"#,
                &store
            )
            .await
            .0,
            "400 Bad Request"
        );
        let (status, body) = super::route(
            "POST",
            "/collect/trigger?source=dsh",
            &format!(r#"{{"session_id":"{DSH_SESSION_UUID}"}}"#),
            &store,
        )
        .await;
        assert_eq!(status, "202 Accepted");
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&body).unwrap()["status"],
            "scheduled"
        );
    }

    #[test]
    fn memory_recall_rejects_empty_query_with_injected_store() {
        let store = crate::telemetry_store::TelemetryStore::in_memory();
        // 空 query 在检索启动前被拒；不触发语义检索（其内部会自开生产库
        // 连接，单测不触达），故只以空 query 断言 400 路径。
        let (status, body) = super::handle_memory_recall(r#"{"query":"   "}"#, Some(&store));
        assert_eq!(status, "400 Bad Request");
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&body).unwrap()["error"],
            "query 不能为空"
        );
        assert_eq!(
            super::handle_memory_recall(r#"{"query":"x"}"#, None).0,
            "503 Service Unavailable"
        );
    }

    #[test]
    fn memory_profile_returns_null_document_or_unavailable() {
        let store = crate::telemetry_store::TelemetryStore::in_memory();
        let (status, body) = super::handle_memory_profile(Some(&store));
        assert_eq!(status, "200 OK");
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&body).unwrap()["profile"],
            serde_json::Value::Null
        );
        assert_eq!(super::handle_memory_profile(None).0, "503 Service Unavailable");
    }

    #[test]
    fn skills_endpoints_list_shape_and_read_validation() {
        let (status, body) = super::handle_skills_list();
        assert_eq!(status, "200 OK");
        let value: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert!(value["skills"].as_array().is_some(), "skills 必须是数组：{body}");
        // read：缺参 400；未知名按标识校验/读取失败 400。
        assert_eq!(super::handle_skills_read("{}").0, "400 Bad Request");
        assert_eq!(super::handle_skills_read(r#"{"source":"dsh"}"#).0, "400 Bad Request");
        let (status, body) =
            super::handle_skills_read(r#"{"source":"dsh","name":"no-such-skill"}"#);
        assert_eq!(status, "400 Bad Request");
        assert!(
            serde_json::from_str::<serde_json::Value>(&body).unwrap()["error"].is_string(),
            "错误响应需带可读 error：{body}"
        );
    }

    #[test]
    fn telemetry_usage_is_idempotent_and_canonicalizes_session() {
        let store = crate::telemetry_store::TelemetryStore::in_memory();
        let body = format!(
            r#"{{"session_id":"{DSH_SESSION_UUID}","model":"zhipu/glm-5.3","input_tokens":1200,"output_tokens":80,"cached_tokens":300,"occurred_at":"2026-10-28T10:00:00Z","message_id":"42"}}"#
        );
        let (first_status, first_body) = super::handle_telemetry_usage(&body, Some(&store));
        assert_eq!(first_status, "202 Accepted");
        let (second_status, second_body) = super::handle_telemetry_usage(&body, Some(&store));
        assert_eq!(second_status, "202 Accepted");
        let first: serde_json::Value = serde_json::from_str(&first_body).unwrap();
        let second: serde_json::Value = serde_json::from_str(&second_body).unwrap();
        assert_eq!(first["status"], "recorded");
        assert_eq!(first["record_id"], second["record_id"]);
        // 同 body 重发：INSERT OR REPLACE 幂等，仅一行明细；裸 uuid 归一成
        // 扫描器口径的目录名；origin 固定 dsh_plugin。
        let conn = store.conn.lock().unwrap();
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM usage_records WHERE source='dsh'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 1);
        let row: (String, String, i64, i64, i64) = conn
            .query_row(
                "SELECT session_id, origin, input_tokens, output_tokens, cached_tokens
                   FROM usage_records WHERE source='dsh'",
                [],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                    ))
                },
            )
            .unwrap();
        assert_eq!(
            row,
            (
                format!("session-{DSH_SESSION_UUID}"),
                "dsh_plugin".to_string(),
                1200,
                80,
                300
            )
        );
    }

    #[test]
    fn telemetry_usage_rejects_malformed_payloads() {
        let store = crate::telemetry_store::TelemetryStore::in_memory();
        for body in [
            "{}".to_string(),
            r#"{"session_id":"abc"}"#.to_string(),
            format!(r#"{{"session_id":"{DSH_SESSION_UUID}"}}"#),
            format!(
                r#"{{"session_id":"{DSH_SESSION_UUID}","input_tokens":0,"output_tokens":0,"cached_tokens":0}}"#
            ),
            format!(
                r#"{{"session_id":"{DSH_SESSION_UUID}","input_tokens":1,"output_tokens":1,"cached_tokens":0,"occurred_at":"昨天"}}"#
            ),
            format!(
                r#"{{"session_id":"{DSH_SESSION_UUID}","input_tokens":-5,"output_tokens":1,"cached_tokens":0}}"#
            ),
        ] {
            let (status, response) = super::handle_telemetry_usage(&body, Some(&store));
            assert_eq!(status, "400 Bad Request", "{body}");
            assert!(
                serde_json::from_str::<serde_json::Value>(&response).unwrap()["error"].is_string(),
                "{body}"
            );
        }
        assert_eq!(
            super::handle_telemetry_usage("not-json", Some(&store)).0,
            "400 Bad Request"
        );
    }

    #[tokio::test]
    async fn collect_trigger_reports_readable_result_for_missing_session() {
        // HTTP 层立即 202（见 dsh_plugin_routes_dispatch_validate_and_schedule）；
        // 后台收集对不存在的会话返回可读错误——这里直接断言 helper 的结果，
        // 即日志与插件侧重试时看到的内容。
        let missing = uuid::Uuid::new_v4().to_string();
        let error = crate::memory_ingest::collect_dsh_session(&missing)
            .await
            .expect_err("随机 uuid 不可能命中真实会话目录");
        assert!(
            error.contains(&missing),
            "错误信息应包含会话标识以便排查：{error}"
        );
    }
}
