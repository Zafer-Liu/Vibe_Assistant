use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use crate::thinking::strip_thinking_blocks;
use aes_gcm::aead::{Aead, AeadCore, Key, KeyInit, OsRng};
use aes_gcm::{Aes256Gcm, Nonce};
use sha2::{Digest, Sha256};

/// 基于应用标识 + 机器名派生固定 32 字节 AES-256 密钥
fn derive_key() -> [u8; 32] {
    let machine_id = std::env::var("COMPUTERNAME")
        .or_else(|_| std::env::var("HOSTNAME"))
        .unwrap_or_else(|_| "agent-manager".to_string());
    let mut hasher = Sha256::new();
    hasher.update(b"agent-manager-key-v1");
    hasher.update(machine_id.as_bytes());
    let result = hasher.finalize();
    let mut key = [0u8; 32];
    key.copy_from_slice(&result);
    key
}

/// 将字节数组编码为十六进制字符串
fn hex_encode(data: &[u8]) -> String {
    data.iter().map(|b| format!("{:02x}", b)).collect()
}

/// 将十六进制字符串解码为字节数组，失败返回 None
fn hex_decode(s: &str) -> Option<Vec<u8>> {
    if s.len() % 2 != 0 {
        return None;
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).ok())
        .collect()
}

const ENCRYPTED_KEY_PREFIX: &str = "v2:";
const GCM_NONCE_BYTES: usize = 12;
const LLM_PROVIDERS_SETTING_KEY: &str = "llm_providers";
const MEMORY_EXTRACTION_SETTING_KEY: &str = "memory_extraction_config";
const OLLAMA_CONFIG_SETTING_KEY: &str = "ollama_config";

/// Encrypt a key with a fresh GCM nonce.  The nonce is stored alongside the
/// ciphertext so each saved provider is independently decryptable.
pub(crate) fn encrypt_api_key(plain: &str) -> Result<String, String> {
    if plain.is_empty() {
        return Ok(String::new());
    }
    let key = derive_key();
    let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(&key));
    let nonce = Aes256Gcm::generate_nonce(&mut OsRng);
    let ciphertext = cipher
        .encrypt(&nonce, plain.as_bytes())
        .map_err(|_| "无法加密 API Key".to_string())?;
    Ok(format!(
        "{ENCRYPTED_KEY_PREFIX}{}:{}",
        hex_encode(nonce.as_slice()),
        hex_encode(&ciphertext)
    ))
}

/// Decrypt v2 keys.  Legacy values remain plaintext-compatible; malformed v2
/// data becomes empty rather than being exposed as a pretend API key.
pub(crate) fn decrypt_api_key(cipher_text: &str) -> String {
    if cipher_text.is_empty() {
        return String::new();
    }
    let Some(payload) = cipher_text.strip_prefix(ENCRYPTED_KEY_PREFIX) else {
        return cipher_text.to_string();
    };
    let Some((nonce_hex, cipher_hex)) = payload.split_once(':') else {
        return String::new();
    };
    let Some(nonce_bytes) = hex_decode(nonce_hex).filter(|bytes| bytes.len() == GCM_NONCE_BYTES)
    else {
        return String::new();
    };
    let Some(ciphertext) = hex_decode(cipher_hex).filter(|bytes| !bytes.is_empty()) else {
        return String::new();
    };
    let key = derive_key();
    let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(&key));
    let nonce = Nonce::from_slice(&nonce_bytes);
    cipher
        .decrypt(nonce, ciphertext.as_ref())
        .ok()
        .and_then(|bytes| String::from_utf8(bytes).ok())
        .unwrap_or_default()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LlmProvider {
    pub id: String,
    pub name: String,
    pub base_url: String,
    pub model: String,
    pub api_key: String,
    pub is_custom: bool,
    pub enabled: bool,
    pub context_window: Option<u32>,
    pub max_output_tokens: Option<u32>,
    #[serde(skip)]
    pub identity: Option<crate::model_registry::ModelIdentity>,
}

/// The one configured provider used to turn a completed Agent conversation
/// into durable memory candidates.  Credentials stay with the provider record;
/// this config only stores its stable id.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub struct MemoryExtractionConfig {
    pub provider_id: Option<String>,
}

/// Local Ollama service connection settings.  Ollama exposes an
/// OpenAI-compatible endpoint under `/v1`, so a connected local model can be
/// registered as a custom provider without a real API key.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct OllamaConfig {
    pub base_url: String,
}

impl Default for OllamaConfig {
    fn default() -> Self {
        Self {
            base_url: "http://localhost:11434".to_string(),
        }
    }
}

/// One locally installed Ollama model, as reported by `GET /api/tags`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OllamaModelInfo {
    pub name: String,
    pub size: u64,
    pub parameter_size: Option<String>,
    pub quantization_level: Option<String>,
}

/// Result of probing an Ollama service: its version plus installed models.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OllamaTestResult {
    pub version: Option<String>,
    pub models: Vec<OllamaModelInfo>,
}

// 内置 provider 默认值。88API 是项目唯一的内置赞助商入口；模型 ID 不臆测，
// 用户在 88API 控制台按当前可用额度选择后填写。
pub const SPONSOR_PROVIDER_ID: &str = "88api";
pub const SPONSOR_PROVIDER_NAME: &str = "88API";
pub const SPONSOR_BASE_URL: &str = "https://api.88api.ai/v1";
// 预留的赞助商注册入口，当前界面未直接引用。
#[allow(dead_code)]
pub const SPONSOR_CREDITS_URL: &str = "https://88api.ai/sign-up?aff=VNNV";

pub fn builtin_defaults(
) -> HashMap<&'static str, (&'static str, &'static str, &'static str, u32, u32)> {
    // id -> (name, base_url, model, context_window, max_output_tokens)
    let mut m = HashMap::new();
    m.insert(
        SPONSOR_PROVIDER_ID,
        (
            SPONSOR_PROVIDER_NAME,
            SPONSOR_BASE_URL,
            "",
            128000,
            16384,
        ),
    );
    m
}

fn config_path() -> std::path::PathBuf {
    dirs_next::data_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join("agent-manager")
        .join("llm_config.json")
}

fn memory_extraction_config_path() -> std::path::PathBuf {
    dirs_next::data_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join("agent-manager")
        .join("memory_extraction.json")
}

fn ollama_config_path() -> std::path::PathBuf {
    dirs_next::data_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join("agent-manager")
        .join("ollama_config.json")
}

pub fn ollama_config() -> OllamaConfig {
    if let Some(store) = crate::telemetry_store::shared_store() {
        if let Some(config) = store.app_setting_get(OLLAMA_CONFIG_SETTING_KEY) {
            return config;
        }
    }
    let config = std::fs::read_to_string(ollama_config_path())
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default();
    if let Some(store) = crate::telemetry_store::shared_store() {
        let _ = store.app_setting_set(OLLAMA_CONFIG_SETTING_KEY, &config);
    }
    config
}

fn save_ollama_config(config: &OllamaConfig) -> Result<(), String> {
    if let Some(store) = crate::telemetry_store::shared_store() {
        store.app_setting_set(OLLAMA_CONFIG_SETTING_KEY, config)?;
    }
    // Compatibility mirror; SQLite is authoritative once initialized.
    let path = ollama_config_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    let text = serde_json::to_string_pretty(config).map_err(|error| error.to_string())?;
    std::fs::write(path, text).map_err(|error| error.to_string())
}

/// Validate a user-supplied Ollama base URL and strip trailing slashes so
/// endpoint paths can be appended directly.
fn normalize_ollama_base_url(raw: &str) -> Result<String, String> {
    let trimmed = raw.trim().trim_end_matches('/');
    if trimmed.is_empty() {
        return Err("Ollama 服务地址不能为空".to_string());
    }
    if !trimmed.starts_with("http://") && !trimmed.starts_with("https://") {
        return Err("Ollama 服务地址必须以 http:// 或 https:// 开头".to_string());
    }
    Ok(trimmed.to_string())
}

fn normalize_memory_extraction_config(mut config: MemoryExtractionConfig) -> MemoryExtractionConfig {
    // The retired presets were built-ins, not user custom providers. Clear a
    // stale selection rather than silently redirecting it to 88API without an
    // API key and explicit model ID.
    if matches!(config.provider_id.as_deref(), Some("deepseek" | "openai")) {
        config.provider_id = None;
    }
    config
}

pub fn memory_extraction_config() -> MemoryExtractionConfig {
    if let Some(store) = crate::telemetry_store::shared_store() {
        if let Some(config) = store.app_setting_get(MEMORY_EXTRACTION_SETTING_KEY) {
            let normalized = normalize_memory_extraction_config(config);
            let _ = store.app_setting_set(MEMORY_EXTRACTION_SETTING_KEY, &normalized);
            return normalized;
        }
    }
    let config = std::fs::read_to_string(memory_extraction_config_path())
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default();
    let config = normalize_memory_extraction_config(config);
    if let Some(store) = crate::telemetry_store::shared_store() {
        let _ = store.app_setting_set(MEMORY_EXTRACTION_SETTING_KEY, &config);
    }
    config
}

fn save_memory_extraction_config(config: &MemoryExtractionConfig) -> Result<(), String> {
    if let Some(store) = crate::telemetry_store::shared_store() {
        store.app_setting_set(MEMORY_EXTRACTION_SETTING_KEY, config)?;
    }
    // Compatibility mirror for older versions; SQLite is the authoritative
    // location once it has been initialized.
    let path = memory_extraction_config_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    let text = serde_json::to_string_pretty(config).map_err(|error| error.to_string())?;
    std::fs::write(path, text).map_err(|error| error.to_string())
}

/// Sanitized compatibility projection of the authoritative registry.
fn load_providers() -> Result<Vec<LlmProvider>, String> {
    use crate::model_registry::{read_optional, sanitized_bytes, Cleanup, Registry, LLM};
    let registry = Registry::shared()?;
    if !registry.migrated(LLM)? {
        let primary = registry.legacy_setting(LLM_PROVIDERS_SETTING_KEY)?;
        let mirror = read_optional(&config_path())?;
        let chosen = primary
            .clone()
            .or_else(|| mirror.as_ref().map(|(v, _)| v.clone()))
            .unwrap_or_else(|| serde_json::json!([]));
        let mut providers: Vec<LlmProvider> =
            serde_json::from_value(chosen).map_err(|_| "旧模型配置字段损坏，已停止迁移")?;
        remove_retired_builtins(&mut providers);
        let cleanups = mirror
            .as_ref()
            .map(|(v, bytes)| {
                Ok::<Cleanup, String>(Cleanup {
                    path: config_path(),
                    original: bytes.clone(),
                    sanitized: sanitized_bytes(v)?,
                })
            })
            .transpose()?
            .into_iter()
            .collect();
        registry.import(
            LLM,
            providers
                .iter()
                .map(serde_json::to_value)
                .collect::<Result<_, _>>()
                .map_err(|e| e.to_string())?,
            serde_json::json!({"primary":primary,"mirror":mirror.map(|(v,_)|v)}),
            Some(LLM_PROVIDERS_SETTING_KEY),
            cleanups,
        )?;
    }
    let mut providers: Vec<LlmProvider> = registry
        .documents(LLM)?
        .into_iter()
        .map(serde_json::from_value)
        .collect::<Result<_, _>>()
        .map_err(|e| e.to_string())?;
    for (id, (name, base_url, model, ctx, max_out)) in builtin_defaults() {
        if !providers.iter().any(|p| p.id == id) {
            let provider = LlmProvider {
                id: id.into(),
                name: name.into(),
                base_url: base_url.into(),
                model: model.into(),
                api_key: String::new(),
                is_custom: false,
                enabled: false,
                context_window: Some(ctx),
                max_output_tokens: Some(max_out),
                identity: None,
            };
            registry.save(
                LLM,
                &serde_json::to_value(&provider).map_err(|e| e.to_string())?,
                None,
            )?;
            providers.insert(0, provider);
        }
    }
    Ok(providers)
}

#[derive(Clone, Serialize)]
pub struct LlmProviderView {
    #[serde(flatten)]
    pub provider: LlmProvider,
    pub has_api_key: bool,
    pub key_mask: Option<String>,
    pub identity: crate::model_registry::ModelIdentity,
}
#[derive(Deserialize)]
pub struct LlmProviderInput {
    #[serde(flatten)]
    pub provider: LlmProvider,
    #[serde(default)]
    pub clear_key: bool,
}
fn provider_view(provider: LlmProvider) -> Result<LlmProviderView, String> {
    let identity = crate::model_registry::Registry::shared()?
        .identity(crate::model_registry::LLM, &provider.id)?;
    Ok(masked_provider(provider, identity))
}
fn masked_provider(
    mut provider: LlmProvider,
    identity: crate::model_registry::ModelIdentity,
) -> LlmProviderView {
    let has_api_key = identity.credential_id.is_some();
    provider.api_key.clear();
    LlmProviderView {
        provider,
        has_api_key,
        key_mask: has_api_key.then(|| "••••••••".into()),
        identity,
    }
}
fn resolve_saved_provider(id: &str) -> Result<LlmProvider, String> {
    load_providers()?;
    let (document, identity, secret) =
        crate::model_registry::Registry::shared()?.resolve(crate::model_registry::LLM, id)?;
    let mut provider: LlmProvider = serde_json::from_value(document).map_err(|e| e.to_string())?;
    provider.api_key = secret.to_string();
    provider.identity = Some(identity);
    Ok(provider)
}
fn validate_model_url(raw: &str) -> Result<(), String> {
    let url = reqwest::Url::parse(raw).map_err(|_| "模型 URL 无效")?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err("请填写不含凭据或查询参数的 HTTP(S) 模型地址".into());
    }
    Ok(())
}
fn resolve_draft(mut provider: LlmProvider) -> Result<LlmProvider, String> {
    validate_model_url(&provider.base_url)?;
    if provider.api_key.is_empty() {
        let saved = resolve_saved_provider(&provider.id)?;
        if saved.base_url.trim_end_matches('/') != provider.base_url.trim_end_matches('/') {
            return Err("更改模型地址后请重新填写 Key，避免将已有凭证发送到新地址".into());
        }
        if provider.model == saved.model {
            provider.identity = saved.identity;
        }
        provider.api_key = saved.api_key;
    }
    Ok(provider)
}
/// Remove retired built-in provider records from persisted settings. They were
/// never user-created custom providers, so the sponsor migration can remove
/// them without touching a user's custom OpenAI-compatible endpoint.
fn remove_retired_builtins(providers: &mut Vec<LlmProvider>) -> bool {
    let before = providers.len();
    providers.retain(|provider| {
        provider.is_custom || !matches!(provider.id.as_str(), "deepseek" | "openai")
    });
    providers.len() != before
}

#[tauri::command]
pub async fn list_llm_providers() -> Result<Vec<LlmProviderView>, String> {
    tauri::async_runtime::spawn_blocking(move || list_llm_providers_impl())
        .await
        .map_err(|_| "模型配置操作未完成，请重试".to_string())?
}

fn list_llm_providers_impl() -> Result<Vec<LlmProviderView>, String> {
    load_providers()?.into_iter().map(provider_view).collect()
}
#[tauri::command]
pub fn memory_extraction_config_get() -> MemoryExtractionConfig {
    memory_extraction_config()
}

#[tauri::command]
pub fn ollama_config_get() -> OllamaConfig {
    ollama_config()
}

#[tauri::command]
pub fn ollama_config_set(config: OllamaConfig) -> Result<(), String> {
    let normalized = OllamaConfig {
        base_url: normalize_ollama_base_url(&config.base_url)?,
    };
    save_ollama_config(&normalized)
}

/// Probe an Ollama service: fetch its version and the locally installed
/// model list.  No API key is involved; Ollama binds to loopback by default.
#[tauri::command]
pub async fn test_ollama_connection(base_url: String) -> Result<OllamaTestResult, String> {
    let base = normalize_ollama_base_url(&base_url)?;
    let client = reqwest::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(5))
        .timeout(std::time::Duration::from_secs(8))
        .build()
        .map_err(|error| format!("无法创建 Ollama 连接: {error}"))?;

    // The version probe is best-effort: model listing below is the real
    // health check, and very old Ollama builds lack /api/version.
    let version = match client.get(format!("{base}/api/version")).send().await {
        Ok(resp) if resp.status().is_success() => resp
            .json::<serde_json::Value>()
            .await
            .ok()
            .and_then(|value| value["version"].as_str().map(str::to_string)),
        _ => None,
    };

    let tags_resp = client
        .get(format!("{base}/api/tags"))
        .send()
        .await
        .map_err(|error| {
            if error.is_connect() || error.is_timeout() {
                format!("无法连接到 Ollama（{base}）。请确认 Ollama 已启动且地址正确")
            } else {
                format!("Ollama 请求失败: {error}")
            }
        })?;
    if !tags_resp.status().is_success() {
        return Err(format!("Ollama 返回 HTTP {}", tags_resp.status()));
    }
    let tags = tags_resp
        .json::<serde_json::Value>()
        .await
        .map_err(|error| format!("无法解析 Ollama 模型列表: {error}"))?;
    let models = tags["models"]
        .as_array()
        .map(|items| {
            items
                .iter()
                .filter_map(|item| {
                    Some(OllamaModelInfo {
                        name: item["name"].as_str()?.to_string(),
                        size: item["size"].as_u64().unwrap_or(0),
                        parameter_size: item["details"]["parameter_size"]
                            .as_str()
                            .map(str::to_string),
                        quantization_level: item["details"]["quantization_level"]
                            .as_str()
                            .map(str::to_string),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    Ok(OllamaTestResult { version, models })
}

#[tauri::command]
pub fn memory_extraction_config_set(config: MemoryExtractionConfig) -> Result<(), String> {
    if let Some(provider_id) = config.provider_id.as_deref() {
        let provider = list_llm_providers_impl()?
            .into_iter()
            .find(|candidate| candidate.provider.id == provider_id)
            .ok_or("所选记忆提取模型不存在")?;
        if !provider.provider.enabled || !provider.has_api_key {
            return Err("所选记忆提取模型尚未启用或未配置 API Key".into());
        }
    }
    save_memory_extraction_config(&config)
}

/// Resolve the configured provider inside the native process.  Hook requests
/// never receive credentials from the webview.
pub fn memory_extraction_provider() -> Result<LlmProvider, String> {
    let id=memory_extraction_config().provider_id.ok_or("请先在设置中选择记忆提取模型")?;
    let provider=resolve_saved_provider(&id)?;
    if !provider.enabled || provider.api_key.trim().is_empty() {return Err("记忆提取模型未启用或缺少 API Key".into())}
    Ok(provider)
}

/// Compatibility facade. New callers should provide a trusted purpose.
#[allow(dead_code)]
pub async fn complete_text(
    provider: &LlmProvider,
    messages: &[serde_json::Value],
) -> Result<String, String> {
    complete_text_with_limit(provider, messages, None).await
}
#[allow(dead_code)]
pub async fn complete_text_with_limit(
    provider: &LlmProvider,
    messages: &[serde_json::Value],
    output_limit: Option<u32>,
) -> Result<String, String> {
    complete_text_for(
        provider,
        messages,
        output_limit,
        crate::llm_trace::ModelPurpose::Interactive,
    )
    .await
}

#[derive(Clone, Debug, Serialize)]
pub struct ModelResponse {
    pub text: String,
    pub request: crate::llm_trace::ModelRequestSnapshot,
}

pub async fn complete_text_for(
    provider: &LlmProvider,
    messages: &[serde_json::Value],
    output_limit: Option<u32>,
    purpose: crate::llm_trace::ModelPurpose,
) -> Result<String, String> {
    complete(
        provider,
        messages,
        output_limit,
        crate::llm_trace::ModelContext::new(purpose),
    )
    .await
    .map(|r| r.text)
}

pub async fn complete(
    provider: &LlmProvider,
    messages: &[serde_json::Value],
    output_limit: Option<u32>,
    context: crate::llm_trace::ModelContext,
) -> Result<ModelResponse, String> {
    let mut trace = crate::llm_trace::ModelTrace::start(
        provider,
        context,
        crate::telemetry_store::shared_store(),
    )?;
    complete_traced(provider, messages, output_limit, &mut trace).await
}

pub(crate) async fn complete_traced(
    provider: &LlmProvider,
    messages: &[serde_json::Value],
    output_limit: Option<u32>,
    trace: &mut crate::llm_trace::ModelTrace,
) -> Result<ModelResponse, String> {
    let result = send_text_request(provider, messages, output_limit, trace).await;
    trace.finish(result.as_ref().err().map(|e| e.kind))?;
    result
        .map(|text| ModelResponse {
            text,
            request: trace.snapshot.clone(),
        })
        .map_err(|e| {
            if provider.api_key.is_empty() {
                e.message
            } else {
                e.message.replace(&provider.api_key, "[key]")
            }
        })
}

struct ModelFailure {
    kind: &'static str,
    message: String,
}
fn failure(kind: &'static str, message: String) -> ModelFailure {
    ModelFailure { kind, message }
}

async fn send_text_request(
    provider: &LlmProvider,
    messages: &[serde_json::Value],
    output_limit: Option<u32>,
    trace: &mut crate::llm_trace::ModelTrace,
) -> Result<String, ModelFailure> {
    let is_minimax = provider.base_url.to_ascii_lowercase().contains("minimax")
        || provider.model.to_ascii_lowercase().contains("minimax");
    // Reasoning models consume their output budget for both thoughts and the
    // final answer.  Keep the configured budget, but ensure an older custom
    // MiniMax record without a value still has enough room for JSON tasks.
    let configured_limit =
        provider
            .max_output_tokens
            .unwrap_or(if is_minimax { 16_384 } else { 1024 });
    let max_output_tokens = output_limit
        .map(|limit| configured_limit.min(limit))
        .unwrap_or(configured_limit);
    let mut body = serde_json::json!({
        "model": provider.model,
        "messages": messages,
        "temperature": 0.1,
        "max_tokens": max_output_tokens,
    });
    if is_minimax {
        body["max_completion_tokens"] = serde_json::json!(max_output_tokens);
        // MiniMax's OpenAI-compatible interface can return thought text in
        // `content` unless this flag is set.  Splitting it keeps the JSON/text
        // answer intact and prevents our safety filter from discarding it.
        body["reasoning_split"] = serde_json::json!(true);
    }
    let client = reqwest::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(20))
        .pool_max_idle_per_host(0)
        .build()
        .map_err(|error| failure("client", format!("无法创建模型连接: {error}")))?;
    let configured_url = format!(
        "{}/chat/completions",
        provider.base_url.trim_end_matches('/')
    );
    // MiniMax M3 can spend several minutes on the large map/reduce prompts
    // used by 30-day memory consolidation.  Keep the shorter default for
    // ordinary providers while allowing the configured MiniMax endpoint to
    // finish its reasoning pass instead of being cut off at 120 seconds.
    let request_timeout = if is_minimax {
        std::time::Duration::from_secs(600)
    } else {
        std::time::Duration::from_secs(120)
    };
    // MiniMax credentials are region-bound: a key issued for minimaxi.com can
    // be rejected by minimax.io (and vice versa) with error 2049.  The settings
    // connection test uses the configured URL, so memory extraction must use
    // that exact same endpoint instead of silently crossing regions.
    let response = client
        .post(&configured_url)
        .header("Authorization", format!("Bearer {}", provider.api_key))
        .header("Content-Type", "application/json")
        .json(&body)
        .timeout(request_timeout)
        .send()
        .await
        .map_err(|error| {
            let kind = if error.is_timeout() {
                "timeout"
            } else if error.is_connect() {
                "connect"
            } else {
                "transport"
            };
            failure(
                kind,
                format!(
                    "模型请求失败（{}）: {}",
                    provider.name,
                    explain_request_error(&error.without_url())
                ),
            )
        })?;
    let status = response.status();
    trace.snapshot.http_status = Some(status.as_u16());
    trace.snapshot.upstream_request_id = response
        .headers()
        .get("x-request-id")
        .or_else(|| response.headers().get("request-id"))
        .and_then(|v| v.to_str().ok())
        .map(|v| crate::llm_trace::safe_text(v, &provider.api_key));
    let value = response.json::<serde_json::Value>().await.map_err(|_| {
        failure(
            if status.is_success() {
                "invalid_json"
            } else {
                "http"
            },
            format!("模型响应无法解析（HTTP {status}）"),
        )
    })?;
    trace.observe(provider, &value);
    if !status.is_success() {
        let message = value
            .pointer("/error/message")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("unknown LLM error");
        return Err(failure(
            "http",
            format!("模型返回 HTTP {status}: {message}"),
        ));
    }
    let finish_reason = value
        .pointer("/choices/0/finish_reason")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("unknown");
    let content = value
        .pointer("/choices/0/message/content")
        .and_then(serde_json::Value::as_str)
        .map(str::to_string)
        .filter(|content| !content.trim().is_empty())
        // MiniMax reasoning models usually send their chain-of-thought in
        // `reasoning_content`; it must never be treated as the final answer.
        .ok_or_else(|| failure("empty_content", {
            if is_minimax {
                format!("MiniMax 未返回最终文本（结束原因：{finish_reason}）。已启用 reasoning_split；请检查模型名称与服务端额度后重试")
            } else {
                "记忆提取模型未返回文本内容".to_string()
            }
        }))?;
    let answer = strip_thinking_blocks(&content);
    (!answer.trim().is_empty()).then_some(answer).ok_or_else(|| failure("empty_content", {
        if is_minimax {
            format!("MiniMax 最终文本在思考过滤后为空（结束原因：{finish_reason}）。请提高最大输出 Token 或稍后重试")
        } else {
            "记忆提取模型仅返回了思考内容，未返回最终文本".to_string()
        }
    }))
}
fn explain_request_error(error: &reqwest::Error) -> String {
    if error.is_timeout() {
        "请求超时，请稍后重试".into()
    } else if error.is_connect() {
        format!("无法建立 HTTPS 连接（{}）", error)
    } else if error.is_request() {
        format!("请求发送失败（{}）", error)
    } else {
        error.to_string()
    }
}

/// Some OpenAI-compatible gateways flatten a reasoning model's thought text
/// into `message.content` instead of exposing `reasoning_content`.  Strip
/// only explicit protocol tags, leaving ordinary XML/Markdown untouched.
#[tauri::command]
pub async fn save_llm_provider(provider: LlmProviderInput) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || save_llm_provider_impl(provider))
        .await
        .map_err(|_| "模型配置操作未完成，请重试".to_string())?
}

fn save_llm_provider_impl(provider: LlmProviderInput) -> Result<(), String> {
    let old = load_providers()?;
    let input = provider;
    let mut provider = input.provider;
    validate_model_url(&provider.base_url)?;
    provider.base_url = provider.base_url.trim().trim_end_matches('/').to_string();
    let registry = crate::model_registry::Registry::shared()?;
    if provider.api_key.is_empty() && !input.clear_key {
        if let Some(previous) = old.iter().find(|p| p.id == provider.id) {
            if previous.base_url.trim_end_matches('/') != provider.base_url
                && registry
                    .identity(crate::model_registry::LLM, &provider.id)?
                    .credential_id
                    .is_some()
            {
                return Err("更改模型地址时请重新填写 Key 或明确清除旧 Key".into());
            }
        }
    }
    let secret = if input.clear_key {
        Some("")
    } else if provider.api_key.is_empty() {
        None
    } else {
        Some(provider.api_key.as_str())
    };
    registry.save(
        crate::model_registry::LLM,
        &serde_json::to_value(&provider).map_err(|e| e.to_string())?,
        secret,
    )?;
    Ok(())
}
#[tauri::command]
pub async fn delete_llm_provider(id: String) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || delete_llm_provider_impl(id))
        .await
        .map_err(|_| "模型配置操作未完成，请重试".to_string())?
}

fn delete_llm_provider_impl(id: String) -> Result<(), String> {
    if builtin_defaults().contains_key(id.as_str()) {
        return Err("Cannot delete built-in provider".into());
    }
    load_providers()?;
    crate::model_registry::Registry::shared()?.delete(crate::model_registry::LLM, &id)
}

#[tauri::command]
pub async fn test_llm_provider(provider: LlmProvider) -> Result<String, String> {
    let provider = tauri::async_runtime::spawn_blocking(move || resolve_draft(provider))
        .await
        .map_err(|_| "模型凭证读取未完成")??;
    if provider.api_key.is_empty() {
        return Err("API key is empty".to_string());
    }

    let client = reqwest::Client::new();
    let url = format!(
        "{}/chat/completions",
        provider.base_url.trim_end_matches('/')
    );

    let is_minimax = provider.base_url.to_ascii_lowercase().contains("minimax")
        || provider.model.to_ascii_lowercase().contains("minimax");
    let mut body = serde_json::json!({
        "model": provider.model,
        "messages": [{"role": "user", "content": "Hi"}],
        "max_tokens": if is_minimax { 256 } else { 5 },
    });
    if is_minimax {
        body["max_completion_tokens"] = serde_json::json!(256);
        body["reasoning_split"] = serde_json::json!(true);
    }

    let resp = client
        .post(&url)
        .header("Authorization", format!("Bearer {}", provider.api_key))
        .header("Content-Type", "application/json")
        .json(&body)
        .timeout(std::time::Duration::from_secs(15))
        .send()
        .await
        .map_err(|e| format!("Request failed: {}", e))?;

    let status = resp.status();
    if status.is_success() {
        Ok(format!(
            "✓ Connected — {} ({})",
            provider.name, provider.model
        ))
    } else {
        let text = resp.text().await.unwrap_or_default();
        let msg = serde_json::from_str::<serde_json::Value>(&text)
            .ok()
            .and_then(|v| v["error"]["message"].as_str().map(|s| s.to_string()))
            .unwrap_or(text);
        Err(format!("HTTP {}: {}", status, msg).replace(&provider.api_key, "[key]"))
    }
}

/// List model ids exposed by an OpenAI-compatible endpoint.
///
/// Gateways such as 88API publish dozens of models and the exact ids change
/// over time, so typing one by hand is error-prone.  Kept separate from
/// `test_llm_provider` because the draft provider being edited may not be
/// saved yet: only the base URL and key are needed here.
#[tauri::command]
pub async fn list_llm_models(
    base_url: String,
    api_key: String,
    provider_id: Option<String>,
) -> Result<Vec<String>, String> {
    validate_model_url(&base_url)?;
    let api_key = if api_key.is_empty() {
        let provider = resolve_saved_provider(
            provider_id
                .as_deref()
                .ok_or("请选择已保存的模型连接，或填写新 Key")?,
        )?;
        if provider.base_url.trim_end_matches('/') != base_url.trim_end_matches('/') {
            return Err("模型地址已更改，请填写新 Key".into());
        }
        provider.api_key
    } else {
        api_key
    };
    let base = base_url.trim().trim_end_matches('/');
    if base.is_empty() {
        return Err("Base URL 不能为空".to_string());
    }
    if api_key.trim().is_empty() {
        return Err("请先填写 API 密钥再获取模型列表".to_string());
    }
    let url = format!("{base}/models");
    let client = reqwest::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(10))
        .timeout(std::time::Duration::from_secs(20))
        .build()
        .map_err(|error| format!("无法创建连接: {error}"))?;
    let resp = client
        .get(&url)
        .header("Authorization", format!("Bearer {}", api_key.trim()))
        .send()
        .await
        .map_err(|error| {
            if error.is_connect() || error.is_timeout() {
                format!("无法连接到 {base}，请检查 Base URL 与网络")
            } else {
                format!("请求失败: {error}")
            }
        })?;
    let status = resp.status();
    let text = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        let msg = serde_json::from_str::<serde_json::Value>(&text)
            .ok()
            .and_then(|value| {
                value["error"]["message"]
                    .as_str()
                    .or_else(|| value["message"].as_str())
                    .map(str::to_string)
            })
            .unwrap_or_else(|| truncate_chars(&text, 200));
        return Err(format!("HTTP {status}: {msg}").replace(&api_key, "[key]"));
    }
    let value: serde_json::Value = serde_json::from_str(&text)
        .map_err(|_| "模型列表返回的不是 JSON，可能不是 OpenAI 兼容接口".to_string())?;
    // OpenAI 兼容实现把列表放在 data[]，个别网关直接返回数组。
    let items = value
        .get("data")
        .and_then(serde_json::Value::as_array)
        .or_else(|| value.as_array())
        .ok_or("响应中没有模型列表（缺少 data 字段）")?;
    let mut models = items
        .iter()
        .filter_map(|item| {
            item.get("id")
                .and_then(serde_json::Value::as_str)
                .or_else(|| item.as_str())
                .map(str::to_string)
        })
        .collect::<Vec<_>>();
    models.sort();
    models.dedup();
    if models.is_empty() {
        return Err("接口未返回任何模型，请确认账号额度或权限".to_string());
    }
    Ok(models)
}
fn truncate_chars(text: &str, limit: usize) -> String {
    let trimmed = text.trim();
    if trimmed.chars().count() <= limit {
        return trimmed.to_string();
    }
    trimmed.chars().take(limit).collect::<String>() + "…"
}

#[cfg(test)]
mod tests {
    use super::{
        builtin_defaults, decrypt_api_key, encrypt_api_key, normalize_memory_extraction_config,
        remove_retired_builtins, LlmProvider, MemoryExtractionConfig, ENCRYPTED_KEY_PREFIX,
        SPONSOR_BASE_URL, SPONSOR_CREDITS_URL, SPONSOR_PROVIDER_ID, SPONSOR_PROVIDER_NAME,
    };
    use crate::thinking::strip_thinking_blocks;

    #[test]
    fn saved_provider_view_excludes_secrets_and_incoming_identity_is_untrusted() {
        let input:super::LlmProviderInput=serde_json::from_value(serde_json::json!({"id":"fixture","name":"fixture","base_url":"https://fixture.invalid","model":"model","api_key":"fixture-secret","enabled":true,"is_custom":true,"context_window":null,"max_output_tokens":null,"identity":{"provider_id":"spoofed","deployment_id":null,"credential_id":"spoofed","config_revision":999}})).unwrap();
        assert!(input.provider.identity.is_none());
        let identity=crate::model_registry::ModelIdentity{provider_id:"real-provider".into(),deployment_id:Some("real-deployment".into()),credential_id:Some("real-credential".into()),config_revision:2};
        let view=super::masked_provider(input.provider,identity);
        let serialized=serde_json::to_value(view).unwrap();
        assert_eq!(serialized["api_key"],"");assert_eq!(serialized["has_api_key"],true);
        assert_eq!(serialized["identity"]["provider_id"],"real-provider");
        assert!(!serialized.to_string().contains("fixture-secret"));assert!(!serialized.to_string().contains("spoofed"));
    }

    #[test]
    fn sponsor_is_the_only_builtin_provider() {
        let defaults = builtin_defaults();
        assert_eq!(defaults.len(), 1);
        assert_eq!(
            defaults.get(SPONSOR_PROVIDER_ID),
            Some(&(SPONSOR_PROVIDER_NAME, SPONSOR_BASE_URL, "", 128000, 16384))
        );
        assert_eq!(SPONSOR_CREDITS_URL, "https://88api.ai/sign-up?aff=VNNV");
    }

    #[test]
    fn sponsor_migration_removes_only_retired_builtin_records() {
        let provider = |id: &str, is_custom: bool| LlmProvider {
            id: id.to_string(), name: id.to_string(), base_url: String::new(), model: String::new(),
            api_key: String::new(), is_custom, enabled: false, context_window: None,
            max_output_tokens: None, identity:None,
        };
        let mut providers = vec![
            provider("deepseek", false),
            provider("openai", false),
            provider("openai", true),
            provider("custom_88proxy", true),
        ];
        assert!(remove_retired_builtins(&mut providers));
        assert_eq!(providers.len(), 2);
        assert!(providers.iter().all(|provider| provider.is_custom));
        assert_eq!(
            normalize_memory_extraction_config(MemoryExtractionConfig { provider_id: Some("deepseek".into()) }).provider_id,
            None
        );
        assert_eq!(
            normalize_memory_extraction_config(MemoryExtractionConfig { provider_id: Some("custom_88proxy".into()) }).provider_id.as_deref(),
            Some("custom_88proxy")
        );
    }

    #[test]
    fn api_key_round_trips_with_a_valid_gcm_nonce() {
        let encrypted = encrypt_api_key("sk-test-secret").expect("encrypt API key");
        assert!(encrypted.starts_with(ENCRYPTED_KEY_PREFIX));
        assert_ne!(encrypted, "sk-test-secret");
        assert_eq!(decrypt_api_key(&encrypted), "sk-test-secret");
    }

    #[test]
    fn malformed_v2_key_does_not_panic_or_echo_ciphertext() {
        assert_eq!(decrypt_api_key("v2:0011:ffee"), "");
    }

    #[test]
    fn minimax_thinking_tags_are_removed_before_parsing_the_answer() {
        assert_eq!(
            strip_thinking_blocks("<think>分析 JSON</think>\n{\"actions\":[]}"),
            "{\"actions\":[]}"
        );
        assert_eq!(
            strip_thinking_blocks("<THINKING>reasoning</THINKING>final"),
            "final"
        );
    }
}


#[cfg(test)]
mod request_evidence_tests {
    use super::*;
    use crate::llm_trace::{read_traces, ModelContext, ModelPurpose, ModelTrace};
    use crate::telemetry_store::TelemetryStore;
    use serde_json::{json, Value};
    use std::io::{Read, Write};
    use std::sync::Arc;

    fn upstream(status: u16, body: &str) -> (LlmProvider, std::thread::JoinHandle<Value>) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let reply = body.to_string();
        let handle = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(std::time::Duration::from_secs(5)))
                .unwrap();
            let mut request = Vec::new();
            let end = loop {
                let mut buffer = [0; 4096];
                let n = stream.read(&mut buffer).unwrap();
                assert!(n > 0);
                request.extend_from_slice(&buffer[..n]);
                if let Some(pos) = request.windows(4).position(|b| b == b"\r\n\r\n") {
                    break pos + 4;
                }
            };
            let headers = String::from_utf8_lossy(&request[..end]);
            assert!(headers.starts_with("POST /v1/chat/completions "));
            let length: usize = headers
                .lines()
                .find_map(|line| {
                    line.to_ascii_lowercase()
                        .strip_prefix("content-length:")
                        .map(|v| v.trim().parse().unwrap())
                })
                .unwrap();
            while request.len() < end + length {
                let mut buffer = [0; 4096];
                let n = stream.read(&mut buffer).unwrap();
                assert!(n > 0);
                request.extend_from_slice(&buffer[..n]);
            }
            let value = serde_json::from_slice(&request[end..end + length]).unwrap();
            write!(stream,"HTTP/1.1 {status} Test\r\nContent-Type: application/json\r\nx-request-id: upstream-123\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",reply.len(),reply).unwrap();
            value
        });
        (
            LlmProvider {
                id: "local-fixture".into(),
                name: "fixture".into(),
                base_url: format!("http://{address}/v1"),
                model: "MiniMax-fixture".into(),
                api_key: "fixture-key".into(),
                is_custom: true,
                enabled: true,
                context_window: None,
                max_output_tokens: Some(256), identity:None,
            },
            handle,
        )
    }

    #[tokio::test]
    async fn direct_response_preserves_actual_model_usage_headers_and_legacy_text() {
        let body=json!({"id":"completion-123","model":"actual-model","choices":[{"finish_reason":"stop","message":{"content":"<think>private thought</think>OK"}}],"usage":{"prompt_tokens":10,"completion_tokens":3,"total_tokens":13,"prompt_tokens_details":{"cached_tokens":5}}}).to_string();
        let (provider, server) = upstream(200, &body);
        let store = Arc::new(TelemetryStore::in_memory());
        let mut trace = ModelTrace::start(
            &provider,
            ModelContext::new(ModelPurpose::MemoryExtract),
            Some(store.clone()),
        )
        .unwrap();
        let result = complete_traced(
            &provider,
            &[json!({"role":"user","content":"private prompt"})],
            Some(128),
            &mut trace,
        )
        .await
        .unwrap();
        assert_eq!(result.text, "OK");
        assert_eq!(result.request.served_model.as_deref(), Some("actual-model"));
        assert_eq!(
            result.request.upstream_request_id.as_deref(),
            Some("upstream-123")
        );
        assert_eq!(result.request.usage.total_tokens, Some(13));
        let sent = server.join().unwrap();
        assert_eq!(sent["max_tokens"], 128);
        assert_eq!(sent["max_completion_tokens"], 128);
        assert_eq!(sent["reasoning_split"], true);
        let records = read_traces(&store, None, 100).unwrap();
        assert_eq!(records.len(), 1);
        let evidence = serde_json::to_string(&records).unwrap();
        for private in ["private prompt", "private thought", "fixture-key"] {
            assert!(!evidence.contains(private));
        }
    }

    #[tokio::test]
    async fn billable_empty_response_http_error_and_invalid_json_remain_observable() {
        for (status,body,kind) in [
            (200,json!({"model":"served","choices":[{"finish_reason":"length","message":{"reasoning_content":"private"}}],"usage":{"completion_tokens":50}}).to_string(),"empty_content"),
            (429,json!({"error":{"message":"fixture-key rate limit"}}).to_string(),"http"),
            (200,"not-json".into(),"invalid_json"),
        ] {
            let (provider,server)=upstream(status,&body);
            let store=Arc::new(TelemetryStore::in_memory());
            let mut trace=ModelTrace::start(&provider,ModelContext::new(ModelPurpose::Review),Some(store.clone())).unwrap();
            let error=complete_traced(&provider,&[],None,&mut trace).await.unwrap_err(); assert!(!error.contains("fixture-key"));
            server.join().unwrap();
            let record=read_traces(&store,None,1).unwrap().remove(0);
            assert_eq!(record.error_kind.as_deref(),Some(kind)); assert_eq!(record.http_status,Some(status)); assert_eq!(record.state,"failed");
            if kind=="empty_content" { assert_eq!(record.usage.output_tokens,Some(50)); assert_eq!(record.finish_reason.as_deref(),Some("length")); }
            else { assert_eq!(record.usage.input_tokens,None); }
        }
    }
}
