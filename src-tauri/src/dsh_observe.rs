//! DeepSeek Harness 的 MCP 调用观测插件（cordis）。
//!
//! DSH 没有外部进程式 Hook，但 cordis 插件可以监听 `session/event`：每条
//! `tool/call` 与 `tool/result` 都会经过事件总线（与 AcpBridge、工具恢复等
//! 内置插件使用同一条通道）。本模块把观测插件分发到 desktop profile：
//!
//! - 插件文件：`<dshHome>/profiles/desktop/plugins/agent-manager-mcp-observe.mjs`
//! - patch 条目：`- insert: [{id: agent-manager-mcp-observe, ...}]`
//!   （相对模块说明符按 profile 目录解析，无需 pnpm/package.json 改动）
//!
//! 插件只上报调用边界（MCP 服务名、工具名、成功/失败），绝不上报工具参数
//! 或返回正文；数据经智管本地 HTTP（`POST /mcp/observation?source=dsh`）
//! 写入 `mcp_call_observations`，origin 为 `dsh_plugin`。
//! `cordis.patch.yml` 被热重载，条目写入后运行中的 DSH 即时生效。

use std::path::PathBuf;

use serde_json::Value;
use sha2::{Digest, Sha256};

/// cordis patch 条目与插件文件共用的稳定 id。
pub(crate) const PLUGIN_ENTRY_ID: &str = "agent-manager-mcp-observe";
const PLUGIN_FILE_NAME: &str = "agent-manager-mcp-observe.mjs";
/// 相对模块说明符：include 层的 baseUrl 是 profile 目录。
const PLUGIN_RELATIVE_NAME: &str = "./plugins/agent-manager-mcp-observe.mjs";

// ── 路径与端点 ────────────────────────────────────────────────────────────────

fn profile_dir() -> Option<PathBuf> {
    crate::agent_sources::dsh_home().map(|home| home.join("profiles").join("desktop"))
}

pub(crate) fn plugin_path() -> Option<PathBuf> {
    profile_dir().map(|dir| dir.join("plugins").join(PLUGIN_FILE_NAME))
}

/// 插件回传地址：与记忆注入插件共用智管本地 HTTP 服务（端口 9420）。
pub(crate) fn endpoint() -> String {
    format!(
        "http://127.0.0.1:{}/mcp/observation?source=dsh",
        crate::agent_http::AGENT_HTTP_PORT
    )
}

// ── 插件源码 ──────────────────────────────────────────────────────────────────

/// 插件 JS：ESM、零依赖（仅 Node 内置能力），由智管整份分发与升级。
fn plugin_source() -> &'static str {
    r#"// Agent Manager (智管) MCP observation bridge for DeepSeek Harness.
// Installed and updated by Agent Manager; manual edits will be overwritten.
// Mechanism: cordis session/event listener on tool/call + tool/result.
export const name = "agent-manager-mcp-observe";

const MCP_PREFIX = "mcp__";

export function apply(ctx, config) {
  const endpoint = typeof config?.endpoint === "string" ? config.endpoint : "";
  if (endpoint === "") return;
  const timeoutMs = Number.isFinite(config?.timeoutMs) ? config.timeoutMs : 1500;
  const maxInflight = Number.isFinite(config?.maxInflight) ? config.maxInflight : 8;
  const maxPending = Number.isFinite(config?.maxPending) ? config.maxPending : 512;
  const inflight = new Set();
  const pending = new Map();

  const post = (payload) => {
    if (inflight.size >= maxInflight) return;
    const task = fetch(endpoint, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify(payload),
      signal: AbortSignal.timeout(timeoutMs),
    }).catch(() => {}).finally(() => { inflight.delete(task); });
    inflight.add(task);
  };

  const splitToolName = (name) => {
    if (typeof name !== "string" || !name.startsWith(MCP_PREFIX)) return undefined;
    const rest = name.slice(MCP_PREFIX.length);
    const index = rest.indexOf("__");
    if (index <= 0) return undefined;
    const server = rest.slice(0, index);
    const tool = rest.slice(index + 2);
    if (server === "" || tool === "") return undefined;
    return { server, tool };
  };

  const remember = (key, value) => {
    if (pending.size >= maxPending) {
      const oldest = pending.keys().next().value;
      if (oldest !== undefined) pending.delete(oldest);
    }
    pending.set(key, value);
  };

  const failureOf = (data) => {
    const message = data?.message;
    const error = data?.error;
    const failed = message?.isError === true || (error !== undefined && error !== null);
    if (!failed) return { status: "success" };
    const payload = {
      status: "failed",
      error_category: "tool_error",
      error_phase: "response",
      failure_kind: "tool_error",
    };
    const name = error?.name;
    if (typeof name === "string" && name !== "") payload.error_reason = name;
    const code = error?.info?.code;
    if (typeof code === "number" && Number.isSafeInteger(code)) payload.error_code = code;
    return payload;
  };

  const listener = (session, event) => {
    try {
      const sessionId = typeof session?.id === "string"
        ? session.id
        : (typeof session?.header?.id === "string" ? session.header.id : "");
      if (sessionId === "") return;
      const surfaceOp = event?.surfaceOp;
      if (surfaceOp !== undefined && surfaceOp !== "append") return;
      const type = event?.type;
      const data = event?.data;
      if (data === undefined || data === null) return;
      if (type === "tool/call") {
        const parsed = splitToolName(data.name);
        const callId = typeof data.callId === "string" ? data.callId : "";
        if (parsed === undefined || callId === "") return;
        remember(sessionId + ":" + callId, parsed);
        post({
          event: "call_start",
          session_id: sessionId,
          call_id: callId,
          server: parsed.server,
          tool: parsed.tool,
        });
        return;
      }
      if (type === "tool/result") {
        const callId = typeof data.message?.toolCallId === "string" ? data.message.toolCallId : "";
        if (callId === "") return;
        const key = sessionId + ":" + callId;
        const started = pending.get(key);
        if (started === undefined) return;
        pending.delete(key);
        post({
          event: "call_end",
          session_id: sessionId,
          call_id: callId,
          server: started.server,
          tool: started.tool,
          ...failureOf(data),
        });
      }
    } catch {}
  };

  ctx.on("session/event", listener);
}
"#
}

// ── patch 条目渲染与移除 ──────────────────────────────────────────────────────

fn render_entry() -> String {
    format!(
        "- insert:\n    - id: {PLUGIN_ENTRY_ID}\n      name: {PLUGIN_RELATIVE_NAME}\n      config:\n        endpoint: {}\n        timeoutMs: 1500\n        maxInflight: 8\n        maxPending: 512",
        crate::dsh_mcp::yaml_quote(&endpoint()),
    )
}

/// 删除含本插件 id 的顶层条目；其余内容逐字节保留。
fn rewrite_observation_items(text: &str) -> Result<String, String> {
    let lines: Vec<&str> = text.lines().collect();
    let items = crate::dsh_mcp::split_top_items(text);
    let mut removed: Vec<(usize, usize)> = Vec::new();
    for item in &items {
        let block = crate::dsh_mcp::item_block(text, item)?;
        if block.contains(PLUGIN_ENTRY_ID) {
            removed.push((item.start, item.end));
        }
    }
    let mut output: Vec<&str> = Vec::with_capacity(lines.len());
    for (index, line) in lines.iter().enumerate() {
        if removed.iter().any(|(start, end)| index >= *start && index < *end) {
            continue;
        }
        output.push(line);
    }
    Ok(output.join("\n"))
}

// ── 对外操作 ──────────────────────────────────────────────────────────────────

/// 安装/修复观测插件：写入插件文件，并确保 patch 层恰好有一条指向它的
/// insert 条目（幂等：先移除旧条目再追加）。
pub(crate) fn install() -> Result<(), String> {
    let plugin = plugin_path()
        .ok_or_else(|| "无法定位 DeepSeek Harness 配置目录（~/.dsh）".to_string())?;
    let patch = crate::dsh_mcp::patch_path_or_error()?;
    if let Some(parent) = plugin.parent() {
        std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    crate::dsh_mcp::write_atomically(&plugin, plugin_source())?;
    let text = match std::fs::read_to_string(&patch) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            crate::dsh_mcp::new_file_header().to_string()
        }
        Err(error) => return Err(format!("读取 {} 失败：{error}", patch.display())),
    };
    let updated = crate::dsh_mcp::append_item(rewrite_observation_items(&text)?, &render_entry());
    crate::dsh_mcp::write_atomically(&patch, &updated)
}

/// 卸载：移除 patch 条目与插件文件。
pub(crate) fn uninstall() -> Result<(), String> {
    let patch = crate::dsh_mcp::patch_path_or_error()?;
    if let Ok(text) = std::fs::read_to_string(&patch) {
        let updated = rewrite_observation_items(&text)?;
        crate::dsh_mcp::write_atomically(&patch, &updated)?;
    }
    if let Some(plugin) = plugin_path() {
        let _ = std::fs::remove_file(plugin);
    }
    Ok(())
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct DshObservationStatus {
    pub installed: bool,
    pub entry_id: String,
    pub plugin_path: Option<String>,
    pub events: Vec<String>,
}

pub(crate) fn observed_events() -> Vec<String> {
    vec!["tool/call".to_string(), "tool/result".to_string()]
}

/// (installed, events)：条目与插件文件都在位才算已安装。
pub(crate) fn status() -> DshObservationStatus {
    let entry_present = crate::dsh_mcp::patch_path()
        .and_then(|path| std::fs::read_to_string(path).ok())
        .map(|text| {
            crate::dsh_mcp::split_top_items(&text)
                .iter()
                .filter_map(|item| crate::dsh_mcp::item_block(&text, item).ok())
                .any(|block| block.contains(PLUGIN_ENTRY_ID))
        })
        .unwrap_or(false);
    let plugin = plugin_path();
    let file_present = plugin.as_ref().is_some_and(|path| path.is_file());
    DshObservationStatus {
        installed: entry_present && file_present,
        entry_id: PLUGIN_ENTRY_ID.to_string(),
        plugin_path: plugin.map(|path| path.to_string_lossy().into_owned()),
        events: observed_events(),
    }
}

#[tauri::command]
pub fn dsh_observation_status() -> DshObservationStatus {
    status()
}

#[tauri::command]
pub fn dsh_observation_set(enabled: bool) -> Result<DshObservationStatus, String> {
    if enabled {
        install()?;
    } else {
        uninstall()?;
    }
    Ok(status())
}

// ── 插件回传载荷 ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DshObservationPhase {
    Start,
    End,
}

#[derive(Debug, Clone)]
pub(crate) struct DshObservation {
    pub phase: DshObservationPhase,
    pub observation_key: String,
    pub session_id: String,
    pub occurred_at: String,
    pub server: String,
    pub tool: String,
    pub status: Option<&'static str>,
    pub category: Option<&'static str>,
    pub error_phase: Option<&'static str>,
    pub failure_kind: Option<&'static str>,
    pub error_code: Option<i64>,
    pub error_reason: Option<String>,
}

const ERROR_CATEGORIES: [&str; 6] = [
    "tool_error",
    "mcp_error",
    "protocol_error",
    "disconnect",
    "timeout",
    "unknown_error",
];
const ERROR_PHASES: [&str; 3] = ["request", "response", "transport"];
const FAILURE_KINDS: [&str; 5] = [
    "parse_error",
    "rpc_error",
    "tool_error",
    "transport_closed",
    "unknown",
];

/// Bound one control-character-free text field; empty or oversized input is a
/// malformed report, never a reason to drop the whole batch.
fn text_field(value: &Value, key: &str, max_chars: usize) -> Result<String, String> {
    let raw = value.get(key).and_then(Value::as_str).unwrap_or("");
    let cleaned: String = raw.chars().filter(|ch| !ch.is_control()).collect();
    let cleaned = cleaned.trim();
    if cleaned.is_empty() || cleaned.chars().count() > max_chars {
        return Err(format!("invalid {key}"));
    }
    Ok(cleaned.to_string())
}

/// MCP server names are restricted to the same alphabet DSH accepts in
/// `serverName`, so the stored name cannot smuggle paths or quoting.
fn server_field(value: &Value) -> Result<String, String> {
    let raw = text_field(value, "server", 64)?;
    if !raw
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '.'))
    {
        return Err("invalid server".into());
    }
    Ok(raw)
}

fn optional_choice(
    value: &Value,
    key: &str,
    allowed: &[&'static str],
) -> Result<Option<&'static str>, String> {
    match value.get(key).and_then(Value::as_str) {
        None => Ok(None),
        Some(raw) => allowed
            .iter()
            .copied()
            .find(|candidate| *candidate == raw)
            .map(Some)
            .ok_or_else(|| format!("invalid {key}")),
    }
}

pub(crate) fn parse_observation(body: &str) -> Result<DshObservation, String> {
    let value: Value =
        serde_json::from_str(body).map_err(|error| format!("invalid JSON: {error}"))?;
    let phase = match value.get("event").and_then(Value::as_str).unwrap_or("") {
        "call_start" => DshObservationPhase::Start,
        "call_end" => DshObservationPhase::End,
        _ => return Err("unsupported observation event".into()),
    };
    let session_id = text_field(&value, "session_id", 128)?;
    let call_id = text_field(&value, "call_id", 256)?;
    let server = server_field(&value)?;
    let tool = text_field(&value, "tool", 128)?;
    let occurred_at = match value.get("occurred_at").and_then(Value::as_str) {
        Some(raw) if !raw.trim().is_empty() => chrono::DateTime::parse_from_rfc3339(raw.trim())
            .map(|time| {
                time.with_timezone(&chrono::Utc)
                    .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
            })
            .map_err(|_| "invalid occurred_at".to_string())?,
        _ => chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
    };
    let mut status = None;
    let mut category = None;
    let mut error_phase = None;
    let mut failure_kind = None;
    let mut error_code = None;
    let mut error_reason = None;
    if phase == DshObservationPhase::End {
        status = Some(match value.get("status").and_then(Value::as_str) {
            Some("success") => "success",
            Some("failed") => "failed",
            _ => return Err("call_end requires status success|failed".into()),
        });
        category = optional_choice(&value, "error_category", &ERROR_CATEGORIES)?;
        error_phase = optional_choice(&value, "error_phase", &ERROR_PHASES)?;
        failure_kind = optional_choice(&value, "failure_kind", &FAILURE_KINDS)?;
        error_code = value.get("error_code").and_then(Value::as_i64);
        error_reason = value
            .get("error_reason")
            .and_then(Value::as_str)
            .map(|raw| {
                raw.chars()
                    .filter(|ch| !ch.is_control())
                    .take(160)
                    .collect::<String>()
            })
            .filter(|reason| !reason.trim().is_empty());
        if status == Some("failed") && category.is_none() {
            category = Some("tool_error");
            error_phase = error_phase.or(Some("response"));
            failure_kind = failure_kind.or(Some("tool_error"));
        }
        if status == Some("success") {
            category = None;
            error_phase = None;
            failure_kind = None;
            error_code = None;
            error_reason = None;
        }
    }
    Ok(DshObservation {
        phase,
        observation_key: observation_key(&session_id, &call_id),
        session_id,
        occurred_at,
        server,
        tool,
        status,
        category,
        error_phase,
        failure_kind,
        error_code,
        error_reason,
    })
}

/// Session and call ids are length-prefixed before hashing so a colon inside
/// either id cannot collide two different calls onto one observation key.
fn observation_key(session_id: &str, call_id: &str) -> String {
    let mut digest = Sha256::new();
    digest.update(b"dsh:");
    digest.update(session_id.len().to_string().as_bytes());
    digest.update(b":");
    digest.update(session_id.as_bytes());
    digest.update(b":");
    digest.update(call_id.len().to_string().as_bytes());
    digest.update(b":");
    digest.update(call_id.as_bytes());
    format!("plugin:{}", hex_lower(&digest.finalize()))
}

fn hex_lower(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        let _ = write!(output, "{byte:02x}");
    }
    output
}

/// Store one parsed report: starts insert a pending row, ends complete it (or
/// insert a completed row when the start report never arrived).
pub(crate) fn record_observation(
    store: &crate::telemetry_store::TelemetryStore,
    observation: &DshObservation,
) -> Result<(), String> {
    match observation.phase {
        DshObservationPhase::Start => store.record_dsh_plugin_call(
            &observation.observation_key,
            &observation.session_id,
            &observation.occurred_at,
            &observation.server,
            &observation.tool,
        ),
        DshObservationPhase::End => store.finish_dsh_plugin_call(
            &observation.observation_key,
            &observation.session_id,
            &observation.occurred_at,
            &observation.server,
            &observation.tool,
            observation.status.unwrap_or("success"),
            observation.category,
            observation.error_phase,
            observation.failure_kind,
            observation.error_code,
            observation.error_reason.as_deref(),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entry_render_contains_stable_id_and_endpoint() {
        let entry = render_entry();
        assert!(entry.contains(&format!("id: {PLUGIN_ENTRY_ID}")));
        assert!(entry.contains("name: ./plugins/agent-manager-mcp-observe.mjs"));
        assert!(entry.contains("/mcp/observation?source=dsh"));
    }

    #[test]
    fn call_start_parses_without_result_fields() {
        let observation = parse_observation(
            r#"{"event":"call_start","session_id":"s1","call_id":"c1","server":"github","tool":"create_issue"}"#,
        )
        .expect("start report must parse");
        assert_eq!(observation.phase, DshObservationPhase::Start);
        assert_eq!(observation.server, "github");
        assert_eq!(observation.status, None);
        assert!(observation.observation_key.starts_with("plugin:"));
    }

    #[test]
    fn call_end_defaults_failure_metadata() {
        let observation = parse_observation(
            r#"{"event":"call_end","session_id":"s1","call_id":"c1","server":"github","tool":"create_issue","status":"failed"}"#,
        )
        .expect("end report must parse");
        assert_eq!(observation.status, Some("failed"));
        assert_eq!(observation.category, Some("tool_error"));
        assert_eq!(observation.failure_kind, Some("tool_error"));
    }

    #[test]
    fn success_clears_error_metadata() {
        let observation = parse_observation(
            r#"{"event":"call_end","session_id":"s1","call_id":"c1","server":"github","tool":"create_issue","status":"success","error_reason":"ignored"}"#,
        )
        .expect("end report must parse");
        assert_eq!(observation.category, None);
        assert_eq!(observation.error_reason, None);
    }

    #[test]
    fn server_identifier_rejects_path_characters() {
        assert!(parse_observation(
            r#"{"event":"call_start","session_id":"s1","call_id":"c1","server":"../etc","tool":"x"}"#
        )
        .is_err());
    }

    #[test]
    fn observation_key_is_stable_and_collision_resistant() {
        let first = observation_key("s1", "c1");
        assert_eq!(first, observation_key("s1", "c1"));
        assert_ne!(observation_key("a:b", "c"), first);
        assert_ne!(observation_key("a", "b:c"), first);
    }
}
