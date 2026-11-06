use super::*;

#[cfg(test)]
#[path = "cloud_usage_tests.rs"]
mod tests;

/// The local export and every dashboard use the same session-fallback timestamp.
const LOCAL_FEED: &str = "
    SELECT record_id, source, session_id,
           strftime('%Y-%m-%dT%H:%M:%fZ', occurred_at) AS occurred_at,
           model, record_kind, input_tokens, output_tokens, cached_tokens, origin
      FROM usage_records
    UNION ALL
    SELECT 'session-total:' || u.source || ':' || u.session_id, u.source, u.session_id,
           strftime('%Y-%m-%dT%H:%M:%fZ', COALESCE(
               (SELECT MAX(e.occurred_at) FROM agent_events e
                 WHERE e.source = u.source AND e.session_id = u.session_id), u.refreshed_at)),
           NULL, 'session_total', u.input_tokens, u.output_tokens, u.cached_tokens, u.origin
      FROM session_token_usage u
     WHERE NOT EXISTS (SELECT 1 FROM usage_records r
                        WHERE r.source = u.source AND r.session_id = u.session_id)
";

const SIGNATURE: &str = "json_array(record_id, source, session_id, occurred_at, model,
    record_kind, input_tokens, output_tokens, cached_tokens, origin)";

pub(super) fn local_feed_sql() -> String {
    format!("WITH local_feed AS ({LOCAL_FEED}) ")
}

fn local_cte() -> String {
    format!("WITH local_feed AS ({LOCAL_FEED}),
        signed_local AS (SELECT *, {SIGNATURE} AS signature FROM local_feed)")
}

/// A copied database keeps stable request IDs. Rank duplicates identically on
/// every device; actual provider counters outrank estimates, then newer edits win.
pub(super) fn feed_sql() -> String {
    format!("{}, candidates AS (
        SELECT l.*, CASE WHEN s.signature = l.signature THEN s.updated_at
                    ELSE strftime('%Y-%m-%dT%H:%M:%fZ', 'now') END AS updated_at
          FROM signed_local l LEFT JOIN usage_sync_local_state s USING(record_id)
        UNION ALL
        SELECT record_id, source, session_id, occurred_at, model, record_kind,
               input_tokens, output_tokens, cached_tokens, origin, {SIGNATURE}, updated_at
          FROM usage_remote_records
    ), ranked AS (
        SELECT *, ROW_NUMBER() OVER (
            PARTITION BY source, record_id
            ORDER BY (origin NOT LIKE 'estimated%') DESC, updated_at DESC, signature DESC
        ) AS position FROM candidates
    ), exact_feed AS (
        SELECT * FROM ranked r WHERE position = 1
          AND (record_kind != 'session_total' OR NOT EXISTS (
              SELECT 1 FROM ranked detail WHERE detail.position = 1
                AND detail.source = r.source AND detail.session_id = r.session_id
                AND detail.record_kind != 'session_total'))
    ), usage_feed AS (
        SELECT record_id, source, session_id, occurred_at, model, record_kind,
               input_tokens, output_tokens, cached_tokens, origin,
               1 AS record_count, NULL AS day_only FROM exact_feed
        UNION ALL
        SELECT 'remote:' || device || ':' || day || ':' || source,
               source, device, day, NULL, 'remote_total',
               input_tokens, output_tokens, cached_tokens, 'remote', record_count, day
          FROM usage_remote_totals legacy
         WHERE NOT EXISTS (SELECT 1 FROM usage_remote_ledgers v2 WHERE v2.device = legacy.device)
    ) ", local_cte())
}

/// Daily-only legacy data is filtered as dates, never as a fictitious UTC noon.
pub(super) const FILTER: &str = " WHERE (?3 IS NULL OR source = ?3) AND (
    (day_only IS NULL AND (?1 IS NULL OR occurred_at >= ?1) AND (?2 IS NULL OR occurred_at < ?2))
    OR (day_only IS NOT NULL
        AND (?1 IS NULL OR day_only >= strftime('%Y-%m-%d', ?1, 'localtime'))
        AND (?2 IS NULL OR day_only < strftime('%Y-%m-%d', ?2, 'localtime')
             OR (day_only = strftime('%Y-%m-%d', ?2, 'localtime')
                 AND strftime('%H:%M:%f', ?2, 'localtime') > '00:00:00.000')))) ";

pub(super) fn initialize(conn: &Connection) -> Result<(), String> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS usage_sync_local_state (
             record_id TEXT PRIMARY KEY, signature TEXT NOT NULL, updated_at TEXT NOT NULL
         );
         CREATE TABLE IF NOT EXISTS usage_remote_ledgers (
             device TEXT PRIMARY KEY, revision INTEGER NOT NULL
         );
         CREATE TABLE IF NOT EXISTS usage_remote_records (
             device TEXT NOT NULL, record_id TEXT NOT NULL, source TEXT NOT NULL,
             session_id TEXT NOT NULL, occurred_at TEXT NOT NULL, model TEXT,
             record_kind TEXT NOT NULL, input_tokens INTEGER NOT NULL,
             output_tokens INTEGER NOT NULL, cached_tokens INTEGER NOT NULL,
             origin TEXT NOT NULL, updated_at TEXT NOT NULL,
             PRIMARY KEY(device, record_id)
         );
         CREATE INDEX IF NOT EXISTS idx_usage_remote_records_session
             ON usage_remote_records(source, session_id);
         CREATE INDEX IF NOT EXISTS idx_usage_remote_records_identity
             ON usage_remote_records(source, record_id);",
    ).map_err(|error| error.to_string())
}

impl TelemetryStore {
    /// Only locally captured records are exported; downloaded rows never echo.
    pub fn cloud_usage_ledger(&self) -> Result<CloudUsageLedger, String> {
        let mut conn = self.conn.lock().map_err(|_| "telemetry store lock poisoned".to_string())?;
        let tx = conn.transaction().map_err(|error| error.to_string())?;
        tx.execute(&format!("{} INSERT INTO usage_sync_local_state (record_id, signature, updated_at)
            SELECT record_id, signature, strftime('%Y-%m-%dT%H:%M:%fZ', 'now') FROM signed_local WHERE 1
            ON CONFLICT(record_id) DO UPDATE SET signature = excluded.signature, updated_at = excluded.updated_at
            WHERE usage_sync_local_state.signature != excluded.signature", local_cte()), [])
            .map_err(|error| error.to_string())?;
        let records = {
            let sql = format!("{} SELECT l.record_id, l.source, l.session_id, l.occurred_at, l.model,
                l.record_kind, l.input_tokens, l.output_tokens, l.cached_tokens, l.origin, s.updated_at
                FROM signed_local l JOIN usage_sync_local_state s USING(record_id) ORDER BY l.record_id", local_cte());
            let mut statement = tx.prepare(&sql).map_err(|error| error.to_string())?;
            let rows = statement.query_map([], |row| Ok(CloudUsageRecord {
                record: TelemetryUsageRecord {
                    record_id: row.get(0)?, source: row.get(1)?, session_id: row.get(2)?,
                    occurred_at: row.get(3)?, model: row.get(4)?, record_kind: row.get(5)?,
                    input_tokens: row.get(6)?, output_tokens: row.get(7)?,
                    cached_tokens: row.get(8)?, origin: row.get(9)?,
                },
                updated_at: row.get(10)?,
            })).map_err(|error| error.to_string())?;
            rows.collect::<Result<Vec<_>, _>>().map_err(|error| error.to_string())?
        };
        tx.commit().map_err(|error| error.to_string())?;
        Ok(CloudUsageLedger { format: 2, records })
    }

    pub fn replace_remote_usage_ledger(
        &self, device: &str, revision: i64, ledger: &CloudUsageLedger,
    ) -> Result<u64, String> {
        if ledger.format != 2 { return Err("云端用量账本版本不支持".into()); }
        let mut conn = self.conn.lock().map_err(|_| "telemetry store lock poisoned".to_string())?;
        let tx = conn.transaction().map_err(|error| error.to_string())?;
        let previous: Option<i64> = tx.query_row(
            "SELECT revision FROM usage_remote_ledgers WHERE device = ?1", [device], |row| row.get(0),
        ).optional().map_err(|error| error.to_string())?;
        if previous.is_some_and(|saved| saved > revision) { return Ok(0); }
        tx.execute("DELETE FROM usage_remote_records WHERE device = ?1", [device])
            .map_err(|error| error.to_string())?;
        let mut seen = HashSet::new();
        for item in &ledger.records {
            let row = &item.record;
            let occurred_at = normalize_occurred_at(Some(&row.occurred_at)).ok_or("云端用量时间无效")?;
            let updated_at = normalize_occurred_at(Some(&item.updated_at)).ok_or("云端用量更新时间无效")?;
            if row.record_id.trim().is_empty() || row.source.trim().is_empty()
                || row.session_id.trim().is_empty() || row.origin.trim().is_empty()
                || row.input_tokens < 0 || row.output_tokens < 0 || row.cached_tokens < 0
                || row.cached_tokens > row.input_tokens || !seen.insert(&row.record_id)
                || !matches!(row.record_kind.as_str(), "response" | "session_total" | "estimated")
            { return Err("云端用量记录无效".into()); }
            tx.execute(
                "INSERT INTO usage_remote_records
                 (device, record_id, source, session_id, occurred_at, model, record_kind,
                  input_tokens, output_tokens, cached_tokens, origin, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
                params![device, row.record_id, row.source, row.session_id, occurred_at, row.model,
                    row.record_kind, row.input_tokens, row.output_tokens, row.cached_tokens, row.origin, updated_at],
            ).map_err(|error| error.to_string())?;
        }
        tx.execute("INSERT INTO usage_remote_ledgers (device, revision) VALUES (?1, ?2)
            ON CONFLICT(device) DO UPDATE SET revision = excluded.revision", params![device, revision])
            .map_err(|error| error.to_string())?;
        // Keep a marker even for an empty v2 ledger, so old daily totals stay suppressed.
        tx.execute("DELETE FROM usage_remote_totals WHERE device = ?1", [device])
            .map_err(|error| error.to_string())?;
        tx.commit().map_err(|error| error.to_string())?;
        Ok(ledger.records.len() as u64)
    }

    pub fn has_remote_usage_ledger(&self, device: &str) -> Result<bool, String> {
        let conn = self.conn.lock().map_err(|_| "telemetry store lock poisoned".to_string())?;
        conn.query_row("SELECT EXISTS(SELECT 1 FROM usage_remote_ledgers WHERE device = ?1)",
            [device], |row| row.get(0)).map_err(|error| error.to_string())
    }
}
