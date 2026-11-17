//! Optional loopback or authenticated remote OpenAI-compatible embeddings.
//! The L1 ledger is authoritative; this database contains only disposable
//! content signatures and vectors.
use super::SemanticMemory;
use crate::memory_retrieval::RecallBudget;
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

pub(crate) mod evaluation;
mod refinement;

pub(super) const REFINEMENT_BUDGET: Duration = Duration::from_secs(30);

pub(super) fn search_refinement(
    telemetry: &TelemetryStore, value: Value, memory_id: &str, query: &str, limit: u32,
    domains: Option<&[String]>, budget: &RecallBudget,
) -> Result<super::SemanticRecall, String> {
    refinement::search(telemetry, value, (memory_id, query), limit, domains, budget, None)
}

const LOCAL_BATCH_SIZE: usize = 24;
/// Cooperative limits, checked between requests; an in-flight request retains
/// its existing timeout and commits before yielding.
const BACKGROUND_MAX_BATCHES: usize = 2;
const BACKGROUND_BUDGET: Duration = Duration::from_secs(5);
const LEASE_SECONDS: i64 = 180;

#[derive(Clone, Copy, PartialEq, Eq)]
enum IndexingMode {
    Complete,
    Background,
}

impl IndexingMode {
    fn should_yield(self, batches: usize, elapsed: Duration) -> bool {
        self == Self::Background
            && (batches >= BACKGROUND_MAX_BATCHES || elapsed >= BACKGROUND_BUDGET)
    }
}

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

/// Request-local prefix of the exact v1 JSON tuple used by `signature`.
/// Cloning the hash state avoids serializing and hashing the route/model/key
/// for every document; the JSON string encoder still handles content bytes.
struct SignatureContext {
    prefix: Sha256,
}

impl SignatureContext {
    fn new(config: &Config) -> Self {
        let mut prefix = serde_json::to_vec(&(
            "l1-embedding-v1", config.endpoint(), &config.model, config.api_key_fingerprint(),
        )).expect("serialize embedding signature prefix");
        *prefix.last_mut().expect("JSON tuple closing bracket") = b',';
        let mut hasher = Sha256::new();
        hasher.update(prefix);
        Self { prefix: hasher }
    }

    fn signature(&self, content: &str) -> String {
        let mut hasher = self.prefix.clone();
        hasher.update(serde_json::to_vec(content).expect("serialize embedding content"));
        hasher.update(b"]");
        format!("{:x}", hasher.finalize())
    }
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
        );
        CREATE TABLE IF NOT EXISTS foreground_requests_v1 (
            route TEXT NOT NULL,
            owner TEXT NOT NULL,
            expires_at INTEGER NOT NULL,
            PRIMARY KEY (route, owner)
        );").map_err(|e| e.to_string())
}

// Serialize requests to the same runtime even when model or credentials differ.
// Content signatures keep their existing model/credential-specific format.
fn lease_route(config: &Config) -> String {
    format!("endpoint:{:x}", Sha256::digest(config.endpoint().as_bytes()))
}

fn lease_connection(path: &std::path::Path) -> Result<Connection, String> {
    let conn = Connection::open(path).map_err(|e| e.to_string())?;
    conn.busy_timeout(Duration::from_secs(8)).map_err(|e| e.to_string())?;
    Ok(conn)
}

/// Durable foreground intent is visible to independent MCP/desktop processes.
/// Owner-scoped rows let overlapping searches finish independently; expiry
/// prevents a crashed search from indefinitely blocking background indexing.
struct ForegroundRequest {
    conn: Connection,
    route: String,
    owner: String,
}

impl ForegroundRequest {
    fn register(path: &std::path::Path, config: &Config) -> Result<Self, String> {
        let request = Self {
            conn: lease_connection(path)?,
            route: lease_route(config),
            owner: uuid::Uuid::new_v4().to_string(),
        };
        let now = chrono::Utc::now().timestamp();
        request.conn.execute("DELETE FROM foreground_requests_v1 WHERE expires_at < ?1", [now])
            .map_err(|e| e.to_string())?;
        request.conn.execute(
            "INSERT INTO foreground_requests_v1 (route, owner, expires_at) VALUES (?1, ?2, ?3)",
            params![request.route, request.owner, now + LEASE_SECONDS],
        ).map_err(|e| e.to_string())?;
        Ok(request)
    }

    /// Interactive requests must not inherit the eight-second SQLite wait or
    /// three-minute intent lifetime used by complete indexing operations.
    fn register_interactive(path: &std::path::Path, config: &Config, budget: &RecallBudget) -> Result<Option<Self>, String> {
        budget.remaining()?;
        let conn = Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE)
            .map_err(|_| "embedding foreground connection unavailable".to_string())?;
        interactive_sqlite_budget(&conn, budget)?;
        let exists: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='foreground_requests_v1')", [], |row| row.get(0),
        ).map_err(|_| "embedding foreground metadata unavailable".to_string())?;
        // Old caches remain usable without creating or migrating tables here.
        if !exists { return Ok(None); }
        let remaining = budget.remaining()?;
        let request = Self { conn, route: lease_route(config), owner: uuid::Uuid::new_v4().to_string() };
        interactive_sqlite_budget(&request.conn, budget)?;
        let inserted = request.conn.execute(
            "INSERT INTO foreground_requests_v1 (route, owner, expires_at) VALUES (?1, ?2, ?3)",
            params![request.route, request.owner, chrono::Utc::now().timestamp() + remaining.as_secs() as i64 + 2],
        ).map_err(|_| "embedding foreground intent unavailable".to_string());
        // Cleanup must never outlive a cancelled request by waiting for a writer.
        let _ = request.conn.busy_timeout(Duration::ZERO);
        inserted?;
        Ok(Some(request))
    }

    fn renew(&self) -> Result<(), String> {
        let changed = self.conn.execute(
            "UPDATE foreground_requests_v1 SET expires_at = ?1 WHERE route = ?2 AND owner = ?3",
            params![chrono::Utc::now().timestamp() + LEASE_SECONDS, self.route, self.owner],
        ).map_err(|e| e.to_string())?;
        if changed != 1 {
            return Err("embedding foreground request expired; retry search".into());
        }
        Ok(())
    }
}

impl Drop for ForegroundRequest {
    fn drop(&mut self) {
        let _ = self.conn.execute("DELETE FROM foreground_requests_v1 WHERE route = ?1 AND owner = ?2",
            params![self.route, self.owner]);
    }
}

// A separate connection owns a renewable lease, allowing cache batch writes
// to use short transactions without holding SQLite locks across HTTP calls.
struct BuildLease {
    conn: Connection,
    route: String,
    legacy_route: String,
    owner: String,
}

impl BuildLease {
    fn new(path: &std::path::Path, config: &Config) -> Result<Self, String> {
        Ok(Self {
            conn: lease_connection(path)?,
            route: lease_route(config),
            legacy_route: signature(config, ""),
            owner: uuid::Uuid::new_v4().to_string(),
        })
    }

    fn try_claim(&self, foreground: bool) -> Result<bool, String> {
        self.try_claim_checked(foreground, || Ok(()))
    }

    fn try_claim_checked(&self, foreground: bool, check: impl Fn() -> Result<(), String>) -> Result<bool, String> {
        self.try_claim_until(foreground, chrono::Utc::now().timestamp() + LEASE_SECONDS, check)
    }

    fn try_claim_until(&self, foreground: bool, expires_at: i64, check: impl Fn() -> Result<(), String>) -> Result<bool, String> {
        check()?;
        let now = chrono::Utc::now().timestamp();
        // The intent check and lease claim are one SQLite statement. A
        // background/manual builder cannot slip ahead of a registered search.
        let tx = self.conn.unchecked_transaction().map_err(|e| e.to_string())?;
        check()?;
        // A foreground claim does not consult intent rows. This also permits
        // read-only interactive recall on caches predating that metadata table.
        let sql = if foreground {
            "INSERT INTO build_leases_v1 (route, owner, expires_at)
             SELECT ?1, ?2, ?3 WHERE ?5
             ON CONFLICT(route) DO UPDATE SET owner = excluded.owner, expires_at = excluded.expires_at
             WHERE build_leases_v1.expires_at < ?4"
        } else {
            "INSERT INTO build_leases_v1 (route, owner, expires_at)
             SELECT ?1, ?2, ?3 WHERE ?5 OR NOT EXISTS (
                 SELECT 1 FROM foreground_requests_v1 WHERE route = ?1 AND expires_at >= ?4
             )
             ON CONFLICT(route) DO UPDATE SET owner = excluded.owner, expires_at = excluded.expires_at
             WHERE build_leases_v1.expires_at < ?4"
        };
        let claimed = tx.execute(sql, params![self.route, self.owner, expires_at, now, foreground])
            .map_err(|e| e.to_string())?;
        if claimed == 0 {
            return Ok(false);
        }
        // Retain the original configuration-specific lock for older MCP/app
        // processes. Claim both rows atomically or roll back the endpoint row;
        // neither transaction survives into runtime startup or an HTTP call.
        check()?;
        let legacy_claimed = tx.execute(
            "INSERT INTO build_leases_v1 (route, owner, expires_at) VALUES (?1, ?2, ?3)
             ON CONFLICT(route) DO UPDATE SET owner = excluded.owner, expires_at = excluded.expires_at
             WHERE build_leases_v1.expires_at < ?4",
            params![self.legacy_route, self.owner, expires_at, now],
        ).map_err(|e| e.to_string())?;
        if legacy_claimed == 0 {
            return Ok(false);
        }
        check()?;
        tx.commit().map_err(|e| e.to_string())?;
        Ok(true)
    }

    /// Wait for the current indexing batch, using the caller's deadline. A
    /// registered foreground intent prevents another background batch racing
    /// ahead as soon as the current owner releases its lease.
    fn acquire_interactive(&self, budget: &RecallBudget) -> Result<(), String> {
        let result = (|| loop {
            let remaining = budget.remaining()?;
            // A crashed short query must not retain the indexing worker's
            // three-minute lease. Keep just the deadline plus clock rounding.
            let expires_at = chrono::Utc::now().timestamp() + remaining.as_secs() as i64 + 2;
            if self.try_claim_until(true, expires_at, || interactive_sqlite_budget(&self.conn, budget))? {
                return Ok(());
            }
            let remaining = budget.remaining()?;
            std::thread::sleep(remaining.min(Duration::from_millis(25)));
        })();
        // Both success and cancellation must leave best-effort Drop nonblocking.
        let _ = self.conn.busy_timeout(Duration::ZERO);
        result
    }

    fn try_acquire(path: &std::path::Path, config: &Config) -> Result<Option<Self>, String> {
        let lease = Self::new(path, config)?;
        if lease.try_claim(false)? { Ok(Some(lease)) } else { Ok(None) }
    }

    fn acquire(path: &std::path::Path, config: &Config, foreground: Option<&ForegroundRequest>) -> Result<Self, String> {
        let lease = Self::new(path, config)?;
        let deadline = Instant::now() + Duration::from_secs(600);
        let mut next_renewal = Instant::now();
        let mut logged_wait = false;
        loop {
            if Instant::now() >= next_renewal {
                if let Some(request) = foreground {
                    request.renew()?;
                }
                next_renewal = Instant::now() + Duration::from_secs(30);
            }
            if lease.try_claim(foreground.is_some())? {
                return Ok(lease);
            }
            if !logged_wait {
                eprintln!("[memory-semantic] waiting for embedding runtime lease");
                logged_wait = true;
            }
            if Instant::now() >= deadline {
                return Err("embedding runtime is busy in another process; retry shortly".into());
            }
            std::thread::sleep(Duration::from_millis(500));
        }
    }

    fn foreground_pending(&self) -> Result<bool, String> {
        self.conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM foreground_requests_v1 WHERE route = ?1 AND expires_at >= ?2)",
            params![self.route, chrono::Utc::now().timestamp()],
            |row| row.get(0),
        ).map_err(|e| e.to_string())
    }

    fn renew(&self) -> Result<(), String> {
        let tx = self.conn.unchecked_transaction().map_err(|e| e.to_string())?;
        let changed = tx.execute(
            "UPDATE build_leases_v1 SET expires_at = ?1 WHERE route IN (?2, ?3) AND owner = ?4",
            params![chrono::Utc::now().timestamp() + LEASE_SECONDS, self.route, self.legacy_route, self.owner],
        ).map_err(|e| e.to_string())?;
        if changed != 2 {
            return Err("embedding cache lease lost; retry search".into());
        }
        tx.commit().map_err(|e| e.to_string())?;
        Ok(())
    }
}

impl Drop for BuildLease {
    fn drop(&mut self) {
        let _ = self.conn.execute("DELETE FROM build_leases_v1 WHERE route IN (?1, ?2) AND owner = ?3",
            params![self.route, self.legacy_route, self.owner]);
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

/// Result of an indexing pass (possibly partial for scheduled work). `vectors`
/// maps cached content signatures to vectors; `signatures` aligns with input
/// documents. `dimension` is 0 when no valid vector was read or embedded.
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

/// Validate candidate cache entries before doing any runtime or lease work.
/// Only a decoded, finite, nonzero vector may establish the model dimension.
/// Reusing this after a lease wait avoids embedding another process's commits.
fn load_cached(
    conn: &Connection,
    signatures: &[String],
    candidates: &[usize],
    mut vectors: Option<&mut HashMap<String, Vec<f32>>>,
    dimension: &mut Option<usize>,
) -> Result<Vec<usize>, String> {
    let mut statement = conn.prepare("SELECT dimension, vector FROM embeddings_v1 WHERE signature = ?1")
        .map_err(|e| e.to_string())?;
    let mut seen = HashSet::new();
    let mut missing = Vec::new();
    for &index in candidates {
        let key = &signatures[index];
        if !seen.insert(key) {
            continue;
        }
        let cached: Option<(i64, Vec<u8>)> = statement.query_row(params![key], |row| Ok((row.get(0)?, row.get(1)?)))
            .optional().map_err(|e| e.to_string())?;
        if let Some((size, bytes)) = cached {
            if let Some(vector) = usize::try_from(size).ok().and_then(|size| decode(&bytes, size)) {
                if dimension.is_none() || *dimension == Some(vector.len()) {
                    *dimension = Some(vector.len());
                    if let Some(vectors) = vectors.as_deref_mut() {
                        vectors.insert(key.clone(), vector);
                    }
                    continue;
                }
            }
        }
        missing.push(index);
    }
    Ok(missing)
}

fn ensure_indexed(
    config: &Config,
    documents: &[crate::telemetry_store::LocalMemorySnapshot],
    mode: IndexingMode,
) -> Result<EnsureIndexedOutcome, String> {
    let (mut conn, cache_path) = open_cache()?;
    ensure_indexed_in_cache(config, documents, mode, &mut conn, &cache_path, None)
}

/// A complete build/search finishes its snapshot; scheduled work returns after
/// a bounded quantum or immediately on contention. Each committed batch drops
/// its lease so a registered foreground search can take over before the next.
fn ensure_indexed_in_cache(
    config: &Config,
    documents: &[crate::telemetry_store::LocalMemorySnapshot],
    mode: IndexingMode,
    conn: &mut Connection,
    cache_path: &std::path::Path,
    foreground: Option<&ForegroundRequest>,
) -> Result<EnsureIndexedOutcome, String> {
    let signatures = documents.iter().map(|doc| signature(config, &doc.memory)).collect::<Vec<_>>();
    let mut vectors = HashMap::new();
    let mut dimension = None;
    // Only searches/manual completion need the full vector corpus in memory;
    // a background quantum only needs validity, dimension and missing indices.
    let retain_vectors = mode == IndexingMode::Complete;
    let missing = load_cached(conn, &signatures, &(0..documents.len()).collect::<Vec<_>>(), retain_vectors.then_some(&mut vectors), &mut dimension)?;
    // A scheduled quantum cannot consume more than this many texts. Avoid
    // planning every remote character-budget batch of a large cold snapshot.
    let planning_limit = if mode == IndexingMode::Background {
        config.batch_size() * BACKGROUND_MAX_BATCHES
    } else {
        missing.len()
    };
    let missing_inputs = missing.iter().take(planning_limit)
        .map(|&index| documents[index].memory.as_str()).collect::<Vec<_>>();
    let planned_batches = plan_batches(config, &missing_inputs);
    let started = Instant::now();
    let mut embedded = 0usize;
    let mut batches = 0usize;
    // Initialize lazily, after both cache checks. Empty and fully cached
    // snapshots never start the runtime, build a client, or acquire a lease.
    let mut client = None;
    for planned in planned_batches {
        if mode.should_yield(batches, started.elapsed()) {
            break;
        }
        // Recheck just this bounded batch, rather than rescanning every
        // outstanding document at each boundary of a large manual build.
        let candidates = planned.iter().map(|&offset| missing[offset]).collect::<Vec<_>>();
        let pending = load_cached(conn, &signatures, &candidates, retain_vectors.then_some(&mut vectors), &mut dimension)?;
        if pending.is_empty() {
            continue;
        }
        let lease = match mode {
            IndexingMode::Background => BuildLease::try_acquire(cache_path, config)?,
            IndexingMode::Complete => Some(BuildLease::acquire(cache_path, config, foreground)?),
        };
        let Some(lease) = lease else { break };
        let pending = load_cached(conn, &signatures, &pending, retain_vectors.then_some(&mut vectors), &mut dimension)?;
        if pending.is_empty() {
            continue;
        }
        lease.renew()?;
        if let Some(request) = foreground {
            request.renew()?;
        }
        if client.is_none() {
            ensure_runtime(config, conn)?;
            client = Some(build_client()?);
        }
        // Startup is another cooperative boundary: if it consumed the time
        // budget or a search arrived meanwhile, release before sending HTTP.
        if mode == IndexingMode::Background
            && (mode.should_yield(batches, started.elapsed()) || lease.foreground_pending()?)
        {
            break;
        }
        // Startup can take 120s; it must not consume the lease's allowance
        // for the subsequent 120s request. Fence again before committing too.
        lease.renew()?;
        if let Some(request) = foreground {
            request.renew()?;
        }
        let batch_inputs = pending.iter().map(|&index| documents[index].memory.as_str()).collect::<Vec<_>>();
        let headers = config.request_headers()?;
        let embedded_batch = embed_batch(client.as_ref().expect("initialized client"), config, &headers, &batch_inputs, dimension)?;
        lease.renew()?;
        if let Some(request) = foreground {
            request.renew()?;
        }
        if dimension.is_none() {
            dimension = embedded_batch.first().map(|vector| vector.len());
        }
        let tx = conn.transaction().map_err(|e| e.to_string())?;
        for (&index, vector) in pending.iter().zip(embedded_batch) {
            let bytes = vector.iter().flat_map(|v| v.to_le_bytes()).collect::<Vec<_>>();
            tx.execute("INSERT OR REPLACE INTO embeddings_v1 (signature, dimension, vector) VALUES (?1, ?2, ?3)",
                params![&signatures[index], vector.len(), bytes]).map_err(|e| e.to_string())?;
            if retain_vectors { vectors.insert(signatures[index].clone(), vector); }
        }
        tx.commit().map_err(|e| e.to_string())?;
        embedded += pending.len();
        batches += 1;
        drop(lease);
        eprintln!("[memory-semantic] indexed batch {}, vectors={}, elapsed_ms={}",
            batches, pending.len(), started.elapsed().as_millis());
    }
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
    let signatures = documents.iter().map(|doc| signature(&config, &doc.memory)).collect::<Vec<_>>();
    let mut dimension = None;
    let missing = load_cached(&conn, &signatures, &(0..documents.len()).collect::<Vec<_>>(), None, &mut dimension)?;
    let missing_keys = missing.iter().map(|&index| signatures[index].as_str()).collect::<HashSet<_>>();
    // Status counts documents, including records sharing the same content.
    // Validate one vector at a time without retaining the whole vector corpus.
    let cached = signatures.iter().filter(|key| !missing_keys.contains(key.as_str())).count();
    let total = documents.len();
    Ok(serde_json::json!({
        "total": total,
        "cached": cached,
        "pending": total.saturating_sub(cached),
    }))
}

/// Manual builds finish their snapshot even when they must yield to searches.
pub(super) fn build_index(telemetry: &TelemetryStore, value: Value) -> Result<Value, String> {
    build_index_with_mode(telemetry, value, IndexingMode::Complete)
}

/// One cooperative scheduled quantum; pending work is resumed by the caller.
pub(super) fn build_index_background(telemetry: &TelemetryStore, value: Value) -> Result<Value, String> {
    build_index_with_mode(telemetry, value, IndexingMode::Background)
}

/// Scheduler-only warmup, including fully cached libraries. Interactive recall
/// never calls this helper or waits for process startup.
pub(super) fn warm_runtime(value: Value) -> Result<(), String> {
    let config = Config::parse(value)?;
    if config.runtime.is_none() { return Ok(()); }
    let (conn, cache_path) = open_cache()?;
    let lease = BuildLease::try_acquire(&cache_path, &config)?
        .ok_or_else(|| "embedding runtime is busy; retry warmup later".to_string())?;
    ensure_runtime(&config, &conn)?;
    lease.renew()?;
    Ok(())
}

fn build_index_with_mode(telemetry: &TelemetryStore, value: Value, mode: IndexingMode) -> Result<Value, String> {
    let config = Config::parse(value.clone())?;
    let documents = telemetry.local_l1_memory_snapshot()?;
    let embedded = ensure_indexed(&config, &documents, mode)?.embedded;
    let mut status = index_status(telemetry, value)?;
    if let Some(object) = status.as_object_mut() {
        object.insert("embedded".into(), Value::from(embedded));
    }
    Ok(status)
}

/// Refinement only consumes valid vectors already cached in the selected
/// scope. Missing documents elsewhere never block it or trigger indexing.
pub(super) fn search_cached(
    telemetry: &TelemetryStore, value: Value, query: &str, limit: u32, domains: Option<&[String]>,
) -> Result<Vec<SemanticMemory>, String> {
    search_impl(telemetry, value, query, limit, domains, false)
}

/// Log fixed stage labels and categories, never endpoints, credentials or text.
fn interactive_stage<T>(stage: &'static str, work: impl FnOnce() -> Result<T, String>) -> Result<T, String> {
    let started = Instant::now();
    let result = work();
    let status = match result.as_ref().err().map(String::as_str) {
        None => "ok",
        Some("recall_deadline" | "embedding total timed out" | "embedding connection timed out"
            | "embedding request timed out" | "embedding response timed out" | "refinement lease timed out") => "timeout",
        Some("recall_cancelled") => "cancelled",
        Some(_) => "error",
    };
    eprintln!("[memory-semantic] stage={stage} elapsed_ms={} status={status}", started.elapsed().as_millis());
    result
}

/// Interactive recall reads existing document vectors only. Cold/partial
/// indexes are left to scheduled builds; no runtime is started on this path.
pub(super) fn search_interactive(
    telemetry: &TelemetryStore, value: Value, query: &str, limit: u32,
    domains: Option<&[String]>, budget: &crate::memory_retrieval::RecallBudget,
) -> Result<super::SemanticRecall, String> {
    search_interactive_at(telemetry, value, query, limit, domains, budget, None)
}

fn search_interactive_at(
    telemetry: &TelemetryStore, value: Value, query: &str, limit: u32,
    domains: Option<&[String]>, budget: &crate::memory_retrieval::RecallBudget,
    evaluation_cache: Option<&std::path::Path>,
) -> Result<super::SemanticRecall, String> {
    budget.remaining()?;
    let config = Config::parse(value)?;
    budget.remaining()?;
    if query.trim().is_empty() {
        return Ok(interactive_recall("local_fallback_cold_index", 0, 0));
    }
    let documents = match interactive_stage("snapshot", || interactive_documents(telemetry, domains, budget)) {
        Ok(documents) => documents,
        Err(_) => {
            budget.remaining()?;
            return Ok(super::SemanticRecall::fallback("local_fallback_busy"));
        }
    };
    let mut recall = interactive_recall("local_fallback_cold_index", documents.len(), 0);
    if documents.is_empty() { return Ok(recall); }
    let cache = match evaluation_cache {
        Some(path) => open_interactive_cache_at(path.to_path_buf(), budget),
        None => open_interactive_cache(budget),
    };
    let (conn, cache_path) = match cache {
        Ok(Some(cache)) => cache,
        Ok(None) => { budget.remaining()?; return Ok(recall); }
        Err(_) => {
            budget.remaining()?;
            recall.mode = "local_fallback_busy";
            recall.cached = None;
            recall.pending = None;
            return Ok(recall);
        }
    };
    let outcome = match interactive_stage("cache_read", || interactive_cached_outcome(&conn, &config, &documents, budget)) {
        Ok(outcome) => outcome,
        Err(_) => {
            budget.remaining()?;
            recall.mode = "local_fallback_busy";
            recall.cached = None;
            recall.pending = None;
            return Ok(recall);
        }
    };
    recall = interactive_index_recall(&outcome, budget)?;
    if outcome.vectors.is_empty() { return Ok(recall); }
    // Native startup belongs to the scheduler. A bounded TCP probe never
    // enters ensure_runtime's process lock or its 120-second startup wait.
    if !config.is_remote() {
        let timeout = budget.remaining()?.min(Duration::from_millis(50));
        let ready = TcpStream::connect_timeout(&runtime_address(&config)?, timeout).is_ok();
        budget.remaining()?;
        if !ready { recall.mode = "local_fallback_runtime"; return Ok(recall); }
    }
    // Do not use lease_connection/new/acquire: their eight-second SQLite and
    // ten-minute acquisition waits are for explicit indexing/refinement.
    let lease_conn = match Connection::open_with_flags(&cache_path, rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE) {
        Ok(conn) => conn,
        Err(_) => {
            budget.remaining()?;
            recall.mode = "local_fallback_busy";
            return Ok(recall);
        }
    };
    let lease = BuildLease {
        conn: lease_conn, route: lease_route(&config), legacy_route: signature(&config, ""),
        owner: uuid::Uuid::new_v4().to_string(),
    };
    let foreground = match interactive_stage("foreground_intent", || ForegroundRequest::register_interactive(&cache_path, &config, budget)) {
        Ok(request) => request,
        Err(_) => {
            let _ = lease.conn.busy_timeout(Duration::ZERO);
            recall.mode = if budget.remaining().is_err() { "local_fallback_budget" } else { "local_fallback_busy" };
            return Ok(recall);
        }
    };
    if interactive_stage("lease_wait", || lease.acquire_interactive(budget)).is_err() {
        recall.mode = if budget.remaining().is_err() { "local_fallback_budget" } else { "local_fallback_busy" };
        return Ok(recall);
    }
    let query_vector = interactive_stage("query_embedding", || embed_interactive_query(&config, query, outcome.dimension, budget))?;
    drop(lease);
    drop(foreground);
    drop(conn);
    budget.remaining()?;
    let mut indexed = HashMap::new();
    for (document, key) in documents.iter().zip(&outcome.signatures) {
        budget.remaining()?;
        indexed.insert(document.id.as_str(), (document.memory.as_str(), key.as_str()));
    }
    // One SQL snapshot rechecks both current text and current domain links.
    // New, removed, edited, or moved-out records cannot reuse an old vector.
    let current = interactive_stage("revalidate", || interactive_documents(telemetry, domains, budget))?;
    let mut results = Vec::new();
    for mut document in current {
        budget.remaining()?;
        let Some(&(content, key)) = indexed.get(document.id.as_str()) else { continue };
        if document.memory != content { continue; }
        let Some(vector) = outcome.vectors.get(key) else { continue };
        document.score = Some(interactive_cosine(&query_vector, vector, budget)?);
        results.push(document);
    }
    budget.remaining()?;
    results.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal).then_with(|| a.id.cmp(&b.id)));
    results.truncate(limit.clamp(1, 100) as usize);
    budget.remaining()?;
    recall.memories = results;
    Ok(recall)
}

fn interactive_recall(mode: &'static str, total: usize, cached: usize) -> super::SemanticRecall {
    let mut recall = super::SemanticRecall::fallback(mode);
    recall.total = Some(total);
    recall.cached = Some(cached);
    recall.pending = Some(total.saturating_sub(cached));
    recall
}

fn interactive_sqlite_budget(conn: &Connection, budget: &RecallBudget) -> Result<(), String> {
    conn.busy_timeout(budget.remaining()?.min(Duration::from_millis(10)))
        .map_err(|_| "embedding cache unavailable".to_string())
}

/// Domain membership and content are read by the same statement, rather than
/// joining two independently timed snapshots. Never wait for the shared mutex
/// or change its busy handler; file stores get a private read-only connection.
fn interactive_documents(
    telemetry: &TelemetryStore, domains: Option<&[String]>, budget: &RecallBudget,
) -> Result<Vec<SemanticMemory>, String> {
    budget.remaining()?;
    if domains.is_some_and(|ids| ids.is_empty()) { return Ok(Vec::new()); }
    let scope = domains.unwrap_or(&[]);
    let condition = if domains.is_some() {
        let placeholders = (1..=scope.len()).map(|index| format!("?{index}")).collect::<Vec<_>>().join(",");
        format!("WHERE EXISTS (SELECT 1 FROM local_memory_domains md WHERE md.memory_id = m.id AND md.domain_id IN ({placeholders}))")
    } else { String::new() };
    let sql = format!("SELECT m.id, m.memory, m.memory_type FROM local_memory_items m {condition} ORDER BY m.updated_at DESC, m.id");
    let read = |conn: &Connection, private: bool| -> Result<Vec<SemanticMemory>, String> {
        budget.remaining()?;
        if private { interactive_sqlite_budget(conn, budget)?; }
        let mut statement = conn.prepare(&sql).map_err(|_| "memory snapshot unavailable".to_string())?;
        if private { interactive_sqlite_budget(conn, budget)?; }
        let mut rows = statement.query(rusqlite::params_from_iter(scope.iter()))
            .map_err(|_| "memory snapshot unavailable".to_string())?;
        let mut documents = Vec::new();
        loop {
            budget.remaining()?;
            if private { interactive_sqlite_budget(conn, budget)?; }
            let Some(row) = rows.next().map_err(|_| "memory snapshot unavailable".to_string())? else { break };
            documents.push(SemanticMemory {
                id: row.get(0).map_err(|_| "invalid memory snapshot".to_string())?,
                memory: row.get(1).map_err(|_| "invalid memory snapshot".to_string())?,
                memory_type: row.get(2).map_err(|_| "invalid memory snapshot".to_string())?, score: None,
            });
        }
        budget.remaining()?;
        Ok(documents)
    };
    if let Some(path) = telemetry.read_database_path() {
        budget.remaining()?;
        let reader = Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .map_err(|_| "memory snapshot unavailable".to_string())?;
        read(&reader, true)
    } else {
        let conn = telemetry.conn.try_lock().map_err(|_| "memory snapshot busy".to_string())?;
        read(&conn, false)
    }
}

/// No directory creation, migrations, WAL changes, or eight-second initializer
/// on a request. Missing/old caches remain cold until a scheduled build runs.
fn open_interactive_cache(budget: &RecallBudget) -> Result<Option<(Connection, std::path::PathBuf)>, String> {
    budget.remaining()?;
    let path = dirs_next::config_dir().ok_or("application data directory unavailable")?
        .join("agent-manager").join("memory-semantic-vectors.sqlite3");
    open_interactive_cache_at(path, budget)
}

fn open_interactive_cache_at(
    path: std::path::PathBuf, budget: &RecallBudget,
) -> Result<Option<(Connection, std::path::PathBuf)>, String> {
    budget.remaining()?;
    if !path.try_exists().map_err(|_| "embedding cache unavailable".to_string())? { return Ok(None); }
    budget.remaining()?;
    let conn = Connection::open_with_flags(&path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|_| "embedding cache unavailable".to_string())?;
    interactive_sqlite_budget(&conn, budget)?;
    let exists: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'embeddings_v1')", [], |row| row.get(0),
    ).map_err(|_| "embedding cache unavailable".to_string())?;
    budget.remaining()?;
    Ok(exists.then_some((conn, path)))
}

// Keep each statement below SQLite's legacy 999-parameter limit and bound the
// work between budget checks. This is a request-local read, never a cache of
// another process's writes across requests.
const INTERACTIVE_CACHE_BATCH_SIZE: usize = 128;

fn interactive_cached_outcome(
    conn: &Connection, config: &Config, documents: &[SemanticMemory], budget: &RecallBudget,
) -> Result<EnsureIndexedOutcome, String> {
    budget.remaining()?;
    let context = SignatureContext::new(config);
    let mut signatures = Vec::with_capacity(documents.len());
    for document in documents {
        budget.remaining()?;
        signatures.push(context.signature(&document.memory));
    }
    let mut seen = HashSet::with_capacity(signatures.len());
    let mut keys = Vec::with_capacity(signatures.len());
    for key in &signatures {
        budget.remaining()?;
        if seen.insert(key.as_str()) { keys.push(key); }
    }
    let mut vectors = HashMap::new();
    let mut dimension = None;
    // Reuse one prepared statement, padding the last batch with NULL keys.
    // Carry ordinals explicitly: neither a join nor an IN lookup guarantees
    // document order, which determines the first valid embedding dimension.
    let values = (0..INTERACTIVE_CACHE_BATCH_SIZE)
        .map(|index| format!("({index},?{})", index + 1)).collect::<Vec<_>>().join(",");
    let sql = format!("WITH requested(ordinal, signature) AS (VALUES {values})
        SELECT requested.ordinal, e.dimension, e.vector FROM requested
        JOIN embeddings_v1 e ON e.signature = requested.signature");
    interactive_sqlite_budget(conn, budget)?;
    let mut statement = conn.prepare(&sql).map_err(|_| "embedding cache unavailable".to_string())?;
    for batch in keys.chunks(INTERACTIVE_CACHE_BATCH_SIZE) {
        interactive_sqlite_budget(conn, budget)?;
        let mut rows = statement.query(rusqlite::params_from_iter(
            (0..INTERACTIVE_CACHE_BATCH_SIZE).map(|index| batch.get(index).map(|key| key.as_str())),
        )).map_err(|_| "embedding cache unavailable".to_string())?;
        let mut decoded = vec![None; batch.len()];
        loop {
            interactive_sqlite_budget(conn, budget)?;
            let Some(row) = rows.next().map_err(|_| "embedding cache unavailable".to_string())? else { break };
            let ordinal: usize = row.get(0).map_err(|_| "embedding cache unavailable".to_string())?;
            // Borrow the SQLite BLOB until the next step, avoiding a temporary
            // byte-vector allocation/copy before the f32 vector is decoded.
            // Malformed storage classes remain cache misses.
            let (size, bytes) = match (
                row.get_ref(1).map_err(|_| "embedding cache unavailable".to_string())?,
                row.get_ref(2).map_err(|_| "embedding cache unavailable".to_string())?,
            ) {
                (rusqlite::types::ValueRef::Integer(size), rusqlite::types::ValueRef::Blob(bytes)) => (size, bytes),
                _ => continue,
            };
            let Some(size) = usize::try_from(size).ok().filter(|size| *size > 0) else { continue };
            if size.checked_mul(4) != Some(bytes.len()) || dimension.is_some_and(|expected| expected != size) { continue; }
            let mut vector = Vec::with_capacity(size);
            let mut nonzero = false;
            let mut valid = true;
            for (index, chunk) in bytes.chunks_exact(4).enumerate() {
                if index % 1024 == 0 { budget.remaining()?; }
                let value = f32::from_le_bytes(chunk.try_into().expect("four bytes"));
                if !value.is_finite() { valid = false; break; }
                nonzero |= value != 0.0;
                vector.push(value);
            }
            if valid && nonzero { decoded[ordinal] = Some(vector); }
        }
        drop(rows);
        for (key, vector) in batch.iter().zip(decoded) {
            budget.remaining()?;
            let Some(vector) = vector else { continue };
            if dimension.is_some_and(|expected| expected != vector.len()) { continue; }
            dimension = Some(vector.len());
            vectors.insert((*key).clone(), vector);
        }
    }
    budget.remaining()?;
    Ok(EnsureIndexedOutcome { signatures, vectors, dimension: dimension.unwrap_or(0), embedded: 0 })
}

fn interactive_index_recall(outcome: &EnsureIndexedOutcome, budget: &RecallBudget) -> Result<super::SemanticRecall, String> {
    budget.remaining()?;
    let mut cached = 0;
    for key in &outcome.signatures {
        budget.remaining()?;
        cached += usize::from(outcome.vectors.contains_key(key));
    }
    let total = outcome.signatures.len();
    let mode = if cached == 0 { "local_fallback_cold_index" }
        else if cached < total { "semantic_partial_index" } else { "semantic" };
    Ok(interactive_recall(mode, total, cached))
}

/// One query only. Both send and body streaming have the current deadline;
/// neither reqwest errors nor response bodies enter a public error string.
fn embed_interactive_query(config: &Config, query: &str, dimension: usize, budget: &RecallBudget) -> Result<Vec<f32>, String> {
    embed_query_with_budget(config, query, dimension, budget, Duration::from_secs(2), false)
}

fn embed_query_with_budget(
    config: &Config, query: &str, dimension: usize, budget: &RecallBudget,
    connect_limit: Duration, detailed_timeouts: bool,
) -> Result<Vec<f32>, String> {
    tauri::async_runtime::block_on(async {
        let remaining = budget.remaining()?;
        let client = reqwest::Client::builder().no_proxy().redirect(reqwest::redirect::Policy::none())
            .connect_timeout(remaining.min(connect_limit)).timeout(remaining)
            .build().map_err(|_| "could not initialize embedding client".to_string())?;
        let input = if config.is_remote() { prepare_remote_text(query) } else { query.to_string() };
        budget.remaining()?;
        let request = client.post(config.endpoint()).headers(config.request_headers()?)
            .json(&serde_json::json!({"model": config.model, "input": [input], "encoding_format": "float"}))
            .timeout(budget.remaining()?);
        let mut response = tokio::time::timeout(budget.remaining()?, request.send()).await
            .map_err(|_| (if detailed_timeouts { "embedding total timed out" } else { "embedding request timed out" }).to_string())?
            .map_err(|error| if error.is_timeout() {
                (if detailed_timeouts && error.is_connect() { "embedding connection timed out" }
                else { "embedding request timed out" }).to_string()
            } else if error.is_connect() {
                "embedding connection failed".to_string()
            } else {
                "embedding request failed".to_string()
            })?;
        budget.remaining()?;
        if !response.status().is_success() {
            return Err(format!("embedding service returned HTTP {}", response.status().as_u16()));
        }
        const MAX_RESPONSE_BYTES: usize = 4 * 1024 * 1024;
        if response.content_length().is_some_and(|length| length > MAX_RESPONSE_BYTES as u64) {
            return Err("embedding response too large".into());
        }
        let mut bytes = Vec::new();
        loop {
            let chunk = tokio::time::timeout(budget.remaining()?, response.chunk()).await
                .map_err(|_| "embedding response timed out".to_string())?
                .map_err(|error| if error.is_timeout() {
                    "embedding response timed out".to_string()
                } else {
                    "could not read embedding response".to_string()
                })?;
            let Some(chunk) = chunk else { break };
            budget.remaining()?;
            if chunk.len() > MAX_RESPONSE_BYTES - bytes.len() { return Err("embedding response too large".into()); }
            bytes.extend_from_slice(&chunk);
        }
        budget.remaining()?;
        let mut response: EmbeddingResponse = serde_json::from_slice(&bytes).map_err(|_| "invalid embedding response".to_string())?;
        budget.remaining()?;
        if response.data.len() != 1 { return Err("embedding response count differs from input count".into()); }
        let item = response.data.pop().expect("one query embedding");
        if item.index != 0 || dimension == 0 || item.embedding.len() != dimension {
            return Err("embedding response has invalid indices or dimensions".into());
        }
        let mut nonzero = false;
        for chunk in item.embedding.chunks(1024) {
            budget.remaining()?;
            if chunk.iter().any(|value| !value.is_finite()) { return Err("invalid embedding vector".into()); }
            nonzero |= chunk.iter().any(|value| *value != 0.0);
        }
        if !nonzero { return Err("invalid embedding vector".into()); }
        budget.remaining()?;
        Ok(item.embedding)
    })
}

fn interactive_cosine(a: &[f32], b: &[f32], budget: &RecallBudget) -> Result<f64, String> {
    if a.is_empty() || a.len() != b.len() { return Err("invalid vectors for cosine similarity".into()); }
    // Both inputs have already been validated, including nonzero norms.
    let (mut dot, mut aa, mut bb) = (0.0_f64, 0.0_f64, 0.0_f64);
    for (index, (&a, &b)) in a.iter().zip(b).enumerate() {
        if index % 1024 == 0 { budget.remaining()?; }
        let (a, b) = (f64::from(a), f64::from(b));
        dot += a * b;
        aa += a * a;
        bb += b * b;
    }
    budget.remaining()?;
    let score = dot / (aa.sqrt() * bb.sqrt());
    if !score.is_finite() { return Err("non-finite cosine similarity".into()); }
    Ok(score.clamp(-1.0, 1.0))
}

fn scoped_documents(
    telemetry: &TelemetryStore, domains: Option<&[String]>,
) -> Result<Vec<crate::telemetry_store::LocalMemorySnapshot>, String> {
    let allowed = domains.map(|ids| telemetry.memory_ids_for_domains(ids)).transpose()?;
    let mut documents = telemetry.local_l1_memory_snapshot()?;
    if let Some(allowed) = allowed { documents.retain(|document| allowed.contains(&document.id)); }
    Ok(documents)
}

fn cached_search_outcome(
    conn: &Connection, config: &Config, documents: &[crate::telemetry_store::LocalMemorySnapshot],
) -> Result<EnsureIndexedOutcome, String> {
    let signatures: Vec<_> = documents.iter().map(|doc| signature(config, &doc.memory)).collect();
    let mut vectors = HashMap::new();
    let mut dimension = None;
    let missing = load_cached(conn, &signatures, &(0..documents.len()).collect::<Vec<_>>(), Some(&mut vectors), &mut dimension)?;
    if !missing.is_empty() {
        return Err(format!("当前检索范围缺少 {} 项有效向量，请完成索引后重试", missing.len()));
    }
    Ok(EnsureIndexedOutcome { signatures, vectors, dimension: dimension.unwrap_or(0), embedded: 0 })
}

fn search_impl(
    telemetry: &TelemetryStore, value: Value, query: &str, limit: u32,
    domains: Option<&[String]>, allow_indexing: bool,
) -> Result<Vec<SemanticMemory>, String> {
    let config = Config::parse(value)?;
    if query.trim().is_empty() {
        return Ok(Vec::new());
    }
    let documents = scoped_documents(telemetry, domains)?;
    if documents.is_empty() {
        return Ok(Vec::new());
    }
    let (mut conn, cache_path) = open_cache()?;
    let foreground = ForegroundRequest::register(&cache_path, &config)?;
    let outcome = if allow_indexing {
        ensure_indexed_in_cache(&config, &documents, IndexingMode::Complete, &mut conn, &cache_path, Some(&foreground))?
    } else {
        // This check happens before runtime startup, lease waiting or HTTP.
        cached_search_outcome(&conn, &config, &documents)?
    };
    // Even a warm-cache query needs runtime startup and serialization. Keep
    // foreground intent across indexing and query embedding so background work
    // cannot reacquire the runtime in the gap between those operations.
    let lease = BuildLease::acquire(&cache_path, &config, Some(&foreground))?;
    ensure_runtime(&config, &conn)?;
    lease.renew()?;
    foreground.renew()?;
    let client = build_client()?;
    let query_vector = embed(&client, &config, &[query], (outcome.dimension > 0).then_some(outcome.dimension))?.remove(0);
    lease.renew()?;
    drop(lease);
    drop(foreground);
    // Cold indexing may take minutes. Re-read the ledger so deleted or edited
    // records cannot leak through the snapshot used to construct the cache.
    let current = scoped_documents(telemetry, domains)?;
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
/// and BGE search). Reads/writes no document vectors; the shared cache holds
/// only its arbitration metadata, and the caller owns vector persistence.
/// Input limits guard the service: at most 64 texts, each
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
    let started = Instant::now();
    let (conn, cache_path) = open_cache()?;
    let foreground = ForegroundRequest::register(&cache_path, &config)?;
    let client = build_client()?;
    let headers = config.request_headers()?;
    let mut vectors = Vec::with_capacity(inputs.len());
    let mut dimension = None;
    for batch in plan_batches(&config, &inputs) {
        let lease = BuildLease::acquire(&cache_path, &config, Some(&foreground))?;
        ensure_runtime(&config, &conn)?;
        lease.renew()?;
        foreground.renew()?;
        let batch_inputs = batch.iter().map(|&index| inputs[index]).collect::<Vec<_>>();
        let embedded = embed_batch(&client, &config, &headers, &batch_inputs, dimension)?;
        lease.renew()?;
        foreground.renew()?;
        if dimension.is_none() {
            dimension = embedded.first().map(|vector| vector.len());
        }
        vectors.extend(embedded);
        // All plans preserve order; keep the marker across batch leases so a
        // desktop search has the same foreground priority as MCP recall.
        drop(lease);
    }
    let dimension = dimension.unwrap_or(0);
    Ok(serde_json::json!({
        "vectors": vectors,
        "dimension": dimension,
        "model": config.model,
        "embedding_space": signature(&config, ""),
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
    fn interactive_query_waits_for_the_current_batch_then_keeps_priority() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("cache.sqlite3");
        let conn = Connection::open(&path).unwrap();
        initialize_cache(&conn).unwrap();
        let config = local_config();
        let background = BuildLease::try_acquire(&path, &config).unwrap().unwrap();
        let budget = RecallBudget::new(Duration::from_secs(2));
        let intent = ForegroundRequest::register_interactive(&path, &config, &budget).unwrap().unwrap();
        let query = BuildLease::new(&path, &config).unwrap();
        let worker = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(40));
            drop(background);
        });
        query.acquire_interactive(&budget).unwrap();
        worker.join().unwrap();
        // Even between the query's lease and intent cleanup, another batch
        // cannot jump ahead. Finishing the query unblocks indexing normally.
        drop(query);
        assert!(BuildLease::try_acquire(&path, &config).unwrap().is_none());
        drop(intent);
        assert!(BuildLease::try_acquire(&path, &config).unwrap().is_some());
    }

    #[test]
    fn interactive_lease_wait_respects_deadline_and_cancellation() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("cache.sqlite3");
        let conn = Connection::open(&path).unwrap();
        initialize_cache(&conn).unwrap();
        let config = local_config();
        let _background = BuildLease::try_acquire(&path, &config).unwrap().unwrap();
        let query = BuildLease::new(&path, &config).unwrap();
        let budget = RecallBudget::new(Duration::from_millis(60));
        let started = Instant::now();
        assert_eq!(query.acquire_interactive(&budget).unwrap_err(), "recall_deadline");
        assert!(started.elapsed() < Duration::from_secs(1));
        let cancelled = RecallBudget::new(Duration::from_secs(2));
        cancelled.cancel();
        assert_eq!(query.acquire_interactive(&cancelled).unwrap_err(), "recall_cancelled");
    }

    #[test]
    fn interactive_cleanup_never_waits_for_a_writer_or_leaves_a_long_lease() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("cache.sqlite3");
        let conn = Connection::open(&path).unwrap();
        initialize_cache(&conn).unwrap();
        let config = local_config();
        let budget = RecallBudget::new(Duration::from_secs(1));
        let intent = ForegroundRequest::register_interactive(&path, &config, &budget).unwrap().unwrap();
        let query = BuildLease::new(&path, &config).unwrap();
        query.acquire_interactive(&budget).unwrap();
        let now = chrono::Utc::now().timestamp();
        let expiry: i64 = conn.query_row("SELECT MAX(expires_at) FROM build_leases_v1", [], |row| row.get(0)).unwrap();
        assert!(expiry <= now + 3, "interactive work must not leave a 180-second lease");
        conn.execute_batch("BEGIN IMMEDIATE").unwrap();
        let started = Instant::now();
        drop(query);
        drop(intent);
        assert!(started.elapsed() < Duration::from_secs(1));
        conn.execute_batch("ROLLBACK").unwrap();
    }

    #[test]
    fn interactive_intent_does_not_migrate_old_cache_metadata() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("cache.sqlite3");
        let conn = Connection::open(&path).unwrap();
        initialize_cache(&conn).unwrap();
        conn.execute_batch("DROP TABLE foreground_requests_v1").unwrap();
        let config = local_config();
        let budget = RecallBudget::new(Duration::from_secs(1));
        assert!(ForegroundRequest::register_interactive(&path, &config, &budget).unwrap().is_none());
        let query = BuildLease::new(&path, &config).unwrap();
        query.acquire_interactive(&budget).unwrap();
        let exists: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name='foreground_requests_v1')", [], |row| row.get(0),
        ).unwrap();
        assert!(!exists);
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
    fn interactive_index_metadata_counts_documents_and_accepts_partial_coverage() {
        let budget = RecallBudget::new(Duration::from_secs(30));
        let mut outcome = EnsureIndexedOutcome {
            signatures: vec!["shared".into(), "missing".into(), "shared".into()],
            vectors: HashMap::from([("shared".into(), vec![1.0, 0.0])]), dimension: 2, embedded: 0,
        };
        let partial = interactive_index_recall(&outcome, &budget).unwrap();
        assert_eq!(partial.mode, "semantic_partial_index");
        assert_eq!((partial.total, partial.cached, partial.pending), (Some(3), Some(2), Some(1)));
        assert!(partial.memories.is_empty());
        outcome.signatures.retain(|key| key == "shared");
        let warm = interactive_index_recall(&outcome, &budget).unwrap();
        assert_eq!(warm.mode, "semantic");
        assert_eq!((warm.total, warm.cached, warm.pending), (Some(2), Some(2), Some(0)));
        outcome.vectors.clear();
        let cold = interactive_index_recall(&outcome, &budget).unwrap();
        assert_eq!(cold.mode, "local_fallback_cold_index");
        assert_eq!((cold.total, cold.cached, cold.pending), (Some(2), Some(0), Some(2)));
        budget.cancel();
        assert!(interactive_index_recall(&outcome, &budget).is_err());
    }

    #[test]
    fn interactive_cosine_checks_budget_and_preserves_numeric_stability() {
        let budget = RecallBudget::new(Duration::from_secs(30));
        assert_eq!(interactive_cosine(&[f32::MAX], &[f32::MAX], &budget).unwrap(), 1.0);
        assert_eq!(interactive_cosine(&[1.0, 0.0], &[0.0, 1.0], &budget).unwrap(), 0.0);
        assert!(interactive_cosine(&[1.0], &[1.0, 2.0], &budget).is_err());
        assert!(interactive_cosine(&[0.0], &[1.0], &budget).is_err());
        budget.cancel();
        assert!(interactive_cosine(&vec![1.0; 4096], &vec![1.0; 4096], &budget).is_err());
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
    fn request_signature_prefix_is_byte_compatible_with_v1() {
        let mut config = local_config();
        let long = "escaped \"text\"\\\n中文".repeat(1024);
        let contents = ["", "memory", "memory ", "中文", "\"\\\n\r\t\u{0000}\u{0008}\u{000c}", "\u{2028}\u{2029}", &long];
        for base_url in ["http://127.0.0.1:7997/v1", "https://example.com/custom/v1"] {
            config.base_url = base_url.into();
            for model in ["bge-m3", "model\"\\\n中文"] {
                config.model = model.into();
                for key in [None, Some(""), Some("secret-one"), Some("secret-two")] {
                    config.api_key = key.map(str::to_string);
                    let context = SignatureContext::new(&config);
                    for content in contents {
                        assert_eq!(context.signature(content), signature(&config, content));
                        assert_eq!(context.signature(content), signature(&config, content));
                    }
                }
            }
        }
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
        let lease = BuildLease::acquire(&path, &config, None).unwrap();
        lease.renew().unwrap();
        let count: i64 = conn.query_row("SELECT count(*) FROM build_leases_v1", [], |row| row.get(0)).unwrap();
        assert_eq!(count, 2);
        drop(lease);
        let count: i64 = conn.query_row("SELECT count(*) FROM build_leases_v1", [], |row| row.get(0)).unwrap();
        assert_eq!(count, 0);
        let lease = BuildLease::acquire(&path, &config, None).unwrap();
        conn.execute("UPDATE build_leases_v1 SET owner = 'replacement'", []).unwrap();
        assert!(lease.renew().is_err());
        drop(lease);
        let count: i64 = conn.query_row("SELECT count(*) FROM build_leases_v1 WHERE owner = 'replacement'", [], |row| row.get(0)).unwrap();
        assert_eq!(count, 2);
    }

    fn document(id: &str, content: &str) -> crate::telemetry_store::LocalMemorySnapshot {
        crate::telemetry_store::LocalMemorySnapshot {
            id: id.into(), source_event_key: String::new(), ordinal: 0,
            memory: content.into(), memory_type: "fact".into(), durability: "long".into(),
            source: "test".into(), session_id: String::new(), user_defined: false,
            event_time: None, created_at: String::new(), updated_at: String::new(),
        }
    }

    fn cache_vector(conn: &Connection, key: &str, dimension: i64, vector: &[f32]) {
        let bytes = vector.iter().flat_map(|v| v.to_le_bytes()).collect::<Vec<_>>();
        conn.execute("INSERT OR REPLACE INTO embeddings_v1 VALUES (?1, ?2, ?3)",
            params![key, dimension, bytes]).unwrap();
    }

    #[test]
    fn interactive_batches_preserve_dimension_order_counts_and_corrupt_misses() {
        let conn = Connection::open_in_memory().unwrap();
        initialize_cache(&conn).unwrap();
        // Exercise a row order different from the order of requested documents.
        conn.execute_batch("PRAGMA reverse_unordered_selects=ON").unwrap();
        let config = local_config();
        let count = INTERACTIVE_CACHE_BATCH_SIZE * 2 + 5;
        let mut documents = (0..count).map(|index| SemanticMemory {
            id: index.to_string(), memory: format!("memory-{index}"), memory_type: "fact".into(), score: None,
        }).collect::<Vec<_>>();
        let keys = documents.iter().map(|document| signature(&config, &document.memory)).collect::<Vec<_>>();
        for (index, size, vector) in [
            (0, 99, vec![1.0]), (1, -1, vec![1.0]), (2, 2, vec![0.0, -0.0]),
            (3, 2, vec![f32::NAN, 1.0]), (4, 2, vec![f32::INFINITY, 1.0]),
            (7, 0, vec![]), (10, i64::MAX, vec![1.0]),
        ] {
            cache_vector(&conn, &keys[index], size, &vector);
        }
        conn.execute("INSERT INTO embeddings_v1 VALUES (?1, 'broken', X'0000803F')", [&keys[5]]).unwrap();
        conn.execute("INSERT INTO embeddings_v1 VALUES (?1, 2, 'not a blob')", [&keys[6]]).unwrap();
        conn.execute("INSERT INTO embeddings_v1 VALUES (?1, 1, X'000080')", [&keys[8]]).unwrap();
        conn.execute("INSERT INTO embeddings_v1 VALUES (?1, 2.5, X'0000803F')", [&keys[9]]).unwrap();
        for (index, key) in keys.iter().enumerate().skip(INTERACTIVE_CACHE_BATCH_SIZE) {
            if index == INTERACTIVE_CACHE_BATCH_SIZE + 1 {
                cache_vector(&conn, key, 3, &[1.0, 0.0, 0.0]);
            } else {
                cache_vector(&conn, key, 2, &[1.0, 0.0]);
            }
        }
        // Duplicate valid and missing content both retain document counts.
        for index in [INTERACTIVE_CACHE_BATCH_SIZE, 20] {
            documents.push(SemanticMemory {
                id: format!("duplicate-{index}"), memory: documents[index].memory.clone(),
                memory_type: "fact".into(), score: None,
            });
        }
        let budget = RecallBudget::new(Duration::from_secs(30));
        let outcome = interactive_cached_outcome(&conn, &config, &documents, &budget).unwrap();
        assert_eq!(outcome.dimension, 2);
        assert_eq!(outcome.embedded, 0);
        assert_eq!(outcome.signatures, documents.iter().map(|document| signature(&config, &document.memory)).collect::<Vec<_>>());
        assert_eq!(outcome.vectors.len(), count - INTERACTIVE_CACHE_BATCH_SIZE - 1);
        for (index, key) in keys.iter().enumerate() {
            assert_eq!(outcome.vectors.contains_key(key), index >= INTERACTIVE_CACHE_BATCH_SIZE && index != INTERACTIVE_CACHE_BATCH_SIZE + 1);
        }
        let recall = interactive_index_recall(&outcome, &budget).unwrap();
        assert_eq!(recall.mode, "semantic_partial_index");
        assert_eq!(recall.total, Some(count + 2));
        assert_eq!(recall.cached, Some(count - INTERACTIVE_CACHE_BATCH_SIZE));
        assert_eq!(recall.pending, Some(INTERACTIVE_CACHE_BATCH_SIZE + 2));
    }

    #[test]
    fn interactive_cache_reads_observe_external_writes_and_respect_budget() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("cache.sqlite3");
        let writer = Connection::open(&path).unwrap();
        initialize_cache(&writer).unwrap();
        let reader = Connection::open_with_flags(&path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
        let config = local_config();
        let documents = [SemanticMemory { id: "one".into(), memory: "memory".into(), memory_type: "fact".into(), score: None }];
        let key = signature(&config, &documents[0].memory);
        let budget = RecallBudget::new(Duration::from_secs(30));
        let read = || interactive_cached_outcome(&reader, &config, &documents, &budget).unwrap();
        assert!(read().vectors.is_empty());
        cache_vector(&writer, &key, 2, &[1.0, 0.0]);
        assert_eq!(read().vectors[&key], vec![1.0, 0.0]);
        cache_vector(&writer, &key, 3, &[0.0, 1.0, 0.0]);
        let updated = read();
        assert_eq!(updated.dimension, 3);
        assert_eq!(updated.vectors[&key], vec![0.0, 1.0, 0.0]);
        writer.execute("DELETE FROM embeddings_v1", []).unwrap();
        assert!(read().vectors.is_empty());
        // No schema or lease access is needed even for a cache predating them.
        writer.execute_batch("DROP TABLE build_leases_v1; DROP TABLE foreground_requests_v1; DROP TABLE runtime_starts_v1").unwrap();
        assert!(read().vectors.is_empty());
        let empty_cache = Connection::open_in_memory().unwrap();
        budget.cancel();
        assert_eq!(interactive_cached_outcome(&empty_cache, &config, &documents, &budget).err().unwrap(), "recall_cancelled");
        let expired = RecallBudget::new(Duration::ZERO);
        assert_eq!(interactive_cached_outcome(&empty_cache, &config, &documents, &expired).err().unwrap(), "recall_deadline");
    }

    #[test]
    fn refinement_cache_readiness_is_scoped_before_missing_vectors_are_checked() {
        let store = TelemetryStore::in_memory();
        let mut documents = vec![document("law", "A legal fact"), document("other", "An unrelated fact")];
        for document in &mut documents { document.source_event_key = document.id.clone(); }
        store.restore_local_l1_memory_snapshot(&documents).unwrap();
        {
            let conn = store.conn.lock().unwrap();
            for (id, domain) in [("law", "custom:law"), ("other", "custom:other")] {
                conn.execute("INSERT INTO memory_domains(id, name, kind, created_at, updated_at)
                    VALUES (?1, ?1, 'custom', 'now', 'now')", [domain]).unwrap();
                conn.execute("INSERT INTO local_memory_domains(memory_id, domain_id, weight, source, created_at, updated_at)
                    VALUES (?1, ?2, 1, 'auto', 'now', 'now')", params![id, domain]).unwrap();
            }
        }
        let conn = Connection::open_in_memory().unwrap();
        initialize_cache(&conn).unwrap();
        conn.execute_batch("CREATE TRIGGER reject_cache_lease BEFORE INSERT ON build_leases_v1
                BEGIN SELECT RAISE(FAIL, 'cached lookup acquired a lease'); END;
            CREATE TRIGGER reject_cache_runtime BEFORE INSERT ON runtime_starts_v1
                BEGIN SELECT RAISE(FAIL, 'cached lookup started a runtime'); END;").unwrap();
        let config = local_config();
        cache_vector(&conn, &signature(&config, "A legal fact"), 2, &[1.0, 0.0]);
        let scoped = scoped_documents(&store, Some(&["custom:law".into()])).unwrap();
        assert_eq!(scoped.len(), 1);
        assert_eq!(scoped[0].id, "law");
        let outcome = cached_search_outcome(&conn, &config, &scoped).unwrap();
        assert_eq!(outcome.embedded, 0);
        assert_eq!(outcome.vectors.len(), 1);
        assert!(cached_search_outcome(&conn, &config, &documents).is_err(), "the other label is cold");
        assert!(scoped_documents(&store, Some(&[])).unwrap().is_empty());
        let cached: i64 = conn.query_row("SELECT count(*) FROM embeddings_v1", [], |row| row.get(0)).unwrap();
        assert_eq!(cached, 1, "readiness must not index missing documents");
    }

    #[test]
    fn scoped_cache_readiness_still_rejects_missing_and_corrupt_vectors() {
        let conn = Connection::open_in_memory().unwrap();
        initialize_cache(&conn).unwrap();
        let config = local_config();
        let documents = [document("law", "A legal fact")];
        assert!(cached_search_outcome(&conn, &config, &documents).is_err());
        let key = signature(&config, &documents[0].memory);
        cache_vector(&conn, &key, 3, &[1.0, 0.0]);
        assert!(cached_search_outcome(&conn, &config, &documents).is_err());
        cache_vector(&conn, &key, 2, &[1.0, 0.0]);
        assert!(cached_search_outcome(&conn, &config, &documents).is_ok());
    }

    #[test]
    fn empty_and_warm_builds_never_touch_runtime_or_build_lease() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("cache.sqlite3");
        let mut conn = Connection::open(&path).unwrap();
        initialize_cache(&conn).unwrap();
        let mut config = local_config();
        // If startup is reached, the runtime lease trigger fails before any
        // spawn. Port zero also cannot accidentally reuse a running service.
        config.base_url = "http://127.0.0.1:0/v1".into();
        config.runtime = Some(Runtime {
            command: "nonexistent-embedding-runtime-for-noop-test".into(),
            args: Vec::new(), env: HashMap::new(),
        });
        conn.execute_batch("CREATE TRIGGER reject_build_lease BEFORE INSERT ON build_leases_v1
                BEGIN SELECT RAISE(FAIL, 'no-op acquired build lease'); END;
            CREATE TRIGGER reject_runtime_start BEFORE INSERT ON runtime_starts_v1
                BEGIN SELECT RAISE(FAIL, 'no-op started runtime'); END;").unwrap();
        let documents = vec![document("one", "cached"), document("two", "cached")];
        let key = signature(&config, "cached");
        cache_vector(&conn, &key, 2, &[1.0, 0.0]);
        for mode in [IndexingMode::Complete, IndexingMode::Background] {
            let empty = ensure_indexed_in_cache(&config, &[], mode, &mut conn, &path, None).unwrap();
            assert_eq!(empty.embedded, 0);
            assert_eq!(empty.dimension, 0);
            assert!(empty.vectors.is_empty());
            let warm = ensure_indexed_in_cache(&config, &documents, mode, &mut conn, &path, None).unwrap();
            assert_eq!(warm.embedded, 0);
            assert_eq!(warm.dimension, 2);
            assert_eq!(warm.signatures, vec![key.clone(), key.clone()]);
            assert_eq!(warm.vectors.len(), usize::from(mode == IndexingMode::Complete));
        }
    }

    #[test]
    fn cache_preflight_rejects_corruption_without_poisoning_dimension() {
        let conn = Connection::open_in_memory().unwrap();
        initialize_cache(&conn).unwrap();
        let keys = ["broken", "valid", "other-dimension", "zero", "negative", "nan", "valid"]
            .map(str::to_string);
        cache_vector(&conn, "broken", 99, &[1.0]);
        cache_vector(&conn, "valid", 2, &[1.0, 0.0]);
        cache_vector(&conn, "other-dimension", 3, &[1.0, 0.0, 0.0]);
        cache_vector(&conn, "zero", 2, &[0.0, 0.0]);
        cache_vector(&conn, "negative", -1, &[1.0]);
        cache_vector(&conn, "nan", 2, &[f32::NAN, 1.0]);
        let mut vectors = HashMap::new();
        let mut dimension = None;
        let missing = load_cached(&conn, &keys, &(0..keys.len()).collect::<Vec<_>>(), Some(&mut vectors), &mut dimension).unwrap();
        assert_eq!(dimension, Some(2));
        assert_eq!(missing, vec![0, 2, 3, 4, 5]);
        assert_eq!(vectors.len(), 1);
        // Status uses the same validation without retaining all vectors.
        let mut status_dimension = None;
        assert_eq!(load_cached(&conn, &keys, &(0..keys.len()).collect::<Vec<_>>(), None, &mut status_dimension).unwrap(), missing);
    }

    #[test]
    fn foreground_intent_wins_next_batch_and_guards_warm_queries() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("cache.sqlite3");
        let conn = Connection::open(&path).unwrap();
        initialize_cache(&conn).unwrap();
        let config = local_config();
        let background = BuildLease::try_acquire(&path, &config).unwrap().unwrap();
        let first = ForegroundRequest::register(&path, &config).unwrap();
        let second = ForegroundRequest::register(&path, &config).unwrap();
        // Foreground intent never preempts an in-flight request.
        let contender = BuildLease::new(&path, &config).unwrap();
        assert!(!contender.try_claim(true).unwrap());
        cache_vector(&conn, "committed-batch", 2, &[1.0, 0.0]);
        drop(background);
        assert!(BuildLease::try_acquire(&path, &config).unwrap().is_none());
        let foreground = BuildLease::acquire(&path, &config, Some(&first)).unwrap();
        drop(first);
        assert!(BuildLease::try_acquire(&path, &config).unwrap().is_none());
        drop(foreground);
        // A second search's intent survives the first search finishing, and
        // still blocks builders during a warm-cache query's acquisition gap.
        assert!(BuildLease::try_acquire(&path, &config).unwrap().is_none());
        second.renew().unwrap();
        drop(second);
        let next = BuildLease::try_acquire(&path, &config).unwrap().unwrap();
        let bytes: Vec<u8> = conn.query_row("SELECT vector FROM embeddings_v1 WHERE signature = 'committed-batch'", [], |row| row.get(0)).unwrap();
        assert_eq!(decode(&bytes, 2).unwrap(), vec![1.0, 0.0]);
        drop(next);
    }

    #[test]
    fn contended_background_pass_yields_before_starting_runtime() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("cache.sqlite3");
        let mut conn = Connection::open(&path).unwrap();
        initialize_cache(&conn).unwrap();
        let mut config = local_config();
        config.base_url = "http://127.0.0.1:0/v1".into();
        config.runtime = Some(Runtime {
            command: "nonexistent-embedding-runtime-for-yield-test".into(),
            args: Vec::new(), env: HashMap::new(),
        });
        conn.execute_batch("CREATE TRIGGER reject_runtime_start BEFORE INSERT ON runtime_starts_v1
            BEGIN SELECT RAISE(FAIL, 'contended background started runtime'); END;").unwrap();
        let foreground = ForegroundRequest::register(&path, &config).unwrap();
        let documents = vec![document("pending", "not cached")];
        let outcome = ensure_indexed_in_cache(&config, &documents, IndexingMode::Background, &mut conn, &path, None).unwrap();
        assert_eq!(outcome.embedded, 0);
        assert_eq!(outcome.dimension, 0);
        assert!(outcome.vectors.is_empty());
        let count: i64 = conn.query_row("SELECT count(*) FROM build_leases_v1", [], |row| row.get(0)).unwrap();
        assert_eq!(count, 0);
        drop(foreground);
    }

    #[test]
    fn stale_foreground_and_build_owners_do_not_block_other_routes() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("cache.sqlite3");
        let conn = Connection::open(&path).unwrap();
        initialize_cache(&conn).unwrap();
        let config = local_config();
        let request = ForegroundRequest::register(&path, &config).unwrap();
        let mut other = local_config();
        other.base_url = "http://127.0.0.1:7998/v1".into();
        let unrelated = BuildLease::try_acquire(&path, &other).unwrap().unwrap();
        drop(unrelated);
        conn.execute("UPDATE foreground_requests_v1 SET expires_at = 0", []).unwrap();
        let expired = BuildLease::try_acquire(&path, &config).unwrap().unwrap();
        conn.execute("UPDATE build_leases_v1 SET expires_at = 0", []).unwrap();
        let replacement = BuildLease::try_acquire(&path, &config).unwrap().unwrap();
        assert!(expired.renew().is_err());
        drop(expired);
        replacement.renew().unwrap();
        drop(replacement);
        drop(request);
    }

    #[test]
    fn legacy_build_lease_is_honored_without_partial_endpoint_claims() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("cache.sqlite3");
        let conn = Connection::open(&path).unwrap();
        initialize_cache(&conn).unwrap();
        let config = local_config();
        conn.execute("INSERT INTO build_leases_v1 VALUES (?1, 'legacy-owner', ?2)",
            params![signature(&config, ""), chrono::Utc::now().timestamp() + LEASE_SECONDS]).unwrap();
        assert!(BuildLease::try_acquire(&path, &config).unwrap().is_none());
        let endpoint_count: i64 = conn.query_row("SELECT count(*) FROM build_leases_v1 WHERE route = ?1",
            [lease_route(&config)], |row| row.get(0)).unwrap();
        assert_eq!(endpoint_count, 0);
        conn.execute("DELETE FROM build_leases_v1 WHERE owner = 'legacy-owner'", []).unwrap();
        let lease = BuildLease::try_acquire(&path, &config).unwrap().unwrap();
        lease.renew().unwrap();
        // Losing either lock fences the owner, even if the endpoint lock is
        // still ours. A failed renewal must roll back both rows together.
        conn.execute("UPDATE build_leases_v1 SET owner = 'replacement' WHERE route = ?1",
            [signature(&config, "")]).unwrap();
        assert!(lease.renew().is_err());
        drop(lease);
        let count: i64 = conn.query_row("SELECT count(*) FROM build_leases_v1 WHERE owner = 'replacement'", [], |row| row.get(0)).unwrap();
        assert_eq!(count, 1);
    }

    #[test]
    fn runtime_lease_is_shared_across_models_and_credentials() {
        let config = local_config();
        let mut changed = local_config();
        changed.model = "another-model".into();
        changed.api_key = Some("another-key".into());
        assert_ne!(signature(&config, "memory"), signature(&changed, "memory"));
        assert_eq!(lease_route(&config), lease_route(&changed));
    }

    #[test]
    fn background_quantum_is_bounded_but_manual_builds_complete() {
        assert!(!IndexingMode::Background.should_yield(0, Duration::ZERO));
        assert!(IndexingMode::Background.should_yield(BACKGROUND_MAX_BATCHES, Duration::ZERO));
        assert!(IndexingMode::Background.should_yield(1, BACKGROUND_BUDGET));
        assert!(!IndexingMode::Complete.should_yield(BACKGROUND_MAX_BATCHES, BACKGROUND_BUDGET));
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
