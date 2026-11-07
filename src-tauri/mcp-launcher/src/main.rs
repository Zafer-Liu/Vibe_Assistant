//! Small process boundary for an observed stdio MCP server. The resolver is
//! shared with the app so the fallback cannot bypass its manifest checks.
#[path = "../../src/mcp_fallback_resolver.rs"]
mod resolver;
#[path = "../../src/mcp_command.rs"]
mod mcp_command;
// Publication APIs are used by the GUI; this binary embeds only selection.
#[allow(dead_code)]
#[path = "../../src/mcp_probe_protocol.rs"]
mod mcp_probe_protocol;
// The GUI also includes this module; this binary only uses read-only lookup.
#[allow(dead_code)]
#[path = "../../src/memory_mcp_runtime.rs"]
mod memory_runtime;

use resolver::FallbackLaunch;
use std::io::{self, Read, Write};
use std::net::{Ipv4Addr, SocketAddrV4, TcpListener, TcpStream};
use std::process::{Child, Command, Stdio};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use uuid::Uuid;

const STARTUP_TIMEOUT: Duration = Duration::from_secs(10);
const OUTPUT_CLOSE_GRACE: Duration = Duration::from_millis(500);
// Upstream stderr is not protocol traffic; only the tail is retained for diagnosis.
const STDERR_TAIL: usize = 4096;

enum StartupFailure { BeforeGo, AfterGo }

fn main() {
    match run() {
        Ok(code) => std::process::exit(code),
        Err(error) => {
            eprintln!("MCP launcher: {error}");
            std::process::exit(1);
        }
    }
}

fn run() -> Result<i32, String> {
    let mut args = std::env::args();
    match (args.next(), args.next()) {
        (Some(_), Some(flag)) if flag == "--mcp-memory" => return run_memory(),
        _ => {}
    }
    let mut args = std::env::args();
    let id = match (args.next(), args.next(), args.next(), args.next()) {
        (Some(_), Some(flag), Some(id), None) if flag == "--mcp-relay" => id,
        _ => return Err("expected --mcp-relay UUID or --mcp-memory".into()),
    };
    let launch = resolver::resolve_fallback(&id)
        .map_err(|reason| format!("unable to resolve MCP assignment: {reason}"))?;
    let mut child = match start_relay(&launch) {
        Ok(child) => child,
        Err(StartupFailure::BeforeGo) => return run_original(&launch),
        Err(StartupFailure::AfterGo) => return Err("MCP relay handoff failed".into()),
    };
    // The GO write is the irreversible handoff: even a write error may mean
    // the relay received GO and has started the original server.
    relay_io(&mut child, None, &default_stderr_tail())
}

fn start_relay(launch: &FallbackLaunch) -> Result<Child, StartupFailure> {
    if launch.relay_command.is_empty() { return Err(StartupFailure::BeforeGo); }
    let listener = TcpListener::bind(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0))
        .map_err(|_| StartupFailure::BeforeGo)?;
    listener.set_nonblocking(true).map_err(|_| StartupFailure::BeforeGo)?;
    let nonce = Uuid::new_v4().to_string();
    let deadline = Instant::now() + STARTUP_TIMEOUT;
    let address = listener.local_addr().map_err(|_| StartupFailure::BeforeGo)?;
    let mut command = Command::new(&launch.relay_command);
    command.args(["--mcp-relay", &launch.assignment_id]);
    command.env("VIBE_MCP_CONTROL_ADDR", address.to_string());
    command.env("VIBE_MCP_CONTROL_NONCE", &nonce);
    command.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null());
    no_window(&mut command);
    let mut child = command.spawn().map_err(|_| StartupFailure::BeforeGo)?;
    if let Err(stage) = await_ready(&listener, &mut child, &nonce, deadline) {
        stop_child(&mut child);
        return Err(stage);
    }
    Ok(child)
}

fn await_ready(
    listener: &TcpListener,
    child: &mut Child,
    nonce: &str,
    deadline: Instant,
) -> Result<(), StartupFailure> {
    let mut stream = loop {
        if Instant::now() >= deadline || child.try_wait().map_err(|_| StartupFailure::BeforeGo)?.is_some() {
            return Err(StartupFailure::BeforeGo);
        }
        match listener.accept() {
            Ok((stream, peer)) if peer.ip().is_loopback() => break stream,
            Ok(_) => continue,
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(_) => return Err(StartupFailure::BeforeGo),
        }
    };
    if read_line(&mut stream, deadline).map_err(|_| StartupFailure::BeforeGo)? != nonce.as_bytes()
        || read_line(&mut stream, deadline).map_err(|_| StartupFailure::BeforeGo)? != b"READY"
        || child.try_wait().map_err(|_| StartupFailure::BeforeGo)?.is_some()
    {
        return Err(StartupFailure::BeforeGo);
    }
    // Once sending GO begins, never start another original server. A failed
    // TCP write may still have reached the relay.
    stream.write_all(b"GO\n").map_err(|_| StartupFailure::AfterGo)?;
    stream.flush().map_err(|_| StartupFailure::AfterGo)?;
    Ok(())
}

fn read_line(stream: &mut TcpStream, deadline: Instant) -> io::Result<Vec<u8>> {
    let mut line = Vec::new();
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(io::Error::new(io::ErrorKind::TimedOut, "relay handshake timed out"));
        }
        stream.set_read_timeout(Some(remaining))?;
        let mut byte = [0u8; 1];
        stream.read_exact(&mut byte)?;
        if byte[0] == b'\n' { return Ok(line); }
        if line.len() >= 128 {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "relay handshake too long"));
        }
        line.push(byte[0]);
    }
}

fn run_original(launch: &FallbackLaunch) -> Result<i32, String> {
    let mut command = original_command(&launch.command, &launch.args, &launch.env)?;
    command.env_remove("VIBE_MCP_CONTROL_ADDR");
    command.env_remove("VIBE_MCP_CONTROL_NONCE");
    command.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
    no_window(&mut command);
    let mut child = command.spawn().map_err(|_| "unable to start MCP server")?;
    let stderr_tail = collect_stderr(&mut child);
    let mut heartbeat = FallbackHeartbeat::new(&launch.assignment_id).ok();
    let result = relay_io(&mut child, heartbeat.as_mut(), &stderr_tail);
    drop(heartbeat);
    result
}

fn collect_stderr(child: &mut Child) -> Arc<Mutex<Vec<u8>>> {
    let tail = default_stderr_tail();
    if let Some(mut stderr) = child.stderr.take() {
        let sink = Arc::clone(&tail);
        std::thread::spawn(move || {
            let mut chunk = [0u8; 8192];
            while let Ok(count @ 1..) = stderr.read(&mut chunk) {
                let Ok(mut tail) = sink.lock() else { return };
                tail.extend_from_slice(&chunk[..count]);
                let excess = tail.len().saturating_sub(STDERR_TAIL);
                if excess > 0 {
                    tail.drain(..excess);
                }
            }
        });
    }
    tail
}

fn default_stderr_tail() -> Arc<Mutex<Vec<u8>>> {
    Arc::new(Mutex::new(Vec::new()))
}

/// Classify upstream stderr without exposing commands, credentials or payloads.
fn stderr_classification(tail: &Arc<Mutex<Vec<u8>>>) -> Option<&'static str> {
    let tail = tail.lock().ok()?;
    let text = String::from_utf8_lossy(&tail);
    let last = text.lines().rev().map(str::trim).find(|line| !line.is_empty())?;
    let last = last.to_ascii_lowercase();
    Some(if last.contains("daemon stopped") || last.contains("daemon_stopped") {
        "stderr: daemon_stopped"
    } else if last.contains("fetch failed") {
        "stderr: fetch failed"
    } else {
        "stderr captured"
    })
}

fn relay_io(child: &mut Child, mut heartbeat: Option<&mut FallbackHeartbeat>, stderr_tail: &Arc<Mutex<Vec<u8>>>) -> Result<i32, String> {
    let mut input = match child.stdin.take() {
        Some(input) => input,
        None => { stop_child(child); return Err("MCP stdin unavailable".into()); }
    };
    let mut output = match child.stdout.take() {
        Some(output) => output,
        None => { stop_child(child); return Err("MCP stdout unavailable".into()); }
    };
    let (events, receiver) = mpsc::channel();
    let input_events = events.clone();
    let input_pump = std::thread::spawn(move || {
        let result = io::copy(&mut io::stdin(), &mut input);
        drop(input);
        let _ = input_events.send((true, result.is_ok()));
    });
    let output_pump = std::thread::spawn(move || {
        let mut stdout = io::stdout().lock();
        let result = io::copy(&mut output, &mut stdout).and_then(|_| stdout.flush());
        let _ = events.send((false, result.is_ok()));
    });
    let mut failed = false;
    let mut output_closed_at = None;
    let mut last_heartbeat = Instant::now();
    let status = loop {
        if last_heartbeat.elapsed() >= Duration::from_secs(3) {
            if let Some(heartbeat) = heartbeat.as_mut() { heartbeat.touch(); }
            last_heartbeat = Instant::now();
        }
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {}
            Err(_) => {
                stop_child(child);
                return Err("unable to wait for MCP server".into());
            }
        }
        if output_closed_at.is_some_and(|time: Instant| time.elapsed() >= OUTPUT_CLOSE_GRACE) {
            let _ = child.kill();
            break child.wait().map_err(|_| "unable to wait for MCP server")?;
        }
        match receiver.recv_timeout(Duration::from_millis(50)) {
            Ok((_, false)) => {
                failed = true;
                stop_child(child);
            }
            Ok((false, true)) => output_closed_at = Some(Instant::now()),
            Ok((true, true)) | Err(mpsc::RecvTimeoutError::Timeout | mpsc::RecvTimeoutError::Disconnected) => {}
        }
    };
    // A parent may keep stdin open after the MCP process exits. Never block on
    // that reader; the process exit will close its pipe and end the reader.
    if input_pump.is_finished() && input_pump.join().is_err() { failed = true; }
    let drain_deadline = Instant::now() + Duration::from_millis(750);
    while !output_pump.is_finished() && Instant::now() < drain_deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    if output_pump.is_finished() {
        if output_pump.join().is_err() { failed = true; }
    } else {
        // An inherited stdout handle can outlive its MCP parent. The drain is
        // bounded so that the launcher cannot wait indefinitely on descendants.
        failed = true;
    }
    while let Ok((_, succeeded)) = receiver.try_recv() {
        if !succeeded { failed = true; }
    }
    if failed {
        return Err(match stderr_classification(stderr_tail) {
            Some(excerpt) => format!("MCP server connection ended unexpectedly: {excerpt}"),
            None => "MCP server connection ended unexpectedly".into(),
        });
    }
    else { Ok(status.code().unwrap_or(1)) }
}

// ---------- `--mcp-memory`: 共享记忆 MCP 的稳定前台 ----------
//
// Agent configurations register this launcher rather than a timestamped runtime.
// Resolve the newest runtime once for each launcher invocation. Each connection
// uses one child; exit with it so the MCP client can reconnect and initialize a
// new session. Never replay input across sessions.

/// Read-only lookup in the same config directory used by GUI publication.
/// New names sort by publication sequence and validate their content hash.
fn newest_memory_runtime() -> Option<std::path::PathBuf> {
    memory_runtime::newest().ok().flatten()
}

fn run_memory() -> Result<i32, String> {
    let exe = newest_memory_runtime()
        .ok_or_else(|| "未找到记忆服务运行副本：请先启动一次应用".to_string())?;
    let mut command = Command::new(&exe);
    command.arg("--mcp-memory");
    command.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
    no_window(&mut command);
    let mut child = command.spawn().map_err(|_| "无法启动记忆服务副本".to_string())?;
    let stderr_tail = collect_stderr(&mut child);
    // Retain stderr for diagnosis without exposing its contents to the client.
    relay_io(&mut child, None, &stderr_tail)
        .map_err(|_| "Memory MCP connection ended unexpectedly".to_string())
}

struct FallbackHeartbeat {
    path: std::path::PathBuf,
    id: String,
}

impl FallbackHeartbeat {
    fn new(id: &str) -> io::Result<Self> {
        let root = dirs_next::config_dir().ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "config directory unavailable"))?
            .join("agent-manager").join("mcp-observation").join("fallback").join(id);
        std::fs::create_dir_all(&root)?;
        let heartbeat = Self { path: root.join(format!("{}.json", std::process::id())), id: id.to_string() };
        heartbeat.write()?;
        Ok(heartbeat)
    }

    fn touch(&mut self) { let _ = self.write(); }

    fn write(&self) -> io::Result<()> {
        let now = SystemTime::now().duration_since(UNIX_EPOCH).map_err(io::Error::other)?.as_millis();
        let payload = serde_json::json!({
            "assignment_id": self.id,
            "pid": std::process::id(),
            "last_seen_unix_ms": now,
        });
        let directory = self.path.parent().ok_or_else(|| io::Error::other("heartbeat directory unavailable"))?;
        let mut temporary = tempfile::NamedTempFile::new_in(directory)?;
        serde_json::to_writer(&mut temporary, &payload).map_err(io::Error::other)?;
        temporary.flush()?;
        temporary.as_file().sync_all()?;
        temporary.persist(&self.path).map_err(|error| error.error)?;
        Ok(())
    }
}

impl Drop for FallbackHeartbeat {
    fn drop(&mut self) { let _ = std::fs::remove_file(&self.path); }
}

fn stop_child(child: &mut Child) {
    let _ = child.kill();
    let _ = child.wait();
}

#[cfg(windows)]
fn no_window(command: &mut Command) {
    use std::os::windows::process::CommandExt;
    command.creation_flags(0x08000000); // CREATE_NO_WINDOW
}
#[cfg(not(windows))]
fn no_window(_: &mut Command) {}

fn original_command(program: &str, args: &[String], environment: &std::collections::BTreeMap<String, String>) -> Result<Command, String> {
    mcp_command::command_with_env(program, args, environment)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn upstream_diagnostics_never_expose_stderr_values() {
        for (text, expected) in [
            ("api-key=private-credential", Some("stderr captured")),
            ("daemon stopped; token=private-credential", Some("stderr: daemon_stopped")),
            ("fetch failed: https://private/token", Some("stderr: fetch failed")),
            ("  ", None),
        ] {
            let tail = Arc::new(Mutex::new(text.as_bytes().to_vec()));
            assert_eq!(stderr_classification(&tail), expected);
        }
    }
}
