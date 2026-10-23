//! Optional loopback or authenticated remote OpenAI-compatible embeddings.
//! The L1 ledger is authoritative; this database contains only disposable
//! content signatures and vectors.
use super::SemanticMemory;
use crate::telemetry_store::TelemetryStore;
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use reqwest::Url;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::net::{IpAddr, SocketAddr, TcpStream};
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};

const LOCAL_BATCH_SIZE: usize = 24;
/// Remote embedding services (e.g. the Orange-seting bge-m3 endpoint) accept
/// at most 16 texts per request and truncate each text at 512 tokens.
const REMOTE_BATCH_SIZE: usize = 16;
/// Server-side per-text limit is 16000 characters; the client truncates
/// conservatively rather than relying on server behaviour.
const REMOTE_MAX_TEXT_CHARS: usize = 16000;
/// Keep each remote request comfortably small: at most 16 texts and a total
/// character budget well below the theoretical 16 x 16000 ceiling.
const REMOTE_MAX_BATCH_CHARS: usize = 24000;
const DEFAULT_USER_AGENT: &str = "orange-embedding-client/1.0";
static RUNTIME_START_LOCK: Mutex<()> = Mutex::new(());

#[derive(Deserialize, Serialize)]
struct Config {
    base_url: String,
    model: String,
    #[serde(default)]
    runtime: Option<Runtime>,
    /// Bearer credential for a remote embedding service. Never echoed back
    /// through the public config getters; see `public_value`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    api_key: Option<String>,
    /// Extra request headers (e.g. a custom User-Agent). Authorization and
    /// other reserved headers are rejected; the API key is applied separately.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    headers: Option<BTreeMap<String, String>>,
}

#[derive(Deserialize, Serialize)]
struct Runtime {
    command: String,
    #[serde(default)]
    args: Vec<String>,
    #[serde(default)]
    env: HashMap<String, String>,
}

fn is_loopback_host(host: &str) -> bool {
    host.trim_matches(['[', ']'])
        .parse::<IpAddr>()
        .map(|ip| ip.is_loopback())
        .unwrap_or(false)
}

impl Config {
    fn parse(value: Value) -> Result<Self, String> {
        let mut config: Self = serde_json::from_value(value)
            .map_err(|_| "memory_semantic_embedding requires base_url and model".to_string())?;
        config.model = config.model.trim().to_string();
        if config.model.is_empty() {
            return Err("memory_semantic_embedding.model must not be empty".into());
        }
        let mut url = Url::parse(config.base_url.trim())
            .map_err(|_| "invalid embedding base_url".to_string())?;
        if !matches!(url.scheme(), "http" | "https")
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
        {
            return Err("embedding base_url must be an HTTP(S) URL without credentials, query or fragment".into());
        }
        // Pin localhost rather than trusting DNS. Redirects and proxies are
        // disabled too, so memory text cannot leave through an indirect path.
        if url.host_str() == Some("localhost") {
            url.set_host(Some("127.0.0.1")).map_err(|_| "invalid loopback host".to_string())?;
        }
        let host = url.host_str().unwrap_or("");
        let loopback = is_loopback_host(host);
        if !loopback && url.scheme() != "https" {
            return Err("remote embedding base_url must use HTTPS; plain HTTP is only allowed on loopback".into());
        }
        let remote = !loopback;
        if remote && config.runtime.is_some() {
            return Err("embedding runtime auto-start is only supported for loopback services".into());
        }
        if let Some(runtime) = config.runtime.as_ref() {
            if runtime.command.trim().is_empty() {
                return Err("embedding runtime command must not be empty".into());
            }
        }
        if let Some(key) = config.api_key.take() {
            let key = key.trim().to_string();
            // An empty string explicitly clears a previously stored key.
            config.api_key = (!key.is_empty()).then_some(key);
        }
        if let Some(headers) = config.headers.take() {
            let mut cleaned = BTreeMap::new();
            for (name, value) in headers {
                let name = name.trim().to_string();
                if name.is_empty() {
                    return Err("embedding headers must not contain an empty name".into());
                }
                let lower = name.to_ascii_lowercase();
                // The API key owns Authorization; connection-level and
                // request-shaping headers stay under client control.
                if matches!(lower.as_str(),
                    "authorization" | "proxy-authorization" | "host" | "content-length"
                    | "content-type" | "connection" | "transfer-encoding")
                {
                    return Err(format!("embedding header {name} is reserved"));
                }
                if HeaderName::try_from(name.as_str()).is_err()
                    || HeaderValue::from_str(value.trim()).is_err()
                {
                    return Err(format!("embedding header {name} has an invalid name or value"));
                }
                cleaned.insert(name, value.trim().to_string());
            }
            config.headers = (!cleaned.is_empty()).then_some(cleaned);
        }
        config.base_url = url.as_str().trim_end_matches('/').to_string();
        Ok(config)
    }

    fn is_remote(&self) -> bool {
        Url::parse(&self.base_url)
            .ok()
            .and_then(|url| url.host_str().map(|host| !is_loopback_host(host)))
            .unwrap_or(false)
    }

    fn endpoint(&self) -> String {
        format!("{}/embeddings", self.base_url)
    }

    fn batch_size(&self) -> usize {
        if self.is_remote() { REMOTE_BATCH_SIZE } else { LOCAL_BATCH_SIZE }
    }

    /// Short fingerprint of the credential for cache signatures. The key
    /// itself is never persisted into the vector cache.
    fn api_key_fingerprint(&self) -> Option<String> {
        self.api_key.as_ref().map(|key| {
            let digest = Sha256::digest(key.as_bytes());
            digest.iter().take(8).map(|byte| format!("{byte:02x}")).collect()
        })
    }

    fn request_headers(&self) -> Result<HeaderMap, String> {
        let mut map = HeaderMap::new();
        let mut has_user_agent = false;
        if let Some(headers) = &self.headers {
            for (name, value) in headers {
                let header_name = HeaderName::try_from(name.as_str())
                    .map_err(|_| format!("embedding header {name} has an invalid name"))?;
                let header_value = HeaderValue::from_str(value)
                    .map_err(|_| format!("embedding header {name} has an invalid value"))?;
                if header_name == reqwest::header::USER_AGENT {
                    has_user_agent = true;
                }
                map.insert(header_name, header_value);
            }
        }
        if !has_user_agent {
            map.insert(reqwest::header::USER_AGENT, HeaderValue::from_static(DEFAULT_USER_AGENT));
        }
        if let Some(key) = &self.api_key {
            let value = HeaderValue::from_str(&format!("Bearer {key}"))
                .map_err(|_| "embedding api_key contains invalid header characters".to_string())?;
            map.insert(reqwest::header::AUTHORIZATION, value);
        }
        Ok(map)
    }

    /// Public view for get/set responses: never contains the API key, only a
    /// boolean marker so the UI can render "key stored" state.
    fn public_value(&self) -> Result<Value, String> {
        let mut value = serde_json::to_value(self)
            .map_err(|_| "could not serialize embedding configuration".to_string())?;
        if let Some(object) = value.as_object_mut() {
            object.remove("api_key");
            object.insert("has_api_key".into(), Value::Bool(self.api_key.is_some()));
        }
        Ok(value)
    }
}

/// Merge secret-preserving semantics for `memory_semantic_config_set`:
/// an omitted `api_key` keeps the previously stored key, an empty string
/// clears it, and any other value replaces it. Returns the merged config
/// value ready for validation and storage.
pub(super) fn merge_api_key(incoming: Value, stored: Option<&Value>) -> Value {
    let Value::Object(mut incoming_map) = incoming else { return incoming };
    match incoming_map.get("api_key") {
        Some(Value::String(key)) if key.trim().is_empty() => {
            incoming_map.remove("api_key");
        }
        Some(_) => {}
        None => {
            if let Some(Value::String(key)) = stored.and_then(|value| value.get("api_key")) {
                if !key.trim().is_empty() {
                    incoming_map.insert("api_key".into(), Value::String(key.clone()));
                }
            }
        }
    }
    Value::Object(incoming_map)
}

/// Settings validation is pure: never starts a process or sends HTTP.
/// The returned value is the full storable config including the API key.
pub(super) fn validate_config(value: Value) -> Result<Value, String> {
    serde_json::to_value(Config::parse(value)?).map_err(|_| "could not serialize embedding configuration".into())
}

/// Public (key-redacted) form of an already-validated stored config value.
pub(super) fn public_config(value: Value) -> Result<Value, String> {
    Config::parse(value)?.public_value()
}

/// Explicit connection test uses no memories, cache, settings writes or runtime.
pub(super) fn test_config(value: Value) -> Result<Value, String> {
    let config = Config::parse(value)?;
    let client = build_client()?;
    let started = Instant::now();
    let vector = embed(&client, &config, &["Local embedding connection test."], None)?.remove(0);
    Ok(serde_json::json!({
        "dimension": vector.len(),
        "model": config.model,
        "latency_ms": started.elapsed().as_millis() as u64,
    }))
}

fn build_client() -> Result<reqwest::blocking::Client, String> {
    reqwest::blocking::Client::builder()
        // Privacy default: never route memory text through a system proxy,
        // even for the remote service. Redirects stay disabled so a
        // compromised or misconfigured endpoint cannot bounce the request
        // (and its Authorization header) to another origin.
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        // Through proxy tunnels (fake-IP TUN) the TLS handshake alone can
        // take 5s+ to reach the remote service; 5s was cutting it off every
        // time. 15s covers slow routes without hanging on dead endpoints.
        .connect_timeout(Duration::from_secs(15))
        // Per request, not an overall cold-cache deadline. Completed batches
        // commit independently and survive a later timeout or client retry.
        .timeout(Duration::from_secs(120))
        .build().map_err(|_| "could not initialize embedding client".to_string())
}

fn runtime_address(config: &Config) -> Result<SocketAddr, String> {
    let url = Url::parse(&config.base_url).map_err(|_| "invalid embedding base_url".to_string())?;
    let host = url.host_str().unwrap_or("").trim_matches(['[', ']']);
    let ip = host.parse::<IpAddr>().map_err(|_| "invalid loopback host".to_string())?;
    let port = url.port_or_known_default().ok_or_else(|| "missing embedding port".to_string())?;
    Ok(SocketAddr::new(ip, port))
}

fn listening(address: SocketAddr) -> bool {
    TcpStream::connect_timeout(&address, Duration::from_millis(300)).is_ok()
}

fn ensure_runtime(config: &Config, conn: &Connection) -> Result<(), String> {
    let Some(runtime) = &config.runtime else { return Ok(()) };
    // Config::parse rejects runtime auto-start for remote endpoints; this is
    // defence in depth for configs constructed before the rule existed.
    if config.is_remote() {
        return Err("embedding runtime auto-start is only supported for loopback services".into());
    }
    let address = runtime_address(config)?;
    if listening(address) {
        return Ok(());
    }
    let _guard = RUNTIME_START_LOCK.lock().map_err(|_| "embedding startup lock poisoned".to_string())?;
    if listening(address) {
        return Ok(());
    }
    // A short SQLite lease serializes independent MCP processes. Never hold a
    // transaction while starting the process, polling TCP, or calling HTTP.
    let now = chrono::Utc::now().timestamp();
    let endpoint = config.endpoint();
    let claimed = conn.execute(
        "INSERT INTO runtime_starts_v1 (endpoint, started_at) VALUES (?1, ?2)
         ON CONFLICT(endpoint) DO UPDATE SET started_at = excluded.started_at
         WHERE runtime_starts_v1.started_at < ?3",
        params![endpoint, now, now - 180],
    ).map_err(|e| e.to_string())?;
    if claimed > 0 {
        let mut command = Command::new(&runtime.command);
        command.args(&runtime.args).envs(&runtime.env)
            .stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
        crate::process_util::no_window(&mut command);
        // No kill-on-drop or job ownership: the service can outlive this MCP.
        // Errors deliberately exclude command arguments and environment values.
        if command.spawn().is_err() {
            let _ = conn.execute("DELETE FROM runtime_starts_v1 WHERE endpoint = ?1 AND started_at = ?2", params![endpoint, now]);
            return Err("could not start configured local embedding runtime".into());
        }
    }
    let deadline = Instant::now() + Duration::from_secs(120);
    while Instant::now() < deadline {
        if listening(address) {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    Err("local embedding runtime did not become reachable within 120 seconds".into())
}

#[derive(Deserialize)]
struct EmbeddingResponse {
    data: Vec<EmbeddingItem>,
}

#[derive(Deserialize)]
struct EmbeddingItem {
    index: usize,
    embedding: Vec<f32>,
}

fn valid_vector(vector: &[f32], dimension: usize) -> bool {
    dimension > 0 && vector.len() == dimension
        && vector.iter().all(|v| v.is_finite())
        && vector.iter().any(|v| *v != 0.0)
}

/// OpenAI response order is not guaranteed. Reject omissions, extras and duplicates.
fn align(response: EmbeddingResponse, count: usize, dimension: Option<usize>) -> Result<Vec<Vec<f32>>, String> {
    if count == 0 || response.data.len() != count {
        return Err("embedding response count differs from input count".into());
    }
    let dimension = dimension.unwrap_or(response.data[0].embedding.len());
    let mut ordered = vec![None; count];
    for item in response.data {
        if item.index >= count || ordered[item.index].is_some()
            || !valid_vector(&item.embedding, dimension)
        {
            return Err("embedding response has invalid indices, dimensions or non-finite/zero vectors".into());
        }
        ordered[item.index] = Some(item.embedding);
    }
    ordered.into_iter().map(|v| v.ok_or_else(|| "missing embedding index".into())).collect()
}

/// Truncate a text for the remote service. The server enforces 512 tokens per
/// text; the client applies the documented 16000-character ceiling so an
/// oversized memory never depends on server-side truncation semantics.
fn prepare_remote_text(text: &str) -> String {
    if text.chars().count() <= REMOTE_MAX_TEXT_CHARS {
        return text.to_string();
    }
    text.chars().take(REMOTE_MAX_TEXT_CHARS).collect()
}

/// Split inputs into request batches, preserving each input's original index
/// so callers never have to recover positions from (possibly duplicated)
/// text. Remote calls additionally respect a conservative total character
/// budget per request.
fn plan_batches(config: &Config, inputs: &[&str]) -> Vec<Vec<usize>> {
    let batch_size = config.batch_size();
    if !config.is_remote() {
        return (0..inputs.len())
            .collect::<Vec<_>>()
            .chunks(batch_size)
            .map(|chunk| chunk.to_vec())
            .collect();
    }
    let mut batches = Vec::new();
    let mut current: Vec<usize> = Vec::new();
    let mut current_chars = 0usize;
    for (index, input) in inputs.iter().enumerate() {
        let chars = input.chars().count().min(REMOTE_MAX_TEXT_CHARS);
        if !current.is_empty()
            && (current.len() >= batch_size || current_chars + chars > REMOTE_MAX_BATCH_CHARS)
        {
            batches.push(std::mem::take(&mut current));
            current_chars = 0;
        }
        current_chars += chars;
        current.push(index);
    }
    if !current.is_empty() {
        batches.push(current);
    }
    batches
}

/// One embedding request for a pre-planned batch. Remote texts are truncated
/// to the documented per-text character ceiling before sending.
fn embed_batch(
    client: &reqwest::blocking::Client,
    config: &Config,
    headers: &HeaderMap,
    inputs: &[&str],
    dimension: Option<usize>,
) -> Result<Vec<Vec<f32>>, String> {
    let body: Vec<String> = if config.is_remote() {
        inputs.iter().map(|text| prepare_remote_text(text)).collect()
    } else {
        inputs.iter().map(|text| text.to_string()).collect()
    };
    let response = client.post(config.endpoint())
        .headers(headers.clone())
        .json(&serde_json::json!({"model": config.model, "input": body, "encoding_format": "float"}))
        .send().map_err(|e| format!("embedding request failed: {e}"))?;
    // Do not echo response bodies: server errors may contain memory text.
    if !response.status().is_success() {
        return Err(format!("embedding service returned HTTP {}", response.status()));
    }
    let response = response.json::<EmbeddingResponse>()
        .map_err(|_| "invalid embedding response".to_string())?;
    align(response, inputs.len(), dimension)
}

fn embed(client: &reqwest::blocking::Client, config: &Config, inputs: &[&str], dimension: Option<usize>) -> Result<Vec<Vec<f32>>, String> {
    let headers = config.request_headers()?;
    let mut outputs: Vec<Option<Vec<f32>>> = vec![None; inputs.len()];
    for batch in plan_batches(config, inputs) {
        let batch_inputs = batch.iter().map(|&index| inputs[index]).collect::<Vec<_>>();
        for (&index, vector) in batch.iter().zip(embed_batch(client, config, &headers, &batch_inputs, dimension)?) {
            outputs[index] = Some(vector);
        }
    }
    outputs.into_iter().map(|v| v.ok_or_else(|| "missing embedding output".to_string())).collect()
}

fn signature(config: &Config, content: &str) -> String {
    // Length-delimited JSON prevents ambiguous field concatenations. Bump v1
    // if document preprocessing or vector representation ever changes. The
    // key fingerprint (never the key) distinguishes credentials so rotating
    // a key against the same endpoint cannot silently reuse stale vectors.
    let payload = serde_json::to_vec(&(
        "l1-embedding-v1",
        config.endpoint(),
        &config.model,
        config.api_key_fingerprint(),
        content,
    )).expect("serialize embedding signature");
    format!("{:x}", Sha256::digest(payload))
}

fn cosine(a: &[f32], b: &[f32]) -> Result<f64, String> {
    if !valid_vector(a, b.len()) || !valid_vector(b, a.len()) {
        return Err("invalid vectors for cosine similarity".into());
    }
    // Accumulate in f64 so even finite, extreme f32 inputs cannot overflow.
    let (mut dot, mut aa, mut bb) = (0.0_f64, 0.0_f64, 0.0_f64);
    for (&a, &b) in a.iter().zip(b) {
        let (a, b) = (f64::from(a), f64::from(b));
        dot += a * b;
        aa += a * a;
        bb += b * b;
    }
    let score = dot / (aa.sqrt() * bb.sqrt());
    if !score.is_finite() {
        return Err("non-finite cosine similarity".into());
    }
    Ok(score.clamp(-1.0, 1.0))
}

fn initialize_cache(conn: &Connection) -> Result<(), String> {
    conn.busy_timeout(Duration::from_secs(8)).map_err(|e| e.to_string())?;
    conn.execute_batch("PRAGMA journal_mode=WAL;
        CREATE TABLE IF NOT EXISTS embeddings_v1 (
            signature TEXT PRIMARY KEY,
            dimension INTEGER NOT NULL,
            vector BLOB NOT NULL
        );
        CREATE TABLE IF NOT EXISTS runtime_starts_v1 (
            endpoint TEXT PRIMARY KEY,
            started_at INTEGER NOT NULL
        );
        CREATE TABLE IF NOT EXISTS build_leases_v1 (
            route TEXT PRIMARY KEY,
            owner TEXT NOT NULL,
            expires_at INTEGER NOT NULL
        );").map_err(|e| e.to_string())
}

// A separate connection owns a renewable lease, allowing cache batch writes
// to use short transactions without holding SQLite locks across HTTP calls.
struct BuildLease {
    conn: Connection,
    route: String,
    owner: String,
}

impl BuildLease {
    fn acquire(path: &std::path::Path, config: &Config) -> Result<Self, String> {
        let lease = Self {
            conn: Connection::open(path).map_err(|e| e.to_string())?,
            route: signature(config, ""),
            owner: uuid::Uuid::new_v4().to_string(),
        };
        lease.conn.busy_timeout(Duration::from_secs(8)).map_err(|e| e.to_string())?;
        let deadline = Instant::now() + Duration::from_secs(600);
        let mut logged_wait = false;
        loop {
            let now = chrono::Utc::now().timestamp();
            let claimed = lease.conn.execute(
                "INSERT INTO build_leases_v1 (route, owner, expires_at) VALUES (?1, ?2, ?3)
                 ON CONFLICT(route) DO UPDATE SET owner = excluded.owner, expires_at = excluded.expires_at
                 WHERE build_leases_v1.expires_at < ?4",
                params![lease.route, lease.owner, now + 180, now],
            ).map_err(|e| e.to_string())?;
            if claimed > 0 {
                return Ok(lease);
            }
            if !logged_wait {
                eprintln!("[memory-semantic] waiting for another process to finish cache indexing");
                logged_wait = true;
            }
            if Instant::now() >= deadline {
                return Err("embedding cache is still being built by another process; retry shortly".into());
            }
            std::thread::sleep(Duration::from_millis(500));
        }
    }

    fn renew(&self) -> Result<(), String> {
        let changed = self.conn.execute(
            "UPDATE build_leases_v1 SET expires_at = ?1 WHERE route = ?2 AND owner = ?3",
            params![chrono::Utc::now().timestamp() + 180, self.route, self.owner],
        ).map_err(|e| e.to_string())?;
        if changed != 1 {
            return Err("embedding cache lease lost; retry search".into());
        }
        Ok(())
    }
}

impl Drop for BuildLease {
    fn drop(&mut self) {
        let _ = self.conn.execute("DELETE FROM build_leases_v1 WHERE route = ?1 AND owner = ?2",
            params![self.route, self.owner]);
    }
}

fn decode(bytes: &[u8], dimension: usize) -> Option<Vec<f32>> {
    if dimension.checked_mul(4)? != bytes.len() {
        return None;
    }
    let vector = bytes.chunks_exact(4)
        .map(|chunk| f32::from_le_bytes(chunk.try_into().expect("four bytes")))
        .collect::<Vec<_>>();
    valid_vector(&vector, dimension).then_some(vector)
}

/// Result of ensuring every document has a cached embedding. `vectors` maps
/// each document's content signature to its vector; `signatures` aligns
/// positionally with the input documents. `dimension` is 0 only when both the
/// snapshot and the cache are empty (no vector exists to learn it from).
pub(super) struct EnsureIndexedOutcome {
    pub vectors: HashMap<String, Vec<f32>>,
    pub signatures: Vec<String>,
    pub dimension: usize,
    pub embedded: usize,
}

/// Open the shared vector cache, creating the directory and schema if needed.
/// Returns the connection and the cache file path. Does not start the runtime
/// or make any HTTP call.
fn open_cache() -> Result<(Connection, std::path::PathBuf), String> {
    let directory = dirs_next::config_dir()
        .ok_or_else(|| "application data directory unavailable".to_string())?
        .join("agent-manager");
    std::fs::create_dir_all(&directory).map_err(|e| e.to_string())?;
    let cache_path = directory.join("memory-semantic-vectors.sqlite3");
    let conn = Connection::open(&cache_path).map_err(|e| e.to_string())?;
    initialize_cache(&conn)?;
    Ok((conn, cache_path))
}

/// Ensure every document has a cached embedding, embedding only the missing
/// ones. Acquires its own BuildLease, renews per batch, drops it before
/// returning. Zero HTTP when nothing is missing (the incremental no-op path).
/// Shared by `search`, the scheduled auto-build and the manual build command.
fn ensure_indexed(
    config: &Config,
    documents: &[crate::telemetry_store::LocalMemorySnapshot],
) -> Result<EnsureIndexedOutcome, String> {
    let (mut conn, cache_path) = open_cache()?;
    ensure_runtime(config, &conn)?;
    let client = build_client()?;
    let lease = BuildLease::acquire(&cache_path, config)?;
    let signatures = documents.iter().map(|doc| signature(config, &doc.memory)).collect::<Vec<_>>();
    let mut vectors: HashMap<String, Vec<f32>> = HashMap::new();
    let mut missing = Vec::new();
    let mut seen = HashSet::new();
    let mut dimension: Option<usize> = None;
    {
        let mut statement = conn.prepare("SELECT dimension, vector FROM embeddings_v1 WHERE signature = ?1")
            .map_err(|e| e.to_string())?;
        for (index, key) in signatures.iter().enumerate() {
            if !seen.insert(key) {
                continue;
            }
            let cached: Option<(usize, Vec<u8>)> = statement.query_row(params![key], |row| Ok((row.get(0)?, row.get(1)?)))
                .optional().map_err(|e| e.to_string())?;
            if let Some((size, bytes)) = cached {
                if dimension.is_none() {
                    dimension = Some(size);
                }
                if let Some(vector) = (Some(size) == dimension).then(|| decode(&bytes, size)).flatten() {
                    vectors.insert(key.clone(), vector);
                    continue;
                }
            }
            missing.push(index);
        }
    }
    let batch_size = config.batch_size();
    eprintln!("[memory-semantic] L1 documents={}, uncached={}, batch_size={batch_size}", documents.len(), missing.len());
    let started = Instant::now();
    let missing_inputs = missing.iter().map(|&index| documents[index].memory.as_str()).collect::<Vec<_>>();
    let batches = plan_batches(config, &missing_inputs);
    let total_batches = batches.len();
    let headers = config.request_headers()?;
    let mut embedded = 0usize;
    for (batch_index, batch) in batches.into_iter().enumerate() {
        lease.renew()?;
        let batch_inputs = batch.iter().map(|&offset| missing_inputs[offset]).collect::<Vec<_>>();
        let embedded_batch = embed_batch(&client, config, &headers, &batch_inputs, dimension)?;
        if dimension.is_none() {
            dimension = embedded_batch.first().map(|vector| vector.len());
        }
        let tx = conn.transaction().map_err(|e| e.to_string())?;
        for (&offset, vector) in batch.iter().zip(embedded_batch) {
            let index = missing[offset];
            let size = vector.len();
            let bytes = vector.iter().flat_map(|v| v.to_le_bytes()).collect::<Vec<_>>();
            tx.execute("INSERT OR REPLACE INTO embeddings_v1 (signature, dimension, vector) VALUES (?1, ?2, ?3)",
                params![&signatures[index], size, bytes]).map_err(|e| e.to_string())?;
            vectors.insert(signatures[index].clone(), vector);
            embedded += 1;
        }
        tx.commit().map_err(|e| e.to_string())?;
        eprintln!("[memory-semantic] indexed batch {}/{}, vectors={}, elapsed_ms={}",
            batch_index + 1, total_batches, batch.len(), started.elapsed().as_millis());
    }
    drop(lease);
    Ok(EnsureIndexedOutcome {
        vectors,
        signatures,
        dimension: dimension.unwrap_or(0),
        embedded,
    })
}

/// Count of cached vs pending documents. Reads the snapshot and the cache only:
/// never embeds, never acquires the lease, never starts the runtime, so it is
/// safe for the UI to poll while a build runs in another process.
pub(super) fn index_status(telemetry: &TelemetryStore, value: Value) -> Result<Value, String> {
    let config = Config::parse(value)?;
    let documents = telemetry.local_l1_memory_snapshot()?;
    let (conn, _) = open_cache()?;
    let mut statement = conn.prepare("SELECT 1 FROM embeddings_v1 WHERE signature = ?1")
        .map_err(|e| e.to_string())?;
    let mut seen = HashSet::new();
    let mut cached = 0usize;
    for doc in &documents {
        let key = signature(&config, &doc.memory);
        if !seen.insert(key.clone()) {
            continue;
        }
        let present: Option<i64> = statement.query_row(params![&key], |_| Ok(1))
            .optional().map_err(|e| e.to_string())?;
        if present.is_some() {
            cached += 1;
        }
    }
    let total = documents.len();
    Ok(serde_json::json!({
        "total": total,
        "cached": cached,
        "pending": total.saturating_sub(cached),
    }))
}

/// Incremental build: embed only the missing signatures, then report status.
pub(super) fn build_index(telemetry: &TelemetryStore, value: Value) -> Result<Value, String> {
    let config = Config::parse(value.clone())?;
    let documents = telemetry.local_l1_memory_snapshot()?;
    let outcome = ensure_indexed(&config, &documents)?;
    let mut status = index_status(telemetry, value)?;
    if let Some(object) = status.as_object_mut() {
        object.insert("embedded".into(), Value::from(outcome.embedded));
    }
    Ok(status)
}

pub(super) fn search(telemetry: &TelemetryStore, value: Value, query: &str, limit: u32) -> Result<Vec<SemanticMemory>, String> {
    let config = Config::parse(value)?;
    if query.trim().is_empty() {
        return Ok(Vec::new());
    }
    let documents = telemetry.local_l1_memory_snapshot()?;
    if documents.is_empty() {
        return Ok(Vec::new());
    }
    let outcome = ensure_indexed(&config, &documents)?;
    let client = build_client()?;
    let query_vector = embed(&client, &config, &[query], (outcome.dimension > 0).then_some(outcome.dimension))?.remove(0);
    // Cold indexing may take minutes. Re-read the ledger so deleted or edited
    // records cannot leak through the snapshot used to construct the cache.
    let current = telemetry.local_l1_memory_snapshot()?;
    let indexed: HashMap<_, _> = documents.iter().zip(&outcome.signatures)
        .map(|(doc, key)| (doc.id.as_str(), key.as_str())).collect();
    let mut results = Vec::new();
    for doc in current {
        let key = signature(&config, &doc.memory);
        if indexed.get(doc.id.as_str()).copied() != Some(key.as_str()) {
            continue;
        }
        let vector = outcome.vectors.get(&key).ok_or_else(|| "missing cached document vector".to_string())?;
        results.push(SemanticMemory {
            id: doc.id,
            memory: doc.memory,
            memory_type: doc.memory_type,
            score: Some(cosine(&query_vector, vector)?),
        });
    }
    results.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal).then_with(|| a.id.cmp(&b.id)));
    results.truncate(limit.clamp(1, 100) as usize);
    Ok(results)
}

/// Embed arbitrary texts for the desktop UI (memory consolidation candidates
/// and BGE search). Reads no cache and writes none; the caller owns any
/// persistence. Input limits guard the service: at most 64 texts, each
/// truncated to the documented per-text character ceiling for remote routes.
/// The returned vectors array aligns positionally with the input texts.
pub(super) fn embed_texts(value: Value, texts: Vec<String>) -> Result<Value, String> {
    const MAX_TEXTS: usize = 64;
    let config = Config::parse(value)?;
    if texts.is_empty() {
        return Err("memory_semantic_embed_texts requires at least one text".into());
    }
    if texts.len() > MAX_TEXTS {
        return Err(format!("memory_semantic_embed_texts accepts at most {MAX_TEXTS} texts per call"));
    }
    let inputs = texts.iter()
        .map(|text| text.trim())
        .collect::<Vec<_>>();
    if let Some(index) = inputs.iter().position(|text| text.is_empty()) {
        return Err(format!("memory_semantic_embed_texts received an empty text at index {index}"));
    }
    let client = build_client()?;
    let started = Instant::now();
    let vectors = embed(&client, &config, &inputs, None)?;
    let dimension = vectors.first().map(|vector| vector.len()).unwrap_or(0);
    Ok(serde_json::json!({
        "vectors": vectors,
        "dimension": dimension,
        "model": config.model,
        "latency_ms": started.elapsed().as_millis() as u64,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn response(items: Vec<(usize, Vec<f32>)>) -> EmbeddingResponse {
        EmbeddingResponse { data: items.into_iter().map(|(index, embedding)| EmbeddingItem { index, embedding }).collect() }
    }

    fn local_config() -> Config {
        Config { base_url: "http://127.0.0.1:7997/v1".into(), model: "bge-m3".into(), runtime: None, api_key: None, headers: None }
    }

    #[test]
    fn responses_are_aligned_by_index_without_truncation() {
        assert_eq!(align(response(vec![(1, vec![0.0, 1.0]), (0, vec![1.0, 0.0])]), 2, None).unwrap(),
            vec![vec![1.0, 0.0], vec![0.0, 1.0]]);
        assert!(align(response(vec![(0, vec![1.0])]), 2, None).is_err());
        assert!(align(response(vec![(0, vec![1.0]), (0, vec![2.0])]), 2, None).is_err());
        assert!(align(response(vec![(1, vec![1.0])]), 1, None).is_err());
    }

    #[test]
    fn invalid_vectors_are_rejected() {
        for vector in [vec![], vec![0.0, 0.0], vec![f32::NAN, 1.0], vec![f32::INFINITY, 1.0], vec![1.0]] {
            assert!(align(response(vec![(0, vector)]), 1, Some(2)).is_err());
        }
        assert!(align(response(vec![(0, vec![1.0]), (1, vec![1.0, 2.0])]), 2, None).is_err());
    }

    #[test]
    fn cosine_checks_dimensions_and_handles_extreme_values() {
        assert!((cosine(&[1.0, 2.0], &[2.0, 4.0]).unwrap() - 1.0).abs() < 1e-12);
        assert_eq!(cosine(&[1.0, 0.0], &[0.0, 1.0]).unwrap(), 0.0);
        assert_eq!(cosine(&[1.0], &[-1.0]).unwrap(), -1.0);
        assert!(cosine(&[1.0], &[1.0, 2.0]).is_err());
        assert!(cosine(&[0.0], &[1.0]).is_err());
        assert_eq!(cosine(&[f32::MAX], &[f32::MAX]).unwrap(), 1.0);
    }

    #[test]
    fn cache_signature_covers_route_model_key_and_exact_content() {
        let mut config = local_config();
        let key = signature(&config, "memory");
        assert_eq!(key, signature(&config, "memory"));
        assert_ne!(key, signature(&config, "memory "));
        config.model = "other".into();
        assert_ne!(key, signature(&config, "memory"));
        config.model = "bge-m3".into();
        config.base_url = "http://127.0.0.1:7998/v1".into();
        assert_ne!(key, signature(&config, "memory"));
        config.base_url = "http://127.0.0.1:7997/v1".into();
        config.api_key = Some("secret-one".into());
        assert_ne!(key, signature(&config, "memory"));
        let with_first_key = signature(&config, "memory");
        config.api_key = Some("secret-two".into());
        assert_ne!(with_first_key, signature(&config, "memory"));
    }

    #[test]
    fn cache_roundtrip_rejects_invalid_dimensions_and_corruption() {
        let conn = Connection::open_in_memory().unwrap();
        initialize_cache(&conn).unwrap();
        let bytes = [1.0_f32, 2.0].into_iter().flat_map(f32::to_le_bytes).collect::<Vec<_>>();
        conn.execute("INSERT INTO embeddings_v1 VALUES ('signature', 2, ?1)", params![bytes]).unwrap();
        let cached: Vec<u8> = conn.query_row("SELECT vector FROM embeddings_v1", [], |row| row.get(0)).unwrap();
        assert_eq!(decode(&cached, 2).unwrap(), vec![1.0, 2.0]);
        assert!(decode(&cached, 1).is_none());
        assert!(decode(&cached[..7], 2).is_none());
        assert!(decode(&f32::NAN.to_le_bytes(), 1).is_none());
    }

    #[test]
    fn build_lease_is_released_and_renewal_is_fenced() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("cache.sqlite3");
        let conn = Connection::open(&path).unwrap();
        initialize_cache(&conn).unwrap();
        let config = local_config();
        let lease = BuildLease::acquire(&path, &config).unwrap();
        lease.renew().unwrap();
        let count: i64 = conn.query_row("SELECT count(*) FROM build_leases_v1", [], |row| row.get(0)).unwrap();
        assert_eq!(count, 1);
        drop(lease);
        let count: i64 = conn.query_row("SELECT count(*) FROM build_leases_v1", [], |row| row.get(0)).unwrap();
        assert_eq!(count, 0);
        let lease = BuildLease::acquire(&path, &config).unwrap();
        conn.execute("UPDATE build_leases_v1 SET owner = 'replacement'", []).unwrap();
        assert!(lease.renew().is_err());
        drop(lease);
        let count: i64 = conn.query_row("SELECT count(*) FROM build_leases_v1 WHERE owner = 'replacement'", [], |row| row.get(0)).unwrap();
        assert_eq!(count, 1);
    }

    #[test]
    fn loopback_and_remote_https_configuration_are_accepted() {
        for base in ["http://127.0.0.1:7997/v1", "http://[::1]:7997/v1", "http://localhost:7997/v1/", "https://127.0.0.1:7997/v1"] {
            assert!(Config::parse(serde_json::json!({"base_url": base, "model": "bge-m3"})).is_ok());
        }
        // Remote HTTPS is allowed, with or without an API key.
        let remote = Config::parse(serde_json::json!({
            "base_url": "https://embedding.example.com/v1",
            "model": "bge-m3",
            "api_key": "secret"
        })).unwrap();
        assert!(remote.is_remote());
        assert_eq!(remote.batch_size(), REMOTE_BATCH_SIZE);
        assert!(Config::parse(serde_json::json!({"base_url": "https://embedding.example.com/v1", "model": "bge-m3"})).is_ok());
    }

    #[test]
    fn invalid_urls_and_remote_http_are_rejected() {
        for base in ["http://192.168.1.1/v1", "http://localhost.example/v1", "http://user@127.0.0.1/v1",
            "http://127.0.0.1/v1?redirect=x", "file:///v1", "https://user:pass@embedding.example.com/v1",
            "https://embedding.example.com/v1#frag"] {
            assert!(Config::parse(serde_json::json!({"base_url": base, "model": "bge-m3"})).is_err(), "{base}");
        }
        assert!(Config::parse(serde_json::json!({"base_url": "http://127.0.0.1/v1"})).is_err());
        assert!(Config::parse(serde_json::json!({"base_url": "http://127.0.0.1/v1", "model": " "})).is_err());
    }

    #[test]
    fn remote_config_forbids_runtime_autostart() {
        assert!(Config::parse(serde_json::json!({
            "base_url": "https://embedding.example.com/v1",
            "model": "bge-m3",
            "runtime": {"command": "python", "args": ["serve.py"]}
        })).is_err());
        assert!(Config::parse(serde_json::json!({
            "base_url": "http://127.0.0.1:7997/v1",
            "model": "bge-m3",
            "runtime": {"command": " "}
        })).is_err());
    }

    #[test]
    fn api_key_is_trimmed_cleared_and_never_serialized_publicly() {
        let config = Config::parse(serde_json::json!({
            "base_url": "https://embedding.example.com/v1",
            "model": "bge-m3",
            "api_key": "  secret  "
        })).unwrap();
        assert_eq!(config.api_key.as_deref(), Some("secret"));
        let public = config.public_value().unwrap();
        assert_eq!(public.get("has_api_key"), Some(&Value::Bool(true)));
        assert!(public.get("api_key").is_none());
        assert!(!public.to_string().contains("secret"));
        // Empty string clears the key.
        let cleared = Config::parse(serde_json::json!({
            "base_url": "https://embedding.example.com/v1",
            "model": "bge-m3",
            "api_key": "   "
        })).unwrap();
        assert!(cleared.api_key.is_none());
        assert_eq!(cleared.public_value().unwrap().get("has_api_key"), Some(&Value::Bool(false)));
    }

    #[test]
    fn merge_api_key_preserves_replaces_and_clears() {
        let stored = serde_json::json!({"base_url": "https://embedding.example.com/v1", "model": "bge-m3", "api_key": "old"});
        // Omitted key keeps the stored one.
        let merged = merge_api_key(serde_json::json!({"base_url": "https://embedding.example.com/v1", "model": "bge-m3"}), Some(&stored));
        assert_eq!(merged.get("api_key"), Some(&Value::String("old".into())));
        // A new key replaces it.
        let merged = merge_api_key(serde_json::json!({"base_url": "https://embedding.example.com/v1", "model": "bge-m3", "api_key": "new"}), Some(&stored));
        assert_eq!(merged.get("api_key"), Some(&Value::String("new".into())));
        // An empty string clears it.
        let merged = merge_api_key(serde_json::json!({"base_url": "https://embedding.example.com/v1", "model": "bge-m3", "api_key": ""}), Some(&stored));
        assert!(merged.get("api_key").is_none());
        // Nothing stored, nothing to preserve.
        let merged = merge_api_key(serde_json::json!({"base_url": "https://embedding.example.com/v1", "model": "bge-m3"}), None);
        assert!(merged.get("api_key").is_none());
    }

    #[test]
    fn headers_are_validated_and_reserved_names_rejected() {
        let config = Config::parse(serde_json::json!({
            "base_url": "https://embedding.example.com/v1",
            "model": "bge-m3",
            "headers": {"X-Team": "memory"}
        })).unwrap();
        let headers = config.request_headers().unwrap();
        assert_eq!(headers.get("x-team").and_then(|v| v.to_str().ok()), Some("memory"));
        // Default User-Agent is applied when not overridden.
        assert_eq!(headers.get(reqwest::header::USER_AGENT).and_then(|v| v.to_str().ok()), Some(DEFAULT_USER_AGENT));
        for name in ["Authorization", "authorization", "Host", "Content-Length", "Proxy-Authorization"] {
            assert!(Config::parse(serde_json::json!({
                "base_url": "https://embedding.example.com/v1",
                "model": "bge-m3",
                "headers": {name: "x"}
            })).is_err(), "{name}");
        }
        // A configured User-Agent wins over the default.
        let config = Config::parse(serde_json::json!({
            "base_url": "https://embedding.example.com/v1",
            "model": "bge-m3",
            "headers": {"User-Agent": "custom-client/2.0"}
        })).unwrap();
        assert_eq!(config.request_headers().unwrap().get(reqwest::header::USER_AGENT).and_then(|v| v.to_str().ok()), Some("custom-client/2.0"));
    }

    #[test]
    fn authorization_header_comes_from_api_key() {
        let config = Config::parse(serde_json::json!({
            "base_url": "https://embedding.example.com/v1",
            "model": "bge-m3",
            "api_key": "secret"
        })).unwrap();
        let headers = config.request_headers().unwrap();
        assert_eq!(headers.get(reqwest::header::AUTHORIZATION).and_then(|v| v.to_str().ok()), Some("Bearer secret"));
        let local = local_config();
        assert!(local.request_headers().unwrap().get(reqwest::header::AUTHORIZATION).is_none());
    }

    #[test]
    fn remote_batches_respect_count_and_character_budget() {
        let remote = Config::parse(serde_json::json!({
            "base_url": "https://embedding.example.com/v1",
            "model": "bge-m3"
        })).unwrap();
        // 40 short texts split by the count limit of 16, indices preserved.
        let texts: Vec<&str> = vec!["abc"; 40];
        let batches = plan_batches(&remote, &texts);
        assert_eq!(batches.len(), 3);
        assert_eq!(batches[0].len(), REMOTE_BATCH_SIZE);
        assert_eq!(batches[0][0], 0);
        assert_eq!(batches[2], vec![32, 33, 34, 35, 36, 37, 38, 39]);
        // Two 16000-char texts exceed the 24000-char budget and split.
        let big = "x".repeat(REMOTE_MAX_TEXT_CHARS);
        let texts: Vec<&str> = vec![big.as_str(), big.as_str(), big.as_str()];
        let batches = plan_batches(&remote, &texts);
        assert_eq!(batches, vec![vec![0], vec![1], vec![2]]);
        // Local configs keep the flat 24-text chunking.
        let local = local_config();
        let texts: Vec<&str> = vec!["abc"; 30];
        let batches = plan_batches(&local, &texts);
        assert_eq!(batches.len(), 2);
        assert_eq!(batches[0].len(), LOCAL_BATCH_SIZE);
        assert_eq!(batches[1], (24..30).collect::<Vec<_>>());
    }

    #[test]
    fn remote_text_truncation_is_char_based() {
        let short = "hello";
        assert_eq!(prepare_remote_text(short), "hello");
        let long: String = "汉".repeat(REMOTE_MAX_TEXT_CHARS + 100);
        let prepared = prepare_remote_text(&long);
        assert_eq!(prepared.chars().count(), REMOTE_MAX_TEXT_CHARS);
    }
}
