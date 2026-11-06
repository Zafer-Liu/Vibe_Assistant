use super::*;
use std::borrow::Cow;

#[cfg(test)]
#[path = "cloud_usage_tests.rs"]
mod tests;

/// The local export and every dashboard use the same session-fallback timestamp.
const LOCAL_FEED: &str = "
    SELECT record_id, source,
           CASE WHEN trim(session_id) = '' THEN 'legacy-session:' || record_id ELSE session_id END AS session_id,
           strftime('%Y-%m-%dT%H:%M:%fZ', occurred_at) AS occurred_at,
           model, record_kind, input_tokens, output_tokens, cached_tokens, origin
      FROM usage_records
    UNION ALL
    SELECT 'session-total:' || u.source || ':' || u.session_id, u.source,
           CASE WHEN trim(u.session_id) = '' THEN 'legacy-session:session-total:' || u.source || ':' || u.session_id
                ELSE u.session_id END,
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
    ), winners AS MATERIALIZED (
        SELECT * FROM ranked WHERE position = 1
    ), detailed_sessions AS MATERIALIZED (
        SELECT DISTINCT source, session_id FROM winners WHERE record_kind != 'session_total'
    ), exact_feed AS (
        SELECT r.* FROM winners r
        LEFT JOIN detailed_sessions d ON d.source = r.source AND d.session_id = r.session_id
        WHERE r.record_kind != 'session_total' OR d.source IS NULL
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

fn usage_session_id(row: &TelemetryUsageRecord) -> Cow<'_, str> {
    if row.session_id.trim().is_empty() {
        Cow::Owned(format!("legacy-session:{}", row.record_id))
    } else {
        Cow::Borrowed(&row.session_id)
    }
}

fn usage_record_error(index: usize, record_id: &str, reason: &str) -> String {
    // Identify the failing row without exposing session IDs or conversation data.
    let digest = hex(&Sha256::digest(record_id.as_bytes()));
    format!("云端用量记录无效（第 {} 条，记录摘要 {}）：{reason}", index + 1, &digest[..12])
}

/// Codex cumulative snapshots can correct an earlier counter downwards while
/// other counters increase. Preserve those signed deltas; validate their session
/// total instead of rejecting the correction or inflating it by clipping to zero.
fn validate_usage_records(records: &[CloudUsageRecord]) -> Result<(), String> {
    let mut seen = HashSet::new();
    let mut sessions: HashMap<(&str, Cow<'_, str>), (i128, i128, i128, usize)> = HashMap::new();
    for (index, item) in records.iter().enumerate() {
        let row = &item.record;
        let invalid = |reason: &str| usage_record_error(index, &row.record_id, reason);
        if row.record_id.trim().is_empty() { return Err(invalid("record_id 不能为空")); }
        if row.source.trim().is_empty() { return Err(invalid("source 不能为空")); }
        if row.origin.trim().is_empty() { return Err(invalid("origin 不能为空")); }
        if !matches!(row.record_kind.as_str(), "response" | "session_total" | "estimated") {
            return Err(invalid("record_kind 不受支持"));
        }
        if !seen.insert(row.record_id.as_str()) { return Err(invalid("record_id 重复")); }
        if normalize_occurred_at(Some(&row.occurred_at)).is_none() {
            return Err(invalid("occurred_at 不是有效的 RFC3339 时间"));
        }
        if normalize_occurred_at(Some(&item.updated_at)).is_none() {
            return Err(invalid("updated_at 不是有效的 RFC3339 时间"));
        }
        let codex_delta = row.source == "codex" && row.origin == "native_transcript"
            && row.record_kind == "response";
        if !codex_delta {
            if row.input_tokens < 0 || row.output_tokens < 0 || row.cached_tokens < 0 {
                return Err(invalid("input_tokens/output_tokens/cached_tokens 不能为负数"));
            }
            if row.cached_tokens > row.input_tokens {
                return Err(invalid("cached_tokens 超过 input_tokens"));
            }
        }
        let total = sessions.entry((row.source.as_str(), usage_session_id(row)))
            .or_insert((0, 0, 0, index));
        total.0 += i128::from(row.input_tokens);
        total.1 += i128::from(row.output_tokens);
        total.2 += i128::from(row.cached_tokens);
    }
    for (_, (input, output, cached, index)) in sessions {
        if input < 0 || output < 0 || cached < 0 || cached > input
            || input > i128::from(i64::MAX) || output > i128::from(i64::MAX)
        {
            return Err(usage_record_error(index, &records[index].record.record_id,
                "会话合计无效：Token 合计须非负、缓存不超过输入且计数不溢出"));
        }
    }
    Ok(())
}

pub(super) const QUERY_FEED: &str = "WITH usage_feed AS (SELECT * FROM temp.usage_query_feed) ";

pub(super) fn read_usage_buckets(
    conn: &Connection, start_at: Option<&str>, end_at: Option<&str>, source: Option<&str>, bucket: &str,
) -> Result<Vec<TelemetryUsageBucket>, String> {
    let group = if bucket == "hour" {
        "strftime('%Y-%m-%d %H:00', occurred_at, 'localtime')"
    } else {
        "COALESCE(day_only, strftime('%Y-%m-%d', occurred_at, 'localtime'))"
    };
    let precision = if bucket == "hour" { " AND day_only IS NULL" } else { "" };
    let sql = format!("{QUERY_FEED} SELECT {group}, SUM(input_tokens), SUM(output_tokens),
        SUM(cached_tokens), SUM(record_count) FROM usage_feed {FILTER}{precision} GROUP BY 1 ORDER BY 1");
    let mut statement = conn.prepare(&sql).map_err(|error| error.to_string())?;
    let rows = statement.query_map(params![start_at, end_at, source], |row| Ok(TelemetryUsageBucket {
        label: row.get(0)?, input_tokens: row.get(1)?, output_tokens: row.get(2)?,
        cached_tokens: row.get(3)?, record_count: row.get(4)?,
    })).map_err(|error| error.to_string())?;
    rows.collect::<Result<Vec<_>, _>>().map_err(|error| error.to_string())
}

impl TelemetryStore {
    /// Materialize the expensive cross-device deduplication once per database
    /// revision. All chart queries then scan this indexed connection-local table.
    /// data_version catches other processes; total_changes catches this connection.
    pub(super) fn prepare_usage_query_feed(&self, conn: &mut Connection) -> Result<(), String> {
        let data_version: i64 = conn.query_row("PRAGMA main.data_version", [], |row| row.get(0))
            .map_err(|error| error.to_string())?;
        let total_changes: i64 = conn.query_row("SELECT total_changes()", [], |row| row.get(0))
            .map_err(|error| error.to_string())?;
        let mut cache = self.usage_query_cache.lock().map_err(|_| "usage query cache poisoned".to_string())?;
        if cache.as_ref().is_some_and(|saved| saved.data_version == data_version && saved.total_changes == total_changes) {
            return Ok(());
        }
        // Build one consistent read snapshot, even while an MCP process writes.
        // Only TEMP tables are written; the durable usage ledger is untouched.
        let tx = conn.transaction().map_err(|error| error.to_string())?;
        tx.execute_batch(
            "CREATE TEMP TABLE IF NOT EXISTS usage_query_feed (
                 record_id TEXT, source TEXT, session_id TEXT, occurred_at TEXT, model TEXT,
                 record_kind TEXT, input_tokens INTEGER, output_tokens INTEGER, cached_tokens INTEGER,
                 origin TEXT, record_count INTEGER, day_only TEXT
             );
             CREATE INDEX IF NOT EXISTS temp.idx_usage_query_time
                 ON usage_query_feed(occurred_at DESC, record_id) WHERE day_only IS NULL;
             CREATE INDEX IF NOT EXISTS temp.idx_usage_query_source_time
                 ON usage_query_feed(source, occurred_at);
             CREATE INDEX IF NOT EXISTS temp.idx_usage_query_day
                 ON usage_query_feed(day_only) WHERE day_only IS NOT NULL;
             DELETE FROM temp.usage_query_feed;",
        ).map_err(|error| error.to_string())?;
        tx.execute(&format!("{} INSERT INTO temp.usage_query_feed SELECT * FROM usage_feed", feed_sql()), [])
            .map_err(|error| error.to_string())?;
        tx.commit().map_err(|error| error.to_string())?;
        // Remember the counter after our own TEMP writes to avoid rebuilding
        // immediately for the next chart in the same screen.
        let total_changes = conn.query_row("SELECT total_changes()", [], |row| row.get(0))
            .map_err(|error| error.to_string())?;
        *cache = Some(UsageQueryCache { data_version, total_changes });
        Ok(())
    }

    pub(super) fn usage_calendar(
        &self, start_at: Option<&str>, end_at: Option<&str>, source: Option<&str>,
    ) -> Result<Vec<TelemetryUsageBucket>, String> {
        let mut conn = self.conn.lock().map_err(|_| "telemetry store lock poisoned".to_string())?;
        self.prepare_usage_query_feed(&mut conn)?;
        read_usage_buckets(&conn, start_at, end_at, source, "day")
    }

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
        // Export and import must agree on what constitutes a valid full ledger.
        validate_usage_records(&records)?;
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
        validate_usage_records(&ledger.records)?;
        tx.execute("DELETE FROM usage_remote_records WHERE device = ?1", [device])
            .map_err(|error| error.to_string())?;
        for item in &ledger.records {
            let row = &item.record;
            let session_id = usage_session_id(row);
            let occurred_at = normalize_occurred_at(Some(&row.occurred_at)).ok_or("云端用量时间无效")?;
            let updated_at = normalize_occurred_at(Some(&item.updated_at)).ok_or("云端用量更新时间无效")?;
            tx.execute(
                "INSERT INTO usage_remote_records
                 (device, record_id, source, session_id, occurred_at, model, record_kind,
                  input_tokens, output_tokens, cached_tokens, origin, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
                params![device, row.record_id, row.source, session_id.as_ref(), occurred_at, row.model,
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
