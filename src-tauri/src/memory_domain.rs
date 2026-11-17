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
fn default_tag_threshold() -> f64 { 0.60 }
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

pub(crate) fn load_config(store: &TelemetryStore) -> Result<MemoryDomainConfig, String> {
    let saved = store.app_setting_get_readonly::<MemoryDomainConfig>(JEV_CONFIG_SETTING_KEY);
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

    fn http_client(&self, timeout_override: Option<Duration>) -> Result<reqwest::Client, String> {
        let configured = Duration::from_millis(self.config.timeout_ms);
        let timeout = timeout_override.unwrap_or(configured).min(configured);
        let mut builder = reqwest::Client::builder().timeout(timeout);
        // Local model traffic must stay local; cloud decisions retain the
        // user's proxy settings, which can be required for provider access.
        let loopback = reqwest::Url::parse(&self.config.endpoint).ok().is_some_and(|url| {
            url.host_str().is_some_and(|host| host.eq_ignore_ascii_case("localhost")
                || host.trim_matches(['[', ']']).parse::<std::net::IpAddr>().is_ok_and(|ip| ip.is_loopback()))
        });
        if loopback { builder = builder.no_proxy(); }
        builder.build().map_err(|error| error.to_string())
    }

    async fn system_one_async(
        &self,
        state: String,
        questions: Map<String, Value>,
        timeout_override: Option<Duration>,
    ) -> Result<Value, String> {
        let config = &self.config;
        let client = self.http_client(timeout_override)?;
        // Native async I/O is cancelled when the routing deadline drops this
        // future; dropping a spawn_blocking handle cannot stop its HTTP job.
        let response = client.post(&config.endpoint)
            .bearer_auth(&config.api_key)
            .json(&json!({ "state": state, "model": config.model, "questions": Value::Object(questions) }))
            .send().await.map_err(|error| {
                if error.is_timeout() { "JEV 请求超时".into() } else { error.to_string() }
            })?;
        let status = response.status();
        let text = response.text().await.map_err(|error| {
            if error.is_timeout() { "JEV 请求超时".into() } else { error.to_string() }
        })?;
        if !status.is_success() {
            let excerpt = text.chars().take(500).collect::<String>().replace(&config.api_key, "[key]");
            return Err(format!("JEV HTTP {status}: {excerpt}"));
        }
        serde_json::from_str(&text).map_err(|error| format!("JEV 响应不是有效 JSON: {error}"))
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
        let config = &self.config;
        let client = self.http_client(timeout_override)?;
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
        let response = request.send().await.map_err(|error| {
            if error.is_timeout() { "Ollama 请求超时".into() } else { error.to_string() }
        })?;
        let status = response.status();
        let text = response.text().await.map_err(|error| {
            if error.is_timeout() { "Ollama 请求超时".into() } else { error.to_string() }
        })?;
        if !status.is_success() {
            let excerpt = text.chars().take(500).collect::<String>();
            let excerpt = if config.api_key.is_empty() { excerpt } else { excerpt.replace(&config.api_key, "[key]") };
            return Err(format!("Ollama HTTP {status}: {excerpt}"));
        }
        let envelope: Value = serde_json::from_str(&text)
            .map_err(|error| format!("Ollama 响应不是有效 JSON: {error}"))?;
        if let Some(error) = envelope.get("error").and_then(Value::as_str) { return Err(format!("Ollama: {error}")); }
        let content = envelope.pointer("/message/content").and_then(Value::as_str)
            .ok_or_else(|| "Ollama 响应缺少 message.content".to_string())?;
        let normalized = content.trim().trim_start_matches("```json").trim_start_matches("```")
            .trim_end_matches("```").trim();
        serde_json::from_str(normalized).map_err(|error| format!("Ollama 决策内容不是有效 JSON: {error}"))
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
    route_query_with_config_async(store, query, explicit_domains, mode, timeout_ms, None).await
}

pub(crate) async fn route_query_with_config_async(
    store: &TelemetryStore, query: &str, explicit_domains: Option<&[String]>, mode: &str,
    timeout_ms: Option<u64>, evaluation_config: Option<MemoryDomainConfig>,
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
    let config = match evaluation_config { Some(config) => config, None => load_config(store)? };
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
    for item in &items {
        let mut batch = classify_memory_batch_async(&store, std::slice::from_ref(item)).await?;
        let domains = batch.pop().ok_or("分类结果缺少当前记忆")?;
        if save_classification_async(&store, item, &domains).await? {
            tagged += if domains.is_empty() { 0 } else { 1 };
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

#[derive(Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub struct MemoryDomainClassificationStatus {
    pub total: u32,
    pub processed: u32,
    pub pending_ids: Vec<String>,
}

fn classification_status_with_store(store: &TelemetryStore) -> Result<MemoryDomainClassificationStatus, String> {
    let completed = store.completed_memory_domain_classification_ids()?;
    let items = store.local_l1_memory_snapshot()?.into_iter()
        .filter(|item| !item.user_defined)
        .collect::<Vec<_>>();
    let pending_ids = items.iter()
        .filter(|item| !completed.contains(&item.id))
        .map(|item| item.id.clone())
        .collect::<Vec<_>>();
    Ok(MemoryDomainClassificationStatus {
        total: items.len() as u32,
        processed: (items.len() - pending_ids.len()) as u32,
        pending_ids,
    })
}

/// Read persisted progress without calling the decision model. The page can
/// reconstruct its queue after an interruption, including partially saved batches.
#[tauri::command]
pub fn memory_domain_classification_status() -> Result<MemoryDomainClassificationStatus, String> {
    classification_status_with_store(&domain_store()?)
}

// SQLite's busy timeout and bounded retries may wait for an independent MCP
// writer. Keep that blocking wait off Tauri's async worker threads.
async fn save_classification_async(
    store: &TelemetryStore,
    item: &LocalMemorySnapshot,
    assignments: &[MemoryDomainAssignment],
) -> Result<bool, String> {
    let store = store.clone();
    let item = item.clone();
    let assignments = assignments.to_vec();
    tauri::async_runtime::spawn_blocking(move || {
        store.save_memory_domain_classification(&item, &assignments)
    }).await.map_err(|error| format!("分类保存任务执行失败：{error}"))?
}

async fn tag_one_batch_with_store(store: &TelemetryStore, items: &[LocalMemorySnapshot]) -> Result<usize, String> {
    let completed = store.completed_memory_domain_classification_ids()?;
    let mut processed = 0;
    for item in items {
        if completed.contains(&item.id) {
            continue;
        }
        let mut batch = classify_memory_batch_async(store, std::slice::from_ref(item)).await?;
        let assignments = batch.pop().ok_or("分类结果缺少当前记忆")?;
        if save_classification_async(store, item, &assignments).await? {
            processed += 1;
        }
    }
    Ok(processed)
}

/// Classify one bounded set of L1 memories. Queue ids may have disappeared or
/// become ineligible since the caller read progress; skip those and keep working.
/// Saved checkpoints make retries idempotent for unchanged memories.
#[tauri::command]
pub async fn memory_domain_classify_batch(
    memory_ids: Vec<String>,
) -> Result<MemoryDomainClassifyBatchResult, String> {
    classify_batch_with_store(&domain_store()?, &memory_ids).await
}

async fn classify_batch_with_store(
    store: &TelemetryStore,
    memory_ids: &[String],
) -> Result<MemoryDomainClassifyBatchResult, String> {
    if memory_ids.is_empty() {
        return Err("memory_ids 不能为空".into());
    }
    if memory_ids.len() > 200 {
        return Err("memory_ids 最多一次处理 200 条".into());
    }
    let mut seen = HashSet::new();
    for id in memory_ids {
        if !seen.insert(id.as_str()) {
            return Err("memory_ids 存在重复项".into());
        }
    }
    let durable = store.local_l1_memory_snapshot()?.into_iter()
        .filter(|item| !item.user_defined)
        .collect::<Vec<_>>();
    let by_id = durable.iter()
        .map(|item| (item.id.as_str(), item))
        .collect::<HashMap<_, _>>();
    let items = memory_ids.iter()
        .filter_map(|id| by_id.get(id.as_str()))
        .map(|item| (*item).clone())
        .collect::<Vec<_>>();
    let processed = tag_one_batch_with_store(store, &items).await? as u32;
    Ok(MemoryDomainClassifyBatchResult { total: items.len() as u32, processed })
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
pub async fn memory_domain_links_map(
    store: tauri::State<'_, crate::telemetry_store::TelemetryStore>,
) -> Result<HashMap<String, Vec<String>>, String> {
    let store = store.inner().clone();
    tauri::async_runtime::spawn_blocking(move || store.with_read_connection(|conn| {
    let mut statement = conn.prepare(
        "SELECT memory_id, domain_id FROM local_memory_domains ORDER BY memory_id, weight DESC",
    ).map_err(|e| e.to_string())?;
    let rows = statement.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    }).map_err(|e| e.to_string())?;
    let mut map: HashMap<String, Vec<String>> = HashMap::new();
    for row in rows {
        let (memory_id, domain_id) = row.map_err(|e| e.to_string())?;
        let entry = map.entry(memory_id).or_default();
        if !entry.contains(&domain_id) {
            entry.push(domain_id);
        }
    }
    Ok(map)
    })).await.map_err(|error| error.to_string())?
}

#[tauri::command]
pub async fn memory_domain_list() -> Result<Vec<crate::telemetry_store::MemoryDomain>, String> {
    tauri::async_runtime::spawn_blocking(|| domain_store()?.memory_domains())
        .await.map_err(|error| error.to_string())?
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One bounded local HTTP request; None waits for client cancellation.
    fn decision_server(reply: Option<(Duration, Value)>) -> (
        String, std::sync::mpsc::Receiver<String>, std::thread::JoinHandle<bool>,
    ) {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        listener.set_nonblocking(true).unwrap();
        let (sender, received) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            let deadline = std::time::Instant::now() + Duration::from_secs(3);
            let mut stream = loop {
                match listener.accept() {
                    Ok((stream, _)) => break stream,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        if std::time::Instant::now() >= deadline { return false; }
                        std::thread::sleep(Duration::from_millis(5));
                    }
                    Err(_) => return false,
                }
            };
            // Accepted sockets inherit nonblocking mode on Windows.
            stream.set_nonblocking(false).unwrap();
            stream.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
            let mut bytes = Vec::new();
            let mut buffer = [0; 4096];
            let header_end = loop {
                match stream.read(&mut buffer) {
                    Ok(0) | Err(_) => return false,
                    Ok(count) => bytes.extend_from_slice(&buffer[..count]),
                }
                if bytes.len() > 64 * 1024 { return false; }
                if let Some(index) = bytes.windows(4).position(|chunk| chunk == b"\r\n\r\n") { break index + 4; }
            };
            let length = String::from_utf8_lossy(&bytes[..header_end]).lines().find_map(|line| {
                line.to_ascii_lowercase().strip_prefix("content-length:").and_then(|value| value.trim().parse::<usize>().ok())
            }).unwrap_or(0);
            if length > 64 * 1024 { return false; }
            while bytes.len() < header_end + length {
                match stream.read(&mut buffer) {
                    Ok(0) | Err(_) => return false,
                    Ok(count) => bytes.extend_from_slice(&buffer[..count]),
                }
            }
            let _ = sender.send(String::from_utf8_lossy(&bytes).into_owned());
            if let Some((delay, body)) = reply {
                std::thread::sleep(delay);
                let body = body.to_string();
                let response = format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
                stream.write_all(response.as_bytes()).is_ok()
            } else {
                // A cancelled async request must release its socket rather
                // than retaining a blocking HTTP job for the full timeout.
                match stream.read(&mut buffer) {
                    Ok(0) => true,
                    Err(error) => matches!(error.kind(), std::io::ErrorKind::ConnectionReset | std::io::ErrorKind::ConnectionAborted),
                    _ => false,
                }
            }
        });
        (endpoint, received, worker)
    }

    fn decision_config(provider: &str, endpoint: String) -> MemoryDomainConfig {
        MemoryDomainConfig { enabled: true, provider: provider.into(), endpoint,
            api_key: "test-jev-secret".into(), timeout_ms: 5_000, ..Default::default() }
    }

    #[test]
    fn routing_accepts_a_valid_decision_that_takes_more_than_one_second() {
        let (endpoint, _, worker) = decision_server(Some((Duration::from_millis(1_150), json!({
            "answers": { "domain_route": { "choice": "work", "probabilities": { "work": 0.99 }, "confidence": 0.99 } }
        }))));
        let store = TelemetryStore::in_memory();
        store.app_setting_set(JEV_CONFIG_SETTING_KEY, &decision_config("jev", endpoint)).unwrap();
        let route = tauri::async_runtime::block_on(route_query_async(
            &store, "Organize reusable project workflows", None, "auto", Some(2_500),
        )).unwrap();
        assert_eq!(route.active_domains, ["work"]);
        assert!(route.fallback.is_none());
        assert!(worker.join().unwrap());
    }

    #[test]
    fn routing_cancellation_closes_the_in_flight_http_request() {
        let (endpoint, received, worker) = decision_server(None);
        let client = DecisionClient::new(decision_config("jev", endpoint)).unwrap();
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let task = runtime.spawn(async move {
            client.system_one_async("test routing".into(), Map::new(), None).await
        });
        received.recv_timeout(Duration::from_secs(3)).unwrap();
        task.abort();
        assert!(runtime.block_on(task).unwrap_err().is_cancelled());
        assert!(worker.join().unwrap(), "cancelled routing must not leave blocking HTTP work running");
    }

    #[test]
    fn routing_timeout_falls_back_globally_within_the_override_budget() {
        let (endpoint, _, worker) = decision_server(None);
        let store = TelemetryStore::in_memory();
        store.app_setting_set(JEV_CONFIG_SETTING_KEY, &decision_config("jev", endpoint)).unwrap();
        let started = std::time::Instant::now();
        let route = tauri::async_runtime::block_on(route_query_async(
            &store, "test query", None, "auto", Some(100),
        )).unwrap();
        assert!(route.active_domains.is_empty());
        assert!(route.fallback.is_some());
        assert!(started.elapsed() < Duration::from_secs(1));
        assert!(worker.join().unwrap());
    }

    #[test]
    fn routing_ollama_preserves_the_json_contract_without_sending_a_jev_key() {
        let (endpoint, received, worker) = decision_server(Some((Duration::ZERO, json!({
            "message": { "content": "{\"domains\":[{\"id\":\"work\",\"probability\":0.9}],\"confidence\":0.9}" }
        }))));
        let client = DecisionClient::new(decision_config("ollama", endpoint)).unwrap();
        let response = tauri::async_runtime::block_on(client.ollama_chat_async(
            "query".into(), "Return JSON".into(), Some(Duration::from_secs(2)),
        )).unwrap();
        assert_eq!(response["domains"][0]["id"], "work");
        let request = received.recv_timeout(Duration::from_secs(1)).unwrap();
        assert!(!request.contains("test-jev-secret"));
        assert!(!request.to_ascii_lowercase().contains("\r\nauthorization:"));
        assert!(worker.join().unwrap());
    }

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

    fn classification_test_store() -> TelemetryStore {
        let store = TelemetryStore::in_memory();
        for id in ["first", "second", "third"] {
            store.conn.lock().unwrap().execute(
                "INSERT INTO local_memory_items
                 (id, source_event_key, ordinal, memory, memory_type, durability, source,
                  session_id, user_defined, created_at, updated_at)
                 VALUES (?1, ?1, 0, ?1, 'fact', 'short_term', 'test', 'test', 0,
                         '2020-01-01T00:00:00Z', '2020-01-01T00:00:00Z')",
                [id],
            ).unwrap();
        }
        store
    }

    fn classification_test_item(store: &TelemetryStore, id: &str) -> LocalMemorySnapshot {
        store.local_l1_memory_snapshot().unwrap().into_iter()
            .find(|item| item.id == id).unwrap()
    }

    #[test]
    fn classification_progress_reuses_legacy_results_but_not_extraction_labels() {
        let store = classification_test_store();
        store.replace_backfill_memory_domains("first", &[MemoryDomainAssignment {
            domain_id: "work".into(), weight: 0.9,
        }]).unwrap();
        // An empty decision result is completed work too (unclassified backfill).
        store.replace_backfill_memory_domains("second", &[]).unwrap();
        store.conn.lock().unwrap().execute(
            "INSERT INTO local_memory_domains
             (memory_id, domain_id, weight, source, created_at, updated_at)
             VALUES ('third', 'unclassified', 0, 'auto', '2020-01-01', '2020-01-01')",
            [],
        ).unwrap();
        let status = classification_status_with_store(&store).unwrap();
        assert_eq!((status.total, status.processed), (3, 2));
        assert_eq!(status.pending_ids, vec!["third"]);
        assert_eq!(store.count_l1_memories_for_domain_classification(false).unwrap(), 1);
        assert_eq!(store.count_l1_memories_for_domain_classification(true).unwrap(), 3);
    }

    #[test]
    fn interrupted_classification_resumes_without_repeating_saved_items() {
        let store = classification_test_store();
        let first = classification_test_item(&store, "first");
        let second = classification_test_item(&store, "second");
        store.save_memory_domain_classification(&first, &[]).unwrap();
        // Model configuration is disabled in this store. A saved item must skip
        // the model entirely; a following unfinished item fails without undoing it.
        assert_eq!(tauri::async_runtime::block_on(tag_one_batch_with_store(
            &store, std::slice::from_ref(&first),
        )).unwrap(), 0);
        assert!(tauri::async_runtime::block_on(tag_one_batch_with_store(
            &store, &[first, second],
        )).is_err());
        let status = classification_status_with_store(&store.clone()).unwrap();
        assert_eq!((status.total, status.processed), (3, 1));
        assert!(!status.pending_ids.contains(&"first".into()));
        assert!(status.pending_ids.contains(&"second".into()));
    }

    #[test]
    fn classification_checkpoint_survives_zero_labels_and_invalidates_on_edit() {
        let store = classification_test_store();
        let item = classification_test_item(&store, "first");
        store.delete_memory_domain("unclassified").unwrap();
        store.save_memory_domain_classification(&item, &[]).unwrap();
        assert!(store.completed_memory_domain_classification_ids().unwrap().contains("first"));
        store.update_local_l1_memory("first", "Edited while classification was in flight").unwrap();
        assert!(!store.completed_memory_domain_classification_ids().unwrap().contains("first"));
        // The old response is skipped without interrupting the batch or marking
        // the new text as complete.
        assert!(!store.save_memory_domain_classification(&item, &[]).unwrap());
        assert!(!store.completed_memory_domain_classification_ids().unwrap().contains("first"));
    }

    #[test]
    fn classification_batch_continues_past_deleted_and_manual_ids() {
        let store = classification_test_store();
        store.delete_local_l1_memory("second").unwrap();
        store.conn.lock().unwrap().execute(
            "UPDATE local_memory_items SET user_defined = 1 WHERE id = 'third'", [],
        ).unwrap();
        let (endpoint, received, worker) = decision_server(Some((Duration::ZERO, json!({
            "message": { "content": "{\"domains\":[]}" }
        }))));
        store.app_setting_set(JEV_CONFIG_SETTING_KEY, &decision_config("ollama", endpoint)).unwrap();
        let result = tauri::async_runtime::block_on(classify_batch_with_store(
            &store, &["missing".into(), "second".into(), "third".into(), "first".into()],
        )).unwrap();
        assert_eq!((result.total, result.processed), (1, 1));
        assert!(received.recv_timeout(Duration::from_secs(1)).unwrap().contains("first"));
        assert!(worker.join().unwrap());
        let status = classification_status_with_store(&store).unwrap();
        assert_eq!((status.total, status.processed), (1, 1));
        assert!(status.pending_ids.is_empty());
    }

    #[test]
    fn classification_batch_with_only_missing_ids_does_not_call_the_model() {
        let store = classification_test_store();
        store.delete_local_l1_memory("first").unwrap();
        // Model configuration is disabled. A stale-only queue must still succeed.
        let result = tauri::async_runtime::block_on(classify_batch_with_store(
            &store, &["first".into(), "missing".into()],
        )).unwrap();
        assert_eq!((result.total, result.processed), (0, 0));
        let status = classification_status_with_store(&store).unwrap();
        assert_eq!((status.total, status.processed), (2, 0));
    }

    #[test]
    fn classification_save_skips_memories_deleted_or_made_manual_in_flight() {
        let store = classification_test_store();
        let deleted = classification_test_item(&store, "first");
        let manual = classification_test_item(&store, "second");
        store.delete_local_l1_memory("first").unwrap();
        store.conn.lock().unwrap().execute(
            "UPDATE local_memory_items SET user_defined = 1 WHERE id = 'second'", [],
        ).unwrap();
        for item in [deleted, manual] {
            assert!(!store.save_memory_domain_classification(&item, &[MemoryDomainAssignment {
                domain_id: "study".into(), weight: 0.9,
            }]).unwrap());
        }
        assert!(store.memory_ids_for_domains(&["study".into()]).unwrap().is_empty());
        let checkpoint_count: i64 = store.conn.lock().unwrap().query_row(
            "SELECT COUNT(*) FROM memory_domain_classification_state", [], |row| row.get(0),
        ).unwrap();
        assert_eq!(checkpoint_count, 0);
        let third = classification_test_item(&store, "third");
        assert!(store.save_memory_domain_classification(&third, &[]).unwrap());
        let status = classification_status_with_store(&store).unwrap();
        assert_eq!((status.total, status.processed), (1, 1));
    }

    #[test]
    fn failed_classification_save_preserves_old_labels_and_checkpoint() {
        let store = classification_test_store();
        let item = classification_test_item(&store, "first");
        store.save_memory_domain_classification(&item, &[MemoryDomainAssignment {
            domain_id: "study".into(), weight: 0.9,
        }]).unwrap();
        // An invalid domain trips the FK after DELETE, exercising the rollback
        // boundary between removing old labels and inserting the new checkpoint.
        assert!(store.save_memory_domain_classification(&item, &[MemoryDomainAssignment {
            domain_id: "domain-that-does-not-exist".into(), weight: 0.9,
        }]).is_err());
        assert!(store.memory_ids_for_domains(&["study".into()]).unwrap().contains("first"));
        assert!(store.completed_memory_domain_classification_ids().unwrap().contains("first"));
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
