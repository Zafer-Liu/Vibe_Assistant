//! On-demand external MCP tool discovery. Runs a short-lived, isolated stdio
//! child to ask a server for `tools/list`. Never touches a live relay and
//! persists nothing: tool names and trimmed descriptions are returned to the
//! caller only.

use crate::mcp_registry::mcp_catalog_list;
use crate::mcp_stdio_relay::child_command;
use crate::process_util::no_window;
use serde::{Deserialize, Serialize};
use std::io::{BufRead, BufReader, Read};
use std::process::{Child, ChildStdin, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

const DISCOVERY_TIMEOUT: Duration = Duration::from_secs(20);
const START_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_TOOLS: usize = 300;
const MAX_PAGES: usize = 20;
const MAX_LINE: usize = 4 * 1024 * 1024;

#[derive(Serialize)]
#[serde(rename_all = "snake_case")]
pub struct McpToolDiscovery {
    pub name: String,
    pub transport: String,
    /// complete | empty | partial | unsupported | error
    pub state: String,
    pub tools: Vec<McpDiscoveredTool>,
    pub fetched_at: String,
    pub complete: bool,
    pub reason: Option<String>,
}

#[derive(Serialize)]
pub struct McpDiscoveredTool {
    pub name: String,
    pub description: String,
}

#[derive(Deserialize)]
struct CatalogEntry {
    name: String,
    transport: String,
    command: String,
    #[serde(default)]
    args: Vec<String>,
    #[serde(default)]
    env: std::collections::BTreeMap<String, String>,
}

/// Discover the advertised tool list of a catalog entry on explicit request.
/// HTTP/SSE transports have no safe client implementation yet and return an
/// honest `unsupported` state instead of inferring tools from observations.
#[tauri::command]
pub async fn mcp_discover_tools(name: String) -> Result<McpToolDiscovery, String> {
    let name = name.trim().to_string();
    if name.is_empty() { return Err("MCP 名称不能为空".into()); }
    let catalog = mcp_catalog_list().await?;
    let Some(entry) = catalog.iter().find(|entry| entry.name == name) else {
        return Err("MCP 服务不在目录中".into());
    };
    // Copy only the fields discovery needs; the catalog itself may carry secrets.
    let serialized = serde_json::to_value(entry).map_err(|_| "MCP 目录数据无效".to_string())?;
    let entry: CatalogEntry = serde_json::from_value(serialized)
        .map_err(|_| "MCP 目录数据无效".to_string())?;
    let transport = if entry.transport.is_empty() { "stdio".to_string() } else { entry.transport.clone() };
    let fetched_at = chrono::Utc::now().to_rfc3339();
    let base = McpToolDiscovery {
        name: entry.name.clone(), transport: transport.clone(), state: String::new(),
        tools: Vec::new(), fetched_at, complete: false, reason: None,
    };
    if transport != "stdio" {
        return Ok(McpToolDiscovery {
            state: "unsupported".into(),
            reason: Some("仅支持 stdio 服务的工具发现；远程 SSE/HTTP 暂未实现".into()),
            ..base
        });
    }
    // A short-lived discovery job must not own a shared daemon: Windows
    // terminates every descendant when the job is cleaned up after tools/list.
    if starts_shared_daemon(&entry) {
        return Ok(McpToolDiscovery {
            state: "unsupported".into(),
            reason: Some("该命令会启动共享后台服务；临时进程探测可能终止其他客户端的连接，请通过已连接的 MCP 客户端查看工具列表".into()),
            ..base
        });
    }
    let discovery = tauri::async_runtime::spawn_blocking(move || run_stdio_discovery(&entry))
        .await.map_err(|_| "MCP 工具探测任务中断".to_string())?;
    match discovery {
        Ok((mut tools, complete)) => {
            let state = if !complete { "partial" } else if tools.is_empty() { "empty" } else { "complete" };
            tools.truncate(MAX_TOOLS);
            Ok(McpToolDiscovery { state: state.into(), tools, complete, ..base })
        }
        Err(reason) => Ok(McpToolDiscovery { state: "error".into(), reason: Some(reason), ..base }),
    }
}

/// Identify the daemon bootstrap by executable/module path, not the catalog label.
fn starts_shared_daemon(entry: &CatalogEntry) -> bool {
    let pieces = std::iter::once(&entry.command).chain(entry.args.iter());
    pieces.into_iter().any(|piece| {
        let normalized = piece.replace('\\', "/").to_ascii_lowercase();
        let file = normalized.rsplit('/').next().unwrap_or(&normalized);
        matches!(file, "zg" | "zg.cmd" | "zg.exe" | "zvec-grep" | "zvec-grep.cmd" | "zvec-grep.exe")
            || normalized.contains("/node_modules/@zvec/zvec-grep/")
    })
}

fn run_stdio_discovery(entry: &CatalogEntry) -> Result<(Vec<McpDiscoveredTool>, bool), String> {
    let mut command = child_command(&entry.command, &entry.args)
        .map_err(|_| "无法安全启动该 MCP 服务命令".to_string())?;
    command.envs(&entry.env)
        .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null());
    no_window(&mut command);
    let deadline = Instant::now() + DISCOVERY_TIMEOUT;
    let mut child = command.spawn().map_err(|_| "MCP 服务启动失败".to_string())?;
    #[cfg(windows)]
    let job = match DiscoveryJob::attach(&child) {
        Ok(job) => std::sync::Arc::new(job),
        Err(()) => {
            let _ = child.kill();
            let _ = child.wait();
            return Err("无法为 MCP 工具探测设置进程清理".into());
        }
    };
    #[cfg(windows)]
    let (stop_watchdog, watchdog) = {
        let job = std::sync::Arc::clone(&job);
        let (stop, stopped) = mpsc::channel::<()>();
        let watchdog = thread::spawn(move || {
            if stopped.recv_timeout(deadline.saturating_duration_since(Instant::now())).is_err() {
                job.terminate();
            }
        });
        (stop, watchdog)
    };
    eprintln!("[mcp-discovery] pid={} started", child.id());
    let result = exchange(&mut child, deadline);
    eprintln!("[mcp-discovery] pid={} cleanup=process-tree result={}", child.id(), if result.is_ok() { "ok" } else { "error" });
    // Close the private Windows job before waiting, including shell descendants
    // that could otherwise keep the stdout reader alive after the deadline.
    #[cfg(windows)]
    {
        let _ = stop_watchdog.send(());
        job.terminate();
        let _ = watchdog.join();
        drop(job);
    }
    let _ = child.kill();
    let _ = child.wait();
    result
}

fn exchange(child: &mut Child, deadline: Instant) -> Result<(Vec<McpDiscoveredTool>, bool), String> {
    let mut stdin = child.stdin.take().ok_or("MCP 服务 stdin 不可用")?;
    let stdout = child.stdout.take().ok_or("MCP 服务 stdout 不可用")?;
    // A dedicated reader thread keeps every wait bounded; a hung server can
    // never block discovery past the deadline.
    let (lines, receiver) = mpsc::sync_channel::<Result<String, ()>>(2);
    thread::spawn(move || {
        let mut reader = BufReader::new(stdout);
        loop {
            if Instant::now() >= deadline { break; }
            let mut bytes = Vec::new();
            // Bound allocation before a newline is received, not after read_line.
            let read = (&mut reader).take((MAX_LINE + 1) as u64).read_until(b'\n', &mut bytes);
            match read {
                Ok(0) => break,
                Ok(_) if bytes.len() <= MAX_LINE => {
                    let Ok(line) = String::from_utf8(bytes) else { let _ = lines.send(Err(())); break; };
                    if lines.send(Ok(line)).is_err() { break; }
                }
                _ => { let _ = lines.send(Err(())); break; }
            }
        }
    });

    let start_deadline = (Instant::now() + START_TIMEOUT).min(deadline);
    let mut next_id: u64 = 0;
    let initialized = request(&mut stdin, &receiver, &mut next_id, "initialize", serde_json::json!({
        "protocolVersion": "2025-03-26", "capabilities": {},
        "clientInfo": {"name": "agent-manager-discovery", "version": "1"},
    }), start_deadline)
        .ok_or("MCP 服务初始化超时或失败".to_string())?;
    if initialized.get("error").is_some() || !initialized.get("result").is_some_and(|value| value.is_object()) {
        return Err("MCP 服务初始化未成功".into());
    }
    write_line(&mut stdin, &serde_json::json!({"jsonrpc": "2.0", "method": "notifications/initialized"}))
        .ok_or("MCP 服务握手失败".to_string())?;

    let mut tools: Vec<McpDiscoveredTool> = Vec::new();
    let mut cursor: Option<serde_json::Value> = None;
    for _ in 0..MAX_PAGES {
        if tools.len() >= MAX_TOOLS { return Ok((tools, false)); }
        let mut params = serde_json::json!({});
        if let Some(value) = cursor.take() { params["cursor"] = value; }
        let response = request(&mut stdin, &receiver, &mut next_id, "tools/list", params, deadline)
            .ok_or("获取工具列表超时或失败".to_string())?;
        if let Some(error) = response.get("error") {
            // Report only the stable JSON-RPC code; arbitrary server text may leak data.
            let code = error.get("code").and_then(|code| code.as_i64()).unwrap_or(0);
            return Err(format!("服务返回错误（代码 {code}）"));
        }
        let result = response.get("result").cloned().unwrap_or_default();
        let items = result.get("tools").and_then(|tools| tools.as_array())
            .ok_or("MCP 服务返回了无效的工具清单".to_string())?;
        {
            for item in items {
                if tools.len() >= MAX_TOOLS { return Ok((tools, false)); }
                let name = item.get("name").and_then(|name| name.as_str()).unwrap_or_default().trim();
                if name.is_empty() { continue; }
                // Bounded, sanitized metadata only: name + short description.
                let description = item.get("description").and_then(|value| value.as_str()).unwrap_or_default()
                    .chars().take(200).collect::<String>();
                tools.push(McpDiscoveredTool {
                    name: name.chars().take(120).collect(),
                    description,
                });
            }
        }
        cursor = result.get("nextCursor").and_then(|value| value.as_str())
            .map(|value| serde_json::Value::String(value.to_string()));
        if cursor.is_none() { return Ok((tools, true)); }
    }
    Ok((tools, false))
}

/// Send one JSON-RPC request and wait for its reply with a hard deadline.
/// Unrelated frames (notifications, server-initiated requests) are skipped.
fn request(
    stdin: &mut ChildStdin,
    receiver: &mpsc::Receiver<Result<String, ()>>,
    next_id: &mut u64,
    method: &str,
    params: serde_json::Value,
    deadline: Instant,
) -> Option<serde_json::Value> {
    let id = *next_id;
    *next_id += 1;
    let payload = serde_json::json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
    write_line(stdin, &payload)?;
    loop {
        let remaining = deadline.checked_duration_since(Instant::now())?;
        if remaining.is_zero() { return None; }
        let line = receiver.recv_timeout(remaining).ok()?.ok()?;
        if line.len() > MAX_LINE { return None; }
        let Ok(value) = serde_json::from_str::<serde_json::Value>(line.trim()) else { continue };
        // JSON-RPC ids are scoped independently in each direction. A server
        // request may reuse our id and must never be mistaken for its reply.
        if value.get("method").is_some() {
            if let Some(server_id) = value.get("id").filter(|id| !id.is_null()) {
                write_line(stdin, &serde_json::json!({
                    "jsonrpc": "2.0", "id": server_id,
                    "error": {"code": -32601, "message": "Discovery client does not support server requests"}
                }))?;
            }
            continue;
        }
        if value.get("id").and_then(|value| value.as_u64()) == Some(id)
            && (value.get("result").is_some() || value.get("error").is_some()) {
            return Some(value);
        }
    }
}

fn write_line(stdin: &mut ChildStdin, payload: &serde_json::Value) -> Option<()> {
    use std::io::Write;
    stdin.write_all(payload.to_string().as_bytes()).ok()?;
    stdin.write_all(b"\n").ok()?;
    stdin.flush().ok()
}

// A discovery owns only its newly spawned process tree, never a live MCP relay.
#[cfg(windows)]
struct DiscoveryJob { _handle: std::os::windows::io::OwnedHandle }

#[cfg(windows)]
impl DiscoveryJob {
    fn terminate(&self) {
        use std::os::windows::io::AsRawHandle;
        unsafe { windows_sys::Win32::System::JobObjects::TerminateJobObject(self._handle.as_raw_handle(), 1); }
    }

    fn attach(child: &Child) -> Result<Self, ()> {
        use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
        use windows_sys::Win32::System::JobObjects::{
            AssignProcessToJobObject, CreateJobObjectW, SetInformationJobObject,
            JobObjectExtendedLimitInformation, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
            JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        };
        unsafe {
            let handle = CreateJobObjectW(std::ptr::null(), std::ptr::null());
            if handle.is_null() { return Err(()); }
            let owned = OwnedHandle::from_raw_handle(handle);
            let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
            limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            if SetInformationJobObject(owned.as_raw_handle(), JobObjectExtendedLimitInformation,
                (&limits as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(), std::mem::size_of_val(&limits) as u32) == 0
                || AssignProcessToJobObject(owned.as_raw_handle(), child.as_raw_handle()) == 0 {
                return Err(());
            }
            Ok(Self { _handle: owned })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shared_daemon_detection_uses_command_not_display_name() {
        let entry = CatalogEntry { name: "renamed".into(), transport: "stdio".into(),
            command: "cmd".into(), args: vec!["/c".into(), r"E:\Tool\node\zg.cmd".into(), "server".into(), "--stdio".into()],
            env: Default::default() };
        assert!(starts_shared_daemon(&entry));
        let unrelated = CatalogEntry { name: "zvec-grep".into(), command: "ordinary-server.exe".into(),
            args: vec![], ..entry };
        assert!(!starts_shared_daemon(&unrelated));
    }
}
