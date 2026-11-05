//! 模型配置的跨设备同步：连接文档、Key、决策模型与语义嵌入配置。
//!
//! Key 在本机由系统主密钥封存（`model_registry`），跨设备传输时用云端同步
//! 密码二次加密；接收端解密后立即用本机主密钥重新封存，明文不落盘。
//! 合并规则：provider 按 `config_revision` 较新者胜；删除以墓碑传播；
//! 决策/语义配置按「本地最后改动时间戳」较新者胜。

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::Arc;

use crate::model_registry::{MasterKeys, Registry, CATALOG, LLM};
use crate::telemetry_store::TelemetryStore;

const STAMP_DECISION: &str = "cloud_vault.model_stamp_decision";
const STAMP_SEMANTIC: &str = "cloud_vault.model_stamp_semantic";
const EXPORTED_IDS: &str = "cloud_vault.model_exported_ids";
const TOMBSTONES: &str = "cloud_vault.model_tombstones";
const DECISION_KEY: &str = "memory_domain_jev";
const SEMANTIC_KEY: &str = "memory_semantic_embedding";

#[derive(Clone, Serialize, Deserialize)]
pub struct CloudModelProvider {
    pub namespace: String,
    pub legacy_id: String,
    pub document: Value,
    pub revision: i64,
    /// Key 由云端同步密码加密；None 表示该 provider 在本机没有 Key。
    pub secret_cipher: Option<String>,
    /// 本机显式清除了 Key：接收端应删除既有凭证。
    #[serde(default)]
    pub clear_secret: bool,
    pub updated_at: String,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct CloudModelTombstone {
    pub namespace: String,
    pub legacy_id: String,
    pub revision: i64,
    pub deleted_at: String,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct StampedConfig {
    pub config: Value,
    /// api_key 由云端同步密码加密；None 表示未配置 Key。
    pub api_key_cipher: Option<String>,
    pub hash: String,
    pub at: String,
}

#[derive(Clone, Serialize, Deserialize, Default)]
pub struct CloudModelConfig {
    pub format: u32,
    #[serde(default)]
    pub providers: Vec<CloudModelProvider>,
    #[serde(default)]
    pub tombstones: Vec<CloudModelTombstone>,
    #[serde(default)]
    pub decision: Option<StampedConfig>,
    #[serde(default)]
    pub semantic: Option<StampedConfig>,
}

pub const MODEL_CONFIG_FORMAT: u32 = 1;

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::Mutex;

    struct MemoryKeys(Mutex<HashMap<String, Vec<u8>>>);
    impl MasterKeys for MemoryKeys {
        fn get(&self, id: &str) -> Result<Option<Vec<u8>>, String> {
            Ok(self.0.lock().unwrap().get(id).cloned())
        }
        fn put(&self, id: &str, key: &[u8]) -> Result<(), String> {
            self.0.lock().unwrap().insert(id.to_string(), key.to_vec());
            Ok(())
        }
    }
    fn keys() -> Arc<dyn MasterKeys> {
        Arc::new(MemoryKeys(Mutex::new(HashMap::new())))
    }
    fn provider_doc(id: &str, base_url: &str) -> Value {
        serde_json::json!({
            "id": id, "name": "Test", "base_url": base_url, "model": "model-a",
            "api_key": "", "enabled": true, "is_custom": true,
        })
    }

    #[test]
    fn model_config_roundtrip_carries_keys_and_deletions() {
        let a = TelemetryStore::in_memory();
        let b = TelemetryStore::in_memory();
        // 每台设备一个主密钥后端：密钥存在后端里，跨调用必须复用同一实例。
        let (keys_a, keys_b) = (keys(), keys());
        let sync = [7u8; 32];
        Registry::with_store(a.clone(), keys_a.clone())
            .save(LLM, &provider_doc("prov-1", "https://example.invalid/v1"), Some("sk-secret"))
            .unwrap();
        let exported = export_model_config(&a, keys_a.clone(), &sync).unwrap();
        assert_eq!(exported.providers.len(), 1);
        assert!(exported.providers[0].secret_cipher.is_some());

        let (applied, _) = apply_model_config(&b, keys_b.clone(), &sync, &exported).unwrap();
        assert_eq!(applied, 1);
        let (_, _, secret) = Registry::with_store(b.clone(), keys_b.clone())
            .resolve(LLM, "prov-1")
            .unwrap();
        assert_eq!(secret.to_string(), "sk-secret", "Key 应在接收端用本机主密钥解封后可用");

        Registry::with_store(a.clone(), keys_a.clone()).delete(LLM, "prov-1").unwrap();
        let exported = export_model_config(&a, keys_a.clone(), &sync).unwrap();
        assert!(exported.providers.is_empty());
        assert_eq!(exported.tombstones.len(), 1);
        let (applied, _) = apply_model_config(&b, keys_b.clone(), &sync, &exported).unwrap();
        assert_eq!(applied, 1);
        assert!(Registry::with_store(b.clone(), keys_b.clone()).resolve(LLM, "prov-1").is_err());
    }

    #[test]
    fn decision_config_merges_by_newer_stamp() {
        let a = TelemetryStore::in_memory();
        let b = TelemetryStore::in_memory();
        let (keys_a, keys_b) = (keys(), keys());
        let sync = [9u8; 32];
        a.app_setting_set(
            DECISION_KEY,
            &serde_json::json!({"enabled": true, "provider": "jev", "endpoint": "https://a.invalid", "model": "m-a", "api_key": "sk-a"}),
        )
        .unwrap();
        let exported = export_model_config(&a, keys_a.clone(), &sync).unwrap();
        assert!(exported.decision.as_ref().unwrap().api_key_cipher.is_some());
        apply_model_config(&b, keys_b.clone(), &sync, &exported).unwrap();
        let config: Value = b.app_setting_get(DECISION_KEY).unwrap();
        assert_eq!(config["api_key"], serde_json::json!("sk-a"));

        b.app_setting_set(
            DECISION_KEY,
            &serde_json::json!({"enabled": true, "provider": "jev", "endpoint": "https://b.invalid", "model": "m-b", "api_key": ""}),
        )
        .unwrap();
        let exported_b = export_model_config(&b, keys_b.clone(), &sync).unwrap();
        apply_model_config(&a, keys_a.clone(), &sync, &exported_b).unwrap();
        let config_a: Value = a.app_setting_get(DECISION_KEY).unwrap();
        assert_eq!(config_a["endpoint"], serde_json::json!("https://b.invalid"), "较新的本地改动应胜出");
    }

    #[test]
    fn unconfigured_settings_are_not_published() {
        let a = TelemetryStore::in_memory();
        let sync = [5u8; 32];
        let exported = export_model_config(&a, keys(), &sync).unwrap();
        assert!(exported.decision.is_none());
        assert!(exported.semantic.is_none());
        assert!(exported.providers.is_empty());
    }
}

fn sha256_hex(data: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(data);
    hasher.finalize().iter().map(|b| format!("{:02x}", b)).collect()
}

fn encrypt_secret(sync_key: &[u8; 32], plain: &str) -> Result<String, String> {
    crate::cloud_sync::vault_encrypt(sync_key, plain.as_bytes())
        .map(|bytes| crate::cloud_sync::b64_encode(&bytes))
}

fn decrypt_secret(sync_key: &[u8; 32], cipher: &str) -> Result<String, String> {
    let blob = crate::cloud_sync::b64_decode(cipher).ok_or("模型 Key 密文编码损坏")?;
    let plain = crate::cloud_sync::vault_decrypt(sync_key, &blob)?;
    String::from_utf8(plain).map_err(|_| "模型 Key 明文不是 UTF-8".to_string())
}

/// 记录某份配置的「最后本地改动时间」：内容哈希不变则沿用旧时间戳。
fn stamp(store: &TelemetryStore, setting: &str, config: &Value) -> Result<StampedConfig, String> {
    let json = serde_json::to_string(config).map_err(|e| e.to_string())?;
    let hash = sha256_hex(json.as_bytes());
    let at = store
        .app_setting_get::<StampedConfig>(setting)
        .filter(|old| old.hash == hash)
        .map(|old| old.at)
        .unwrap_or_else(|| chrono::Utc::now().to_rfc3339());
    let stamped = StampedConfig {
        config: config.clone(),
        api_key_cipher: None,
        hash,
        at,
    };
    store.app_setting_set(setting, &stamped)?;
    Ok(stamped)
}

fn split_api_key(config: &Value) -> (Value, Option<String>) {
    let mut document = config.clone();
    let key = document
        .get("api_key")
        .and_then(Value::as_str)
        .map(str::to_string)
        .filter(|value| !value.is_empty());
    if let Some(obj) = document.as_object_mut() {
        obj.insert("api_key".into(), Value::String(String::new()));
    }
    (document, key)
}

/// 导出本机模型配置。Key 用同步密码加密后随载荷上传。
pub fn export_model_config(
    store: &TelemetryStore,
    keys: Arc<dyn MasterKeys>,
    sync_key: &[u8; 32],
) -> Result<CloudModelConfig, String> {
    let registry = Registry::with_store(store.clone(), keys);
    let mut providers = Vec::new();
    let mut current_ids: Vec<(String, String, i64)> = Vec::new();
    for namespace in [LLM, CATALOG] {
        for document in registry.metadata_documents(namespace)? {
            let legacy_id = document["id"].as_str().unwrap_or_default().to_string();
            if legacy_id.is_empty() {
                continue;
            }
            let identity = registry.identity(namespace, &legacy_id)?;
            let (_, _, secret) = registry.resolve(namespace, &legacy_id)?;
            let secret = secret.to_string();
            current_ids.push((namespace.to_string(), legacy_id.clone(), identity.config_revision));
            providers.push(CloudModelProvider {
                namespace: namespace.to_string(),
                legacy_id,
                document,
                revision: identity.config_revision,
                secret_cipher: (!secret.is_empty()).then(|| encrypt_secret(sync_key, &secret)).transpose()?,
                clear_secret: false,
                updated_at: chrono::Utc::now().to_rfc3339(),
            });
        }
    }
    // 本地删除的 provider 以墓碑传播：与上一次导出集合做差集。
    let previous = store.app_setting_get::<Vec<(String, String, i64)>>(EXPORTED_IDS).unwrap_or_default();
    let mut tombstones = store.app_setting_get::<Vec<CloudModelTombstone>>(TOMBSTONES).unwrap_or_default();
    let now = chrono::Utc::now().to_rfc3339();
    for (namespace, legacy_id, revision) in &previous {
        if !current_ids.iter().any(|(ns, id, _)| ns == namespace && id == legacy_id)
            && !tombstones.iter().any(|tomb| &tomb.namespace == namespace && &tomb.legacy_id == legacy_id)
        {
            tombstones.push(CloudModelTombstone {
                namespace: namespace.clone(),
                legacy_id: legacy_id.clone(),
                revision: *revision,
                deleted_at: now.clone(),
            });
        }
    }
    store.app_setting_set(EXPORTED_IDS, &current_ids)?;
    store.app_setting_set(TOMBSTONES, &tombstones)?;

    // 未配置过的设置不发布：空配置不得覆盖对端已有配置。
    let stamp_optional = |setting: &str, stamp_key: &str| -> Result<Option<StampedConfig>, String> {
        let Some(config) = store.app_setting_get::<Value>(setting) else {
            return Ok(None);
        };
        let (document, key) = split_api_key(&config);
        let mut stamped = stamp(store, stamp_key, &document)?;
        stamped.api_key_cipher = key.map(|plain| encrypt_secret(sync_key, &plain)).transpose()?;
        Ok(Some(stamped))
    };
    let decision = stamp_optional(DECISION_KEY, STAMP_DECISION)?;
    let semantic = stamp_optional(SEMANTIC_KEY, STAMP_SEMANTIC)?;
    Ok(CloudModelConfig { format: MODEL_CONFIG_FORMAT, providers, tombstones, decision, semantic })
}

/// 应用远端模型配置。返回 (落地条数, 跳过条数)。
pub fn apply_model_config(
    store: &TelemetryStore,
    keys: Arc<dyn MasterKeys>,
    sync_key: &[u8; 32],
    remote: &CloudModelConfig,
) -> Result<(usize, usize), String> {
    if remote.format != MODEL_CONFIG_FORMAT {
        return Err(format!("模型配置是版本 {}，本机只认版本 {MODEL_CONFIG_FORMAT}", remote.format));
    }
    let registry = Registry::with_store(store.clone(), keys);
    let mut applied = 0usize;
    let mut skipped = 0usize;
    for tomb in &remote.tombstones {
        match registry.identity(&tomb.namespace, &tomb.legacy_id) {
            Ok(identity) if identity.config_revision <= tomb.revision => {
                registry.delete(&tomb.namespace, &tomb.legacy_id)?;
                applied += 1;
            }
            Ok(_) => skipped += 1,
            Err(_) => skipped += 1,
        }
    }
    for provider in &remote.providers {
        let local_revision = registry
            .identity(&provider.namespace, &provider.legacy_id)
            .map(|identity| identity.config_revision)
            .unwrap_or(0);
        if provider.revision <= local_revision {
            skipped += 1;
            continue;
        }
        let secret = match (&provider.secret_cipher, provider.clear_secret) {
            (Some(cipher), _) => Some(decrypt_secret(sync_key, cipher)?),
            (None, true) => Some(String::new()),
            // 远端没有 Key：保留本机既有 Key。既避免文档更新触发「改地址需重填 Key」
            // 守卫，也防止无 Key 的旧版本误删本机密钥；显式清除走 clear_secret。
            (None, false) => match registry.resolve(&provider.namespace, &provider.legacy_id) {
                Ok((_, _, local_secret)) => {
                    let local = local_secret.to_string();
                    if local.is_empty() { None } else { Some(local) }
                }
                Err(_) => None,
            },
        };
        registry.save(
            &provider.namespace,
            &provider.document,
            secret.as_deref(),
        )?;
        registry.set_sync_revision(&provider.namespace, &provider.legacy_id, provider.revision)?;
        applied += 1;
    }
    for (setting, stamped, stamp_key) in [
        (DECISION_KEY, remote.decision.as_ref(), STAMP_DECISION),
        (SEMANTIC_KEY, remote.semantic.as_ref(), STAMP_SEMANTIC),
    ] {
        let Some(stamped) = stamped else { continue };
        let local_at = store
            .app_setting_get::<StampedConfig>(stamp_key)
            .map(|old| old.at.clone());
        if local_at.as_deref() >= Some(stamped.at.as_str()) {
            skipped += 1;
            continue;
        }
        let mut config = stamped.config.clone();
        let key = stamped
            .api_key_cipher
            .as_deref()
            .map(|cipher| decrypt_secret(sync_key, cipher))
            .transpose()?
            .unwrap_or_default();
        if let Some(obj) = config.as_object_mut() {
            obj.insert("api_key".into(), Value::String(key));
        }
        store.app_setting_set(setting, &config)?;
        // 水位只保留哈希与时间戳：密文不重复落盘。
        let mut watermark = stamped.clone();
        watermark.api_key_cipher = None;
        store.app_setting_set(stamp_key, &watermark)?;
        applied += 1;
    }
    Ok((applied, skipped))
}
