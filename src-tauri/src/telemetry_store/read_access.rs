//! Bounded, lazy read connections. The existing main connection remains the
//! serialized writer; WAL readers do not queue behind its long transactions.
use super::*;
use rusqlite::OpenFlags;
use std::sync::atomic::{AtomicUsize, Ordering};

#[cfg(test)]
#[path = "read_access_tests.rs"]
mod tests;

const READER_COUNT: usize = 2;

pub(super) struct ReadConnections {
    path: Option<PathBuf>,
    readers: Vec<Mutex<Option<Connection>>>,
    analytics: Mutex<Option<Connection>>,
    next: AtomicUsize,
}

impl ReadConnections {
    pub(super) fn new(conn: &Connection) -> Self {
        let path = conn.path().filter(|path| !path.is_empty() && *path != ":memory:").map(PathBuf::from);
        Self {
            path,
            readers: (0..READER_COUNT).map(|_| Mutex::new(None)).collect(),
            analytics: Mutex::new(None),
            next: AtomicUsize::new(0),
        }
    }

    fn open(&self) -> Result<Connection, String> {
        let path = self.path.as_ref().ok_or("read connection requires a file database")?;
        let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX)
            .map_err(|error| error.to_string())?;
        conn.busy_timeout(std::time::Duration::from_secs(2)).map_err(|error| error.to_string())?;
        // READ_ONLY protects the durable ledger, while TEMP tables remain
        // writable for the dedicated analytics snapshot. No migrations here.
        conn.pragma_update(None, "foreign_keys", true).map_err(|error| error.to_string())?;
        Ok(conn)
    }
}

impl TelemetryStore {
    /// Immutable path metadata is available even while the writer is busy.
    pub(crate) fn read_database_path(&self) -> Option<&std::path::Path> {
        self.read_connections.path.as_deref()
    }

    /// Settings used by interactive reads must not wait on long writer jobs.
    /// Keep the writer-backed getter available for existing transaction users.
    pub(crate) fn app_setting_get_readonly<T: DeserializeOwned>(&self, key: &str) -> Option<T> {
        let raw: Option<String> = self.with_read_connection(|conn| {
            conn.query_row("SELECT value_json FROM app_settings WHERE setting_key = ?1", [key], |row| row.get(0))
                .optional().map_err(|error| error.to_string())
        }).ok()?;
        raw.and_then(|value| serde_json::from_str(&value).ok())
    }

    pub(crate) fn with_read_connection<T>(
        &self, read: impl FnOnce(&mut Connection) -> Result<T, String>,
    ) -> Result<T, String> {
        let pool = &self.read_connections;
        if pool.path.is_none() {
            // Isolated in-memory stores cannot be reopened by filename.
            let mut conn = self.conn.lock().map_err(|_| "telemetry store lock poisoned".to_string())?;
            return read(&mut conn);
        }
        let start = pool.next.fetch_add(1, Ordering::Relaxed) % pool.readers.len();
        for offset in 0..pool.readers.len() {
            let slot = &pool.readers[(start + offset) % pool.readers.len()];
            if let Ok(mut guard) = slot.try_lock() {
                if guard.is_none() { *guard = Some(pool.open()?); }
                return read(guard.as_mut().ok_or("read connection unavailable")?);
            }
        }
        let mut guard = pool.readers[start].lock().map_err(|_| "read connection lock poisoned".to_string())?;
        if guard.is_none() { *guard = Some(pool.open()?); }
        read(guard.as_mut().ok_or("read connection unavailable")?)
    }

    /// Evidence never queues indefinitely behind an occupied reader. The SQL
    /// watcher is joined while the mutex is still held, before this connection
    /// becomes available to the next ordinary or evidence query.
    pub(crate) fn with_budgeted_read_connection<T>(
        &self, budget: &crate::memory_retrieval::RecallBudget,
        read: impl FnOnce(&mut Connection) -> Result<T, String>,
    ) -> Result<T, String> {
        fn execute<T>(conn: &mut Connection, budget: &crate::memory_retrieval::RecallBudget,
            read: impl FnOnce(&mut Connection) -> Result<T, String>) -> Result<T, String> {
            budget.remaining()?;
            let timeout = super::recall_traces::RestoreBusyTimeout::new(conn)
                .map_err(|_| "unavailable".to_string())?;
            timeout.conn.busy_timeout(budget.remaining()?.min(std::time::Duration::from_millis(10)))
                .map_err(|_| "unavailable".to_string())?;
            let watch = crate::memory_evidence::SqlWatch::new(timeout.conn, budget)?;
            let result = read(&mut *timeout.conn);
            drop(watch);
            let restored = timeout.restore().map_err(|_| "unavailable".to_string());
            match (result, restored) { (Ok(value), Ok(())) => Ok(value), (Err(error), _) | (_, Err(error)) => Err(error) }
        }
        let pool = &self.read_connections;
        let mut read = Some(read);
        loop {
            budget.remaining()?;
            if pool.path.is_none() {
                if let Ok(mut conn) = self.conn.try_lock() {
                    return execute(&mut conn, budget, read.take().expect("reader called once"));
                }
            } else {
                let start = pool.next.fetch_add(1, Ordering::Relaxed) % pool.readers.len();
                for offset in 0..pool.readers.len() {
                    if let Ok(mut guard) = pool.readers[(start+offset) % pool.readers.len()].try_lock() {
                        if guard.is_none() { *guard = Some(pool.open().map_err(|_| "unavailable".to_string())?); }
                        return execute(guard.as_mut().ok_or("unavailable")?, budget, read.take().expect("reader called once"));
                    }
                }
            }
            std::thread::park_timeout(budget.remaining()?.min(std::time::Duration::from_millis(2)));
        }
    }

    /// Keep one analytics connection, so charts share one TEMP snapshot rather
    /// than copying the full ledger into each ordinary reader.
    pub(super) fn with_usage_connection<T>(
        &self, read: impl FnOnce(&mut Connection) -> Result<T, String>,
    ) -> Result<T, String> {
        let pool = &self.read_connections;
        if pool.path.is_none() {
            let mut conn = self.conn.lock().map_err(|_| "telemetry store lock poisoned".to_string())?;
            return read(&mut conn);
        }
        let mut guard = pool.analytics.lock().map_err(|_| "analytics connection lock poisoned".to_string())?;
        if guard.is_none() {
            *guard = Some(pool.open()?);
            *self.usage_query_cache.lock().map_err(|_| "usage query cache poisoned".to_string())? = None;
        }
        read(guard.as_mut().ok_or("analytics connection unavailable")?)
    }

    /// Backup restore can replace a database with the same revision counter.
    /// Drop reader snapshots explicitly instead of trusting counter equality.
    pub(crate) fn invalidate_read_connections(&self) -> Result<(), String> {
        let pool = &self.read_connections;
        let mut analytics = pool.analytics.lock().map_err(|_| "analytics connection lock poisoned".to_string())?;
        *analytics = None;
        *self.usage_query_cache.lock().map_err(|_| "usage query cache poisoned".to_string())? = None;
        for slot in &pool.readers {
            *slot.lock().map_err(|_| "read connection lock poisoned".to_string())? = None;
        }
        Ok(())
    }
}
