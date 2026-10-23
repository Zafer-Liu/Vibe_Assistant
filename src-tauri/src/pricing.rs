//! 模型归一化 + 定价引擎（PRD 一期）。
//!
//! 归一化三层（参考 magpie 的 `provider/model` 标识）：
//! 1. model_catalog alias 精确/大小写不敏感匹配
//! 2. 前缀启发式（claude-*→anthropic、gpt-*→openai…）
//! 3. 按 source（Agent 类型）兜底到 `source:<agent>` 归属
//!
//! 定价三级：user_override > provider.model_overrides > model_catalog(models_dev)，
//! 乘以 provider.cost_multiplier，四维分别计费。
//! 缓存语义（cc-switch 的坑）：OpenAI/Gemini 的 input 含缓存命中，
//! Anthropic 不含——按来源协议扣减避免双计。

use rusqlite::Connection;

use crate::provider_catalog::load_providers;
use crate::provider_catalog::catalog_lookup_by_alias;
use crate::provider_catalog::catalog_price;
use crate::provider_catalog::ModelPrice;
use crate::provider_catalog::ProviderConfig;

// ---------------------------------------------------------------------------
// 归一化
// ---------------------------------------------------------------------------

/// 前缀启发式表（顺序即优先级）。
const PREFIX_RULES: &[(&str, &str)] = &[
    ("claude", "anthropic"),
    ("gpt-", "openai"),
    ("o1", "openai"),
    ("o3", "openai"),
    ("o4", "openai"),
    ("chatgpt", "openai"),
    ("gemini", "google"),
    ("deepseek", "deepseek"),
    ("kimi", "moonshot"),
    ("k3", "moonshot"),
    ("glm", "zhipu"),
    ("minimax", "minimax"),
    ("qwen", "alibaba"),
];

/// Agent source → 兜底供应商 id（第三层）。
fn source_default(source: &str) -> &'static str {
    match source {
        "claude" | "zcode" | "workbuddy" | "qoder" => "source:claude",
        "codex" => "source:codex",
        "kimi" => "source:kimi",
        "minimax" => "source:minimax",
        // DeepSeek Harness 可接任意供应商（deepseek/magpie/pi-ai…），
        // 模型串多为 `provider/model` 形式，归一不到时落到自有兜底。
        "dsh" => "source:dsh",
        _ => "source:claude",
    }
}

#[allow(dead_code)]
pub struct NormalizedModel {
    pub provider_id: String,
    pub model_id: String,
    /// 归一化命中层：alias | prefix | source_default。
    pub method: &'static str,
}

/// 三层归一化。model 为空时直接落到 source 兜底。
pub fn normalize_model(conn: &Connection, model: Option<&str>, source: &str) -> NormalizedModel {
    let Some(model) = model.map(str::trim).filter(|value| !value.is_empty()) else {
        return NormalizedModel {
            provider_id: source_default(source).into(),
            model_id: "(unknown)".into(),
            method: "source_default",
        };
    };
    // 1. alias 精确匹配。
    if let Ok(Some((provider_id, model_id, _))) = catalog_lookup_by_alias(conn, model) {
        return NormalizedModel {
            provider_id,
            model_id,
            method: "alias",
        };
    }
    // 2. 前缀启发式。
    let lower = model.to_lowercase();
    for (prefix, provider) in PREFIX_RULES {
        if lower.starts_with(prefix) {
            return NormalizedModel {
                provider_id: (*provider).into(),
                model_id: model.to_string(),
                method: "prefix",
            };
        }
    }
    // 3. source 兜底。
    NormalizedModel {
        provider_id: source_default(source).into(),
        model_id: model.to_string(),
        method: "source_default",
    }
}

/// base URL → 供应商 id（域名反查 providers.json）。
#[allow(dead_code)]
pub fn provider_by_gateway(base_url: &str) -> Option<String> {
    let host = base_url
        .trim_start_matches("https://")
        .trim_start_matches("http://")
        .split('/')
        .next()?
    ;
    let host = host.to_lowercase();
    let providers = load_providers().ok()?;
    providers
        .iter()
        .find(|provider: &&ProviderConfig| {
            provider
                .base_url
                .trim_start_matches("https://")
                .trim_start_matches("http://")
                .split('/')
                .next()
                .map(|candidate| candidate.to_lowercase() == host)
                .unwrap_or(false)
        })
        .map(|provider| provider.id.clone())
}

// ---------------------------------------------------------------------------
// 定价引擎
// ---------------------------------------------------------------------------

pub struct PricedUsage {
    pub cost_usd: f64,
    /// user_override | provider_override | models_dev | none
    pub pricing_source: &'static str,
}

/// 缓存语义：input 是否已包含 cached（OpenAI/Gemini 是，Anthropic 否）。
/// 注意：providers 由调用方传入（每行计费会调用，绝不在此读盘）。
fn input_includes_cache(providers: &[ProviderConfig], source: &str, provider_id: &str) -> bool {
    // providers.json 的 api_format 是更权威的判据；Agent source 是代理。
    if let Some(provider) = providers.iter().find(|item| item.id == provider_id) {
        return matches!(
            provider.api_format,
            crate::provider_catalog::ApiFormat::OpenaiChat
                | crate::provider_catalog::ApiFormat::OpenaiResponses
                | crate::provider_catalog::ApiFormat::GeminiNative
        );
    }
    // 归属不到供应商时按 Agent 协议猜：codex(OpenAI) 含缓存，其余不含。
    source == "codex"
}

/// 三级价格查找。
pub fn lookup_price(
    conn: &Connection,
    providers: &[ProviderConfig],
    provider_id: &str,
    model_id: &str,
) -> (Option<ModelPrice>, &'static str) {
    if let Some(provider) = providers.iter().find(|item| item.id == provider_id) {
        if let Some(override_price) = provider.model_overrides.get(model_id) {
            return (Some(override_price.clone()), "provider_override");
        }
    }
    if let Ok(Some(catalog)) = catalog_price(conn, provider_id, model_id) {
        if catalog.source == "user_override" {
            return (
                Some(ModelPrice {
                    input_per_mtok: catalog.input_per_mtok,
                    output_per_mtok: catalog.output_per_mtok,
                    cache_read_per_mtok: catalog.cache_read_per_mtok,
                    cache_write_per_mtok: catalog.cache_write_per_mtok,
                }),
                "user_override",
            );
        }
        return (
            Some(ModelPrice {
                input_per_mtok: catalog.input_per_mtok,
                output_per_mtok: catalog.output_per_mtok,
                cache_read_per_mtok: catalog.cache_read_per_mtok,
                cache_write_per_mtok: catalog.cache_write_per_mtok,
            }),
            "models_dev",
        );
    }
    (None, "none")
}

/// 单条用量计费。找不到价目返回 pricing_source="none"、cost=0。
pub fn price_usage(
    conn: &Connection,
    providers: &[ProviderConfig],
    source: &str,
    provider_id: &str,
    model_id: &str,
    input_tokens: i64,
    output_tokens: i64,
    cached_tokens: i64,
) -> PricedUsage {
    let (price, pricing_source) = lookup_price(conn, providers, provider_id, model_id);
    let Some(price) = price else {
        return PricedUsage {
            cost_usd: 0.0,
            pricing_source: "none",
        };
    };
    // 缓存语义归一：input 含缓存的平台先扣掉缓存部分再计 input 价。
    let (billable_input, cache_read) = if input_includes_cache(providers, source, provider_id) {
        (
            (input_tokens - cached_tokens).max(0),
            cached_tokens,
        )
    } else {
        (input_tokens, cached_tokens)
    };
    let multiplier = providers
        .iter()
        .find(|item| item.id == provider_id)
        .map(|item| item.cost_multiplier)
        .unwrap_or(1.0);
    let cost = (billable_input as f64 / 1e6) * price.input_per_mtok
        + (output_tokens as f64 / 1e6) * price.output_per_mtok
        + (cache_read as f64 / 1e6) * price.cache_read_per_mtok.unwrap_or(0.0);
    PricedUsage {
        cost_usd: cost * multiplier,
        pricing_source,
    }
}

// ---------------------------------------------------------------------------
// 单元测试
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn memory_conn() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        crate::provider_catalog::ensure_catalog_table(&conn).unwrap();
        conn
    }

    #[test]
    fn normalize_prefix_rules() {
        let conn = memory_conn();
        let normalized = normalize_model(&conn, Some("claude-sonnet-5"), "codex");
        assert_eq!(normalized.provider_id, "anthropic");
        assert_eq!(normalized.method, "prefix");
        let normalized = normalize_model(&conn, Some("MiniMax-M3"), "claude");
        assert_eq!(normalized.provider_id, "minimax");
        let normalized = normalize_model(&conn, Some("weird-model"), "kimi");
        assert_eq!(normalized.provider_id, "source:kimi");
        assert_eq!(normalized.method, "source_default");
        let normalized = normalize_model(&conn, None, "codex");
        assert_eq!(normalized.model_id, "(unknown)");
    }

    #[test]
    fn normalize_alias_layer_wins() {
        let conn = memory_conn();
        conn.execute(
            "INSERT INTO model_catalog (provider_id, model_id, aliases, source, updated_at)
             VALUES ('moonshot', 'kimi-k3', '[\"k3-agent\"]', 'models_dev', '2026-01-01')",
            [],
        )
        .unwrap();
        let normalized = normalize_model(&conn, Some("K3-Agent"), "claude");
        assert_eq!(normalized.provider_id, "moonshot");
        assert_eq!(normalized.model_id, "kimi-k3");
        assert_eq!(normalized.method, "alias");
    }

    #[test]
    fn cache_semantics_deduplicates_openai_style() {
        let conn = memory_conn();
        conn.execute(
            "INSERT INTO model_catalog (provider_id, model_id, aliases, input_per_mtok,
             output_per_mtok, cache_read_per_mtok, source, updated_at)
             VALUES ('source:codex', 'gpt-5', '[]', 1.25, 10.0, 0.125, 'models_dev', '2026-01-01')",
            [],
        )
        .unwrap();
        let providers: Vec<ProviderConfig> = Vec::new();
        // OpenAI 风格：input 1000 含 600 缓存 → billable input 400 + cache 600。
        let priced = price_usage(
            &conn, &providers, "codex", "source:codex", "gpt-5",
            1000, 0, 600,
        );
        let expected = (400.0 / 1e6) * 1.25 + (600.0 / 1e6) * 0.125;
        assert!((priced.cost_usd - expected).abs() < 1e-12);
        assert_eq!(priced.pricing_source, "models_dev");
    }

    #[test]
    fn anthropic_style_keeps_full_input() {
        let conn = memory_conn();
        conn.execute(
            "INSERT INTO model_catalog (provider_id, model_id, aliases, input_per_mtok,
             output_per_mtok, cache_read_per_mtok, source, updated_at)
             VALUES ('source:claude', 'claude-sonnet-5', '[]', 3.0, 15.0, 0.3, 'models_dev', '2026-01-01')",
            [],
        )
        .unwrap();
        let providers: Vec<ProviderConfig> = Vec::new();
        // Anthropic 风格：input 1000（不含缓存）全价 + 600 缓存读。
        let priced = price_usage(
            &conn, &providers, "claude", "source:claude", "claude-sonnet-5",
            1000, 0, 600,
        );
        let expected = (1000.0 / 1e6) * 3.0 + (600.0 / 1e6) * 0.3;
        assert!((priced.cost_usd - expected).abs() < 1e-12);
    }

    #[test]
    fn unknown_price_is_free_with_none_source() {
        let conn = memory_conn();
        let providers: Vec<ProviderConfig> = Vec::new();
        let priced = price_usage(
            &conn, &providers, "claude", "source:claude", "no-such-model",
            1000, 1000, 0,
        );
        assert_eq!(priced.cost_usd, 0.0);
        assert_eq!(priced.pricing_source, "none");
    }

    #[test]
    fn gateway_lookup_matches_by_host() {
        // providers.json 是真实文件；内置预设必含 anthropic 官方域名。
        if let Some(provider) = provider_by_gateway("https://api.anthropic.com/v1") {
            assert_eq!(provider, "anthropic");
        }
    }
}
