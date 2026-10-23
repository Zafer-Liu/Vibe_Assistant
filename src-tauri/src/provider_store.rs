//! Provider IPC commands and catalog refresh orchestration.
use crate::telemetry_store::TelemetryStore;
use tauri::Emitter;

use crate::provider_catalog::{
    apply_model_catalog_impl, fetch_models_dev, load_providers,
    set_user_price_override, ModelPrice, ProviderConfig, ProviderUsageSummary,
};

const CATALOG_SYNC_KEY: &str = "provider.catalog.last_sync_ms";

pub fn catalog_sync_needed(store: &TelemetryStore) -> bool {
    let last: i64 = store
        .app_setting_get::<i64>(CATALOG_SYNC_KEY)
        .unwrap_or(0);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    now - last > 24 * 3600 * 1000
}

pub fn mark_catalog_synced(store: &TelemetryStore) {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    let _ = store.app_setting_set(CATALOG_SYNC_KEY, &now);
}

// ---------------------------------------------------------------------------
// Tauri 命令
// ---------------------------------------------------------------------------

#[derive(serde::Serialize)]
pub struct ProviderView {
    #[serde(flatten)]
    provider: ProviderConfig,
    has_api_key: bool,
    key_mask: Option<String>,
    identity: crate::model_registry::ModelIdentity,
}
#[derive(serde::Deserialize)]
pub struct ProviderInput {
    #[serde(flatten)]
    provider: ProviderConfig,
    #[serde(default)]
    clear_key: bool,
}
#[tauri::command]
pub async fn list_providers() -> Result<Vec<ProviderView>, String> {
    let registry = crate::model_registry::Registry::shared()?;
    load_providers()?
        .into_iter()
        .map(|provider| {
            let identity = registry.identity(crate::model_registry::CATALOG, &provider.id)?;
            let has_api_key = identity.credential_id.is_some();
            Ok(ProviderView {
                provider,
                has_api_key,
                key_mask: has_api_key.then(|| "••••••••".into()),
                identity,
            })
        })
        .collect()
}
#[tauri::command]
pub async fn save_provider(provider: ProviderInput) -> Result<(), String> {
    let input = provider;
    let provider = input.provider;
    let previous = load_providers()?;
    let registry = crate::model_registry::Registry::shared()?;
    let secret = if input.clear_key {
        Some("")
    } else {
        provider.api_key.as_deref().filter(|key| !key.is_empty())
    };
    if secret.is_none()
        && previous
            .iter()
            .find(|p| p.id == provider.id)
            .is_some_and(|p| p.base_url != provider.base_url)
        && registry
            .identity(crate::model_registry::CATALOG, &provider.id)?
            .credential_id
            .is_some()
    {
        return Err("更改供应商地址时请重新填写 Key 或明确清除旧 Key".into());
    }
    registry.save(
        crate::model_registry::CATALOG,
        &serde_json::to_value(&provider).map_err(|e| e.to_string())?,
        secret,
    )?;
    if let Some(store) = crate::telemetry_store::shared_store() {
        store.invalidate_provider_prices(&provider.id)?;
    }
    Ok(())
}
#[tauri::command]
pub async fn delete_provider(provider_id: String) -> Result<(), String> {
    let providers = load_providers()?;
    if providers.iter().any(|p| p.id == provider_id && p.built_in) {
        return Err("内置供应商只能禁用，不能删除".into());
    }
    crate::model_registry::Registry::shared()?
        .delete(crate::model_registry::CATALOG, &provider_id)?;
    if let Some(store) = crate::telemetry_store::shared_store() {
        store.invalidate_provider_prices(&provider_id)?;
    }
    Ok(())
}
#[tauri::command]
pub async fn sync_model_catalog(app: tauri::AppHandle) -> Result<(usize, usize), String> {
    let Some(store) = crate::telemetry_store::shared_store() else {
        return Err("telemetry store 未初始化".into());
    };
    let store_for_task = store.clone();
    let result = tauri::async_runtime::spawn_blocking(move || {
        // Network IO must finish before taking the connection lock.
        let rows = fetch_models_dev()?;
        let conn = store_for_task
            .conn
            .lock()
            .map_err(|_| "telemetry store lock poisoned".to_string())?;
        apply_model_catalog_impl(&conn, &rows)
    })
    .await
    .map_err(|e| e.to_string())??;
    store.invalidate_catalog_prices()?;
    mark_catalog_synced(&store);
    let _ = app.emit("telemetry-updated", ());
    Ok(result)
}

#[tauri::command]
pub async fn set_model_price_override(
    state: tauri::State<'_, TelemetryStore>,
    provider_id: String,
    model_id: String,
    price: ModelPrice,
) -> Result<(), String> {
    let conn = state
        .conn
        .lock()
        .map_err(|_| "telemetry store lock poisoned".to_string())?;
    set_user_price_override(&conn, &provider_id, &model_id, &price)?;
    drop(conn);
    state.invalidate_provider_prices(&provider_id)?;
    Ok(())
}

/// 按供应商聚合用量与成本（供二期 UI；一期先暴露数据）。
#[tauri::command]
pub async fn get_provider_usage_summary(
    state: tauri::State<'_, TelemetryStore>,
    start_at: Option<String>,
    end_at: Option<String>,
) -> Result<Vec<ProviderUsageSummary>, String> {
    state.provider_usage_summary(start_at.as_deref(), end_at.as_deref())
}

#[cfg(test)]
mod real_data_tests {
    use super::*;

    /// 真实库上验证：目录同步 → 成本回填 → 供应商聚合。
    /// cargo test --lib real_pricing_pipeline -- --ignored --nocapture
    #[test]
    #[ignore]
    fn real_pricing_pipeline() {
        use crate::telemetry_store::TelemetryStore;
        let store = TelemetryStore::new().expect("store");
        {
            let conn = store.conn.lock().unwrap();
            let _ = conn.execute_batch("PRAGMA busy_timeout=30000;");
            let started = std::time::Instant::now();
            match fetch_models_dev().and_then(|rows| apply_model_catalog_impl(&conn, &rows)) {
                Ok((added, _)) => println!("catalog sync added/updated={added} elapsed={:?}", started.elapsed()),
                Err(error) => println!("catalog sync failed (offline ok): {error}"),
            }
        }
        let started = std::time::Instant::now();
        let processed = store.recalculate_costs(false).expect("recalc");
        println!("recalc processed={processed} elapsed={:?}", started.elapsed());
        let summary = store.provider_usage_summary(None, None).expect("summary");
        println!("summary groups={}", summary.len());
        for row in summary.iter().take(12) {
            println!(
                "  {:?} {:?} in={} out={} cache={} cost={:.4} src={:?} n={}",
                row.provider_id, row.model, row.input_tokens, row.output_tokens,
                row.cached_tokens, row.cost_usd, row.pricing_source, row.record_count
            );
        }
        assert!(processed > 0, "回填应处理至少一行");
    }
}
