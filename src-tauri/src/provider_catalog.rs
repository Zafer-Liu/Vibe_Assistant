//! Provider configuration persistence, model catalog, and price data.
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::path::PathBuf;

const PROVIDERS_FILE: &str = "providers.json";

// ---------------------------------------------------------------------------
// 数据结构
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Debug)]
#[serde(rename_all = "snake_case")]
pub enum ApiFormat {
    Anthropic,
    OpenaiChat,
    OpenaiResponses,
    GeminiNative,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct ModelPrice {
    pub input_per_mtok: f64,
    pub output_per_mtok: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_read_per_mtok: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_write_per_mtok: Option<f64>,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct ProviderConfig {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub base_url: String,
    #[serde(default = "default_api_format")]
    pub api_format: ApiFormat,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "default_cost_multiplier")]
    pub cost_multiplier: f64,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub model_overrides: HashMap<String, ModelPrice>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub daily_limit_usd: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub monthly_limit_usd: Option<f64>,
    #[serde(default)]
    pub built_in: bool,
}

fn default_api_format() -> ApiFormat {
    ApiFormat::OpenaiChat
}
fn default_true() -> bool {
    true
}
fn default_cost_multiplier() -> f64 {
    1.0
}

/// 内置预设（幂等种子：仅当 id 不存在时插入，绝不覆盖用户修改）。
fn seed_providers() -> Vec<ProviderConfig> {
    let seed = |id: &str, name: &str, base_url: &str, api_format: ApiFormat| ProviderConfig {
        id: id.into(),
        name: name.into(),
        base_url: base_url.into(),
        api_format,
        api_key: None,
        enabled: true,
        cost_multiplier: 1.0,
        model_overrides: HashMap::new(),
        daily_limit_usd: None,
        monthly_limit_usd: None,
        built_in: true,
    };
    vec![
        seed(
            "anthropic",
            "Anthropic 官方",
            "https://api.anthropic.com",
            ApiFormat::Anthropic,
        ),
        seed(
            "openai",
            "OpenAI 官方",
            "https://api.openai.com",
            ApiFormat::OpenaiResponses,
        ),
        seed(
            "google",
            "Google Gemini",
            "https://generativelanguage.googleapis.com",
            ApiFormat::GeminiNative,
        ),
        seed(
            "deepseek",
            "DeepSeek 官方",
            "https://api.deepseek.com",
            ApiFormat::OpenaiChat,
        ),
        seed(
            "moonshot",
            "Kimi / Moonshot",
            "https://api.moonshot.cn",
            ApiFormat::OpenaiChat,
        ),
        seed(
            "zhipu",
            "智谱 GLM",
            "https://open.bigmodel.cn",
            ApiFormat::OpenaiChat,
        ),
        seed(
            "minimax",
            "MiniMax",
            "https://api.minimaxi.com",
            ApiFormat::OpenaiChat,
        ),
        seed(
            "openrouter",
            "OpenRouter",
            "https://openrouter.ai",
            ApiFormat::OpenaiChat,
        ),
        seed(
            "88api",
            "88API 中转",
            "https://api.88api.ai",
            ApiFormat::Anthropic,
        ),
        seed(
            "ollama",
            "Ollama 本地",
            "http://127.0.0.1:11434",
            ApiFormat::OpenaiChat,
        ),
        // 各 Agent 客户端的兜底归属（PRD 归一化第三层）。
        seed(
            "source:claude",
            "Claude Code（未匹配网关）",
            "",
            ApiFormat::Anthropic,
        ),
        seed(
            "source:codex",
            "Codex（未匹配网关）",
            "",
            ApiFormat::OpenaiResponses,
        ),
        seed(
            "source:kimi",
            "Kimi Code（未匹配网关）",
            "",
            ApiFormat::OpenaiChat,
        ),
        seed(
            "source:minimax",
            "MiniMax Code（未匹配网关）",
            "",
            ApiFormat::Anthropic,
        ),
        seed(
            "source:qoder",
            "Qoder（未匹配网关）",
            "",
            ApiFormat::Anthropic,
        ),
        seed(
            "source:workbuddy",
            "WorkBuddy（未匹配网关）",
            "",
            ApiFormat::Anthropic,
        ),
        seed(
            "source:zcode",
            "ZCode（未匹配网关）",
            "",
            ApiFormat::Anthropic,
        ),
    ]
}

fn providers_path() -> Result<PathBuf, String> {
    let dir = dirs_next::config_dir()
        .ok_or("无法定位配置目录")?
        .join("agent-manager");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    Ok(dir.join(PROVIDERS_FILE))
}

pub fn load_providers() -> Result<Vec<ProviderConfig>, String> {
    use crate::model_registry::{read_optional, sanitized_bytes, Cleanup, Registry, CATALOG};
    let registry = Registry::shared()?;
    if !registry.migrated(CATALOG)? {
        let path = providers_path()?;
        let source = read_optional(&path)?;
        let raw = source
            .as_ref()
            .map(|(v, _)| v.clone())
            .unwrap_or_else(|| serde_json::json!([]));
        let providers: Vec<ProviderConfig> =
            serde_json::from_value(raw.clone()).map_err(|_| "旧供应商配置字段损坏，已停止迁移")?;
        let cleanups = source
            .map(|(v, bytes)| {
                Ok::<Cleanup, String>(Cleanup {
                    path,
                    original: bytes,
                    sanitized: sanitized_bytes(&v)?,
                })
            })
            .transpose()?
            .into_iter()
            .collect();
        registry.import(
            CATALOG,
            providers
                .iter()
                .map(serde_json::to_value)
                .collect::<Result<_, _>>()
                .map_err(|e| e.to_string())?,
            raw,
            None,
            cleanups,
        )?;
    }
    let mut providers: Vec<ProviderConfig> = registry
        .documents(CATALOG)?
        .into_iter()
        .map(serde_json::from_value)
        .collect::<Result<_, _>>()
        .map_err(|e| e.to_string())?;
    for seed in seed_providers() {
        if !providers.iter().any(|p| p.id == seed.id) {
            registry.save(
                CATALOG,
                &serde_json::to_value(&seed).map_err(|e| e.to_string())?,
                None,
            )?;
            providers.push(seed);
        }
    }
    for provider in &mut providers {
        provider.api_key = None;
    }
    Ok(providers)
}
/// Pricing reads no credentials and never opens/unlocks the native vault.
/// Until first credential migration, legacy JSON is a read-only compatibility source.
pub(crate) fn pricing_snapshot() -> Result<Vec<ProviderConfig>,String> {
    use crate::model_registry::{Registry,CATALOG,read_optional};
    if crate::telemetry_store::shared_store().is_none(){return Ok(Vec::new())}
    let registry=Registry::shared()?;
    let mut providers:Vec<ProviderConfig>=if registry.migrated(CATALOG)? {
        registry.metadata_documents(CATALOG)?.into_iter().map(serde_json::from_value).collect::<Result<_,_>>().map_err(|e|e.to_string())?
    } else {
        let value=read_optional(&providers_path()?)?.map(|(v,_)|v).unwrap_or_else(||serde_json::json!([]));
        serde_json::from_value(value).map_err(|_|"供应商定价配置损坏")?
    };
    for seed in seed_providers(){if !providers.iter().any(|p|p.id==seed.id){providers.push(seed)}}
    for provider in &mut providers{provider.api_key=None;}
    Ok(providers)
}

// ---------------------------------------------------------------------------
// model_catalog 表
// ---------------------------------------------------------------------------

pub fn ensure_catalog_table(conn: &Connection) -> Result<(), String> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS model_catalog (
           provider_id TEXT NOT NULL,
           model_id TEXT NOT NULL,
           aliases TEXT NOT NULL DEFAULT '[]',
           input_per_mtok REAL,
           output_per_mtok REAL,
           cache_read_per_mtok REAL,
           cache_write_per_mtok REAL,
           source TEXT NOT NULL,
           updated_at TEXT NOT NULL,
           PRIMARY KEY(provider_id, model_id)
         );",
    )
    .map_err(|e| e.to_string())
}

pub struct CatalogPrice {
    pub input_per_mtok: f64,
    pub output_per_mtok: f64,
    pub cache_read_per_mtok: Option<f64>,
    pub cache_write_per_mtok: Option<f64>,
    pub source: String,
}

/// 读目录单价（含 user_override）。PRD 三级查找的目录层。
pub fn catalog_price(
    conn: &Connection,
    provider_id: &str,
    model_id: &str,
) -> Result<Option<CatalogPrice>, String> {
    let mut statement = conn
        .prepare(
            "SELECT input_per_mtok, output_per_mtok, cache_read_per_mtok, cache_write_per_mtok, source
               FROM model_catalog WHERE provider_id = ?1 AND model_id = ?2
              ORDER BY CASE source WHEN 'user_override' THEN 0 ELSE 1 END LIMIT 1",
        )
        .map_err(|e| e.to_string())?;
    let row = statement
        .query_row(params![provider_id, model_id], |row| {
            Ok(CatalogPrice {
                input_per_mtok: row.get::<_, Option<f64>>(0)?.unwrap_or(0.0),
                output_per_mtok: row.get::<_, Option<f64>>(1)?.unwrap_or(0.0),
                cache_read_per_mtok: row.get(2)?,
                cache_write_per_mtok: row.get(3)?,
                source: row.get(4)?,
            })
        })
        .ok();
    Ok(row)
}

/// 按 alias 精确匹配查目录（归一化第一层用）。
pub fn catalog_lookup_by_alias(
    conn: &Connection,
    raw_model: &str,
) -> Result<Option<(String, String, Option<CatalogPrice>)>, String> {
    let mut statement = conn
        .prepare(
            "SELECT provider_id, model_id, aliases FROM model_catalog
              WHERE EXISTS (
                SELECT 1 FROM json_each(CASE WHEN json_valid(model_catalog.aliases)
                     THEN model_catalog.aliases ELSE '[]' END) alias
                 WHERE lower(alias.value) = lower(?1)
              )",
        )
        .map_err(|e| e.to_string())?;
    let hit = statement
        .query_row(params![raw_model], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .ok();
    let Some((provider_id, model_id)) = hit else {
        return Ok(None);
    };
    let price = catalog_price(conn, &provider_id, &model_id)?;
    Ok(Some((provider_id, model_id, price)))
}

pub fn set_user_price_override(
    conn: &Connection,
    provider_id: &str,
    model_id: &str,
    price: &ModelPrice,
) -> Result<(), String> {
    ensure_catalog_table(conn)?;
    let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    conn.execute(
        "INSERT INTO model_catalog
           (provider_id, model_id, aliases, input_per_mtok, output_per_mtok,
            cache_read_per_mtok, cache_write_per_mtok, source, updated_at)
         VALUES (?1, ?2, '[]', ?3, ?4, ?5, ?6, 'user_override', ?7)
         ON CONFLICT(provider_id, model_id) DO UPDATE SET
           input_per_mtok=excluded.input_per_mtok, output_per_mtok=excluded.output_per_mtok,
           cache_read_per_mtok=excluded.cache_read_per_mtok,
           cache_write_per_mtok=excluded.cache_write_per_mtok,
           source='user_override', updated_at=excluded.updated_at",
        params![
            provider_id,
            model_id,
            price.input_per_mtok,
            price.output_per_mtok,
            price.cache_read_per_mtok,
            price.cache_write_per_mtok,
            now
        ],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

// ---------------------------------------------------------------------------
// models.dev 同步
// ---------------------------------------------------------------------------

/// 拉取 https://models.dev/api.json 并展开成目录行。
/// 失败静默返回 Err（离线可用旧缓存）。
pub(crate) type ModelCatalogRows = Vec<(String, String, Option<ModelPrice>)>;

pub(crate) fn fetch_models_dev() -> Result<ModelCatalogRows, String> {
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .map_err(|e| e.to_string())?;
    let body: Value = client
        .get("https://models.dev/api.json")
        .send()
        .map_err(|e| e.to_string())?
        .error_for_status()
        .map_err(|e| e.to_string())?
        .json()
        .map_err(|e| e.to_string())?;
    let mut rows = Vec::new();
    let Some(providers) = body.as_object() else {
        return Err("models.dev 响应不是对象".into());
    };
    for (provider_id, provider) in providers {
        let Some(models) = provider.get("models").and_then(Value::as_object) else {
            continue;
        };
        for (model_id, model) in models {
            let cost = model.get("cost");
            let price = cost.and_then(|cost| {
                let input = cost.get("input").and_then(Value::as_f64)?;
                let output = cost.get("output").and_then(Value::as_f64)?;
                Some(ModelPrice {
                    input_per_mtok: input,
                    output_per_mtok: output,
                    cache_read_per_mtok: cost.get("cache_read").and_then(Value::as_f64),
                    cache_write_per_mtok: cost.get("cache_write").and_then(Value::as_f64),
                })
            });
            rows.push((provider_id.clone(), model_id.clone(), price));
        }
    }
    Ok(rows)
}

pub(crate) fn apply_model_catalog_impl(
    conn: &Connection,
    rows: &ModelCatalogRows,
) -> Result<(usize, usize), String> {
    ensure_catalog_table(conn)?;
    let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    let mut added = 0usize;
    // BEGIN IMMEDIATE：WAL 下 DEFERRED 先读后写会撞 BUSY_SNAPSHOT。
    conn.execute_batch("BEGIN IMMEDIATE")
        .map_err(|e| e.to_string())?;
    for (provider_id, model_id, price) in rows {
        // user_override 行绝不覆盖。
        let changed = conn
            .execute(
                "INSERT INTO model_catalog
                   (provider_id, model_id, aliases, input_per_mtok, output_per_mtok,
                    cache_read_per_mtok, cache_write_per_mtok, source, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 'models_dev', ?8)
                 ON CONFLICT(provider_id, model_id) DO UPDATE SET
                   aliases=excluded.aliases,
                   input_per_mtok=COALESCE(excluded.input_per_mtok, model_catalog.input_per_mtok),
                   output_per_mtok=COALESCE(excluded.output_per_mtok, model_catalog.output_per_mtok),
                   cache_read_per_mtok=COALESCE(excluded.cache_read_per_mtok, model_catalog.cache_read_per_mtok),
                   cache_write_per_mtok=COALESCE(excluded.cache_write_per_mtok, model_catalog.cache_write_per_mtok),
                   source='models_dev', updated_at=excluded.updated_at
                 WHERE model_catalog.source != 'user_override'",
                params![
                    provider_id,
                    model_id,
                    serde_json::to_string(&[model_id]).unwrap_or_else(|_| "[]".into()),
                    price.as_ref().map(|price| price.input_per_mtok),
                    price.as_ref().map(|price| price.output_per_mtok),
                    price.as_ref().and_then(|price| price.cache_read_per_mtok),
                    price.as_ref().and_then(|price| price.cache_write_per_mtok),
                    now
                ],
            )
            .map_err(|e| e.to_string())?;
        if changed > 0 {
            added += 1;
        }
    }
    conn.execute_batch("COMMIT").map_err(|e| e.to_string())?;
    Ok((added, added))
}

/// models.dev 目录里的原生模型名（含别名候选）：归一化第一层的种子。
/// 首次同步后 alias 表自带 model_id 精确匹配，这里不需要额外工作。
#[derive(Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub struct ProviderUsageSummary {
    pub provider_id: Option<String>,
    pub gateway: Option<String>,
    pub model: Option<String>,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cached_tokens: i64,
    pub cost_usd: f64,
    pub pricing_source: Option<String>,
    pub record_count: i64,
}
