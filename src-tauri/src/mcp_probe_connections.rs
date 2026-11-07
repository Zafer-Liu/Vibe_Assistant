//! Optional runtime telemetry; never changes bindings, catalog or publication.
use crate::mcp_probe_protocol::valid_digest;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::io::Write;
use std::path::{Path, PathBuf};

const LIVE_MS: u64 = 20_000;
const STALE_MS: u64 = 300_000;

#[derive(Serialize, Deserialize)]
struct Marker {
    assignment: String,
    connection: String,
    pid: u32,
    version: String,
    seen_at_ms: u64,
}

fn now_ms() -> Result<u64, String> {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)
        .ok().and_then(|time| u64::try_from(time.as_millis()).ok()).ok_or_else(|| "probe_clock_unavailable".into())
}

fn directory() -> Result<PathBuf, String> {
    dirs_next::config_dir().map(|path| path.join("agent-manager").join("mcp-observation").join("runtime-connections"))
        .ok_or_else(|| "probe_connection_directory_unavailable".into())
}

fn valid_assignment(value: &str) -> bool {
    uuid::Uuid::parse_str(value).ok().is_some_and(|id| id.to_string() == value)
}

pub(crate) struct ConnectionLease {
    path: PathBuf,
    marker: Marker,
}

impl ConnectionLease {
    pub(crate) fn new(assignment: &str, connection: &str, version: &str) -> Result<Self, String> {
        Self::new_at(&directory()?, assignment, connection, version, now_ms()?)
    }

    fn new_at(root: &Path, assignment: &str, connection: &str, version: &str, now: u64) -> Result<Self, String> {
        if !valid_assignment(assignment) || !valid_digest(connection) || !valid_digest(version) {
            return Err("probe_connection_identity_invalid".into());
        }
        let parent = root.join(assignment);
        std::fs::create_dir_all(&parent).map_err(|_| "probe_connection_directory_unavailable")?;
        if std::fs::symlink_metadata(&parent).map_err(|_| "probe_connection_directory_unavailable")?.file_type().is_symlink() {
            return Err("probe_connection_directory_invalid".into());
        }
        let lease = Self {
            path: parent.join(format!("{connection}.json")),
            marker: Marker { assignment: assignment.into(), connection: connection.into(),
                pid: std::process::id(), version: version.into(), seen_at_ms: now },
        };
        lease.write()?;
        Ok(lease)
    }

    fn write(&self) -> Result<(), String> {
        let parent = self.path.parent().ok_or("probe_connection_directory_invalid")?;
        let mut temporary = tempfile::NamedTempFile::new_in(parent).map_err(|_| "probe_connection_marker_unavailable")?;
        serde_json::to_writer(&mut temporary, &self.marker).map_err(|_| "probe_connection_marker_invalid")?;
        temporary.write_all(b"\n").map_err(|_| "probe_connection_marker_unavailable")?;
        // Heartbeats are disposable evidence, not a durable publication. An
        // atomic rename suffices; fsync on every heartbeat would delay tracking.
        temporary.persist(&self.path).map_err(|_| "probe_connection_marker_unavailable")?;
        Ok(())
    }

    pub(crate) fn touch(&mut self) -> Result<(), String> {
        self.marker.seen_at_ms = now_ms()?;
        self.write()
    }
}

impl Drop for ConnectionLease {
    fn drop(&mut self) {
        let own = std::fs::symlink_metadata(&self.path).ok().filter(|m| m.is_file() && !m.file_type().is_symlink())
            .and_then(|_| std::fs::read(&self.path).ok())
            .and_then(|bytes| serde_json::from_slice::<Marker>(&bytes).ok())
            .is_some_and(|marker| marker.connection == self.marker.connection && marker.pid == self.marker.pid);
        if own { let _ = std::fs::remove_file(&self.path); }
    }
}

pub(crate) fn current_connections(assignment: &str, version: &str) -> Result<HashSet<String>, String> {
    current_at(&directory()?, assignment, version, now_ms()?)
}

fn current_at(root: &Path, assignment: &str, version: &str, now: u64) -> Result<HashSet<String>, String> {
    if !valid_assignment(assignment) || !valid_digest(version) { return Err("probe_connection_identity_invalid".into()); }
    let files = match std::fs::read_dir(root.join(assignment)) {
        Ok(files) => files,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(HashSet::new()),
        Err(_) => return Err("probe_connection_markers_unavailable".into()),
    };
    let mut result = HashSet::new();
    for file in files {
        let file = file.map_err(|_| "probe_connection_markers_unavailable")?;
        let path = file.path();
        if path.extension().is_none_or(|extension| extension != "json") { continue; }
        if !file.file_type().map_err(|_| "probe_connection_markers_unavailable")?.is_file() {
            return Err("probe_connection_marker_invalid".into());
        }
        let bytes = match std::fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(_) => return Err("probe_connection_markers_unavailable".into()),
        };
        let marker: Marker = serde_json::from_slice(&bytes).map_err(|_| "probe_connection_marker_invalid")?;
        if marker.assignment != assignment || !valid_digest(&marker.connection) || !valid_digest(&marker.version)
            || path.file_stem().and_then(|value| value.to_str()) != Some(marker.connection.as_str()) {
            return Err("probe_connection_marker_invalid".into());
        }
        if marker.version == version && marker.seen_at_ms <= now && now - marker.seen_at_ms < LIVE_MS {
            result.insert(marker.connection);
        }
    }
    Ok(result)
}

pub(crate) fn prune_stale() -> Result<(), String> {
    let root = directory()?;
    let entries = match std::fs::read_dir(root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(_) => return Err("probe_connection_markers_unavailable".into()),
    };
    let now = now_ms()?;
    for entry in entries.flatten() {
        if !entry.file_type().ok().is_some_and(|kind| kind.is_dir()) { continue; }
        let assignment = entry.file_name().to_string_lossy().into_owned();
        if !valid_assignment(&assignment) { continue; }
        let Ok(files) = std::fs::read_dir(entry.path()) else { continue };
        for file in files.flatten() {
            if !file.file_type().ok().is_some_and(|kind| kind.is_file()) { continue; }
            let path = file.path();
            let Ok(bytes) = std::fs::read(&path) else { continue };
            let Ok(marker) = serde_json::from_slice::<Marker>(&bytes) else { continue };
            if marker.assignment == assignment && marker.seen_at_ms <= now && now - marker.seen_at_ms > STALE_MS
                && path.file_stem().and_then(|value| value.to_str()) == Some(marker.connection.as_str())
                && std::fs::read(&path).ok().as_ref() == Some(&bytes) {
                let _ = std::fs::remove_file(path);
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runtime_lease_is_optional_isolated_and_does_not_modify_publication() {
        let root = tempfile::tempdir().unwrap();
        let assignment = uuid::Uuid::new_v4().to_string();
        let connection = "a".repeat(64); let version = "b".repeat(64);
        let publication = root.path().join("publication.json");
        std::fs::write(&publication, b"immutable route").unwrap();
        let lease = ConnectionLease::new_at(root.path(), &assignment, &connection, &version, 1000).unwrap();
        assert!(current_at(root.path(), &assignment, &version, 2000).unwrap().contains(&connection));
        assert!(current_at(root.path(), &assignment, &"c".repeat(64), 2000).unwrap().is_empty());
        assert!(current_at(root.path(), &assignment, &version, 1000 + LIVE_MS).unwrap().is_empty());
        assert_eq!(std::fs::read(&publication).unwrap(), b"immutable route");
        let path = lease.path.clone();
        drop(lease);
        assert!(!path.exists());
    }

    #[test]
    fn unknown_marker_does_not_report_a_current_runtime() {
        let root = tempfile::tempdir().unwrap();
        let assignment = uuid::Uuid::new_v4().to_string();
        let parent = root.path().join(&assignment); std::fs::create_dir(&parent).unwrap();
        std::fs::write(parent.join(format!("{}.json", "a".repeat(64))), b"invalid").unwrap();
        assert!(current_at(root.path(), &assignment, &"b".repeat(64), 1000).is_err());
    }
}
