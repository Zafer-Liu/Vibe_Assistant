//! 内置记忆引擎托管：随应用启动/停止 Qdrant、Neo4j、Embedding 代理与记忆 API。
//!
//! Vibe Assistant 将长期记忆能力作为原生功能内置：引擎组件随应用启动时自动拉起
//! （已在运行则复用），退出时由应用停止由本模块拉起的进程。API 细节对用户隐藏。

use crate::process_util::no_window;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};
use tauri::Emitter;

const QDRANT_PORT: u16 = 6333;
const NEO4J_PORT: u16 = 7474;
// Do not share the conventional development ports 8000/8001.  They are
// commonly occupied by user projects and an unrelated service must never be
// mistaken for Vibe Assistant's private memory sidecar.
const EMBED_PORT: u16 = 18001;
const API_PORT: u16 = 18000;

const DEFAULT_API_KEY: &str = "dev-api-key-001";
use crate::llm;
use crate::telemetry_store::{MemoryImportance, MemoryImportanceCandidate};

const SEMANTIC_MEMORY_OWNER: &str = "agent-manager";
const SEMANTIC_INDEX_BATCH_SIZE: usize = 24;

fn semantic_api_key() -> String {
    std::env::var("MINDMEMOS_API_KEY").unwrap_or_else(|_| DEFAULT_API_KEY.to_string())
}

/// A result returned by the optional MindMemOS vector index.  The native L1
/// ledger remains authoritative; this is only the semantic retrieval layer
/// used to find an otherwise differently-worded L1 item.
#[derive(Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub struct SemanticMemory {
    pub id: String,
    pub memory: String,
    pub memory_type: String,
    pub score: Option<f64>,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
struct ConsolidationSnapshot {
    id: String,
    created_at: String,
    #[serde(default)]
    memories: Vec<SnapshotMemory>,
    #[serde(default)]
    local_memories: Vec<crate::telemetry_store::LocalMemorySnapshot>,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
struct SnapshotMemory {
    id: String,
    memory: String,
    memory_type: Option<String>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub struct MemoryImportanceSummary {
    pub reviewed: usize,
    pub high: usize,
    pub medium: usize,
    pub low: usize,
    pub message: String,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub struct ConsolidationResult {
    pub snapshot_id: String,
    pub before_count: usize,
    pub after_count: usize,
    pub scopes: i64,
    pub clusters: i64,
    pub actions: i64,
    pub message: String,
}

#[derive(Deserialize)]
struct ConsolidationPlan {
    #[serde(default)]
    actions: Vec<ConsolidationAction>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum ConsolidationPlanResponse {
    Wrapped(ConsolidationPlan),
    Bare(Vec<ConsolidationAction>),
}

#[derive(Deserialize)]
struct ConsolidationAction {
    keep_id: String,
    #[serde(default)]
    remove_ids: Vec<String>,
    content: String,
    #[serde(default)]
    reason: String,
}

#[derive(Clone, Deserialize)]
pub struct ConsolidationCandidate {
    pub id: String,
    // These fields make the request self-describing for diagnostics, but are
    // deliberately ignored: native SQLite reloads the authoritative content.
    #[serde(rename = "memory")]
    _memory: String,
    #[serde(rename = "memory_type")]
    _memory_type: Option<String>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub struct ComponentStatus {
    pub name: &'static str,
    pub online: bool,
    pub detail: String,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub struct EngineStatus {
    pub online: bool,
    pub api_port: u16,
    pub components: Vec<ComponentStatus>,
}

#[derive(Clone)]
pub struct MemoryBackend {
    pub root: PathBuf,
    pub api_key: String,
    children: Arc<Mutex<HashMap<String, Child>>>,
}

/// 全局共享后端（供无 State 上下文的模块——如 agent_http/ingest——使用）。
static SHARED: OnceLock<Arc<MemoryBackend>> = OnceLock::new();

pub fn init_shared(backend: MemoryBackend) -> Arc<MemoryBackend> {
    let arc = Arc::new(backend);
    let _ = SHARED.set(Arc::clone(&arc));
    arc
}

pub fn shared_backend() -> Option<Arc<MemoryBackend>> {
    SHARED.get().cloned()
}

impl MemoryBackend {
    pub fn new() -> Self {
        // Release builds use the runtime shipped with Vibe Assistant.  An
        // explicit MINDMEMOS_HOME remains a development-only override, which
        // keeps local contributors productive without silently coupling user
        // installs to an arbitrary pre-existing server.
        let bundled_root = bundled_sidecar_root();
        let root = if cfg!(debug_assertions) {
            std::env::var("MINDMEMOS_HOME")
                .map(PathBuf::from)
                .unwrap_or(bundled_root)
        } else {
            bundled_root
        };
        let api_key =
            std::env::var("MINDMEMOS_API_KEY").unwrap_or_else(|_| DEFAULT_API_KEY.to_string());
        Self {
            root,
            api_key,
            children: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    fn api_base(&self) -> String {
        format!("http://127.0.0.1:{API_PORT}")
    }

    pub fn api_url(&self, path: &str) -> String {
        format!("{}{}", self.api_base(), path)
    }

    pub fn api_key(&self) -> &str {
        &self.api_key
    }

    fn consolidation_snapshot_dir(&self) -> PathBuf {
        dirs_next::config_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("agent-manager")
            .join("memory-backups")
    }

    fn probe(port: u16) -> bool {
        std::net::TcpStream::connect_timeout(
            &format!("127.0.0.1:{port}").parse().unwrap(),
            Duration::from_millis(700),
        )
        .is_ok()
    }

    /// The semantic service is a private sidecar.  A listening socket alone is
    /// deliberately insufficient: another local program must never become our
    /// memory engine merely because it happened to use the same port.
    pub fn api_online(&self) -> bool {
        self.children.lock().unwrap().contains_key("api") && Self::probe(API_PORT)
    }

    fn spawn(&self, name: &str, mut cmd: Command) {
        let child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => {
                eprintln!("[memory-engine] {name} spawn failed: {e}");
                return;
            }
        };
        self.children
            .lock()
            .unwrap()
            .insert(name.to_string(), child);
        eprintln!("[memory-engine] {name} started");
    }

    /// 确保 Vibe Assistant 自己的语义 sidecar 运行。
    pub fn ensure_started(&self) {
        let runtime_python = self.root.join(".venv").join("Scripts").join("python.exe");
        if !runtime_python.exists() {
            eprintln!("[memory-engine] bundled sidecar runtime is missing at {}; semantic indexing remains disabled", runtime_python.display());
            return;
        }
        // 1) Qdrant 向量库
        if !Self::probe(QDRANT_PORT) {
            let qdir = self.root.join(".devtools").join("qdrant");
            let exe = qdir.join("qdrant.exe");
            if exe.exists() {
                let mut cmd = Command::new(&exe);
                cmd.current_dir(&qdir);
                cmd.stdout(Stdio::null()).stderr(Stdio::null());
                no_window(&mut cmd);
                self.spawn("qdrant", cmd);
            } else {
                eprintln!("[memory-engine] qdrant.exe not found at {}", exe.display());
            }
        }

        // 2) Neo4j 知识图谱（需 JDK）
        if !Self::probe(NEO4J_PORT) {
            let neo4j_home = self
                .root
                .join(".devtools")
                .join("neo4j")
                .join("neo4j-community-5.26.0");
            let bat = neo4j_home.join("bin").join("neo4j.bat");
            if bat.exists() {
                let jdk = self
                    .root
                    .join(".devtools")
                    .join("jdk")
                    .join("jdk-17.0.20+8");
                let java_home = if jdk.exists() {
                    jdk.to_string_lossy().into_owned()
                } else {
                    String::new()
                };
                let mut cmd = Command::new("cmd");
                cmd.args(["/c", "bin\\neo4j.bat console"]);
                cmd.current_dir(&neo4j_home);
                if !java_home.is_empty() {
                    cmd.env("JAVA_HOME", &java_home);
                }
                cmd.stdout(Stdio::null()).stderr(Stdio::null());
                no_window(&mut cmd);
                self.spawn("neo4j", cmd);
            } else {
                eprintln!("[memory-engine] neo4j.bat not found at {}", bat.display());
            }
        }

        // 3) Embedding 代理（MiniMax → OpenAI 兼容）
        if !Self::probe(EMBED_PORT) {
            let proxy = self.root.join(".devtools").join("embed_proxy.py");
            if proxy.exists() {
                let mut cmd = Command::new(&runtime_python);
                cmd.arg(&proxy);
                cmd.current_dir(self.root.join(".devtools"));
                cmd.env("EMBED_PROXY_PORT", EMBED_PORT.to_string());
                cmd.stdout(Stdio::null()).stderr(Stdio::null());
                no_window(&mut cmd);
                self.spawn("embedding", cmd);
            } else {
                eprintln!(
                    "[memory-engine] embedding proxy missing at {}",
                    proxy.display()
                );
            }
        }

        // 4) Private memory API service.  Never reuse a process that was not
        // launched by this MemoryBackend instance.
        if !Self::probe(API_PORT) {
            if runtime_python.exists() {
                let mut cmd = Command::new(&runtime_python);
                cmd.args([
                    "-m",
                    "uvicorn",
                    "mindmemos.api.app:app",
                    "--host",
                    "127.0.0.1",
                    "--port",
                    &API_PORT.to_string(),
                ]);
                cmd.current_dir(&self.root);
                cmd.env("PYTHONPATH", "");
                cmd.env("MINDMEMOS_CONFIG_NAME", "dev");
                cmd.env("AGENT_MANAGER_MEMORY_SIDECAR", "1");
                cmd.env(
                    "MINDMEMOS_QDRANT_URL",
                    format!("http://127.0.0.1:{QDRANT_PORT}"),
                );
                cmd.env("MINDMEMOS_QDRANT_GRPC_PORT", "6334");
                cmd.env("MINDMEMOS_QDRANT_PREFER_GRPC", "false");
                cmd.env("MINDMEMOS_NEO4J_URI", "bolt://127.0.0.1:7687");
                cmd.env("MINDMEMOS_NEO4J_USERNAME", "neo4j");
                cmd.env("MINDMEMOS_NEO4J_PASSWORD", "mindmemos_dev_password");
                cmd.stdout(Stdio::null()).stderr(Stdio::null());
                no_window(&mut cmd);
                self.spawn("api", cmd);
            }
        } else if !self.children.lock().unwrap().contains_key("api") {
            eprintln!("[memory-engine] private API port {API_PORT} is occupied by another process; refusing to reuse it");
        }
    }

    /// 停止本模块拉起的子进程。
    pub fn stop(&self) {
        let mut children = self.children.lock().unwrap();
        for (name, mut child) in children.drain() {
            let _ = child.kill();
            let _ = child.wait();
            eprintln!("[memory-engine] {name} stopped");
        }
    }

    /// 组件在线状态。
    pub fn status(&self) -> EngineStatus {
        let qdrant_ok = Self::probe(QDRANT_PORT);
        let neo4j_ok = Self::probe(NEO4J_PORT);
        let embed_ok = Self::probe(EMBED_PORT);
        let api_port_open = Self::probe(API_PORT);
        let api_ok = self.api_online();
        let runtime_missing = !self
            .root
            .join(".venv")
            .join("Scripts")
            .join("python.exe")
            .exists();
        let api_detail = if api_ok {
            format!("Vibe Assistant 专属语义服务 :{API_PORT}")
        } else if runtime_missing {
            "Vibe Assistant 内置语义运行时未安装".into()
        } else if api_port_open {
            format!("端口 :{API_PORT} 被外部进程占用；未复用")
        } else if self.children.lock().unwrap().contains_key("api") {
            format!("Vibe Assistant 语义服务启动中 :{API_PORT}")
        } else {
            format!("Vibe Assistant 专属语义服务未启动 :{API_PORT}")
        };

        EngineStatus {
            online: api_ok,
            api_port: API_PORT,
            components: vec![
                ComponentStatus {
                    name: "qdrant",
                    online: qdrant_ok,
                    detail: format!("向量库 :{QDRANT_PORT}"),
                },
                ComponentStatus {
                    name: "neo4j",
                    online: neo4j_ok,
                    detail: format!("图谱 :{NEO4J_PORT}"),
                },
                ComponentStatus {
                    name: "embedding",
                    online: embed_ok,
                    detail: format!("向量化 :{EMBED_PORT}"),
                },
                ComponentStatus {
                    name: "api",
                    online: api_ok,
                    detail: api_detail,
                },
            ],
        }
    }
}

fn bundled_sidecar_root() -> PathBuf {
    // Tauri places `bundle.resources` beside the executable in release
    // builds. During development this also gives us a predictable workspace
    // location, instead of discovering or sharing a process on port 8000.
    if !cfg!(debug_assertions) {
        let executable_dir = std::env::current_exe()
            .ok()
            .and_then(|path| path.parent().map(PathBuf::from));
        if let Some(dir) = executable_dir {
            let direct = dir.join("resources").join("mindmemos");
            if direct.exists() {
                return direct;
            }
            let macos = dir.join("..").join("Resources").join("mindmemos");
            if macos.exists() {
                return macos;
            }
        }
    }
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("resources")
        .join("mindmemos")
}

async fn memory_request(
    backend: &MemoryBackend,
    path: &str,
    body: Value,
    timeout: Duration,
) -> Result<Value, String> {
    let response = reqwest::Client::new()
        .post(backend.api_url(path))
        .header("Content-Type", "application/json")
        .header("Authorization", format!("Bearer {}", backend.api_key()))
        .json(&body)
        .timeout(timeout)
        .send()
        .await
        .map_err(|error| {
            if error.is_timeout() {
                "记忆服务请求超时".to_string()
            } else {
                error.to_string()
            }
        })?;
    let status = response.status();
    let value = response
        .json::<Value>()
        .await
        .map_err(|error| error.to_string())?;
    if !status.is_success()
        || !matches!(value.get("code").and_then(Value::as_str), None | Some("ok"))
    {
        return Err(value
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("memory API request failed")
            .to_string());
    }
    Ok(value)
}

/// Mirror already-extracted L1 text into the local vector service.  This is
/// deliberately best-effort: SQLite has already committed the durable memory,
/// so a stopped sidecar must never make conversation ingestion retry or lose
/// data. MindMemOS derives deterministic IDs from content, making repeated
/// batches safe when a sidecar restarts midway through a sync.
pub fn queue_semantic_l1_index(memories: Vec<String>) {
    let memories = memories
        .into_iter()
        .map(|memory| memory.trim().to_string())
        .filter(|memory| !memory.is_empty())
        .collect::<Vec<_>>();
    if memories.is_empty() || !MemoryBackend::probe(API_PORT) {
        return;
    }
    std::thread::spawn(move || {
        let client = reqwest::blocking::Client::new();
        for batch in memories.chunks(SEMANTIC_INDEX_BATCH_SIZE) {
            let body = serde_json::json!({
                "user_id": SEMANTIC_MEMORY_OWNER,
                "agent_id": SEMANTIC_MEMORY_OWNER,
                "messages": batch.iter().map(|content| serde_json::json!({ "role": "user", "content": content })).collect::<Vec<_>>(),
                "mode": "sync",
                "metadata": { "source": "agent-manager-l1" }
            });
            let result = client
                .post(format!("http://127.0.0.1:{API_PORT}/v1/memory/add"))
                .header("Content-Type", "application/json")
                .header("Authorization", format!("Bearer {}", semantic_api_key()))
                .json(&body)
                .timeout(Duration::from_secs(30))
                .send()
                .and_then(|response| response.error_for_status());
            if let Err(error) = result {
                eprintln!("[memory-semantic] L1 index batch skipped: {error}");
                break;
            }
        }
    });
}

/// Backfill the optional vector index from the authoritative local L1 ledger.
/// This is deliberately separate from MCP initialization: indexing historical
/// memories must never require preloading them into every Agent's context.
pub fn queue_semantic_l1_reindex() -> Result<usize, String> {
    let memories = crate::telemetry_store::TelemetryStore::new()?
        .local_l1_memory_snapshot()?
        .into_iter()
        .map(|memory| memory.memory)
        .collect::<Vec<_>>();
    let count = memories.len();
    queue_semantic_l1_index(memories);
    Ok(count)
}

/// Query the local vector index from an MCP subprocess.  MCP runs in a
/// separate executable and therefore cannot use the desktop process's Tauri
/// state; it only ever talks to the loopback-only memory sidecar.
pub fn search_semantic_l1_memories(query: &str, limit: u32) -> Result<Vec<SemanticMemory>, String> {
    if !MemoryBackend::probe(API_PORT) {
        return Err("本地语义记忆服务未运行".into());
    }
    let response = reqwest::blocking::Client::new()
        .post(format!("http://127.0.0.1:{API_PORT}/v1/memory/search"))
        .header("Content-Type", "application/json")
        .header("Authorization", format!("Bearer {}", semantic_api_key()))
        .json(&serde_json::json!({
            "user_id": SEMANTIC_MEMORY_OWNER,
            "agent_id": SEMANTIC_MEMORY_OWNER,
            "query": query,
            "top_k": limit.clamp(1, 100),
            "search_strategy": "fast",
            "rerank": true,
        }))
        .timeout(Duration::from_secs(8))
        .send()
        .map_err(|error| {
            if error.is_timeout() {
                "语义检索超时".to_string()
            } else {
                error.to_string()
            }
        })?;
    let status = response.status();
    let value = response
        .json::<Value>()
        .map_err(|error| error.to_string())?;
    if !status.is_success()
        || !matches!(value.get("code").and_then(Value::as_str), None | Some("ok"))
    {
        return Err(value
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("语义检索失败")
            .to_string());
    }
    Ok(value
        .pointer("/data/memories")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|item| {
            let memory = item
                .get("memory")
                .or_else(|| item.get("content"))
                .and_then(Value::as_str)?
                .trim();
            (!memory.is_empty()).then(|| SemanticMemory {
                id: item
                    .get("id")
                    .and_then(Value::as_str)
                    .unwrap_or("semantic")
                    .to_string(),
                memory: memory.to_string(),
                memory_type: item
                    .get("memory_type")
                    .and_then(Value::as_str)
                    .unwrap_or("semantic")
                    .to_string(),
                score: item.get("score").and_then(Value::as_f64),
            })
        })
        .collect())
}

async fn all_memories(backend: &MemoryBackend) -> Result<Vec<SnapshotMemory>, String> {
    let response = memory_request(
        backend,
        "/v1/memory/get",
        serde_json::json!({ "top_k": 1000 }),
        Duration::from_secs(30),
    )
    .await?;
    Ok(response
        .pointer("/data/memories")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|item| {
            Some(SnapshotMemory {
                id: item.get("id")?.as_str()?.to_string(),
                memory: item.get("memory")?.as_str()?.to_string(),
                memory_type: item
                    .get("memory_type")
                    .and_then(Value::as_str)
                    .map(str::to_string),
            })
        })
        .collect())
}

/// Importance is owned by Vibe Assistant's local evidence ledger.  The optional
/// semantic backend contributes its legacy/manual memories when reachable, but
/// it must never prevent L1 conversation memory from being ranked.
async fn importance_memory_inventory(
    backend: &MemoryBackend,
    telemetry: &crate::telemetry_store::TelemetryStore,
) -> Result<Vec<SnapshotMemory>, String> {
    let mut memories = telemetry
        .local_l1_memories(500)?
        .into_iter()
        .map(|memory| SnapshotMemory {
            id: memory.id,
            memory: memory.memory,
            memory_type: Some(memory.memory_type),
        })
        .collect::<Vec<_>>();
    if backend.api_online() {
        match all_memories(backend).await {
            Ok(engine_memories) => memories.extend(engine_memories),
            Err(error) => eprintln!(
                "[memory-importance] semantic memory unavailable, ranked local L1 only: {error}"
            ),
        }
    }
    Ok(memories)
}

fn normalized_content_key(content: &str) -> String {
    content
        .chars()
        .filter(|character| !character.is_whitespace() && !character.is_ascii_punctuation())
        .flat_map(char::to_lowercase)
        .collect()
}

#[cfg(test)]
fn consolidation_terms(content: &str) -> HashSet<String> {
    let normalized = content.to_lowercase();
    let mut terms = HashSet::new();
    let mut latin = String::new();
    let mut han = Vec::new();
    for character in normalized.chars() {
        if character.is_ascii_alphanumeric() || character == '_' || character == '-' {
            latin.push(character);
            continue;
        }
        if latin.len() >= 3 {
            terms.insert(std::mem::take(&mut latin));
        } else {
            latin.clear();
        }
        if ('\u{4e00}'..='\u{9fff}').contains(&character) {
            han.push(character);
        }
    }
    if latin.len() >= 3 {
        terms.insert(latin);
    }
    for pair in han.windows(2) {
        terms.insert(pair.iter().collect());
    }
    terms
}

/// Reduce a full library to high-confidence candidate groups before involving
/// the model. This protects local consolidation from a slow reasoning pass,
/// while intentionally retaining only pairs that look genuinely related.
#[cfg(test)]
fn consolidation_candidates(memories: &[SnapshotMemory]) -> Vec<SnapshotMemory> {
    let terms = memories
        .iter()
        .map(|memory| consolidation_terms(&memory.memory))
        .collect::<Vec<_>>();
    let mut selected = HashSet::new();
    for left in 0..memories.len() {
        for right in left + 1..memories.len() {
            let left_key = normalized_content_key(&memories[left].memory);
            let right_key = normalized_content_key(&memories[right].memory);
            let exact = !left_key.is_empty() && left_key == right_key;
            let shared = terms[left].intersection(&terms[right]).count();
            let smaller = terms[left].len().min(terms[right].len()).max(1);
            let overlap = shared as f64 / smaller as f64;
            if exact || (shared >= 2 && overlap >= 0.55) {
                selected.insert(left);
                selected.insert(right);
            }
        }
    }
    selected
        .into_iter()
        .map(|index| memories[index].clone())
        .collect()
}

fn memory_type_weight(memory_type: Option<&str>) -> i64 {
    match memory_type.unwrap_or_default().to_lowercase().as_str() {
        "preference" | "pref" | "constraint" | "profile" => 28,
        "fact" => 22,
        "decision" | "rule" | "skill" => 26,
        _ => 16,
    }
}

fn importance_score(
    memory: &SnapshotMemory,
    supporting_sessions: i64,
    supporting_agents: i64,
    duplicate_count: i64,
    recall_count: i64,
    pinned: bool,
) -> i64 {
    let reinforcement = (supporting_sessions.saturating_sub(1) * 7).min(28);
    let cross_agent = (supporting_agents.saturating_sub(1) * 8).min(16);
    let recall = (recall_count * 4).min(16);
    let pin = if pinned { 18 } else { 0 };
    (memory_type_weight(memory.memory_type.as_deref()) + reinforcement + cross_agent + recall + pin
        - duplicate_count * 10)
        .clamp(0, 100)
}

#[tauri::command]
pub async fn memory_importance_refresh(
    state: tauri::State<'_, MemoryBackend>,
    telemetry: tauri::State<'_, crate::telemetry_store::TelemetryStore>,
) -> Result<MemoryImportanceSummary, String> {
    let memories = importance_memory_inventory(&state, &telemetry).await?;
    if memories.is_empty() {
        return Ok(MemoryImportanceSummary {
            reviewed: 0,
            high: 0,
            medium: 0,
            low: 0,
            message: "尚无可计算重要性的记忆；完成对话整理后再试".into(),
        });
    }
    let candidates = memories
        .iter()
        .map(|memory| MemoryImportanceCandidate {
            memory_id: memory.id.clone(),
            content: memory.memory.clone(),
        })
        .collect::<Vec<_>>();
    let evidence = telemetry.importance_evidence(&candidates)?;
    let existing = telemetry
        .importance_records(
            &memories
                .iter()
                .map(|memory| memory.id.clone())
                .collect::<Vec<_>>(),
        )?
        .into_iter()
        .map(|record| (record.memory_id.clone(), record))
        .collect::<HashMap<_, _>>();
    let mut content_counts = HashMap::<String, i64>::new();
    for memory in &memories {
        *content_counts
            .entry(normalized_content_key(&memory.memory))
            .or_default() += 1;
    }
    let now = chrono::Utc::now().to_rfc3339();
    let records = memories
        .iter()
        .map(|memory| {
            let support = evidence.get(&memory.id).cloned().unwrap_or_default();
            let current = existing.get(&memory.id);
            let duplicate_count = content_counts
                .get(&normalized_content_key(&memory.memory))
                .copied()
                .unwrap_or(1)
                .saturating_sub(1);
            let recall_count = current.map(|item| item.recall_count).unwrap_or(0);
            let pinned = current.map(|item| item.pinned).unwrap_or(false);
            MemoryImportance {
                memory_id: memory.id.clone(),
                score: importance_score(
                    memory,
                    support.supporting_sessions,
                    support.supporting_agents,
                    duplicate_count,
                    recall_count,
                    pinned,
                ),
                supporting_sessions: support.supporting_sessions,
                supporting_agents: support.supporting_agents,
                duplicate_count,
                recall_count,
                pinned,
                updated_at: now.clone(),
            }
        })
        .collect::<Vec<_>>();
    let high = records.iter().filter(|record| record.score >= 70).count();
    let medium = records
        .iter()
        .filter(|record| (40..70).contains(&record.score))
        .count();
    let low = records.len().saturating_sub(high + medium);
    telemetry.save_importance_records(&records)?;
    let source_note = if state.api_online() {
        "本地 L1 与语义库"
    } else {
        "本地 L1（语义引擎离线）"
    };
    Ok(MemoryImportanceSummary {
        reviewed: records.len(),
        high,
        medium,
        low,
        message: format!(
            "已为 {} 重新计算重要性：类型、跨会话强化、跨 Agent 使用、检索反馈与重复项",
            source_note
        ),
    })
}

#[tauri::command]
pub async fn memory_importance_list(
    telemetry: tauri::State<'_, crate::telemetry_store::TelemetryStore>,
    memory_ids: Vec<String>,
) -> Result<Vec<MemoryImportance>, String> {
    telemetry.importance_records(&memory_ids)
}

#[tauri::command]
pub fn memory_importance_set_pinned(
    telemetry: tauri::State<'_, crate::telemetry_store::TelemetryStore>,
    memory_id: String,
    pinned: bool,
) -> Result<(), String> {
    telemetry.set_memory_pinned(&memory_id, pinned)
}

fn write_consolidation_snapshot(
    backend: &MemoryBackend,
    memories: Vec<SnapshotMemory>,
) -> Result<ConsolidationSnapshot, String> {
    let snapshot = ConsolidationSnapshot {
        id: uuid::Uuid::new_v4().to_string(),
        created_at: chrono::Utc::now().to_rfc3339(),
        memories,
        local_memories: Vec::new(),
    };
    let dir = backend.consolidation_snapshot_dir();
    std::fs::create_dir_all(&dir).map_err(|error| error.to_string())?;
    let path = dir.join(format!("{}.json", snapshot.id));
    let body = serde_json::to_vec_pretty(&snapshot).map_err(|error| error.to_string())?;
    std::fs::write(path, body).map_err(|error| error.to_string())?;
    Ok(snapshot)
}

fn write_local_consolidation_snapshot(
    backend: &MemoryBackend,
    memories: Vec<crate::telemetry_store::LocalMemorySnapshot>,
) -> Result<ConsolidationSnapshot, String> {
    let snapshot = ConsolidationSnapshot {
        id: uuid::Uuid::new_v4().to_string(),
        created_at: chrono::Utc::now().to_rfc3339(),
        memories: Vec::new(),
        local_memories: memories,
    };
    let dir = backend.consolidation_snapshot_dir();
    std::fs::create_dir_all(&dir).map_err(|error| error.to_string())?;
    let path = dir.join(format!("{}.json", snapshot.id));
    let body = serde_json::to_vec_pretty(&snapshot).map_err(|error| error.to_string())?;
    std::fs::write(path, body).map_err(|error| error.to_string())?;
    Ok(snapshot)
}

fn parse_consolidation_plan(text: &str) -> Result<ConsolidationPlan, String> {
    let fenced = text
        .trim()
        .strip_prefix("```json")
        .or_else(|| text.trim().strip_prefix("```"))
        .unwrap_or(text.trim())
        .trim()
        .trim_end_matches("```")
        .trim();
    let object = fenced
        .find('{')
        .zip(fenced.rfind('}'))
        .filter(|(start, end)| start <= end)
        .map(|(start, end)| &fenced[start..=end]);
    let array = fenced
        .find('[')
        .zip(fenced.rfind(']'))
        .filter(|(start, end)| start <= end)
        .map(|(start, end)| &fenced[start..=end]);
    // Try the untouched reply first.  For a bare action array, extracting the
    // first `{...}` would otherwise turn the single action into an empty plan.
    for candidate in [Some(fenced), object, array].into_iter().flatten() {
        if let Ok(ConsolidationPlanResponse::Wrapped(plan)) =
            serde_json::from_str::<ConsolidationPlanResponse>(candidate)
        {
            return Ok(plan);
        }
        if let Ok(ConsolidationPlanResponse::Bare(actions)) =
            serde_json::from_str::<ConsolidationPlanResponse>(candidate)
        {
            return Ok(ConsolidationPlan { actions });
        }
    }
    Err("记忆整理模型未返回有效的 JSON 合并计划".to_string())
}

async fn create_candidate_batch_plan(
    candidates: &[SnapshotMemory],
    library_size: usize,
) -> Result<ConsolidationPlan, String> {
    let provider = llm::memory_extraction_provider()?;
    if candidates.is_empty() {
        return Ok(ConsolidationPlan {
            actions: Vec::new(),
        });
    }
    let library = candidates
        .iter()
        .map(|memory| format!("[{}]\n{}", memory.id, memory.memory))
        .collect::<Vec<_>>()
        .join("\n\n---\n\n");
    let messages = vec![
        serde_json::json!({"role": "system", "content": "你是 Vibe Assistant 的记忆去重裁决器。输入已经是本地筛出的疑似重复候选；只在两条或多条记忆表达同一事实、明显重复或彼此冲突且能安全合并时生成动作。不要合并不同时间的经历、不同任务、不同文件修改、不同偏好或仅主题相近的内容。每个动作保留一个 keep_id，把其余重复 ID 放入 remove_ids，并用 content 给出保留后的完整、准确、简洁记忆。content 必须继承被合并记忆的主要语言；禁止编造信息。若无安全合并项，返回空 actions。直接输出严格 JSON，不要解释：{\"actions\":[{\"keep_id\":\"已有 ID\",\"remove_ids\":[\"已有 ID\"],\"content\":\"合并后记忆\",\"reason\":\"简短理由\"}]}。"}),
        serde_json::json!({"role": "user", "content": format!("请裁决以下 {} 条由本地 BGE-small 语义检索选出的疑似重复候选（来自全库 {} 条记忆）：\n\n{}", candidates.len(), library_size, library)}),
    ];
    let response = llm::complete_text_with_limit(&provider, &messages, Some(2048)).await?;
    match parse_consolidation_plan(&response) {
        Ok(plan) => Ok(plan),
        Err(_) => {
            let repair = vec![
                serde_json::json!({"role": "system", "content": "你是 JSON 格式修复器。仅输出可解析 JSON，不要 Markdown、解释或新内容。输出格式必须是 {\"actions\":[{\"keep_id\":\"...\",\"remove_ids\":[\"...\"],\"content\":\"...\",\"reason\":\"...\"}]}；如果原回复没有安全合并动作，输出 {\"actions\":[]}。"}),
                serde_json::json!({"role": "user", "content": format!("将下面的整理模型回复转换为严格 JSON。不要改变任何 ID 或补充事实：\n\n{}", response)}),
            ];
            parse_consolidation_plan(
                &llm::complete_text_with_limit(&provider, &repair, Some(2048)).await?,
            )
        }
    }
}

/// A last-resort MiniMax-friendly judge.  It has exactly two memories, asks
/// for a tiny yes/no JSON response, and consequently keeps hidden reasoning
/// from exhausting the provider's visible-completion ceiling.
async fn create_compact_pair_plan(
    pair: &[SnapshotMemory],
    library_size: usize,
) -> Result<ConsolidationPlan, String> {
    if pair.len() != 2 {
        return Ok(ConsolidationPlan {
            actions: Vec::new(),
        });
    }
    let provider = llm::memory_extraction_provider()?;
    let left = &pair[0];
    let right = &pair[1];
    let messages = vec![
        serde_json::json!({"role": "system", "content": "判断两条记忆是否完全重复或直接冲突且能安全合并。仅输出 JSON：重复/冲突时 {\"actions\":[{\"keep_id\":\"ID\",\"remove_ids\":[\"ID\"],\"content\":\"合并记忆\",\"reason\":\"重复\"}]}；否则 {\"actions\":[]}。不要解释。"}),
        serde_json::json!({"role": "user", "content": format!("全库共 {library_size} 条。\n[{}]\n{}\n\n[{}]\n{}", left.id, left.memory, right.id, right.memory)}),
    ];
    let response = llm::complete_text_with_limit(&provider, &messages, Some(2048)).await?;
    parse_consolidation_plan(&response)
}

#[tauri::command]
pub async fn memory_consolidate(
    state: tauri::State<'_, MemoryBackend>,
    telemetry: tauri::State<'_, crate::telemetry_store::TelemetryStore>,
    app: tauri::AppHandle,
    candidate_batches: Vec<Vec<ConsolidationCandidate>>,
) -> Result<ConsolidationResult, String> {
    // L1 is Vibe Assistant's source of truth.  Consolidation must therefore
    // remain available when the optional MindMemOS sidecar is stopped.
    let local_before = telemetry.local_l1_memory_snapshot()?;
    let before = local_before
        .iter()
        .map(|memory| SnapshotMemory {
            id: memory.id.clone(),
            memory: memory.memory.clone(),
            memory_type: Some(memory.memory_type.clone()),
        })
        .collect::<Vec<_>>();
    if before.len() < 2 {
        return Ok(ConsolidationResult {
            snapshot_id: String::new(),
            before_count: before.len(),
            after_count: before.len(),
            scopes: before.len() as i64,
            clusters: 0,
            actions: 0,
            message: "已审阅全部记忆；少于两条，无需整理".into(),
        });
    }
    if candidate_batches.is_empty() {
        return Ok(ConsolidationResult {
            snapshot_id: String::new(), before_count: before.len(), after_count: before.len(),
            scopes: before.len() as i64, clusters: 0, actions: 0,
            message: "本地 BGE-small 已完成全库语义筛查，未找到达到安全阈值的相近候选；未调用整理模型，也未修改记忆".into(),
        });
    }
    // Reasoning models plus the JSON-repair fallback need far more than the
    // old 18s per adjudication call; a single slow provider reply used to
    // skip the whole batch.  Give each call a generous but still bounded budget.
    const CONSOLIDATION_CALL_LIMIT: Duration = Duration::from_secs(90);
    // The global budget scales with the number of candidate batches so a full
    // run is no longer cut short by the old fixed 75s ceiling, yet stays
    // bounded so a stuck provider can never hang consolidation indefinitely.
    let total_batches = candidate_batches.len();
    let total_budget =
        Duration::from_secs(240u64.max(total_batches as u64 * 150)).min(Duration::from_secs(1200));
    let deadline = Instant::now() + total_budget;
    // 用户自定义记忆只有用户能删改：不进入巩固的已知集合，任何合并
    // 计划都无法把它们当作保留项改写或被合并项删除（前端候选已按
    // local-l1: 前缀过滤，这里是后端兜底）。
    let known_ids = local_before
        .iter()
        .filter(|memory| !memory.user_defined)
        .map(|memory| memory.id.as_str())
        .collect::<std::collections::HashSet<_>>();
    let library_by_id = before
        .iter()
        .map(|memory| (memory.id.as_str(), memory))
        .collect::<HashMap<_, _>>();
    let mut plan_actions = Vec::new();
    let mut skipped_length_pairs = 0usize;
    let mut skipped_timeout_batches = 0usize;
    for (batch_index, batch) in candidate_batches.into_iter().enumerate() {
        if Instant::now() >= deadline {
            skipped_timeout_batches += total_batches.saturating_sub(batch_index);
            break;
        }
        let _ = app.emit("memory-consolidation-progress", serde_json::json!({
            "detail": format!("正在用整理模型裁决第 {}/{} 个 BGE 语义候选批", batch_index + 1, total_batches),
        }));
        let mut ids = HashSet::new();
        let candidates = batch
            .into_iter()
            .filter_map(|candidate| {
                // Never trust content or metadata supplied by the WebView. The UI
                // merely nominates IDs; SQLite remains the source of truth.
                if !ids.insert(candidate.id.clone()) {
                    return None;
                }
                library_by_id
                    .get(candidate.id.as_str())
                    .map(|memory| (*memory).clone())
            })
            .collect::<Vec<_>>();
        if candidates.len() < 2 {
            continue;
        }
        let batch_result = tokio::time::timeout(
            CONSOLIDATION_CALL_LIMIT,
            create_candidate_batch_plan(&candidates, before.len()),
        )
        .await;
        match batch_result {
            Err(_) => {
                skipped_timeout_batches += 1;
                continue;
            }
            Ok(result) => match result {
                Ok(plan) => plan_actions.extend(plan.actions),
                Err(error) if error == "记忆整理模型未返回有效的 JSON 合并计划" => {
                    continue
                }
                Err(error) if error.contains("结束原因：length") => {
                    // The initial semantic group was still too demanding for the
                    // provider. Retry independently in pairs; one failed pair is
                    // never allowed to cancel unrelated safe consolidation work.
                    for pair in candidates.chunks(2) {
                        if Instant::now() >= deadline {
                            skipped_timeout_batches += 1;
                            break;
                        }
                        match tokio::time::timeout(
                            CONSOLIDATION_CALL_LIMIT,
                            create_compact_pair_plan(pair, before.len()),
                        )
                        .await
                        {
                            Err(_) => skipped_timeout_batches += 1,
                            Ok(result) => match result {
                                Ok(plan) => plan_actions.extend(plan.actions),
                                Err(pair_error) if pair_error.contains("结束原因：length") => {
                                    skipped_length_pairs += 1
                                }
                                Err(pair_error)
                                    if pair_error == "记忆整理模型未返回有效的 JSON 合并计划" =>
                                {
                                    continue
                                }
                                Err(pair_error) => return Err(pair_error),
                            },
                        }
                    }
                }
                Err(error) => return Err(error),
            },
        }
    }
    let plan = ConsolidationPlan {
        actions: plan_actions,
    };
    let planned_keep_ids = plan
        .actions
        .iter()
        .map(|action| action.keep_id.clone())
        .filter(|id| known_ids.contains(id.as_str()))
        .collect::<std::collections::HashSet<_>>();
    let mut used_keep_ids = std::collections::HashSet::new();
    let mut removed_ids = std::collections::HashSet::new();
    let mut valid_actions = Vec::new();
    for action in plan.actions {
        if !known_ids.contains(action.keep_id.as_str())
            || !used_keep_ids.insert(action.keep_id.clone())
        {
            continue;
        }
        let removes = action
            .remove_ids
            .into_iter()
            .filter(|id| {
                id != &action.keep_id
                    && known_ids.contains(id.as_str())
                    && !planned_keep_ids.contains(id)
                    && removed_ids.insert(id.clone())
            })
            .collect::<Vec<_>>();
        if !removes.is_empty() && !action.content.trim().is_empty() {
            valid_actions.push((
                action.keep_id,
                removes,
                action.content.trim().to_string(),
                action.reason,
            ));
        }
    }
    if valid_actions.is_empty() {
        let message = if skipped_length_pairs > 0 || skipped_timeout_batches > 0 {
            format!("已完成本轮 BGE 语义巩固；MiniMax 思考耗尽 {skipped_length_pairs} 个最小裁决、超时跳过 {skipped_timeout_batches} 批，未修改记忆")
        } else {
            "已完成 BGE 语义筛查与模型裁决，未发现可安全合并的重复或冲突项".into()
        };
        return Ok(ConsolidationResult {
            snapshot_id: String::new(),
            before_count: before.len(),
            after_count: before.len(),
            scopes: before.len() as i64,
            clusters: 0,
            actions: 0,
            message,
        });
    }
    let snapshot = write_local_consolidation_snapshot(&state, local_before)?;
    let mut applied_groups = 0i64;
    let mut removed_count = 0i64;
    let mut errors = Vec::new();
    for (keep_id, remove_ids, content, _reason) in valid_actions {
        if let Err(error) = telemetry.update_local_l1_memory(&keep_id, &content) {
            errors.push(error);
            continue;
        }
        let mut group_applied = true;
        for remove_id in remove_ids {
            match telemetry.delete_local_l1_memory(&remove_id) {
                Ok(_) => removed_count += 1,
                Err(error) => {
                    errors.push(error);
                    group_applied = false;
                }
            }
        }
        if group_applied {
            applied_groups += 1;
        }
    }
    let after_count = telemetry.local_l1_memory_snapshot()?.len();
    let message = if errors.is_empty() {
        if skipped_length_pairs > 0 || skipped_timeout_batches > 0 {
            format!("已完成 BGE 语义巩固，合并 {applied_groups} 组记忆；MiniMax 思考耗尽 {skipped_length_pairs} 个最小裁决、超时跳过 {skipped_timeout_batches} 批")
        } else {
            format!("已完成 BGE 语义巩固，合并 {applied_groups} 组记忆")
        }
    } else {
        format!(
            "全库整理已部分完成（{} 项请求失败）；可使用恢复点回退",
            errors.len()
        )
    };
    Ok(ConsolidationResult {
        snapshot_id: snapshot.id,
        before_count: before.len(),
        after_count,
        scopes: before.len() as i64,
        clusters: applied_groups,
        actions: removed_count,
        message,
    })
}

#[tauri::command]
pub async fn memory_consolidation_restore(
    state: tauri::State<'_, MemoryBackend>,
    telemetry: tauri::State<'_, crate::telemetry_store::TelemetryStore>,
    snapshot_id: String,
) -> Result<ConsolidationResult, String> {
    if !snapshot_id
        .chars()
        .all(|character| character.is_ascii_alphanumeric() || character == '-')
    {
        return Err("无效的恢复点标识".into());
    }
    let path = state
        .consolidation_snapshot_dir()
        .join(format!("{snapshot_id}.json"));
    let snapshot: ConsolidationSnapshot =
        serde_json::from_slice(&std::fs::read(path).map_err(|error| error.to_string())?)
            .map_err(|error| error.to_string())?;
    if !snapshot.local_memories.is_empty() {
        let current = telemetry.local_l1_memory_snapshot()?;
        let _rollback_point = write_local_consolidation_snapshot(&state, current)?;
        telemetry.restore_local_l1_memory_snapshot(&snapshot.local_memories)?;
        return Ok(ConsolidationResult {
            snapshot_id: String::new(),
            before_count: _rollback_point.local_memories.len(),
            after_count: snapshot.local_memories.len(),
            scopes: 0,
            clusters: 0,
            actions: snapshot.local_memories.len() as i64,
            message: format!(
                "已恢复巩固前的 {} 条本地记忆；已同时保存恢复前状态",
                snapshot.local_memories.len()
            ),
        });
    }
    let current = all_memories(&state).await?;
    let before_count = current.len();
    let _rollback_point = write_consolidation_snapshot(&state, current.clone())?;
    for memory in current {
        memory_request(
            &state,
            "/v1/memory/delete",
            serde_json::json!({ "memory_id": memory.id }),
            Duration::from_secs(30),
        )
        .await?;
    }
    for memory in &snapshot.memories {
        memory_request(
            &state,
            "/v1/memory/add",
            serde_json::json!({
                "user_id": "agent-manager",
                "agent_id": "agent-manager",
                "messages": [{ "role": "user", "content": memory.memory }],
                "mode": "sync",
            }),
            Duration::from_secs(60),
        )
        .await?;
    }
    let after_count = all_memories(&state).await?.len();
    Ok(ConsolidationResult {
        // A second checkpoint is retained on disk before restoring, but it is
        // intentionally not offered as a second in-app action: the visible
        // action remains a one-click recovery of the original consolidation.
        snapshot_id: String::new(),
        before_count,
        after_count,
        scopes: 0,
        clusters: 0,
        actions: snapshot.memories.len() as i64,
        message: format!(
            "已恢复巩固前的 {} 条记忆；已同时保存恢复前状态",
            snapshot.memories.len()
        ),
    })
}

#[tauri::command]
pub fn memory_backend_status(state: tauri::State<'_, MemoryBackend>) -> EngineStatus {
    state.status()
}

#[tauri::command]
pub async fn memory_backend_start(
    state: tauri::State<'_, MemoryBackend>,
) -> Result<EngineStatus, String> {
    state.ensure_started();
    // 等待组件就绪（最多 ~8s）
    for _ in 0..16 {
        let st = state.status();
        if st.online {
            break;
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    let status = state.status();
    if status.online {
        match queue_semantic_l1_reindex() {
            Ok(count) => {
                eprintln!("[memory-semantic] queued {count} local L1 memories for index backfill")
            }
            Err(error) => eprintln!("[memory-semantic] local L1 index backfill skipped: {error}"),
        }
    }
    Ok(status)
}

#[tauri::command]
pub fn memory_backend_stop(state: tauri::State<'_, MemoryBackend>) {
    state.stop();
}

/// Keep the memory-engine credential in the native process.  The frontend
/// never receives this key, which is especially important because it can load
/// Agent webviews and intentionally has broad desktop capabilities.
#[tauri::command]
pub async fn memory_backend_request(
    state: tauri::State<'_, MemoryBackend>,
    path: String,
    body: Value,
) -> Result<Value, String> {
    if !path.starts_with("/v1/memory/") {
        return Err("unsupported memory API path".into());
    }
    let response = reqwest::Client::new()
        .post(state.api_url(&path))
        .header("Content-Type", "application/json")
        .header("Authorization", format!("Bearer {}", state.api_key()))
        .json(&body)
        .send()
        .await
        .map_err(|e| e.to_string())?;
    let status = response.status();
    let value = response.json::<Value>().await.map_err(|e| e.to_string())?;
    if !status.is_success() {
        return Err(value
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("memory API request failed")
            .to_string());
    }
    if path == "/v1/memory/search" {
        let recalled_ids = value
            .pointer("/data/memories")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|memory| memory.get("id").and_then(Value::as_str).map(str::to_string))
            .collect::<Vec<_>>();
        if let Some(telemetry) = crate::telemetry_store::shared_store() {
            let _ = telemetry.record_memory_recall(&recalled_ids);
        }
    }
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::{
        consolidation_candidates, importance_score, parse_consolidation_plan, SnapshotMemory,
    };

    #[test]
    fn consolidation_plan_accepts_a_fenced_json_reply() {
        let plan = parse_consolidation_plan("说明如下：\n```json\n{\"actions\":[{\"keep_id\":\"keep\",\"remove_ids\":[\"duplicate\"],\"content\":\"合并记忆\"}]}\n```")
            .expect("plan should parse");
        assert_eq!(plan.actions.len(), 1);
        assert_eq!(plan.actions[0].keep_id, "keep");
    }

    #[test]
    fn consolidation_plan_accepts_a_bare_action_array() {
        let plan = parse_consolidation_plan(
            "[{\"keep_id\":\"keep\",\"remove_ids\":[\"duplicate\"],\"content\":\"合并记忆\"}]",
        )
        .expect("bare action list should parse");
        assert_eq!(plan.actions.len(), 1);
        assert_eq!(plan.actions[0].remove_ids, ["duplicate"]);
    }

    #[test]
    fn consolidation_candidate_filter_keeps_only_related_pairs() {
        let memories = vec![
            SnapshotMemory {
                id: "a".into(),
                memory: "用户偏好使用 pnpm 管理前端依赖".into(),
                memory_type: None,
            },
            SnapshotMemory {
                id: "b".into(),
                memory: "前端依赖统一用 pnpm 管理".into(),
                memory_type: None,
            },
            SnapshotMemory {
                id: "c".into(),
                memory: "本周需要修复 Claude Hook 监听".into(),
                memory_type: None,
            },
        ];
        let selected = consolidation_candidates(&memories);
        assert_eq!(selected.len(), 2);
        assert!(selected.iter().any(|item| item.id == "a"));
        assert!(selected.iter().any(|item| item.id == "b"));
    }

    #[test]
    fn bundled_sidecar_root_is_private_to_agent_manager() {
        let root = super::bundled_sidecar_root();
        assert!(root.ends_with("resources\\mindmemos") || root.ends_with("resources/mindmemos"));
    }

    #[test]
    fn local_conversation_memory_receives_an_importance_score() {
        let memory = SnapshotMemory {
            id: "local-l1:one".into(),
            memory: "完成会话摘要".into(),
            memory_type: Some("conversation".into()),
        };
        assert!(importance_score(&memory, 1, 1, 0, 0, false) > 0);
    }
}
