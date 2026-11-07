//! Memory runtime publication shared by the GUI and the standalone launcher.
//!
//! The first filename number is a publication sequence, NOT a build timestamp.
//! Immutable legacy launchers only compare this number, so each activation
//! (including a rollback) must exceed every name those launchers can see. The
//! SHA-256 suffix identifies the bytes independently of their mtime or length.
//! No active pointer or client configuration migration is needed.
use sha2::{Digest, Sha256};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Seek, Write};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime};

const PREFIX: &str = "agent-manager-memory-";
// Keep .exe on Unix too: already installed launchers require this exact suffix.
const SUFFIX: &str = ".exe";
const HASH_PREFIX: &str = "sha256-";
const LOCK_TIMEOUT: Duration = Duration::from_secs(30);
const RETAINED_FALLBACKS: usize = 2;
const STARTUP_GRACE: Duration = Duration::from_secs(30 * 60);
const RETENTION_DIRECTORY: &str = ".retirement-grace";

pub fn directory() -> io::Result<PathBuf> {
    directory_from_config(dirs_next::config_dir())
}

fn directory_from_config(config: Option<PathBuf>) -> io::Result<PathBuf> {
    config.map(|root| root.join("agent-manager").join("mcp-runtime"))
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "memory runtime config directory unavailable"))
}

pub fn publish(source: &Path) -> io::Result<PathBuf> {
    publish_in(source, &directory()?)
}

pub fn newest() -> io::Result<Option<PathBuf>> {
    newest_in(&directory()?)
}

#[derive(Debug)]
struct Candidate {
    sequence: u64,
    name: String,
    path: PathBuf,
}

/// Match precisely the names considered by immutable, already installed
/// launchers. Even an invalid file/directory reserves its sequence: publishing
/// below it would leave those launchers pinned to it.
fn legacy_sequence(name: &str) -> Option<u64> {
    let (sequence, _) = name.strip_prefix(PREFIX)?.strip_suffix(SUFFIX)?.split_once('-')?;
    sequence.parse().ok()
}

fn candidates(root: &Path) -> io::Result<Vec<Candidate>> {
    let entries = match fs::read_dir(root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error),
    };
    let mut result = Vec::new();
    // Do not silently ignore enumeration errors when reserving a sequence.
    for entry in entries {
        let entry = entry?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        let Some(sequence) = legacy_sequence(name) else { continue };
        result.push(Candidate { sequence, name: name.to_string(), path: entry.path() });
    }
    result.sort_by(|left, right| left.sequence.cmp(&right.sequence).then(left.name.cmp(&right.name)));
    Ok(result)
}

fn published_digest(name: &str) -> Option<&str> {
    let (_, rest) = name.strip_prefix(PREFIX)?.strip_suffix(SUFFIX)?.split_once('-')?;
    let digest = rest.strip_prefix(HASH_PREFIX)?;
    (digest.len() == 64 && digest.bytes().all(|byte|
        byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)
    )).then_some(digest)
}

fn digest_reader(reader: &mut impl Read) -> io::Result<String> {
    let mut hash = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let count = reader.read(&mut buffer)?;
        if count == 0 { break; }
        hash.update(&buffer[..count]);
    }
    Ok(format!("{:x}", hash.finalize()))
}

fn valid_candidate(candidate: &Candidate) -> io::Result<bool> {
    let metadata = fs::symlink_metadata(&candidate.path)?;
    if !metadata.is_file() || metadata.file_type().is_symlink() { return Ok(false); }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o111 == 0 { return Ok(false); }
    }
    if let Some(digest) = published_digest(&candidate.name) {
        return Ok(digest_reader(&mut File::open(&candidate.path)?)? == digest);
    }
    // Read-only compatibility with pre-sequence publications. Their numeric
    // length suffix is a format marker only; it is not a content identity.
    let rest = candidate.name.strip_prefix(PREFIX).and_then(|name| name.strip_suffix(SUFFIX))
        .and_then(|name| name.split_once('-')).map(|(_, rest)| rest);
    Ok(rest.is_some_and(|rest| rest.parse::<u64>().is_ok()))
}

fn managed_name(name: &str) -> bool {
    if published_digest(name).is_some() { return true; }
    name.strip_prefix(PREFIX).and_then(|name| name.strip_suffix(SUFFIX))
        .and_then(|name| name.split_once('-'))
        .is_some_and(|(_, rest)| rest.parse::<u64>().is_ok())
}

fn newest_in(root: &Path) -> io::Result<Option<PathBuf>> {
    // Deterministic tie handling for legacy files; new publishers never reuse
    // a sequence. An incomplete temporary copy cannot enter this list.
    for candidate in candidates(root)?.into_iter().rev() {
        if valid_candidate(&candidate).unwrap_or(false) { return Ok(Some(candidate.path)); }
    }
    Ok(None)
}

fn publication_lock(root: &Path) -> io::Result<File> {
    // Keep the lock inode/path forever. Deleting it could let another writer
    // lock a replacement while the original writer still holds its old handle.
    let file = OpenOptions::new().read(true).write(true).create(true).truncate(false)
        .open(root.join(".publication.lock"))?;
    let deadline = Instant::now() + LOCK_TIMEOUT;
    loop {
        match file.try_lock() {
            Ok(()) => return Ok(file),
            Err(fs::TryLockError::WouldBlock) if Instant::now() < deadline =>
                std::thread::sleep(Duration::from_millis(10)),
            Err(fs::TryLockError::WouldBlock) => return Err(io::Error::new(
                io::ErrorKind::WouldBlock, "memory runtime publication busy; retry later",
            )),
            Err(fs::TryLockError::Error(error)) => return Err(error),
        }
    }
}

fn stage_runtime(source: &Path, root: &Path) -> io::Result<(tempfile::NamedTempFile, String)> {
    let mut input = File::open(source)?;
    let metadata = input.metadata()?;
    if !metadata.is_file() {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "memory runtime source is not a file"));
    }
    let mut temporary = tempfile::NamedTempFile::new_in(root)?;
    let mut hash = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let count = input.read(&mut buffer)?;
        if count == 0 { break; }
        temporary.write_all(&buffer[..count])?;
        hash.update(&buffer[..count]);
    }
    let digest = format!("{:x}", hash.finalize());
    // A build/reinstall may replace or rewrite the source during the copy.
    // Re-read the same open file, comparing bytes rather than mtime/length.
    input.rewind()?;
    if digest_reader(&mut input)? != digest {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "memory runtime source changed during publication"));
    }
    temporary.flush()?;
    temporary.as_file().set_permissions(metadata.permissions())?;
    temporary.as_file().sync_all()?;
    Ok((temporary, digest))
}

fn grace_elapsed(now: SystemTime, since: SystemTime) -> bool {
    // Future times, clock rollback and unreadable timestamps always retain.
    now.duration_since(since).is_ok_and(|age| age >= STARTUP_GRACE)
}

fn plain_marker(path: &Path) -> io::Result<fs::Metadata> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_file() || metadata.file_type().is_symlink() || metadata.len() != 0 {
        return Err(io::Error::other("invalid memory runtime grace marker"));
    }
    Ok(metadata)
}

/// Called only by a publisher holding the publication lock, including no-op
/// publications. Lookup stays read-only. Retain the current publication, any
/// higher sequence, and the two newest valid fallbacks without an age limit.
/// Other managed files get a 30-minute grace starting when they first leave
/// that retained set, and must also have been published at least 30 minutes ago.
///
/// This covers old launchers selecting the maximum immediately before a
/// publication, even when that runtime's own mtime is very old. Grace markers
/// are not selection pointers and never change filename ordering.
fn cleanup_runtime_copies(root: &Path, keep: &Path, now: SystemTime) {
    let Some(keep_sequence) = keep.file_name().and_then(|name| name.to_str()).and_then(legacy_sequence)
        else { return };
    let Ok(existing) = candidates(root) else { return };
    let grace_root = root.join(RETENTION_DIRECTORY);
    if fs::create_dir_all(&grace_root).is_err() { return; }
    let Ok(directory_metadata) = fs::symlink_metadata(&grace_root) else { return };
    if !directory_metadata.is_dir() || directory_metadata.file_type().is_symlink() { return; }
    let mut fallbacks = 0;
    let mut eligible = Vec::new();
    for candidate in existing.iter().rev() {
        if !managed_name(&candidate.name) { continue; }
        let Ok(metadata) = fs::symlink_metadata(&candidate.path) else { continue };
        if !metadata.is_file() || metadata.file_type().is_symlink() { continue; }
        let mut protected = candidate.path == keep || candidate.sequence >= keep_sequence;
        if !protected && fallbacks < RETAINED_FALLBACKS {
            match valid_candidate(candidate) {
                Ok(true) => { fallbacks += 1; protected = true; }
                Ok(false) => {}
                Err(_) => continue,
            }
        }
        let marker = grace_root.join(format!("{}.grace", candidate.name));
        if protected {
            // A fallback can re-enter the retained set if a newer file is lost
            // or corrupted. Clear its old eligibility clock before any deletes;
            // leaving the retained set again must start a fresh full grace.
            match plain_marker(&marker) {
                Ok(_) => if fs::remove_file(&marker).is_err() { return; },
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(_) => return,
            }
        } else {
            eligible.push((candidate, metadata, marker));
        }
    }
    for (candidate, metadata, marker) in eligible {
        let Ok(published) = metadata.modified() else { continue };
        // Never backdate or overwrite a grace record. A missing marker grants
        // a full new grace, including migration from previously unbounded
        // retention. Creation races and all I/O errors defer this candidate.
        match OpenOptions::new().write(true).create_new(true).open(&marker) {
            Ok(file) => { let _ = file.sync_all(); continue; }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(_) => continue,
        }
        let Ok(retired) = plain_marker(&marker).and_then(|metadata| metadata.modified()) else { continue };
        if !grace_elapsed(now, published) || !grace_elapsed(now, retired) { continue; }
        // No process is stopped and no executable is overwritten. Windows
        // locks simply leave the file and marker for a later publication;
        // unlinking on Unix does not interrupt an already running process.
        if fs::remove_file(&candidate.path).is_ok() { let _ = fs::remove_file(&marker); }
    }
    // Bound tiny sidecars too when an executable was removed externally.
    let Ok(markers) = fs::read_dir(&grace_root) else { return };
    for entry in markers.flatten() {
        let name = entry.file_name();
        let Some(runtime_name) = name.to_str().and_then(|name| name.strip_suffix(".grace")) else { continue };
        if !managed_name(runtime_name) || plain_marker(&entry.path()).is_err() { continue; }
        if matches!(fs::symlink_metadata(root.join(runtime_name)), Err(error) if error.kind() == io::ErrorKind::NotFound) {
            let _ = fs::remove_file(entry.path());
        }
    }
}

fn publish_in(source: &Path, root: &Path) -> io::Result<PathBuf> {
    // File locks serialize cooperating processes; the mutex also serializes
    // GUI startup and explicit installation on platforms with process locks.
    static LOCAL: OnceLock<Mutex<()>> = OnceLock::new();
    let _local = LOCAL.get_or_init(|| Mutex::new(())).lock()
        .map_err(|_| io::Error::other("memory runtime publication lock poisoned"))?;
    fs::create_dir_all(root)?;
    let _writer = publication_lock(root)?;
    let (temporary, digest) = stage_runtime(source, root)?;
    let existing = candidates(root)?;
    if let Some(latest) = existing.last() {
        let unique = existing.len() == 1 || existing[existing.len() - 2].sequence != latest.sequence;
        // Repeated startup/install of the already selected bytes is a no-op.
        // Only reuse the unambiguous maximum visible to OLD launchers too.
        if unique && published_digest(&latest.name) == Some(digest.as_str())
            && valid_candidate(latest).unwrap_or(false) {
            cleanup_runtime_copies(root, &latest.path, SystemTime::now());
            return Ok(latest.path.clone());
        }
    }
    let sequence = existing.last().map_or(Some(1), |latest| latest.sequence.checked_add(1))
        .ok_or_else(|| io::Error::other("memory runtime publication sequence exhausted"))?;
    let target = root.join(format!("{PREFIX}{sequence}-{HASH_PREFIX}{digest}{SUFFIX}"));
    // The only activation step is an atomic no-clobber publication of complete,
    // synced bytes. Never replace a published executable or reset the counter.
    temporary.persist_noclobber(&target).map_err(|error| error.error)?;
    #[cfg(unix)]
    File::open(root)?.sync_all()?;
    cleanup_runtime_copies(root, &target, SystemTime::now());
    Ok(target)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;
    use std::sync::{Arc, Barrier};

    fn source(root: &Path, name: &str, bytes: &[u8]) -> PathBuf {
        let path = root.join(name);
        fs::write(&path, bytes).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
        }
        path
    }

    // Deliberately model the immutable old binary, without the new parser or
    // validation, to prove that its exact selection rule still sees activation.
    fn legacy_pick(root: &Path) -> Option<PathBuf> {
        let mut newest: Option<(u64, PathBuf)> = None;
        for entry in fs::read_dir(root).ok()?.flatten() {
            let name = entry.file_name();
            let Some(rest) = name.to_str().and_then(|name| name.strip_prefix("agent-manager-memory-"))
                .and_then(|rest| rest.strip_suffix(".exe")) else { continue };
            let Some((stamp, _)) = rest.split_once('-') else { continue };
            let Ok(stamp) = stamp.parse::<u64>() else { continue };
            if newest.as_ref().map_or(true, |(best, _)| stamp > *best) {
                newest = Some((stamp, entry.path()));
            }
        }
        newest.map(|(_, path)| path)
    }

    fn assert_selected(root: &Path, expected: &Path) {
        assert_eq!(legacy_pick(root).as_deref(), Some(expected));
        assert_eq!(newest_in(root).unwrap().as_deref(), Some(expected));
    }

    #[test]
    fn rollback_and_reinstall_activate_above_a_broken_future_build() {
        let scratch = tempfile::tempdir().unwrap();
        let runtime = scratch.path().join("runtime");
        fs::create_dir(&runtime).unwrap();
        let broken = source(&runtime, "agent-manager-memory-9999999999-6.exe", b"broken");
        let older = source(scratch.path(), "old-gui", b"older runtime");
        let published = publish_in(&older, &runtime).unwrap();
        assert!(legacy_sequence(published.file_name().unwrap().to_str().unwrap()).unwrap() > 9999999999);
        assert_selected(&runtime, &published);
        assert!(broken.is_file());
        assert_eq!(publish_in(&older, &runtime).unwrap(), published);
        let newer = source(scratch.path(), "new-gui", b"newer runtime");
        let next = publish_in(&newer, &runtime).unwrap();
        assert_selected(&runtime, &next);
        let rollback = publish_in(&older, &runtime).unwrap();
        assert_ne!(published, rollback);
        assert_selected(&runtime, &rollback);
        assert_eq!(fs::read(rollback).unwrap(), b"older runtime");
        assert!(next.is_file());
    }

    #[test]
    fn equal_source_mtime_and_length_still_select_the_last_publication() {
        let scratch = tempfile::tempdir().unwrap();
        let runtime = scratch.path().join("runtime");
        let first = source(scratch.path(), "first", b"build A");
        let second = source(scratch.path(), "second", b"build B");
        let time = std::time::UNIX_EPOCH + Duration::from_secs(100);
        for path in [&first, &second] {
            File::options().write(true).open(path).unwrap()
                .set_times(fs::FileTimes::new().set_modified(time)).unwrap();
        }
        let a = publish_in(&first, &runtime).unwrap();
        let b = publish_in(&second, &runtime).unwrap();
        assert_ne!(a, b);
        assert_selected(&runtime, &b);
        assert_eq!(fs::read(&a).unwrap(), b"build A");
        assert_eq!(fs::read(&b).unwrap(), b"build B");
        // An in-place rewrite with the same metadata must also publish.
        fs::write(&second, b"build C").unwrap();
        File::options().write(true).open(&second).unwrap()
            .set_times(fs::FileTimes::new().set_modified(time)).unwrap();
        let c = publish_in(&second, &runtime).unwrap();
        assert_selected(&runtime, &c);
        assert_eq!(fs::read(c).unwrap(), b"build C");
    }

    #[test]
    fn concurrent_publishers_get_unique_sequences_and_keep_all_committed_paths() {
        let scratch = tempfile::tempdir().unwrap();
        let runtime = scratch.path().join("runtime");
        let barrier = Arc::new(Barrier::new(4));
        let mut workers = Vec::new();
        for index in 0..4 {
            let input = source(scratch.path(), &format!("source-{index}"), format!("runtime {index}").as_bytes());
            let runtime = runtime.clone();
            let barrier = Arc::clone(&barrier);
            workers.push(std::thread::spawn(move || {
                barrier.wait();
                publish_in(&input, &runtime).unwrap()
            }));
        }
        let publications: Vec<_> = workers.into_iter().map(|worker| worker.join().unwrap()).collect();
        let sequences: HashSet<_> = publications.iter().map(|path|
            legacy_sequence(path.file_name().unwrap().to_str().unwrap()).unwrap()
        ).collect();
        assert_eq!(sequences.len(), 4);
        assert!(publications.iter().all(|path| path.is_file()));
        let latest = publications.iter().max_by_key(|path|
            legacy_sequence(path.file_name().unwrap().to_str().unwrap()).unwrap()
        ).unwrap();
        assert_selected(&runtime, latest);
        for candidate in candidates(&runtime).unwrap() {
            assert!(valid_candidate(&candidate).unwrap());
        }
    }

    #[test]
    fn publication_lock_excludes_independently_opened_handles() {
        let scratch = tempfile::tempdir().unwrap();
        let writer = publication_lock(scratch.path()).unwrap();
        let contender = OpenOptions::new().read(true).write(true)
            .open(scratch.path().join(".publication.lock")).unwrap();
        assert!(matches!(contender.try_lock(), Err(fs::TryLockError::WouldBlock)));
        drop(writer);
        contender.try_lock().unwrap();
        // Releasing a writer must not unlink the path used by other processes.
        assert!(scratch.path().join(".publication.lock").is_file());
    }

    #[test]
    fn staging_and_failed_publication_do_not_activate_or_delete_a_selected_runtime() {
        let scratch = tempfile::tempdir().unwrap();
        let runtime = scratch.path().join("runtime");
        let a = source(scratch.path(), "a", b"selected runtime");
        let b = source(scratch.path(), "b", b"next runtime");
        let first = publish_in(&a, &runtime).unwrap();
        let mut selected_handle = File::open(&first).unwrap();
        let (staged, _) = stage_runtime(&b, &runtime).unwrap();
        assert_selected(&runtime, &first);
        assert!(publish_in(&scratch.path().join("missing"), &runtime).is_err());
        assert_selected(&runtime, &first);
        drop(staged);
        let second = publish_in(&b, &runtime).unwrap();
        assert_selected(&runtime, &second);
        assert!(first.is_file());
        let mut bytes = Vec::new();
        selected_handle.read_to_end(&mut bytes).unwrap();
        assert_eq!(bytes, b"selected runtime");
    }

    #[test]
    fn validation_rejects_tampering_and_republication_repairs_legacy_selection() {
        let scratch = tempfile::tempdir().unwrap();
        let runtime = scratch.path().join("runtime");
        let a = source(scratch.path(), "a", b"runtime A");
        let b = source(scratch.path(), "b", b"runtime B");
        let first = publish_in(&a, &runtime).unwrap();
        let second = publish_in(&b, &runtime).unwrap();
        fs::write(&second, b"tampered!").unwrap();
        assert_eq!(newest_in(&runtime).unwrap(), Some(first.clone()));
        let repaired = publish_in(&b, &runtime).unwrap();
        assert_ne!(repaired, second);
        assert_selected(&runtime, &repaired);
        assert_eq!(fs::read(&repaired).unwrap(), b"runtime B");
        // Even names that the new reader rejects must be exceeded for old readers.
        fs::create_dir(runtime.join("agent-manager-memory-1000000-anything.exe")).unwrap();
        let repaired_again = publish_in(&b, &runtime).unwrap();
        assert_selected(&runtime, &repaired_again);
        assert!(first.is_file());
    }

    #[test]
    fn legacy_ties_are_superseded_by_one_unique_publication() {
        let scratch = tempfile::tempdir().unwrap();
        let runtime = scratch.path().join("runtime");
        fs::create_dir(&runtime).unwrap();
        source(&runtime, "agent-manager-memory-100-1.exe", b"a");
        let legacy = source(&runtime, "agent-manager-memory-100-2.exe", b"bb");
        assert_eq!(newest_in(&runtime).unwrap(), Some(legacy));
        let input = source(scratch.path(), "gui", b"current");
        let selected = publish_in(&input, &runtime).unwrap();
        assert_selected(&runtime, &selected);
    }

    #[test]
    fn sequence_exhaustion_fails_without_replacing_a_published_file() {
        let scratch = tempfile::tempdir().unwrap();
        let runtime = scratch.path().join("runtime");
        fs::create_dir(&runtime).unwrap();
        let original = source(&runtime, &format!("agent-manager-memory-{}-3.exe", u64::MAX), b"old");
        let input = source(scratch.path(), "gui", b"current");
        assert!(publish_in(&input, &runtime).is_err());
        assert_selected(&runtime, &original);
        assert_eq!(fs::read(original).unwrap(), b"old");
    }

    #[test]
    fn publication_and_lookup_use_the_config_directory_on_every_platform() {
        let scratch = tempfile::tempdir().unwrap();
        let config = scratch.path().join("config");
        let data = scratch.path().join("data");
        let root = directory_from_config(Some(config.clone())).unwrap();
        assert_eq!(root, config.join("agent-manager/mcp-runtime"));
        let input = source(scratch.path(), "gui", b"runtime");
        let selected = publish_in(&input, &root).unwrap();
        assert_selected(&root, &selected);
        assert!(!data.join("agent-manager/mcp-runtime").exists());
        assert!(directory_from_config(None).is_err());
        assert_eq!(directory().unwrap(), dirs_next::config_dir().unwrap().join("agent-manager/mcp-runtime"));
    }

    fn grace_marker(root: &Path, runtime: &Path) -> PathBuf {
        root.join(RETENTION_DIRECTORY).join(format!("{}.grace", runtime.file_name().unwrap().to_str().unwrap()))
    }

    fn set_modified(path: &Path, time: SystemTime) {
        File::options().write(true).open(path).unwrap()
            .set_times(fs::FileTimes::new().set_modified(time)).unwrap();
    }

    fn retention_fixture(scratch: &Path, count: usize) -> (PathBuf, Vec<PathBuf>, Vec<PathBuf>) {
        let runtime = scratch.join("runtime");
        let mut inputs = Vec::new();
        let mut publications = Vec::new();
        for index in 0..count {
            let input = source(scratch, &format!("input-{index}"), format!("build {index}").as_bytes());
            publications.push(publish_in(&input, &runtime).unwrap());
            inputs.push(input);
        }
        (runtime, inputs, publications)
    }

    fn expire_eligible(root: &Path, publications: &[PathBuf], now: SystemTime) {
        let past = now.checked_sub(STARTUP_GRACE * 2).unwrap();
        for path in publications { set_modified(path, past); }
        cleanup_runtime_copies(root, publications.last().unwrap(), now);
        for path in publications.iter().take(publications.len().saturating_sub(RETAINED_FALLBACKS + 1)) {
            set_modified(&grace_marker(root, path), past);
        }
    }

    #[test]
    fn retention_keeps_current_and_two_fallbacks_and_rollback_republishes_above_them() {
        let scratch = tempfile::tempdir().unwrap();
        let (runtime, inputs, publications) = retention_fixture(scratch.path(), 6);
        let now = SystemTime::now();
        expire_eligible(&runtime, &publications, now);
        let current = publications.last().unwrap();
        cleanup_runtime_copies(&runtime, current, now);
        for path in &publications[..3] {
            assert!(!path.exists());
            assert!(!grace_marker(&runtime, path).exists());
        }
        assert!(publications[3..].iter().all(|path| path.is_file()));
        assert_selected(&runtime, current);
        let rollback = publish_in(&inputs[0], &runtime).unwrap();
        assert_selected(&runtime, &rollback);
        assert!(legacy_sequence(rollback.file_name().unwrap().to_str().unwrap()).unwrap()
            > legacy_sequence(current.file_name().unwrap().to_str().unwrap()).unwrap());
        assert_eq!(fs::read(rollback).unwrap(), b"build 0");
    }

    #[test]
    fn an_old_runtime_gets_a_full_grace_when_it_leaves_the_fallback_set() {
        let scratch = tempfile::tempdir().unwrap();
        let runtime = scratch.path().join("runtime");
        let input = source(scratch.path(), "old", b"selected by legacy launcher");
        let selected = publish_in(&input, &runtime).unwrap();
        set_modified(&selected, SystemTime::now().checked_sub(STARTUP_GRACE * 10).unwrap());
        let mut current = selected.clone();
        for index in 0..3 {
            let input = source(scratch.path(), &format!("new-{index}"), format!("new {index}").as_bytes());
            current = publish_in(&input, &runtime).unwrap();
        }
        cleanup_runtime_copies(&runtime, &current, SystemTime::now());
        assert!(selected.is_file());
        assert!(grace_marker(&runtime, &selected).is_file());
        assert_selected(&runtime, &current);
    }

    #[test]
    fn recent_publication_future_times_and_invalid_markers_defer_retention() {
        let scratch = tempfile::tempdir().unwrap();
        let (runtime, _, publications) = retention_fixture(scratch.path(), 6);
        let now = SystemTime::now();
        let past = now.checked_sub(STARTUP_GRACE * 2).unwrap();
        expire_eligible(&runtime, &publications, now);
        // A recently published file is protected even with an aged marker.
        set_modified(&publications[0], now);
        // Clock rollback/future metadata on either clock must not imply age.
        set_modified(&publications[1], now + STARTUP_GRACE);
        set_modified(&grace_marker(&runtime, &publications[2]), now + STARTUP_GRACE);
        cleanup_runtime_copies(&runtime, publications.last().unwrap(), now);
        assert!(publications.iter().all(|path| path.is_file()));
        assert!(!grace_elapsed(now, now + STARTUP_GRACE));
        // Invalid/unreadable marker state grants no permission to delete.
        set_modified(&publications[0], past);
        let invalid = grace_marker(&runtime, &publications[0]);
        fs::remove_file(&invalid).unwrap();
        fs::create_dir(&invalid).unwrap();
        cleanup_runtime_copies(&runtime, publications.last().unwrap(), now);
        assert!(publications[0].is_file());
    }

    #[test]
    fn reentering_the_valid_fallback_set_resets_retirement_grace() {
        let scratch = tempfile::tempdir().unwrap();
        let (runtime, inputs, publications) = retention_fixture(scratch.path(), 5);
        let now = SystemTime::now();
        expire_eligible(&runtime, &publications, now);
        fs::write(&publications[2], b"corrupted").unwrap();
        fs::write(&publications[3], b"corrupted").unwrap();
        cleanup_runtime_copies(&runtime, publications.last().unwrap(), now);
        for path in &publications[..2] {
            assert!(path.is_file());
            assert!(!grace_marker(&runtime, path).exists());
        }
        // Repair the newer fallbacks. The older files must get fresh grace,
        // rather than being deleted using their earlier eligibility records.
        for index in [2, 3] {
            fs::write(&publications[index], fs::read(&inputs[index]).unwrap()).unwrap();
        }
        cleanup_runtime_copies(&runtime, publications.last().unwrap(), SystemTime::now());
        assert!(publications[..2].iter().all(|path| path.is_file()));
        assert!(publications[..2].iter().all(|path| grace_marker(&runtime, path).is_file()));
    }

    #[test]
    fn no_op_publication_retries_cleanup_without_changing_active_sequence() {
        let scratch = tempfile::tempdir().unwrap();
        let (runtime, inputs, publications) = retention_fixture(scratch.path(), 4);
        expire_eligible(&runtime, &publications, SystemTime::now());
        assert_eq!(publish_in(inputs.last().unwrap(), &runtime).unwrap(), *publications.last().unwrap());
        assert!(!publications[0].exists());
        assert_selected(&runtime, publications.last().unwrap());
    }

    #[test]
    fn stale_cleanup_cannot_delete_a_higher_publication_or_unmanaged_files() {
        let scratch = tempfile::tempdir().unwrap();
        let (runtime, _, publications) = retention_fixture(scratch.path(), 6);
        let now = SystemTime::now();
        expire_eligible(&runtime, &publications, now);
        let unrelated = source(&runtime, "agent-manager-memory-0-user-file.exe", b"user data");
        cleanup_runtime_copies(&runtime, &publications[0], now);
        assert!(publications.iter().all(|path| path.is_file()));
        assert!(unrelated.is_file());
    }

    #[cfg(windows)]
    #[test]
    fn locked_executables_do_not_block_retention_of_other_expired_files() {
        use std::os::windows::fs::OpenOptionsExt;
        let scratch = tempfile::tempdir().unwrap();
        let (runtime, _, publications) = retention_fixture(scratch.path(), 6);
        let now = SystemTime::now();
        expire_eligible(&runtime, &publications, now);
        let locked = OpenOptions::new().read(true).share_mode(0x1 | 0x2)
            .open(&publications[0]).unwrap(); // Share read/write, but not delete.
        cleanup_runtime_copies(&runtime, publications.last().unwrap(), now);
        assert!(publications[0].is_file());
        assert!(grace_marker(&runtime, &publications[0]).is_file());
        assert!(!publications[1].exists());
        assert!(!publications[2].exists());
        drop(locked);
        cleanup_runtime_copies(&runtime, publications.last().unwrap(), now);
        assert!(!publications[0].exists());
        assert_selected(&runtime, publications.last().unwrap());
    }

    #[cfg(unix)]
    #[test]
    fn unix_publications_preserve_execute_permission_and_reject_symlinks() {
        use std::os::unix::fs::{symlink, PermissionsExt};
        let scratch = tempfile::tempdir().unwrap();
        let runtime = scratch.path().join("runtime");
        let input = source(scratch.path(), "gui", b"executable");
        let selected = publish_in(&input, &runtime).unwrap();
        assert_eq!(fs::metadata(&selected).unwrap().permissions().mode() & 0o777, 0o700);
        symlink(&selected, runtime.join("agent-manager-memory-500-10.exe")).unwrap();
        assert_eq!(newest_in(&runtime).unwrap(), Some(selected));
        let repaired = publish_in(&input, &runtime).unwrap();
        assert_selected(&runtime, &repaired);
    }
}
