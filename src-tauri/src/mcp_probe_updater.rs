//! Native probe maintenance. Publication leaves client bindings untouched;
//! confirmed parameter-error recovery may restore an exact managed proxy.
use crate::mcp_probe_protocol::{digest_from_path, ProbeGeneration, ProbeInfo};
use std::collections::BTreeMap;
use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::{mpsc, Mutex, OnceLock};
use std::time::{Duration, Instant};
use tauri::Emitter;

const CHECK_INTERVAL: Duration = Duration::from_secs(30);
const INFO_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_INFO_BYTES: u64 = 8192;

#[derive(Clone, Default, PartialEq, Eq)]
pub(crate) struct UpdateReport {
    pub errors: BTreeMap<String, String>,
    pub published: usize,
    pub configuration_stopped: usize,
    pub transitions_recovered: usize,
}

fn report() -> &'static Mutex<UpdateReport> {
    static REPORT: OnceLock<Mutex<UpdateReport>> = OnceLock::new();
    REPORT.get_or_init(|| Mutex::new(UpdateReport::default()))
}

pub(crate) fn last_error(assignment: &str) -> Option<String> {
    let report = report().lock().ok()?;
    report.errors.get(assignment).or_else(|| report.errors.get("*")).cloned()
}

fn parse_info(bytes: &[u8]) -> Result<ProbeInfo, String> {
    if bytes.len() as u64 > MAX_INFO_BYTES { return Err("probe_info_too_large".into()); }
    let info: ProbeInfo = serde_json::from_slice(bytes).map_err(|_| "probe_info_invalid")?;
    if !info.compatible() { return Err("probe_abi_incompatible".into()); }
    Ok(info)
}

fn probe_info(path: &Path) -> Result<ProbeInfo, String> {
    let mut command = Command::new(path);
    command.arg("--probe-info").stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null());
    crate::process_util::no_window(&mut command);
    let mut child = command.spawn().map_err(|_| "probe_info_start_failed")?;
    let stdout = child.stdout.take().ok_or("probe_info_pipe_failed")?;
    let (sender, receiver) = mpsc::sync_channel(1);
    std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let result = stdout.take(MAX_INFO_BYTES + 1).read_to_end(&mut bytes).map(|_| bytes);
        let _ = sender.send(result);
    });
    let deadline = Instant::now() + INFO_TIMEOUT;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                if !status.success() { return Err("probe_info_rejected".into()); }
                let bytes = receiver.recv_timeout(deadline.saturating_duration_since(Instant::now()))
                    .map_err(|_| "probe_info_timeout")?.map_err(|_| "probe_info_read_failed")?;
                return parse_info(&bytes);
            }
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(10)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return Err("probe_info_timeout".into());
            }
        }
    }
}

pub(crate) fn candidate() -> Result<ProbeGeneration, String> {
    static CACHE: OnceLock<Mutex<Option<ProbeGeneration>>> = OnceLock::new();
    // Installation verifies both source and immutable installed bytes on every
    // check; cached metadata never bypasses payload identity verification.
    let command = crate::mcp_launcher_installation::relay_executable()?;
    let cache = CACHE.get_or_init(|| Mutex::new(None));
    if let Some(value) = cache.lock().map_err(|_| "probe_cache_unavailable")?.as_ref()
        .filter(|value| value.command == command).cloned() { return Ok(value); }
    let info = probe_info(Path::new(&command))?;
    let digest = digest_from_path(Path::new(&command)).map_err(str::to_string)?;
    let generation = ProbeGeneration { command, digest, info, published_at_ms: 0, retire_after_ms: None };
    *cache.lock().map_err(|_| "probe_cache_unavailable")? = Some(generation.clone());
    Ok(generation)
}

#[derive(serde::Serialize, serde::Deserialize)]
struct UpdateOwner {
    protocol: u32,
    pid: u32,
    started: u64,
    seen_at_ms: u64,
}

fn owner_is_current(marker: &UpdateOwner, pid: u32, started: u64, now: u64) -> bool {
    marker.protocol == 1 && marker.pid == pid && marker.started == started
        && marker.seen_at_ms <= now && now - marker.seen_at_ms < 90_000
}

fn another_gui_writer() -> Result<bool, String> {
    let system = sysinfo::System::new_all();
    let pid = sysinfo::Pid::from_u32(std::process::id());
    let own = system.process(pid).ok_or("probe_owner_unavailable")?;
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)
        .ok().and_then(|time| u64::try_from(time.as_millis()).ok()).ok_or("probe_clock_unavailable")?;
    let owners = dirs_next::config_dir().ok_or("probe_owner_unavailable")?
        .join("agent-manager").join("mcp-observation").join("update-owners");
    std::fs::create_dir_all(&owners).map_err(|_| "probe_owner_unavailable")?;
    let owner = UpdateOwner { protocol: 1, pid: std::process::id(), started: own.start_time(), seen_at_ms: now };
    let mut temp = tempfile::NamedTempFile::new_in(&owners).map_err(|_| "probe_owner_unavailable")?;
    serde_json::to_writer(&mut temp, &owner).map_err(|_| "probe_owner_unavailable")?;
    temp.as_file().sync_all().map_err(|_| "probe_owner_unavailable")?;
    temp.persist(owners.join(format!("{}.json", owner.pid))).map_err(|_| "probe_owner_unavailable")?;
    Ok(system.processes().values().any(|process| {
        if process.pid() == pid || process.name() != own.name()
            || process.cmd().iter().any(|arg| matches!(arg.to_str(), Some("--mcp-relay" | "--mcp-memory"))) {
            return false;
        }
        if let (Some(left), Some(right)) = (own.user_id(), process.user_id()) {
            if left != right { return false; }
        }
        let marker = std::fs::read(owners.join(format!("{}.json", process.pid().as_u32())))
            .ok().and_then(|bytes| serde_json::from_slice::<UpdateOwner>(&bytes).ok());
        !marker.is_some_and(|marker| owner_is_current(&marker, process.pid().as_u32(), process.start_time(), now))
    }))
}

fn refresh_once() -> Result<UpdateReport, String> {
    // Retry only durable evidence produced by a failed startup, never infer a
    // configuration problem from a status poll or a missing heartbeat.
    let mut report = crate::mcp_registry::retry_parameter_configuration()?;
    match crate::mcp_registry::resume_observation_transitions() {
        Ok(recovered) => {
            report.transitions_recovered = recovered.transitions_recovered;
            report.errors.extend(recovered.errors);
        }
        Err(_) => { report.errors.insert("transition-recovery".into(), "observation_transition_recovery_pending".into()); },
    }
    if !crate::mcp_registry::has_probe_observations()? { return Ok(report); }
    if another_gui_writer()? {
        report.errors.insert("*".into(), "another_manager_writer_active".into());
        return Ok(report);
    }
    match candidate().and_then(|generation| crate::mcp_registry::publish_probe_candidate(&generation)) {
        Ok(published) => {
            report.published = published.published;
            report.errors.extend(published.errors);
        }
        Err(error) => { report.errors.insert("*".into(), error); }
    }
    let _ = crate::mcp_probe_connections::prune_stale();
    Ok(report)
}

pub(crate) fn start(app: tauri::AppHandle) {
    static STARTED: OnceLock<()> = OnceLock::new();
    if STARTED.set(()).is_err() { return; }
    tauri::async_runtime::spawn(async move {
        crate::startup::wait_for_frontend_ready().await;
        tokio::time::sleep(crate::startup::MAINTENANCE_GRACE).await;
        let mut timer = tokio::time::interval(CHECK_INTERVAL);
        timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            timer.tick().await;
            let result = tauri::async_runtime::spawn_blocking(refresh_once).await;
            let next = match result {
                Ok(Ok(report)) => report,
                Ok(Err(reason)) => UpdateReport { errors: BTreeMap::from([("*".into(), reason)]), ..Default::default() },
                Err(_) => UpdateReport { errors: BTreeMap::from([("*".into(), "probe_update_worker_failed".into())]), ..Default::default() },
            };
            let changed = if let Ok(mut previous) = report().lock() {
                let changed = *previous != next;
                if changed { *previous = next; }
                changed
            } else { false };
            if changed { let _ = app.emit("mcp-probe-updated", ()); }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcp_probe_protocol::{LAUNCHER_ABI, PROBE_SCHEMA};

    #[test]
    fn peer_marker_requires_matching_process_start_and_fresh_supported_protocol() {
        let mut marker = UpdateOwner { protocol: 1, pid: 42, started: 100, seen_at_ms: 2000 };
        assert!(owner_is_current(&marker, 42, 100, 2500));
        assert!(!owner_is_current(&marker, 42, 101, 2500));
        assert!(!owner_is_current(&marker, 43, 100, 2500));
        assert!(!owner_is_current(&marker, 42, 100, 92_000));
        assert!(!owner_is_current(&marker, 42, 100, 1000));
        marker.protocol = 2;
        assert!(!owner_is_current(&marker, 42, 100, 2500));
    }

    #[test]
    fn info_rejects_placeholder_unstructured_wrong_abi_and_zero_build() {
        assert!(parse_info(b"expected --mcp-relay UUID").is_err());
        assert!(parse_info(&vec![b' '; MAX_INFO_BYTES as usize + 1]).is_err());
        let mut info = ProbeInfo { schema: PROBE_SCHEMA, launcher_abi: LAUNCHER_ABI,
            generation_resolution: 1, version: "1.2.3".into(), build_sequence: 10 };
        assert!(parse_info(&serde_json::to_vec(&info).unwrap()).is_ok());
        info.launcher_abi = 2;
        assert!(parse_info(&serde_json::to_vec(&info).unwrap()).is_err());
        info.launcher_abi = LAUNCHER_ABI; info.build_sequence = 0;
        assert!(parse_info(&serde_json::to_vec(&info).unwrap()).is_err());
    }
}
