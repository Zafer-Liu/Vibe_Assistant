//! One authoritative model registry. Legacy namespaces remain explicit aliases.
//! Secrets use a random key in the OS credential store and authenticated envelopes.
use crate::telemetry_store::TelemetryStore;
use aes_gcm::aead::{rand_core::RngCore, Aead, AeadCore, KeyInit, OsRng, Payload};
use aes_gcm::{Aes256Gcm, Nonce};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use uuid::Uuid;
use zeroize::Zeroizing;

pub const LLM: &str = "llm_providers";
pub const CATALOG: &str = "catalog_providers";
pub const MASTER: &str = "collaboration_settings";
static CONFIG_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
const LOCKED: &str =
    "系统凭证存储不可用或已锁定；请解锁系统钥匙串后重试。旧配置会保留，不会退回明文保存。";

pub(crate) trait MasterKeys: Send + Sync {
    fn get(&self, id: &str) -> Result<Option<Vec<u8>>, String>;
    fn put(&self, id: &str, key: &[u8]) -> Result<(), String>;
}
struct NativeKeys;
impl MasterKeys for NativeKeys {
    fn get(&self, id: &str) -> Result<Option<Vec<u8>>, String> {
        if !cfg!(any(
            target_os = "windows",
            target_os = "macos",
            target_os = "linux"
        )) {
            return Err(LOCKED.into());
        }
        let entry =
            keyring::Entry::new("com.agentmanager.model-vault.v1", id).map_err(|_| LOCKED)?;
        match entry.get_secret() {
            Ok(value) => Ok(Some(value)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(_) => Err(LOCKED.into()),
        }
    }
    fn put(&self, id: &str, key: &[u8]) -> Result<(), String> {
        if !cfg!(any(
            target_os = "windows",
            target_os = "macos",
            target_os = "linux"
        )) {
            return Err(LOCKED.into());
        }
        keyring::Entry::new("com.agentmanager.model-vault.v1", id)
            .map_err(|_| LOCKED)?
            .set_secret(key)
            .map_err(|_| LOCKED.into())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ModelIdentity {
    pub provider_id: String,
    pub deployment_id: Option<String>,
    pub credential_id: Option<String>,
    pub config_revision: i64,
}
#[derive(Clone)]
pub(crate) struct Registry {
    pub store: Arc<TelemetryStore>,
    keys: Arc<dyn MasterKeys>,
}
#[derive(Clone)]
pub(crate) struct Cleanup {
    pub path: PathBuf,
    pub original: Vec<u8>,
    pub sanitized: Vec<u8>,
}

pub(crate) fn initialize(conn: &Connection) -> Result<(), String> {
    conn.execute_batch("CREATE TABLE IF NOT EXISTS model_vault_state(id INTEGER PRIMARY KEY CHECK(id=1),key_id TEXT NOT NULL);
        CREATE TABLE IF NOT EXISTS model_secret_values(id TEXT PRIMARY KEY,key_id TEXT NOT NULL,envelope BLOB NOT NULL);
        CREATE TABLE IF NOT EXISTS model_provider_connections(
          id TEXT PRIMARY KEY,namespace TEXT NOT NULL,legacy_id TEXT NOT NULL,document_json TEXT NOT NULL,
          protocol TEXT NOT NULL,revision INTEGER NOT NULL DEFAULT 1,UNIQUE(namespace,legacy_id));
        CREATE TABLE IF NOT EXISTS model_provider_credentials(id TEXT PRIMARY KEY,provider_id TEXT NOT NULL UNIQUE REFERENCES model_provider_connections(id) ON DELETE CASCADE,secret_ref TEXT NOT NULL UNIQUE REFERENCES model_secret_values(id));
        CREATE TABLE IF NOT EXISTS model_provider_deployments(id TEXT PRIMARY KEY,provider_id TEXT NOT NULL UNIQUE REFERENCES model_provider_connections(id) ON DELETE CASCADE,upstream_model TEXT NOT NULL);
        CREATE TABLE IF NOT EXISTS model_config_migrations(namespace TEXT PRIMARY KEY,backup_ref TEXT,imported_at TEXT NOT NULL);
        CREATE TABLE IF NOT EXISTS model_legacy_documents(namespace TEXT PRIMARY KEY,original_hash TEXT NOT NULL,sanitized_json TEXT NOT NULL);
        CREATE TABLE IF NOT EXISTS model_legacy_cleanups(namespace TEXT NOT NULL,path TEXT NOT NULL,original_hash TEXT NOT NULL,sanitized BLOB NOT NULL,PRIMARY KEY(namespace,path));").map_err(|e| e.to_string())
}

fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn contains_key(v: &Value) -> bool {
    match v {
        Value::Object(map) => map.iter().any(|(k, v)| {
            k == "api_key" && v.as_str().is_some_and(|s| !s.is_empty()) || contains_key(v)
        }),
        Value::Array(values) => values.iter().any(contains_key),
        _ => false,
    }
}
fn seal(key: &[u8], id: &str, bytes: &[u8]) -> Result<Vec<u8>, String> {
    let cipher = Aes256Gcm::new_from_slice(key).map_err(|_| LOCKED)?;
    let nonce = Aes256Gcm::generate_nonce(&mut OsRng);
    let encrypted = cipher
        .encrypt(
            &nonce,
            Payload {
                msg: bytes,
                aad: id.as_bytes(),
            },
        )
        .map_err(|_| "无法保护模型凭证")?;
    Ok([nonce.as_slice(), encrypted.as_slice()].concat())
}
fn open(key: &[u8], id: &str, bytes: &[u8]) -> Result<Vec<u8>, String> {
    if bytes.len() < 28 {
        return Err("模型凭证密文损坏，请从原始凭证重新配置".into());
    }
    Aes256Gcm::new_from_slice(key)
        .map_err(|_| LOCKED)?
        .decrypt(
            Nonce::from_slice(&bytes[..12]),
            Payload {
                msg: &bytes[12..],
                aad: id.as_bytes(),
            },
        )
        .map_err(|_| "模型凭证无法解密；请恢复系统凭证存储，或重新配置 Key".into())
}

impl Registry {
    pub fn shared() -> Result<Self, String> {
        let store = crate::telemetry_store::shared_store().ok_or("模型注册表尚未初始化")?;
        Ok(Self {
            store,
            keys: Arc::new(NativeKeys),
        })
    }
    fn master(&self) -> Result<(String, Zeroizing<Vec<u8>>), String> {
        let conn = self.store.conn.lock().map_err(|_| "模型注册表锁已损坏")?;
        let existing: Option<String> = conn
            .query_row("SELECT key_id FROM model_vault_state WHERE id=1", [], |r| {
                r.get(0)
            })
            .optional()
            .map_err(|e| e.to_string())?;
        drop(conn);
        if let Some(id) = existing {
            let key = Zeroizing::new(self.keys.get(&id)?.ok_or(
                "系统中的模型保护密钥已丢失；不会生成新密钥覆盖旧凭证，请恢复原系统钥匙串",
            )?);
            if key.len() != 32 {
                return Err("系统模型保护密钥格式损坏".into());
            }
            return Ok((id, key));
        }
        let id = Uuid::new_v4().to_string();
        let mut key = Zeroizing::new(vec![0; 32]);
        OsRng.fill_bytes(&mut key);
        self.keys.put(&id, &key)?;
        let verified = Zeroizing::new(self.keys.get(&id)?.ok_or(LOCKED)?);
        if verified.as_slice() != key.as_slice() {
            return Err("系统凭证写入验证失败；旧配置未更改".into());
        }
        let conn = self.store.conn.lock().map_err(|_| "模型注册表锁已损坏")?;
        conn.execute(
            "INSERT OR IGNORE INTO model_vault_state(id,key_id) VALUES(1,?1)",
            [&id],
        )
        .map_err(|e| e.to_string())?;
        let actual: String = conn
            .query_row("SELECT key_id FROM model_vault_state WHERE id=1", [], |r| {
                r.get(0)
            })
            .map_err(|e| e.to_string())?;
        drop(conn);
        if actual != id {
            return self.master();
        }
        Ok((id, key))
    }
    pub fn legacy_setting(&self, key: &str) -> Result<Option<Value>, String> {
        let conn = self.store.conn.lock().map_err(|_| "模型注册表锁已损坏")?;
        let raw: Option<String> = conn
            .query_row(
                "SELECT value_json FROM app_settings WHERE setting_key=?1",
                [key],
                |r| r.get(0),
            )
            .optional()
            .map_err(|e| e.to_string())?;
        raw.map(|s| {
            serde_json::from_str(&s)
                .map_err(|_| "旧模型数据库设置损坏，已停止迁移并保留原数据".into())
        })
        .transpose()
    }
    pub fn migrated(&self, namespace: &str) -> Result<bool, String> {
        self.store
            .conn
            .lock()
            .map_err(|_| "模型注册表锁已损坏")?
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM model_config_migrations WHERE namespace=?1)",
                [namespace],
                |r| r.get(0),
            )
            .map_err(|e| e.to_string())
    }
    /// Recovery envelope, credentials, sanitized settings and migration marker commit together.
    pub fn import(
        &self,
        namespace: &str,
        rows: Vec<Value>,
        backup: Value,
        setting: Option<&str>,
        cleanups: Vec<Cleanup>,
    ) -> Result<(), String> {
        let _guard = CONFIG_LOCK
            .get_or_init(|| Mutex::new(()))
            .lock()
            .map_err(|_| "模型配置锁已损坏")?;
        if self.migrated(namespace)? {
            return self.cleanup(namespace);
        }
        let mut seen = std::collections::HashSet::new();
        for row in &rows {
            let id = row["id"].as_str().ok_or("旧模型配置缺少 ID，迁移已停止")?;
            if id.is_empty() || !seen.insert(id.to_string()) {
                return Err("旧模型配置包含空或重复 ID，迁移已停止".into());
            }
        }
        let needs_key = contains_key(&backup) || rows.iter().any(contains_key);
        let master = if needs_key {
            Some(self.master()?)
        } else {
            None
        };
        let mut conn = self.store.conn.lock().map_err(|_| "模型注册表锁已损坏")?;
        conn.pragma_update(None, "secure_delete", "ON")
            .map_err(|e| e.to_string())?;
        let tx = conn.transaction().map_err(|e| e.to_string())?;
        if tx
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM model_config_migrations WHERE namespace=?1)",
                [namespace],
                |r| r.get::<_, bool>(0),
            )
            .map_err(|e| e.to_string())?
        {
            drop(tx);
            drop(conn);
            return self.cleanup(namespace);
        }
        let backup_ref = if let Some((key_id, key)) = &master {
            let id = Uuid::new_v4().to_string();
            let bytes = Zeroizing::new(serde_json::to_vec(&backup).map_err(|e| e.to_string())?);
            let encrypted = seal(key, &id, &bytes)?;
            if Zeroizing::new(open(key, &id, &encrypted)?).as_slice() != bytes.as_slice() {
                return Err("模型配置恢复副本校验失败".into());
            }
            tx.execute(
                "INSERT INTO model_secret_values(id,key_id,envelope) VALUES(?1,?2,?3)",
                params![id, key_id, encrypted],
            )
            .map_err(|e| e.to_string())?;
            let stored: Vec<u8> = tx
                .query_row(
                    "SELECT envelope FROM model_secret_values WHERE id=?1",
                    [&id],
                    |row| row.get(0),
                )
                .map_err(|e| e.to_string())?;
            if Zeroizing::new(open(key, &id, &stored)?).as_slice() != bytes.as_slice() {
                return Err("模型配置恢复副本回读验证失败".into());
            }
            Some(id)
        } else {
            None
        };
        let mut sanitized = Vec::new();
        for mut row in rows {
            let raw = row
                .get("api_key")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let secret = Zeroizing::new(if namespace == CATALOG {
                raw.to_string()
            } else {
                crate::llm::decrypt_api_key(raw)
            });
            if !raw.is_empty() && secret.is_empty() {
                return Err("旧模型 Key 无法解密，迁移已停止；请先恢复原配置或重新填写 Key".into());
            }
            row["api_key"] = Value::String(String::new());
            if let Some(obj) = row.as_object_mut() {
                obj.remove("has_api_key");
                obj.remove("clear_key");
            }
            self.upsert_tx(&tx, namespace, &row, Some(&secret), master.as_ref())?;
            sanitized.push(row);
        }
        if let Some(setting) = setting {
            tx.execute(
                "UPDATE app_settings SET value_json=?2,updated_at=?3 WHERE setting_key=?1",
                params![
                    setting,
                    serde_json::to_string(&sanitized).map_err(|e| e.to_string())?,
                    chrono::Utc::now().to_rfc3339()
                ],
            )
            .map_err(|e| e.to_string())?;
        }
        for cleanup in cleanups {
            tx.execute("INSERT INTO model_legacy_cleanups(namespace,path,original_hash,sanitized) VALUES(?1,?2,?3,?4)",params![namespace,cleanup.path.to_string_lossy(),hash(&cleanup.original),cleanup.sanitized]).map_err(|e|e.to_string())?;
        }
        tx.execute("INSERT INTO model_config_migrations(namespace,backup_ref,imported_at) VALUES(?1,?2,?3)",params![namespace,backup_ref,chrono::Utc::now().to_rfc3339()]).map_err(|e|e.to_string())?;
        if namespace == MASTER {
            tx.execute("INSERT INTO model_legacy_documents(namespace,original_hash,sanitized_json) VALUES(?1,?2,?3)",params![namespace,hash(&serde_json::to_vec(&backup).map_err(|e|e.to_string())?),String::from_utf8(sanitized_bytes(&backup)?).map_err(|e|e.to_string())?]).map_err(|e|e.to_string())?;
        }
        tx.commit().map_err(|e| e.to_string())?;
        drop(conn);
        self.cleanup(namespace)
    }
    // The removed collaboration settings document is exercised by migration fixtures only.
    #[cfg(test)]
    pub fn legacy_document_cleanup(
        &self,
        namespace: &str,
        current: &Value,
    ) -> Result<Option<Value>, String> {
        let conn = self.store.conn.lock().map_err(|_| "模型注册表锁已损坏")?;
        let expected:Option<(String,String)>=conn.query_row("SELECT original_hash,sanitized_json FROM model_legacy_documents WHERE namespace=?1",[namespace],|r|Ok((r.get(0)?,r.get(1)?))).optional().map_err(|e|e.to_string())?;
        let Some((original, sanitized)) = expected else {
            return Err("旧数据库配置缺少迁移清理证据，已保留原记录".into());
        };
        let safe: Value =
            serde_json::from_str(&sanitized).map_err(|_| "旧数据库配置清理记录损坏")?;
        if &safe == current {
            return Ok(None);
        }
        if hash(&serde_json::to_vec(current).map_err(|e| e.to_string())?) != original {
            return Err(
                "旧协同模型配置在迁移后发生变化，已保留新记录，请关闭其他版本并核对配置".into(),
            );
        }
        Ok(Some(safe))
    }

    pub fn cleanup(&self, namespace: &str) -> Result<(), String> {
        let conn = self.store.conn.lock().map_err(|_| "模型注册表锁已损坏")?;
        let mut stmt = conn
            .prepare(
                "SELECT path,original_hash,sanitized FROM model_legacy_cleanups WHERE namespace=?1",
            )
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([namespace], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, Vec<u8>>(2)?,
                ))
            })
            .map_err(|e| e.to_string())?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())?;
        drop(stmt);
        drop(conn);
        for (path, original, sanitized) in rows {
            let path = Path::new(&path);
            if !path.exists() {
                continue;
            }
            let current = std::fs::read(path)
                .map_err(|_| "无法读取旧模型配置；加密迁移已保存，请恢复文件访问权限后重试")?;
            if current == sanitized {
                continue;
            }
            if hash(&current) != original {
                return Err("旧模型配置在迁移后发生变化；为避免覆盖新凭证，已停止清理。请关闭其他版本并核对原配置".into());
            }
            atomic_file(path, &sanitized)?;
        }
        Ok(())
    }
    fn upsert_tx(
        &self,
        conn: &Connection,
        namespace: &str,
        document: &Value,
        secret: Option<&str>,
        master: Option<&(String, Zeroizing<Vec<u8>>)>,
    ) -> Result<ModelIdentity, String> {
        let legacy = document["id"].as_str().ok_or("模型配置 ID 无效")?;
        if legacy.is_empty() || legacy.len() > 256 {
            return Err("模型配置 ID 无效".into());
        }
        if let Some(base) = document["base_url"].as_str().filter(|s| !s.is_empty()) {
            let url = reqwest::Url::parse(base).map_err(|_| "模型连接 URL 无效；旧配置已保留")?;
            if !matches!(url.scheme(), "http" | "https")
                || url.host_str().is_none()
                || !url.username().is_empty()
                || url.password().is_some()
                || url.query().is_some()
                || url.fragment().is_some()
            {
                return Err("模型地址不能包含凭据或查询参数；请使用 Key 字段，旧配置已保留".into());
            }
        }
        let existing: Option<String> = conn
            .query_row(
                "SELECT id FROM model_provider_connections WHERE namespace=?1 AND legacy_id=?2",
                params![namespace, legacy],
                |r| r.get(0),
            )
            .optional()
            .map_err(|e| e.to_string())?;
        let provider_id = existing.unwrap_or_else(|| Uuid::new_v4().to_string());
        let mut safe = document.clone();
        safe["api_key"] = Value::String(String::new());
        if let Some(obj) = safe.as_object_mut() {
            obj.remove("has_api_key");
            obj.remove("clear_key");
            obj.remove("identity");
            obj.remove("key_mask");
        }
        let protocol = if namespace == CATALOG {
            safe["api_format"].as_str().unwrap_or("openai_chat")
        } else {
            "openai_chat"
        };
        conn.execute("INSERT INTO model_provider_connections(id,namespace,legacy_id,document_json,protocol) VALUES(?1,?2,?3,?4,?5) ON CONFLICT(namespace,legacy_id) DO UPDATE SET document_json=excluded.document_json,protocol=excluded.protocol,revision=revision+1",params![provider_id,namespace,legacy,serde_json::to_string(&safe).map_err(|e|e.to_string())?,protocol]).map_err(|e|e.to_string())?;
        if namespace != CATALOG {
            conn.execute("INSERT INTO model_provider_deployments(id,provider_id,upstream_model) VALUES(?1,?2,?3) ON CONFLICT(provider_id) DO UPDATE SET upstream_model=excluded.upstream_model",params![Uuid::new_v4().to_string(),provider_id,safe["model"].as_str().unwrap_or_default()]).map_err(|e|e.to_string())?;
        }
        if let Some(secret) = secret {
            // A replacement is copy-on-write. No OS key or old ciphertext is overwritten.
            let old: Option<String> = conn
                .query_row(
                    "SELECT secret_ref FROM model_provider_credentials WHERE provider_id=?1",
                    [&provider_id],
                    |r| r.get(0),
                )
                .optional()
                .map_err(|e| e.to_string())?;
            if secret.is_empty() {
                conn.execute(
                    "DELETE FROM model_provider_credentials WHERE provider_id=?1",
                    [&provider_id],
                )
                .map_err(|e| e.to_string())?;
            } else {
                let (key_id, key) = master.ok_or(LOCKED)?;
                let id = Uuid::new_v4().to_string();
                let encrypted = seal(key, &id, secret.as_bytes())?;
                if Zeroizing::new(open(key, &id, &encrypted)?).as_slice() != secret.as_bytes() {
                    return Err("模型凭证写入验证失败".into());
                }
                conn.execute(
                    "INSERT INTO model_secret_values(id,key_id,envelope) VALUES(?1,?2,?3)",
                    params![id, key_id, encrypted],
                )
                .map_err(|e| e.to_string())?;
                conn.execute("INSERT INTO model_provider_credentials(id,provider_id,secret_ref) VALUES(?1,?2,?3) ON CONFLICT(provider_id) DO UPDATE SET secret_ref=excluded.secret_ref",params![Uuid::new_v4().to_string(),provider_id,id]).map_err(|e|e.to_string())?;
            }
            if let Some(old) = old {
                conn.execute("DELETE FROM model_secret_values WHERE id=?1", [old])
                    .map_err(|e| e.to_string())?;
            }
        }
        identity(conn, namespace, legacy)
    }
    pub fn documents(&self, namespace: &str) -> Result<Vec<Value>, String> {
        self.cleanup(namespace)?;
        self.metadata_documents(namespace)
    }
    pub fn metadata_documents(&self, namespace: &str) -> Result<Vec<Value>, String> {
        let conn = self.store.conn.lock().map_err(|_| "模型注册表锁已损坏")?;
        let mut stmt=conn.prepare("SELECT document_json FROM model_provider_connections WHERE namespace=?1 ORDER BY rowid").map_err(|e|e.to_string())?;
        let rows = stmt
            .query_map([namespace], |r| r.get::<_, String>(0))
            .map_err(|e| e.to_string())?;
        rows.map(|r| {
            serde_json::from_str(&r.map_err(|e| e.to_string())?).map_err(|e| e.to_string())
        })
        .collect()
    }
    pub fn identity(&self, namespace: &str, id: &str) -> Result<ModelIdentity, String> {
        identity(
            &*self.store.conn.lock().map_err(|_| "模型注册表锁已损坏")?,
            namespace,
            id,
        )
    }
    pub fn save(
        &self,
        namespace: &str,
        document: &Value,
        secret: Option<&str>,
    ) -> Result<ModelIdentity, String> {
        let _guard = CONFIG_LOCK
            .get_or_init(|| Mutex::new(()))
            .lock()
            .map_err(|_| "模型配置锁已损坏")?;
        if let Some(key) = secret {
            if key.len() > 64 * 1024 {
                return Err("模型 Key 超过保存上限".into());
            }
        }
        let master = if secret.is_some_and(|s| !s.is_empty()) {
            Some(self.master()?)
        } else {
            None
        };
        let mut conn = self.store.conn.lock().map_err(|_| "模型注册表锁已损坏")?;
        conn.pragma_update(None, "secure_delete", "ON")
            .map_err(|e| e.to_string())?;
        let tx = conn.transaction().map_err(|e| e.to_string())?;
        if secret.is_none() {
            let old:Option<(String,bool)>=tx.query_row("SELECT p.document_json,EXISTS(SELECT 1 FROM model_provider_credentials c WHERE c.provider_id=p.id) FROM model_provider_connections p WHERE namespace=?1 AND legacy_id=?2",params![namespace,document["id"].as_str().unwrap_or_default()],|r|Ok((r.get(0)?,r.get(1)?))).optional().map_err(|e|e.to_string())?;
            if let Some((old, true)) = old {
                let old: Value = serde_json::from_str(&old).map_err(|e| e.to_string())?;
                if old["base_url"]
                    .as_str()
                    .unwrap_or_default()
                    .trim_end_matches('/')
                    != document["base_url"]
                        .as_str()
                        .unwrap_or_default()
                        .trim_end_matches('/')
                {
                    return Err("更改模型地址时请重新填写 Key 或明确清除旧 Key".into());
                }
            }
        }
        let result = self.upsert_tx(&tx, namespace, document, secret, master.as_ref())?;
        tx.commit().map_err(|e| e.to_string())?;
        Ok(result)
    }
    pub fn resolve(
        &self,
        namespace: &str,
        id: &str,
    ) -> Result<(Value, ModelIdentity, Zeroizing<String>), String> {
        let conn = self.store.conn.lock().map_err(|_| "模型注册表锁已损坏")?;
        let document:String=conn.query_row("SELECT document_json FROM model_provider_connections WHERE namespace=?1 AND legacy_id=?2",params![namespace,id],|r|r.get(0)).map_err(|_|"模型连接不存在")?;
        let identity = identity(&conn, namespace, id)?;
        let record:Option<(String,String,Vec<u8>)>=conn.query_row("SELECT s.id,s.key_id,s.envelope FROM model_provider_connections p JOIN model_provider_credentials c ON c.provider_id=p.id JOIN model_secret_values s ON s.id=c.secret_ref WHERE p.namespace=?1 AND p.legacy_id=?2",params![namespace,id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional().map_err(|e|e.to_string())?;
        drop(conn);
        let document = serde_json::from_str(&document).map_err(|_| "模型注册表配置损坏")?;
        let Some((secret_ref, key_id, encrypted)) = record else {
            return Ok((document, identity, Zeroizing::new(String::new())));
        };
        let key = Zeroizing::new(self.keys.get(&key_id)?.ok_or(LOCKED)?);
        let plain = Zeroizing::new(open(&key, &secret_ref, &encrypted)?);
        let secret = String::from_utf8(plain.to_vec())
            .map(Zeroizing::new)
            .map_err(|_| "模型凭证格式损坏")?;
        Ok((document, identity, secret))
    }
    pub fn delete(&self, namespace: &str, id: &str) -> Result<(), String> {
        let _guard = CONFIG_LOCK
            .get_or_init(|| Mutex::new(()))
            .lock()
            .map_err(|_| "模型配置锁已损坏")?;
        let mut conn = self.store.conn.lock().map_err(|_| "模型注册表锁已损坏")?;
        conn.pragma_update(None, "secure_delete", "ON")
            .map_err(|e| e.to_string())?;
        let tx = conn.transaction().map_err(|e| e.to_string())?;
        let secret:Option<String>=tx.query_row("SELECT c.secret_ref FROM model_provider_credentials c JOIN model_provider_connections p ON p.id=c.provider_id WHERE p.namespace=?1 AND p.legacy_id=?2",params![namespace,id],|r|r.get(0)).optional().map_err(|e|e.to_string())?;
        tx.execute(
            "DELETE FROM model_provider_connections WHERE namespace=?1 AND legacy_id=?2",
            params![namespace, id],
        )
        .map_err(|e| e.to_string())?;
        if let Some(secret) = secret {
            tx.execute("DELETE FROM model_secret_values WHERE id=?1", [secret])
                .map_err(|e| e.to_string())?;
        }
        tx.commit().map_err(|e| e.to_string())
    }
}
fn identity(conn: &Connection, namespace: &str, id: &str) -> Result<ModelIdentity, String> {
    conn.query_row("SELECT p.id,d.id,c.id,p.revision FROM model_provider_connections p LEFT JOIN model_provider_deployments d ON d.provider_id=p.id LEFT JOIN model_provider_credentials c ON c.provider_id=p.id WHERE p.namespace=?1 AND p.legacy_id=?2",params![namespace,id],|r|Ok(ModelIdentity{provider_id:r.get(0)?,deployment_id:r.get(1)?,credential_id:r.get(2)?,config_revision:r.get(3)?})).map_err(|_|"模型连接不存在".into())
}
pub(crate) fn read_optional(path: &Path) -> Result<Option<(Value, Vec<u8>)>, String> {
    match std::fs::read(path) {
        Ok(bytes) => {
            let value = serde_json::from_slice(&bytes)
                .map_err(|_| "旧模型配置不是有效 JSON，已保留原文件，请修复后重试")?;
            Ok(Some((value, bytes)))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(_) => Err("旧模型配置无法读取，已停止迁移".into()),
    }
}
pub(crate) fn sanitized_bytes(value: &Value) -> Result<Vec<u8>, String> {
    fn clean(v: &mut Value) {
        match v {
            Value::Object(map) => {
                for (k, v) in map {
                    if k == "api_key" {
                        *v = Value::String(String::new())
                    } else {
                        clean(v)
                    }
                }
            }
            Value::Array(a) => a.iter_mut().for_each(clean),
            _ => {}
        }
    }
    let mut safe = value.clone();
    clean(&mut safe);
    serde_json::to_vec_pretty(&safe).map_err(|e| e.to_string())
}
fn atomic_file(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let parent = path.parent().ok_or("旧模型配置路径无效")?;
    let mut tmp = tempfile::NamedTempFile::new_in(parent)
        .map_err(|_| "无法写入旧配置清理文件；迁移已保存，请修复目录权限后重试")?;
    use std::io::Write;
    tmp.write_all(bytes)
        .and_then(|_| tmp.as_file().sync_all())
        .map_err(|_| "旧配置清理写入失败")?;
    tmp.persist(path)
        .map_err(|_| "旧配置清理替换失败；请关闭占用配置的程序后重试")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicBool, Ordering};
    #[derive(Default)]
    struct MemoryKeys {
        keys: Mutex<HashMap<String, Vec<u8>>>,
        unavailable: AtomicBool,
    }
    impl MasterKeys for MemoryKeys {
        fn get(&self, id: &str) -> Result<Option<Vec<u8>>, String> {
            if self.unavailable.load(Ordering::SeqCst) {
                return Err(LOCKED.into());
            }
            Ok(self.keys.lock().unwrap().get(id).cloned())
        }
        fn put(&self, id: &str, key: &[u8]) -> Result<(), String> {
            if self.unavailable.load(Ordering::SeqCst) {
                return Err(LOCKED.into());
            }
            self.keys.lock().unwrap().insert(id.into(), key.into());
            Ok(())
        }
    }
    fn fixture() -> (Registry, Arc<MemoryKeys>) {
        let keys = Arc::new(MemoryKeys::default());
        (
            Registry {
                store: Arc::new(TelemetryStore::in_memory()),
                keys: keys.clone(),
            },
            keys,
        )
    }
    fn document(id: &str, key: &str) -> Value {
        json!({"id":id,"name":"fixture","base_url":"https://fixture.invalid/v1","model":"model-a","api_key":key,"enabled":true,"is_custom":true})
    }
    #[test]
    fn migration_is_atomic_idempotent_masked_and_recoverable() {
        let (r, _) = fixture();
        let row = document("same", "secret-fixture");
        r.store
            .app_setting_set("llm_providers", &vec![row.clone()])
            .unwrap();
        r.import(
            LLM,
            vec![row.clone()],
            json!({"original":[row]}),
            Some("llm_providers"),
            vec![],
        )
        .unwrap();
        let public = r.documents(LLM).unwrap();
        assert!(!serde_json::to_string(&public)
            .unwrap()
            .contains("secret-fixture"));
        let (_, first, key) = r.resolve(LLM, "same").unwrap();
        assert_eq!(key.as_str(), "secret-fixture");
        assert!(first.deployment_id.is_some());
        assert!(first.credential_id.is_some());
        let setting: Value = r.store.app_setting_get("llm_providers").unwrap();
        assert_eq!(setting[0]["api_key"], "");
        let conn = r.store.conn.lock().unwrap();
        let (id,key_id,envelope):(String,String,Vec<u8>)=conn.query_row("SELECT s.id,s.key_id,s.envelope FROM model_config_migrations m JOIN model_secret_values s ON s.id=m.backup_ref WHERE m.namespace=?1",[LLM],|row|Ok((row.get(0)?,row.get(1)?,row.get(2)?))).unwrap();
        drop(conn);
        let key = r.keys.get(&key_id).unwrap().unwrap();
        let backup: Value = serde_json::from_slice(&open(&key, &id, &envelope).unwrap()).unwrap();
        assert_eq!(backup["original"][0]["api_key"], "secret-fixture");
        r.import(
            LLM,
            vec![document("same", "new-ignored")],
            json!({}),
            None,
            vec![],
        )
        .unwrap();
        let (_, second, key) = r.resolve(LLM, "same").unwrap();
        assert_eq!(first.provider_id, second.provider_id);
        assert_eq!(key.as_str(), "secret-fixture");
        let encrypted = crate::llm::encrypt_api_key("legacy-v2-key").unwrap();
        let row = document("encrypted", &encrypted);
        r.import(MASTER, vec![row.clone()], row, None, vec![])
            .unwrap();
        assert_eq!(
            r.resolve(MASTER, "encrypted").unwrap().2.as_str(),
            "legacy-v2-key"
        );
    }
    #[test]
    fn locked_store_preserves_sources_and_never_commits_partial_migration() {
        let (r, keys) = fixture();
        keys.unavailable.store(true, Ordering::SeqCst);
        let row = document("p", "secret-fixture");
        r.store.app_setting_set(LLM, &vec![row.clone()]).unwrap();
        assert!(r
            .import(LLM, vec![row.clone()], row.clone(), Some(LLM), vec![])
            .is_err());
        assert!(!r.migrated(LLM).unwrap());
        assert!(r.documents(LLM).unwrap().is_empty());
        let old: Vec<Value> = r.store.app_setting_get(LLM).unwrap();
        assert_eq!(old[0]["api_key"], "secret-fixture");
        keys.unavailable.store(false, Ordering::SeqCst);
        r.import(LLM, vec![row.clone()], row, Some(LLM), vec![])
            .unwrap();
    }
    #[test]
    fn duplicate_ids_corrupt_encryption_and_sql_failure_do_not_sanitize_sources() {
        let (r, _) = fixture();
        let row = document("p", "secret-fixture");
        assert!(r
            .import(
                LLM,
                vec![row.clone(), row.clone()],
                row.clone(),
                None,
                vec![]
            )
            .is_err());
        let bad = document("p", "v2:broken");
        assert!(r.import(LLM, vec![bad.clone()], bad, None, vec![]).is_err());
        r.store.app_setting_set(LLM, &vec![row.clone()]).unwrap();
        r.store.conn.lock().unwrap().execute_batch("CREATE TRIGGER fail_import BEFORE INSERT ON model_provider_connections BEGIN SELECT RAISE(ABORT,'fixture failure'); END;").unwrap();
        assert!(r
            .import(LLM, vec![row.clone()], row, Some(LLM), vec![])
            .is_err());
        assert!(!r.migrated(LLM).unwrap());
        assert!(r.documents(LLM).unwrap().is_empty());
        let old: Vec<Value> = r.store.app_setting_get(LLM).unwrap();
        assert_eq!(old[0]["api_key"], "secret-fixture");
    }
    #[test]
    fn keep_replace_clear_and_namespace_identity_are_independent() {
        let (r, _) = fixture();
        let row = document("same", "");
        let initial = r.save(LLM, &row, Some("one")).unwrap();
        let mut changed = row.clone();
        changed["model"] = json!("model-b");
        let kept = r.save(LLM, &changed, None).unwrap();
        assert_eq!(kept.provider_id, initial.provider_id);
        assert_eq!(kept.deployment_id, initial.deployment_id);
        assert_eq!(kept.credential_id, initial.credential_id);
        assert!(kept.config_revision > initial.config_revision);
        assert_eq!(r.resolve(LLM, "same").unwrap().2.as_str(), "one");
        let replaced = r.save(LLM, &changed, Some("two")).unwrap();
        assert_eq!(replaced.credential_id, initial.credential_id);
        assert_eq!(r.resolve(LLM, "same").unwrap().2.as_str(), "two");
        let other = r.save(CATALOG, &row, Some("other")).unwrap();
        assert_ne!(other.provider_id, initial.provider_id);
        assert!(other.deployment_id.is_none());
        r.save(LLM, &changed, Some("")).unwrap();
        assert!(r.identity(LLM, "same").unwrap().credential_id.is_none());
        assert!(r.resolve(LLM, "same").unwrap().2.is_empty());
        assert_eq!(r.resolve(CATALOG, "same").unwrap().2.as_str(), "other");
        r.delete(LLM, "same").unwrap();
        assert!(r.resolve(LLM, "same").is_err());
        assert!(r.resolve(CATALOG, "same").is_ok());
    }
    #[test]
    fn changed_endpoint_requires_explicit_credential_action() {
        let (r, _) = fixture();
        let mut row = document("p", "");
        r.save(LLM, &row, Some("one")).unwrap();
        row["base_url"] = json!("https://other.invalid");
        assert!(r.save(LLM, &row, None).is_err());
        let (saved, _, secret) = r.resolve(LLM, "p").unwrap();
        assert_eq!(saved["base_url"], "https://fixture.invalid/v1");
        assert_eq!(secret.as_str(), "one");
        r.save(LLM, &row, Some("two")).unwrap();
        assert_eq!(r.resolve(LLM, "p").unwrap().2.as_str(), "two");
    }
    #[test]
    fn missing_master_and_tampered_or_swapped_ciphertext_fail_closed() {
        let (r, keys) = fixture();
        r.save(LLM, &document("a", ""), Some("one")).unwrap();
        r.save(LLM, &document("b", ""), Some("two")).unwrap();
        r.store.conn.lock().unwrap().execute_batch("UPDATE model_secret_values SET envelope=(SELECT envelope FROM model_secret_values ORDER BY rowid LIMIT 1);").unwrap();
        assert!(r.resolve(LLM, "b").is_err());
        keys.keys.lock().unwrap().clear();
        assert!(r.resolve(LLM, "a").is_err());
        assert!(r
            .save(LLM, &document("c", ""), Some("replacement"))
            .is_err());
        assert!(keys.keys.lock().unwrap().is_empty());
    }
    #[test]
    fn cleanup_is_restart_safe_and_rejects_external_edits() {
        let (r, _) = fixture();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("legacy.json");
        let source = json!([document("p", "secret-fixture")]);
        let original = serde_json::to_vec(&source).unwrap();
        std::fs::write(&path, &original).unwrap();
        let sanitized = sanitized_bytes(&source).unwrap();
        r.import(
            LLM,
            vec![document("p", "secret-fixture")],
            source,
            None,
            vec![Cleanup {
                path: path.clone(),
                original: original.clone(),
                sanitized: sanitized.clone(),
            }],
        )
        .unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), sanitized);
        r.cleanup(LLM).unwrap();
        std::fs::write(&path, &original).unwrap();
        r.cleanup(LLM).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), sanitized);
        std::fs::write(&path, b"external edit").unwrap();
        assert!(r.cleanup(LLM).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), b"external edit");
    }
    #[test]
    fn database_cleanup_rejects_a_new_key_written_after_migration() {
        let (r, _) = fixture();
        let original = document("collaboration-master", "original-key");
        r.import(
            MASTER,
            vec![original.clone()],
            original.clone(),
            None,
            vec![],
        )
        .unwrap();
        let safe = r
            .legacy_document_cleanup(MASTER, &original)
            .unwrap()
            .unwrap();
        assert_eq!(safe["api_key"], "");
        assert!(r.legacy_document_cleanup(MASTER, &safe).unwrap().is_none());
        let mut external = original.clone();
        external["api_key"] = json!("new-unbacked-key");
        assert!(r.legacy_document_cleanup(MASTER, &external).is_err());
        assert_eq!(
            r.resolve(MASTER, "collaboration-master")
                .unwrap()
                .2
                .as_str(),
            "original-key"
        );
    }

    #[test]
    fn malformed_legacy_file_and_setting_are_not_treated_as_empty() {
        let (r, _) = fixture();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("broken.json");
        std::fs::write(&path, b"{broken").unwrap();
        assert!(read_optional(&path).is_err());
        r.store.conn.lock().unwrap().execute("INSERT INTO app_settings(setting_key,value_json,updated_at) VALUES(?1,'{broken','now')",[LLM]).unwrap();
        assert!(r.legacy_setting(LLM).is_err());
    }
    #[test]
    #[ignore = "explicit OS credential-store smoke test; creates and deletes only a random fixture entry"]
    fn native_credential_store_round_trip() {
        let id = format!("fixture-{}", Uuid::new_v4());
        let entry = keyring::Entry::new("com.agentmanager.model-vault.v1", &id).unwrap();
        let secret = [42u8; 32];
        NativeKeys.put(&id, &secret).unwrap();
        let actual = NativeKeys.get(&id);
        let cleanup = entry.delete_credential();
        assert_eq!(actual.unwrap().unwrap(), secret);
        cleanup.unwrap();
    }
}
