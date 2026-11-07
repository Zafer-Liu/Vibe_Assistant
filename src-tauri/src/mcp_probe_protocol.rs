//! Relay publication metadata, independent of the stable client binding.
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::path::{Path, PathBuf};

pub const PROBE_SCHEMA: u32 = 1;
pub const LAUNCHER_ABI: u32 = 1;
pub const STARTUP_GRACE_MS: u64 = 120_000;
pub const MAX_GENERATIONS: usize = 16;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProbeInfo {
    pub schema: u32,
    pub launcher_abi: u32,
    pub generation_resolution: u32,
    pub version: String,
    pub build_sequence: u64,
}

impl ProbeInfo {
    pub fn compatible(&self) -> bool {
        self.schema == PROBE_SCHEMA && self.launcher_abi == LAUNCHER_ABI
            && self.generation_resolution == 1 && self.build_sequence > 0
            && !self.version.is_empty() && self.version.len() <= 64
            && self.version.is_ascii() && !self.version.chars().any(char::is_control)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProbeGeneration {
    pub command: String,
    pub digest: String,
    pub info: ProbeInfo,
    pub published_at_ms: u64,
    pub retire_after_ms: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProbeRuntime {
    pub revision: u64,
    pub preferred: String,
    pub generations: Vec<ProbeGeneration>,
}

pub fn valid_digest(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

pub fn digest_from_path(path: &Path) -> Result<String, &'static str> {
    let name = path.file_name().and_then(|s| s.to_str()).ok_or("probe_filename_invalid")?;
    let stem = if cfg!(windows) { name.strip_suffix(".exe").ok_or("probe_filename_invalid")? } else { name };
    let digest = stem.strip_prefix("mcp-relay-").ok_or("probe_filename_invalid")?;
    if !valid_digest(digest) { return Err("probe_filename_invalid"); }
    Ok(digest.to_string())
}

pub fn verify_file(command: &str, digest: &str, directory: &Path) -> Result<PathBuf, &'static str> {
    let path = Path::new(command);
    if !path.is_absolute() || digest_from_path(path)? != digest { return Err("probe_identity_invalid"); }
    let metadata = std::fs::symlink_metadata(path).map_err(|_| "probe_file_unavailable")?;
    if !metadata.is_file() || metadata.file_type().is_symlink() { return Err("probe_file_invalid"); }
    let parent = path.parent().ok_or("probe_directory_invalid")?.canonicalize().map_err(|_| "probe_directory_unavailable")?;
    if parent != directory.canonicalize().map_err(|_| "probe_directory_unavailable")? {
        return Err("probe_directory_invalid");
    }
    let bytes = std::fs::read(path).map_err(|_| "probe_file_unavailable")?;
    if format!("{:x}", Sha256::digest(bytes)) != digest { return Err("probe_hash_mismatch"); }
    path.canonicalize().map_err(|_| "probe_file_unavailable")
}

impl ProbeRuntime {
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.revision == 0 || self.generations.is_empty() || self.generations.len() > MAX_GENERATIONS {
            return Err("probe_runtime_invalid");
        }
        let mut seen = HashSet::new();
        for generation in &self.generations {
            if !valid_digest(&generation.digest) || !seen.insert(&generation.digest)
                || digest_from_path(Path::new(&generation.command))? != generation.digest
                || generation.info.schema != PROBE_SCHEMA || generation.info.launcher_abi != LAUNCHER_ABI {
                return Err("probe_runtime_invalid");
            }
            if generation.digest == self.preferred {
                if !generation.info.compatible() || generation.retire_after_ms.is_some() {
                    return Err("probe_preferred_invalid");
                }
            } else if generation.retire_after_ms.is_none() {
                return Err("probe_retention_invalid");
            }
        }
        if !seen.contains(&self.preferred) { return Err("probe_preferred_missing"); }
        Ok(())
    }

    pub fn preferred_generation(&self) -> Result<&ProbeGeneration, &'static str> {
        self.validate()?;
        self.generations.iter().find(|g| g.digest == self.preferred).ok_or("probe_preferred_missing")
    }

    pub fn selected_generation(&self, digest: &str, now_ms: u64) -> Result<&ProbeGeneration, &'static str> {
        self.validate()?;
        let generation = self.generations.iter().find(|g| g.digest == digest).ok_or("probe_generation_unapproved")?;
        if generation.digest != self.preferred && generation.retire_after_ms.is_some_and(|until| now_ms >= until) {
            return Err("probe_generation_retired");
        }
        Ok(generation)
    }

    /// Returns None for an unchanged or stale candidate. Capacity pressure defers
    /// publication; it never evicts a protected in-flight generation.
    pub fn publish(
        current: Option<&Self>, legacy_command: Option<&str>, candidate: &ProbeGeneration, now_ms: u64,
    ) -> Result<Option<Self>, &'static str> {
        if !candidate.info.compatible() || !valid_digest(&candidate.digest)
            || digest_from_path(Path::new(&candidate.command))? != candidate.digest {
            return Err("probe_candidate_incompatible");
        }
        let mut generations;
        let revision;
        if let Some(current) = current {
            let preferred = current.preferred_generation()?;
            if candidate.digest == preferred.digest {
                if candidate.info != preferred.info || candidate.command != preferred.command {
                    return Err("probe_identity_conflict");
                }
                return Ok(None);
            }
            if candidate.info.build_sequence < preferred.info.build_sequence { return Ok(None); }
            if candidate.info.build_sequence == preferred.info.build_sequence { return Err("probe_release_conflict"); }
            generations = current.generations.iter().filter(|g|
                g.digest == current.preferred || g.retire_after_ms.is_none_or(|until| now_ms < until)
            ).cloned().collect::<Vec<_>>();
            revision = current.revision.checked_add(1).ok_or("probe_revision_overflow")?;
        } else {
            generations = Vec::new();
            revision = 1;
            if let Some(command) = legacy_command {
                let digest = digest_from_path(Path::new(command))?;
                if digest != candidate.digest {
                    generations.push(ProbeGeneration {
                        command: command.to_string(), digest,
                        info: ProbeInfo { schema: PROBE_SCHEMA, launcher_abi: LAUNCHER_ABI,
                            generation_resolution: 0, version: "legacy".into(), build_sequence: 0 },
                        published_at_ms: now_ms,
                        retire_after_ms: Some(now_ms.checked_add(STARTUP_GRACE_MS).ok_or("probe_clock_invalid")?),
                    });
                }
            }
        }
        if generations.len() >= MAX_GENERATIONS { return Err("probe_startups_protected"); }
        let retire = now_ms.checked_add(STARTUP_GRACE_MS).ok_or("probe_clock_invalid")?;
        for generation in &mut generations {
            if generation.retire_after_ms.is_none() { generation.retire_after_ms = Some(retire); }
        }
        let mut published = candidate.clone();
        published.published_at_ms = now_ms;
        published.retire_after_ms = None;
        generations.push(published);
        let next = Self { revision, preferred: candidate.digest.clone(), generations };
        next.validate()?;
        Ok(Some(next))
    }
}

/// Binding recovery cannot restore an old whole-manifest snapshot over a newer
/// publication. The caller also serializes writers with the control-plane lock.
pub fn merge_runtime(
    requested: Option<&ProbeRuntime>, persisted: Option<&ProbeRuntime>,
) -> Result<Option<ProbeRuntime>, &'static str> {
    if let Some(persisted) = persisted {
        persisted.validate()?;
        if let Some(requested) = requested {
            requested.validate()?;
            if requested.revision > persisted.revision {
                let wanted = requested.preferred_generation()?;
                let known = persisted.preferred_generation()?;
                if wanted.info.build_sequence < known.info.build_sequence {
                    return Err("probe_release_regression");
                }
                if wanted.info.build_sequence == known.info.build_sequence && wanted.digest != known.digest {
                    return Err("probe_release_conflict");
                }
                return Ok(Some(requested.clone()));
            }
            if requested.revision == persisted.revision && requested != persisted {
                return Err("probe_revision_conflict");
            }
        }
        return Ok(Some(persisted.clone()));
    }
    if let Some(requested) = requested { requested.validate()?; }
    Ok(requested.cloned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candidate(sequence: u64) -> ProbeGeneration {
        let digest = format!("{sequence:064x}");
        let name = format!("mcp-relay-{digest}{}", if cfg!(windows) { ".exe" } else { "" });
        ProbeGeneration {
            command: std::env::temp_dir().join(name).to_string_lossy().into_owned(), digest,
            info: ProbeInfo { schema: PROBE_SCHEMA, launcher_abi: LAUNCHER_ABI,
                generation_resolution: 1, version: "1.2.3".into(), build_sequence: sequence },
            published_at_ms: 0, retire_after_ms: None,
        }
    }

    #[test]
    fn successive_updates_keep_inflight_a_but_new_connections_select_c() {
        let a = candidate(1); let b = candidate(2); let c = candidate(3);
        let first = ProbeRuntime::publish(None, None, &a, 1_000).unwrap().unwrap();
        let second = ProbeRuntime::publish(Some(&first), None, &b, 2_000).unwrap().unwrap();
        let third = ProbeRuntime::publish(Some(&second), None, &c, 3_000).unwrap().unwrap();
        assert_eq!(third.preferred_generation().unwrap().digest, c.digest);
        assert_eq!(third.selected_generation(&a.digest, 4_000).unwrap().command, a.command);
        assert_eq!(third.selected_generation(&b.digest, 4_000).unwrap().command, b.command);
        assert!(third.selected_generation(&a.digest, 2_000 + STARTUP_GRACE_MS).is_err());
        assert!(ProbeRuntime::publish(Some(&third), None, &b, 4_000).unwrap().is_none());
        assert!(ProbeRuntime::publish(Some(&third), None, &c, 4_000).unwrap().is_none());
    }

    #[test]
    fn stale_binding_recovery_preserves_new_preferred_runtime() {
        let first = ProbeRuntime::publish(None, None, &candidate(1), 1).unwrap().unwrap();
        let next = ProbeRuntime::publish(Some(&first), None, &candidate(2), 2).unwrap().unwrap();
        assert_eq!(merge_runtime(Some(&first), Some(&next)).unwrap(), Some(next.clone()));
        assert_eq!(merge_runtime(None, Some(&next)).unwrap(), Some(next.clone()));
        let mut conflict = next.clone(); conflict.generations[0].published_at_ms += 1;
        assert!(merge_runtime(Some(&conflict), Some(&next)).is_err());
    }

    #[test]
    fn protected_history_defers_publication_without_losing_preference() {
        let mut current = ProbeRuntime::publish(None, None, &candidate(1), 1).unwrap().unwrap();
        for seq in 2..=MAX_GENERATIONS as u64 {
            current = ProbeRuntime::publish(Some(&current), None, &candidate(seq), seq).unwrap().unwrap();
        }
        let before = current.clone();
        assert_eq!(ProbeRuntime::publish(Some(&current), None, &candidate(100), 100).unwrap_err(), "probe_startups_protected");
        assert_eq!(current, before);
        let after = ProbeRuntime::publish(Some(&current), None, &candidate(100), STARTUP_GRACE_MS + 100).unwrap().unwrap();
        assert_eq!(after.preferred_generation().unwrap().digest, candidate(100).digest);
    }

    #[test]
    fn equal_release_different_hash_and_wrong_abi_never_publish() {
        let a = candidate(1);
        let current = ProbeRuntime::publish(None, None, &a, 1).unwrap().unwrap();
        let mut conflict = candidate(2); conflict.info.build_sequence = 1;
        assert_eq!(ProbeRuntime::publish(Some(&current), None, &conflict, 2).unwrap_err(), "probe_release_conflict");
        conflict.info.launcher_abi = 2;
        assert_eq!(ProbeRuntime::publish(Some(&current), None, &conflict, 2).unwrap_err(), "probe_candidate_incompatible");
    }

    #[test]
    fn file_identity_rejects_hash_changes_and_foreign_directory() {
        let root = tempfile::tempdir().unwrap();
        let bytes = b"test payload, never executed";
        let digest = format!("{:x}", Sha256::digest(bytes));
        let name = format!("mcp-relay-{digest}{}", if cfg!(windows) { ".exe" } else { "" });
        let path = root.path().join(name);
        std::fs::write(&path, bytes).unwrap();
        assert!(verify_file(path.to_str().unwrap(), &digest, root.path()).is_ok());
        let foreign = tempfile::tempdir().unwrap();
        assert!(verify_file(path.to_str().unwrap(), &digest, foreign.path()).is_err());
        std::fs::write(&path, b"modified").unwrap();
        assert_eq!(verify_file(path.to_str().unwrap(), &digest, root.path()).unwrap_err(), "probe_hash_mismatch");
    }
}
