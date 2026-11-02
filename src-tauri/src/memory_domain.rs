//! Decision-model-backed domain tagging and query routing for durable L1 memories.
//!
//! JEV and Ollama are optional. New memories remain writable without either and
//! recall falls back to the legacy global search with explicit metadata.

use crate::telemetry_store::{
    LocalMemorySnapshot, MemoryDomain, MemoryDomainAssignment, TelemetryStore,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

const JEV_CONFIG_SETTING_KEY: &str = "memory_domain_jev";
const DEFAULT_ENDPOINT: &str = "https://api.typesafe.ai/v1/systemone";
const NONE_DOMAIN: &str = "__none__";

fn default_provider() -> String { "jev".into() }
fn default_endpoint() -> String { DEFAULT_ENDPOINT.into() }
fn default_model() -> String { "jev-latest".into() }
fn default_timeout() -> u64 { 8_000 }
fn default_tag_threshold() -> f64 { 0.70 }
fn default_route_threshold() -> f64 { 0.60 }
fn default_second_threshold() -> f64 { 0.55 }

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct MemoryDomainConfig {
    pub enabled: bool,
    #[serde(default = "default_provider")]
    pub provider: String,
    #[serde(default = "default_endpoint")]
    pub endpoint: String,
    #[serde(default = "default_model")]
    pub model: String,
    #[serde(default)]
    pub api_key: String,
    #[serde(default = "default_timeout")]
    pub timeout_ms: u64,
    #[serde(default = "default_tag_threshold")]
    pub tag_threshold: f64,
    #[serde(default = "default_route_threshold")]
    pub route_threshold: f64,
    #[serde(default = "default_second_threshold")]
    pub second_threshold: f64,
}

impl Default for MemoryDomainConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            provider: default_provider(),
            endpoint: default_endpoint(),
            model: default_model(),
            api_key: String::new(),
            timeout_ms: default_timeout(),
            tag_threshold: default_tag_threshold(),
            route_threshold: default_route_threshold(),
            second_threshold: default_second_threshold(),
        }
    }
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub struct MemoryDomainConfigStatus {
    pub enabled: bool,
    pub provider: String,
    pub endpoint: String,
    pub model: String,
    pub has_api_key: bool,
    pub key_source: String,
    pub timeout_ms: u64,
    pub tag_threshold: f64,
    pub route_threshold: f64,
    pub second_threshold: f64,
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct MemoryDomainConfigUpdate {
    pub enabled: bool,
    #[serde(default = "default_provider")]
    pub provider: String,
    pub endpoint: String,
    pub model: String,
    #[serde(default)]
    pub api_key: Option<String>,
    #[serde(default)]
    pub clear_api_key: bool,
    pub timeout_ms: u64,
    pub tag_threshold: f64,
    pub route_threshold: f64,
    pub second_threshold: f64,
}

fn domain_store() -> Result<TelemetryStore, String> {
    match crate::telemetry_store::shared_store() {
        Some(store) => Ok(store.as_ref().clone()),
        None => TelemetryStore::new(),
    }
}

fn normalize_config(mut config: MemoryDomainConfig) -> Result<MemoryDomainConfig, String> {
    config.provider = config.provider.trim().to_ascii_lowercase();
    if !matches!(config.provider.as_str(), "jev" | "ollama") {
        return Err("决策模型提供方只能是 jev 或 ollama".into());
    }
    config.endpoint = config.endpoint.trim().trim_end_matches('/').to_string();
    let url = reqwest::Url::parse(&config.endpoint)
        .map_err(|_| "JEV endpoint 不是有效 URL".to_string())?;
    let loopback = url.host_str().map(|host| {
        host.eq_ignore_ascii_case("localhost")
            || host.trim_matches(['[', ']']).parse::<std::net::IpAddr>().ok()
                .map(|ip| ip.is_loopback())
                .unwrap_or(false)
    }).unwrap_or(false);
    if url.scheme() != "https" && !(url.scheme() == "http" && loopback) {
        return Err("JEV endpoint 必须使用 HTTPS；仅本机服务允许 HTTP".into());
    }
    if !url.username().is_empty() || url.password().is_some()
        || url.query().is_some() || url.fragment().is_some()
    {
        return Err("JEV endpoint 不得包含凭据、query 或 fragment".into());
    }
    config.model = config.model.trim().to_string();
    if config.model.is_empty() {
        return Err("决策模型 ID 不能为空".into());
    }
    config.timeout_ms = config.timeout_ms.clamp(250, 120_000);
    for threshold in [
        &mut config.tag_threshold,
        &mut config.route_threshold,
        &mut config.second_threshold,
    ] {
        if !threshold.is_finite() {
            return Err("JEV 阈值必须是有限数值".into());
        }
        *threshold = threshold.clamp(0.0, 1.0);
    }
    Ok(config)
}

fn load_config(store: &TelemetryStore) -> Result<MemoryDomainConfig, String> {
    let saved = store.app_setting_get::<MemoryDomainConfig>(JEV_CONFIG_SETTING_KEY);
    let mut config = saved.unwrap_or_default();
    let stored_key_present = !config.api_key.trim().is_empty();
    if config.provider == "jev" && !stored_key_present {
        if let Ok(key) = std::env::var("TYPESAFE_API_KEY") {
            // The environment can supply a credential, but upload remains an
            // explicit in-app opt-in rather than an implicit side effect.
            config.api_key = key.trim().to_string();
        }
    }
    normalize_config(config)
}

fn config_status(config: &MemoryDomainConfig) -> MemoryDomainConfigStatus {
    let key_source = if config.api_key.is_empty() {
        "none"
    } else if std::env::var("TYPESAFE_API_KEY").ok().is_some_and(|key| key.trim() == config.api_key.as_str()) {
        "environment"
    } else {
        "local_setting"
    };
    MemoryDomainConfigStatus {
        enabled: config.enabled,
        provider: config.provider.clone(),
        endpoint: config.endpoint.clone(),
        model: config.model.clone(),
        has_api_key: !config.api_key.is_empty(),
        key_source: key_source.into(),
        timeout_ms: config.timeout_ms,
        tag_threshold: config.tag_threshold,
        route_threshold: config.route_threshold,
        second_threshold: config.second_threshold,
    }
}

#[tauri::command]
pub fn memory_domain_config_get() -> Result<MemoryDomainConfigStatus, String> {
    let store = domain_store()?;
    Ok(config_status(&load_config(&store)?))
}

#[tauri::command]
pub fn memory_domain_config_set(
    update: MemoryDomainConfigUpdate,
) -> Result<MemoryDomainConfigStatus, String> {
    let store = domain_store()?;
    let current = load_config(&store)?;
    let api_key = if update.clear_api_key {
        String::new()
    } else {
        match update.api_key.as_deref().map(str::trim) {
            Some(key) if !key.is_empty() => key.to_string(),
            _ => current.api_key,
        }
    };
    let mut config = normalize_config(MemoryDomainConfig {
        enabled: update.enabled,
        provider: update.provider,
        endpoint: update.endpoint,
        model: update.model,
        api_key,
        timeout_ms: update.timeout_ms,
        tag_threshold: update.tag_threshold,
        route_threshold: update.route_threshold,
        second_threshold: update.second_threshold,
    })?;
    if config.provider == "jev" && config.api_key.is_empty() {
        if let Ok(key) = std::env::var("TYPESAFE_API_KEY") {
            config.api_key = key.trim().to_string();
        }
    }
    if config.enabled && config.provider == "jev" && config.api_key.is_empty() {
        return Err("启用 JEV 前请配置 TYPESAFE_API_KEY".into());
    }
    store.app_setting_set(JEV_CONFIG_SETTING_KEY, &config)?;
    Ok(config_status(&config))
}

struct DecisionClient {
    config: MemoryDomainConfig,
}

impl DecisionClient {
    fn new(config: MemoryDomainConfig) -> Result<Self, String> {
        if !config.enabled {
            return Err("领域决策模型未启用".into());
        }
        if config.provider == "jev" && config.api_key.trim().is_empty() {
            return Err("JEV 缺少 TYPESAFE_API_KEY".into());
        }
        if config.provider == "ollama" && config.model.trim().is_empty() {
            return Err("请选择 Ollama 决策模型".into());
        }
        Ok(Self { config })
    }

    async fn system_one_async(
        &self,
        state: String,
        questions: Map<String, Value>,
        timeout_override: Option<Duration>,
    ) -> Result<Value, String> {
        let config = self.config.clone();
        tokio::task::spawn_blocking(move || {
            let timeout = timeout_override
                .unwrap_or_else(|| Duration::from_millis(config.timeout_ms))
                .min(Duration::from_millis(config.timeout_ms));
            let client = reqwest::blocking::Client::builder()
                .timeout(timeout)
                .build()
                .map_err(|e| e.to_string())?;
            let response = client.post(&config.endpoint)
                .bearer_auth(&config.api_key)
                .json(&json!({
                    "state": state,
                    "model": config.model,
                    "questions": Value::Object(questions),
                }))
                .send()
                .map_err(|e| {
                    if e.is_timeout() { "JEV 请求超时".into() } else { e.to_string() }
                })?;
            let status = response.status();
            let text = response.text().map_err(|e| e.to_string())?;
            if !status.is_success() {
                let excerpt = text.chars().take(500).collect::<String>()
                    .replace(&config.api_key, "[key]");
                return Err(format!("JEV HTTP {status}: {excerpt}"));
            }
            serde_json::from_str(&text).map_err(|e| format!("JEV 响应不是有效 JSON: {e}"))
        })
        .await
        .map_err(|e| e.to_string())?
    }

    /// Ollama adapts the same decision contracts through constrained JSON chat.
    /// Its scores are model-reported routing weights, not JEV-calibrated Noul
    /// probabilities; thresholds still bound which labels may be persisted.
    async fn ollama_chat_async(
        &self,
        state: String,
        instructions: String,
        timeout_override: Option<Duration>,
    ) -> Result<Value, String> {
        let config = self.config.clone();
        tokio::task::spawn_blocking(move || {
            let timeout = timeout_override
                .unwrap_or_else(|| Duration::from_millis(config.timeout_ms))
                .min(Duration::from_millis(config.timeout_ms));
            let client = reqwest::blocking::Client::builder()
                .timeout(timeout)
                .build()
                .map_err(|e| e.to_string())?;
            let request = client.post(format!("{}/api/chat", config.endpoint))
                .json(&json!({
                    "model": config.model,
                    "messages": [
                        { "role": "system", "content": instructions },
                        { "role": "user", "content": state },
                    ],
                    "stream": false,
                    "format": "json",
                    "options": { "temperature": 0 },
                }));
            // A preserved TypeSafe credential is never an Ollama credential.
            // Ollama authentication, if needed later, must use a separate secret.
            let response = request.send().map_err(|e| {
                if e.is_timeout() { "Ollama 请求超时".into() } else { e.to_string() }
            })?;
            let status = response.status();
            let text = response.text().map_err(|e| e.to_string())?;
            if !status.is_success() {
                let excerpt = text.chars().take(500).collect::<String>();
                let excerpt = if config.api_key.is_empty() {
                    excerpt
                } else {
                    excerpt.replace(&config.api_key, "[key]")
                };
                return Err(format!("Ollama HTTP {status}: {excerpt}"));
            }
            let envelope: Value = serde_json::from_str(&text)
                .map_err(|e| format!("Ollama 响应不是有效 JSON: {e}"))?;
            if let Some(error) = envelope.get("error").and_then(Value::as_str) {
                return Err(format!("Ollama: {error}"));
            }
            let content = envelope.pointer("/message/content").and_then(Value::as_str)
                .ok_or_else(|| "Ollama 响应缺少 message.content".to_string())?;
            let normalized = content.trim()
                .trim_start_matches("```json").trim_start_matches("```")
                .trim_end_matches("```").trim();
            serde_json::from_str(normalized)
                .map_err(|e| format!("Ollama 决策内容不是有效 JSON: {e}"))
        })
        .await
        .map_err(|e| e.to_string())?
    }

}

#[derive(Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub struct MemoryDomainTestResult {
    pub model: String,
    pub probability: f64,
}

#[tauri::command]
pub async fn memory_domain_config_test() -> Result<MemoryDomainTestResult, String> {
    let store = domain_store()?;
    let config = load_config(&store)?;
    let client = DecisionClient::new(config)?;
    if client.config.provider == "ollama" {
        let value = client.ollama_chat_async(
            "The user is preparing for a job interview.".into(),
            "Return only JSON with one numeric field probability (0..1) indicating whether this query belongs to job_search.".into(),
            None,
        ).await?;
        return Ok(MemoryDomainTestResult {
            model: client.config.model.clone(),
            probability: value.get("probability").and_then(Value::as_f64)
                .ok_or_else(|| "Ollama 测试响应缺少 probability".to_string())?,
        });
    }
    let mut questions = Map::new();
    questions.insert("test".into(), json!({
        "type": "noul",
        "instructions": "The user is preparing for a job interview."
    }));
    let value = client.system_one_async(
        "The user is preparing for a job interview.".into(),
        questions,
        None,
    ).await?;
    Ok(MemoryDomainTestResult {
        model: value.get("model").and_then(Value::as_str).unwrap_or("jev").into(),
        probability: value.pointer("/answers/test/noul").and_then(Value::as_f64)
            .ok_or_else(|| "JEV 测试响应缺少 noul".to_string())?,
    })
}

fn select_assignments(
    mut scores: Vec<(String, f64)>,
    threshold: f64,
) -> Vec<MemoryDomainAssignment> {
    scores.retain(|(_, score)| score.is_finite() && *score >= threshold);
    scores.sort_by(|left, right| right.1.total_cmp(&left.1).then_with(|| left.0.cmp(&right.0)));
    scores.truncate(3);
    scores.into_iter()
        .map(|(domain_id, weight)| MemoryDomainAssignment { domain_id, weight })
        .collect()
}

/// Decision requests classify the gist of a memory, not its full transcript.
/// Extracted L1 items are normally short, but imported/legacy rows can be far
/// larger; one oversized row must not make the whole classification batch fail.
const DECISION_MEMORY_MAX_CHARS: usize = 4_000;

fn decision_memory_excerpt(memory: &str) -> String {
    if memory.chars().count() <= DECISION_MEMORY_MAX_CHARS {
        return memory.to_string();
    }
    let mut excerpt: String = memory.chars().take(DECISION_MEMORY_MAX_CHARS).collect();
    excerpt.push_str("\n[truncated for domain classification]");
    excerpt
}

async fn ollama_classify_one_async(
    client: &DecisionClient,
    item: &LocalMemorySnapshot,
    domains: &[MemoryDomain],
    threshold: f64,
) -> Result<Vec<MemoryDomainAssignment>, String> {
    let state = serde_json::to_string(&json!({
        "memory": {
            "id": item.id,
            "memory": decision_memory_excerpt(&item.memory),
            "type": item.memory_type,
            "durability": item.durability,
        },
        "domains": domains.iter().map(|domain| json!({
            "id": domain.id, "name": domain.name,
            "description": domain.description, "aliases": domain.aliases,
        })).collect::<Vec<_>>(),
    })).map_err(|e| e.to_string())?;
    let response = client.ollama_chat_async(
        state,
        "Classify this memory into zero to three of the supplied domains. Use only supplied domain ids; never invent domains or rewrite the memory. general is only for genuinely cross-domain durable preferences or constraints. Return only JSON: {\"domains\":[{\"id\":\"domain id\",\"weight\":0.0}]}. Weight is a 0..1 domain-association score. Use an empty domains array when no label is reliable.".into(),
        None,
    ).await?;
    let rows = response.get("domains").and_then(Value::as_array)
        .ok_or_else(|| "Ollama 打标响应缺少 domains 数组".to_string())?;
    let scores = rows.iter().filter_map(|assignment| {
        let id = assignment.get("id").and_then(Value::as_str)?;
        let weight = assignment.get("weight").or_else(|| assignment.get("probability"))?.as_f64()?;
        domains.iter().any(|domain| domain.id == id).then(|| (id.to_string(), weight))
    }).collect::<Vec<_>>();
    Ok(select_assignments(scores, threshold))
}

async fn jev_classify_one_async(
    client: &DecisionClient,
    item: &LocalMemorySnapshot,
    domains: &[MemoryDomain],
    threshold: f64,
) -> Result<Vec<MemoryDomainAssignment>, String> {
    let state = serde_json::to_string(&json!({
        "id": item.id,
        "memory": decision_memory_excerpt(&item.memory),
        "type": item.memory_type,
        "durability": item.durability,
    })).map_err(|e| e.to_string())?;
    let mut questions = Map::new();
    for (domain_index, domain) in domains.iter().enumerate() {
        questions.insert(format!("d{domain_index}"), json!({
            "type": "noul",
            "instructions": {
                "question": "Does the supplied memory belong to this domain?",
                "domain": domain.name,
                "definition": domain.description,
                "aliases": domain.aliases,
            },
            "criteria": {
                "true": "The memory is reusable in this domain, including genuinely overlapping domains.",
                "false": "The connection is only incidental wording or the memory belongs elsewhere."
            }
        }));
    }
    let response = client.system_one_async(state, questions, None).await?;
    let answers = response.get("answers").and_then(Value::as_object)
        .ok_or_else(|| "JEV 响应缺少 answers".to_string())?;
    let mut scores = Vec::with_capacity(domains.len());
    for (domain_index, domain) in domains.iter().enumerate() {
        let key = format!("d{domain_index}");
        if let Some(probability) = answers.get(&key)
            .and_then(|answer| answer.get("noul"))
            .and_then(Value::as_f64)
        {
            scores.push((domain.id.clone(), probability));
        }
    }
    Ok(select_assignments(scores, threshold))
}

/// Classify memories one request per L1. JEV evaluates all domain questions for
/// one memory in parallel, but batching multiple memories multiplies both state
/// and question tokens and can exceed the service's request budget.
async fn classify_memory_batch_async(
    store: &TelemetryStore,
    items: &[LocalMemorySnapshot],
) -> Result<Vec<Vec<MemoryDomainAssignment>>, String> {
    let config = load_config(store)?;
    let client = DecisionClient::new(config.clone())?;
    let domains = store.memory_domains()?.into_iter()
        .filter(|domain| domain.enabled && domain.id != "unclassified")
        .collect::<Vec<_>>();
    if domains.is_empty() {
        return Err("没有可用于决策模型打标的领域".into());
    }
    let mut assignments = Vec::with_capacity(items.len());
    for item in items {
        if config.provider == "ollama" {
            assignments.push(ollama_classify_one_async(
                &client, item, &domains, config.tag_threshold,
            ).await?);
        } else {
            assignments.push(jev_classify_one_async(
                &client, item, &domains, config.tag_threshold,
            ).await?);
        }
    }
    Ok(assignments)
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub struct DomainRoute {
    pub mode: String,
    pub active_domains: Vec<String>,
    pub probabilities: HashMap<String, f64>,
    pub confidence: Option<f64>,
    pub fallback: Option<String>,
}

impl DomainRoute {
    fn global(mode: &str, fallback: Option<&str>) -> Self {
        Self {
            mode: mode.into(), active_domains: Vec::new(), probabilities: HashMap::new(),
            confidence: None, fallback: fallback.map(str::to_string),
        }
    }
}

async fn ollama_route_query_async(
    client: &DecisionClient,
    config: &MemoryDomainConfig,
    query: &str,
    domains: &[MemoryDomain],
    timeout_ms: Option<u64>,
) -> Result<DomainRoute, String> {
    let state = serde_json::to_string(&json!({
        "query": query,
        "domains": domains.iter().map(|domain| json!({
            "id": domain.id, "name": domain.name,
            "description": domain.description, "aliases": domain.aliases,
        })).collect::<Vec<_>>(),
    })).map_err(|e| e.to_string())?;
    let response = client.ollama_chat_async(
        state,
        "Select up to two memory domains to search for the query. Use only supplied domain ids. Return an empty domains array when the query is cross-domain or no domain is clear. Return only JSON: {\"domains\":[{\"id\":\"domain id\",\"probability\":0.0}],\"confidence\":0.0}. Scores must be between 0 and 1.".into(),
        timeout_ms.map(Duration::from_millis),
    ).await?;
    let probabilities = response.get("domains").and_then(Value::as_array)
        .ok_or_else(|| "Ollama 路由响应缺少 domains 数组".to_string())?
        .iter().filter_map(|row| {
            let id = row.get("id").and_then(Value::as_str)?;
            let probability = row.get("probability").or_else(|| row.get("weight"))?.as_f64()?;
            domains.iter().any(|domain| domain.id == id).then(|| (id.to_string(), probability))
        }).collect::<HashMap<_, _>>();
    let confidence = response.get("confidence").and_then(Value::as_f64);
    let mut ranked = probabilities.iter()
        .map(|(domain, probability)| (domain.clone(), *probability))
        .collect::<Vec<_>>();
    ranked.sort_by(|left, right| right.1.total_cmp(&left.1).then_with(|| left.0.cmp(&right.0)));
    let Some((first, first_probability)) = ranked.first().cloned() else {
        return Ok(DomainRoute {
            mode: "auto".into(), active_domains: Vec::new(), probabilities,
            confidence, fallback: Some("low_confidence".into()),
        });
    };
    if first_probability < config.route_threshold {
        return Ok(DomainRoute {
            mode: "auto".into(), active_domains: Vec::new(), probabilities,
            confidence, fallback: Some("low_confidence".into()),
        });
    }
    let mut active_domains = vec![first];
    if let Some((second, second_probability)) = ranked.get(1) {
        if *second_probability >= config.second_threshold && *second_probability >= first_probability * 0.8 {
            active_domains.push(second.clone());
        }
    }
    Ok(DomainRoute {
        mode: "auto".into(), active_domains, probabilities, confidence, fallback: None,
    })
}

pub(crate) async fn route_query_async(
    store: &TelemetryStore,
    query: &str,
    explicit_domains: Option<&[String]>,
    mode: &str,
    timeout_ms: Option<u64>,
) -> Result<DomainRoute, String> {
    if !matches!(mode, "auto" | "manual" | "off") {
        return Err("domain_mode 只能是 auto / manual / off".into());
    }
    if mode == "off" {
        if explicit_domains.is_some_and(|domains| !domains.is_empty()) {
            return Err("domain_mode=off 时不能传 domains".into());
        }
        return Ok(DomainRoute::global("off", None));
    }
    let explicit = explicit_domains.unwrap_or(&[]);
    if mode == "manual" || !explicit.is_empty() {
        if explicit.is_empty() || explicit.len() > 2 {
            return Err("manual 模式必须显式指定 1–2 个 domains".into());
        }
        let available = store.memory_domains()?;
        for requested in explicit {
            if !available.iter().any(|domain| &domain.id == requested && domain.enabled) {
                return Err(format!("未知或已停用领域: {requested}"));
            }
        }
        return Ok(DomainRoute {
            mode: "manual".into(), active_domains: explicit.to_vec(),
            probabilities: HashMap::new(), confidence: Some(1.0), fallback: None,
        });
    }
    let config = load_config(store)?;
    let client = match DecisionClient::new(config.clone()) {
        Ok(client) => client,
        Err(error) => {
            eprintln!("[memory-domain] query routing unavailable: {error}");
            let fallback = if config.provider == "ollama" { "ollama_unavailable" } else { "jev_unavailable" };
            return Ok(DomainRoute::global("auto", Some(fallback)));
        }
    };
    let domains = store.memory_domains()?.into_iter()
        .filter(|domain| domain.enabled && !matches!(domain.id.as_str(), "general" | "unclassified"))
        .collect::<Vec<_>>();
    if domains.is_empty() {
        return Ok(DomainRoute::global("auto", Some("no_domains")));
    }
    if config.provider == "ollama" {
        return match ollama_route_query_async(&client, &config, query, &domains, timeout_ms).await {
            Ok(route) => Ok(route),
            Err(error) => {
                eprintln!("[memory-domain] Ollama routing failed; using global recall: {error}");
                Ok(DomainRoute::global("auto", Some("ollama_error")))
            }
        };
    }
    let mut criteria = Map::new();
    for domain in &domains {
        criteria.insert(domain.id.clone(), json!({
            "name": domain.name,
            "definition": domain.description,
            "aliases": domain.aliases,
        }));
    }
    criteria.insert(NONE_DOMAIN.into(), json!(
        "The query has no clear domain, is genuinely cross-domain, or the available criteria are insufficient."
    ));
    let mut questions = Map::new();
    questions.insert("domain_route".into(), json!({
        "type": "choice",
        "instructions": "Choose the memory domain that should be searched for this query. Choose a domain only when its definition clearly matches.",
        "criteria": Value::Object(criteria),
    }));
    let response = match client.system_one_async(
        query.to_string(),
        questions,
        timeout_ms.map(Duration::from_millis),
    ).await {
        Ok(response) => response,
        Err(error) => {
            eprintln!("[memory-domain] query routing failed; using global recall: {error}");
            return Ok(DomainRoute::global("auto", Some("jev_error")));
        }
    };
    let Some(answer) = response.pointer("/answers/domain_route").cloned() else {
        eprintln!("[memory-domain] JEV response omitted domain_route; using global recall");
        return Ok(DomainRoute::global("auto", Some("jev_invalid_response")));
    };
    let probabilities = answer.get("probabilities").and_then(Value::as_object)
        .map(|values| values.iter().filter_map(|(key, value)| {
            value.as_f64().map(|probability| (key.clone(), probability))
        }).collect::<HashMap<_, _>>()).unwrap_or_default();
    let confidence = answer.get("confidence").and_then(Value::as_f64);
    let mut ranked = probabilities.iter()
        .filter(|(domain, _)| domain.as_str() != NONE_DOMAIN)
        .map(|(domain, probability)| (domain.clone(), *probability))
        .collect::<Vec<_>>();
    ranked.sort_by(|left, right| right.1.total_cmp(&left.1).then_with(|| left.0.cmp(&right.0)));
    let no_clear_domain = answer.get("choice").and_then(Value::as_str) == Some(NONE_DOMAIN);
    let Some((first, first_probability)) = ranked.first().cloned() else {
        return Ok(DomainRoute::global("auto", Some("jev_no_route")));
    };
    if no_clear_domain || first_probability < config.route_threshold {
        return Ok(DomainRoute {
            mode: "auto".into(), active_domains: Vec::new(), probabilities,
            confidence, fallback: Some("low_confidence".into()),
        });
    }
    let mut active_domains = vec![first];
    if let Some((second, second_probability)) = ranked.get(1) {
        if *second_probability >= config.second_threshold && *second_probability >= first_probability * 0.8 {
            active_domains.push(second.clone());
        }
    }
    Ok(DomainRoute {
        mode: "auto".into(), active_domains, probabilities, confidence, fallback: None,
    })
}

static BACKFILL_RUNNING: AtomicBool = AtomicBool::new(false);

#[derive(Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub struct MemoryDomainBackfillResult {
    pub processed: u32,
    pub tagged: u32,
    pub remaining: u32,
    pub message: String,
}

/// Process a bounded page. Repeating the command resumes from untagged rows;
/// no long-lived cursor or automatic whole-library upload is required.
#[tauri::command]
pub async fn memory_domain_backfill(max_items: Option<u32>, retag: Option<bool>) -> Result<MemoryDomainBackfillResult, String> {
    if BACKFILL_RUNNING.swap(true, Ordering::AcqRel) {
        return Err("已有领域分类整理任务正在运行".into());
    }
    let result = memory_domain_backfill_inner(
        max_items.unwrap_or(40).clamp(1, 200),
        retag.unwrap_or(false),
    ).await;
    BACKFILL_RUNNING.store(false, Ordering::Release);
    result
}

async fn memory_domain_backfill_inner(max_items: u32, retag: bool) -> Result<MemoryDomainBackfillResult, String> {
    let store = domain_store()?;
    let config = load_config(&store)?;
    DecisionClient::new(config)?;
    let items = if retag {
        store.local_l1_memory_snapshot()?.into_iter()
            .filter(|item| !item.user_defined)
            .collect::<Vec<_>>()
    } else {
        store.l1_memories_for_domain_classification(max_items, false)?
    };
    let total_candidates = items.len() as u32;
    let total = items.len() as u32;
    let mut processed = 0u32;
    let mut tagged = 0u32;
    for chunk in items.chunks(8) {
        let store_clone = store.clone();
        let chunk_owned = chunk.to_vec();
        let assignments = classify_memory_batch_async(&store_clone, &chunk_owned).await?;
        for (item, domains) in chunk.iter().zip(assignments) {
            tagged += if domains.is_empty() { 0 } else { 1 };
            store.replace_backfill_memory_domains(&item.id, &domains)?;
            processed += 1;
        }
    }
    let remaining = if retag {
        total_candidates.saturating_sub(processed)
    } else {
        store.count_l1_memories_for_domain_classification(false)?
    };
    Ok(MemoryDomainBackfillResult {
        processed, tagged, remaining,
        message: if total == 0 { "没有待分类记忆".into() } else {
            format!("已处理 {processed} 条，{tagged} 条获得领域标签")
        },
    })
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct MemoryDomainUpsertInput {
    pub id: Option<String>,
    pub name: String,
    pub description: String,
    #[serde(default)]
    pub aliases: Vec<String>,
    pub enabled: bool,
}

#[tauri::command]
pub fn memory_domain_upsert(input: MemoryDomainUpsertInput) -> Result<MemoryDomain, String> {
    let name = input.name.trim();
    if name.is_empty() || name.chars().count() > 80 {
        return Err("主题名称需为 1–80 个字符".into());
    }
    if input.description.chars().count() > 500 {
        return Err("主题定义不能超过 500 个字符".into());
    }
    let mut aliases = input.aliases.into_iter()
        .map(|alias| alias.trim().to_string())
        .filter(|alias| !alias.is_empty())
        .collect::<Vec<_>>();
    aliases.sort();
    aliases.dedup();
    if aliases.len() > 20 || aliases.iter().any(|alias| alias.chars().count() > 40) {
        return Err("主题别名最多 20 个，每个不超过 40 字符".into());
    }
    if input.id.as_deref().is_some_and(|id| id.trim().is_empty()) {
        return Err("主题 ID 不能为空".into());
    }
    domain_store()?.save_memory_domain(
        input.id.as_deref(),
        name,
        input.description.trim(),
        &aliases,
        input.enabled,
    )
}

#[tauri::command]
pub fn memory_domain_delete(id: String) -> Result<(), String> {
    if id.trim().is_empty() {
        return Err("主题 ID 不能为空".into());
    }
    domain_store()?.delete_memory_domain(id.trim())
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub struct MemoryDomainClassifyBatchResult {
    pub total: u32,
    pub processed: u32,
}

async fn tag_one_batch_with_store(store: &TelemetryStore, items: &[LocalMemorySnapshot]) -> Result<usize, String> {
    for item in items {
        let mut batch = classify_memory_batch_async(store, std::slice::from_ref(item)).await?;
        let assignments = batch.pop().ok_or("分类结果缺少当前记忆")?;
        store.clear_memory_domain_links(&item.id)?;
        store.insert_memory_domain_links(&item.id, &assignments)?;
    }
    Ok(items.len())
}

/// Classify exactly one bounded set of L1 memories. The caller owns batching
/// and progress reporting; this command is retryable and idempotent for the
/// provided ids.
#[tauri::command]
pub async fn memory_domain_classify_batch(
    memory_ids: Vec<String>,
) -> Result<MemoryDomainClassifyBatchResult, String> {
    if memory_ids.is_empty() {
        return Err("memory_ids 不能为空".into());
    }
    if memory_ids.len() > 200 {
        return Err("memory_ids 最多一次处理 200 条".into());
    }
    let store = domain_store()?;
    let config = load_config(&store)?;
    DecisionClient::new(config)?;
    let durable = store.local_l1_memory_snapshot()?.into_iter()
        .filter(|item| !item.user_defined)
        .collect::<Vec<_>>();
    let mut seen = HashSet::new();
    for id in &memory_ids {
        if !seen.insert(id.as_str()) {
            return Err("memory_ids 存在重复项".into());
        }
    }
    let by_id = durable.iter()
        .map(|item| (item.id.as_str(), item))
        .collect::<HashMap<_, _>>();
    for id in &memory_ids {
        if by_id.get(id.as_str()).is_none() {
            return Err(format!("memory_id 不存在或属于用户手动添加记忆: {id}"));
        }
    }
    let items = memory_ids.iter()
        .filter_map(|id| by_id.get(id.as_str()))
        .map(|item| (*item).clone())
        .collect::<Vec<_>>();
    let processed = tag_one_batch_with_store(&store, &items).await? as u32;
    Ok(MemoryDomainClassifyBatchResult { total: processed, processed })
}

#[tauri::command]
pub async fn memory_domain_classify_snapshot_ids(
    snapshot_ids: Vec<String>,
) -> Result<MemoryDomainClassifyBatchResult, String> {
    if snapshot_ids.is_empty() {
        return Err("snapshot_ids 不能为空".into());
    }
    let store = domain_store()?;
    let memories = store.local_l1_memory_snapshot()?;
    let mut ids = Vec::with_capacity(snapshot_ids.len());
    for event_key in snapshot_ids {
        let snapshot_memories = memories.iter()
            .filter(|item| item.source_event_key == event_key && !item.user_defined)
            .collect::<Vec<_>>();
        if snapshot_memories.is_empty() {
            return Err(format!("未找到对应自动整理记忆: {event_key}"));
        }
        ids.extend(snapshot_memories.into_iter().map(|item| item.id.clone()));
    }
    memory_domain_classify_batch(ids).await
}

#[tauri::command]
pub fn memory_domain_links(
    store: tauri::State<'_, crate::telemetry_store::TelemetryStore>,
    memory_id: String,
) -> Result<Vec<crate::telemetry_store::MemoryDomainAssignment>, String> {
    let conn = store.conn.lock().map_err(|_| "telemetry store lock poisoned".to_string())?;
    let mut statement = conn.prepare(
        "SELECT domain_id, weight FROM local_memory_domains WHERE memory_id = ?1 ORDER BY weight DESC",
    ).map_err(|e| e.to_string())?;
    let rows = statement.query_map([memory_id.trim()], |row| {
        Ok(crate::telemetry_store::MemoryDomainAssignment {
            domain_id: row.get(0)?,
            weight: row.get(1)?,
        })
    }).map_err(|e| e.to_string())?;
    rows.collect::<Result<Vec<_>, _>>().map_err(|e| e.to_string())
}

#[tauri::command]
pub fn memory_domain_list() -> Result<Vec<crate::telemetry_store::MemoryDomain>, String> {
    domain_store()?.memory_domains()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tagging_keeps_at_most_three_domains_above_threshold() {
        let selected = select_assignments(vec![
            ("job_search".into(), 0.92),
            ("study".into(), 0.81),
            ("life".into(), 0.75),
            ("work".into(), 0.72),
        ], 0.7);
        assert_eq!(selected.len(), 3);
        assert_eq!(selected[0].domain_id, "job_search");
        assert_eq!(selected[1].domain_id, "study");
        assert_eq!(selected[2].domain_id, "life");
    }

    #[test]
    fn tagging_allows_zero_labels() {
        assert!(select_assignments(vec![("life".into(), 0.4)], 0.7).is_empty());
    }

    #[test]
    fn scoped_keyword_recall_excludes_other_domains() {
        let store = TelemetryStore::in_memory();
        for (id, memory) in [
            ("job", "用户正在准备算法面试"),
            ("life", "用户正在准备周末出行"),
        ] {
            store.conn.lock().unwrap().execute(
                "INSERT INTO local_memory_items
                 (id, source_event_key, ordinal, memory, memory_type, durability, source,
                  session_id, user_defined, created_at, updated_at)
                 VALUES (?1, ?1, 0, ?2, 'fact', 'short_term', 'test', 'test', 0, 'now', 'now')",
                rusqlite::params![id, memory],
            ).unwrap();
        }
        store.replace_backfill_memory_domains("job", &[MemoryDomainAssignment {
            domain_id: "job_search".into(), weight: 0.95,
        }]).unwrap();
        store.replace_backfill_memory_domains("life", &[MemoryDomainAssignment {
            domain_id: "life".into(), weight: 0.95,
        }]).unwrap();
        let hits = store.search_local_l1_memories_scoped(
            "准备",
            10,
            Some(&["job_search".into()]),
        ).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].id, "job");
        assert!(tauri::async_runtime::block_on(route_query_async(&store, "任意查询", None, "off", None))
            .unwrap().active_domains.is_empty());
    }

    #[test]
    fn deleting_builtin_domain_preserves_l1_and_allows_missing_fallback() {
        let store = TelemetryStore::in_memory();
        store.conn.lock().unwrap().execute(
            "INSERT INTO local_memory_items
             (id, source_event_key, ordinal, memory, memory_type, durability, source,
              session_id, user_defined, created_at, updated_at)
             VALUES ('memory', 'memory', 0, '保留这条记忆', 'fact', 'short_term', 'test',
                     'test', 0, 'now', 'now')",
            [],
        ).unwrap();
        store.delete_memory_domain("job_search").unwrap();
        store.delete_memory_domain("unclassified").unwrap();
        store.replace_backfill_memory_domains("memory", &[]).unwrap();
        assert_eq!(store.local_l1_memory_snapshot().unwrap().len(), 1);
        assert!(store.memory_ids_for_domains(&["unclassified".into()]).unwrap().is_empty());
        let custom = store.save_memory_domain(
            None,
            "阅读",
            "书籍与长文阅读",
            &["读书".into()],
            true,
        ).unwrap();
        assert_eq!(custom.kind, "custom");
    }
}
