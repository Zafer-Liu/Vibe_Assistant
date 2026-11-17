//! Batched durable checkpoints, independent for conversation and usage parsing.
use super::{TelemetryStore, TranscriptFile};
use chrono::{DateTime, Utc};
use rusqlite::{params, OptionalExtension};
use std::collections::HashMap;

impl TelemetryStore {
    pub(crate) fn transcript_scan_candidates(
        &self,
        files: &[TranscriptFile],
        retry_failed: bool,
    ) -> Result<Vec<TranscriptFile>, String> {
        let checkpoints = self.with_read_connection(|conn| {
            let mut statement = conn.prepare("SELECT source, transcript_path, file_size, modified_at, state FROM transcript_scan_states")
                .map_err(|error| error.to_string())?;
            let rows = statement.query_map([], |row| Ok((
                (row.get::<_, String>(0)?, row.get::<_, String>(1)?),
                (row.get::<_, i64>(2)?, row.get::<_, String>(3)?, row.get::<_, String>(4)?),
            ))).map_err(|error| error.to_string())?;
            rows.collect::<Result<HashMap<_, _>, _>>().map_err(|error| error.to_string())
        })?;
        Ok(files.iter().filter(|file| {
            match checkpoints.get(&(file.source.clone(), file.path.display().to_string())) {
                Some((size, modified, state)) => *size != file.size as i64 || modified != &file.modified_text() || (retry_failed && state == "failed"),
                None => true,
            }
        }).cloned().collect())
    }

    pub(crate) fn usage_scan_checkpoints(&self) -> Result<HashMap<(String, String), DateTime<Utc>>, String> {
        self.with_read_connection(|conn| {
            let mut statement = conn.prepare("SELECT source, session_id, refreshed_at FROM session_token_usage")
                .map_err(|error| error.to_string())?;
            let rows = statement.query_map([], |row| Ok((
                (row.get::<_, String>(0)?, row.get::<_, String>(1)?), row.get::<_, String>(2)?,
            ))).map_err(|error| error.to_string())?;
            let mut checkpoints = HashMap::new();
            for row in rows {
                let (key, refreshed) = row.map_err(|error| error.to_string())?;
                if let Ok(time) = DateTime::parse_from_rfc3339(&refreshed) {
                    checkpoints.insert(key, time.with_timezone(&Utc));
                }
            }
            Ok(checkpoints)
        })
    }

    pub(crate) fn record_transcript_scan_snapshot(
        &self,
        file: &TranscriptFile,
        result: Result<(), &str>,
    ) {
        // Store metadata sampled BEFORE reading. If a transcript grows during
        // parsing the next pass must still notice it, even after a success.
        let (state, error) = match result {
            Ok(()) => ("stored", None),
            Err(detail) => ("failed", Some(detail)),
        };
        if let Ok(conn) = self.conn.lock() {
            let _ = conn.execute(
                "INSERT INTO transcript_scan_states (source, transcript_path, file_size, modified_at, state, error_detail, session_id, last_scanned_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
                 ON CONFLICT(source, transcript_path) DO UPDATE SET file_size=excluded.file_size, modified_at=excluded.modified_at,
                   state=excluded.state, error_detail=excluded.error_detail, session_id=excluded.session_id, last_scanned_at=excluded.last_scanned_at",
                params![file.source, file.path.display().to_string(), file.size as i64, file.modified_text(), state, error, file.session_id, Utc::now().to_rfc3339()],
            );
        }
    }

    pub(crate) fn conversation_checkpoint_matches(&self, file: &TranscriptFile, retry_failed: bool) -> bool {
        // The explicit single-session DSH path still uses a single lookup.
        self.with_read_connection(|conn| {
            let previous = conn.query_row(
                "SELECT file_size, modified_at, state FROM transcript_scan_states WHERE source = ?1 AND transcript_path = ?2",
                params![file.source, file.path.display().to_string()],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?, row.get::<_, String>(2)?)),
            ).optional().map_err(|error| error.to_string())?;
            Ok(previous.is_some_and(|(size, modified, state)| size == file.size as i64 && modified == file.modified_text() && !(retry_failed && state == "failed")))
        }).unwrap_or(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::time::{Duration, SystemTime};

    #[test]
    fn usage_checkpoints_keep_sources_separate_and_retry_invalid_timestamps() {
        let store = TelemetryStore::in_memory();
        store.conn.lock().unwrap().execute_batch(
            "INSERT INTO session_token_usage (source, session_id, origin, refreshed_at) VALUES
             ('claude', 'shared', 'native_transcript', '2026-11-08T01:00:00Z'),
             ('codex', 'shared', 'native_transcript', '2026-11-08T02:00:00Z'),
             ('kimi', 'invalid', 'native_transcript', 'invalid');",
        ).unwrap();
        let checkpoints = store.usage_scan_checkpoints().unwrap();
        assert_eq!(checkpoints.len(), 2);
        assert_ne!(checkpoints[&("claude".into(), "shared".into())], checkpoints[&("codex".into(), "shared".into())]);
        assert!(!checkpoints.contains_key(&("kimi".into(), "invalid".into())));
    }

    #[test]
    fn batched_checkpoints_preserve_failed_retry_and_before_read_metadata() {
        let store = TelemetryStore::in_memory();
        let file = TranscriptFile {
            source: "claude".into(), session_id: "session".into(), path: PathBuf::from("session.jsonl"),
            size: 1, modified: SystemTime::UNIX_EPOCH + Duration::from_secs(10),
        };
        store.record_transcript_scan_snapshot(&file, Err("invalid"));
        assert!(store.transcript_scan_candidates(&[file.clone()], false).unwrap().is_empty());
        assert_eq!(store.transcript_scan_candidates(&[file.clone()], true).unwrap().len(), 1);
        let mut appended = file.clone();
        appended.size += 1;
        assert_eq!(store.transcript_scan_candidates(&[appended], false).unwrap().len(), 1);
        store.record_transcript_scan_snapshot(&file, Ok(()));
        assert!(store.transcript_scan_candidates(&[file], true).unwrap().is_empty());
    }
}
