//! 自动沉淀管道：接收 Agent hook 回调，自动提取记忆。
//!
//! 数据流：Agent（Claude Code / Codex 等）hook 回调 → 本模块按会话聚合
//! 对话与工具轨迹 → 会话结束（Stop）时批量调用记忆引擎：
//!   - 对话片段 → 本地 L1 记忆账本（LLM 抽取后直接、可恢复地写入）
//!   - 原始 Hook 事件 → 本地遥测账本（供 Token 统计与审计）
//!
//! 全部异步执行，不阻塞 hook 回调；带节流（按会话聚合，避免逐条调用 LLM）。

use serde_json::{json, Value};
use sha2::Digest;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use crate::llm;
use crate::memory_backend::MemoryBackend;
use crate::thinking::strip_thinking_blocks;

/// All harnesses serve the same local user.  Memories must stay portable when
/// a conversation continues from Codex to Claude or another connected agent.
const GLOBAL_MEMORY_OWNER: &str = "agent-manager";
const ORGANIZE_BATCH_LIMIT: u32 = 10;
const ORGANIZE_ONE_CONVERSATION_TIMEOUT: Duration = Duration::from_secs(150);
const IMPORT_MAX_FILES: usize = 100;
const IMPORT_MAX_FILE_CHARS: usize = 48_000;
const IMPORT_MAX_TOTAL_CHARS: usize = 240_000;
const IMPORT_CHUNK_CHARS: usize = 18_000;
const IMPORT_MAX_CHUNKS: usize = 14;
const NATIVE_SCAN_MAX_FILES_PER_SOURCE: usize = 100;
const NATIVE_SCAN_MAX_ENTRIES: usize = 20_000;
const NATIVE_SESSION_MAX_MESSAGES: usize = 200;
const NATIVE_SESSION_MAX_CHARS: usize = 120_000;
// This caps only the final profile that is persisted. It must comfortably fit
// a complete compact Markdown profile; reasoning payloads are removed before
// this check and are not part of the persisted document.
//
// L2/L3 预算统一为 10000 cl100k token（与注入侧
// memory_mcp::MEMORY_LAYER_INJECTION_TOKENS 同源）。字符上限按 3 字符/token
// 放缩，保证 10000 token 的中英混排内容不会被字符闸误杀。
const MEMORY_LAYER_TOKEN_BUDGET: usize = 10_000;
const MEMORY_LAYER_DOC_MAX_CHARS: usize = MEMORY_LAYER_TOKEN_BUDGET * 3;
const L3_PROFILE_MAX_CHARS: usize = MEMORY_LAYER_DOC_MAX_CHARS;
const L3_PROFILE_TARGET_CHARS: usize = MEMORY_LAYER_DOC_MAX_CHARS;
const L3_L1_EVIDENCE_LIMIT: usize = 80;

/// 全局单例（供 agent_http 的 route 无 State 上下文使用）。
static INGEST: OnceLock<IngestStore> = OnceLock::new();

// Manual L1 extraction is shared by the Memory Center and Pending Memories.
// Keep its cancellation flag independent of the UI so navigation cannot orphan a run.
static MANUAL_EXTRACTION: Mutex<Option<tokio::sync::watch::Sender<bool>>> = Mutex::new(None);

struct ManualExtractionGuard(tokio::sync::watch::Receiver<bool>);

impl ManualExtractionGuard {
    fn start() -> Result<Self, String> {
        let mut active = MANUAL_EXTRACTION.lock().unwrap();
        if active.is_some() {
            return Err("已有记忆提取任务正在运行".into());
        }
        let (sender, receiver) = tokio::sync::watch::channel(false);
        *active = Some(sender);
        Ok(Self(receiver))
    }

    fn cancelled(&self) -> bool {
        *self.0.borrow()
    }

    async fn wait_for_cancel(&self) {
        let mut receiver = self.0.clone();
        loop {
            if *receiver.borrow_and_update() || receiver.changed().await.is_err() {
                return;
            }
        }
    }

    async fn extract(&self, text: &str) -> Option<Result<Result<Vec<crate::telemetry_store::L1MemoryCandidate>, String>, tokio::time::error::Elapsed>> {
        tokio::select! {
            biased;
            _ = self.wait_for_cancel() => None,
            result = tokio::time::timeout(ORGANIZE_ONE_CONVERSATION_TIMEOUT, extract_l1_conversation(text)) => Some(result),
        }
    }

    async fn pause_before_retry(&self) -> bool {
        tokio::select! {
            biased;
            _ = self.wait_for_cancel() => false,
            _ = tokio::time::sleep(Duration::from_millis(500)) => true,
        }
    }
}

impl Drop for ManualExtractionGuard {
    fn drop(&mut self) {
        *MANUAL_EXTRACTION.lock().unwrap() = None;
    }
}

pub fn init_ingest(store: IngestStore) {
    let _ = INGEST.set(store);
}

pub fn ingest_store() -> Option<&'static IngestStore> {
    INGEST.get()
}

/// 单个会话的聚合缓冲
#[derive(Clone, Debug)]
struct SessionBuffer {
    agent_id: String,
    /// Hook 来源标识（如 claude / codex），用于在整理日志中显示所属 Agent。
    source: String,
    messages: Vec<(String, String)>, // (role, content)
    last_active: Instant,
}

#[derive(Default)]
struct IngestInner {
    sessions: HashMap<String, SessionBuffer>,
    /// 最近提取记录（前端展示）
    recent: Vec<IngestLog>,
}

#[derive(Clone, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub struct IngestLog {
    pub at: String,
    pub agent_id: String,
    pub kind: String,  // memory | skill
    pub state: String, // working | stored | retrying
    pub detail: String,
}

#[derive(Clone)]
pub struct IngestStore {
    inner: Arc<Mutex<IngestInner>>,
    /// 会话静默超时（秒），超时自动冲刷
    idle_timeout_secs: u64,
    enabled: Arc<Mutex<bool>>,
}

impl IngestStore {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(IngestInner::default())),
            idle_timeout_secs: 180,
            enabled: Arc::new(Mutex::new(true)),
        }
    }

    pub fn set_enabled(&self, on: bool) {
        *self.enabled.lock().unwrap() = on;
    }

    pub fn is_enabled(&self) -> bool {
        *self.enabled.lock().unwrap()
    }

    /// 处理一条 hook 回调（不阻塞；耗时的记忆提取在后台执行）。
    pub fn handle_hook(
        &self,
        backend: Arc<MemoryBackend>,
        body: &str,
        source: &str,
    ) -> Result<Value, String> {
        let parsed: Value =
            serde_json::from_str(body).map_err(|e| format!("invalid hook JSON: {e}"))?;
        if let Some(store) = crate::telemetry_store::shared_store() {
            store.record_hook(source, &parsed)?;
        }
        let event = parsed
            .get("hook_event_name")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let session_id = parsed
            .get("session_id")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let agent_id = GLOBAL_MEMORY_OWNER.to_string();

        if session_id.is_empty() {
            return Ok(
                json!({"status": "ok", "note": "no session_id, retained in telemetry only"}),
            );
        }

        match event {
            // 用户提交提示词 → 记录对话
            "UserPromptSubmit" => {
                let prompt = parsed
                    .get("prompt")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                self.push_message(&session_id, &agent_id, source, "user", &prompt);
            }
            // 工具轨迹属于同一段对话上下文。保留一个有大小上限的摘要，
            // 但绝不在此处提取，统一等待会话 Stop。
            "PostToolUse" => {
                self.touch_session(&session_id, &agent_id, source);
                let tool_name = parsed
                    .get("tool_name")
                    .and_then(Value::as_str)
                    .unwrap_or("tool");
                let input = value_excerpt(parsed.get("tool_input"), 2_000);
                let output = value_excerpt(
                    parsed
                        .get("tool_response")
                        .or_else(|| parsed.get("tool_output")),
                    4_000,
                );
                let detail = format!("工具 {tool_name}\n输入：{input}\n结果：{output}");
                self.push_message(&session_id, &agent_id, source, "tool", &detail);
            }
            // 会话结束 → 冲刷：批量提取记忆。
            "Stop" => {
                // Agent hooks provide operational events, while their local
                // transcripts hold the actual user/assistant dialogue.  Read
                // the one completed turn so tool traffic never becomes memory.
                let transcript_replaced = read_hook_transcript(source, &parsed);
                let mut conversation_state = "unavailable";
                let mut conversation_messages = Vec::new();
                if let Some(messages) = transcript_replaced {
                    conversation_state = "full";
                    conversation_messages = messages.clone();
                    self.replace_session_messages(&session_id, &agent_id, source, messages);
                } else if let Some(reply) = parsed
                    .get("last_assistant_message")
                    .or_else(|| parsed.get("assistant_message"))
                    .or_else(|| parsed.get("final_response"))
                    .and_then(Value::as_str)
                {
                    // Adapters without a readable transcript can still supply
                    // a final assistant response as a useful fallback.
                    self.push_message(&session_id, &agent_id, source, "assistant", reply);
                    conversation_state = "partial";
                    conversation_messages.push(("assistant".to_string(), reply.to_string()));
                }
                if let Some(store) = crate::telemetry_store::shared_store() {
                    if let Err(error) = store.record_conversation(
                        source,
                        &parsed,
                        conversation_state,
                        &conversation_messages,
                    ) {
                        eprintln!("[memory-ingest] failed to annotate hook receipt: {error}");
                    }
                }
                self.flush_session(backend, &session_id);
            }
            _ => {}
        }

        Ok(json!({"status": "ok"}))
    }

    fn push_message(
        &self,
        session_id: &str,
        agent_id: &str,
        source: &str,
        role: &str,
        content: &str,
    ) {
        let content = strip_memory_thinking(role, content);
        if content.is_empty() {
            return;
        }
        {
            let mut inner = self.inner.lock().unwrap();
            let buf = inner
                .sessions
                .entry(session_id.to_string())
                .or_insert_with(|| SessionBuffer {
                    agent_id: agent_id.to_string(),
                    source: source.to_string(),
                    messages: Vec::new(),
                    last_active: Instant::now(),
                });
            buf.messages
                .push((role.to_string(), truncate_text(&content, 12_000)));
            buf.last_active = Instant::now();
        }
    }

    fn touch_session(&self, session_id: &str, agent_id: &str, source: &str) {
        let mut inner = self.inner.lock().unwrap();
        let buf = inner
            .sessions
            .entry(session_id.to_string())
            .or_insert_with(|| SessionBuffer {
                agent_id: agent_id.to_string(),
                source: source.to_string(),
                messages: Vec::new(),
                last_active: Instant::now(),
            });
        buf.last_active = Instant::now();
    }

    fn replace_session_messages(
        &self,
        session_id: &str,
        agent_id: &str,
        source: &str,
        messages: Vec<(String, String)>,
    ) {
        let messages = messages
            .into_iter()
            .map(|(role, content)| {
                let content = strip_memory_thinking(&role, &content);
                (role, content)
            })
            .filter(|(_, content)| !content.is_empty())
            .collect();
        let mut inner = self.inner.lock().unwrap();
        inner.sessions.insert(
            session_id.to_string(),
            SessionBuffer {
                agent_id: agent_id.to_string(),
                source: source.to_string(),
                messages,
                last_active: Instant::now(),
            },
        );
    }

    /// 冲刷一个会话：取出缓冲 → 后台提取记忆 + 沉淀 skill。
    pub fn flush_session(&self, backend: Arc<MemoryBackend>, session_id: &str) {
        if !self.is_enabled() {
            return;
        }
        let snapshot = {
            let mut inner = self.inner.lock().unwrap();
            inner.sessions.remove(session_id)
        };
        let Some(buf) = snapshot else { return };
        if buf.messages.is_empty() {
            return;
        }

        let store = self.clone();
        let session_id = session_id.to_string();
        tauri::async_runtime::spawn(async move {
            store.ingest_memory(&backend, &session_id, &buf).await;
        });
    }

    /// One completed hook session is one extraction unit.  The configured LLM
    /// sees the full buffered transcript only after Stop (or the idle timeout).
    async fn ingest_memory(&self, _backend: &MemoryBackend, session_id: &str, buf: &SessionBuffer) {
        if buf.messages.is_empty() {
            return;
        }
        let provider = match llm::memory_extraction_provider() {
            Ok(provider) => provider,
            Err(error) => {
                self.restore_session_for(session_id, buf.clone());
                self.log(IngestLog {
                    at: now_str(),
                    agent_id: buf.agent_id.clone(),
                    kind: "memory".into(),
                    state: "retrying".into(),
                    detail: format!("{error}；已保留完整会话等待配置完成"),
                });
                return;
            }
        };
        let transcript = buf
            .messages
            .iter()
            .map(|(role, content)| format!("[{role}]\n{content}"))
            .collect::<Vec<_>>()
            .join("\n\n");
        let extraction_messages = l1_extraction_messages(&transcript);
        let extracted = match llm::complete_text(&provider, &extraction_messages).await {
            Ok(text) => match parse_typed_l1_candidates(&text) {
                Ok(candidates) => candidates,
                Err(error) => {
                    if let Some(telemetry) = crate::telemetry_store::shared_store() {
                        let _ = telemetry.set_l1_failure_for_session(session_id, &error);
                    }
                    self.restore_session_for(session_id, buf.clone());
                    self.log(IngestLog {
                        at: now_str(),
                        agent_id: buf.agent_id.clone(),
                        kind: "memory".into(),
                        state: "failed".into(),
                        detail: format!(
                            "L1 标签校验失败：{error}；完整会话已保留，可修正模型输出后重跑"
                        ),
                    });
                    return;
                }
            },
            Err(error) => {
                self.restore_session_for(session_id, buf.clone());
                self.log(IngestLog {
                    at: now_str(),
                    agent_id: buf.agent_id.clone(),
                    kind: "memory".into(),
                    state: "retrying".into(),
                    detail: format!("{}；已保留完整会话等待重试", error),
                });
                return;
            }
        };
        match crate::telemetry_store::shared_store()
            .ok_or_else(|| "本地记忆账本尚未初始化".to_string())
            .and_then(|telemetry| {
                telemetry.store_typed_l1_memories_for_session(session_id, &extracted)
            }) {
            Ok(n) => {
                crate::memory_backend::queue_semantic_l1_index(
                    extracted.iter().map(|item| item.content.clone()).collect(),
                );
                self.log(IngestLog {
                    at: now_str(),
                    agent_id: buf.agent_id.clone(),
                    kind: "memory".into(),
                    state: "stored".into(),
                    detail: if buf.source.is_empty() {
                        format!(
                            "{} 已分析完整会话；已写入本地记忆库 {} 条（无需等待记忆服务二次提取）",
                            provider.name, n
                        )
                    } else {
                        format!(
                            "{} 已分析 {} 的完整会话；已写入本地记忆库 {} 条（无需等待记忆服务二次提取）",
                            provider.name,
                            crate::agent_sources::agent_label(&buf.source),
                            n
                        )
                    },
                });
            }
            Err(e) => {
                eprintln!("[memory-ingest] local L1 write failed: {e}");
                if let Some(telemetry) = crate::telemetry_store::shared_store() {
                    let _ = telemetry.set_l1_failure_for_session(session_id, &e);
                }
                self.restore_session_for(session_id, buf.clone());
                self.log(IngestLog {
                    at: now_str(),
                    agent_id: buf.agent_id.clone(),
                    kind: "memory".into(),
                    state: "failed".into(),
                    detail: format!("L1 写入校验失败：{e}；完整会话已保留，可重跑"),
                });
            }
        }
    }

    fn restore_session_for(&self, session_id: &str, mut previous: SessionBuffer) {
        previous.last_active = Instant::now();
        let mut inner = self.inner.lock().unwrap();
        if let Some(current) = inner.sessions.remove(session_id) {
            previous.messages.extend(current.messages);
        }
        inner.sessions.insert(session_id.to_string(), previous);
    }

    fn log(&self, entry: IngestLog) {
        let mut inner = self.inner.lock().unwrap();
        inner.recent.insert(0, entry);
        inner.recent.truncate(30);
    }

    /// 前端展示用：最近提取记录。
    pub fn recent_logs(&self) -> Vec<IngestLog> {
        self.inner.lock().unwrap().recent.clone()
    }

    /// 当前缓冲中的会话数。
    pub fn buffered_sessions(&self) -> usize {
        self.inner.lock().unwrap().sessions.len()
    }

    pub fn flush_pending(&self, backend: Arc<MemoryBackend>) -> usize {
        if !self.is_enabled() {
            return 0;
        }
        let session_ids = self
            .inner
            .lock()
            .unwrap()
            .sessions
            .keys()
            .cloned()
            .collect::<Vec<_>>();
        let count = session_ids.len();
        for session_id in session_ids {
            self.flush_session(Arc::clone(&backend), &session_id);
        }
        count
    }
}

fn l1_extraction_messages(transcript: &str) -> Vec<Value> {
    let language = output_language_directive(transcript);
    vec![
        json!({"role": "system", "content": format!("你是本地 Vibe Assistant 的高质量会话记忆整理器。每个完整会话最多输出 3 条、通常 1–2 条。第一条必须是 `summary`，且 durability 必须为 `session`；其余只保留可跨后续任务复用的事实、已确认决定、偏好候选或约束。不要逐步复述操作、工具调用、短暂报错、无结论讨论、重复表述或大段代码。每条须可独立理解，明确主体和结论；不得臆测用户偏好。\n\n必须为每条写准确 durability：`session` 仅当前会话摘要、进度或临时排查；`short_term` 是当前项目/近期任务可能持续数天到数周的决定；`long_term` 仅限用户明确表达、跨项目且 90 天后仍适用的偏好或硬约束。助手自行做出的代码修改、模型/端点/Token 配置、UI 实现、构建/测试记录一律不得标为 `long_term`。若某条拿不准，请保守标为 `short_term`（summary 一律标 `session`），不要猜测升级。{language}\n\n严格输出 JSON：{{\"memories\":[{{\"content\":\"...\",\"type\":\"summary|fact|decision|constraint|preference_candidate|open_item\",\"durability\":\"session|short_term|long_term\"}}]}}。只输出这个 JSON，不要包裹在代码块或说明文字里。没有可长期复用的信息时只输出一个 summary。")}),
        json!({"role": "user", "content": format!("以下是一段已经结束的会话，请整体理解后整理为 L1：\n\n{transcript}")}),
    ]
}

/// Keep generated memory in the conversation's primary language.  This is
/// intentionally determined from the captured content instead of the app UI
/// locale: a Chinese conversation still needs Chinese memory in an English UI.
fn output_language_directive(source: &str) -> &'static str {
    let mut han = 0usize;
    let mut latin = 0usize;
    let mut japanese = 0usize;
    let mut korean = 0usize;
    for ch in source.chars() {
        match ch {
            '\u{4e00}'..='\u{9fff}' => han += 1,
            '\u{3040}'..='\u{30ff}' => japanese += 1,
            '\u{ac00}'..='\u{d7af}' => korean += 1,
            _ if ch.is_ascii_alphabetic() => latin += 1,
            _ => {}
        }
    }
    if japanese > 0 {
        "输出必须使用日语；不得翻译成其他语言。"
    } else if korean > 0 {
        "输出必须使用韩语；不得翻译成其他语言。"
    // Technical conversations often contain many ASCII identifiers, file
    // names, and product names inside otherwise Chinese prose. Require Latin
    // text to substantially outweigh Han text before treating it as English.
    } else if han.saturating_mul(2) > latin {
        "输出必须使用中文；不得翻译成英文或其他语言。"
    } else if latin > 0 {
        "Output must be in English; do not translate it into another language."
    } else {
        "输出必须使用原始内容的主要语言；不得自行翻译。"
    }
}

/// Only assistant output is filtered for thinking: a user may deliberately
/// discuss the literal marker syntax, while assistant reasoning must never
/// enter L0.  Injected harness context (system-reminder/meta 等) 对两个角色
/// 都剥离——它是宿主对模型说的话，不是对话内容。
fn strip_memory_thinking(role: &str, content: &str) -> String {
    let cleaned = crate::telemetry_store::strip_injected_context(content);
    if role == "assistant" {
        strip_thinking_blocks(&cleaned)
    } else {
        cleaned.trim().to_string()
    }
}

async fn extract_l1_conversation(
    transcript: &str,
) -> Result<Vec<crate::telemetry_store::L1MemoryCandidate>, String> {
    let provider = llm::memory_extraction_provider()?;
    let text = llm::complete_text(&provider, &l1_extraction_messages(transcript)).await?;
    parse_typed_l1_candidates(&text)
}

#[derive(Clone, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub struct MemoryImportResult {
    pub folder: String,
    pub scanned_files: u32,
    pub recognized_files: u32,
    pub skipped_files: u32,
    pub imported_memories: u32,
    pub message: String,
}

fn is_importable_memory_file(path: &Path) -> bool {
    matches!(
        path.extension()
            .and_then(|extension| extension.to_str())
            .map(|extension| extension.to_ascii_lowercase())
            .as_deref(),
        Some("md" | "txt" | "json" | "jsonl" | "yaml" | "yml")
    )
}

fn is_ignored_import_directory(path: &Path) -> bool {
    matches!(
        path.file_name().and_then(|name| name.to_str()),
        Some(
            ".git"
                | "node_modules"
                | "target"
                | "dist"
                | "build"
                | ".venv"
                | "venv"
                | "__pycache__"
                | "cache"
                | "blobs"
                | "binaries"
                | "vendor"
        )
    )
}

fn collect_import_files(
    directory: &Path,
    files: &mut Vec<PathBuf>,
    scanned: &mut u32,
    skipped: &mut u32,
    depth: u8,
) -> Result<(), String> {
    if depth > 12 || files.len() >= IMPORT_MAX_FILES {
        return Ok(());
    }
    let entries = fs::read_dir(directory).map_err(|error| format!("无法读取文件夹：{error}"))?;
    for entry in entries {
        if files.len() >= IMPORT_MAX_FILES {
            break;
        }
        let entry = entry.map_err(|error| format!("无法枚举文件夹内容：{error}"))?;
        let file_type = entry
            .file_type()
            .map_err(|error| format!("无法读取文件类型：{error}"))?;
        if file_type.is_symlink() {
            *skipped += 1;
            continue;
        }
        let path = entry.path();
        if file_type.is_dir() {
            if is_ignored_import_directory(&path) {
                *skipped += 1;
            } else {
                collect_import_files(&path, files, scanned, skipped, depth + 1)?;
            }
        } else if file_type.is_file() {
            *scanned += 1;
            if is_importable_memory_file(&path) {
                files.push(path);
            } else {
                *skipped += 1;
            }
        }
    }
    Ok(())
}

fn take_import_chunks(text: &str) -> Vec<String> {
    let chars = text.chars().collect::<Vec<_>>();
    chars
        .chunks(IMPORT_CHUNK_CHARS)
        .take(IMPORT_MAX_CHUNKS)
        .map(|chunk| chunk.iter().collect::<String>())
        .collect()
}

/// Manually import a folder of exported conversations or memory documents.
/// Only safe text formats are read and the selected source is never treated as
/// an executable configuration file.
#[tauri::command]
pub async fn memory_import_folder(
    telemetry: tauri::State<'_, crate::telemetry_store::TelemetryStore>,
    folder: String,
) -> Result<MemoryImportResult, String> {
    let root = PathBuf::from(&folder)
        .canonicalize()
        .map_err(|error| format!("无法访问选择的文件夹：{error}"))?;
    if !root.is_dir() {
        return Err("请选择一个文件夹，而不是单个文件".into());
    }

    let mut paths = Vec::new();
    let mut scanned = 0;
    let mut skipped = 0;
    collect_import_files(&root, &mut paths, &mut scanned, &mut skipped, 0)?;
    paths.sort();
    if paths.is_empty() {
        return Ok(MemoryImportResult {
            folder: root.display().to_string(),
            scanned_files: scanned,
            recognized_files: 0,
            skipped_files: skipped,
            imported_memories: 0,
            message: "未在该文件夹中找到可识别的记忆或会话文本文件".into(),
        });
    }

    let mut source = String::new();
    let mut recognized = 0u32;
    for path in paths {
        let metadata = match fs::metadata(&path) {
            Ok(metadata) => metadata,
            Err(_) => {
                skipped += 1;
                continue;
            }
        };
        if metadata.len() > 1_500_000 {
            skipped += 1;
            continue;
        }
        let content = match fs::read_to_string(&path) {
            Ok(content) => content,
            Err(_) => {
                skipped += 1;
                continue;
            }
        };
        let content = truncate_text(&content, IMPORT_MAX_FILE_CHARS);
        if content.trim().is_empty() {
            skipped += 1;
            continue;
        }
        let relative = path.strip_prefix(&root).unwrap_or(&path).display();
        let addition = format!("\n\n--- 文件：{relative} ---\n{content}");
        if source.chars().count() + addition.chars().count() > IMPORT_MAX_TOTAL_CHARS {
            break;
        }
        source.push_str(&addition);
        recognized += 1;
    }
    if source.trim().is_empty() {
        return Ok(MemoryImportResult {
            folder: root.display().to_string(),
            scanned_files: scanned,
            recognized_files: 0,
            skipped_files: skipped,
            imported_memories: 0,
            message: "识别到的文件没有可提取的文本内容".into(),
        });
    }

    let mut unique = HashSet::new();
    let mut candidates = Vec::new();
    for chunk in take_import_chunks(&source) {
        for candidate in extract_l1_conversation(&chunk).await? {
            let normalized = candidate
                .content
                .split_whitespace()
                .collect::<String>()
                .to_lowercase();
            if candidate.content.trim().len() >= 4 && unique.insert(normalized) {
                candidates.push(candidate);
            }
        }
    }
    if candidates.is_empty() {
        return Err("记忆模型未能从所选文件中提取有效记忆".into());
    }

    let source_key_digest = sha2::Sha256::digest(root.to_string_lossy().as_bytes());
    let import_key = format!(
        "manual-import:{}",
        crate::telemetry_store::hex(&source_key_digest)[..24].to_string()
    );
    let imported =
        telemetry.store_imported_memories(&import_key, &root.display().to_string(), &candidates)?;
    Ok(MemoryImportResult {
        folder: root.display().to_string(),
        scanned_files: scanned,
        recognized_files: recognized,
        skipped_files: skipped,
        imported_memories: imported as u32,
        message: format!("已从 {recognized} 个文本文件提取并写入 {imported} 条共享记忆"),
    })
}

fn truncate_text(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    let truncated = text.chars().take(max_chars).collect::<String>();
    format!("{truncated}\n[内容已截断]")
}

fn value_excerpt(value: Option<&Value>, max_chars: usize) -> String {
    let text = value
        .and_then(Value::as_str)
        .map(str::to_string)
        .or_else(|| value.and_then(|item| serde_json::to_string(item).ok()))
        .unwrap_or_else(|| "（未提供）".into());
    truncate_text(&text, max_chars)
}

/// Read a transcript turn and turn parser failures into an observable log
/// while preserving the hook payload's fallback response.
fn read_transcript_turn(
    path: Option<&str>,
    turn_id: Option<&str>,
    reader: fn(&Path, &str) -> Result<Vec<(String, String)>, String>,
    agent_name: &str,
) -> Option<Vec<(String, String)>> {
    let (Some(path), Some(turn_id)) = (path, turn_id) else {
        return None;
    };
    match reader(Path::new(path), turn_id) {
        Ok(messages) if !messages.is_empty() => Some(messages),
        Ok(_) => {
            eprintln!("[memory-ingest] no {agent_name} transcript messages for turn {turn_id}");
            None
        }
        Err(error) => {
            eprintln!("[memory-ingest] {agent_name} transcript read failed: {error}");
            None
        }
    }
}

fn read_hook_transcript(source: &str, payload: &Value) -> Option<Vec<(String, String)>> {
    match source {
        "claude" => read_transcript_turn(
            payload.get("transcript_path").and_then(Value::as_str),
            payload.get("prompt_id").and_then(Value::as_str),
            read_claude_turn,
            "Claude",
        ),
        "codex" => read_transcript_turn(
            payload.get("transcript_path").and_then(Value::as_str),
            payload.get("turn_id").and_then(Value::as_str),
            read_codex_turn,
            "Codex",
        ),
        _ => None,
    }
}

/// Populate newly introduced receipt previews from retained Stop records.
/// This never calls the LLM or memory backend; it only reads the original
/// local transcript and preserves user/assistant messages in the local ledger.
#[tauri::command]
pub fn telemetry_backfill_conversations() -> Result<u32, String> {
    let Some(store) = crate::telemetry_store::shared_store() else {
        return Ok(0);
    };
    let mut updated = 0;
    for (source, payload) in store.unannotated_stop_payloads(50)? {
        if let Some(messages) = read_hook_transcript(&source, &payload) {
            store.record_conversation(&source, &payload, "full", &messages)?;
            updated += 1;
        }
    }
    updated += scan_native_transcripts(&store, false)?;
    Ok(updated)
}

/// Import native transcripts from every locally supported Agent into the same
/// durable conversation ledger used by Hook traffic. This only stages L1
/// candidates; model extraction remains governed by the existing explicit
/// “organize conversations” action and its configured provider.
/// 转录读取器清洗规则的版本。每次修改结构化/文本清洗规则时递增，
/// 存量扫描状态会被清空并触发一次全量重扫，已污染的 L0 行随内容寻址
/// 重录自动修复（同 event_key 覆盖正文、重新进入待提取队列）。
/// v2：结构化块只保留 text 类；v3：注入包裹剥离下沉到读取器，
///     保证内容指纹反映清洗后文本（否则指纹不变的旧行永不重录）。
const TRANSCRIPT_SCANNER_VERSION: i64 = 3;

fn scan_native_transcripts(
    store: &crate::telemetry_store::TelemetryStore,
    retry_failed: bool,
) -> Result<u32, String> {
    if store
        .app_setting_get::<i64>("transcript_scanner_version")
        .unwrap_or(0)
        != TRANSCRIPT_SCANNER_VERSION
    {
        store.reset_transcript_scan_states()?;
        store.app_setting_set("transcript_scanner_version", &TRANSCRIPT_SCANNER_VERSION)?;
    }
    let mut imported = 0u32;
    // 转录根目录来自统一的 Agent 数据源注册表，支持按设备覆盖。
    for source in crate::agent_sources::AGENT_SOURCE_IDS {
        for root in crate::agent_sources::transcript_roots(source) {
            let mut pending = vec![root];
            let mut visited = 0usize;
            let mut candidates = Vec::new();
            while let Some(directory) = pending.pop() {
                if visited >= NATIVE_SCAN_MAX_ENTRIES {
                    break;
                }
                let Ok(entries) = fs::read_dir(directory) else {
                    continue;
                };
                for entry in entries.flatten() {
                    visited += 1;
                    let path = entry.path();
                    if path.is_dir() {
                        pending.push(path);
                        continue;
                    }
                    if path.extension().and_then(|extension| extension.to_str()) != Some("jsonl") {
                        continue;
                    }
                    let modified = entry
                        .metadata()
                        .ok()
                        .and_then(|metadata| metadata.modified().ok())
                        .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
                    candidates.push((modified, path));
                }
            }
            candidates.sort_by(|left, right| right.0.cmp(&left.0));
            for (_, path) in candidates
                .into_iter()
                .take(NATIVE_SCAN_MAX_FILES_PER_SOURCE)
            {
                if !store.should_scan_transcript(source, &path, retry_failed) {
                    continue;
                }
                let Some(session_id) = native_session_id(source, &path) else {
                    continue;
                };
                // ZCode 与 Claude Code 的会话互为镜像（同一 UUID 出现在两边的
                // projects 目录）。同一会话只记第一次扫描的来源，后到的镜像
                // 副本标记已扫但不再入账，避免对话和用量翻倍。
                if store.native_session_recorded_by_other_source(source, &session_id)? {
                    store.record_transcript_scan(source, &path, Some(&session_id), Ok(()));
                    continue;
                }
                let messages = match read_native_session(source, &path) {
                    Ok(messages) => messages,
                    Err(error) => {
                        eprintln!(
                            "[memory-ingest] {source} transcript read failed ({}): {error}",
                            path.display()
                        );
                        store.record_transcript_scan(source, &path, Some(&session_id), Err(&error));
                        continue;
                    }
                };
                if messages.is_empty() {
                    store.record_transcript_scan(source, &path, Some(&session_id), Ok(()));
                    continue;
                }
                let occurred_at = fs::metadata(&path)
                    .ok()
                    .and_then(|metadata| metadata.modified().ok())
                    .map(chrono::DateTime::<chrono::Utc>::from)
                    .map(|time| time.to_rfc3339())
                    .unwrap_or_else(now_str);
                if store.record_native_conversation(
                    source,
                    &session_id,
                    &path,
                    &occurred_at,
                    &messages,
                )? {
                    imported += 1;
                }
                store.record_transcript_scan(source, &path, Some(&session_id), Ok(()));
            }
        }
    }
    Ok(imported)
}

fn native_session_id(source: &str, path: &Path) -> Option<String> {
    crate::agent_sources::session_id_for_path(source, path)
}

fn read_native_session(source: &str, path: &Path) -> Result<Vec<(String, String)>, String> {
    match source {
        "codex" => read_codex_session(path),
        "claude" => read_claude_session(path),
        // ZCode 内置 Claude Code 引擎，转录与 Claude Code 同构。
        "zcode" => read_claude_session(path),
        "qoder" => read_qoder_session(path),
        "workbuddy" => read_workbuddy_session(path),
        "minimax" => read_minimax_session(path),
        "kimi" => read_kimi_session(path),
        _ => Ok(Vec::new()),
    }
}

/// MiniMax Code 会话位于 `v2/sessions/**/messages.jsonl`，每行
/// `{message_id, turn_id, message:{role, content:[...]}}`。只保留
/// user/assistant 的 text 片段；thinking 与工具载荷不进入记忆。
fn read_minimax_session(path: &Path) -> Result<Vec<(String, String)>, String> {
    let metadata = fs::metadata(path).map_err(|error| error.to_string())?;
    if metadata.len() > 16 * 1024 * 1024 {
        return Ok(Vec::new());
    }
    let file = fs::File::open(path).map_err(|error| error.to_string())?;
    let mut seen = HashSet::new();
    let mut messages = Vec::new();
    let mut total_chars = 0usize;
    for line in BufReader::new(file).lines() {
        if messages.len() >= NATIVE_SESSION_MAX_MESSAGES || total_chars >= NATIVE_SESSION_MAX_CHARS
        {
            break;
        }
        let line = line.map_err(|error| error.to_string())?;
        let Ok(entry) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        let Some(message) = entry.get("message") else {
            continue;
        };
        let Some(role) = message
            .get("role")
            .and_then(Value::as_str)
            .filter(|role| matches!(*role, "user" | "assistant"))
        else {
            continue;
        };
        // 同一 message_id 可能分多行追加（流式片段），按 message_id 去重。
        let identity = entry
            .get("message_id")
            .and_then(Value::as_str)
            .map(str::to_string)
            .unwrap_or_else(|| line.clone());
        if !seen.insert(identity) {
            continue;
        }
        let text = qoder_transcript_text(message.get("content"));
        if text.is_empty() {
            continue;
        }
        append_native_message(&mut messages, &mut total_chars, role, text);
    }
    Ok(messages)
}

/// Kimi（Kimi Code / Kimi Work）会话是 `<home>/sessions/**/wire.jsonl`。
/// 用户消息在 `context.append_message`，助手正文在 loop 事件的
/// `content.part`（`part.type == "text"`；`think` 不进入记忆）。
fn read_kimi_session(path: &Path) -> Result<Vec<(String, String)>, String> {
    let metadata = fs::metadata(path).map_err(|error| error.to_string())?;
    if metadata.len() > 16 * 1024 * 1024 {
        return Ok(Vec::new());
    }
    let file = fs::File::open(path).map_err(|error| error.to_string())?;
    let mut messages = Vec::new();
    let mut total_chars = 0usize;
    for line in BufReader::new(file).lines() {
        if messages.len() >= NATIVE_SESSION_MAX_MESSAGES || total_chars >= NATIVE_SESSION_MAX_CHARS
        {
            break;
        }
        let line = line.map_err(|error| error.to_string())?;
        let Ok(entry) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        match entry.get("type").and_then(Value::as_str) {
            Some("context.append_message") => {
                let Some(message) = entry.get("message") else {
                    continue;
                };
                if message.get("role").and_then(Value::as_str) != Some("user") {
                    continue;
                }
                let text = qoder_transcript_text(message.get("content"));
                if text.is_empty() {
                    continue;
                }
                append_native_message(&mut messages, &mut total_chars, "user", text);
            }
            Some("context.append_loop_event") => {
                let event = entry.get("event").cloned().unwrap_or(Value::Null);
                if event.get("type").and_then(Value::as_str) != Some("content.part") {
                    continue;
                }
                let Some(part) = event.get("part") else {
                    continue;
                };
                if part.get("type").and_then(Value::as_str) != Some("text") {
                    continue;
                }
                let Some(text) = part.get("text").and_then(Value::as_str) else {
                    continue;
                };
                let text = text.trim().to_string();
                if text.is_empty() {
                    continue;
                }
                append_native_message(&mut messages, &mut total_chars, "assistant", text);
            }
            _ => {}
        }
    }
    Ok(messages)
}

/// Extract only user/assistant text from a Qoder project JSONL. Tool payloads
/// are intentionally excluded from memories and token estimates. The limits
/// keep historical imports bounded before an LLM ever sees the transcript.
fn read_qoder_session(path: &Path) -> Result<Vec<(String, String)>, String> {
    let metadata = fs::metadata(path).map_err(|error| error.to_string())?;
    if metadata.len() > 16 * 1024 * 1024 {
        return Ok(Vec::new());
    }
    let file = fs::File::open(path).map_err(|error| error.to_string())?;
    let mut seen = HashSet::new();
    let mut messages = Vec::new();
    let mut total_chars = 0usize;
    for line in BufReader::new(file).lines() {
        if messages.len() >= NATIVE_SESSION_MAX_MESSAGES || total_chars >= NATIVE_SESSION_MAX_CHARS
        {
            break;
        }
        let line = line.map_err(|error| error.to_string())?;
        let Ok(entry) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        let Some(role) = entry
            .get("type")
            .and_then(Value::as_str)
            .filter(|role| matches!(*role, "user" | "assistant"))
        else {
            continue;
        };
        let identity = entry
            .get("uuid")
            .and_then(Value::as_str)
            .map(str::to_string)
            .unwrap_or(line);
        if !seen.insert(identity) {
            continue;
        }
        let text = qoder_transcript_text(
            entry
                .get("message")
                .and_then(|message| message.get("content")),
        );
        if text.is_empty() {
            continue;
        }
        append_native_message(&mut messages, &mut total_chars, role, text);
    }
    Ok(messages)
}

/// 从消息 content 中提取纯文本。结构化块只接受文本类（`text` /
/// `input_text` / `output_text`）；`tool_use`、`tool_result`、thinking、
/// 图片等载荷一律不进入 L0——它们是 Agent 的操作细节，不是对话内容。
/// 带 `tool_use_id` 的无 type 块同样视为工具结果剔除。
fn qoder_transcript_text(value: Option<&Value>) -> String {
    match value {
        Some(Value::String(text)) => text.trim().to_string(),
        Some(Value::Array(items)) => items
            .iter()
            .map(|item| qoder_transcript_text(Some(item)))
            .filter(|text| !text.is_empty())
            .collect::<Vec<_>>()
            .join("\n"),
        Some(Value::Object(object)) => {
            if object.contains_key("tool_use_id") {
                return String::new();
            }
            if let Some(kind) = object.get("type").and_then(Value::as_str) {
                if !matches!(kind, "text" | "input_text" | "output_text") {
                    return String::new();
                }
                return object
                    .get("text")
                    .or_else(|| object.get("content"))
                    .map(|text| qoder_transcript_text(Some(text)))
                    .unwrap_or_default();
            }
            object
                .get("text")
                .or_else(|| object.get("content"))
                .map(|child| qoder_transcript_text(Some(child)))
                .unwrap_or_default()
        }
        _ => String::new(),
    }
}

fn append_native_message(
    messages: &mut Vec<(String, String)>,
    total_chars: &mut usize,
    role: &str,
    text: String,
) {
    let text = strip_memory_thinking(role, &text);
    if text.is_empty()
        || messages.len() >= NATIVE_SESSION_MAX_MESSAGES
        || *total_chars >= NATIVE_SESSION_MAX_CHARS
    {
        return;
    }
    let remaining = NATIVE_SESSION_MAX_CHARS.saturating_sub(*total_chars);
    let text = truncate_text(&text, remaining.min(24_000));
    *total_chars += text.chars().count();
    messages.push((role.to_string(), text));
}

fn read_claude_session(path: &Path) -> Result<Vec<(String, String)>, String> {
    let metadata = fs::metadata(path).map_err(|error| error.to_string())?;
    if metadata.len() > 16 * 1024 * 1024 {
        return Ok(Vec::new());
    }
    let file = fs::File::open(path).map_err(|error| error.to_string())?;
    let mut seen = HashSet::new();
    let mut messages = Vec::new();
    let mut total_chars = 0usize;
    for line in BufReader::new(file).lines() {
        if messages.len() >= NATIVE_SESSION_MAX_MESSAGES || total_chars >= NATIVE_SESSION_MAX_CHARS
        {
            break;
        }
        let line = line.map_err(|error| error.to_string())?;
        let Ok(entry) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        let event_type = entry.get("type").and_then(Value::as_str).unwrap_or("");
        if !matches!(event_type, "user" | "assistant")
            || entry.get("sourceToolAssistantUUID").is_some()
        {
            continue;
        }
        let message = entry.get("message");
        let role = message
            .and_then(|message| message.get("role"))
            .and_then(Value::as_str)
            .unwrap_or(event_type);
        if !matches!(role, "user" | "assistant") {
            continue;
        }
        let identity = entry
            .get("uuid")
            .and_then(Value::as_str)
            .or_else(|| {
                message
                    .and_then(|message| message.get("id"))
                    .and_then(Value::as_str)
            })
            .map(str::to_string)
            .unwrap_or(line);
        if !seen.insert(identity) {
            continue;
        }
        append_native_message(
            &mut messages,
            &mut total_chars,
            role,
            transcript_text(message.and_then(|message| message.get("content"))),
        );
    }
    Ok(messages)
}

fn read_codex_session(path: &Path) -> Result<Vec<(String, String)>, String> {
    let metadata = fs::metadata(path).map_err(|error| error.to_string())?;
    if metadata.len() > 16 * 1024 * 1024 {
        return Ok(Vec::new());
    }
    let file = fs::File::open(path).map_err(|error| error.to_string())?;
    let mut seen = HashSet::new();
    let mut messages = Vec::new();
    let mut total_chars = 0usize;
    for line in BufReader::new(file).lines() {
        if messages.len() >= NATIVE_SESSION_MAX_MESSAGES || total_chars >= NATIVE_SESSION_MAX_CHARS
        {
            break;
        }
        let line = line.map_err(|error| error.to_string())?;
        let Ok(entry) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        if entry.get("type").and_then(Value::as_str) != Some("response_item") {
            continue;
        }
        let Some(payload) = entry.get("payload").filter(|payload| payload.is_object()) else {
            continue;
        };
        if payload.get("type").and_then(Value::as_str) != Some("message") {
            continue;
        }
        let Some(role) = payload
            .get("role")
            .and_then(Value::as_str)
            .filter(|role| matches!(*role, "user" | "assistant"))
        else {
            continue;
        };
        let identity = payload
            .get("id")
            .and_then(Value::as_str)
            .map(str::to_string)
            .unwrap_or(line);
        if !seen.insert(identity) {
            continue;
        }
        append_native_message(
            &mut messages,
            &mut total_chars,
            role,
            codex_transcript_text(payload.get("content")),
        );
    }
    Ok(messages)
}

fn read_workbuddy_session(path: &Path) -> Result<Vec<(String, String)>, String> {
    let metadata = fs::metadata(path).map_err(|error| error.to_string())?;
    if metadata.len() > 16 * 1024 * 1024 {
        return Ok(Vec::new());
    }
    let file = fs::File::open(path).map_err(|error| error.to_string())?;
    let mut seen = HashSet::new();
    let mut messages = Vec::new();
    let mut total_chars = 0usize;
    for line in BufReader::new(file).lines() {
        if messages.len() >= NATIVE_SESSION_MAX_MESSAGES || total_chars >= NATIVE_SESSION_MAX_CHARS
        {
            break;
        }
        let line = line.map_err(|error| error.to_string())?;
        let Ok(entry) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        let message = entry.get("message");
        let role = entry
            .get("role")
            .or_else(|| message.and_then(|message| message.get("role")))
            .or_else(|| entry.get("type"))
            .and_then(Value::as_str)
            .filter(|role| matches!(*role, "user" | "assistant"));
        let Some(role) = role else {
            continue;
        };
        let identity = entry
            .get("id")
            .or_else(|| entry.get("uuid"))
            .and_then(Value::as_str)
            .map(str::to_string)
            .unwrap_or(line);
        if !seen.insert(identity) {
            continue;
        }
        let content = message
            .and_then(|message| message.get("content"))
            .or_else(|| entry.get("content"))
            .or_else(|| entry.get("text"));
        append_native_message(
            &mut messages,
            &mut total_chars,
            role,
            qoder_transcript_text(content),
        );
    }
    Ok(messages)
}

/// Read the single Claude turn that begins with this hook's `prompt_id`.
/// Transcript lines for tool results are intentionally skipped: they are
/// operational detail, whereas the user prompt and assistant text form the
/// conversation we want the memory model to understand.
fn read_claude_turn(path: &Path, prompt_id: &str) -> Result<Vec<(String, String)>, String> {
    let metadata = std::fs::metadata(path).map_err(|error| error.to_string())?;
    if metadata.len() > 16 * 1024 * 1024 {
        return Err("Claude transcript exceeds 16 MB safety limit".into());
    }
    let file = std::fs::File::open(path).map_err(|error| error.to_string())?;
    let mut collecting = false;
    let mut messages = Vec::new();
    for line in BufReader::new(file).lines() {
        let line = line.map_err(|error| error.to_string())?;
        let entry: Value = match serde_json::from_str(&line) {
            Ok(entry) => entry,
            Err(_) => continue,
        };
        let entry_type = entry.get("type").and_then(Value::as_str).unwrap_or("");
        let is_prompt = entry_type == "user"
            && entry.get("promptId").and_then(Value::as_str).is_some()
            && entry.get("sourceToolAssistantUUID").is_none();
        if is_prompt {
            let entry_prompt_id = entry.get("promptId").and_then(Value::as_str).unwrap_or("");
            if collecting && entry_prompt_id != prompt_id {
                break;
            }
            if entry_prompt_id == prompt_id {
                collecting = true;
            }
        }
        if !collecting || !matches!(entry_type, "user" | "assistant") {
            continue;
        }
        let role = entry
            .get("message")
            .and_then(|message| message.get("role"))
            .and_then(Value::as_str)
            .unwrap_or(entry_type);
        let content = entry
            .get("message")
            .and_then(|message| message.get("content"));
        let text = transcript_text(content);
        let text = strip_memory_thinking(role, &text);
        if !text.is_empty() {
            messages.push((role.to_string(), truncate_text(&text, 24_000)));
        }
    }
    Ok(messages)
}

/// Read the Codex desktop JSONL records belonging to a hook's `turn_id`.
/// Codex stores its dialogue as `response_item/message` payloads, each with
/// `internal_chat_message_metadata_passthrough.turn_id`; tool calls and
/// reasoning are separate response-item types and are intentionally ignored.
fn read_codex_turn(path: &Path, turn_id: &str) -> Result<Vec<(String, String)>, String> {
    let metadata = std::fs::metadata(path).map_err(|error| error.to_string())?;
    if metadata.len() > 16 * 1024 * 1024 {
        return Err("Codex transcript exceeds 16 MB safety limit".into());
    }
    let file = std::fs::File::open(path).map_err(|error| error.to_string())?;
    let mut messages = Vec::new();
    for line in BufReader::new(file).lines() {
        let line = line.map_err(|error| error.to_string())?;
        let entry: Value = match serde_json::from_str(&line) {
            Ok(entry) => entry,
            Err(_) => continue,
        };
        if entry.get("type").and_then(Value::as_str) != Some("response_item") {
            continue;
        }
        let payload = match entry.get("payload") {
            Some(Value::Object(_)) => entry.get("payload").unwrap(),
            _ => continue,
        };
        if payload.get("type").and_then(Value::as_str) != Some("message") {
            continue;
        }
        let message_turn_id = payload
            .get("internal_chat_message_metadata_passthrough")
            .and_then(|metadata| metadata.get("turn_id"))
            .and_then(Value::as_str);
        if message_turn_id != Some(turn_id) {
            continue;
        }
        let role = payload.get("role").and_then(Value::as_str).unwrap_or("");
        if !matches!(role, "user" | "assistant") {
            continue;
        }
        let text = codex_transcript_text(payload.get("content"));
        let text = strip_memory_thinking(role, &text);
        if !text.is_empty() {
            messages.push((role.to_string(), truncate_text(&text, 24_000)));
        }
    }
    Ok(messages)
}

fn transcript_text(content: Option<&Value>) -> String {
    match content {
        Some(Value::String(text)) => text.trim().to_string(),
        Some(Value::Array(blocks)) => blocks
            .iter()
            .filter_map(|block| {
                (block.get("type").and_then(Value::as_str) == Some("text"))
                    .then(|| block.get("text").and_then(Value::as_str))
                    .flatten()
            })
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

fn codex_transcript_text(content: Option<&Value>) -> String {
    match content {
        Some(Value::String(text)) => text.trim().to_string(),
        Some(Value::Array(blocks)) => blocks
            .iter()
            .filter_map(|block| {
                matches!(
                    block.get("type").and_then(Value::as_str),
                    Some("input_text" | "output_text")
                )
                .then(|| block.get("text").and_then(Value::as_str))
                .flatten()
            })
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

/// 模型常见别名与未标注的 durability 归一化。拿不准（undetermined/缺失）时
/// 保守降级：summary 恒为 session，其余按 short_term 处理——宁可入库层级
/// 偏低，也不让整批记忆因一个标签被拒绝丢失。
fn normalize_l1_durability(raw: Option<&str>, memory_type: &str) -> String {
    let normalized = raw
        .map(|value| {
            value
                .trim()
                .to_ascii_lowercase()
                .replace([' ', '-', '_'], "")
        })
        .unwrap_or_default();
    match normalized.as_str() {
        "session" | "temporary" | "temp" => "session".into(),
        "shortterm" => "short_term".into(),
        "longterm" | "permanent" => "long_term".into(),
        // undetermined / unknown / 缺失 / 无法判断：保守取 short_term。
        _ => {
            if memory_type == "summary" {
                "session".into()
            } else {
                "short_term".into()
            }
        }
    }
}

/// 从模型输出中提取 memories 数组：容忍代码块、前后说明文字、裸数组、
/// `memory` 单数键等常见包装形态。返回 None 表示确实找不到任何数组。
fn extract_l1_memories_value(normalized: &str) -> Option<Value> {
    // 模型偶尔把键名写成单数 `memory`；契约里两个键都指同一数组。
    fn memories_array(value: &Value) -> Option<Value> {
        for key in ["memories", "memory"] {
            if let Some(values) = value.get(key).and_then(Value::as_array) {
                return Some(Value::Array(values.clone()));
            }
        }
        None
    }
    if let Ok(value) = serde_json::from_str::<Value>(normalized) {
        if let Some(values) = memories_array(&value) {
            return Some(values);
        }
        if value.is_array() {
            return Some(value);
        }
        return None;
    }
    // 整体不是合法 JSON：截取首个 '{' 到末个 '}'（或 '[' … ']'）再试一次。
    for (open, close) in [('{', '}'), ('[', ']')] {
        if let (Some(start), Some(end)) = (normalized.find(open), normalized.rfind(close)) {
            if start < end {
                if let Ok(value) = serde_json::from_str::<Value>(&normalized[start..=end]) {
                    if let Some(values) = memories_array(&value) {
                        return Some(values);
                    }
                    if value.is_array() {
                        return Some(value);
                    }
                }
            }
        }
    }
    // 括号不闭合（输出被截断）：逐元素回收 memories 数组里完整的对象。
    salvage_truncated_memories(normalized)
}

/// 契约失败时附在错误消息里的一小段模型原文。l1_error_detail 只保留 600
/// 字符，摘录压成单行并截到 200 字符，给错误前缀留出余量——此前只存错误
/// 消息不存原文，定位模型到底输出了什么只能靠重放请求。
fn output_excerpt(text: &str) -> String {
    let flattened = text.replace(['\n', '\r', '\t'], " ");
    let mut chars = flattened.chars();
    let excerpt: String = chars.by_ref().take(200).collect();
    if chars.next().is_some() {
        format!("{excerpt}…")
    } else {
        excerpt
    }
}

/// 截断补救：模型输出被 max_tokens 或服务端截断时 JSON 断在半途，整体
/// 解析与首尾大括号截取都会失败（括号不闭合）。定位 memories/memory 数组
/// 后逐元素扫描，只回收完整闭合的对象，丢弃断尾的残缺元素。
fn salvage_truncated_memories(normalized: &str) -> Option<Value> {
    let key_pos = ["memories", "memory"]
        .iter()
        .filter_map(|key| normalized.find(&format!("\"{key}\"")))
        .min()?;
    let array_start = normalized[key_pos..].find('[')? + key_pos;
    let bytes = normalized.as_bytes();
    let mut depth = 0usize;
    let mut in_string = false;
    let mut escaped = false;
    let mut elements: Vec<&str> = Vec::new();
    let mut element_start: Option<usize> = None;
    for (offset, &byte) in bytes.iter().enumerate().skip(array_start + 1) {
        let ch = byte as char;
        if in_string {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                in_string = false;
            }
            continue;
        }
        match ch {
            '"' => in_string = true,
            '{' => {
                if depth == 0 {
                    element_start = Some(offset);
                }
                depth += 1;
            }
            '}' => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    if let Some(start) = element_start.take() {
                        elements.push(&normalized[start..=offset]);
                    }
                }
            }
            // 数组顶层遇到 ']' 即数组结束；未闭合的残缺元素自然丢弃。
            ']' if depth == 0 => break,
            _ => {}
        }
    }
    let values = elements
        .iter()
        .filter_map(|element| serde_json::from_str::<Value>(element).ok())
        .collect::<Vec<_>>();
    (!values.is_empty()).then_some(Value::Array(values))
}

/// 解析 L1 输出并做**修复式**校验：模型输出轻微偏离契约（顺序、条数、
/// durability 标签、JSON 包装）时自动修正，而不是整批拒绝——此前严格校验
/// 导致 20 个会话永久失败且重试无益。仅在完全无法解析或没有任何可用条目
/// 时才报错。
fn parse_typed_l1_candidates(
    text: &str,
) -> Result<Vec<crate::telemetry_store::L1MemoryCandidate>, String> {
    let normalized = text
        .trim()
        .trim_start_matches("```json")
        .trim_start_matches("```")
        .trim_end_matches("```")
        .trim();
    let Some(values) =
        extract_l1_memories_value(normalized).and_then(|value| value.as_array().cloned())
    else {
        let excerpt = output_excerpt(normalized);
        return Err(if excerpt.is_empty() {
            "模型输出未符合 L1 JSON 契约，缺少 memories 数组（模型返回了空内容）".into()
        } else {
            format!("模型输出未符合 L1 JSON 契约，缺少 memories 数组。输出开头：{excerpt}")
        });
    };
    let mut candidates = Vec::new();
    for value in &values {
        let content = value
            .get("content")
            .or_else(|| value.get("memory"))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|content| !content.is_empty());
        let memory_type = value
            .get("type")
            .or_else(|| value.get("memory_type"))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|memory_type| !memory_type.is_empty());
        let (Some(content), Some(memory_type)) = (content, memory_type) else {
            continue;
        };
        let memory_type = memory_type.to_ascii_lowercase();
        if !matches!(
            memory_type.as_str(),
            "summary" | "fact" | "decision" | "constraint" | "preference_candidate" | "open_item"
        ) {
            continue;
        }
        candidates.push(crate::telemetry_store::L1MemoryCandidate {
            content: content.into(),
            durability: normalize_l1_durability(
                value.get("durability").and_then(Value::as_str),
                &memory_type,
            ),
            memory_type,
        });
    }
    if candidates.is_empty() {
        return Err(format!(
            "模型未输出任何可用的 L1 记忆。输出开头：{}",
            output_excerpt(normalized)
        ));
    }
    // 修复 1：summary 不在首位时移到首位（契约只要求第一条是 summary）。
    if let Some(index) = candidates
        .iter()
        .position(|candidate| candidate.memory_type == "summary")
    {
        let summary = candidates.remove(index);
        candidates.insert(0, summary);
    }
    // 修复 2：超过 3 条时保留 summary + 前 2 条其余记忆。
    candidates.truncate(3);
    Ok(candidates)
}

fn now_str() -> String {
    chrono::Local::now().format("%H:%M:%S").to_string()
}

#[derive(Clone, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub struct MemoryLayerRunResult {
    pub document: crate::telemetry_store::MemoryLayerDocument,
    pub selected_l1_count: u32,
    pub stage_count: u32,
    pub message: String,
}

/// Select a bounded and diverse evidence set.  Decisions, constraints and
/// preference candidates are never dropped merely because a session is long;
/// remaining items are capped per session to prevent one transcript from
/// monopolising the L2 input.
fn select_l2_evidence(
    items: Vec<crate::telemetry_store::LocalMemorySnapshot>,
) -> Vec<crate::telemetry_store::LocalMemorySnapshot> {
    let mut selected = Vec::new();
    let mut selected_ids = HashSet::new();
    for item in &items {
        if matches!(
            item.memory_type.as_str(),
            "decision" | "constraint" | "preference_candidate"
        ) && selected_ids.insert(item.id.clone())
        {
            selected.push(item.clone());
        }
    }
    let mut per_session: HashMap<String, u8> = HashMap::new();
    for item in items {
        if selected.len() >= 240 {
            break;
        }
        let count = per_session.entry(item.session_id.clone()).or_default();
        if *count >= 2 || !selected_ids.insert(item.id.clone()) {
            continue;
        }
        *count += 1;
        selected.push(item);
    }
    selected.truncate(240);
    selected
}

fn l2_evidence_text(items: &[crate::telemetry_store::LocalMemorySnapshot]) -> String {
    items
        .iter()
        .map(|item| {
            format!(
                "[{} | {} | {} | {}]\n{}",
                item.id,
                item.memory_type,
                item.source,
                item.event_time.as_deref().unwrap_or("unknown-time"),
                item.memory
            )
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// L3 must not rely exclusively on a single recent L2. Feed it direct L1
/// decisions, constraints, and explicit preference candidates as auditable
/// long-term evidence, while retaining the L2 for contextual synthesis.
fn select_l3_l1_evidence(
    items: Vec<crate::telemetry_store::LocalMemorySnapshot>,
) -> Vec<crate::telemetry_store::LocalMemorySnapshot> {
    items
        .into_iter()
        .filter(|item| {
            matches!(
                item.memory_type.as_str(),
                "decision" | "constraint" | "preference_candidate"
            )
        })
        .filter(|item| item.durability == "long_term")
        .filter(|item| !is_l3_volatile_bullet(&item.memory))
        .take(L3_L1_EVIDENCE_LIMIT)
        .collect()
}

fn l3_l1_evidence_text(items: &[crate::telemetry_store::LocalMemorySnapshot]) -> String {
    items
        .iter()
        .map(|item| {
            format!(
                "[L1:{} | {} | {} | {}]\n{}",
                item.id,
                item.memory_type,
                item.durability,
                item.event_time.as_deref().unwrap_or("unknown-time"),
                item.memory
            )
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

fn l2_map_messages(evidence: &str) -> Vec<Value> {
    vec![
        json!({"role":"system","content":"You are performing evidence-preserving L1-to-L2 memory compression. Return concise Markdown in the input language. Preserve confirmed decisions, constraints, explicit preferences, unresolved commitments, and their qualifiers. Do not invent facts, merge conflicting claims, or turn one-off activity into a user preference. Keep source ids in square brackets after every retained claim. This is an intermediate map; do not write a profile."}),
        json!({"role":"user","content":format!("L1 evidence:\n{evidence}")}),
    ]
}

fn l2_reduce_messages(parts: &[String]) -> Vec<Value> {
    vec![
        json!({"role":"system","content":format!("Create the current L2 short-term working memory from evidence-preserving map summaries. Output ONLY the finished memory document in concise Markdown in the original language, at most {MEMORY_LAYER_DOC_MAX_CHARS} characters. Begin directly with the memory; never describe the task, map summaries, source material, your plan, or reasoning. Organize: Current focus, Confirmed decisions, Constraints/preferences, Open items/risks. Retain source ids in square brackets for every factual bullet. Resolve neither ambiguity nor conflicts: label them explicitly. Exclude routine actions and stale details.")}),
        json!({"role":"user","content":format!("Map summaries:\n{}", parts.join("\n\n---\n\n"))}),
    ]
}

fn is_l2_final_heading(line: &str) -> bool {
    let title = line
        .trim_start()
        .trim_start_matches('#')
        .trim_start()
        .to_ascii_lowercase();
    title.starts_with("当前聚焦") || title.starts_with("current focus")
}

fn unwrap_markdown_document_fence(content: &str) -> String {
    let trimmed = content.trim();
    let Some((opening, remainder)) = trimmed.split_once('\n') else {
        return trimmed.to_string();
    };
    let language = opening
        .trim()
        .strip_prefix("```")
        .map(str::trim)
        .map(str::to_ascii_lowercase);
    if !matches!(language.as_deref(), Some("" | "markdown" | "md" | "text")) {
        return trimmed.to_string();
    }
    let body = remainder
        .rsplit_once("\n```")
        .map(|(body, _)| body)
        .unwrap_or(remainder)
        .trim();
    let is_markdown_document = body.lines().any(|line| {
        let line = line.trim_start();
        line.starts_with('#')
            || line.starts_with("- ")
            || line.starts_with("* ")
            || line.starts_with("| ")
    });
    if is_markdown_document {
        body.to_string()
    } else {
        trimmed.to_string()
    }
}

/// Reasoning models sometimes expose a natural-language planning preamble even
/// when no explicit `<think>` tags are present.  The L2 contract requires the
/// document to start with the Current Focus heading, so retain only the final
/// document when that heading appears later in the response.
fn normalize_l2_document(content: &str) -> String {
    let content = unwrap_markdown_document_fence(&strip_thinking_blocks(content));
    let mut byte_offset = 0;
    let mut final_heading_offset = None;
    for part in content.split_inclusive('\n') {
        if is_l2_final_heading(part) {
            // A leaked plan often contains an outline headed "Current focus"
            // before the actual document. The final occurrence is the only
            // candidate that can start the publishable L2 result.
            final_heading_offset = Some(byte_offset);
        }
        byte_offset += part.len();
    }
    if let Some(offset) = final_heading_offset {
        return content[offset..].trim().to_string();
    }
    // `split_inclusive` does not yield an empty final part, but it does yield a
    // non-newline-terminated final line. This fallback documents that a valid
    // heading is mandatory rather than silently publishing model narration.
    if let Some(last_line) = content
        .lines()
        .last()
        .filter(|line| is_l2_final_heading(line))
    {
        return last_line.trim().to_string();
    }
    content.trim().to_string()
}

fn is_l2_analysis_preamble(content: &str) -> bool {
    let opening = content
        .trim_start()
        .chars()
        .take(240)
        .collect::<String>()
        .to_ascii_lowercase();
    [
        "the user wants",
        "let me analyze",
        "let me review",
        "let me think",
        "let me synthesize",
        "i need to ",
        "i will ",
        "analysis:",
        "用户希望",
        "让我",
        "我需要",
        "我将",
        "分析：",
        "思考：",
    ]
    .iter()
    .any(|prefix| opening.starts_with(prefix))
}

fn has_l2_final_heading(content: &str) -> bool {
    content
        .lines()
        .find(|line| !line.trim().is_empty())
        .is_some_and(is_l2_final_heading)
}

fn normalize_l3_profile(content: &str) -> String {
    unwrap_markdown_document_fence(&strip_thinking_blocks(content))
}

fn has_l3_category_heading(content: &str) -> bool {
    content.lines().any(|line| {
        let trimmed = line.trim_start();
        trimmed.starts_with("## ") || trimmed.starts_with("### ")
    })
}

/// L2 also carries recent implementation decisions. These markers identify
/// material that may be useful in a work log but must not become user-level
/// L3 memory merely because a model described it as a constraint.
fn is_l3_volatile_bullet(line: &str) -> bool {
    let lower = line.to_ascii_lowercase();
    [
        "minimax",
        "endpoint",
        "api.minimax",
        "tls",
        "reasoning_split",
        "reasoning_content",
        "max_completion",
        "token",
        "模型",
        "端点",
        "输出预算",
        "i18n",
        "tauri",
        "window.confirm",
        "window.alert",
        "ui ",
        "ui、",
        "前端日期",
        "rust 9",
        "纳秒",
        "cargo check",
        "单元测试",
        "test command",
        "dev 进程",
        "dev process",
        "构建命令",
        "全局 json",
        "thinking",
        "当前任务",
        "当前项目状态",
    ]
    .iter()
    .any(|marker| lower.contains(marker))
}

/// Keep only categorized, durable bullets. L3 is a user profile, not a
/// project changelog; an empty filtered section is deliberately removed.
fn filter_l3_profile(content: &str) -> String {
    let mut sections = Vec::<(String, Vec<String>)>::new();
    let mut heading: Option<String> = None;
    let mut bullets = Vec::<String>::new();
    let flush = |heading: &mut Option<String>,
                 bullets: &mut Vec<String>,
                 sections: &mut Vec<(String, Vec<String>)>| {
        if let Some(title) = heading.take().filter(|_| !bullets.is_empty()) {
            sections.push((title, std::mem::take(bullets)));
        } else {
            bullets.clear();
        }
    };
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("## ") || trimmed.starts_with("### ") {
            flush(&mut heading, &mut bullets, &mut sections);
            heading = Some(trimmed.to_string());
        } else if (trimmed.starts_with("- ") || trimmed.starts_with("* "))
            && !is_l3_volatile_bullet(trimmed)
        {
            bullets.push(format!("- {}", trimmed[2..].trim()));
        }
    }
    flush(&mut heading, &mut bullets, &mut sections);
    sections
        .into_iter()
        .flat_map(|(heading, bullets)| std::iter::once(heading).chain(bullets))
        .collect::<Vec<_>>()
        .join("\n")
}

fn source_prefers_chinese(source: &str) -> bool {
    let han = source
        .chars()
        .filter(|character| matches!(*character, '\u{4e00}'..='\u{9fff}'))
        .count();
    let latin = source
        .chars()
        .filter(|character| character.is_ascii_alphabetic())
        .count();
    han.saturating_mul(2) > latin
}

fn is_usable_l3_profile(content: &str, source: &str) -> bool {
    let trimmed = content.trim();
    if trimmed.is_empty()
        || trimmed.chars().count() > L3_PROFILE_MAX_CHARS
        || is_l2_analysis_preamble(trimmed)
        || !has_l3_category_heading(trimmed)
        || trimmed.lines().any(is_l3_volatile_bullet)
    {
        return false;
    }
    let lower = trimmed.to_ascii_lowercase();
    if lower.contains("[内容已截断]")
        || lower.contains("[truncated]")
        || lower.contains("pending human approval")
    {
        return false;
    }
    !source_prefers_chinese(source)
        || trimmed
            .chars()
            .filter(|character| matches!(*character, '\u{4e00}'..='\u{9fff}'))
            .count()
            >= 8
}

fn l3_profile_messages(baseline: &str, evidence: &str, language: &str) -> Vec<Value> {
    vec![
        json!({"role":"system","content":format!("You write an L3 user Profile for long-term reuse across agent sessions. The existing published profile in the user message is the AUTHORITATIVE baseline and your most important input: carry its preferences, constraints and decisions forward by default, including any manual edits the user made, and only drop or change an existing item when the new evidence clearly contradicts it. {language}\n\nOutput ONLY the finished categorized Markdown document: no document title, preamble, code fence, approval notice, or explanation. Use 2–6 short category headings in the source language (each as `## Category`), with relevant factual bullets below each heading. Stop after the final bullet. The final document MUST be no more than {L3_PROFILE_TARGET_CHARS} characters.\n\nYou receive both L2 working-memory context and filtered direct L1 evidence. Prefer explicit L1 preferences and constraints, using L2 only to corroborate or add durable context. Apply a strict 90-day, cross-project test: retain only an explicit user preference or a hard constraint that would still apply to unrelated future work. Exclude current tasks, project status, change logs, dates, paths, source ids, secrets, model/provider names, endpoints, token budgets, framework APIs, UI conventions, i18n file rules, date-parsing quirks, build/test commands, temporary incidents, and implementation details. If evidence is merely a project decision or current engineering practice, omit it. Do not infer preferences. Clearly label uncertainty instead of guessing.")}),
        json!({"role":"user","content":format!("Existing published profile (may be empty):\n{baseline}\n\nL2 evidence:\n{evidence}")}),
    ]
}

fn l3_profile_retry_messages(baseline: &str, evidence: &str, language: &str) -> Vec<Value> {
    vec![
        json!({"role":"system","content":format!("Write the final long-term L3 Profile now. The existing published profile is the authoritative baseline and your most important input: carry its items forward by default, including manual user edits, and only change them when the new evidence clearly contradicts them. {language} Output only Markdown with 2–6 `## Category` headings in the source language and concise bullets under them: no document title, no preamble, no code fence, no explanation. Each bullet should stay one or two sentences and the entire final body must be under {L3_PROFILE_TARGET_CHARS} characters. Stop immediately after the final bullet. Use explicit filtered L1 preferences and constraints plus corroborating L2 context. Keep only facts that still apply after 90 days across unrelated projects. Remove model/provider names, endpoints, token budgets, framework/UI/i18n rules, build/test commands, current work, implementation detail, tool lists, status updates, source ids, and anything temporary. Do not infer preferences.")}),
        json!({"role":"user","content":format!("Existing published profile (may be empty):\n{baseline}\n\nL2 evidence:\n{evidence}")}),
    ]
}

fn l2_final_retry_messages(parts: &[String]) -> Vec<Value> {
    vec![
        json!({"role":"system","content":"Return the FINAL L2 working-memory document now. Do not plan, analyze, acknowledge, explain, or mention this instruction, the user, map summaries, or source documents. The first non-whitespace line MUST be the `## Current focus` heading in the evidence language; emit no text before it. Your entire response must be the finished concise Markdown memory in the evidence language. Use headings: Current focus, Confirmed decisions, Constraints/preferences, Open items/risks. Every retained factual bullet must keep its square-bracket source id. If there is no supported content for a heading, omit it."}),
        json!({"role":"user","content":format!("Evidence-preserving map summaries:\n{}", parts.join("\n\n---\n\n"))}),
    ]
}

/// Manual L2 generation only reads a bounded recent L1 window.  It uses a
/// map-reduce compression pass and stores all selected evidence ids, rather
/// than feeding the entire memory library into one unreviewable prompt.
pub(crate) async fn consolidate_l2(
    telemetry: &crate::telemetry_store::TelemetryStore,
    days: i64,
) -> Result<MemoryLayerRunResult, String> {
    let days = days.clamp(1, 90);
    let mut evidence = select_l2_evidence(telemetry.recent_l1_memories(days, 500)?);
    // 归入 L2 的用户自定义记忆强制进入整理证据：不受时间窗与类型筛选
    // 影响；L3 层自定义记忆只进 Profile，两者相互独立。
    let selected_ids = evidence
        .iter()
        .map(|item| item.id.clone())
        .collect::<std::collections::HashSet<_>>();
    for item in telemetry.user_defined_l1_memories_for_scope("l2")? {
        if !selected_ids.contains(&item.id) {
            evidence.push(item);
        }
    }
    if evidence.is_empty() {
        return Err(format!("最近 {days} 天没有可用于 L2 的已提取会话记忆"));
    }
    let provider = llm::memory_extraction_provider()?;
    let mut maps = Vec::new();
    for chunk in evidence.chunks(50) {
        let text = l2_evidence_text(chunk);
        maps.push(truncate_text(
            &llm::complete_text(&provider, &l2_map_messages(&text)).await?,
            8_000,
        ));
    }
    let mut content =
        normalize_l2_document(&llm::complete_text(&provider, &l2_reduce_messages(&maps)).await?);
    if is_l2_analysis_preamble(&content) || !has_l2_final_heading(&content) {
        content = normalize_l2_document(
            &llm::complete_text(&provider, &l2_final_retry_messages(&maps)).await?,
        );
    }
    // Normalise before truncation.  Otherwise a long exposed planning preamble
    // can consume the L2 character budget and discard the actual memory.
    content = truncate_text(&content, MEMORY_LAYER_DOC_MAX_CHARS);
    if content.trim().is_empty() {
        return Err("L2 记忆模型返回空内容，未写入任何数据".into());
    }
    if is_l2_analysis_preamble(&content) || !has_l2_final_heading(&content) {
        return Err("L2 记忆模型返回任务分析而非最终记忆；已停止发布，请稍后重新生成".into());
    }
    let source_ids = evidence
        .iter()
        .map(|item| item.id.clone())
        .collect::<Vec<_>>();
    let now = chrono::Utc::now().to_rfc3339();
    let start = (chrono::Utc::now() - chrono::Duration::days(days)).to_rfc3339();
    let document = telemetry.save_memory_layer_document(
        "l2",
        &content,
        "published",
        "l1",
        &source_ids,
        Some(start),
        Some(now),
    )?;
    Ok(MemoryLayerRunResult {
        document,
        selected_l1_count: source_ids.len() as u32,
        stage_count: maps.len() as u32,
        message: format!(
            "L2 已由 {} 条 L1 证据分 {} 组压缩并发布；每条保留来源 id",
            source_ids.len(),
            maps.len()
        ),
    })
}

#[tauri::command]
pub async fn memory_short_term_consolidate(
    telemetry: tauri::State<'_, crate::telemetry_store::TelemetryStore>,
    days: Option<i64>,
) -> Result<MemoryLayerRunResult, String> {
    begin_layer_refresh()?;
    let result = consolidate_l2(&telemetry, days.unwrap_or(30)).await;
    end_layer_refresh();
    result
}

/// Build, but never automatically publish, a compact L3 Profile. Existing
/// Profile text is input as a baseline so updates preserve stable preferences
/// unless newer L2 evidence explicitly supports a change.
pub(crate) async fn draft_l3(
    telemetry: &crate::telemetry_store::TelemetryStore,
) -> Result<MemoryLayerRunResult, String> {
    let l2 = telemetry
        .memory_layer_documents("l2", 12)?
        .into_iter()
        .filter(|doc| doc.state == "published")
        .collect::<Vec<_>>();
    if l2.is_empty() {
        return Err("请先手动生成 L2 近 30 天工作记忆，再创建 L3 Profile 草案".into());
    }
    let current = telemetry.active_memory_layer_document("l3")?;
    let mut l1 = select_l3_l1_evidence(telemetry.local_l1_memory_snapshot()?);
    // 归入 L3 的用户自定义记忆强制进入 Profile 证据：类型/易变性筛选
    // 可能把它们挡在外面，但用户手写条目本身就是明确的长期偏好与约束。
    let selected_ids = l1
        .iter()
        .map(|item| item.id.clone())
        .collect::<std::collections::HashSet<_>>();
    for item in telemetry.user_defined_l1_memories_for_scope("l3")? {
        if !selected_ids.contains(&item.id) {
            l1.push(item);
        }
    }
    let mut source_references = l2
        .iter()
        .map(|doc| ("l2".to_string(), doc.id.clone()))
        .collect::<Vec<_>>();
    source_references.extend(l1.iter().map(|item| ("l1".to_string(), item.id.clone())));
    let l2_input = l2
        .iter()
        .map(|doc| {
            format!(
                "[L2:{} | {}]\n{}",
                doc.id,
                doc.created_at,
                normalize_l2_document(&doc.content)
            )
        })
        .collect::<Vec<_>>()
        .join("\n\n---\n\n");
    let input = format!(
        "Published L2 context:\n{l2_input}\n\n---\n\nFiltered direct L1 evidence:\n{}",
        l3_l1_evidence_text(&l1)
    );
    let provider = llm::memory_extraction_provider()?;
    let baseline = current
        .as_ref()
        .map(|doc| normalize_l3_profile(&doc.content))
        .unwrap_or_else(|| "(no published profile yet)".to_string());
    let language = output_language_directive(&input);
    // Use the user's configured provider generation limit here. A narrow
    // caller-side ceiling can end an M3 response before it emits `content`,
    // even when the final persisted Profile itself is deliberately short.
    // Treat any empty/invalid first response like malformed output and retry
    // once with a much smaller final-body contract.
    let initial = llm::complete_text_with_limit(
        &provider,
        &l3_profile_messages(&baseline, &input, language),
        None,
    )
    .await;
    let initial_error = initial.as_ref().err().cloned();
    let mut content = initial
        .ok()
        .map(|text| filter_l3_profile(&normalize_l3_profile(&text)))
        .unwrap_or_default();
    if !is_usable_l3_profile(&content, &input) {
        let retry = llm::complete_text_with_limit(
            &provider,
            &l3_profile_retry_messages(&baseline, &input, language),
            None,
        )
        .await;
        content = match retry {
            Ok(text) => filter_l3_profile(&normalize_l3_profile(&text)),
            Err(retry_error) => {
                return Err(initial_error
                    .map(|error| format!("L3 首次生成失败：{error}；紧凑重试失败：{retry_error}"))
                    .unwrap_or(retry_error))
            }
        };
    }
    if !is_usable_l3_profile(&content, &input) {
        return Err(
            "L3 Profile 未满足语言、长度或长期记忆格式要求，未保存草案；请稍后重新生成".into(),
        );
    }
    // 永久写入 MCP 调用提示：确定性追加而非依赖生成模型，重生成后提示
    // 依然存在；已含标记时 with_l3_mcp_hint 原样返回。
    let content = crate::memory_mcp::with_l3_mcp_hint(&content);
    let document = telemetry.save_memory_layer_document_with_sources(
        "l3",
        &content,
        "draft",
        &source_references,
        None,
        None,
    )?;
    Ok(MemoryLayerRunResult {
        document,
        selected_l1_count: l1.len() as u32,
        stage_count: l2.len() as u32,
        message: format!(
            "已基于 {} 份已发布 L2 与 {} 条筛选 L1 证据创建 L3 Profile 草案；请检查后手动发布",
            l2.len(),
            l1.len()
        ),
    })
}

#[tauri::command]
pub async fn memory_long_term_profile_draft(
    telemetry: tauri::State<'_, crate::telemetry_store::TelemetryStore>,
) -> Result<MemoryLayerRunResult, String> {
    begin_layer_refresh()?;
    let result = draft_l3(&telemetry).await;
    end_layer_refresh();
    result
}

#[tauri::command]
pub fn memory_long_term_profile_publish(
    telemetry: tauri::State<'_, crate::telemetry_store::TelemetryStore>,
    document_id: String,
    content: Option<String>,
) -> Result<crate::telemetry_store::MemoryLayerDocument, String> {
    let document = telemetry.publish_memory_layer_document(&document_id, content.as_deref())?;
    if document.layer != "l3" {
        return Err("只能发布 L3 Profile 草案".into());
    }
    Ok(document)
}

/// 手动编辑已发布的 L2 工作记忆正文：L2 生成即发布、没有发布闸门，
/// 保存即覆盖当前发布版，下一次注入与 L3 草案立刻生效。
#[tauri::command]
pub fn memory_layer_document_update(
    telemetry: tauri::State<'_, crate::telemetry_store::TelemetryStore>,
    document_id: String,
    content: String,
) -> Result<crate::telemetry_store::MemoryLayerDocument, String> {
    telemetry.update_published_memory_layer_content(&document_id, &content)
}

#[tauri::command]
pub fn memory_long_term_profile_delete_draft(
    telemetry: tauri::State<'_, crate::telemetry_store::TelemetryStore>,
    document_id: String,
) -> Result<(), String> {
    telemetry.delete_l3_draft(&document_id)
}

#[tauri::command]
pub async fn memory_layer_documents(
    telemetry: tauri::State<'_, crate::telemetry_store::TelemetryStore>,
    layer: String,
) -> Result<Vec<crate::telemetry_store::MemoryLayerDocument>, String> {
    if !matches!(layer.as_str(), "l2" | "l3") {
        return Err("仅支持 L2 或 L3 记忆层".into());
    }
    telemetry.memory_layer_documents(&layer, 12)
}

// ────────────────────────────────────────────────────────────────────────────
// Hook 自动配置：向支持 hook 的 Agent 写入回调（Claude Code settings.json）
// ────────────────────────────────────────────────────────────────────────────

/// Stable marker used to recognise entries injected by Vibe Assistant.  Do not
/// use the HTTP address here: the hook server port is user configurable.
const HOOK_MARKER: &str = "X-Agent-Manager-Hook";
const CLAUDE_SETTINGS: &str = ".claude/settings.json";
const CODEX_HOOKS: &str = ".codex/hooks.json";
const WORKBUDDY_SETTINGS: &str = ".workbuddy/settings.json";

fn agent_settings_path(agent_type: &str) -> Option<PathBuf> {
    if agent_type == "qoder" {
        // Qoder 国际版在 ~/.qoder，国内版在 ~/.qoder-cn；由注册表按实际
        // 活动目录（含用户覆盖）选出配置主目录。
        return crate::agent_sources::config_home("qoder").map(|home| home.join("settings.json"));
    }
    let relative = match agent_type {
        "claude" => CLAUDE_SETTINGS,
        "codex" => CODEX_HOOKS,
        "workbuddy" => WORKBUDDY_SETTINGS,
        _ => return None,
    };
    dirs_next::home_dir().map(|h| h.join(relative))
}

fn hook_command(agent_type: &str) -> Result<String, String> {
    let url = format!(
        "http://127.0.0.1:{}/memory/hook?source={agent_type}",
        crate::agent_http::AGENT_HTTP_PORT
    );
    let marker = format!(r#" -H "{HOOK_MARKER}: 1""#);

    if cfg!(windows) {
        Ok(format!(
            r#"curl.exe -fsS --max-time 5 -X POST "{url}" -H "Content-Type: application/json" --data-binary "@-" -o NUL{marker}"#
        ))
    } else {
        Ok(format!(
            r#"curl -fsS --max-time 5 -X POST '{url}' -H 'Content-Type: application/json' --data-binary '@-' -o /dev/null{marker}"#
        ))
    }
}

/// SessionStart 注入 hook：拉取 L3 长期记忆 + L2 近 30 天工作记忆合并写到
/// stdout。端点已按来源返回 Claude 形态的结构化 JSON
/// （hookSpecificOutput.additionalContext），curl 只需透传，harness 解析后注入
/// 模型上下文。应用未运行时 curl 静默失败，不阻塞会话启动。URL 携带 source
/// 标识，端点据此把注入计入记忆注入摘要。
fn hook_inject_command(agent_type: &str) -> String {
    let url = format!(
        "http://127.0.0.1:{}/memory/context?source={agent_type}&event=SessionStart",
        crate::agent_http::AGENT_HTTP_PORT
    );
    let marker = format!(r#" -H "{HOOK_MARKER}: 1""#);
    if cfg!(windows) {
        format!(r#"curl.exe -fsS --max-time 5 "{url}"{marker}"#)
    } else {
        format!(r#"curl -fsS --max-time 5 '{url}'{marker}"#)
    }
}

/// UserPromptSubmit 组合 hook：先透传 stdin 事件到沉淀端点（出站），再拉取
/// L3 长期记忆写到 stdout（入站）。端点按内容指纹门控：L3 未变化时返回空
/// 正文，本轮零注入成本。两条 curl 用分隔符串联：沉淀失败（应用未运行）
/// 不阻断注入；注入失败时 stdout 为空，harness 按无附加上下文继续，不阻塞
/// 提问。端点带 event=UserPromptSubmit，按事件名返回协议格式并单独计入
/// 记忆注入摘要（跳过也会留痕，正文不重复存储）。
fn hook_prompt_command(agent_type: &str) -> Result<String, String> {
    let sink = hook_command(agent_type)?;
    let inject_url = format!(
        "http://127.0.0.1:{}/memory/context?source={agent_type}&event=UserPromptSubmit",
        crate::agent_http::AGENT_HTTP_PORT
    );
    let marker = format!(r#" -H "{HOOK_MARKER}: 1""#);
    if cfg!(windows) {
        Ok(format!(
            r#"{sink} & curl.exe -fsS --max-time 5 "{inject_url}"{marker}"#
        ))
    } else {
        Ok(format!(
            r#"{sink}; curl -fsS --max-time 5 '{inject_url}'{marker}"#
        ))
    }
}

/// Install hooks for an adapter that implements the command-hook shape.  The
/// configuration paths stay explicit per harness; one Agent is never written
/// into another Agent's settings file.
#[tauri::command]
pub fn memory_hook_install(agent_type: String) -> Result<Vec<String>, String> {
    match agent_type.as_str() {
        "claude" | "qoder" | "codex" | "workbuddy" => install_command_hooks(&agent_type),
        other => Err(format!("暂不支持的 Agent 类型: {other}")),
    }
}

fn install_command_hooks(agent_type: &str) -> Result<Vec<String>, String> {
    let path = agent_settings_path(agent_type).ok_or("无法定位用户主目录")?;
    let mut settings: Value = std::fs::read_to_string(&path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_else(|| json!({}));

    let hooks = settings
        .get_mut("hooks")
        .map(|h| h.take())
        .unwrap_or_else(|| json!({}));
    let mut hooks = if hooks.is_object() { hooks } else { json!({}) };

    let command = hook_command(agent_type)?;
    let inject_command = hook_inject_command(agent_type);
    let prompt_command = hook_prompt_command(agent_type)?;
    let mut installed = Vec::new();
    // 出站沉淀（Agent → 本应用）与入站注入：SessionStart 注入 L3+L2 完整
    // 稳定层；UserPromptSubmit 用组合命令沉淀当前轮，并按内容指纹门控注入
    // L3（内容未变化时端点返回空正文，本轮跳过）。
    for (event, command) in [
        ("UserPromptSubmit", &prompt_command),
        ("PostToolUse", &command),
        ("Stop", &command),
        ("SessionStart", &inject_command),
    ] {
        // 已有的该事件配置保留；若未包含我们的 hook 则追加
        let existing = hooks.get(event).cloned().unwrap_or_else(|| json!([]));
        let contains_ours = serde_json::to_string(&existing)
            .map(|s| s.contains(HOOK_MARKER))
            .unwrap_or(false);
        if contains_ours && event != "SessionStart" && event != "UserPromptSubmit" {
            continue;
        }
        let mut arr = if existing.is_array() {
            existing.as_array().unwrap().clone()
        } else {
            vec![]
        };
        // 注入命令可能随版本演进（例如补充 source 标识、升级为每轮注入）；
        // 始终把我们旧版的 SessionStart / UserPromptSubmit hook 替换为最新命令，
        // 其余事件的已有配置保持不动。
        if event == "SessionStart" || event == "UserPromptSubmit" {
            arr.retain(|item| {
                serde_json::to_string(item)
                    .map(|s| !s.contains(HOOK_MARKER))
                    .unwrap_or(true)
            });
        }
        arr.push(json!({
            "hooks": [{"type": "command", "command": command}]
        }));
        hooks[event] = Value::Array(arr);
        installed.push(event.to_string());
    }

    settings["hooks"] = hooks;
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    std::fs::write(
        &path,
        serde_json::to_string_pretty(&settings).map_err(|e| e.to_string())?,
    )
    .map_err(|e| format!("写入 {} 失败: {e}", path.display()))?;
    Ok(installed)
}

/// 卸载本工具注入的 hook 回调（只移除包含 memory/hook 的命令，保留其他配置）。
#[tauri::command]
pub fn memory_hook_uninstall(agent_type: String) -> Result<(), String> {
    if !matches!(
        agent_type.as_str(),
        "claude" | "qoder" | "codex" | "workbuddy"
    ) {
        return Ok(());
    }
    let path = agent_settings_path(&agent_type).ok_or("无法定位用户主目录")?;
    let Ok(text) = std::fs::read_to_string(&path) else {
        return Ok(());
    };
    let Ok(mut settings) = serde_json::from_str::<Value>(&text) else {
        return Ok(());
    };
    let Some(hooks) = settings.get_mut("hooks") else {
        return Ok(());
    };
    if !hooks.is_object() {
        return Ok(());
    }
    for event in ["UserPromptSubmit", "PostToolUse", "Stop", "SessionStart"] {
        if let Some(arr) = hooks.get_mut(event) {
            if let Some(list) = arr.as_array_mut() {
                list.retain(|item| {
                    serde_json::to_string(item)
                        .map(|s| !s.contains(HOOK_MARKER))
                        .unwrap_or(true)
                });
            }
        }
    }
    std::fs::write(
        &path,
        serde_json::to_string_pretty(&settings).map_err(|e| e.to_string())?,
    )
    .map_err(|e| format!("写入 {} 失败: {e}", path.display()))?;
    Ok(())
}

#[derive(serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub struct HookStatus {
    pub installed: bool,
    pub agent_type: String,
    pub events: Vec<String>,
}

/// 检查某类 Agent 是否已安装记忆 hook。
#[tauri::command]
pub async fn memory_hook_status(agent_type: String) -> Result<HookStatus, String> {
    if matches!(
        agent_type.as_str(),
        "claude" | "qoder" | "codex" | "workbuddy"
    ) {
        if let Some(path) = agent_settings_path(&agent_type) {
            if let Ok(text) = std::fs::read_to_string(&path) {
                let Ok(settings) = serde_json::from_str::<Value>(&text) else {
                    return Ok(HookStatus {
                        installed: false,
                        agent_type,
                        events: vec![],
                    });
                };
                let events = ["UserPromptSubmit", "PostToolUse", "Stop", "SessionStart"]
                    .iter()
                    .filter(|e| {
                        settings
                            .get("hooks")
                            .and_then(|hooks| hooks.get(**e))
                            .and_then(Value::as_array)
                            .map(|items| {
                                items.iter().any(|item| {
                                    serde_json::to_string(item)
                                        .map(|item| {
                                            item.contains(HOOK_MARKER)
                                                // 旧版注入命令不带 source 标识，无法计入
                                                // 记忆注入摘要；旧版 UserPromptSubmit 只
                                                // 沉淀不注入。两者均视为未启用以引导升级。
                                                && ((**e != "SessionStart"
                                                    && **e != "UserPromptSubmit")
                                                    || item.contains("memory/context?source="))
                                        })
                                        .unwrap_or(false)
                                })
                            })
                            .unwrap_or(false)
                    })
                    .map(|s| s.to_string())
                    .collect::<Vec<_>>();
                return Ok(HookStatus {
                    installed: !events.is_empty(),
                    agent_type,
                    events,
                });
            }
        }
    }
    Ok(HookStatus {
        installed: false,
        agent_type,
        events: vec![],
    })
}

/// 沉淀管道状态（前端展示）。
#[derive(serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub struct IngestStatus {
    pub enabled: bool,
    pub buffered_sessions: usize,
    pub model_provider_id: Option<String>,
    pub model_ready: bool,
    pub manual_extraction_running: bool,
    pub manual_extraction_cancelling: bool,
    pub recent: Vec<IngestLog>,
}

#[tauri::command]
pub async fn memory_ingest_status(
    state: tauri::State<'_, IngestStore>,
) -> Result<IngestStatus, String> {
    let model_provider_id = crate::llm::memory_extraction_config().provider_id;
    let model_ready = crate::llm::memory_extraction_provider().is_ok();
    let manual_extraction = MANUAL_EXTRACTION.lock().unwrap();
    Ok(IngestStatus {
        enabled: state.is_enabled(),
        buffered_sessions: state.buffered_sessions(),
        model_provider_id,
        model_ready,
        manual_extraction_running: manual_extraction.is_some(),
        manual_extraction_cancelling: manual_extraction.as_ref().is_some_and(|sender| *sender.borrow()),
        recent: state.recent_logs(),
    })
}

#[tauri::command]
pub fn memory_ingest_set_enabled(state: tauri::State<'_, IngestStore>, enabled: bool) {
    state.set_enabled(enabled);
}

#[tauri::command]
pub fn memory_ingest_flush_pending(state: tauri::State<'_, IngestStore>) -> usize {
    crate::memory_backend::shared_backend()
        .map(|backend| state.flush_pending(backend))
        .unwrap_or(0)
}

/// Backfill complete conversations received before memory extraction was
/// configured, or that failed during a prior extraction attempt.
#[derive(Clone, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub struct OrganizeConversationsResult {
    pub attempted: u32,
    pub succeeded: u32,
    pub failed: u32,
    pub failure_reasons: Vec<String>,
    pub cancelled: bool,
}

#[tauri::command]
pub fn memory_ingest_cancel_organize() -> bool {
    let active = MANUAL_EXTRACTION.lock().unwrap();
    if let Some(sender) = active.as_ref() {
        sender.send_replace(true);
        true
    } else {
        false
    }
}

#[tauri::command]
pub async fn memory_ingest_organize_conversations() -> Result<OrganizeConversationsResult, String> {
    let run = ManualExtractionGuard::start()?;
    let ingest = ingest_store().ok_or("自动沉淀模块尚未初始化")?;
    let telemetry = crate::telemetry_store::shared_store().ok_or("本地对话账本尚未初始化")?;
    let native_imported = scan_native_transcripts(&telemetry, true)?;
    let pending = if run.cancelled() {
        Vec::new()
    } else {
        telemetry.pending_l1_conversations(1_000)?
    };
    ingest.log(IngestLog { at: now_str(), agent_id: GLOBAL_MEMORY_OWNER.into(), kind: "memory".into(), state: "working".into(), detail: format!("整理队列开始：原生扫描新增 {native_imported}；待处理 {} 个会话，每批 {ORGANIZE_BATCH_LIMIT} 个", pending.len()) });
    let mut result = OrganizeConversationsResult {
        attempted: 0,
        succeeded: 0,
        failed: 0,
        failure_reasons: Vec::new(),
        cancelled: false,
    };
    'batches: for (batch_index, batch) in pending.chunks(ORGANIZE_BATCH_LIMIT as usize).enumerate() {
        if run.cancelled() { result.cancelled = true; break; }
        ingest.log(IngestLog {
            at: now_str(),
            agent_id: GLOBAL_MEMORY_OWNER.into(),
            kind: "memory".into(),
            state: "working".into(),
            detail: format!(
                "正在执行第 {} 批（{} 个会话）",
                batch_index + 1,
                batch.len()
            ),
        });
        for conversation in batch {
            if run.cancelled() { result.cancelled = true; break 'batches; }
            result.attempted += 1;
            let mut last_error = String::new();
            let mut stored = false;
            for retry in 0..=3 {
                if run.cancelled() { result.cancelled = true; break 'batches; }
                ingest.log(IngestLog {
                    at: now_str(),
                    agent_id: GLOBAL_MEMORY_OWNER.into(),
                    kind: "memory".into(),
                    state: "working".into(),
                    detail: format!("正在提取会话要点（第 {}/4 次）", retry + 1),
                });
                match run.extract(&conversation.conversation_text).await {
                    None => { result.cancelled = true; break 'batches; }
                    Some(Ok(Ok(candidates))) => {
                        if run.cancelled() { result.cancelled = true; break 'batches; }
                        let count = telemetry
                            .store_typed_l1_memories(&conversation.event_key, &candidates)?;
                        crate::memory_backend::queue_semantic_l1_index(
                            candidates.iter().map(|item| item.content.clone()).collect(),
                        );
                        result.succeeded += 1;
                        stored = true;
                        ingest.log(IngestLog {
                            at: now_str(),
                            agent_id: GLOBAL_MEMORY_OWNER.into(),
                            kind: "memory".into(),
                            state: "stored".into(),
                            detail: format!("整理完成：写入 {count} 条记忆"),
                        });
                        break;
                    }
                    Some(Err(_)) => {
                        last_error = format!(
                            "记忆模型在 {} 秒内未完成",
                            ORGANIZE_ONE_CONVERSATION_TIMEOUT.as_secs()
                        )
                    }
                    Some(Ok(Err(error))) => last_error = error,
                }
                if retry < 3 && !run.pause_before_retry().await {
                    result.cancelled = true;
                    break 'batches;
                }
            }
            if !stored {
                telemetry.set_l1_failure(&conversation.event_key, &last_error)?;
                result.failed += 1;
                let reason = format!("{}：{}", conversation.event_key, last_error);
                result.failure_reasons.push(reason.clone());
                ingest.log(IngestLog {
                    at: now_str(),
                    agent_id: GLOBAL_MEMORY_OWNER.into(),
                    kind: "memory".into(),
                    state: "retrying".into(),
                    detail: format!("整理失败，已重试 3 次：{reason}"),
                });
            }
        }
    }
    result.cancelled |= run.cancelled();
    ingest.log(IngestLog {
        at: now_str(),
        agent_id: GLOBAL_MEMORY_OWNER.into(),
        kind: "memory".into(),
        state: if result.cancelled { "cancelled" } else { "stored" }.into(),
        detail: format!(
            "整理队列{}：成功 {}，失败 {}",
            if result.cancelled { "已中断" } else { "结束" }, result.succeeded, result.failed
        ),
    });
    Ok(result)
}

/// 「待提取记忆」面板的会话列表：已完成但尚未成功提炼的会话，
/// 只带开头摘要，不返回完整对话正文。
#[tauri::command]
pub fn memory_pending_l1_sessions(
    limit: Option<u32>,
) -> Result<Vec<crate::telemetry_store::PendingMemorySession>, String> {
    let telemetry = crate::telemetry_store::shared_store().ok_or("本地对话账本尚未初始化")?;
    telemetry.pending_memory_session_list(limit.unwrap_or(200))
}

/// 「已整理对话」面板的会话列表：已成功提炼为记忆的完整会话，
/// 只带开头摘要，不返回完整对话正文。
#[tauri::command]
pub fn memory_organized_l1_sessions(
    limit: Option<u32>,
) -> Result<Vec<crate::telemetry_store::PendingMemorySession>, String> {
    let telemetry = crate::telemetry_store::shared_store().ok_or("本地对话账本尚未初始化")?;
    telemetry.organized_memory_session_list(limit.unwrap_or(200))
}

/// 面板弹窗用的完整 sanitized 对话正文（user/assistant 轮次）。
#[tauri::command]
pub fn memory_l1_conversation_detail(
    event_key: String,
) -> Result<crate::telemetry_store::MemoryConversationDetail, String> {
    let telemetry = crate::telemetry_store::shared_store().ok_or("本地对话账本尚未初始化")?;
    telemetry
        .conversation_detail_by_event_key(&event_key)?
        .ok_or_else(|| "未找到该会话的完整对话记录".to_string())
}

/// Organize a single staged conversation from the pending panel.  Extraction
/// still goes through the configured memory model; nothing is written when
/// the model is unavailable, and the row is marked failed with the reason.
#[tauri::command]
pub async fn memory_ingest_organize_session(
    event_key: String,
) -> Result<OrganizeConversationsResult, String> {
    let run = ManualExtractionGuard::start()?;
    let ingest = ingest_store().ok_or("自动沉淀模块尚未初始化")?;
    let telemetry = crate::telemetry_store::shared_store().ok_or("本地对话账本尚未初始化")?;
    let conversation = telemetry
        .conversation_by_event_key(&event_key)?
        .ok_or("该会话不在待提取列表中（可能已整理完成）")?;
    let mut result = OrganizeConversationsResult {
        attempted: 1,
        succeeded: 0,
        failed: 0,
        failure_reasons: Vec::new(),
        cancelled: false,
    };
    let mut last_error = String::new();
    let mut stored = false;
    for retry in 0..=1 {
        if run.cancelled() { result.cancelled = true; break; }
        ingest.log(IngestLog {
            at: now_str(),
            agent_id: GLOBAL_MEMORY_OWNER.into(),
            kind: "memory".into(),
            state: "working".into(),
            detail: format!("正在提取单个会话要点（第 {}/2 次）", retry + 1),
        });
        match run.extract(&conversation.conversation_text).await {
            None => { result.cancelled = true; break; }
            Some(Ok(Ok(candidates))) => {
                if run.cancelled() { result.cancelled = true; break; }
                let count =
                    telemetry.store_typed_l1_memories(&conversation.event_key, &candidates)?;
                crate::memory_backend::queue_semantic_l1_index(
                    candidates.iter().map(|item| item.content.clone()).collect(),
                );
                result.succeeded = 1;
                stored = true;
                ingest.log(IngestLog {
                    at: now_str(),
                    agent_id: GLOBAL_MEMORY_OWNER.into(),
                    kind: "memory".into(),
                    state: "stored".into(),
                    detail: format!("单会话整理完成：写入 {count} 条记忆"),
                });
                break;
            }
            Some(Err(_)) => {
                last_error = format!(
                    "记忆模型在 {} 秒内未完成",
                    ORGANIZE_ONE_CONVERSATION_TIMEOUT.as_secs()
                )
            }
            Some(Ok(Err(error))) => last_error = error,
        }
        if retry < 1 && !run.pause_before_retry().await {
            result.cancelled = true;
            break;
        }
    }
    result.cancelled = !stored && (result.cancelled || run.cancelled());
    if result.cancelled {
        ingest.log(IngestLog {
            at: now_str(),
            agent_id: GLOBAL_MEMORY_OWNER.into(),
            kind: "memory".into(),
            state: "cancelled".into(),
            detail: "单会话记忆提取已中断".into(),
        });
    }
    if !stored && !result.cancelled {
        telemetry.set_l1_failure(&conversation.event_key, &last_error)?;
        result.failed = 1;
        result
            .failure_reasons
            .push(format!("{}：{}", conversation.event_key, last_error));
    }
    Ok(result)
}

/// Native L1 storage is the reliable source of completed-conversation memory.
/// The optional semantic service remains available for legacy/hand-written
/// items, but its failure must never make captured conversations disappear.
#[tauri::command]
pub async fn local_memory_list(
    telemetry: tauri::State<'_, crate::telemetry_store::TelemetryStore>,
    limit: Option<u32>,
) -> Result<Vec<crate::telemetry_store::LocalMemory>, String> {
    telemetry.local_l1_memories(limit.unwrap_or(200))
}

#[tauri::command]
pub async fn local_memory_stats(
    telemetry: tauri::State<'_, crate::telemetry_store::TelemetryStore>,
) -> Result<crate::telemetry_store::LocalMemoryStats, String> {
    telemetry.local_l1_memory_stats()
}

/// Explicit, user-initiated L1 rebuild.  This never deletes L0 transcripts;
/// it only clears derived layers and requeues completed conversations.
#[tauri::command]
pub fn local_memory_reset_for_reextraction(
    telemetry: tauri::State<'_, crate::telemetry_store::TelemetryStore>,
) -> Result<crate::telemetry_store::L1ResetResult, String> {
    telemetry.reset_l1_for_reextraction()
}

#[tauri::command]
pub fn local_memory_search(
    telemetry: tauri::State<'_, crate::telemetry_store::TelemetryStore>,
    query: String,
    limit: Option<u32>,
) -> Result<Vec<crate::telemetry_store::LocalMemory>, String> {
    telemetry.search_local_l1_memories(&query, limit.unwrap_or(50))
}

#[tauri::command]
pub fn local_memory_update(
    telemetry: tauri::State<'_, crate::telemetry_store::TelemetryStore>,
    id: String,
    content: String,
) -> Result<(), String> {
    telemetry.update_local_l1_memory(&id, &content)
}

#[tauri::command]
pub fn local_memory_delete(
    telemetry: tauri::State<'_, crate::telemetry_store::TelemetryStore>,
    id: String,
) -> Result<(), String> {
    telemetry.delete_local_l1_memory(&id)
}

/// 用户手动添加自定义长期记忆：固定 long_term 并标记 user_defined，
/// 自动整理与 L1 重建都会跳过它，只有用户能在面板删改；scope 指定
/// 归属层级（l2 工作记忆 / l3 长期 Profile，默认 l3），两层相互独立。
#[tauri::command]
pub fn local_memory_add_user(
    telemetry: tauri::State<'_, crate::telemetry_store::TelemetryStore>,
    content: String,
    memory_type: Option<String>,
    scope: Option<String>,
) -> Result<crate::telemetry_store::LocalMemory, String> {
    telemetry.add_user_defined_l1_memory(
        &content,
        memory_type.as_deref().unwrap_or("fact"),
        scope.as_deref().unwrap_or("l3"),
    )
}

// ────────────────────────────────────────────────────────────────────────────
// 节流巡检：定期冲刷静默超时的会话（由 lib.rs 启动的定时任务调用）
// ────────────────────────────────────────────────────────────────────────────
pub fn start_idle_flusher(backend: Arc<MemoryBackend>, store: IngestStore) {
    tauri::async_runtime::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_secs(30)).await;
            let idle = Duration::from_secs(store.idle_timeout_secs);
            let stale: Vec<String> = {
                let inner = store.inner.lock().unwrap();
                inner
                    .sessions
                    .iter()
                    .filter(|(_, b)| b.last_active.elapsed() > idle)
                    .map(|(k, _)| k.clone())
                    .collect()
            };
            for sid in stale {
                store.flush_session(Arc::clone(&backend), &sid);
            }
        }
    });
}

// ── L2/L3 后台自动重算 ─────────────────────────────────────────────────────

/// app_settings 键。取值是 AutoRefreshConfig 对象；早期版本存过布尔开关，
/// 解析时按「enabled」迁移。
const AUTO_REFRESH_SETTING_KEY: &str = "memory_auto_refresh_l2_l3";
/// 失败后的重试退避：不必等满最小间隔，1 小时后再试一次。
const REFRESH_RETRY_BACKOFF: Duration = Duration::from_secs(3600);
/// 检查周期：半小时醒一次看条件是否满足。
const REFRESH_TICK: Duration = Duration::from_secs(1800);
/// 启动延迟：先让 L1 整理队列和遥测回补跑完，避免与启动高峰抢模型。
const REFRESH_STARTUP_DELAY: Duration = Duration::from_secs(600);

/// 定时重算设置：开关 + 两层各自的重建最小间隔。默认 L2 7 天、L3 1 个月
/// （长期画像稳定，间隔比 L2 长），设置页可改。
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct AutoRefreshConfig {
    pub enabled: bool,
    pub l2_days: u32,
    pub l3_days: u32,
}

impl Default for AutoRefreshConfig {
    fn default() -> Self {
        AutoRefreshConfig { enabled: true, l2_days: 7, l3_days: 30 }
    }
}

/// 纯函数：兼容布尔旧值 / 部分字段 / 越界值，越界一律收敛到允许区间。
fn parse_auto_refresh_config(raw: Option<Value>) -> AutoRefreshConfig {
    let mut config = AutoRefreshConfig::default();
    match raw {
        // 旧版只存布尔开关：保留开关语义，间隔取默认。
        Some(Value::Bool(enabled)) => config.enabled = enabled,
        Some(value) => {
            if let Ok(parsed) = serde_json::from_value::<AutoRefreshConfig>(value) {
                config = parsed;
            }
        }
        None => {}
    }
    config.l2_days = config.l2_days.clamp(1, 90);
    config.l3_days = config.l3_days.clamp(1, 365);
    config
}

/// 手动整理与后台调度共用一把互斥闸：L2 的 map/reduce 是多轮模型调用，
/// 一次可能持续数分钟，重入只会重复烧钱并互相覆盖文档。
static LAYER_REFRESH_RUNNING: AtomicBool = AtomicBool::new(false);

fn begin_layer_refresh() -> Result<(), String> {
    LAYER_REFRESH_RUNNING
        .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
        .map(|_| ())
        .map_err(|_| "已有 L2/L3 整理任务在进行中，请稍后再试".to_string())
}

fn try_begin_layer_refresh() -> bool {
    LAYER_REFRESH_RUNNING
        .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
        .is_ok()
}

fn end_layer_refresh() {
    LAYER_REFRESH_RUNNING.store(false, Ordering::Release);
}

fn parse_doc_time(value: &str) -> Option<chrono::DateTime<chrono::Utc>> {
    chrono::DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|time| time.with_timezone(&chrono::Utc))
}

/// L2 是否到期：文档缺失→到期；否则需同时满足「距上次达到设置间隔」与
/// 「期间有更新的 L1 证据」——没有新内容的定时重跑只会原样复述旧文档。
fn l2_refresh_due(
    doc_time: Option<chrono::DateTime<chrono::Utc>>,
    now: chrono::DateTime<chrono::Utc>,
    has_newer_evidence: bool,
    min_interval: chrono::Duration,
) -> bool {
    match doc_time {
        None => true,
        Some(time) => has_newer_evidence && now - time >= min_interval,
    }
}

/// L3 是否到期：没有已发布 L3→到期（前提是有 L2）；否则需 L2 比 L3 新
/// （画像未覆盖当前 L2 内容）且距上次发布达到设置间隔。
fn l3_refresh_due(
    l2_time: Option<chrono::DateTime<chrono::Utc>>,
    l3_time: Option<chrono::DateTime<chrono::Utc>>,
    now: chrono::DateTime<chrono::Utc>,
    min_interval: chrono::Duration,
) -> bool {
    let Some(l2_time) = l2_time else { return false };
    match l3_time {
        None => true,
        Some(time) => l2_time > time && now - time >= min_interval,
    }
}

#[tauri::command]
pub fn memory_auto_refresh_get(
    telemetry: tauri::State<'_, crate::telemetry_store::TelemetryStore>,
) -> Result<AutoRefreshConfig, String> {
    Ok(parse_auto_refresh_config(
        telemetry.app_setting_get::<Value>(AUTO_REFRESH_SETTING_KEY),
    ))
}

#[tauri::command]
pub fn memory_auto_refresh_set(
    telemetry: tauri::State<'_, crate::telemetry_store::TelemetryStore>,
    enabled: bool,
    l2_days: u32,
    l3_days: u32,
) -> Result<AutoRefreshConfig, String> {
    // 越界值收敛后回传，设置页据此显示生效值。
    let config = AutoRefreshConfig {
        enabled,
        l2_days: l2_days.clamp(1, 90),
        l3_days: l3_days.clamp(1, 365),
    };
    telemetry.app_setting_set(AUTO_REFRESH_SETTING_KEY, &config)?;
    Ok(config)
}

fn scheduler_log(state: &str, detail: String) {
    if let Some(ingest) = ingest_store() {
        ingest.log(IngestLog {
            at: now_str(),
            agent_id: GLOBAL_MEMORY_OWNER.into(),
            kind: "memory".into(),
            state: state.into(),
            detail,
        });
    }
}

/// 后台自动重算：L2 有新 L1 证据且距上次文档达到设置间隔（默认 7 天）时
/// 重建；L2 更新过且距上次发布达到设置间隔（默认 30 天）时重建草案并
/// **自动发布**（注入用文档是发布版，不发布等于没更新）。间隔可在设置页
/// 调整，每轮 tick 现读现用，改完无需重启。所有动作都写进注入页的记录
/// 流，前端经 `memory-layers-updated` 事件刷新。
pub fn start_l2_l3_refresh_scheduler(app: tauri::AppHandle) {
    use tauri::Emitter;
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(REFRESH_STARTUP_DELAY).await;
        let mut next_l2_attempt = Instant::now();
        let mut next_l3_attempt = Instant::now();
        loop {
            if let Some(store) = crate::telemetry_store::shared_store() {
                let config = parse_auto_refresh_config(
                    store.app_setting_get::<Value>(AUTO_REFRESH_SETTING_KEY),
                );
                let model_ready = llm::memory_extraction_provider().is_ok();
                if !config.enabled || !model_ready || !try_begin_layer_refresh() {
                    tokio::time::sleep(REFRESH_TICK).await;
                    continue;
                }
                let l2_interval = chrono::Duration::days(config.l2_days as i64);
                let l3_interval = chrono::Duration::days(config.l3_days as i64);
                let now = Instant::now();
                if now >= next_l2_attempt {
                    match refresh_l2_if_due(&store, l2_interval).await {
                        Ok(Some(document)) => {
                            scheduler_log(
                                "stored",
                                format!("后台自动重算 L2：{}（{} 条来源）", document.created_at, document.source_count),
                            );
                            let _ = app.emit("memory-layers-updated", ());
                        }
                        Ok(None) => {}
                        Err(error) => {
                            next_l2_attempt = Instant::now() + REFRESH_RETRY_BACKOFF;
                            scheduler_log("failed", format!("后台自动重算 L2 失败：{error}"));
                        }
                    }
                }
                let now = Instant::now();
                if now >= next_l3_attempt {
                    match refresh_l3_if_due(&store, l3_interval).await {
                        Ok(true) => {
                            scheduler_log("stored", "后台自动重算 L3：已重建草案并自动发布".into());
                            let _ = app.emit("memory-layers-updated", ());
                        }
                        Ok(false) => {}
                        Err(error) => {
                            next_l3_attempt = Instant::now() + REFRESH_RETRY_BACKOFF;
                            scheduler_log("failed", format!("后台自动重算 L3 失败：{error}"));
                        }
                    }
                }
                end_layer_refresh();
            }
            tokio::time::sleep(REFRESH_TICK).await;
        }
    });
}

/// L2 到期则重算。`Ok(None)` = 未到期或没有新证据（正常跳过，不算失败）。
async fn refresh_l2_if_due(
    store: &crate::telemetry_store::TelemetryStore,
    min_interval: chrono::Duration,
) -> Result<Option<crate::telemetry_store::MemoryLayerDocument>, String> {
    let evidence = store.recent_l1_memories(30, 500)?;
    if evidence.is_empty()
        && store.user_defined_l1_memories_for_scope("l2")?.is_empty()
    {
        return Ok(None);
    }
    let current = store.active_memory_layer_document("l2")?;
    let doc_time = current.as_ref().and_then(|doc| parse_doc_time(&doc.created_at));
    // 仅有新 L1 证据落在当前文档之后才值得重算；否则模型会原样复述旧文档。
    let has_newer = evidence
        .iter()
        .filter_map(|item| parse_doc_time(&item.created_at))
        .any(|created| doc_time.is_none_or(|doc_time| created > doc_time));
    if l2_refresh_due(doc_time, chrono::Utc::now(), has_newer, min_interval) {
        consolidate_l2(store, 30)
            .await
            .map(|result| Some(result.document))
    } else {
        Ok(None)
    }
}

/// L3 到期则重建草案并自动发布。`Ok(false)` = 未到期。
async fn refresh_l3_if_due(
    store: &crate::telemetry_store::TelemetryStore,
    min_interval: chrono::Duration,
) -> Result<bool, String> {
    let Some(l2) = store.active_memory_layer_document("l2")? else {
        return Ok(false);
    };
    let Some(l2_time) = parse_doc_time(&l2.created_at) else {
        return Ok(false);
    };
    let l3 = store.active_memory_layer_document("l3")?;
    let l3_time = l3
        .as_ref()
        .and_then(|doc| {
            doc.published_at
                .as_deref()
                .and_then(parse_doc_time)
                .or_else(|| parse_doc_time(&doc.created_at))
        });
    if !l3_refresh_due(Some(l2_time), l3_time, chrono::Utc::now(), min_interval) {
        return Ok(false);
    }
    let draft = draft_l3(store).await?;
    // 自动发布：注入读的是发布版。草案生成本身以已发布 Profile 为基线，
    // 手工编辑过的条目会被生成提示词默认保留。
    store.publish_memory_layer_document(&draft.document.id, None)?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::{
        filter_l3_profile, has_l2_final_heading, hook_inject_command, hook_prompt_command,
        is_l2_analysis_preamble, is_usable_l3_profile, normalize_l2_document,
        output_language_directive, parse_typed_l1_candidates, read_claude_session,
        read_claude_turn, read_codex_session, read_codex_turn, read_qoder_session,
        read_workbuddy_session, select_l2_evidence, strip_memory_thinking,
        unwrap_markdown_document_fence,
    };

    #[test]
    fn manual_extraction_can_be_cancelled_and_restarted() {
        let first = super::ManualExtractionGuard::start().unwrap();
        assert!(!first.cancelled());
        assert!(super::ManualExtractionGuard::start().is_err());
        assert!(super::memory_ingest_cancel_organize());
        assert!(first.cancelled());
        drop(first);
        assert!(!super::memory_ingest_cancel_organize());
        let second = super::ManualExtractionGuard::start().unwrap();
        assert!(!second.cancelled());
    }

    #[tokio::test]
    async fn manual_extraction_interrupts_pending_request() {
        let run = super::ManualExtractionGuard::start().unwrap();
        let waiting = async {
            run.wait_for_cancel().await;
            run.cancelled()
        };
        assert!(super::memory_ingest_cancel_organize());
        assert!(tokio::time::timeout(std::time::Duration::from_secs(1), waiting).await.unwrap());
        assert!(tokio::time::timeout(
            std::time::Duration::from_secs(1),
            run.extract("cancelled request never reaches the model"),
        ).await.unwrap().is_none());
    }

    #[test]
    fn prompt_hook_combines_sink_and_per_turn_injection() {
        let command = hook_prompt_command("qoder").expect("command must build");
        // 出站沉淀与入站注入必须在同一条命令里，且都带来源标识。
        assert!(command.contains("/memory/hook?source=qoder"));
        assert!(command.contains("/memory/context?source=qoder&event=UserPromptSubmit"));
        assert!(command.contains("X-Agent-Manager-Hook: 1"));
        // 沉淀失败不阻断注入：分隔符串联而非 && 短路。
        assert!(command.contains('&') || command.contains(';'));
    }

    #[test]
    fn session_start_and_prompt_hooks_request_distinct_context_scopes() {
        let session = hook_inject_command("claude");
        let prompt = hook_prompt_command("claude").expect("command must build");
        assert!(session.contains("event=SessionStart"));
        assert!(!session.contains("event=UserPromptSubmit"));
        assert!(prompt.contains("event=UserPromptSubmit"));
    }

    #[test]
    fn assistant_thinking_is_removed_before_l0_and_l1_processing() {
        assert_eq!(
            strip_memory_thinking("assistant", "<think>内部推理</think>最终结论"),
            "最终结论"
        );
        assert_eq!(
            strip_memory_thinking("user", "<think>这是用户原文</think>"),
            "<think>这是用户原文</think>"
        );
    }

    #[test]
    fn layer_refresh_due_predicates_match_scheduler_intent() {
        use super::{l2_refresh_due, l3_refresh_due, parse_doc_time};
        use chrono::DateTime;
        // 库里实际存的两种 rfc3339 形态（7 位与 6 位纳秒）都必须能解析。
        let seven = parse_doc_time("2026-10-04T02:53:24.592798300+00:00").expect("7-digit nanos");
        let six = parse_doc_time("2026-09-21T15:12:36.673637+00:00").expect("6-digit nanos");
        assert_eq!(seven.timestamp() % 60, 24);
        assert_eq!(six.timestamp() % 60, 36);
        assert!(parse_doc_time("garbage").is_none());

        let now = DateTime::parse_from_rfc3339("2026-10-04T12:00:00+00:00")
            .unwrap()
            .with_timezone(&chrono::Utc);
        let day = chrono::Duration::days(1);
        let l2_default = chrono::Duration::days(7);
        let l3_default = chrono::Duration::days(30);

        // L2：没有文档 → 到期（有证据时的前置检查在调用方）。
        assert!(l2_refresh_due(None, now, false, l2_default));
        // 文档年轻（即使有新证据）→ 不到期。
        assert!(!l2_refresh_due(Some(now - chrono::Duration::hours(13)), now, true, l2_default));
        // 到达间隔但没有新证据 → 不到期（不重复烧模型复述旧文档）。
        assert!(!l2_refresh_due(Some(now - l2_default), now, false, l2_default));
        // 到达间隔且有新证据 → 到期。
        assert!(l2_refresh_due(Some(now - l2_default), now, true, l2_default));
        assert!(l2_refresh_due(Some(now - l2_default - day), now, true, l2_default));
        // 无效时间戳按「无文档」处理 → 到期（重算后写回有效时间自愈）。
        assert!(l2_refresh_due(None, now, true, day));

        // L3：没有 L2 → 永不到期。
        assert!(!l3_refresh_due(None, None, now, l3_default));
        // 有 L2 没 L3 → 到期。
        assert!(l3_refresh_due(Some(now - day), None, now, l3_default));
        // L3 比 L2 新（画像已覆盖 L2 内容）→ 不到期，即使过了间隔。
        assert!(!l3_refresh_due(
            Some(now - day),
            Some(now - chrono::Duration::hours(1)),
            now,
            l3_default
        ));
        // L2 比 L3 新但距上次发布未到间隔 → 不到期。
        let old_l3 = now - chrono::Duration::days(10);
        assert!(!l3_refresh_due(Some(now - day), Some(old_l3), now, l3_default));
        // L2 比 L3 新且到达间隔 → 到期。
        assert!(l3_refresh_due(
            Some(now - day),
            Some(now - l3_default - day),
            now,
            l3_default
        ));
        // 间隔改成 7 天后，同一状态即到期：配置即时生效。
        assert!(l3_refresh_due(Some(now - day), Some(old_l3), now, chrono::Duration::days(7)));
    }

    #[test]
    fn auto_refresh_config_parsing_covers_defaults_migration_and_clamps() {
        use super::{parse_auto_refresh_config, AutoRefreshConfig};
        // 无存储值 → 默认：开、L2 7 天、L3 30 天。
        assert_eq!(
            parse_auto_refresh_config(None),
            AutoRefreshConfig { enabled: true, l2_days: 7, l3_days: 30 }
        );
        // 旧版布尔开关：只迁移 enabled，间隔用默认。
        assert_eq!(
            parse_auto_refresh_config(Some(serde_json::json!(false))),
            AutoRefreshConfig { enabled: false, l2_days: 7, l3_days: 30 }
        );
        // 完整对象原样读取。
        assert_eq!(
            parse_auto_refresh_config(Some(serde_json::json!({
                "enabled": false, "l2_days": 3, "l3_days": 90
            }))),
            AutoRefreshConfig { enabled: false, l2_days: 3, l3_days: 90 }
        );
        // 越界与损坏值收敛：间隔夹到允许区间，垃圾 JSON 回退默认。
        assert_eq!(parse_auto_refresh_config(Some(serde_json::json!({
            "enabled": true, "l2_days": 0, "l3_days": 9999
        }))).l2_days, 1);
        assert_eq!(parse_auto_refresh_config(Some(serde_json::json!({
            "enabled": true, "l2_days": 0, "l3_days": 9999
        }))).l3_days, 365);
        assert_eq!(
            parse_auto_refresh_config(Some(serde_json::json!("garbage"))),
            AutoRefreshConfig { enabled: true, l2_days: 7, l3_days: 30 }
        );
    }

    #[test]
    fn typed_l1_parser_preserves_the_model_type() {
        let memories = parse_typed_l1_candidates(
            r#"{"memories":[{"content":"会话摘要","type":"summary","durability":"session"},{"content":"用户要求中文输出","type":"preference_candidate","durability":"long_term"}]}"#,
        ).unwrap();
        assert_eq!(memories.len(), 2);
        assert_eq!(memories[1].memory_type, "preference_candidate");
        assert_eq!(memories[1].durability, "long_term");
    }

    #[test]
    fn typed_l1_parser_repairs_common_contract_drift() {
        // summary 不在首位 → 移到首位。
        let memories = parse_typed_l1_candidates(
            r#"{"memories":[{"content":"事实","type":"fact","durability":"short_term"},{"content":"摘要","type":"summary","durability":"session"}]}"#,
        ).unwrap();
        assert_eq!(memories[0].memory_type, "summary");
        // 超过 3 条 → 截断为 summary + 2 条。
        let memories = parse_typed_l1_candidates(
            r#"{"memories":[
                {"content":"a","type":"fact","durability":"short_term"},
                {"content":"b","type":"fact","durability":"short_term"},
                {"content":"c","type":"fact","durability":"short_term"},
                {"content":"s","type":"summary","durability":"session"}]}"#,
        )
        .unwrap();
        assert_eq!(memories.len(), 3);
        assert_eq!(memories[0].memory_type, "summary");
        // undetermined / 缺失 durability → 保守 short_term（summary 恒为 session）。
        let memories = parse_typed_l1_candidates(
            r#"{"memories":[{"content":"摘要","type":"summary","durability":"undetermined"},{"content":"事实","type":"fact"}]}"#,
        ).unwrap();
        assert_eq!(memories[0].durability, "session");
        assert_eq!(memories[1].durability, "short_term");
        // 别名归一化。
        let memories = parse_typed_l1_candidates(
            r#"{"memories":[{"content":"长期偏好","type":"preference_candidate","durability":"Long-Term"}]}"#,
        ).unwrap();
        assert_eq!(memories[0].durability, "long_term");
    }

    #[test]
    fn typed_l1_parser_tolerates_wrapped_or_bare_array_output() {
        // 前后带说明文字 + 代码块。
        let memories = parse_typed_l1_candidates(
            "整理结果如下：\n```json\n{\"memories\":[{\"content\":\"摘要\",\"type\":\"summary\",\"durability\":\"session\"}]}\n```\n以上。",
        ).unwrap();
        assert_eq!(memories.len(), 1);
        // 裸数组。
        let memories = parse_typed_l1_candidates(
            r#"[{"content":"摘要","type":"summary","durability":"session"}]"#,
        )
        .unwrap();
        assert_eq!(memories.len(), 1);
        // 完全无法解析 → 报错。
        assert!(parse_typed_l1_candidates("我认为这次会话没有值得记录的内容。").is_err());
    }

    #[test]
    fn typed_l1_parser_accepts_memory_singular_key() {
        // 模型偶发把键名写成单数 `memory`；与 `memories` 同义，不应整批拒绝。
        let memories = parse_typed_l1_candidates(
            r#"{"memory":[{"content":"摘要","type":"summary","durability":"session"},{"content":"事实","type":"fact","durability":"short_term"}]}"#,
        )
        .unwrap();
        assert_eq!(memories.len(), 2);
        assert_eq!(memories[0].memory_type, "summary");
        // 包在说明文字里的单数键同样要能救回来。
        let memories = parse_typed_l1_candidates(
            "好的，以下是整理结果：{\"memory\":[{\"content\":\"摘要\",\"type\":\"summary\",\"durability\":\"session\"}]} 希望有帮助",
        )
        .unwrap();
        assert_eq!(memories.len(), 1);
    }

    #[test]
    fn typed_l1_parser_recovers_truncated_json() {
        // MiniMax-M3 实测失败形态：输出在 max_tokens 或服务端处被截断，
        // JSON 断在字符串中间，括号不闭合。完整的元素必须被救回。
        let full = r#"{"memories":[{"content":"本地与云端长期记忆同步漏拉的根因已定位并修复：旧拉取算法把最大 revision 当作全局游标","type":"summary","durability":"session"},{"content":"拉取游标改为按设备分别记录","type":"decision","durability":"short_term"}]}"#;
        // 断在第 2 个元素的 content 字符串中间。
        let truncated = &full[..full.find("按设备").unwrap() + 6];
        let memories = parse_typed_l1_candidates(truncated).unwrap();
        assert_eq!(memories.len(), 1);
        assert_eq!(memories[0].memory_type, "summary");
        assert!(memories[0].content.contains("旧拉取算法"));
        // 断在第 1 个元素里 → 无完整元素，仍报错（错误里带原文片段）。
        let head = &full[..full.find("旧拉取算法").unwrap()];
        let error = match parse_typed_l1_candidates(head) {
            Err(error) => error,
            Ok(_) => panic!("fully truncated output must fail"),
        };
        assert!(error.contains("输出开头"), "got: {error}");
    }

    #[test]
    fn contract_failure_error_carries_output_excerpt() {
        // 失败原因里必须带模型原文片段：l1_error_detail 只存错误消息时，
        // 无法事后定位模型实际输出了什么。
        let error = match parse_typed_l1_candidates("这段对话只是一次性的 Docker 排查，没有需要沉淀的内容。") {
            Err(error) => error,
            Ok(_) => panic!("prose output must fail the contract"),
        };
        assert!(
            error.contains("Docker 排查"),
            "error should quote the model output, got: {error}"
        );
        // 换行压平 + 超长截断，保证单条错误不超过存储上限太多。
        let long = format!("{}\n{}", "很长的说明。".repeat(40), "结尾");
        let error = match parse_typed_l1_candidates(&long) {
            Err(error) => error,
            Ok(_) => panic!("long prose must fail"),
        };
        assert!(!error.contains('\n'), "excerpt must be flattened to one line");
        assert!(error.chars().count() < 300, "error stays bounded, got {} chars", error.chars().count());
        // 数组解析成功但没有任何可用条目时同样附原文。
        let error = match parse_typed_l1_candidates(r#"{"memories":[{"content":"","type":"summary"}]}"#) {
            Err(error) => error,
            Ok(_) => panic!("no usable items must fail"),
        };
        assert!(error.contains("输出开头"), "empty-candidates error should quote output, got: {error}");
    }

    #[test]
    fn l2_selection_retains_critical_evidence_before_session_cap() {
        let row = |id: &str, memory_type: &str, session_id: &str| {
            crate::telemetry_store::LocalMemorySnapshot {
                id: id.into(),
                source_event_key: "event".into(),
                ordinal: 0,
                memory: id.into(),
                memory_type: memory_type.into(),
                durability: "short_term".into(),
                source: "codex".into(),
                session_id: session_id.into(),
                user_defined: false,
                event_time: Some("2026-08-16T00:00:00Z".into()),
                created_at: "2026-08-16T00:00:00Z".into(),
                updated_at: "2026-08-16T00:00:00Z".into(),
            }
        };
        let output = select_l2_evidence(vec![
            row("a", "fact", "s1"),
            row("b", "fact", "s1"),
            row("c", "fact", "s1"),
            row("d", "constraint", "s1"),
        ]);
        assert!(output.iter().any(|item| item.id == "d"));
        assert!(output.len() <= 3);
    }

    #[test]
    fn l2_rejects_task_narration_but_keeps_final_memory_markdown() {
        assert!(is_l2_analysis_preamble("The user wants me to create an L2 short-term working memory from the provided map summaries."));
        assert!(is_l2_analysis_preamble("Let me analyze the three sources:"));
        assert!(!is_l2_analysis_preamble(
            "## Current focus\n\n- 完成 L2 记忆整理 [l1:123]"
        ));
    }

    #[test]
    fn l2_discards_untagged_reasoning_before_the_final_heading() {
        let model_output = "The user wants me to create an L2 memory. Let me analyze the sources.\n\n## Current focus\nThis is only the planned outline.\n\nLet me draft it now.\n\n## 当前聚焦\n- 修复记忆提取 [l1:1]";
        let document = normalize_l2_document(model_output);
        assert_eq!(document, "## 当前聚焦\n- 修复记忆提取 [l1:1]");
        assert!(has_l2_final_heading(&document));
        assert!(!is_l2_analysis_preamble(&document));
    }

    #[test]
    fn unwraps_a_fence_that_wraps_an_entire_memory_document() {
        assert_eq!(
            unwrap_markdown_document_fence("```markdown\n# Profile\n\n- 使用 SQLite\n```"),
            "# Profile\n\n- 使用 SQLite",
        );
    }

    #[test]
    fn l3_rejects_truncated_or_wrong_language_profiles() {
        let chinese_evidence = "用户偏好中文，长期使用 SQLite";
        assert!(!is_usable_l3_profile(
            "## Collaboration\n- Use SQLite\n[内容已截断]",
            chinese_evidence
        ));
        assert!(!is_usable_l3_profile(
            "## Collaboration\n- Use SQLite as the durable ledger.",
            chinese_evidence
        ));
        assert!(is_usable_l3_profile(
            "## 协作偏好\n- 默认使用中文沟通。",
            chinese_evidence
        ));
        assert!(!is_usable_l3_profile(
            "- 默认使用中文沟通。",
            chinese_evidence
        ));
    }

    #[test]
    fn l3_accepts_a_complete_final_body_within_its_persisted_limit() {
        let chinese_evidence = "用户偏好中文，长期使用 SQLite";
        let complete_profile = format!("## 协作偏好\n- {}", "持久偏好。".repeat(900));
        assert!(is_usable_l3_profile(&complete_profile, chinese_evidence));
    }

    #[test]
    fn l3_removes_volatile_project_details_before_persistence() {
        let profile = "## 模型与端点\n- MiniMax 默认端点 api.minimaxi.com。\n## 长期约束\n- 助手只能通过本地项目和数据库进行诊断。\n## 代码约定\n- i18n 键必须同时维护。";
        assert_eq!(
            filter_l3_profile(profile),
            "## 长期约束\n- 助手只能通过本地项目和数据库进行诊断。"
        );
    }

    #[test]
    fn memory_output_language_follows_the_captured_conversation() {
        assert!(output_language_directive("请把记忆整理成中文").contains("中文"));
        assert!(
            output_language_directive("Please retain this project decision").contains("English")
        );
        assert!(output_language_directive("用户偏好中文并使用 SQLite MCP Server").contains("中文"));
    }

    #[test]
    fn claude_transcript_reader_uses_one_prompt_turn_and_skips_tool_result_rows() {
        let path = std::env::temp_dir().join(format!(
            "agent-manager-transcript-{}.jsonl",
            uuid::Uuid::new_v4()
        ));
        let rows = [
            serde_json::json!({"type":"user", "promptId":"p-1", "message":{"role":"user", "content":"remember this project decision"}}),
            serde_json::json!({"type":"assistant", "message":{"role":"assistant", "content":[{"type":"text", "text":"I will use SQLite."}]}}),
            serde_json::json!({"type":"user", "sourceToolAssistantUUID":"tool-1", "message":{"role":"user", "content":[{"type":"tool_result", "content":"large output"}]}}),
            serde_json::json!({"type":"assistant", "message":{"role":"assistant", "content":[{"type":"text", "text":"The migration is complete."}]}}),
            serde_json::json!({"type":"user", "promptId":"p-2", "message":{"role":"user", "content":"a different turn"}}),
        ];
        std::fs::write(
            &path,
            rows.iter()
                .map(serde_json::Value::to_string)
                .collect::<Vec<_>>()
                .join("\n"),
        )
        .unwrap();
        assert_eq!(
            read_claude_turn(&path, "p-1").unwrap(),
            vec![
                ("user".into(), "remember this project decision".into()),
                ("assistant".into(), "I will use SQLite.".into()),
                ("assistant".into(), "The migration is complete.".into()),
            ]
        );
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn codex_transcript_reader_uses_only_the_hook_turn_dialogue() {
        let path = std::env::temp_dir().join(format!(
            "agent-manager-codex-transcript-{}.jsonl",
            uuid::Uuid::new_v4()
        ));
        let rows = [
            serde_json::json!({"type":"event_msg", "payload":{"type":"task_started", "turn_id":"turn-1"}}),
            serde_json::json!({"type":"response_item", "payload":{"type":"message", "role":"user", "content":[{"type":"input_text", "text":"Persist this decision"}], "internal_chat_message_metadata_passthrough":{"turn_id":"turn-1"}}}),
            serde_json::json!({"type":"response_item", "payload":{"type":"function_call", "name":"shell", "arguments":"secret tool details", "internal_chat_message_metadata_passthrough":{"turn_id":"turn-1"}}}),
            serde_json::json!({"type":"response_item", "payload":{"type":"message", "role":"assistant", "content":[{"type":"output_text", "text":"I will use the shared skill library."}], "internal_chat_message_metadata_passthrough":{"turn_id":"turn-1"}}}),
            serde_json::json!({"type":"response_item", "payload":{"type":"message", "role":"user", "content":[{"type":"input_text", "text":"Different request"}], "internal_chat_message_metadata_passthrough":{"turn_id":"turn-2"}}}),
        ];
        std::fs::write(
            &path,
            rows.iter()
                .map(serde_json::Value::to_string)
                .collect::<Vec<_>>()
                .join("\n"),
        )
        .unwrap();
        assert_eq!(
            read_codex_turn(&path, "turn-1").unwrap(),
            vec![
                ("user".into(), "Persist this decision".into()),
                (
                    "assistant".into(),
                    "I will use the shared skill library.".into()
                ),
            ]
        );
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn qoder_native_reader_keeps_dialogue_and_skips_non_dialogue_rows() {
        let path = std::env::temp_dir().join(format!(
            "agent-manager-qoder-transcript-{}.jsonl",
            uuid::Uuid::new_v4()
        ));
        let rows = [
            serde_json::json!({"type":"user", "uuid":"u-1", "message":{"content":[{"content":"记住本项目使用 SQLite"}]}}),
            serde_json::json!({"type":"tool", "uuid":"tool-1", "message":{"content":"secret tool payload"}}),
            serde_json::json!({"type":"assistant", "uuid":"a-1", "message":{"content":[{"content":"已记录并会写入本地账本。"}]}}),
            serde_json::json!({"type":"assistant", "uuid":"a-1", "message":{"content":[{"content":"重复行"}]}}),
        ];
        std::fs::write(
            &path,
            rows.iter()
                .map(serde_json::Value::to_string)
                .collect::<Vec<_>>()
                .join("\n"),
        )
        .unwrap();
        assert_eq!(
            read_qoder_session(&path).unwrap(),
            vec![
                ("user".into(), "记住本项目使用 SQLite".into()),
                ("assistant".into(), "已记录并会写入本地账本。".into()),
            ]
        );
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn native_session_readers_extract_only_dialogue_across_agent_formats() {
        let claude = std::env::temp_dir().join(format!(
            "agent-manager-claude-session-{}.jsonl",
            uuid::Uuid::new_v4()
        ));
        std::fs::write(&claude, [
            serde_json::json!({"type":"user","uuid":"u-1","message":{"role":"user","content":"保留这个决定"}}).to_string(),
            serde_json::json!({"type":"assistant","uuid":"a-1","message":{"role":"assistant","content":[{"type":"text","text":"使用 SQLite"}]}}).to_string(),
            serde_json::json!({"type":"user","sourceToolAssistantUUID":"tool-1","message":{"role":"user","content":"工具回显"}}).to_string(),
        ].join("\n")).unwrap();
        assert_eq!(read_claude_session(&claude).unwrap().len(), 2);

        let codex = std::env::temp_dir().join(format!(
            "agent-manager-codex-session-{}.jsonl",
            uuid::Uuid::new_v4()
        ));
        std::fs::write(&codex, [
            serde_json::json!({"type":"response_item","payload":{"type":"message","id":"u-1","role":"user","content":[{"type":"input_text","text":"记住配置"}]}}).to_string(),
            serde_json::json!({"type":"response_item","payload":{"type":"function_call","id":"tool-1","arguments":"secret"}}).to_string(),
            serde_json::json!({"type":"response_item","payload":{"type":"message","id":"a-1","role":"assistant","content":[{"type":"output_text","text":"已完成"}]}}).to_string(),
        ].join("\n")).unwrap();
        assert_eq!(read_codex_session(&codex).unwrap().len(), 2);

        let workbuddy = std::env::temp_dir().join(format!(
            "agent-manager-workbuddy-session-{}.jsonl",
            uuid::Uuid::new_v4()
        ));
        std::fs::write(&workbuddy, [
            serde_json::json!({"id":"u-1","type":"user","message":{"content":"讨论需求"}}).to_string(),
            serde_json::json!({"id":"tool-1","type":"tool","message":{"content":"工具数据"}}).to_string(),
            serde_json::json!({"id":"a-1","type":"assistant","message":{"content":[{"text":"给出方案"}]}}).to_string(),
        ].join("\n")).unwrap();
        assert_eq!(read_workbuddy_session(&workbuddy).unwrap().len(), 2);

        for path in [claude, codex, workbuddy] {
            let _ = std::fs::remove_file(path);
        }
    }

    #[test]
    fn qoder_reader_drops_tool_calls_results_and_thinking_blocks() {
        let path = std::env::temp_dir().join(format!(
            "agent-manager-qoder-clean-{}.jsonl",
            uuid::Uuid::new_v4()
        ));
        let rows = [
            // 真实用户消息：纯字符串 content。
            serde_json::json!({"type":"user", "uuid":"u-1", "message":{"role":"user", "content":"记忆中心只保留对话"}}),
            // 工具调用：type=tool_use，携带完整参数 JSON。
            serde_json::json!({"type":"assistant", "uuid":"a-1", "message":{"role":"assistant", "content":[{"type":"tool_use", "id":"call_1", "name":"Bash", "input":{"command":"secret command"}}]}}),
            // 助手思考块。
            serde_json::json!({"type":"assistant", "uuid":"a-2", "message":{"role":"assistant", "content":[{"type":"thinking", "thinking":"secret reasoning"}]}}),
            // 助手正文。
            serde_json::json!({"type":"assistant", "uuid":"a-3", "message":{"role":"assistant", "content":[{"type":"text", "text":"好的，只保留用户与回复。"}]}}),
            // 工具结果：伪装成 user，带 tool_use_id / type=tool_result。
            serde_json::json!({"type":"user", "uuid":"u-2", "message":{"role":"user", "content":[{"type":"tool_result", "tool_use_id":"call_1", "is_error":false, "content":"Command completed. secret output"}]}}),
        ];
        std::fs::write(
            &path,
            rows.iter()
                .map(serde_json::Value::to_string)
                .collect::<Vec<_>>()
                .join("\n"),
        )
        .unwrap();
        assert_eq!(
            read_qoder_session(&path).unwrap(),
            vec![
                ("user".into(), "记忆中心只保留对话".into()),
                ("assistant".into(), "好的，只保留用户与回复。".into()),
            ]
        );
        let _ = std::fs::remove_file(path);
    }
}
