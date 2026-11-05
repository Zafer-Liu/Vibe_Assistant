//! Configuration export / import for cross-device migration.
//!
//! Exports a ZIP containing telemetry.sqlite3, shared-skills/, and
//! memory-backups/ — everything needed to reproduce the app state on
//! another machine.

use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use rusqlite::{backup, Connection};
use serde::Serialize;
use tauri::State;
use zip::ZipArchive;
use zip::ZipWriter;

use crate::telemetry_store::TelemetryStore;

const DB_FILENAME: &str = "telemetry.sqlite3";
const SKILLS_DIR: &str = "shared-skills";
const BACKUPS_DIR: &str = "memory-backups";
const MANIFEST_FILENAME: &str = "manifest.json";
const ARCHIVE_VERSION: u32 = 1;

fn data_dir() -> PathBuf {
    dirs_next::data_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("agent-manager")
}

#[derive(Serialize)]
pub struct ExportManifest {
    version: u32,
    created_at: String,
    app_version: String,
    tables: Vec<String>,
    skill_count: usize,
    backup_count: usize,
}

/// Recursively add a directory to a ZIP archive.
fn add_dir_to_zip<W: Write + std::io::Seek>(
    zip: &mut ZipWriter<W>,
    base: &Path,
    zip_prefix: &str,
) -> Result<usize, String> {
    let mut count = 0usize;
    if !base.exists() {
        return Ok(0);
    }
    let options = zip::write::SimpleFileOptions::default();
    for entry in walkdir(base)? {
        let rel = entry.strip_prefix(base).map_err(|e| e.to_string())?;
        let zip_name = if zip_prefix.is_empty() {
            rel.to_string_lossy().replace('\\', "/")
        } else {
            format!(
                "{}/{}",
                zip_prefix,
                rel.to_string_lossy().replace('\\', "/")
            )
        };
        if entry.is_dir() {
            zip.add_directory(&zip_name, options)
                .map_err(|e| e.to_string())?;
            continue;
        }
        zip.start_file(&zip_name, options)
            .map_err(|e| e.to_string())?;
        let mut f = fs::File::open(&entry).map_err(|e| e.to_string())?;
        let mut buf = Vec::with_capacity(8192);
        f.read_to_end(&mut buf).map_err(|e| e.to_string())?;
        zip.write_all(&buf).map_err(|e| e.to_string())?;
        count += 1;
    }
    Ok(count)
}

/// Simple recursive directory walker that returns all paths (files + dirs).
fn walkdir(root: &Path) -> Result<Vec<PathBuf>, String> {
    let mut result = Vec::new();
    walkdir_inner(root, &mut result)?;
    result.sort();
    Ok(result)
}

fn walkdir_inner(dir: &Path, out: &mut Vec<PathBuf>) -> Result<(), String> {
    let entries = fs::read_dir(dir).map_err(|e| e.to_string())?;
    for entry in entries.flatten() {
        let path = entry.path();
        out.push(path.clone());
        if path.is_dir() {
            walkdir_inner(&path, out)?;
        }
    }
    Ok(())
}

#[tauri::command]
pub fn config_export(
    dest_path: String,
    store: State<'_, TelemetryStore>,
) -> Result<ExportManifest, String> {
    let dir = data_dir();
    let db_path = dir.join(DB_FILENAME);

    // Flush WAL into the main database file so the copy is consistent.
    {
        let conn = store
            .conn
            .lock()
            .map_err(|_| "database lock poisoned".to_string())?;
        conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")
            .map_err(|e| e.to_string())?;
    }

    // Create the ZIP at a temp path first, then move to the target.
    let tmp =
        tempfile::NamedTempFile::new().map_err(|e| format!("failed to create temp file: {e}"))?;
    let file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(tmp.path())
        .map_err(|e| e.to_string())?;
    let mut zip = ZipWriter::new(file);
    let options = zip::write::SimpleFileOptions::default();

    // 1. Database file
    let mut tables = Vec::new();
    if db_path.exists() {
        zip.start_file(DB_FILENAME, options)
            .map_err(|e| e.to_string())?;
        let mut f = fs::File::open(&db_path).map_err(|e| e.to_string())?;
        let mut buf = Vec::with_capacity(65536);
        f.read_to_end(&mut buf).map_err(|e| e.to_string())?;
        zip.write_all(&buf).map_err(|e| e.to_string())?;

        // Collect table names for the manifest.
        if let Ok(conn) = Connection::open(&db_path) {
            let mut stmt = conn
                .prepare("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name")
                .map_err(|e| e.to_string())?;
            let rows = stmt
                .query_map([], |row| row.get::<_, String>(0))
                .map_err(|e| e.to_string())?;
            for row in rows.flatten() {
                tables.push(row);
            }
        }
    }

    // 2. Shared skills directory
    let skills_path = dir.join(SKILLS_DIR);
    let skill_count = add_dir_to_zip(&mut zip, &skills_path, SKILLS_DIR)?;

    // 3. Memory backups directory
    let backups_path = dir.join(BACKUPS_DIR);
    let backup_count = add_dir_to_zip(&mut zip, &backups_path, BACKUPS_DIR)?;

    // 4. Manifest
    let manifest = ExportManifest {
        version: ARCHIVE_VERSION,
        created_at: chrono::Utc::now().to_rfc3339(),
        app_version: env!("CARGO_PKG_VERSION").to_string(),
        tables,
        skill_count,
        backup_count,
    };
    zip.start_file(MANIFEST_FILENAME, options)
        .map_err(|e| e.to_string())?;
    let manifest_json = serde_json::to_string_pretty(&manifest).map_err(|e| e.to_string())?;
    zip.write_all(manifest_json.as_bytes())
        .map_err(|e| e.to_string())?;

    zip.finish().map_err(|e| e.to_string())?;

    // Move temp file to destination (handles cross-volume via copy fallback).
    if fs::rename(tmp.path(), &dest_path).is_err() {
        fs::copy(tmp.path(), &dest_path).map_err(|e| format!("failed to write output: {e}"))?;
        let _ = fs::remove_file(tmp.path());
    }

    Ok(manifest)
}

#[derive(Serialize)]
pub struct ImportResult {
    pub tables_restored: usize,
    pub skills_restored: usize,
    pub backups_restored: usize,
    pub app_restart_required: bool,
}

#[tauri::command]
pub fn config_import(
    source_path: String,
    store: State<'_, TelemetryStore>,
) -> Result<ImportResult, String> {
    let dir = data_dir();
    fs::create_dir_all(&dir).map_err(|e| e.to_string())?;

    // Open the ZIP archive.
    let file = fs::File::open(&source_path).map_err(|e| e.to_string())?;
    let mut archive = ZipArchive::new(file).map_err(|e| e.to_string())?;

    // Extract the database to a temp file, then use SQLite Backup API to
    // copy its contents into the live connection.
    let mut tables_restored = 0usize;
    if let Ok(mut entry) = archive.by_name(DB_FILENAME) {
        let tmp = tempfile::NamedTempFile::new().map_err(|e| format!("temp file: {e}"))?;
        let mut out = fs::File::create(tmp.path()).map_err(|e| e.to_string())?;
        let mut buf = [0u8; 65536];
        loop {
            let n = entry.read(&mut buf).map_err(|e| e.to_string())?;
            if n == 0 {
                break;
            }
            out.write_all(&buf[..n]).map_err(|e| e.to_string())?;
        }
        drop(out);

        // Open source connection to the extracted database.
        let src_conn = Connection::open(tmp.path()).map_err(|e| e.to_string())?;

        // Count tables for reporting.
        let count: i64 = src_conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table'",
                [],
                |row| row.get(0),
            )
            .map_err(|e| e.to_string())?;
        tables_restored = count as usize;

        // Use SQLite Online Backup API to replace live DB contents.
        let mut dst_conn = store
            .conn
            .lock()
            .map_err(|_| "database lock poisoned".to_string())?;
        let backup = backup::Backup::new(&src_conn, &mut *dst_conn).map_err(|e| e.to_string())?;
        backup
            .run_to_completion(100, std::time::Duration::from_millis(250), None)
            .map_err(|e| e.to_string())?;
    }

    // Extract shared-skills/ and memory-backups/ directories.
    let skills_restored = extract_dir(&mut archive, SKILLS_DIR, &dir.join(SKILLS_DIR))?;
    let backups_restored = extract_dir(&mut archive, BACKUPS_DIR, &dir.join(BACKUPS_DIR))?;

    // 恢复的数据库带着源机器的云端设备身份。若源机仍在同步，两台机器会争用
    // 同一个单写者聚合对象；轮换身份并让旧身份退休，避免互相覆盖。
    if tables_restored > 0 {
        crate::cloud_sync::retire_cloud_device_identity(store.inner())?;
    }

    Ok(ImportResult {
        tables_restored,
        skills_restored,
        backups_restored,
        app_restart_required: true,
    })
}

/// Extract all entries whose name starts with `prefix/` into `dest_dir`.
/// Returns the number of files written.
fn extract_dir<R: Read + std::io::Seek>(
    archive: &mut ZipArchive<R>,
    prefix: &str,
    dest_dir: &Path,
) -> Result<usize, String> {
    let mut count = 0usize;

    // Collect matching entry names first (avoids holding a borrow while writing).
    let names: Vec<String> = (0..archive.len())
        .filter_map(|i| {
            let entry = archive.by_index(i).ok()?;
            let name = entry.name().to_string();
            if name.starts_with(&format!("{}/", prefix)) || name == prefix {
                Some(name)
            } else {
                None
            }
        })
        .collect();

    if names.is_empty() {
        return Ok(0);
    }

    fs::create_dir_all(dest_dir).map_err(|e| e.to_string())?;

    for name in &names {
        let rel = name.strip_prefix(&format!("{}/", prefix)).unwrap_or("");
        if rel.is_empty() {
            continue;
        }
        let dest_path = dest_dir.join(rel.replace('\\', "/"));

        // Directory entry.
        if name.ends_with('/') {
            fs::create_dir_all(&dest_path).map_err(|e| e.to_string())?;
            continue;
        }

        if let Some(parent) = dest_path.parent() {
            fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }

        let mut entry = archive.by_name(name).map_err(|e| e.to_string())?;
        let mut out = fs::File::create(&dest_path).map_err(|e| e.to_string())?;
        let mut buf = [0u8; 65536];
        loop {
            let n = entry.read(&mut buf).map_err(|e| e.to_string())?;
            if n == 0 {
                break;
            }
            out.write_all(&buf[..n]).map_err(|e| e.to_string())?;
        }
        count += 1;
    }

    Ok(count)
}
