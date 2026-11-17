//! Process-local metadata inventory shared by conversation and usage readers.
//! Consumers keep their own durable parse checkpoints; this cache only avoids
//! repeated directory walks. Missing directory mtimes are never trusted.
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime};

const SNAPSHOT_REUSE: Duration = Duration::from_secs(2);
const FULL_RECONCILE: Duration = Duration::from_secs(15 * 60);
const MAX_ENTRIES_PER_ROOT: usize = 20_000;
type FileKey = (String, PathBuf);

#[derive(Clone, Debug)]
pub(crate) struct TranscriptFile {
    pub source: String,
    pub session_id: String,
    pub path: PathBuf,
    pub size: u64,
    pub modified: SystemTime,
}

impl TranscriptFile {
    pub fn inspect(source: &str, path: &Path) -> Option<Self> {
        let session_id = crate::agent_sources::session_id_for_path(source, path)?;
        let metadata = std::fs::metadata(path).ok()?;
        if !metadata.is_file() { return None; }
        Some(Self {
            source: source.to_string(), session_id, path: path.to_path_buf(),
            size: metadata.len(), modified: metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH),
        })
    }

    pub fn modified_text(&self) -> String {
        chrono::DateTime::<chrono::Utc>::from(self.modified).to_rfc3339()
    }
}

#[derive(Default)]
struct Inventory {
    roots: Vec<FileKey>,
    directories: HashMap<FileKey, Option<SystemTime>>,
    files: HashMap<FileKey, TranscriptFile>,
    deferred: Vec<FileKey>,
    checked_at: Option<Instant>,
    reconciled_at: Option<Instant>,
    snapshot: Arc<Vec<TranscriptFile>>,
}

impl Inventory {
    fn snapshot(&mut self, roots: Vec<FileKey>, force: bool) -> Arc<Vec<TranscriptFile>> {
        self.snapshot_with_budget(roots, force, MAX_ENTRIES_PER_ROOT)
    }

    fn snapshot_with_budget(&mut self, roots: Vec<FileKey>, force: bool, entry_budget: usize) -> Arc<Vec<TranscriptFile>> {
        // Follow locations explicitly selected by the user, but not arbitrary
        // symlink descendants discovered while walking inside those locations.
        let roots = roots.into_iter().map(|(source, root)| {
            let resolved = std::fs::symlink_metadata(&root).ok()
                .filter(|meta| meta.file_type().is_symlink())
                .and_then(|_| std::fs::canonicalize(&root).ok()).unwrap_or(root);
            (source, resolved)
        }).collect::<Vec<_>>();
        let now = Instant::now();
        let roots_changed = roots != self.roots;
        if !force && !roots_changed && self.checked_at.is_some_and(|at| now.duration_since(at) < SNAPSHOT_REUSE) {
            return Arc::clone(&self.snapshot);
        }
        let full = force || roots_changed || self.reconciled_at.is_none_or(|at| now.duration_since(at) >= FULL_RECONCILE);
        self.roots = roots;
        // Retain unreadable subtrees on transient errors. Only a successful
        // directory listing or a definite NotFound may remove their entries.
        if roots_changed {
            self.directories.retain(|(source, path), _| self.roots.iter().any(|(kind, root)| kind == source && path.starts_with(root)));
            self.files.retain(|(source, path), _| self.roots.iter().any(|(kind, root)| kind == source && path.starts_with(root)));
            self.deferred.retain(|(source, path)| self.roots.iter().any(|(kind, root)| kind == source && path.starts_with(root)));
        }
        let mut pending = self.roots.clone();
        pending.extend(self.directories.keys().cloned());
        // Resume previously deferred directories before revisiting old roots.
        pending.extend(std::mem::take(&mut self.deferred));
        let mut visited = HashSet::new();
        let mut counts = HashMap::<FileKey, usize>::new();
        while let Some((source, directory)) = pending.pop() {
            if !visited.insert((source.clone(), directory.clone())) { continue; }
            let Some(root) = self.roots.iter().find(|(kind, root)| kind == &source && directory.starts_with(root)).cloned() else { continue };
            let count = counts.entry(root).or_default();
            if *count >= entry_budget {
                self.directories.insert((source.clone(), directory.clone()), None);
                self.deferred.push((source, directory));
                continue;
            }
            let modified = match std::fs::symlink_metadata(&directory) {
                Ok(meta) if meta.is_dir() && !meta.file_type().is_symlink() => meta.modified().ok(),
                Ok(_) => { self.remove_tree(&source, &directory); continue; }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => { self.remove_tree(&source, &directory); continue; }
                Err(_) => continue,
            };
            if !full && modified.is_some() && self.directories.get(&(source.clone(), directory.clone())) == Some(&modified) { continue; }
            let Ok(entries) = std::fs::read_dir(&directory) else { continue };
            let mut children = HashSet::new();
            let mut complete = true;
            for entry in entries {
                let Ok(entry) = entry else { complete = false; continue };
                // Complete the open listing even when the inter-directory
                // budget is reached; a stable prefix must not hide its tail.
                *count += 1;
                let path = entry.path();
                children.insert(path.clone());
                let Ok(kind) = entry.file_type() else { complete = false; continue };
                // Avoid cycles and escaping the configured roots via symlinks.
                if kind.is_symlink() { self.remove_tree(&source, &path); continue; }
                if kind.is_dir() {
                    self.directories.entry((source.clone(), path.clone())).or_insert(None);
                    pending.push((source.clone(), path));
                } else if kind.is_file() && matches!(path.extension().and_then(|ext| ext.to_str()), Some("jsonl") | Some("zstd")) {
                    if let Some(file) = TranscriptFile::inspect(&source, &path) {
                        self.files.insert((source.clone(), path), file);
                    } else if crate::agent_sources::session_id_for_path(&source, &path).is_none() {
                        // DSH keeps several format generations in one folder;
                        // the old generation ceases to be authoritative.
                        self.files.remove(&(source.clone(), path));
                    } else {
                        // Do not evict a previously known file on a temporary
                        // sharing/permission error; retry this listing next pass.
                        complete = false;
                    }
                }
            }
            if complete {
                let removed = self.directories.keys().filter(|(kind, path)| kind == &source && path.parent() == Some(directory.as_path()) && !children.contains(path)).cloned().collect::<Vec<_>>();
                for (kind, path) in removed { self.remove_tree(&kind, &path); }
                self.files.retain(|(kind, path), _| kind != &source || path.parent() != Some(directory.as_path()) || children.contains(path));
                self.directories.insert((source, directory), modified);
            } else {
                self.directories.insert((source, directory), None);
            }
        }
        // Appending to a file does not change its parent directory mtime.
        // Restat known files even when no directory had to be listed.
        self.files.retain(|(_, path), file| match std::fs::metadata(path) {
            Ok(meta) if meta.is_file() => {
                file.size = meta.len();
                file.modified = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
                true
            }
            Ok(_) => false,
            Err(error) => error.kind() != std::io::ErrorKind::NotFound,
        });
        let mut files = self.files.values().cloned().collect::<Vec<_>>();
        files.sort_by(|left, right| left.source.cmp(&right.source).then_with(|| right.modified.cmp(&left.modified)).then_with(|| left.path.cmp(&right.path)));
        self.snapshot = Arc::new(files);
        self.checked_at = Some(now);
        if full { self.reconciled_at = Some(now); }
        Arc::clone(&self.snapshot)
    }

    fn remove_tree(&mut self, source: &str, directory: &Path) {
        self.directories.retain(|(kind, path), _| kind != source || !path.starts_with(directory));
        self.files.retain(|(kind, path), _| kind != source || !path.starts_with(directory));
    }
}

#[derive(Default)]
struct SourceInventory {
    inventory: Mutex<Inventory>,
    published: Mutex<Arc<Vec<TranscriptFile>>>,
}

fn source_inventory(source: &str, force: bool, wait: bool) -> Arc<Vec<TranscriptFile>> {
    static INVENTORIES: OnceLock<HashMap<&'static str, SourceInventory>> = OnceLock::new();
    let inventories = INVENTORIES.get_or_init(|| crate::agent_sources::AGENT_SOURCE_IDS.into_iter()
        .map(|source| (source, SourceInventory::default())).collect());
    let Some(slot) = inventories.get(source) else { return Arc::new(Vec::new()) };
    let mut inventory = if wait {
        slot.inventory.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    } else {
        match slot.inventory.try_lock() {
            Ok(guard) => guard,
            Err(std::sync::TryLockError::Poisoned(poisoned)) => poisoned.into_inner(),
            // Hook responses must not wait for a background filesystem sweep.
            // A missing new path remains in the durable receipt for backfill.
            Err(std::sync::TryLockError::WouldBlock) => return Arc::clone(
                &slot.published.lock().unwrap_or_else(|poisoned| poisoned.into_inner()),
            ),
        }
    };
    let roots = crate::agent_sources::transcript_roots(source).into_iter()
        .map(|root| (source.to_string(), root)).collect();
    let snapshot = inventory.snapshot(roots, force);
    *slot.published.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = Arc::clone(&snapshot);
    snapshot
}

/// Missing-hook lookup touches only its requested source and never waits on a
/// background refresh. Ordinary all-source consumers refresh sources in turn.
pub(super) fn source_transcript_inventory(source: &str) -> Arc<Vec<TranscriptFile>> {
    source_inventory(source, false, false)
}

pub(crate) fn native_transcript_inventory(force_reconcile: bool) -> Arc<Vec<TranscriptFile>> {
    let mut files = Vec::new();
    for source in crate::agent_sources::AGENT_SOURCE_IDS {
        files.extend(source_inventory(source, force_reconcile, true).iter().cloned());
    }
    // Preserve registry source order (including Claude/ZCode mirror ownership)
    // and each source's newest-first order.
    Arc::new(files)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inventory_finds_archived_codex_transcripts_without_hook_receipts() {
        let root = std::env::temp_dir().join(format!("transcript-inventory-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(root.join("nested")).unwrap();
        let session_id = "019f8e90-36f5-71a2-bddf-f0fe9ee23b57";
        let path = root.join("nested").join(format!("rollout-2026-07-23T18-40-43-{session_id}.jsonl"));
        std::fs::write(&path, "{}").unwrap();
        let mut inventory = Inventory::default();
        let found = inventory.snapshot(vec![("codex".into(), root.clone())], false);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].session_id, session_id);
        assert_eq!(found[0].path, path);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn inventory_refreshes_appends_discovers_new_directories_and_removes_deleted_files() {
        let root = std::env::temp_dir().join(format!("transcript-inventory-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let first = root.join("first.jsonl");
        std::fs::write(&first, "a").unwrap();
        let roots = vec![("claude".into(), root.clone())];
        let mut inventory = Inventory::default();
        assert_eq!(inventory.snapshot(roots.clone(), false)[0].size, 1);
        std::fs::write(&first, "appended").unwrap();
        inventory.checked_at = None;
        assert_eq!(inventory.snapshot(roots.clone(), false)[0].size, 8);
        let nested = root.join("nested");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::write(nested.join("second.jsonl"), "b").unwrap();
        // Force low-frequency reconciliation, independent of filesystem mtime resolution.
        assert_eq!(inventory.snapshot(roots.clone(), true).len(), 2);
        std::fs::remove_file(&first).unwrap();
        inventory.checked_at = None;
        assert_eq!(inventory.snapshot(roots, false).len(), 1);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn inventory_finishes_large_flat_directories_and_resumes_deferred_children() {
        let root = std::env::temp_dir().join(format!("transcript-inventory-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        for name in ["one", "two", "tail"] {
            std::fs::write(root.join(format!("{name}.jsonl")), "a").unwrap();
        }
        let roots = vec![("claude".into(), root.clone())];
        let mut inventory = Inventory::default();
        assert_eq!(inventory.snapshot_with_budget(roots.clone(), false, 2).len(), 3);
        for name in ["child-a", "child-b"] {
            let child = root.join(name);
            std::fs::create_dir_all(&child).unwrap();
            std::fs::write(child.join("nested.jsonl"), "b").unwrap();
        }
        inventory.snapshot_with_budget(roots.clone(), true, 2);
        assert!(!inventory.deferred.is_empty());
        inventory.checked_at = None;
        let next = inventory.snapshot_with_budget(roots, false, 2);
        assert_eq!(next.len(), 5);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn explicit_symlink_root_is_followed_without_following_descendant_links() {
        let root = std::env::temp_dir().join(format!("transcript-inventory-{}", uuid::Uuid::new_v4()));
        let physical = root.join("physical");
        std::fs::create_dir_all(&physical).unwrap();
        std::fs::write(physical.join("session.jsonl"), "a").unwrap();
        std::os::unix::fs::symlink(&physical, root.join("selected")).unwrap();
        std::os::unix::fs::symlink(&physical, physical.join("cycle")).unwrap();
        let mut inventory = Inventory::default();
        assert_eq!(inventory.snapshot(vec![("claude".into(), root.join("selected"))], false).len(), 1);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn changing_roots_drops_old_inventory_and_snapshot_reuses_pending_metadata() {
        let root = std::env::temp_dir().join(format!("transcript-inventory-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("session.jsonl"), "a").unwrap();
        let roots = vec![("claude".into(), root.clone())];
        let mut inventory = Inventory::default();
        let first = inventory.snapshot(roots.clone(), false);
        assert!(Arc::ptr_eq(&first, &inventory.snapshot(roots, false)));
        assert!(inventory.snapshot(Vec::new(), false).is_empty());
        std::fs::remove_dir_all(root).unwrap();
    }
}
