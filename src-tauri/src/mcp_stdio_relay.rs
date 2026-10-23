//! Standalone, byte-preserving stdio MCP relay. Inspection is best effort and
//! never changes bytes handed to either peer or prints protocol payloads.

use crate::mcp_registry::relay_launch;
use crate::telemetry_store::TelemetryStore;
use serde_json::Value;
use std::collections::HashMap;
use std::io::{self, Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::process::{Command, Stdio};
use std::sync::{atomic::{AtomicBool, AtomicU8, Ordering}, mpsc, Arc};
use std::time::{Duration, Instant};
use uuid::Uuid;

// Bounds apply to the observation copy, never to forwarded MCP bytes.
const MAX_FRAME: usize = 1024 * 1024;
const MAX_OBSERVED_STRING: usize = 4096;
const MAX_HEADERS: usize = 8192;
const MAX_PENDING: usize = 1024;
const CONTROL_ADDR: &str = "VIBE_MCP_CONTROL_ADDR";
const CONTROL_NONCE: &str = "VIBE_MCP_CONTROL_NONCE";
const MAX_CONTROL_NONCE: usize = 256;
const CONTROL_TIMEOUT: Duration = Duration::from_secs(5);
// Keep a bounded stderr tail in memory; persist only fixed diagnostic classifications.
const STDERR_TAIL: usize = 4096;

// One static warning per operation for this relay process, regardless of error content.
#[derive(Clone, Copy)]
enum TelemetryOperation { OpenStore, SystemFailure, ConnectionOpen, ConnectionTouch, RecordCall, MarkUnknown, FinishCall, ConnectionClose }

impl TelemetryOperation {
    fn label(self) -> &'static str {
        match self {
            Self::OpenStore => "[mcp relay] telemetry failed: open_store",
            Self::SystemFailure => "[mcp relay] telemetry failed: record_system_failure",
            Self::ConnectionOpen => "[mcp relay] telemetry failed: connection_open",
            Self::ConnectionTouch => "[mcp relay] telemetry failed: connection_touch",
            Self::RecordCall => "[mcp relay] telemetry failed: record_call",
            Self::MarkUnknown => "[mcp relay] telemetry failed: mark_call_unknown",
            Self::FinishCall => "[mcp relay] telemetry failed: finish_call",
            Self::ConnectionClose => "[mcp relay] telemetry failed: connection_close",
        }
    }
}

fn telemetry_failure_label(failed: bool, operation: TelemetryOperation, reported: &AtomicU8) -> Option<&'static str> {
    let bit = 1u8 << operation as u8;
    if failed && reported.fetch_or(bit, Ordering::Relaxed) & bit == 0 {
        Some(operation.label())
    } else {
        None
    }
}

fn telemetry_result<T, E>(result: Result<T, E>, operation: TelemetryOperation) -> Result<T, E> {
    static REPORTED: AtomicU8 = AtomicU8::new(0);
    if let Some(label) = telemetry_failure_label(result.is_err(), operation, &REPORTED) {
        // Never format database errors, identifiers, or protocol content here.
        let _ = writeln!(io::stderr().lock(), "{label}");
    }
    result
}

fn relay_failure(store: &TelemetryStore, connection_id: &str, server: Option<&str>, phase: &str, category: &str, kind: &str, reason: &str) {
    let _ = telemetry_result(store.record_proxy_system_failure(connection_id, server, phase, category, kind, reason), TelemetryOperation::SystemFailure);
}

/// Run one connection; stdout is reserved exclusively for the original MCP bytes.
pub fn run_stdio(assignment_id: &str) -> Result<(), String> {
    let store = telemetry_result(TelemetryStore::new(), TelemetryOperation::OpenStore).map_err(|_| "Unable to open relay telemetry".to_string())?;
    let connection_id = Uuid::new_v4().to_string();
    let control = control_settings().map_err(|error| {
        relay_failure(&store, &connection_id, None, "control", "relay_control", "control_invalid", "relay control settings invalid");
        error
    })?;
    let launch = relay_launch(assignment_id).map_err(|error| {
        relay_failure(&store, &connection_id, None, "assignment", "relay_assignment", "assignment_unavailable", "relay assignment unavailable");
        let _ = error;
        "Unable to resolve MCP relay assignment".to_string()
    })?;
    let server_name = launch.server_name.clone();

    let mut command = child_command(&launch.command, &launch.args)
        .map_err(|e| { relay_failure(&store, &connection_id, Some(&server_name), "startup", "relay_startup", "command_invalid", "command preparation failed"); e })?;
    command.envs(&launch.env);
    // The control channel belongs only to this relay, never the original MCP server.
    command.env_remove(CONTROL_ADDR).env_remove(CONTROL_NONCE);
    command.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
    crate::process_util::no_window(&mut command);
    if let Some((address, nonce)) = control {
        startup_handshake(address, &nonce)
            .map_err(|e| { relay_failure(&store, &connection_id, Some(&server_name), "startup", "relay_startup", "handshake_failed", "control handshake failed"); e })?;
    }
    let mut child = command.spawn()
        .map_err(|_e| { relay_failure(&store, &connection_id, Some(&server_name), "startup", "relay_startup", "spawn_failed", "unable to start MCP server"); "Unable to start MCP server".to_string() })?;
    let child_stdin = child.stdin.take()
        .ok_or_else(|| { relay_failure(&store, &connection_id, Some(&server_name), "transport", "relay_transport", "pipe_unavailable", "stdin unavailable"); "MCP server stdin unavailable".to_string() })?;
    let child_stdout = child.stdout.take()
        .ok_or_else(|| { relay_failure(&store, &connection_id, Some(&server_name), "transport", "relay_transport", "pipe_unavailable", "stdout unavailable"); "MCP server stdout unavailable".to_string() })?;
    // Drain stderr continuously so the server never blocks on a full pipe; keep
    // only the tail and record a fixed classification on an abnormal exit.
    let stderr_tail = Arc::new(std::sync::Mutex::new(Vec::new()));
    let child_stderr = child.stderr.take();
    child_stderr.map(|stderr| {
        let tail = Arc::clone(&stderr_tail);
        std::thread::spawn(move || drain_tail(stderr, tail))
    });

    let _ = telemetry_result(store.proxy_connection_open(&connection_id, &server_name, assignment_id), TelemetryOperation::ConnectionOpen);
    let mut tracker = CallTracker::new(connection_id.clone(), launch.server_name, store.clone());
    let (events, receiver) = mpsc::sync_channel(64);
    let (completed, completions) = mpsc::channel();
    let dropped_metadata = Arc::new(AtomicU8::new(0));
    let input_dropped = Arc::clone(&dropped_metadata);
    let output_dropped = Arc::clone(&dropped_metadata);
    let input_events = events.clone();
    let input_completed = completed.clone();
    let input_done = Arc::new(AtomicBool::new(false));
    let input_flag = Arc::clone(&input_done);
    let input_pump = std::thread::spawn(move || {
        let result = pump(io::stdin(), child_stdin, input_events, Direction::Client, input_dropped);
        input_flag.store(true, Ordering::Release);
        let _ = input_completed.send(PumpEvent::Input(result));
    });
    let output_pump = std::thread::spawn(move || {
        let result = pump(child_stdout, io::stdout(), events, Direction::Server, output_dropped);
        let _ = completed.send(PumpEvent::Output(result));
    });

    let mut transfer_failed = false;
    let mut termination = None;
    let mut output_closed_at = None;
    let mut last_touch = Instant::now();
    // The coordinator owns telemetry and polls process state; pumps only handle bytes.
    // Never join a stdin reader blocked on the parent's still-open input pipe.
    let exit = loop {
        for _ in 0..64 {
            let Ok((direction, frame)) = receiver.try_recv() else { break; };
            tracker.observe(direction, &frame);
        }
        match dropped_metadata.swap(0, Ordering::AcqRel) {
            flags if flags & 2 != 0 => tracker.observation_gap("observer_framing_invalid", true),
            flags if flags & 1 != 0 => tracker.observation_gap("observer_queue_overflow", false),
            _ => {}
        }
        if output_closed_at.is_some_and(|since: Instant| since.elapsed() >= Duration::from_millis(500)) {
            if child.kill().is_ok() && termination.is_none() {
                termination = Some(RelayTermination::OutputClosed);
            }
        }
        if last_touch.elapsed() >= Duration::from_secs(5) {
            let _ = telemetry_result(store.proxy_connection_touch(&connection_id), TelemetryOperation::ConnectionTouch);
            last_touch = Instant::now();
        }
        if let Some(status) = child.try_wait().map_err(|_| {
            relay_failure(&store, &connection_id, Some(&server_name), "wait", "relay_transport", "wait_failed", "child wait failed");
            "Unable to wait for MCP server".to_string()
        })? {
            break status;
        }
        match completions.recv_timeout(Duration::from_millis(50)) {
            Ok(PumpEvent::Input(Err(_))) | Ok(PumpEvent::Output(Err(_))) => {
                relay_failure(&store, &connection_id, Some(&server_name), "transport", "relay_transport", "pump_failed", "stdio transfer failed");
                transfer_failed = true;
                if child.kill().is_ok() {
                    termination = Some(RelayTermination::PumpFailure);
                }
            }
            Ok(PumpEvent::Output(Ok(()))) => {
                // Give a child which closed stdout time to exit cleanly.
                output_closed_at = Some(Instant::now());
            }
            Ok(PumpEvent::Input(Ok(()))) | Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                // Both transfer pumps have completed. The server may still be
                // flushing or exiting; let its own status decide the outcome.
            },
        }
    };
    // Process the final stdout frames after exit before marking requests disconnected.
    // Descendants can inherit the write handle, so waiting for EOF is bounded.
    let drain_deadline = Instant::now() + Duration::from_millis(750);
    while !output_pump.is_finished() && Instant::now() < drain_deadline {
        match receiver.recv_timeout(Duration::from_millis(25)) {
            Ok((direction, frame)) => tracker.observe(direction, &frame),
            Err(mpsc::RecvTimeoutError::Timeout | mpsc::RecvTimeoutError::Disconnected) => {}
        }
    }
    // Once the output pump finishes it has queued every observed frame.
    if output_pump.is_finished() && output_pump.join().is_err() {
        relay_failure(&store, &connection_id, Some(&server_name), "transport", "relay_transport", "pump_failed", "output pump failed");
        transfer_failed = true;
    }
    if input_done.load(Ordering::Acquire) && input_pump.join().is_err() {
        relay_failure(&store, &connection_id, Some(&server_name), "transport", "relay_transport", "pump_failed", "input pump failed");
        transfer_failed = true;
    }
    while let Ok((direction, frame)) = receiver.try_recv() {
        tracker.observe(direction, &frame);
    }
    match dropped_metadata.swap(0, Ordering::AcqRel) {
        flags if flags & 2 != 0 => tracker.observation_gap("observer_framing_invalid", true),
        flags if flags & 1 != 0 => tracker.observation_gap("observer_queue_overflow", false),
        _ => {}
    }
    tracker.disconnect();
    if !exit.success() {
        let reason = child_exit_reason(&exit, termination, stderr_classification(&stderr_tail));
        relay_failure(&store, &connection_id, Some(&server_name), "child", "relay_child_exit", "child_exit", &reason);
        return Err("MCP server connection ended unexpectedly".into());
    }
    if transfer_failed {
        Err("MCP server connection ended unexpectedly".into())
    } else {
        Ok(())
    }
}

/// Keep only the last `STDERR_TAIL` bytes of a stream; readers never block on it.
fn drain_tail<R: Read>(mut reader: R, tail: Arc<std::sync::Mutex<Vec<u8>>>) {
    let mut chunk = [0u8; 8192];
    loop {
        let count = match reader.read(&mut chunk) {
            Ok(0) | Err(_) => return,
            Ok(count) => count,
        };
        let Ok(mut tail) = tail.lock() else { return; };
        tail.extend_from_slice(&chunk[..count]);
        let excess = tail.len().saturating_sub(STDERR_TAIL);
        if excess > 0 {
            tail.drain(..excess);
        }
    }
}

#[derive(Clone, Copy)]
enum RelayTermination { PumpFailure, OutputClosed }

fn child_exit_reason(status: &std::process::ExitStatus, termination: Option<RelayTermination>, stderr: Option<&str>) -> String {
    let cause = match termination {
        Some(RelayTermination::PumpFailure) => "MCP server terminated by relay after stdio transfer failure",
        Some(RelayTermination::OutputClosed) => "MCP server terminated by relay after stdout closed",
        None => "MCP server exited with error",
    };
    let code = status.code().map(|code| code.to_string()).unwrap_or_else(|| "unavailable".into());
    let mut reason = format!("{cause}; exit code: {code}");
    if let Some(classification) = stderr {
        reason.push_str("; ");
        reason.push_str(classification);
    }
    reason
}

/// Classify the last non-empty line without returning any upstream text. Even
/// unrecognized credential formats and protocol payloads remain private.
fn stderr_classification(tail: &Arc<std::sync::Mutex<Vec<u8>>>) -> Option<&'static str> {
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

fn control_settings() -> Result<Option<(SocketAddr, String)>, String> {
    let address = std::env::var(CONTROL_ADDR);
    let nonce = std::env::var(CONTROL_NONCE);
    match (address, nonce) {
        (Err(std::env::VarError::NotPresent), Err(std::env::VarError::NotPresent)) => Ok(None),
        (Ok(address), Ok(nonce)) => {
            let address: SocketAddr = address.parse()
                .map_err(|_| "Invalid MCP relay control address")?;
            if !address.ip().is_loopback() || address.port() == 0 {
                return Err("MCP relay control address must be loopback".into());
            }
            if nonce.is_empty() || nonce.len() > MAX_CONTROL_NONCE
                || !nonce.bytes().all(|byte| byte.is_ascii_graphic())
            {
                return Err("Invalid MCP relay control nonce".into());
            }
            Ok(Some((address, nonce)))
        }
        _ => Err("Incomplete MCP relay control settings".into()),
    }
}

fn startup_handshake(address: SocketAddr, nonce: &str) -> Result<(), String> {
    let mut control = TcpStream::connect_timeout(&address, CONTROL_TIMEOUT)
        .map_err(|_| "Unable to connect to MCP relay control")?;
    control.set_read_timeout(Some(CONTROL_TIMEOUT))
        .map_err(|_| "Unable to set MCP relay control timeout")?;
    control.set_write_timeout(Some(CONTROL_TIMEOUT))
        .map_err(|_| "Unable to set MCP relay control timeout")?;
    control.write_all(nonce.as_bytes())
        .and_then(|_| control.write_all(b"\nREADY\n"))
        .map_err(|_| "Unable to signal MCP relay readiness")?;
    let mut go = [0; 3];
    control.read_exact(&mut go).map_err(|_| "Unable to receive MCP relay GO")?;
    if go != *b"GO\n" { return Err("Invalid MCP relay GO".into()); }
    Ok(())
}

#[cfg(windows)]
pub(crate) fn child_command(program: &str, args: &[String]) -> Result<Command, String> {
    use std::os::windows::process::CommandExt;
    use std::path::{Path, PathBuf};

    let mut executable = PathBuf::from(program);
    // CreateProcess does not resolve npm's .cmd shims from a bare `npx`.
    if executable.extension().is_none() {
        let names = [format!("{program}.exe"), format!("{program}.cmd"), format!("{program}.bat")];
        if executable.components().count() > 1 {
            if let Some(found) = names.iter().map(PathBuf::from).find(|path| path.is_file()) {
                executable = found;
            }
        } else if let Some(paths) = std::env::var_os("PATH") {
            'search: for directory in std::env::split_paths(&paths) {
                for name in &names {
                    let candidate = directory.join(name);
                    if candidate.is_file() {
                        executable = candidate;
                        break 'search;
                    }
                }
            }
        }
    } else if executable.components().count() == 1 && !executable.is_file() {
        if let Some(paths) = std::env::var_os("PATH") {
            if let Some(found) = std::env::split_paths(&paths)
                .map(|directory| directory.join(program)).find(|candidate| candidate.is_file()) {
                executable = found;
            }
        }
    }
    let batch = executable.extension().and_then(|ext| ext.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("cmd") || ext.eq_ignore_ascii_case("bat"));
    if !batch {
        let mut command = Command::new(executable);
        command.args(args);
        return Ok(command);
    }
    // cmd.exe parses its /c string as shell syntax. Reject metacharacters and
    // expansions in all user-configured pieces instead of interpolating them.
    fn quoted(value: &str) -> Result<String, String> {
        if value.chars().any(|ch| matches!(ch, '"' | '&' | '|' | '<' | '>' | '^' | '%' | '!' | '\r' | '\n' | '\0')) {
            return Err("Unsafe MCP batch command or argument".into());
        }
        Ok(format!("\"{value}\""))
    }
    let mut line = format!("\"{}", quoted(&executable.to_string_lossy())?);
    for arg in args {
        line.push(' ');
        line.push_str(&quoted(arg)?);
    }
    line.push('"');
    let system_root = std::env::var_os("SystemRoot").ok_or("Windows system root unavailable")?;
    let cmd_path = Path::new(&system_root).join("System32").join("cmd.exe");
    if !cmd_path.is_file() { return Err("Windows command processor unavailable".into()); }
    let mut command = Command::new(cmd_path);
    command.args(["/d", "/s", "/c"]);
    command.raw_arg(line);
    Ok(command)
}

#[cfg(not(windows))]
pub(crate) fn child_command(program: &str, args: &[String]) -> Result<Command, String> {
    let mut command = Command::new(program);
    command.args(args);
    Ok(command)
}

#[derive(Clone, Copy)]
enum Direction { Client, Server }
enum PumpEvent { Input(io::Result<()>), Output(io::Result<()>) }

fn pump<R: Read, W: Write>(mut source: R, mut target: W, observations: mpsc::SyncSender<(Direction, Vec<u8>)>, direction: Direction, dropped_metadata: Arc<AtomicU8>) -> io::Result<()> {
    let mut observer = FrameObserver::default();
    let mut bytes = [0u8; 8192];
    loop {
        let count = source.read(&mut bytes)?;
        if count == 0 { return Ok(()); }
        // Queue client metadata before delivery, so a fast response cannot
        // overtake its request. Database work stays on the coordinator thread.
        if matches!(direction, Direction::Client) {
            observer.feed(&bytes[..count], |frame| {
                if frame == b"!" && matches!(direction, Direction::Server) { dropped_metadata.fetch_or(2, Ordering::Release); }
                if observations.try_send((direction, frame.to_vec())).is_err() { dropped_metadata.fetch_or(1, Ordering::Release); }
            });
        }
        target.write_all(&bytes[..count])?;
        target.flush()?;
        // For server messages, forward before observing. A full observer queue
        // only drops metadata and never changes the forwarded bytes.
        if matches!(direction, Direction::Server) {
            observer.feed(&bytes[..count], |frame| {
                if frame == b"!" && matches!(direction, Direction::Server) { dropped_metadata.fetch_or(2, Ordering::Release); }
                if observations.try_send((direction, frame.to_vec())).is_err() { dropped_metadata.fetch_or(1, Ordering::Release); }
            });
        }
    }
}

/// A bounded observation projection. Large JSON strings become empty objects in this copy;
/// ids, result.isError and error.code remain available regardless of payload order.
/// The pump still forwards the original bytes without modification.
#[derive(Default)]
struct FrameObserver {
    state: FrameState,
    buffer: Vec<u8>,
    in_string: bool,
    escaped: bool,
    unicode_remaining: u8,
    string_start: usize,
    discard_string: bool,
    invalid_string: bool,
    utf8_remaining: u8,
    utf8_min: u8,
    utf8_max: u8,
    unicode_value: u16,
    high_surrogate: bool,
}

#[derive(Default, Clone, Copy)]
enum FrameState { #[default] Start, Line, Headers, Body(usize), DiscardLine, DiscardBody(usize), Disabled }

impl FrameObserver {
    fn reset_projection(&mut self) {
        self.buffer.clear();
        self.in_string = false;
        self.escaped = false;
        self.unicode_remaining = 0;
        self.discard_string = false;
        self.invalid_string = false;
        self.utf8_remaining = 0;
        self.high_surrogate = false;
    }

    fn project(&mut self, byte: u8) -> bool {
        if self.in_string {
            if self.utf8_remaining > 0 {
                if !(self.utf8_min..=self.utf8_max).contains(&byte) { self.invalid_string = true; }
                self.utf8_remaining -= 1;
                self.utf8_min = 0x80; self.utf8_max = 0xbf;
            } else if byte >= 0x80 {
                let (remaining, min, max) = match byte {
                    0xc2..=0xdf => (1, 0x80, 0xbf), 0xe0 => (2, 0xa0, 0xbf),
                    0xe1..=0xec | 0xee..=0xef => (2, 0x80, 0xbf), 0xed => (2, 0x80, 0x9f),
                    0xf0 => (3, 0x90, 0xbf), 0xf1..=0xf3 => (3, 0x80, 0xbf), 0xf4 => (3, 0x80, 0x8f),
                    _ => { self.invalid_string = true; (0, 0x80, 0xbf) }
                };
                self.utf8_remaining = remaining; self.utf8_min = min; self.utf8_max = max;
            }
            if self.high_surrogate && self.unicode_remaining == 0
                && ((!self.escaped && byte != b'\\') || (self.escaped && byte != b'u')) {
                self.invalid_string = true;
            }
            let closes = !self.escaped && self.unicode_remaining == 0 && byte == b'"';
            if self.unicode_remaining > 0 {
                if !byte.is_ascii_hexdigit() { self.invalid_string = true; }
                self.unicode_value = self.unicode_value.wrapping_mul(16).wrapping_add((byte as char).to_digit(16).unwrap_or(0) as u16);
                self.unicode_remaining -= 1;
                if self.unicode_remaining == 0 {
                    if self.high_surrogate {
                        if !(0xdc00..=0xdfff).contains(&self.unicode_value) { self.invalid_string = true; }
                        self.high_surrogate = false;
                    } else if (0xd800..=0xdbff).contains(&self.unicode_value) { self.high_surrogate = true; }
                    else if (0xdc00..=0xdfff).contains(&self.unicode_value) { self.invalid_string = true; }
                }
            } else if self.escaped {
                if byte == b'u' { self.unicode_remaining = 4; self.unicode_value = 0; }
                else if !matches!(byte, b'"' | b'\\' | b'/' | b'b' | b'f' | b'n' | b'r' | b't') { self.invalid_string = true; }
                self.escaped = false;
            } else if byte == b'\\' { self.escaped = true; }
            else if byte < 0x20 { self.invalid_string = true; }
            if closes {
                self.in_string = false;
                if self.discard_string { self.buffer.extend_from_slice(b"{}"); }
                else { self.buffer.push(byte); }
                self.discard_string = false;
            } else if !self.discard_string {
                self.buffer.push(byte);
                if self.buffer.len() - self.string_start > MAX_OBSERVED_STRING {
                    self.buffer.truncate(self.string_start);
                    self.discard_string = true;
                }
            }
        } else {
            if byte == b'"' {
                self.in_string = true;
                self.string_start = self.buffer.len();
            }
            self.buffer.push(byte);
        }
        self.buffer.len() <= MAX_FRAME
    }

    fn finish(&mut self, on_frame: &mut impl FnMut(&[u8])) {
        if self.in_string || self.invalid_string || self.utf8_remaining > 0 || self.high_surrogate { on_frame(b"?"); }
        else { on_frame(&self.buffer); }
        self.reset_projection();
        self.state = FrameState::Start;
    }

    fn feed(&mut self, bytes: &[u8], mut on_frame: impl FnMut(&[u8])) {
        for &byte in bytes {
            if matches!(self.state, FrameState::Disabled) { break; }
            if matches!(self.state, FrameState::Start) {
                if byte.is_ascii_whitespace() { continue; }
                self.state = if byte.is_ascii_alphabetic() { FrameState::Headers } else { FrameState::Line };
            }
            match self.state {
                FrameState::Headers => {
                    self.buffer.push(byte);
                    if self.buffer.len() > MAX_HEADERS {
                        self.reset_projection(); self.state = FrameState::Disabled; on_frame(b"!");
                    } else if self.buffer.ends_with(b"\r\n\r\n") || self.buffer.ends_with(b"\n\n") {
                        match content_length(&self.buffer) {
                            Some(0) => self.finish(&mut on_frame),
                            Some(length) => { self.reset_projection(); self.state = FrameState::Body(length); }
                            None => { self.reset_projection(); self.state = FrameState::Disabled; on_frame(b"!"); }
                        }
                    }
                }
                FrameState::Line => {
                    if byte == b'\n' { self.finish(&mut on_frame); }
                    else if !self.project(byte) {
                        self.reset_projection(); self.state = FrameState::DiscardLine; on_frame(&[]);
                    }
                }
                FrameState::Body(remaining) => {
                    if !self.project(byte) {
                        self.reset_projection();
                        self.state = if remaining == 1 { FrameState::Start } else { FrameState::DiscardBody(remaining - 1) };
                        on_frame(&[]);
                    } else if remaining == 1 { self.finish(&mut on_frame); }
                    else { self.state = FrameState::Body(remaining - 1); }
                }
                FrameState::DiscardLine if byte == b'\n' => self.state = FrameState::Start,
                FrameState::DiscardBody(1) => self.state = FrameState::Start,
                FrameState::DiscardBody(remaining) => self.state = FrameState::DiscardBody(remaining - 1),
                _ => {}
            }
        }
    }
}

fn content_length(headers: &[u8]) -> Option<usize> {
    let text = std::str::from_utf8(headers).ok()?;
    let mut length = None;
    for line in text.lines().map(str::trim).filter(|line| !line.is_empty()) {
        let (key, value) = line.split_once(':')?;
        if key.trim().eq_ignore_ascii_case("content-length") {
            if length.is_some() || value.trim().is_empty() || !value.trim().bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
            length = value.trim().parse().ok();
        }
    }
    length
}

struct CallTracker {
    connection_id: String,
    server: String,
    sequence: u64,
    pending: HashMap<String, Option<String>>,
    store: TelemetryStore,
    server_observation_broken: bool,
}

impl CallTracker {
    fn new(connection_id: String, server: String, store: TelemetryStore) -> Self {
        Self { connection_id, server, sequence: 0, pending: HashMap::new(), store, server_observation_broken: false }
    }

    fn observe(&mut self, direction: Direction, frame: &[u8]) {
        if frame == b"?" { self.observation_gap("observer_frame_invalid", false); return; }
        if frame.is_empty() || frame == b"!" {
            self.observation_gap(if frame == b"!" { "observer_framing_invalid" } else { "observer_metadata_limit" },
                frame == b"!" && matches!(direction, Direction::Server));
            return;
        }
        let Ok(message) = serde_json::from_slice::<Value>(frame) else {
            self.observation_gap("observer_frame_invalid", false);
            return;
        };
        if !message.is_object() { self.observation_gap("observer_frame_invalid", false); return; }
        let Some(id) = message.get("id").filter(|id| matches!(id, Value::String(_) | Value::Number(_)))
            .map(Value::to_string).filter(|id| id.len() <= 128) else { return; };
        match direction {
            Direction::Client => {
                if message.get("method").and_then(Value::as_str) != Some("tools/call") { return; }
                // A repeated JSON-RPC id has no unambiguous response correlation.
                if let Some(previous) = self.pending.get_mut(&id) {
                    if let Some(key) = previous.take() {
                        let _ = telemetry_result(self.store.mark_proxy_call_unknown(&key, "request_id_reused"), TelemetryOperation::MarkUnknown);
                    }
                    return;
                }
                if self.pending.len() >= MAX_PENDING { return; }
                let Some(tool) = message.get("params").and_then(|p| p.get("name"))
                    .and_then(Value::as_str).filter(|name| !name.is_empty() && name.len() <= 256 && !name.chars().any(char::is_control))
                else { return; };
                let Some(next) = self.sequence.checked_add(1) else { return; };
                self.sequence = next;
                let key = format!("{}:{next}", self.connection_id);
                if telemetry_result(self.store.record_proxy_call(&key, &self.server, tool), TelemetryOperation::RecordCall).is_ok() {
                    if self.server_observation_broken {
                        let _ = telemetry_result(self.store.mark_proxy_call_unknown(&key, "observer_framing_invalid"), TelemetryOperation::MarkUnknown);
                    }
                    self.pending.insert(id, Some(key));
                }
            }
            Direction::Server => {
                if message.get("method").is_some() { return; }
                let error = message.get("error").is_some_and(|error| !error.is_null());
                // Numeric JSON-RPC error codes are stable metadata; the message
                // text is not copied into storage.
                let error_code = message.get("error").and_then(|error| error.get("code"))
                    .and_then(Value::as_i64);
                let Some(result) = message.get("result") else {
                    if !error { return; }
                    let Some(Some(key)) = self.pending.remove(&id) else { return; };
                    let _ = telemetry_result(self.store.finish_proxy_call(&key, "failed", Some("mcp_error"),
                        Some("response"), Some("rpc_error"), error_code), TelemetryOperation::FinishCall);
                    return;
                };
                let Some(Some(key)) = self.pending.remove(&id) else { return; };
                let tool_error = !error && result.get("isError").and_then(Value::as_bool) == Some(true);
                let failure = error || tool_error;
                let (status, category) = if failure { ("failed", Some("mcp_error")) } else { ("success", None) };
                let kind = if tool_error { Some("tool_error") } else if failure { Some("rpc_error") } else { None };
                let _ = telemetry_result(self.store.finish_proxy_call(&key, status, category,
                    if failure { Some("response") } else { None }, kind, error_code), TelemetryOperation::FinishCall);
            }
        }
    }

    fn observation_gap(&mut self, reason: &str, permanent: bool) {
        self.server_observation_broken |= permanent;
        for key in self.pending.values().flatten() {
            let _ = telemetry_result(self.store.mark_proxy_call_unknown(key, reason), TelemetryOperation::MarkUnknown);
        }
    }

    fn disconnect(&mut self) {
        for (_, key) in self.pending.drain() {
            if let Some(key) = key {
                let _ = telemetry_result(self.store.mark_proxy_call_unknown(&key, "connection_closed"), TelemetryOperation::MarkUnknown);
            }
        }
    }
}

impl Drop for CallTracker {
    fn drop(&mut self) {
        self.disconnect();
        let _ = telemetry_result(self.store.proxy_connection_close(&self.connection_id), TelemetryOperation::ConnectionClose);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn observes_split_json_lines_metadata() {
        let mut observer = FrameObserver::default();
        let mut frames = Vec::new();
        observer.feed(b"{\"method\":\"tools/call\"}\n{\"id\":", |f| frames.push(f.to_vec()));
        observer.feed(b"1}\r\n", |f| frames.push(f.to_vec()));
        let messages: Vec<Value> = frames.iter().map(|frame| serde_json::from_slice(frame).unwrap()).collect();
        assert_eq!(messages, vec![serde_json::json!({"method":"tools/call"}), serde_json::json!({"id":1})]);
    }

    #[test]
    fn observes_content_length_across_chunks_and_ignores_newlines_in_body() {
        let body = b"{\"id\":1,\n\"result\":{}}";
        let header = format!("Content-Length: {}\r\nContent-Type: application/json\r\n\r\n", body.len());
        let mut observer = FrameObserver::default();
        let mut frames = Vec::new();
        for byte in header.bytes().chain(body.iter().copied()) {
            observer.feed(&[byte], |frame| frames.push(frame.to_vec()));
        }
        assert_eq!(frames, vec![body.to_vec()]);
    }

    #[test]
    fn large_images_preserve_response_metadata_after_the_payload_and_forward_original_bytes() {
        let image = "x".repeat(MAX_FRAME + 128);
        let bytes = format!("{{\"result\":{{\"content\":[{{\"type\":\"image\",\"data\":\"{image}\"}}],\"isError\":true}},\"id\":\"after-image\"}}\n{{\"id\":2,\"result\":{{}}}}\n").into_bytes();
        let mut forwarded = Vec::new();
        let (sender, receiver) = mpsc::sync_channel(64);
        let health = Arc::new(AtomicU8::new(0));
        pump(io::Cursor::new(&bytes), &mut forwarded, sender, Direction::Server, Arc::clone(&health)).unwrap();
        assert_eq!(forwarded, bytes);
        assert_eq!(health.load(Ordering::Acquire), 0);
        let frames: Vec<_> = receiver.try_iter().collect();
        assert_eq!(frames.len(), 2);
        let first: Value = serde_json::from_slice(&frames[0].1).unwrap();
        assert_eq!(first["id"], "after-image");
        assert_eq!(first["result"]["isError"], true);
        assert_eq!(first["result"]["content"][0]["data"], serde_json::json!({}));
        assert!(frames[0].1.len() < 1024);
        assert_eq!(serde_json::from_slice::<Value>(&frames[1].1).unwrap()["id"], 2);
    }

    #[test]
    fn metadata_queue_loss_does_not_block_original_byte_forwarding() {
        let bytes = b"{\"id\":1,\"result\":{}}\n";
        let mut forwarded = Vec::new();
        let (sender, receiver) = mpsc::sync_channel(0);
        let health = Arc::new(AtomicU8::new(0));
        pump(io::Cursor::new(bytes), &mut forwarded, sender, Direction::Server, Arc::clone(&health)).unwrap();
        assert_eq!(forwarded.as_slice(), bytes.as_slice());
        assert_eq!(health.load(Ordering::Acquire) & 1, 1);
        assert!(receiver.try_iter().next().is_none());
    }

    #[test]
    fn projected_error_keeps_non_null_presence() {
        let response = format!("{{\"id\":1,\"error\":\"{}\"}}\n", "x".repeat(MAX_FRAME + 128));
        let mut observer = FrameObserver::default();
        let mut frames = Vec::new();
        observer.feed(response.as_bytes(), |frame| frames.push(frame.to_vec()));
        let message: Value = serde_json::from_slice(&frames[0]).unwrap();
        assert!(!message["error"].is_null());
    }

    #[test]
    fn large_content_length_response_recovers_for_the_next_frame() {
        let body = format!("{{\"result\":{{\"content\":[{{\"data\":\"{}\"}}]}},\"id\":42}}", "x".repeat(MAX_FRAME + 128));
        let wire = format!("Content-Length: {}\r\n\r\n{}{{\"id\":43,\"result\":{{}}}}\n", body.len(), body);
        let mut observer = FrameObserver::default();
        let mut frames = Vec::new();
        for chunk in wire.as_bytes().chunks(701) { observer.feed(chunk, |frame| frames.push(frame.to_vec())); }
        assert_eq!(frames.len(), 2);
        assert_eq!(serde_json::from_slice::<Value>(&frames[0]).unwrap()["id"], 42);
        assert_eq!(serde_json::from_slice::<Value>(&frames[1]).unwrap()["id"], 43);
    }

    #[test]
    fn projection_limit_marks_one_gap_and_resumes_at_the_next_line() {
        let wire = format!("{{\"result\":[{}0],\"id\":1}}\n{{\"id\":2,\"result\":{{}}}}\n", "0,".repeat(MAX_FRAME / 2 + 100));
        let mut observer = FrameObserver::default();
        let mut frames = Vec::new();
        for chunk in wire.as_bytes().chunks(8192) { observer.feed(chunk, |frame| frames.push(frame.to_vec())); }
        assert_eq!(frames.len(), 2);
        assert!(frames[0].is_empty());
        assert_eq!(serde_json::from_slice::<Value>(&frames[1]).unwrap()["id"], 2);
    }

    #[test]
    fn discarded_strings_still_validate_utf8_and_unicode_escapes() {
        let mut wire = format!("{{\"id\":1,\"result\":\"{}", "x".repeat(MAX_OBSERVED_STRING + 1)).into_bytes();
        wire.extend_from_slice(&[0xff]);
        wire.extend_from_slice(b"\"}\n");
        let mut observer = FrameObserver::default();
        let mut frames = Vec::new();
        observer.feed(&wire, |frame| frames.push(frame.to_vec()));
        assert_eq!(frames, vec![b"?".to_vec()]);
        let valid = format!("{{\"id\":2,\"result\":\"{}\\uD83D\\uDE00\"}}\n", "x".repeat(MAX_OBSERVED_STRING + 1));
        observer.feed(valid.as_bytes(), |frame| frames.push(frame.to_vec()));
        assert_eq!(serde_json::from_slice::<Value>(&frames[1]).unwrap()["id"], 2);
    }

    #[test]
    fn stderr_tail_is_bounded_and_classifies_the_last_line() {
        let tail = Arc::new(std::sync::Mutex::new(Vec::new()));
        let noise = format!("{}\nTraceback: upstream closed\n\n", "x".repeat(STDERR_TAIL + 512));
        drain_tail(io::Cursor::new(noise.into_bytes()), Arc::clone(&tail));
        assert_eq!(tail.lock().unwrap().len(), STDERR_TAIL);
        assert_eq!(stderr_classification(&tail), Some("stderr captured"));
    }

    #[test]
    fn stderr_classification_tolerates_non_utf8_and_empty_streams() {
        let tail = Arc::new(std::sync::Mutex::new(Vec::new()));
        assert_eq!(stderr_classification(&tail), None);
        drain_tail(io::Cursor::new([b'a', 0xff, b'\n']), Arc::clone(&tail));
        assert_eq!(stderr_classification(&tail), Some("stderr captured"));
    }

    #[test]
    fn stderr_classifications_never_include_credentials_or_payloads() {
        for (line, expected) in [
            ("Authorization: Bearer secret-token", "stderr captured"),
            ("API_KEY=secret-key", "stderr captured"),
            ("{\"result\":\"private payload\"}", "stderr captured"),
            ("Daemon stopped: Authorization: Bearer secret-token", "stderr: daemon_stopped"),
            ("[bridge] daemon_stopped secret-token", "stderr: daemon_stopped"),
            ("FETCH FAILED https://user:secret@example.test?api_key=secret", "stderr: fetch failed"),
        ] {
            let tail = Arc::new(std::sync::Mutex::new(line.as_bytes().to_vec()));
            assert_eq!(stderr_classification(&tail), Some(expected));
        }
    }

    #[test]
    fn telemetry_warnings_are_static_and_once_per_operation() {
        let reported = AtomicU8::new(0);
        for operation in [
            TelemetryOperation::OpenStore, TelemetryOperation::SystemFailure,
            TelemetryOperation::ConnectionOpen, TelemetryOperation::ConnectionTouch,
            TelemetryOperation::RecordCall, TelemetryOperation::MarkUnknown,
            TelemetryOperation::FinishCall, TelemetryOperation::ConnectionClose,
        ] {
            assert_eq!(telemetry_failure_label(false, operation, &reported), None);
            assert_eq!(telemetry_failure_label(true, operation, &reported), Some(operation.label()));
            assert_eq!(telemetry_failure_label(true, operation, &reported), None);
        }
        let result: Result<(), &str> = Err("private database error");
        assert_eq!(telemetry_result(result, TelemetryOperation::RecordCall), result);
    }

    #[test]
    fn child_exit_diagnostics_include_numeric_status_and_relay_cause() {
        #[cfg(windows)]
        use std::os::windows::process::ExitStatusExt;
        #[cfg(unix)]
        use std::os::unix::process::ExitStatusExt;
        #[cfg(windows)]
        let status = std::process::ExitStatus::from_raw(7);
        #[cfg(unix)]
        let status = std::process::ExitStatus::from_raw(7 << 8);
        assert_eq!(child_exit_reason(&status, None, None), "MCP server exited with error; exit code: 7");
        assert_eq!(child_exit_reason(&status, Some(RelayTermination::PumpFailure), Some("stderr captured")),
            "MCP server terminated by relay after stdio transfer failure; exit code: 7; stderr captured");
        assert_eq!(child_exit_reason(&status, Some(RelayTermination::OutputClosed), None),
            "MCP server terminated by relay after stdout closed; exit code: 7");
    }

    #[cfg(unix)]
    #[test]
    fn child_exit_diagnostics_handle_signal_without_numeric_exit_code() {
        use std::os::unix::process::ExitStatusExt;
        let status = std::process::ExitStatus::from_raw(9);
        assert_eq!(child_exit_reason(&status, None, None), "MCP server exited with error; exit code: unavailable");
    }

    #[test]
    fn rejects_invalid_headers_but_accepts_large_declared_bodies() {
        assert_eq!(content_length(b"CONTENT-LENGTH: 12\r\n\r\n"), Some(12));
        assert_eq!(content_length(b"Content-Length: 1\r\ncontent-length: 2\r\n\r\n"), None);
        assert_eq!(content_length(b"Content-Length: 1x\r\n\r\n"), None);
        let mut observer = FrameObserver::default();
        let mut frames = Vec::new();
        observer.feed(format!("Content-Length: {}\r\n\r\n", MAX_FRAME + 1).as_bytes(), |frame| frames.push(frame.to_vec()));
        assert!(frames.is_empty());
        assert!(matches!(observer.state, FrameState::Body(_)));
    }
}
