//! 云端记忆库（Cloud Memory Vault）客户端同步引擎。
//! 设计见 docs/16-cloud-memory-vault-design.md：本地 SQLite 权威、
//! 端到端 AES-256-GCM 加密、CAS 修订号仲裁、`updated_at` 较新者胜出。
//!
//! 两种传输形态：
//!   - `server`：自建 REST 服务端，逐对象 CAS PUT（`If-Match` 修订号）。
//!   - `webdav` / `s3`：整库打成一个加密信封文件（见 `vault_remote`），
//!     一轮同步一次读 + 一次条件写，冲突仍在本地按对象裁决。

use aes_gcm::aead::{Aead, AeadCore, Key, KeyInit, OsRng};
use aes_gcm::{Aes256Gcm, Nonce};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tauri::State;

use crate::telemetry_store::{CloudSyncConflict, TelemetryStore, UsageDayAggregate};
use crate::vault_remote::{RemoteConfig, RemoteKind};

const SETTING_URL: &str = "cloud_vault.url";
const SETTING_PAT: &str = "cloud_vault.pat";
/// 同步目标：`server` / `webdav` / `s3`。缺省按 `server` 处理，老配置无需改动。
const SETTING_KIND: &str = "cloud_vault.kind";
/// WebDAV 用户名 / S3 Access Key ID。
const SETTING_USER: &str = "cloud_vault.user";
const SETTING_ENDPOINT: &str = "cloud_vault.endpoint";
const SETTING_REGION: &str = "cloud_vault.region";
const SETTING_PATH_STYLE: &str = "cloud_vault.path_style";
/// 已保存的凭据属于哪种目标。凭据只在目标一致时沿用：否则换到 WebDAV 时会拿
/// 自建服务端的 PAT 去当坚果云密码，既连不上，又把旧凭据发给了另一个服务器。
const SETTING_CRED_KIND: &str = "cloud_vault.cred_kind";
const SETTING_ENABLED: &str = "cloud_vault.enabled";
const SETTING_DEVICE_ID: &str = "cloud_vault.device_id";
const SETTING_LAST_SYNC: &str = "cloud_vault.last_sync";
const SETTING_PASSWORD: &str = "cloud_vault.password";
const SETTING_AUTO_INTERVAL_MIN: &str = "cloud_vault.auto_interval_min";
const SETTING_LAST_AUTO_SYNC: &str = "cloud_vault.last_auto_sync";
/// A single server-wide cursor.  It must not be derived from per-object CAS
/// revisions: a local write can have a later revision than an unseen remote
/// object, which would otherwise make that remote object invisible forever.
const SETTING_PULL_CURSOR: &str = "cloud_vault.pull_cursor";

const SYNC_LAYERS: [&str; 2] = ["l3", "l2"];

// ---------- 加密 ----------

fn vault_key(password: &str) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(password.as_bytes());
    let out = hasher.finalize();
    let mut key = [0u8; 32];
    key.copy_from_slice(&out);
    key
}

fn sha256_hex(data: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(data);
    hasher
        .finalize()
        .iter()
        .map(|b| format!("{:02x}", b))
        .collect()
}

fn vault_encrypt(key: &[u8; 32], plain: &[u8]) -> Result<Vec<u8>, String> {
    let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(key));
    let nonce = Aes256Gcm::generate_nonce(&mut OsRng);
    let sealed = cipher
        .encrypt(&nonce, plain)
        .map_err(|_| "云端加密失败".to_string())?;
    let mut out = nonce.to_vec();
    out.extend_from_slice(&sealed);
    Ok(out)
}

fn vault_decrypt(key: &[u8; 32], blob: &[u8]) -> Result<Vec<u8>, String> {
    if blob.len() <= 12 {
        return Err("云端密文损坏".into());
    }
    let (nonce, ciphertext) = blob.split_at(12);
    let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(key));
    cipher
        .decrypt(Nonce::from_slice(nonce), ciphertext)
        .map_err(|_| "云端解密失败（同步密码是否正确？）".to_string())
}

// ---------- base64（payload 传输编码，避免新增 crate） ----------

const B64_TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

fn b64_encode(data: &[u8]) -> String {
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    let mut buf: u32 = 0;
    let mut bits = 0u32;
    for byte in data {
        buf = (buf << 8) | *byte as u32;
        bits += 8;
        while bits >= 6 {
            bits -= 6;
            out.push(B64_TABLE[((buf >> bits) & 0x3f) as usize] as char);
        }
    }
    if bits > 0 {
        out.push(B64_TABLE[((buf << (6 - bits)) & 0x3f) as usize] as char);
    }
    out
}

fn b64_decode(s: &str) -> Option<Vec<u8>> {
    let trimmed: Vec<u8> = s
        .bytes()
        .filter(|b| !b.is_ascii_whitespace() && *b != b'=')
        .collect();
    if trimmed.len() % 4 == 1 {
        return None;
    }
    let mut out = Vec::with_capacity(trimmed.len() * 3 / 4);
    let mut buf: u32 = 0;
    let mut bits = 0u32;
    for byte in trimmed {
        let v = B64_TABLE.iter().position(|c| *c == byte)? as u32;
        buf = (buf << 6) | v;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((buf >> bits) as u8);
        }
    }
    Some(out)
}

// ---------- 类型 ----------

#[derive(Serialize)]
pub struct CloudSyncReport {
    pub pushed: u32,
    pub pulled: u32,
    pub skipped: u32,
    pub conflicts: u32,
    pub usage_imported: u32,
    pub errors: Vec<String>,
}

#[derive(Serialize)]
pub struct CloudVaultSettingsView {
    /// `server` / `webdav` / `s3`。
    pub kind: String,
    pub url: String,
    pub user: String,
    /// 自建服务端 PAT / WebDAV 密码 / S3 Secret 是否已保存（永不回传明文）。
    pub pat_set: bool,
    pub password_set: bool,
    pub endpoint: String,
    pub region: String,
    pub path_style: bool,
    pub enabled: bool,
    pub auto_interval_min: i64,
}

#[derive(Serialize)]
pub struct CloudVaultStatus {
    pub configured: bool,
    pub enabled: bool,
    pub dirty: u32,
    pub conflicts: u32,
    pub last_sync_at: Option<String>,
    pub last_report: Option<String>,
    pub auto_interval_min: i64,
}

#[derive(Serialize, Deserialize, Clone)]
struct RemoteObject {
    key: String,
    revision: i64,
    content_hash: String,
    payload: String,
    updated_at: String,
    #[allow(dead_code)]
    device_id: String,
}

#[derive(Serialize, Deserialize, Clone)]
struct RemoteTombstone {
    key: String,
    #[serde(default)]
    revision: i64,
    deleted_at: String,
}

/// WebDAV / S3 上的整库信封：所有对象 + 墓碑 + 信封级修订号。
/// 信封本身在上传前整体加密，服务端只持有密文。
#[derive(Serialize, Deserialize, Default, Clone)]
struct VaultEnvelope {
    format: u32,
    revision: i64,
    #[serde(default)]
    objects: Vec<RemoteObject>,
    #[serde(default)]
    tombstones: Vec<RemoteTombstone>,
}

const ENVELOPE_FORMAT: u32 = 1;
/// 信封文件头：用于把「不是本应用的文件 / 口令错误」和「文件损坏」区分开。
const ENVELOPE_MAGIC: &[u8; 8] = b"AMVAULT1";

#[derive(Deserialize)]
struct RemoteList {
    objects: Vec<RemoteObject>,
    #[serde(default)]
    tombstones: Vec<RemoteTombstone>,
    #[allow(dead_code)]
    revision: i64,
}

/// 自定义 L1 的云端载荷：内容 + 类型 + 时间戳（冲突裁决用）。
#[derive(Serialize, Deserialize)]
struct L1Payload {
    content: String,
    memory_type: String,
    updated_at: String,
}

#[derive(Serialize)]
struct PutBody<'a> {
    content_hash: &'a str,
    payload: String,
    updated_at: String,
    device_id: String,
}

// ---------- 配置 ----------

/// 一次同步所需的全部配置：远端目标与凭据、同步密码、开关。
struct VaultConfig {
    remote: RemoteConfig,
    enabled: bool,
    password: String,
}

fn vault_config(store: &TelemetryStore) -> Result<VaultConfig, String> {
    let decrypt = |key: &str| -> String {
        store
            .app_setting_get::<String>(key)
            .map(|value| crate::llm::decrypt_api_key(&value))
            .unwrap_or_default()
    };
    let kind = RemoteKind::parse(
        &store
            .app_setting_get::<String>(SETTING_KIND)
            .unwrap_or_default(),
    );
    // 老配置没有 cred_kind，那种情况下只可能是自建服务端。
    let saved_for = store
        .app_setting_get::<String>(SETTING_CRED_KIND)
        .unwrap_or_default();
    let saved_for = if saved_for.is_empty() {
        RemoteKind::Server
    } else {
        RemoteKind::parse(&saved_for)
    };
    let credential = if saved_for == kind {
        decrypt(SETTING_PAT)
    } else {
        String::new()
    };
    Ok(VaultConfig {
        remote: RemoteConfig {
            kind,
            url: store.app_setting_get::<String>(SETTING_URL).unwrap_or_default(),
            user: decrypt(SETTING_USER),
            credential,
            endpoint: store
                .app_setting_get::<String>(SETTING_ENDPOINT)
                .unwrap_or_default(),
            region: store
                .app_setting_get::<String>(SETTING_REGION)
                .unwrap_or_default(),
            path_style: store
                .app_setting_get::<bool>(SETTING_PATH_STYLE)
                .unwrap_or(false),
        },
        enabled: store.app_setting_get::<bool>(SETTING_ENABLED).unwrap_or(false),
        password: decrypt(SETTING_PASSWORD),
    })
}

fn device_id(store: &TelemetryStore) -> String {
    if let Some(id) = store.app_setting_get::<String>(SETTING_DEVICE_ID) {
        if !id.is_empty() {
            return id;
        }
    }
    let machine = std::env::var("COMPUTERNAME")
        .or_else(|_| std::env::var("HOSTNAME"))
        .unwrap_or_else(|_| "device".to_string());
    let mut raw = [0u8; 4];
    use aes_gcm::aead::rand_core::RngCore;
    OsRng.fill_bytes(&mut raw);
    let suffix: String = raw.iter().map(|b| format!("{:02x}", b)).collect();
    let id = format!("{machine}-{suffix}");
    let _ = store.app_setting_set(SETTING_DEVICE_ID, &id);
    id
}

fn http_client() -> reqwest::blocking::Client {
    reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .build()
        .expect("failed to build http client")
}

// ---------- 同步引擎 ----------

fn parse_ts(ts: &str) -> chrono::DateTime<chrono::Utc> {
    chrono::DateTime::parse_from_rfc3339(ts)
        .map(|t| t.with_timezone(&chrono::Utc))
        .unwrap_or_else(|_| chrono::DateTime::UNIX_EPOCH)
}

fn push_layer(
    store: &TelemetryStore,
    client: &reqwest::blocking::Client,
    url: &str,
    pat: &str,
    device: &str,
    key: &str,
    layer: &str,
    vault_key_bytes: &[u8; 32],
    report: &mut CloudSyncReport,
) {
    let Some(doc) = store.active_memory_layer_document(layer).ok().flatten() else {
        return;
    };
    let meta = store.cloud_sync_meta_row(key).ok().flatten();
    let base_revision = meta.as_ref().map(|(rev, _, _, _)| *rev).unwrap_or(0);
    let local_hash = sha256_hex(doc.content.as_bytes());
    if let Some((_, Some(hash), state, _)) = meta.as_ref() {
        if state == "conflict" {
            report.skipped += 1;
            return;
        }
        if *hash == local_hash && state == "clean" {
            report.skipped += 1;
            return;
        }
    }
    let payload = match vault_encrypt(vault_key_bytes, doc.content.as_bytes()) {
        Ok(p) => p,
        Err(e) => {
            report.errors.push(format!("{key}: {e}"));
            return;
        }
    };
    let body = PutBody {
        content_hash: &local_hash,
        payload: b64_encode(&payload),
        updated_at: doc
            .published_at
            .clone()
            .unwrap_or_else(|| chrono::Utc::now().to_rfc3339()),
        device_id: device.to_string(),
    };
    let resp = client
        .put(format!("{url}/objects/{key}"))
        .bearer_auth(pat)
        .header("x-device-id", device)
        .header("If-Match", base_revision.to_string())
        .json(&body)
        .send();
    let resp = match resp {
        Ok(r) => r,
        Err(e) => {
            report.errors.push(format!("{key}: 推送失败 {e}"));
            return;
        }
    };
    match resp.status().as_u16() {
        200..=299 => {
            if let Ok(json) = resp.json::<serde_json::Value>() {
                let rev = json["revision"].as_i64().unwrap_or(base_revision);
                let _ = store.cloud_sync_meta_set(key, rev, Some(&local_hash), "clean");
                let _ =
                    store.cloud_sync_snapshot_set(key, rev, Some(&local_hash), Some(&doc.content));
            }
            report.pushed += 1;
        }
        409 => match fetch_object(client, url, pat, key) {
            Some(remote) => {
                record_layer_conflict(store, layer, vault_key_bytes, remote, report);
            }
            None => report.errors.push(format!("{key}: 冲突但无法读取远端版本")),
        },
        status => report.errors.push(format!("{key}: 推送失败 HTTP {status}")),
    }
}

fn fetch_object(
    client: &reqwest::blocking::Client,
    url: &str,
    pat: &str,
    key: &str,
) -> Option<RemoteObject> {
    client
        .get(format!("{url}/objects/{key}"))
        .bearer_auth(pat)
        .send()
        .ok()?
        .error_for_status()
        .ok()?
        .json::<RemoteObject>()
        .ok()
}

fn decrypt_remote_content(
    vault_key_bytes: &[u8; 32],
    obj: &RemoteObject,
    report: &mut CloudSyncReport,
) -> Option<String> {
    let blob = b64_decode(&obj.payload).or_else(|| {
        report
            .errors
            .push(format!("{}: 远端 payload 编码损坏", obj.key));
        None
    })?;
    let plain = vault_decrypt(vault_key_bytes, &blob)
        .map_err(|e| {
            report.errors.push(format!("{}: {e}", obj.key));
        })
        .ok()?;
    if sha256_hex(&plain) != obj.content_hash {
        report
            .errors
            .push(format!("{}: 远端内容哈希校验失败，已跳过", obj.key));
        return None;
    }
    Some(String::from_utf8_lossy(&plain).to_string())
}

fn record_layer_conflict(
    store: &TelemetryStore,
    layer: &str,
    vault_key_bytes: &[u8; 32],
    obj: RemoteObject,
    report: &mut CloudSyncReport,
) -> bool {
    let key = obj.key.clone();
    let Some(remote_content) = decrypt_remote_content(vault_key_bytes, &obj, report) else {
        return false;
    };
    let local = store.active_memory_layer_document(layer).ok().flatten();
    let conflict = CloudSyncConflict {
        object_key: key.clone(),
        object_kind: layer.to_string(),
        base_content: store.cloud_sync_snapshot_content(&key).ok().flatten(),
        local_content: local.as_ref().map(|item| item.content.clone()),
        remote_content: Some(remote_content),
        local_memory_type: None,
        remote_memory_type: None,
        remote_revision: obj.revision,
        remote_content_hash: Some(obj.content_hash.clone()),
        local_updated_at: local.and_then(|item| item.published_at),
        remote_updated_at: Some(obj.updated_at),
        created_at: chrono::Utc::now().to_rfc3339(),
    };
    if let Err(error) = store.cloud_sync_conflict_upsert(&conflict) {
        report.errors.push(format!("{key}: 保存冲突失败 {error}"));
        return false;
    }
    let _ = store.cloud_sync_meta_set(&key, obj.revision, Some(&obj.content_hash), "conflict");
    report.conflicts += 1;
    true
}

fn record_l1_conflict(
    store: &TelemetryStore,
    local: &crate::telemetry_store::LocalMemorySnapshot,
    vault_key_bytes: &[u8; 32],
    obj: RemoteObject,
    report: &mut CloudSyncReport,
) -> bool {
    let key = obj.key.clone();
    let Some(remote_plain) = decrypt_remote_content(vault_key_bytes, &obj, report) else {
        return false;
    };
    let remote: L1Payload = match serde_json::from_str(&remote_plain) {
        Ok(payload) => payload,
        Err(error) => {
            report
                .errors
                .push(format!("{key}: 远端记忆载荷无效 {error}"));
            return false;
        }
    };
    let local_plain = serde_json::to_string(&L1Payload {
        content: local.memory.clone(),
        memory_type: local.memory_type.clone(),
        updated_at: local.updated_at.clone(),
    })
    .unwrap_or_default();
    let conflict = CloudSyncConflict {
        object_key: key.clone(),
        object_kind: "l1".into(),
        base_content: store.cloud_sync_snapshot_content(&key).ok().flatten(),
        local_content: Some(local_plain),
        remote_content: Some(remote_plain),
        local_memory_type: Some(local.memory_type.clone()),
        remote_memory_type: Some(remote.memory_type),
        remote_revision: obj.revision,
        remote_content_hash: Some(obj.content_hash.clone()),
        local_updated_at: Some(local.updated_at.clone()),
        remote_updated_at: Some(remote.updated_at),
        created_at: chrono::Utc::now().to_rfc3339(),
    };
    if let Err(error) = store.cloud_sync_conflict_upsert(&conflict) {
        report.errors.push(format!("{key}: 保存冲突失败 {error}"));
        return false;
    }
    let _ = store.cloud_sync_meta_set(&key, obj.revision, Some(&obj.content_hash), "conflict");
    report.conflicts += 1;
    true
}

fn record_l1_delete_conflict(
    store: &TelemetryStore,
    key: &str,
    _base_revision: i64,
    vault_key_bytes: &[u8; 32],
    obj: RemoteObject,
    report: &mut CloudSyncReport,
) -> bool {
    let Some(remote_plain) = decrypt_remote_content(vault_key_bytes, &obj, report) else {
        return false;
    };
    let remote: L1Payload = match serde_json::from_str(&remote_plain) {
        Ok(payload) => payload,
        Err(error) => {
            report
                .errors
                .push(format!("{key}: 远端记忆载荷无效 {error}"));
            return false;
        }
    };
    let conflict = CloudSyncConflict {
        object_key: key.to_string(),
        object_kind: "l1".into(),
        base_content: store.cloud_sync_snapshot_content(key).ok().flatten(),
        local_content: None,
        remote_content: Some(remote_plain),
        local_memory_type: None,
        remote_memory_type: Some(remote.memory_type),
        remote_revision: obj.revision,
        remote_content_hash: Some(obj.content_hash.clone()),
        local_updated_at: None,
        remote_updated_at: Some(remote.updated_at),
        created_at: chrono::Utc::now().to_rfc3339(),
    };
    if let Err(error) = store.cloud_sync_conflict_upsert(&conflict) {
        report
            .errors
            .push(format!("{key}: 保存删除冲突失败 {error}"));
        return false;
    }
    let _ = store.cloud_sync_meta_set(key, obj.revision, Some(&obj.content_hash), "conflict");
    report.conflicts += 1;
    true
}

/// 本机删掉了这条自定义记忆，同时其他设备把它改了。
///
/// 两边都是「有意的操作」：本地的删除不能拿来抹掉别人的编辑，别人的编辑也不能
/// 悄悄把删掉的记忆拉回来。交给用户选——保留删除、还是接受远端这份修改。
fn record_remote_edit_vs_local_delete(
    store: &TelemetryStore,
    vault_key_bytes: &[u8; 32],
    obj: RemoteObject,
    report: &mut CloudSyncReport,
) -> bool {
    let key = obj.key.clone();
    let remote_content = decrypt_remote_content(vault_key_bytes, &obj, report);
    let conflict = CloudSyncConflict {
        object_key: key.clone(),
        object_kind: "l1".into(),
        base_content: store.cloud_sync_snapshot_content(&key).ok().flatten(),
        local_content: None,
        remote_content,
        local_memory_type: None,
        remote_memory_type: None,
        remote_revision: obj.revision,
        remote_content_hash: Some(obj.content_hash.clone()),
        local_updated_at: None,
        remote_updated_at: Some(obj.updated_at),
        created_at: chrono::Utc::now().to_rfc3339(),
    };
    if let Err(error) = store.cloud_sync_conflict_upsert(&conflict) {
        report
            .errors
            .push(format!("{key}: 保存删除冲突失败 {error}"));
        return false;
    }
    let _ = store.cloud_sync_meta_set(&key, obj.revision, Some(&obj.content_hash), "conflict");
    report.conflicts += 1;
    true
}

fn apply_remote(
    store: &TelemetryStore,
    layer: &str,
    vault_key_bytes: &[u8; 32],
    obj: RemoteObject,
    report: &mut CloudSyncReport,
) -> bool {
    let key = obj.key.as_str();
    let blob = match b64_decode(&obj.payload) {
        Some(b) => b,
        None => {
            report.errors.push(format!("{key}: 远端 payload 编码损坏"));
            return false;
        }
    };
    let plain = match vault_decrypt(vault_key_bytes, &blob) {
        Ok(p) => p,
        Err(e) => {
            report.errors.push(format!("{key}: {e}"));
            return false;
        }
    };
    if sha256_hex(&plain) != obj.content_hash {
        report
            .errors
            .push(format!("{key}: 远端内容哈希校验失败，已跳过"));
        return false;
    }
    let content = String::from_utf8_lossy(&plain).to_string();
    if let Err(e) = store.upsert_published_layer_content(layer, &content) {
        report.errors.push(format!("{key}: 落地失败 {e}"));
        return false;
    }
    let _ = store.cloud_sync_meta_set(key, obj.revision, Some(&obj.content_hash), "clean");
    let _ =
        store.cloud_sync_snapshot_set(key, obj.revision, Some(&obj.content_hash), Some(&content));
    report.pulled += 1;
    true
}

/// 自定义 L1 推送：快照 diff——比对当前用户自定义条目与已知水位，
/// 内容变化（或新增）的对象走 CAS PUT；本地已删除但云端还有的对象
/// 推 DELETE（服务端落墓碑），使删除跨设备传播。
fn push_l1(
    store: &TelemetryStore,
    client: &reqwest::blocking::Client,
    url: &str,
    pat: &str,
    device: &str,
    vault_key_bytes: &[u8; 32],
    report: &mut CloudSyncReport,
) {
    let items = match store.user_defined_l1_memories() {
        Ok(items) => items,
        Err(e) => {
            report.errors.push(format!("l1: 读取自定义记忆失败 {e}"));
            return;
        }
    };
    let metas: Vec<(String, i64, Option<String>, String, Option<String>)> = store
        .cloud_sync_meta_rows()
        .unwrap_or_default()
        .into_iter()
        .filter(|(key, _, _, _, _)| key.starts_with("l1-user:"))
        .collect();
    for item in &items {
        let key = format!("l1-user:{}", item.id);
        let payload = L1Payload {
            content: item.memory.clone(),
            memory_type: item.memory_type.clone(),
            updated_at: item.updated_at.clone(),
        };
        let plain = serde_json::to_string(&payload).unwrap_or_default();
        let local_hash = sha256_hex(plain.as_bytes());
        let meta = metas.iter().find(|(k, _, _, _, _)| *k == key);
        if meta.is_some_and(|(_, _, _, state, _)| state == "conflict") {
            report.skipped += 1;
            continue;
        }
        if let Some((_, _, Some(hash), state, _)) = meta {
            if *hash == local_hash && state == "clean" {
                continue;
            }
        }
        let base_revision = meta.map(|(_, rev, _, _, _)| *rev).unwrap_or(0);
        let blob = match vault_encrypt(vault_key_bytes, plain.as_bytes()) {
            Ok(b) => b,
            Err(e) => {
                report.errors.push(format!("{key}: {e}"));
                continue;
            }
        };
        let body = PutBody {
            content_hash: &local_hash,
            payload: b64_encode(&blob),
            updated_at: payload.updated_at.clone(),
            device_id: device.to_string(),
        };
        let resp = client
            .put(format!("{url}/objects/{key}"))
            .bearer_auth(pat)
            .header("x-device-id", device)
            .header("If-Match", base_revision.to_string())
            .json(&body)
            .send();
        match resp {
            Ok(r) if r.status().is_success() => {
                let rev = r
                    .json::<serde_json::Value>()
                    .ok()
                    .and_then(|v| v["revision"].as_i64())
                    .unwrap_or(base_revision);
                let _ = store.cloud_sync_meta_set(&key, rev, Some(&local_hash), "clean");
                report.pushed += 1;
            }
            Ok(r) if r.status().as_u16() == 409 => {
                if let Some(remote) = fetch_object(client, url, pat, &key) {
                    record_l1_conflict(store, item, vault_key_bytes, remote, report);
                } else {
                    report.errors.push(format!("{key}: 冲突但无法读取远端版本"));
                }
            }
            Ok(r) => report
                .errors
                .push(format!("{key}: 推送失败 HTTP {}", r.status())),
            Err(e) => report.errors.push(format!("{key}: 推送失败 {e}")),
        }
    }
    // 本地删除传播：水位里有、本地已无对应条目 → 推墓碑。
    let live_ids: Vec<String> = items
        .iter()
        .map(|item| format!("l1-user:{}", item.id))
        .collect();
    for (key, revision, _, state, _) in metas {
        if !live_ids.contains(&key) {
            if state == "conflict" {
                report.skipped += 1;
                continue;
            }
            match client
                .delete(format!("{url}/objects/{key}"))
                .bearer_auth(pat)
                .header("x-device-id", device)
                .header("If-Match", revision.to_string())
                .send()
            {
                Ok(r) if r.status().is_success() || r.status().as_u16() == 404 => {
                    let _ = store.cloud_sync_meta_delete(&key);
                    report.pushed += 1;
                }
                Ok(r) if r.status().as_u16() == 409 => {
                    if let Some(remote) = fetch_object(client, url, pat, &key) {
                        record_l1_delete_conflict(
                            store,
                            &key,
                            revision,
                            vault_key_bytes,
                            remote,
                            report,
                        );
                    } else {
                        report
                            .errors
                            .push(format!("{key}: 删除冲突但无法读取远端版本"));
                    }
                }
                Ok(r) => report
                    .errors
                    .push(format!("{key}: 删除推送失败 HTTP {}", r.status())),
                Err(e) => report.errors.push(format!("{key}: 删除推送失败 {e}")),
            }
        }
    }
}

/// Token 用量推送：每台设备只写一个小对象 `usage-agg:{device}`，
/// 内容是「天 × 来源」的总量统计（不含逐条明细）。单写者，无并发冲突；
/// 内容变化由 hash 比对发现后整体重推。
fn push_usage(
    store: &TelemetryStore,
    client: &reqwest::blocking::Client,
    url: &str,
    pat: &str,
    device: &str,
    vault_key_bytes: &[u8; 32],
    report: &mut CloudSyncReport,
) {
    let key = format!("usage-agg:{}", device.replace(':', "-"));
    let rows = match store.usage_day_aggregates() {
        Ok(rows) => rows,
        Err(e) => {
            report.errors.push(format!("{key}: 统计读取失败 {e}"));
            return;
        }
    };
    let plain = match serde_json::to_vec(&rows) {
        Ok(p) => p,
        Err(e) => {
            report.errors.push(format!("{key}: 序列化失败 {e}"));
            return;
        }
    };
    let local_hash = sha256_hex(&plain);
    if let Some((_, Some(hash), state, _)) = store.cloud_sync_meta_row(&key).ok().flatten() {
        if hash == local_hash && state == "clean" {
            return;
        }
    }
    let base_revision = store
        .cloud_sync_meta_row(&key)
        .ok()
        .flatten()
        .map(|(rev, _, _, _)| rev)
        .unwrap_or(0);
    let blob = match vault_encrypt(vault_key_bytes, &plain) {
        Ok(b) => b,
        Err(e) => {
            report.errors.push(format!("{key}: {e}"));
            return;
        }
    };
    if blob.len() > 250 * 1024 {
        report
            .errors
            .push(format!("{key}: 统计超过 250KB 上限，已跳过"));
        return;
    }
    let body = PutBody {
        content_hash: &local_hash,
        payload: b64_encode(&blob),
        updated_at: chrono::Utc::now().to_rfc3339(),
        device_id: device.to_string(),
    };
    let resp = client
        .put(format!("{url}/objects/{key}"))
        .bearer_auth(pat)
        .header("x-device-id", device)
        .header("If-Match", base_revision.to_string())
        .json(&body)
        .send();
    match resp {
        Ok(r) if r.status().is_success() => {
            let rev = r
                .json::<serde_json::Value>()
                .ok()
                .and_then(|v| v["revision"].as_i64())
                .unwrap_or(base_revision);
            let _ = store.cloud_sync_meta_set(&key, rev, Some(&local_hash), "clean");
            report.pushed += 1;
        }
        // 本设备单写者，409 只可能是水位错位；按远端修订强制重推一次。
        Ok(r) if r.status().as_u16() == 409 => {
            let current_rev = r
                .json::<serde_json::Value>()
                .ok()
                .and_then(|v| v["current_revision"].as_i64())
                .unwrap_or(-1);
            if current_rev < 0 {
                report
                    .errors
                    .push(format!("{key}: 冲突且无法获取远端修订号"));
                return;
            }
            let forced = client
                .put(format!("{url}/objects/{key}"))
                .bearer_auth(pat)
                .header("x-device-id", device)
                .header("If-Match", current_rev.to_string())
                .json(&body)
                .send();
            match forced {
                Ok(r) if r.status().is_success() => {
                    let rev = r
                        .json::<serde_json::Value>()
                        .ok()
                        .and_then(|v| v["revision"].as_i64())
                        .unwrap_or(current_rev);
                    let _ = store.cloud_sync_meta_set(&key, rev, Some(&local_hash), "clean");
                    report.pushed += 1;
                }
                Ok(r) => report
                    .errors
                    .push(format!("{key}: 用量推送失败 HTTP {}", r.status())),
                Err(e) => report.errors.push(format!("{key}: 用量推送失败 {e}")),
            }
        }
        Ok(r) => report
            .errors
            .push(format!("{key}: 用量推送失败 HTTP {}", r.status())),
        Err(e) => report.errors.push(format!("{key}: 用量推送失败 {e}")),
    }
}

fn pull(
    store: &TelemetryStore,
    client: &reqwest::blocking::Client,
    url: &str,
    pat: &str,
    vault_key_bytes: &[u8; 32],
    report: &mut CloudSyncReport,
    force_full: bool,
) {
    let since = if force_full {
        0
    } else {
        store
            .app_setting_get::<i64>(SETTING_PULL_CURSOR)
            .unwrap_or(0)
    };
    let resp = match client
        .get(format!("{url}/objects"))
        .query(&[("since_rev", since.to_string())])
        .bearer_auth(pat)
        .send()
    {
        Ok(r) => r,
        Err(e) => {
            report.errors.push(format!("拉取失败 {e}"));
            return;
        }
    };
    if !resp.status().is_success() {
        report
            .errors
            .push(format!("拉取失败 HTTP {}", resp.status()));
        return;
    }
    let list = match resp.json::<RemoteList>() {
        Ok(l) => l,
        Err(e) => {
            report.errors.push(format!("拉取响应解析失败 {e}"));
            return;
        }
    };
    let RemoteList {
        objects,
        tombstones,
        revision,
    } = list;
    let device = device_id(store);
    let objects: Vec<RemoteObject> = objects
        .into_iter()
        .filter(|obj| {
            if obj.key.starts_with("usage:") {
                // 旧版逐条账本分片：不再同步，直接从服务端清除。
                let _ = client
                    .delete(format!("{url}/objects/{}", obj.key))
                    .bearer_auth(pat)
                    .header("x-device-id", &device)
                    .send();
                let _ = store.cloud_sync_meta_delete(&obj.key);
                false
            } else {
                true
            }
        })
        .collect();
    // 出错时绝不推进游标：下一轮必须还能重试这些对象。
    if apply_remote_objects(store, &objects, &tombstones, vault_key_bytes, report) {
        let _ = store.app_setting_set(SETTING_PULL_CURSOR, &revision);
    }
}

/// 把一批远端对象落地到本地，与传输方式无关：自建服务端、WebDAV、S3 共用同一套
/// 解密 → 验哈希 → 与本地水位比对 → 落地或记冲突的流程。
///
/// 返回本轮是否没有新增错误。返回 false 时调用方不应推进游标 / 回写信封，
/// 否则一条解不开的密文或一个错口令会永久跳过。
fn apply_remote_objects(
    store: &TelemetryStore,
    objects: &[RemoteObject],
    tombstones: &[RemoteTombstone],
    vault_key_bytes: &[u8; 32],
    report: &mut CloudSyncReport,
) -> bool {
    let errors_before = report.errors.len();
    for obj in objects.iter().cloned() {
        let meta = store.cloud_sync_meta_row(&obj.key).ok().flatten();
        // 信封模式每轮都拿到全量对象，所以必须先问一句「远端其实有没有变」：
        //   · 远端 == 上次同步的水位 → 远端没有新东西。本地若有改动，那是本地的新进度，
        //     留给推送阶段，绝不能当成「两边都改过」的冲突（否则本机改动永远推不上去，
        //     且每轮重复报同一个假冲突）。
        //   · 冲突未人工处理 → 保持冲突，等用户裁决，不能被新版本静默覆盖。
        // 逐对象模式里服务端只回增量，这一层短路同样成立、只是那时大多命中不到。
        if let Some((_, Some(hash), state, _)) = meta.as_ref() {
            if *hash == obj.content_hash || state == "conflict" {
                report.skipped += 1;
                continue;
            }
        }
        if let Some(id) = obj.key.strip_prefix("l1-user:") {
            // 本机同步过这条却已经删掉：绝不能把它落地回来，否则本机的删除永远传不出去
            // （apply 排在 build 之前，一旦落地，墓碑判定就再也看不见它）。
            // 远端在此期间被人改过则没法自动合并，记成冲突交给用户裁决。
            let local_gone = store.find_user_defined_l1(id).ok().flatten().is_none();
            if local_gone && meta.is_some() {
                // dirty 只可能来自冲突裁决：用户已经选了「保留本地（删除）」，
                // 这一票今天就生效，别每轮再记一次同样的冲突把它钉死在原地。
                let conflicted = meta.as_ref().is_some_and(|(_, _, state, _)| {
                    state == "conflict" || state == "dirty"
                });
                if !conflicted {
                    record_remote_edit_vs_local_delete(store, vault_key_bytes, obj.clone(), report);
                }
                report.skipped += 1;
                continue;
            }
            apply_remote_l1(store, id.to_string(), vault_key_bytes, obj, report);
            continue;
        }
        if let Some(rest) = obj.key.strip_prefix("usage-agg:") {
            apply_remote_usage(store, rest.to_string(), vault_key_bytes, obj, report);
            continue;
        }
        let Some(layer) = obj.key.strip_suffix(":published").map(str::to_string) else {
            continue;
        };
        if !SYNC_LAYERS.contains(&layer.as_str()) {
            continue;
        }
        let dirty = meta
            .as_ref()
            .map(|(_, _, state, _)| state == "dirty")
            .unwrap_or(false);
        if !dirty {
            apply_remote(store, &layer, vault_key_bytes, obj, report);
        } else {
            record_layer_conflict(store, &layer, vault_key_bytes, obj, report);
        }
    }
    // 墓碑：其他设备删除的自定义 L1 在本地同步删除（仅 user_defined 条目）。
    for tomb in tombstones.iter().cloned() {
        let Some(id) = tomb.key.strip_prefix("l1-user:") else {
            continue;
        };
        let local = store.find_user_defined_l1(id).ok().flatten();
        let meta = store.cloud_sync_meta_row(&tomb.key).ok().flatten();
        let locally_changed = local.as_ref().is_some_and(|item| {
            let plain = serde_json::to_string(&L1Payload {
                content: item.memory.clone(),
                memory_type: item.memory_type.clone(),
                updated_at: item.updated_at.clone(),
            })
            .unwrap_or_default();
            meta.as_ref().map_or(true, |(_, hash, state, _)| {
                state == "dirty" || hash.as_deref() != Some(&sha256_hex(plain.as_bytes()))
            })
        });
        if locally_changed {
            let conflict = CloudSyncConflict {
                object_key: tomb.key.clone(),
                object_kind: "l1".into(),
                base_content: store.cloud_sync_snapshot_content(&tomb.key).ok().flatten(),
                local_content: local.and_then(|item| {
                    serde_json::to_string(&L1Payload {
                        content: item.memory,
                        memory_type: item.memory_type,
                        updated_at: item.updated_at,
                    })
                    .ok()
                }),
                remote_content: None,
                local_memory_type: None,
                remote_memory_type: None,
                remote_revision: tomb.revision,
                remote_content_hash: None,
                local_updated_at: None,
                remote_updated_at: Some(tomb.deleted_at),
                created_at: chrono::Utc::now().to_rfc3339(),
            };
            if store.cloud_sync_conflict_upsert(&conflict).is_ok() {
                let _ = store.cloud_sync_meta_set(&tomb.key, tomb.revision, None, "conflict");
                report.conflicts += 1;
            }
            continue;
        }
        match store.delete_synced_l1_memory(id) {
            Ok(true) => report.pulled += 1,
            Ok(false) => {}
            Err(e) => report
                .errors
                .push(format!("{}: 删除落地失败 {e}", tomb.key)),
        }
        let _ = store.cloud_sync_meta_delete(&tomb.key);
    }
    // 冲突是本地持久记录，可以随游标一起确认；密文损坏或口令错误则不行。
    report.errors.len() == errors_before
}

/// 远端用量总量统计落地：解密验哈希 → 整设备覆盖 usage_remote_totals。
/// 本设备自己的对象跳过落地（推送阶段保证本地为源），只对齐水位。
fn apply_remote_usage(
    store: &TelemetryStore,
    device_suffix: String,
    vault_key_bytes: &[u8; 32],
    obj: RemoteObject,
    report: &mut CloudSyncReport,
) -> bool {
    let device = device_id(store).replace(':', "-");
    if device_suffix == device {
        let _ = store.cloud_sync_meta_set(&obj.key, obj.revision, Some(&obj.content_hash), "clean");
        report.skipped += 1;
        return true;
    }
    let blob = match b64_decode(&obj.payload) {
        Some(b) => b,
        None => {
            report
                .errors
                .push(format!("usage-agg: 远端 payload 编码损坏 ({})", obj.key));
            return false;
        }
    };
    let plain = match vault_decrypt(vault_key_bytes, &blob) {
        Ok(p) => p,
        Err(e) => {
            report.errors.push(format!("usage-agg: {e} ({})", obj.key));
            return false;
        }
    };
    if sha256_hex(&plain) != obj.content_hash {
        report
            .errors
            .push(format!("usage-agg: 远端哈希校验失败，已跳过 ({})", obj.key));
        return false;
    }
    let rows: Vec<UsageDayAggregate> = match serde_json::from_slice(&plain) {
        Ok(r) => r,
        Err(e) => {
            report
                .errors
                .push(format!("usage-agg: 载荷解析失败 {e} ({})", obj.key));
            return false;
        }
    };
    match store.replace_remote_usage_totals(&device_suffix, &rows) {
        Ok(imported) => {
            let _ =
                store.cloud_sync_meta_set(&obj.key, obj.revision, Some(&obj.content_hash), "clean");
            report.pulled += 1;
            report.usage_imported += imported as u32;
        }
        Err(e) => {
            report
                .errors
                .push(format!("usage-agg: 落地失败 {e} ({})", obj.key));
            return false;
        }
    }
    true
}

/// 远端自定义 L1 落地：解密验哈希 → 与本地比对 → upsert 或按 updated_at 裁决。
fn apply_remote_l1(
    store: &TelemetryStore,
    id: String,
    vault_key_bytes: &[u8; 32],
    obj: RemoteObject,
    report: &mut CloudSyncReport,
) -> bool {
    let blob = match b64_decode(&obj.payload) {
        Some(b) => b,
        None => {
            report
                .errors
                .push(format!("l1-user:{id}: 远端 payload 编码损坏"));
            return false;
        }
    };
    let plain = match vault_decrypt(vault_key_bytes, &blob) {
        Ok(p) => p,
        Err(e) => {
            report.errors.push(format!("l1-user:{id}: {e}"));
            return false;
        }
    };
    if sha256_hex(&plain) != obj.content_hash {
        report
            .errors
            .push(format!("l1-user:{id}: 远端内容哈希校验失败，已跳过"));
        return false;
    }
    let payload: L1Payload = match serde_json::from_slice(&plain) {
        Ok(p) => p,
        Err(e) => {
            report
                .errors
                .push(format!("l1-user:{id}: 载荷解析失败 {e}"));
            return false;
        }
    };
    let meta = store.cloud_sync_meta_row(&obj.key).ok().flatten();
    // conflict：等用户裁决；dirty：用户已经在冲突面板选了「保留本地」。两种情况
    // 本地都是权威，本 rv 不能拿远端内容覆盖它，也不能再记一条新冲突。
    if meta
        .as_ref()
        .is_some_and(|(_, _, state, _)| state == "conflict" || state == "dirty")
    {
        report.skipped += 1;
        return false;
    }
    // 本地已有同 hash → 只对齐水位。
    if let Ok(Some(item)) = store.find_user_defined_l1(&id) {
        let local_payload = L1Payload {
            content: item.memory.clone(),
            memory_type: item.memory_type.clone(),
            updated_at: item.updated_at.clone(),
        };
        let local_plain = serde_json::to_string(&local_payload).unwrap_or_default();
        if sha256_hex(local_plain.as_bytes()) == obj.content_hash {
            let _ =
                store.cloud_sync_meta_set(&obj.key, obj.revision, Some(&obj.content_hash), "clean");
            let _ = store.cloud_sync_snapshot_set(
                &obj.key,
                obj.revision,
                Some(&obj.content_hash),
                Some(&String::from_utf8_lossy(&plain)),
            );
            report.skipped += 1;
            return true;
        }
        // 内容不同且本地不是已对齐的旧版本，说明双方都改过同一记忆。
        // 保留双方，等待用户选择，绝不能拿设备时钟静默裁决。
        let local_changed = meta.as_ref().map_or(true, |(_, known_hash, state, _)| {
            state == "dirty" || known_hash.as_deref() != Some(&sha256_hex(local_plain.as_bytes()))
        });
        if local_changed {
            return record_l1_conflict(store, &item, vault_key_bytes, obj, report);
        }
        if meta
            .as_ref()
            .is_some_and(|(_, _, state, _)| state == "dirty")
        {
            return false;
        }
    }
    if let Err(e) = store.upsert_synced_l1_memory(
        &id,
        &payload.content,
        &payload.memory_type,
        &payload.updated_at,
    ) {
        report.errors.push(format!("l1-user:{id}: 落地失败 {e}"));
        return false;
    }
    let _ = store.cloud_sync_meta_set(&obj.key, obj.revision, Some(&obj.content_hash), "clean");
    let _ = store.cloud_sync_snapshot_set(
        &obj.key,
        obj.revision,
        Some(&obj.content_hash),
        Some(&String::from_utf8_lossy(&plain)),
    );
    report.pulled += 1;
    true
}

/// WebDAV / S3 与自建服务端的公共前置校验。
fn ready(config: &VaultConfig) -> Result<(), String> {
    if !config.enabled {
        return Err("云端记忆库未启用".into());
    }
    if config.password.is_empty() {
        return Err("未设置同步密码：请到「设置 → 云端记忆库」保存同步密码".into());
    }
    Ok(())
}

fn run_sync(store: &TelemetryStore) -> Result<CloudSyncReport, String> {
    let config = vault_config(store)?;
    ready(&config)?;
    if config.remote.kind != RemoteKind::Server {
        return run_sync_envelope(store, &config);
    }
    let url = config.remote.url.trim().trim_end_matches('/').to_string();
    let pat = config.remote.credential.clone();
    if url.is_empty() || pat.is_empty() {
        return Err("云端记忆库未配置服务端 URL 或 PAT".into());
    }
    let key = vault_key(&config.password);
    let client = http_client();
    let device = device_id(store);
    let mut report = CloudSyncReport {
        pushed: 0,
        pulled: 0,
        skipped: 0,
        conflicts: 0,
        usage_imported: 0,
        errors: Vec::new(),
    };
    // Pull first so a freshly installed device obtains the vault before it
    // attempts any local write.  A second pull captures remote changes that
    // arrived while this round was uploading.
    pull(store, &client, &url, &pat, &key, &mut report, false);
    for layer in SYNC_LAYERS {
        let key_name = format!("{layer}:published");
        push_layer(
            store,
            &client,
            &url,
            &pat,
            &device,
            &key_name,
            layer,
            &key,
            &mut report,
        );
    }
    push_l1(store, &client, &url, &pat, &device, &key, &mut report);
    push_usage(store, &client, &url, &pat, &device, &key, &mut report);
    pull(store, &client, &url, &pat, &key, &mut report, false);
    let summary = format!(
        "推送 {} · 拉取 {} · 用量 {} 条 · 跳过 {} · 冲突 {} · 错误 {}",
        report.pushed,
        report.pulled,
        report.usage_imported,
        report.skipped,
        report.conflicts,
        report.errors.len()
    );
    let _ = store.app_setting_set(SETTING_LAST_SYNC, &chrono::Utc::now().to_rfc3339());
    let _ = store.app_setting_set("cloud_vault.last_report", &summary);
    Ok(report)
}

fn run_pull_only(store: &TelemetryStore) -> Result<CloudSyncReport, String> {
    let config = vault_config(store)?;
    ready(&config)?;
    if config.remote.kind != RemoteKind::Server {
        return run_pull_envelope(store, &config);
    }
    let url = config.remote.url.trim().trim_end_matches('/').to_string();
    let pat = config.remote.credential.clone();
    if url.is_empty() || pat.is_empty() {
        return Err("云端记忆库未配置服务端 URL 或 PAT".into());
    }
    let mut report = CloudSyncReport {
        pushed: 0,
        pulled: 0,
        skipped: 0,
        conflicts: 0,
        usage_imported: 0,
        errors: Vec::new(),
    };
    // The explicit pull button is also a recovery operation for clients that
    // previously advanced the old per-object cursor too far.  The vault is
    // deliberately small, so fetching the complete remote object set is cheap
    // and guarantees that older L2/L3 objects cannot remain invisible.
    pull(
        store,
        &http_client(),
        url.trim().trim_end_matches('/'),
        &pat,
        &vault_key(&config.password),
        &mut report,
        true,
    );
    let summary = format!(
        "完整拉取：获取 {} · 用量 {} 条 · 跳过 {} · 冲突 {} · 错误 {}",
        report.pulled,
        report.usage_imported,
        report.skipped,
        report.conflicts,
        report.errors.len()
    );
    let _ = store.app_setting_set(SETTING_LAST_SYNC, &chrono::Utc::now().to_rfc3339());
    let _ = store.app_setting_set("cloud_vault.last_report", &summary);
    Ok(report)
}

// ---------- WebDAV / S3：整库信封 ----------

/// 信封文件 = 8 字节魔数 + AES-256-GCM 密文。服务端只持有这一团密文。
fn encode_envelope(vault_key_bytes: &[u8; 32], envelope: &VaultEnvelope) -> Result<Vec<u8>, String> {
    let plain =
        serde_json::to_vec(envelope).map_err(|e| format!("云端信封序列化失败：{e}"))?;
    let sealed = vault_encrypt(vault_key_bytes, &plain)?;
    let mut out = ENVELOPE_MAGIC.to_vec();
    out.extend_from_slice(&sealed);
    Ok(out)
}

fn decode_envelope(vault_key_bytes: &[u8; 32], bytes: &[u8]) -> Result<VaultEnvelope, String> {
    if bytes.len() <= ENVELOPE_MAGIC.len() || &bytes[..ENVELOPE_MAGIC.len()] != ENVELOPE_MAGIC {
        return Err("远端文件不是本应用的云端信封：它可能是别的文件，或已被其他程序覆盖".into());
    }
    let plain = vault_decrypt(vault_key_bytes, &bytes[ENVELOPE_MAGIC.len()..])?;
    let envelope: VaultEnvelope =
        serde_json::from_slice(&plain).map_err(|_| "云端信封内容无法解析".to_string())?;
    if envelope.format != ENVELOPE_FORMAT {
        return Err(format!(
            "云端信封是版本 {}，本机只认版本 {ENVELOPE_FORMAT}：请升级客户端后再同步",
            envelope.format
        ));
    }
    Ok(envelope)
}

/// 某个 key 是否该把本地内容推上去。
///
/// 三态判定（与自建服务端模式下 `pull` + `push_*` 的合起来等价）：
///   - 冲突未人工处理 → 保持远端，绝不用设备时钟静默裁决；
///   - 远端没有 → 创建；
///   - 远端与我上次同步的水位一致 → 本地更新，推；
///   - 远端比水位新、本地也改了 → 保持远端，冲突已在 apply 阶段记录。
fn push_decision(
    store: &TelemetryStore,
    key: &str,
    remote_hash: Option<&str>,
    local_hash: &str,
) -> bool {
    let meta = store.cloud_sync_meta_row(key).ok().flatten();
    if let Some((_, _, state, _)) = meta.as_ref() {
        if state == "conflict" {
            return false;
        }
    }
    // l1-user 的 dirty 只来自冲突裁决：本机的内容是用户拍板过的，直接推。
    // 层的 dirty 是「重新发布」，有自己的水位比对，不能一并放开。
    if key.starts_with("l1-user:")
        && meta.as_ref().is_some_and(|(_, _, state, _)| state == "dirty")
    {
        return true;
    }
    let known = meta.as_ref().and_then(|(_, hash, _, _)| hash.clone());
    match remote_hash {
        None => true,
        Some(remote) if remote == local_hash => false,
        Some(remote) if known.as_deref() == Some(remote) => true,
        _ => false,
    }
}

/// 信封写成功之后才允许落地的本地副作用。
///
/// 水位、冲突快照、墓碑后的 meta 删除都必须等远端确认写入，否则一次超时或 429
/// 会让本机误以为「已同步」，下一轮被云端的旧版本静默回滚，而且报告里一个错误都没有。
enum PendingWrite {
    Meta {
        key: String,
        revision: i64,
        hash: Option<String>,
        state: &'static str,
    },
    Snapshot {
        key: String,
        revision: i64,
        hash: Option<String>,
        content: Option<String>,
    },
    DeleteMeta {
        key: String,
    },
}

fn apply_pending_writes(store: &TelemetryStore, pending: Vec<PendingWrite>) {
    for item in pending {
        match item {
            PendingWrite::Meta {
                key,
                revision,
                hash,
                state,
            } => {
                let _ = store.cloud_sync_meta_set(&key, revision, hash.as_deref(), state);
            }
            PendingWrite::Snapshot {
                key,
                revision,
                hash,
                content,
            } => {
                let _ = store.cloud_sync_snapshot_set(&key, revision, hash.as_deref(), content.as_deref());
            }
            PendingWrite::DeleteMeta { key } => {
                let _ = store.cloud_sync_meta_delete(&key);
            }
        }
    }
}

/// 用本地当前状态重建信封：远端已有的一律保留（除非判定该推本地版本），
/// 本地删除的 L1 落墓碑。
fn build_envelope(
    store: &TelemetryStore,
    current: &VaultEnvelope,
    vault_key_bytes: &[u8; 32],
    device: &str,
    report: &mut CloudSyncReport,
) -> Result<(VaultEnvelope, Vec<PendingWrite>), String> {
    let mut objects: std::collections::BTreeMap<String, RemoteObject> = current
        .objects
        .iter()
        .cloned()
        .map(|obj| (obj.key.clone(), obj))
        .collect();
    let mut tombstones: std::collections::BTreeMap<String, RemoteTombstone> = current
        .tombstones
        .iter()
        .cloned()
        .map(|tomb| (tomb.key.clone(), tomb))
        .collect();
    let mut revision = objects
        .values()
        .map(|obj| obj.revision)
        .chain(tombstones.values().map(|tomb| tomb.revision))
        .max()
        .unwrap_or(0)
        .max(current.revision)
        // 换同步目标、或远端信封被重建时，版本号不该倒退：本地水位里
        // 见过的最高值也算数，否则别的设备会把新内容当成旧版本。
        .max(store.cloud_sync_max_revision().unwrap_or(0));
    let now = chrono::Utc::now().to_rfc3339();
    // 本地副作用（水位、冲突快照、删除 meta）一律**挂账**，等远端真的写成功才落地。
    // 否则一次网络中断就把本地标记成「已同步」，下一轮会拿云端的旧版本静默回滚刚改的内容。
    let mut pending: Vec<PendingWrite> = Vec::new();
    for layer in SYNC_LAYERS {
        let key = format!("{layer}:published");
        let Some(doc) = store.active_memory_layer_document(layer).ok().flatten() else {
            continue;
        };
        let local_hash = sha256_hex(doc.content.as_bytes());
        let remote_hash = objects.get(&key).map(|obj| obj.content_hash.clone());
        if !push_decision(store, &key, remote_hash.as_deref(), &local_hash) {
            continue;
        }
        let payload = b64_encode(&vault_encrypt(vault_key_bytes, doc.content.as_bytes())?);
        revision += 1;
        objects.insert(
            key.clone(),
            RemoteObject {
                key: key.clone(),
                revision,
                content_hash: local_hash.clone(),
                payload,
                updated_at: doc.published_at.clone().unwrap_or_else(|| now.clone()),
                device_id: device.to_string(),
            },
        );
        tombstones.remove(&key);
        pending.push(PendingWrite::Meta {
            key: key.clone(),
            revision,
            hash: Some(local_hash.clone()),
            state: "clean",
        });
        pending.push(PendingWrite::Snapshot {
            key: key.clone(),
            revision,
            hash: Some(local_hash),
            content: Some(doc.content),
        });
        report.pushed += 1;
    }

    // 自定义 L1 记忆。
    let items = store
        .user_defined_l1_memories()
        .map_err(|e| format!("l1: 读取自定义记忆失败 {e}"))?;
    for item in &items {
        let key = format!("l1-user:{}", item.id);
        let payload = L1Payload {
            content: item.memory.clone(),
            memory_type: item.memory_type.clone(),
            updated_at: item.updated_at.clone(),
        };
        let plain = serde_json::to_string(&payload).unwrap_or_default();
        let local_hash = sha256_hex(plain.as_bytes());
        let remote_hash = objects.get(&key).map(|obj| obj.content_hash.clone());
        if !push_decision(store, &key, remote_hash.as_deref(), &local_hash) {
            continue;
        }
        let sealed = b64_encode(&vault_encrypt(vault_key_bytes, plain.as_bytes())?);
        revision += 1;
        objects.insert(
            key.clone(),
            RemoteObject {
                key: key.clone(),
                revision,
                content_hash: local_hash.clone(),
                payload: sealed,
                updated_at: payload.updated_at.clone(),
                device_id: device.to_string(),
            },
        );
        tombstones.remove(&key);
        pending.push(PendingWrite::Meta {
            key: key.clone(),
            revision,
            hash: Some(local_hash.clone()),
            state: "clean",
        });
        // L1 的冲突快照也要留一份：用户手工合并时才有「上一次一致的版本」做三方基线。
        pending.push(PendingWrite::Snapshot {
            key: key.clone(),
            revision,
            hash: Some(local_hash),
            content: Some(plain),
        });
        report.pushed += 1;
    }

    // 本地已删除、云端还在的 L1 → 墓碑，让其他设备也删掉。
    // 只对本设备同步过（有水位）的条目落墓碑：别的设备的条目不由本设备删除。
    let live: std::collections::BTreeSet<String> = items
        .iter()
        .map(|item| format!("l1-user:{}", item.id))
        .collect();
    let candidates: Vec<String> = objects.keys().cloned().collect();
    for key in candidates {
        if !key.starts_with("l1-user:") || live.contains(&key) {
            continue;
        }
        let Some((_, _, state, _)) = store.cloud_sync_meta_row(&key).ok().flatten() else {
            continue;
        };
        if state == "conflict" {
            continue;
        }
        revision += 1;
        tombstones.insert(
            key.clone(),
            RemoteTombstone {
                key: key.clone(),
                revision,
                deleted_at: now.clone(),
            },
        );
        objects.remove(&key);
        pending.push(PendingWrite::DeleteMeta { key: key.clone() });
        report.pushed += 1;
    }

    // 用量总量：每台设备只写自己那一个对象，内容变了就整体覆盖。
    let usage_key = format!("usage-agg:{}", device.replace(':', "-"));
    let rows = store
        .usage_day_aggregates()
        .map_err(|e| format!("{usage_key}: 统计读取失败 {e}"))?;
    let plain = serde_json::to_vec(&rows).map_err(|e| format!("{usage_key}: 序列化失败 {e}"))?;
    if plain.len() > 250 * 1024 {
        report
            .errors
            .push(format!("{usage_key}: 统计超过 250KB 上限，已跳过"));
    } else {
        let local_hash = sha256_hex(&plain);
        let remote_hash = objects.get(&usage_key).map(|obj| obj.content_hash.clone());
        // 单写者：与远端不同就整体重推，不需要 CAS 水位。
        if remote_hash.as_deref() != Some(local_hash.as_str()) {
            let sealed = b64_encode(&vault_encrypt(vault_key_bytes, &plain)?);
            revision += 1;
            objects.insert(
                usage_key.clone(),
                RemoteObject {
                    key: usage_key.clone(),
                    revision,
                    content_hash: local_hash.clone(),
                    payload: sealed,
                    updated_at: now.clone(),
                    device_id: device.to_string(),
                },
            );
            pending.push(PendingWrite::Meta {
                key: usage_key,
                revision,
                hash: Some(local_hash),
                state: "clean",
            });
            report.pushed += 1;
        }
    }

    let objects: Vec<RemoteObject> = objects.into_values().collect();
    let tombstones: Vec<RemoteTombstone> = tombstones.into_values().collect();
    let revision = objects
        .iter()
        .map(|obj| obj.revision)
        .chain(tombstones.iter().map(|tomb| tomb.revision))
        .max()
        .unwrap_or(0)
        .max(current.revision);
    Ok((
        VaultEnvelope {
            format: ENVELOPE_FORMAT,
            revision,
            objects,
            tombstones,
        },
        pending,
    ))
}

/// WebDAV / S3 的一轮同步：读信封信封 → 落地 → 重建 → 条件写回。
///
/// 条件写（`If-Match: ETag`）保证不会覆盖别的设备在这期间写下的版本；被抢先
/// 时重新读一次再合并，最多重试一轮，再失败就留给下一个周期。
fn run_sync_envelope(
    store: &TelemetryStore,
    config: &VaultConfig,
) -> Result<CloudSyncReport, String> {
    use crate::vault_remote::PutOutcome;

    if config.remote.url.trim().is_empty() {
        return Err("云端记忆库未配置地址".into());
    }
    let remote = crate::vault_remote::open(&config.remote)?;
    let key = vault_key(&config.password);
    let device = device_id(store);
    let mut report = CloudSyncReport {
        pushed: 0,
        pulled: 0,
        skipped: 0,
        conflicts: 0,
        usage_imported: 0,
        errors: Vec::new(),
    };
    // 重试回合会把同样的内容重算一遍，计数要拨回本轮起点，否则报告翻倍。
    // 快照必须在循环**外**取：放在循环里取到的是上一回合已经加过的账，回滚就成了空操作。
    let mark = (
        report.pushed,
        report.pulled,
        report.skipped,
        report.conflicts,
        report.usage_imported,
    );
    let mut attempt = 0;
    loop {
        if attempt > 0 {
            report.pushed = mark.0;
            report.pulled = mark.1;
            report.skipped = mark.2;
            report.conflicts = mark.3;
            report.usage_imported = mark.4;
        }
        let label = remote.kind().label();
        let (current, etag) = match remote
            .get()
            .map_err(|e| format!("{label} 读取失败：{e}"))?
        {
            Some((bytes, etag)) => (decode_envelope(&key, &bytes)?, etag),
            None => (VaultEnvelope::default(), None),
        };
        let clean =
            apply_remote_objects(store, &current.objects, &current.tombstones, &key, &mut report);
        if !clean {
            // 有对象没能落地（坏密文 / 口令不对 / 冲突未决）：这一轮不回写，
            // 否则会把没消化完的远端内容盖掉。
            break;
        }
        let (next, pending) = build_envelope(store, &current, &key, &device, &mut report)?;
        let changed = match (serde_json::to_vec(&next), serde_json::to_vec(&current)) {
            (Ok(a), Ok(b)) => a != b,
            // 序列化失败是无处安放的异常，继续写一次让错误自然浮出来，别当成「无变化」。
            _ => true,
        };
        if !changed {
            break; // 无变化，省一次写。
        }
        let payload = encode_envelope(&key, &next)?;
        match remote.put(&payload, etag.as_deref()) {
            Ok(PutOutcome::Written(_)) => {
                // 只有真正写进远端，本地才承认这次同步发生过。
                apply_pending_writes(store, pending);
                break;
            }
            Ok(PutOutcome::Changed) if attempt == 0 => {
                attempt += 1;
                continue;
            }
            Ok(PutOutcome::Changed) => {
                report
                    .errors
                    .push("远端信封在本轮同步期间被其他设备更新，将在下一轮合并".into());
                break;
            }
            Err(e) => {
                report.errors.push(format!("{label} 写入失败：{e}"));
                break;
            }
        }
    }
    let summary = format!(
        "推送 {} · 拉取 {} · 用量 {} 条 · 跳过 {} · 冲突 {} · 错误 {}",
        report.pushed,
        report.pulled,
        report.usage_imported,
        report.skipped,
        report.conflicts,
        report.errors.len()
    );
    let _ = store.app_setting_set(SETTING_LAST_SYNC, &chrono::Utc::now().to_rfc3339());
    let _ = store.app_setting_set("cloud_vault.last_report", &summary);
    Ok(report)
}

/// WebDAV / S3 的「完整拉取」：只读不写，用于把本机拉回云端状态。
fn run_pull_envelope(
    store: &TelemetryStore,
    config: &VaultConfig,
) -> Result<CloudSyncReport, String> {
    if config.remote.url.trim().is_empty() {
        return Err("云端记忆库未配置地址".into());
    }
    let remote = crate::vault_remote::open(&config.remote)?;
    let key = vault_key(&config.password);
    let mut report = CloudSyncReport {
        pushed: 0,
        pulled: 0,
        skipped: 0,
        conflicts: 0,
        usage_imported: 0,
        errors: Vec::new(),
    };
    if let Some((bytes, _)) = remote.get()? {
        let envelope = decode_envelope(&key, &bytes)?;
        if !apply_remote_objects(
            store,
            &envelope.objects,
            &envelope.tombstones,
            &key,
            &mut report,
        ) {
            // 与 run_sync 同一条纪律：没消化完就不宣称完成。
            report
                .errors
                .push("部分远端对象未能落地（密码或内容有误），解决后请重新拉取".into());
        }
    }
    let summary = format!(
        "完整拉取：获取 {} · 用量 {} 条 · 跳过 {} · 冲突 {} · 错误 {}",
        report.pulled,
        report.usage_imported,
        report.skipped,
        report.conflicts,
        report.errors.len()
    );
    let _ = store.app_setting_set(SETTING_LAST_SYNC, &chrono::Utc::now().to_rfc3339());
    let _ = store.app_setting_set("cloud_vault.last_report", &summary);
    Ok(report)
}

// ---------- Tauri 命令 ----------

#[tauri::command]
pub fn cloud_vault_get_settings(
    telemetry: State<'_, TelemetryStore>,
) -> Result<CloudVaultSettingsView, String> {
    let config = vault_config(&telemetry)?;
    Ok(CloudVaultSettingsView {
        kind: config.remote.kind.as_str().to_string(),
        url: config.remote.url,
        user: config.remote.user,
        pat_set: !config.remote.credential.is_empty(),
        password_set: !config.password.is_empty(),
        endpoint: config.remote.endpoint,
        region: config.remote.region,
        path_style: config.remote.path_style,
        enabled: config.enabled,
        auto_interval_min: telemetry
            .app_setting_get::<i64>(SETTING_AUTO_INTERVAL_MIN)
            .unwrap_or(0),
    })
}

#[tauri::command]
pub fn cloud_vault_save_settings(
    telemetry: State<'_, TelemetryStore>,
    kind: Option<String>,
    url: String,
    user: Option<String>,
    pat: Option<String>,
    password: Option<String>,
    endpoint: Option<String>,
    region: Option<String>,
    path_style: Option<bool>,
    auto_interval_min: i64,
    enabled: bool,
) -> Result<(), String> {
    // 地址带 s3:// 前缀时自动判为 S3，与 Magpie 的做法一致。
    let kind = match kind {
        Some(value) if !value.trim().is_empty() => crate::vault_remote::RemoteKind::parse(&value),
        _ => {
            if url.trim().to_ascii_lowercase().starts_with("s3://") {
                crate::vault_remote::RemoteKind::S3
            } else {
                crate::vault_remote::RemoteKind::Server
            }
        }
    };
    let url = url.trim().trim_end_matches('/').to_string();
    telemetry.app_setting_set(SETTING_KIND, &kind.as_str())?;
    telemetry.app_setting_set(SETTING_URL, &url)?;
    if let Some(user) = user {
        let enc = if user.is_empty() {
            String::new()
        } else {
            crate::llm::encrypt_api_key(&user)?
        };
        telemetry.app_setting_set(SETTING_USER, &enc)?;
    }
    if let Some(pat) = pat {
        let enc = if pat.is_empty() {
            String::new()
        } else {
            crate::llm::encrypt_api_key(&pat)?
        };
        telemetry.app_setting_set(SETTING_PAT, &enc)?;
        // 凭据归属当前目标：留空表示「没有凭据」，而不是「沿用别的目标的凭据」。
        telemetry.app_setting_set(
            SETTING_CRED_KIND,
            &if pat.is_empty() {
                String::new()
            } else {
                kind.as_str().to_string()
            },
        )?;
    }
    if let Some(password) = password {
        let enc = if password.is_empty() {
            String::new()
        } else {
            crate::llm::encrypt_api_key(&password)?
        };
        telemetry.app_setting_set(SETTING_PASSWORD, &enc)?;
    }
    if let Some(endpoint) = endpoint {
        telemetry.app_setting_set(SETTING_ENDPOINT, &endpoint.trim().to_string())?;
    }
    if let Some(region) = region {
        telemetry.app_setting_set(SETTING_REGION, &region.trim().to_string())?;
    }
    if let Some(path_style) = path_style {
        telemetry.app_setting_set(SETTING_PATH_STYLE, &path_style)?;
    }
    telemetry.app_setting_set(SETTING_AUTO_INTERVAL_MIN, &auto_interval_min.clamp(0, 1440))?;
    telemetry.app_setting_set(SETTING_ENABLED, &enabled)?;
    Ok(())
}

/// 表单没给 kind 时按地址前缀推断：`s3://` 是 S3，其余沿用已保存的类型。
fn resolve_kind(kind: Option<String>, url: &str, fallback: RemoteKind) -> RemoteKind {
    match kind.map(|value| value.trim().to_ascii_lowercase()) {
        Some(value) if !value.is_empty() => RemoteKind::parse(&value),
        _ => {
            if url.trim().to_ascii_lowercase().starts_with("s3://") {
                RemoteKind::S3
            } else {
                fallback
            }
        }
    }
}

/// 表单里留空的字段沿用已保存的值，避免每次保存都要重填密码。
fn merge_field(given: Option<String>, stored: String) -> String {
    match given {
        Some(value) => {
            let value = value.trim().to_string();
            if value.is_empty() {
                stored
            } else {
                value
            }
        }
        None => stored,
    }
}

#[tauri::command]
pub async fn cloud_vault_test_connection(
    telemetry: State<'_, TelemetryStore>,
    kind: Option<String>,
    url: String,
    user: Option<String>,
    pat: Option<String>,
    endpoint: Option<String>,
    region: Option<String>,
    path_style: Option<bool>,
) -> Result<String, String> {
    let stored = vault_config(&telemetry)?.remote;
    let config = RemoteConfig {
        kind: resolve_kind(kind, &url, stored.kind),
        url: merge_field(Some(url), stored.url.clone()),
        user: merge_field(user, stored.user.clone()),
        credential: merge_field(pat, stored.credential.clone()),
        endpoint: merge_field(endpoint, stored.endpoint.clone()),
        region: merge_field(region, stored.region.clone()),
        path_style: path_style.unwrap_or(stored.path_style),
    };
    let url = config.url.trim().trim_end_matches('/').to_string();
    if url.is_empty() {
        return Err("请先填写服务端地址".into());
    }
    if config.credential.is_empty() {
        return Err("请填写访问凭据后再测试连接".into());
    }
    if config.kind != RemoteKind::Server {
        // 阻塞式 HTTP：挪出 async 运行时，避免卡住 UI 线程调度。
        return tauri::async_runtime::spawn_blocking(move || {
            crate::vault_remote::open(&config)?.probe()
        })
        .await
        .map_err(|e| e.to_string())?;
    }
    let pat = config.credential.clone();
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .map_err(|e| e.to_string())?;
    let health = client
        .get(format!("{url}/health"))
        .send()
        .await
        .map_err(|e| format!("连接失败: {e}"))?;
    if !health.status().is_success() {
        return Err(format!("服务端健康检查返回 HTTP {}", health.status()));
    }
    let body: serde_json::Value = health.json().await.map_err(|e| e.to_string())?;
    // /health is public and therefore cannot prove that synchronization is
    // authorized.  Query above any realistic revision to validate the PAT
    // without downloading the encrypted vault payloads.
    let auth = client
        .get(format!("{url}/objects"))
        .query(&[("since_rev", i64::MAX.to_string())])
        .bearer_auth(&pat)
        .send()
        .await
        .map_err(|e| format!("PAT 验证失败: {e}"))?;
    if !auth.status().is_success() {
        return Err(format!("PAT 验证返回 HTTP {}", auth.status()));
    }
    Ok(body["version"].as_str().unwrap_or("unknown").to_string())
}

#[tauri::command]
pub async fn cloud_vault_sync() -> Result<CloudSyncReport, String> {
    let store = crate::telemetry_store::shared_store().ok_or("telemetry store 未初始化")?;
    tauri::async_runtime::spawn_blocking(move || run_sync(&store))
        .await
        .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn cloud_vault_pull() -> Result<CloudSyncReport, String> {
    let store = crate::telemetry_store::shared_store().ok_or("telemetry store 未初始化")?;
    tauri::async_runtime::spawn_blocking(move || run_pull_only(&store))
        .await
        .map_err(|e| e.to_string())?
}

#[tauri::command]
pub fn cloud_vault_list_conflicts(
    telemetry: State<'_, TelemetryStore>,
) -> Result<Vec<CloudSyncConflict>, String> {
    telemetry.cloud_sync_conflicts()
}

/// Resolving is deliberately local and deterministic: it applies the selected
/// version to this device, anchors the CAS watermark at the remote revision,
/// and leaves the next regular sync to publish the selected local/merged copy.
#[tauri::command]
pub fn cloud_vault_resolve_conflict(
    telemetry: State<'_, TelemetryStore>,
    object_key: String,
    resolution: String,
    merged_content: Option<String>,
) -> Result<(), String> {
    let conflict = telemetry
        .cloud_sync_conflict_get(&object_key)?
        .ok_or("未找到待处理的云端冲突")?;
    if !matches!(resolution.as_str(), "local" | "remote" | "merged") {
        return Err("无效的冲突处理方式".into());
    }
    if conflict.object_kind == "l2" || conflict.object_kind == "l3" {
        let selected = match resolution.as_str() {
            "local" => conflict.local_content.clone(),
            "remote" => conflict.remote_content.clone(),
            _ => merged_content,
        }
        .filter(|content| !content.trim().is_empty())
        .ok_or("所选版本内容为空，无法保存")?;
        telemetry.upsert_published_layer_content(&conflict.object_kind, &selected)?;
        let hash = sha256_hex(selected.as_bytes());
        if resolution == "remote" {
            telemetry.cloud_sync_meta_set(
                &object_key,
                conflict.remote_revision,
                conflict.remote_content_hash.as_deref(),
                "clean",
            )?;
            telemetry.cloud_sync_snapshot_set(
                &object_key,
                conflict.remote_revision,
                conflict.remote_content_hash.as_deref(),
                Some(&selected),
            )?;
        } else {
            telemetry.cloud_sync_meta_set(
                &object_key,
                conflict.remote_revision,
                Some(&hash),
                "dirty",
            )?;
        }
    } else if conflict.object_kind == "l1" {
        let id = object_key
            .strip_prefix("l1-user:")
            .ok_or("无效的自定义记忆冲突 key")?;
        let selected_payload = match resolution.as_str() {
            "local" => conflict.local_content.clone(),
            "remote" => conflict.remote_content.clone(),
            _ => {
                let local: L1Payload = serde_json::from_str(
                    conflict
                        .local_content
                        .as_deref()
                        .ok_or("删除版本不能手动合并")?,
                )
                .map_err(|_| "本地记忆载荷损坏")?;
                Some(
                    serde_json::to_string(&L1Payload {
                        content: merged_content
                            .filter(|value| !value.trim().is_empty())
                            .ok_or("合并内容不能为空")?,
                        memory_type: local.memory_type,
                        updated_at: chrono::Utc::now().to_rfc3339(),
                    })
                    .map_err(|error| error.to_string())?,
                )
            }
        };
        match selected_payload {
            Some(raw) => {
                let payload: L1Payload =
                    serde_json::from_str(&raw).map_err(|_| "所选记忆载荷损坏")?;
                telemetry.upsert_synced_l1_memory(
                    id,
                    &payload.content,
                    &payload.memory_type,
                    &payload.updated_at,
                )?;
                let hash = sha256_hex(raw.as_bytes());
                if resolution == "remote" {
                    telemetry.cloud_sync_meta_set(
                        &object_key,
                        conflict.remote_revision,
                        conflict.remote_content_hash.as_deref(),
                        "clean",
                    )?;
                    telemetry.cloud_sync_snapshot_set(
                        &object_key,
                        conflict.remote_revision,
                        conflict.remote_content_hash.as_deref(),
                        Some(&raw),
                    )?;
                } else {
                    // A remote tombstone has no object revision to match on a
                    // re-create; start at zero so the normal PUT creates it.
                    let revision = if conflict.remote_content.is_some() {
                        conflict.remote_revision
                    } else {
                        0
                    };
                    telemetry.cloud_sync_meta_set(&object_key, revision, Some(&hash), "dirty")?;
                }
            }
            None => {
                telemetry.delete_synced_l1_memory(id)?;
                if conflict.remote_content.is_some() {
                    // User chose their local delete over an edited remote item.
                    telemetry.cloud_sync_meta_set(
                        &object_key,
                        conflict.remote_revision,
                        None,
                        "dirty",
                    )?;
                } else {
                    telemetry.cloud_sync_meta_delete(&object_key)?;
                }
            }
        }
    } else {
        return Err("不支持的云端冲突类型".into());
    }
    telemetry.cloud_sync_conflict_delete(&object_key)
}

#[tauri::command]
pub fn cloud_vault_status(
    telemetry: State<'_, TelemetryStore>,
) -> Result<CloudVaultStatus, String> {
    let config = vault_config(&telemetry)?;
    let enabled = config.enabled;
    let configured = !config.remote.url.trim().is_empty()
        && !config.remote.credential.is_empty()
        && !config.password.is_empty();
    let dirty = telemetry
        .cloud_sync_meta_rows()?
        .iter()
        .filter(|(_, _, _, state, _)| state == "dirty")
        .count() as u32;
    let conflicts = telemetry.cloud_sync_conflicts()?.len() as u32;
    Ok(CloudVaultStatus {
        configured,
        enabled,
        dirty,
        conflicts,
        last_sync_at: telemetry.app_setting_get(SETTING_LAST_SYNC),
        last_report: telemetry.app_setting_get("cloud_vault.last_report"),
        auto_interval_min: telemetry
            .app_setting_get::<i64>(SETTING_AUTO_INTERVAL_MIN)
            .unwrap_or(0),
    })
}

/// 定时同步调度：每分钟检查一次设置（改设置无需重启），
/// 到期则触发一轮完整推拉。失败静默，下个周期重试。
pub fn start_cloud_sync_scheduler() {
    tauri::async_runtime::spawn(async move {
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(60)).await;
            let Some(store) = crate::telemetry_store::shared_store() else {
                continue;
            };
            let interval_min = store
                .app_setting_get::<i64>(SETTING_AUTO_INTERVAL_MIN)
                .unwrap_or(0);
            if interval_min <= 0 {
                continue;
            }
            if store.app_setting_get::<bool>(SETTING_ENABLED) != Some(true) {
                continue;
            }
            let now = chrono::Utc::now();
            let due = match store.app_setting_get::<String>(SETTING_LAST_AUTO_SYNC) {
                Some(last) => parse_ts(&last) + chrono::Duration::minutes(interval_min) <= now,
                None => true,
            };
            if !due {
                continue;
            }
            let _ = store.app_setting_set(SETTING_LAST_AUTO_SYNC, &now.to_rfc3339());
            let task_store = store.clone();
            let _ = tauri::async_runtime::spawn_blocking(move || run_sync(&task_store)).await;
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::telemetry_store::TelemetryStore;
    use crate::test_http::fake_dav;
    use crate::vault_remote::PutOutcome;
    use rusqlite::Connection;
    use std::sync::{Arc, Mutex};

    const SYNC_PASSWORD: &str = "envelope-test-password";
    const REMOTE_PATH: &str = fake_dav::VAULT_PATH;

    /// `TelemetryStore::new()` 打开的是用户配置目录里真实的
    /// telemetry.sqlite3：并行跑会互相踩、还会弄脏本机数据，所以这里改用内存库，
    /// 但建表脚本**直接取生产那份**——自己维护一份简化过的测试库等于没测，
    /// 结构一旦漂移还会骗过断言。提取标记对不上必须响报错，
    /// 不能悄悄退化成空库继续跑。
    fn new_store() -> TelemetryStore {
        let source = include_str!("telemetry_store.rs");
        let quoted = source
            .find("\"PRAGMA journal_mode=WAL;")
            .expect("telemetry_store.rs 里找不到建表脚本起点");
        let tail = source[quoted..]
            .strip_prefix('"')
            .expect("建表脚本应以引号开头");
        let end = tail
            .find(",\n        )\n        .map_err")
            .expect("telemetry_store.rs 里找不到建表脚本终点");
        // 结束标记从逗号起算，前面那个引号要还回去。
        let schema = tail[..end]
            .strip_suffix('"')
            .expect("建表脚本应以引号收尾");
        let head: String = schema.chars().take(20).collect();
        assert!(
            schema.starts_with("PRAGMA journal_mode=WAL;"),
            "建表脚本起点不对，取到的是：{head}"
        );
        assert!(
            schema.contains("cloud_sync_meta") && schema.contains("local_memory_items"),
            "取出来的不是完整建表脚本"
        );
        let conn = Connection::open_in_memory().expect("打开内存 SQLite 失败");
        conn.execute_batch(schema).expect("按生产脚本建表失败");
        TelemetryStore {
            conn: Arc::new(Mutex::new(conn)),
        }
    }

    /// 一台配好同一云端的设备：独立的 store + 独立的 device_id。
    fn paired(url: &str) -> TelemetryStore {
        let store = new_store();
        store
            .app_setting_set(SETTING_KIND, &RemoteKind::WebDav.as_str().to_string())
            .expect("保存同步目标");
        store
            .app_setting_set(SETTING_URL, &url.to_string())
            .expect("保存地址");
        store
            .app_setting_set(SETTING_ENABLED, &true)
            .expect("保存开关");
        store
            .app_setting_set(SETTING_CRED_KIND, &RemoteKind::WebDav.as_str().to_string())
            .expect("标注凭据归属");
        for (setting, plain) in [
            (SETTING_USER, "tester@example.com"),
            (SETTING_PAT, "app-password"),
            (SETTING_PASSWORD, SYNC_PASSWORD),
        ] {
            let sealed = crate::llm::encrypt_api_key(plain).expect("加密凭据");
            store
                .app_setting_set(setting, &sealed)
                .expect("保存凭据");
        }
        store
    }

    fn open_remote(url: &str) -> Box<dyn crate::vault_remote::Remote> {
        let config = RemoteConfig {
            kind: RemoteKind::WebDav,
            url: url.to_string(),
            user: "tester@example.com".to_string(),
            credential: "app-password".to_string(),
            ..RemoteConfig::default()
        };
        crate::vault_remote::open(&config).expect("打开 WebDAV 远端")
    }

    fn key() -> [u8; 32] {
        vault_key(SYNC_PASSWORD)
    }

    fn add_l1(store: &TelemetryStore, id: &str, content: &str) {
        let now = chrono::Utc::now().to_rfc3339();
        store
            .upsert_synced_l1_memory(id, content, "fact", &now)
            .expect("写入本地自定义记忆");
    }

    fn l1_of(store: &TelemetryStore, id: &str) -> Option<String> {
        store
            .find_user_defined_l1(id)
            .ok()
            .flatten()
            .map(|item| item.memory)
    }

    fn remote_envelope(server: &fake_dav::Server) -> VaultEnvelope {
        let bytes = server.body(REMOTE_PATH).expect("远端应有信封文件");
        decode_envelope(&key(), &bytes).expect("信封应能用同步密码解开")
    }

    fn object<'a>(envelope: &'a VaultEnvelope, want: &str) -> Option<&'a RemoteObject> {
        envelope.objects.iter().find(|obj| obj.key == want)
    }

    /// 解开某个对象的载荷，按 {content, memory_type, updated_at} 的形式返回。
    fn plain_of(obj: &RemoteObject) -> String {
        let blob = b64_decode(&obj.payload).expect("payload 应是合法 base64");
        let plain = vault_decrypt(&key(), &blob).expect("载荷应能解密");
        String::from_utf8_lossy(&plain).to_string()
    }

    /// 一个只含 `l1-user:shared` 的信封，用来模拟另一台设备写下来的版本。
    fn envelope_with(content: &str) -> VaultEnvelope {
        let payload = L1Payload {
            content: content.to_string(),
            memory_type: "fact".to_string(),
            updated_at: "2026-10-08T10:00:00Z".to_string(),
        };
        let plain = serde_json::to_vec(&payload).expect("序列化 L1 载荷");
        VaultEnvelope {
            format: ENVELOPE_FORMAT,
            revision: 1,
            objects: vec![RemoteObject {
                key: "l1-user:shared".to_string(),
                revision: 1,
                content_hash: sha256_hex(&plain),
                payload: b64_encode(&vault_encrypt(&key(), &plain).expect("加密载荷")),
                updated_at: payload.updated_at.clone(),
                device_id: "peer-device".to_string(),
            }],
            tombstones: Vec::new(),
        }
    }

    /// `VaultEnvelope` 没派生 Debug，取错误信息只能自己 match。
    fn decode_err(bytes: &[u8]) -> String {
        decode_err_with(&key(), bytes)
    }

    fn decode_err_with(used_key: &[u8; 32], bytes: &[u8]) -> String {
        match decode_envelope(used_key, bytes) {
            Ok(_) => panic!("这里应当解密失败"),
            Err(message) => message,
        }
    }

    // ---------- 信封的编码与容错 ----------

    #[test]
    fn envelope_roundtrip_preserves_every_field() {
        let plain = serde_json::to_vec(&L1Payload {
            content: "记住这一条".to_string(),
            memory_type: "fact".to_string(),
            updated_at: "2026-10-08T10:00:00Z".to_string(),
        })
        .expect("序列化载荷");
        let source = VaultEnvelope {
            format: ENVELOPE_FORMAT,
            revision: 42,
            objects: vec![RemoteObject {
                key: "l1-user:pickup".to_string(),
                revision: 7,
                content_hash: sha256_hex(&plain),
                payload: b64_encode(&vault_encrypt(&key(), &plain).expect("加密载荷")),
                updated_at: "2026-10-08T10:00:00Z".to_string(),
                device_id: "pc-1".to_string(),
            }],
            tombstones: vec![RemoteTombstone {
                key: "l1-user:gone".to_string(),
                revision: 9,
                deleted_at: "2026-10-08T11:00:00Z".to_string(),
            }],
        };
        let bytes = encode_envelope(&key(), &source).expect("加密信封");
        assert_eq!(
            &bytes[..ENVELOPE_MAGIC.len()],
            ENVELOPE_MAGIC,
            "信封必须以魔数开头"
        );

        let back = decode_envelope(&key(), &bytes).expect("解密信封");
        assert_eq!(back.format, ENVELOPE_FORMAT, "format 应原样回合");
        assert_eq!(back.revision, 42, "revision 应原样回合");
        assert_eq!(back.objects.len(), 1);
        assert_eq!(back.objects[0].key, "l1-user:pickup");
        assert_eq!(back.objects[0].revision, 7);
        assert_eq!(back.objects[0].content_hash, sha256_hex(&plain));
        assert_eq!(back.objects[0].updated_at, "2026-10-08T10:00:00Z");
        assert_eq!(back.objects[0].device_id, "pc-1");
        assert_eq!(plain_of(&back.objects[0]).contains("记住这一条"), true);
        assert_eq!(back.tombstones.len(), 1);
        assert_eq!(back.tombstones[0].key, "l1-user:gone");
        assert_eq!(back.tombstones[0].revision, 9);
        assert_eq!(back.tombstones[0].deleted_at, "2026-10-08T11:00:00Z");
    }

    #[test]
    fn decode_envelope_rejects_foreign_empty_and_broken_files() {
        let k = key();
        // 空文件：网盘上的 0 字节占位，或被别的程序覆盖过。
        assert!(decode_envelope(&k, &[]).is_err(), "空文件必须报错");
        assert!(decode_envelope(&k, ENVELOPE_MAGIC).is_err(), "只有魔数必须报错");
        assert!(
            decode_envelope(&k, b"another app's file, not ours at all").is_err(),
            "非本应用文件必须报错"
        );

        let bytes = encode_envelope(&k, &envelope_with("内容")).expect("加密信封");
        // 截断：上传被中继掐断只留下一半，必须报错而不是 panic。
        let cut = ENVELOPE_MAGIC.len() + 30;
        assert!(
            decode_envelope(&k, &bytes[..cut]).is_err(),
            "被截断的密文必须报错"
        );
        // 篡改：GCM 必须验出密文被动过。
        let mut tampered = bytes.clone();
        let last = tampered.len() - 1;
        tampered[last] ^= 0xff;
        assert!(
            decode_envelope(&k, &tampered).is_err(),
            "被篡改的密文必须报错"
        );
        // 魔数不对要和「口令不对」分开报：前者是别人的文件。
        let mut foreign = bytes.clone();
        foreign[0] = b'X';
        let message = decode_err(&foreign);
        assert!(message.contains("不是本应用"), "错误信息应对用户可读：{message}");
    }

    #[test]
    fn decode_envelope_points_at_the_password_when_it_is_wrong() {
        let bytes = encode_envelope(&key(), &envelope_with("内容")).expect("加密信封");
        let message = decode_err_with(&vault_key("另一台设备上设的密码"), &bytes);
        assert!(message.contains("同步密码"), "错误信息应指向同步密码：{message}");
    }

    // ---------- 条件写 ----------

    /// 两台设备抢写同一份信封：拿着过期 ETag 的写入必须被判为 Changed，
    /// 不能把别人的版本盖掉——这是信封模式下唯一的并发保护。
    #[test]
    fn stale_etag_write_is_reported_as_changed_not_overwrite() {
        let server = fake_dav::Server::start();
        let remote = open_remote(&server.url);
        let from_a = encode_envelope(&key(), &envelope_with("设备 A 的记忆")).expect("加密 A");
        match remote.put(&from_a, None).expect("A 首次写入") {
            PutOutcome::Written(_) => {}
            other => panic!("首次创建应成功，实际 {other:?}"),
        }
        let (_, etag_a) = remote
            .get()
            .expect("读取 A 写下的信封")
            .expect("信封应已存在");
        let etag_a = etag_a.as_deref().expect("服务端应回 ETag");

        // 设备 B 抢先用同一个 ETag 覆盖成功。
        let from_b = encode_envelope(&key(), &envelope_with("设备 B 的记忆")).expect("加密 B");
        match remote.put(&from_b, Some(etag_a)).expect("B 抢写") {
            PutOutcome::Written(_) => {}
            other => panic!("设备 B 应写入成功，实际 {other:?}"),
        }
        // 设备 A 再拿那个已经过期的 ETag 写：412 → Changed，而不是覆盖。
        let stale =
            encode_envelope(&key(), &envelope_with("设备 A 准备覆盖的版本")).expect("加密 A2");
        match remote.put(&stale, Some(etag_a)).expect("A 用过期 ETag 写") {
            PutOutcome::Changed => {}
            other => panic!("过期 ETag 必须判为 Changed，实际 {other:?}"),
        }
        let kept = remote_envelope(&server);
        assert_eq!(
            plain_of(object(&kept, "l1-user:shared").expect("对象应还在")),
            "{\"content\":\"设备 B 的记忆\",\"memory_type\":\"fact\",\"updated_at\":\"2026-10-08T10:00:00Z\"}",
            "远端必须保留设备 B 的版本"
        );
    }

    // ---------- 整轮同步 ----------

    #[test]
    fn two_devices_exchange_custom_memories() {
        let server = fake_dav::Server::start();
        let a = paired(&server.url);
        let b = paired(&server.url);

        add_l1(&a, "alpha", "阿尔法：第一台设备写的");
        let report = run_sync(&a).expect("A 首轮同步");
        assert!(report.errors.is_empty(), "A 首轮不该有错误：{:?}", report.errors);
        assert!(report.pushed >= 1, "A 应至少推一个对象");
        assert!(
            object(&remote_envelope(&server), "l1-user:alpha").is_some(),
            "远端应有 alpha"
        );

        let report = run_sync(&b).expect("B 首轮拉取");
        assert!(report.errors.is_empty(), "B 不该有错误：{:?}", report.errors);
        assert_eq!(
            l1_of(&b, "alpha").as_deref(),
            Some("阿尔法：第一台设备写的"),
            "B 应拉到 alpha"
        );

        add_l1(&b, "beta", "贝塔：第二台设备写的");
        let report = run_sync(&b).expect("B 推送 beta");
        assert!(report.pushed >= 1, "B 应把 beta 推上去");
        let report = run_sync(&a).expect("A 拉取 beta");
        assert!(report.errors.is_empty(), "A 不该有错误：{:?}", report.errors);
        assert_eq!(
            l1_of(&a, "beta").as_deref(),
            Some("贝塔：第二台设备写的"),
            "A 应拉到 beta"
        );
        // 后续轮次不能把本机已有的记忆弄丢。
        assert_eq!(
            l1_of(&a, "alpha").as_deref(),
            Some("阿尔法：第一台设备写的"),
            "alpha 不该被后来的轮次改掉"
        );
    }

    #[test]
    fn published_l3_document_reaches_the_other_device() {
        let server = fake_dav::Server::start();
        let a = paired(&server.url);
        let b = paired(&server.url);
        a.save_memory_layer_document("l3", "# 长期记忆\n用户偏好 concise", "published", "l1", &[], None, None)
            .expect("发布 L3 文档");

        let report = run_sync(&a).expect("A 推送 L3");
        assert!(report.errors.is_empty(), "A 不该有错误：{:?}", report.errors);
        let report = run_sync(&b).expect("B 拉取 L3");
        assert!(report.errors.is_empty(), "B 不该有错误：{:?}", report.errors);
        let doc = b
            .active_memory_layer_document("l3")
            .expect("读取 L3")
            .expect("B 应有 L3 文档");
        assert!(
            doc.content.contains("用户偏好 concise"),
            "B 应拿到 L3 正文：{}",
            doc.content
        );
    }

    /// 已发布过的 L2/L3 重新发布后应该在同一轮推出去（这条是当前能对的行为，
    /// 改 #1 的合并顺序时不能把它改坏）。
    #[test]
    fn republished_l3_document_is_pushed_again() {
        let server = fake_dav::Server::start();
        let a = paired(&server.url);
        a.save_memory_layer_document("l3", "第一版长期记忆", "published", "l1", &[], None, None)
            .expect("发布第一版");
        let first = run_sync(&a).expect("首轮推送");
        assert!(first.pushed >= 1, "首轮应推上去");
        let first_hash = object(&remote_envelope(&server), "l3:published")
            .expect("应有 L3 对象")
            .content_hash
            .clone();

        a.save_memory_layer_document("l3", "第二版长期记忆", "published", "l1", &[], None, None)
            .expect("重新发布");
        let second = run_sync(&a).expect("第二轮推送");
        assert!(second.errors.is_empty(), "不该有错误：{:?}", second.errors);
        assert_eq!(second.conflicts, 0, "本机重新发布不该被判成冲突");
        assert!(second.pushed >= 1, "重新发布的内容应被推上去");
        let second_hash = object(&remote_envelope(&server), "l3:published")
            .expect("应有 L3 对象")
            .content_hash
            .clone();
        assert_ne!(first_hash, second_hash, "远端应换成第二版");
        assert_eq!(
            a.active_memory_layer_document("l3")
                .expect("读 L3")
                .map(|doc| doc.content),
            Some("第二版长期记忆".to_string()),
            "本机应保留自己重新发布的内容"
        );
    }

    /// 没有变化时不应重写远端文件：信封每次加密的 nonce 都是随机的，
    /// 一旦按「密文有没有变」判断就会每轮白写一次，坚果云按请求数限流。
    #[test]
    fn idle_round_does_not_rewrite_the_remote_file() {
        let server = fake_dav::Server::start();
        let a = paired(&server.url);
        add_l1(&a, "alpha", "稳定的内容");
        run_sync(&a).expect("首轮同步");
        let after_first = server.body(REMOTE_PATH).expect("应有信封");

        let report = run_sync(&a).expect("第二轮同步");
        assert!(report.errors.is_empty(), "第二轮不该有错误：{:?}", report.errors);
        assert_eq!(report.pushed, 0, "无变化不该有推送");
        assert_eq!(
            server.body(REMOTE_PATH).as_ref(),
            Some(&after_first),
            "无变化不应重写远端文件"
        );
    }

    /// 写入出错时：错误要进 report.errors（而不是整轮 Result::Err 丢掉报告），
    /// 远端文件要原样保留，本地改动也要留着等下轮重试。
    #[test]
    fn write_failure_is_reported_and_nothing_is_lost() {
        let server = fake_dav::Server::start();
        let a = paired(&server.url);
        add_l1(&a, "alpha", "已经同步过的内容");
        run_sync(&a).expect("首轮同步");
        let before = server.body(REMOTE_PATH).expect("应有信封");

        server.fail_puts();
        add_l1(&a, "beta", "写失败时也应留在本地");
        let report = run_sync(&a).expect("第二轮应返回报告而不是整体失败");
        assert_eq!(
            report.errors.len(),
            1,
            "写入失败必须落到 report.errors：{:?}",
            report.errors
        );
        assert_eq!(
            server.body(REMOTE_PATH).as_ref(),
            Some(&before),
            "写失败不该改动远端文件"
        );
        assert_eq!(
            l1_of(&a, "beta").as_deref(),
            Some("写失败时也应留在本地"),
            "本地新增不该因为写失败丢掉"
        );
    }

    /// 一轮同步里被别人抢先（412）应当重新读一次再合并，而不是立刻报错。
    #[test]
    fn changed_write_is_merged_within_the_same_round() {
        let server = fake_dav::Server::start();
        let a = paired(&server.url);
        add_l1(&a, "alpha", "先同步好的内容");
        run_sync(&a).expect("首轮同步");

        server.fail_next_put(412);
        add_l1(&a, "beta", "抢写之后也要落上去");
        let report = run_sync(&a).expect("第二轮同步");
        assert!(
            report.errors.is_empty(),
            "抢写应在一次重试内消化：{:?}",
            report.errors
        );
        assert!(
            object(&remote_envelope(&server), "l1-user:beta").is_some(),
            "重试后应写入 beta"
        );
    }

    /// 用量统计是「每台设备只写自己那一个对象」：本机变了就重写自己的，
    /// 别人的必须原样保留。
    #[test]
    fn own_usage_object_is_refreshed_and_peers_are_kept() {
        let server = fake_dav::Server::start();
        let a = paired(&server.url);
        let b = paired(&server.url);
        run_sync(&a).expect("A 首轮");
        run_sync(&b).expect("B 首轮");
        let key_a = format!("usage-agg:{}", device_id(&a).replace(':', "-"));
        let key_b = format!("usage-agg:{}", device_id(&b).replace(':', "-"));

        let before = remote_envelope(&server);
        let before_a = object(&before, &key_a).expect("应有 A 的用量对象").payload.clone();
        let before_b = object(&before, &key_b).expect("应有 B 的用量对象").payload.clone();

        {
            let conn = a.conn.lock().expect("锁");
            conn.execute(
                "INSERT INTO usage_records
                   (record_id, source, session_id, occurred_at, model, record_kind,
                    input_tokens, output_tokens, cached_tokens, origin)
                 VALUES ('r1', 'claude', 's1', '2026-10-08T10:00:00Z', 'm', 'estimated', 100, 20, 0, 'hooks')",
                [],
            )
            .expect("写入一条用量明细");
        }
        let report = run_sync(&a).expect("A 第二轮");
        assert!(report.errors.is_empty(), "A 不该有错误：{:?}", report.errors);

        let after = remote_envelope(&server);
        assert_ne!(
            object(&after, &key_a).expect("A 的用量对象应保留").payload,
            before_a,
            "本机用量变了就该重写自己那个对象"
        );
        assert_eq!(
            object(&after, &key_b).map(|obj| obj.payload.clone()),
            Some(before_b),
            "不该动别人的用量对象"
        );
    }

    // ---------- 回归用例：这三处都是「修复前会静默出错」的地方，别删 ----------

    /// 本机改一条已同步过的自定义记忆：改动必须推上去，不能被判成冲突。
    /// 信封模式每轮返回全量对象，apply 阶段若不先问「远端有没有变」，
    /// 就会把「远端 == 水位」误认成远端变更，用户于是永远处理不完一个假冲突。
    #[test]
    fn local_edit_of_synced_memory_is_pushed() {
        let server = fake_dav::Server::start();
        let a = paired(&server.url);
        add_l1(&a, "alpha", "第一版");
        run_sync(&a).expect("首轮推送");

        add_l1(&a, "alpha", "第二版：本机改过的");
        let report = run_sync(&a).expect("第二轮同步");
        assert!(report.errors.is_empty(), "不该有错误：{:?}", report.errors);
        assert_eq!(report.conflicts, 0, "本机自己的修改不该被判成冲突");
        assert!(
            plain_of(object(&remote_envelope(&server), "l1-user:alpha").expect("应有 alpha"))
                .contains("第二版"),
            "远端应收到本机修改"
        );
        assert_eq!(
            l1_of(&a, "alpha").as_deref(),
            Some("第二版：本机改过的"),
            "本机修改不该被远端旧版本覆盖"
        );
    }

    /// 缺陷 #2：本机删掉一条已同步过的自定义记忆，下一轮应该落墓碑，
    /// 别设备据此删除，且本机不能被拉回来复活。
    #[test]
    fn local_delete_propagates_as_tombstone() {
        let server = fake_dav::Server::start();
        let a = paired(&server.url);
        let b = paired(&server.url);
        add_l1(&a, "alpha", "待删除的记忆");
        run_sync(&a).expect("A 推送");
        run_sync(&b).expect("B 拉取");
        assert_eq!(l1_of(&b, "alpha").as_deref(), Some("待删除的记忆"));

        {
            let conn = a.conn.lock().expect("锁");
            conn.execute("DELETE FROM local_memory_items WHERE id = 'alpha'", [])
                .expect("本地删除");
        }
        let report = run_sync(&a).expect("A 删除同步");
        assert!(report.errors.is_empty(), "不该有错误：{:?}", report.errors);
        let envelope = remote_envelope(&server);
        assert!(
            object(&envelope, "l1-user:alpha").is_none(),
            "对象应从信封里移除"
        );
        assert!(
            envelope
                .tombstones
                .iter()
                .any(|tomb| tomb.key == "l1-user:alpha"),
            "应留下墓碑"
        );
        assert!(
            l1_of(&a, "alpha").is_none(),
            "删掉的记忆不该又被拉回来复活"
        );

        let report = run_sync(&b).expect("B 拉取墓碑");
        assert!(report.errors.is_empty(), "B 不该有错误：{:?}", report.errors);
        assert!(l1_of(&b, "alpha").is_none(), "B 也应删掉这条记忆");
    }

    /// 缺陷 #3：`build_envelope` 在 `put` 还没被确认时就把本地水位写成 clean、
    /// 顺手写掉快照；写失败的下一轮拿这份水位当证据，判定「本地没改过」，
    /// 于是用云端旧版本把用户刚发布的内容覆盖掉，而且报告里一个错误都没有。
    #[test]
    fn failed_write_does_not_roll_back_local_layer() {
        let server = fake_dav::Server::start();
        let a = paired(&server.url);
        a.save_memory_layer_document("l3", "第一版长期记忆", "published", "l1", &[], None, None)
            .expect("发布第一版");
        run_sync(&a).expect("首轮");

        server.fail_puts();
        a.save_memory_layer_document("l3", "第二版长期记忆", "published", "l1", &[], None, None)
            .expect("发布第二版");
        let failing = run_sync(&a).expect("写失败的一轮");
        assert_eq!(
            failing.errors.len(),
            1,
            "应记录一次写入失败：{:?}",
            failing.errors
        );

        server.allow_puts();
        let recovered = run_sync(&a).expect("网络恢复后的一轮");
        assert!(recovered.errors.is_empty(), "不该有错误：{:?}", recovered.errors);
        assert_eq!(
            a.active_memory_layer_document("l3")
                .expect("读 L3")
                .map(|doc| doc.content),
            Some("第二版长期记忆".to_string()),
            "网络恢复后本机不该被云端旧版本回滚"
        );
        let content = plain_of(object(&remote_envelope(&server), "l3:published").expect("应有 L3"));
        assert!(content.contains("第二版"), "网络恢复后应把第二版补推上去：{content}");
    }

    /// 抢写重试会把同一批对象重算一遍，报告里的计数必须只算一次。
    #[test]
    fn retry_does_not_double_count_the_report() {
        let server = fake_dav::Server::start();
        let a = paired(&server.url);
        add_l1(&a, "alpha", "先同步好的内容");
        run_sync(&a).expect("首轮");

        server.fail_next_put(412);
        add_l1(&a, "beta", "重试后才落上去");
        let report = run_sync(&a).expect("第二轮");
        assert!(report.errors.is_empty(), "抢写应被重试消化：{:?}", report.errors);
        assert_eq!(report.pushed, 1, "重试不该让推送计数翻倍");
        assert_eq!(
            report.conflicts, 0,
            "重试不该产生第二个冲突计数"
        );
        assert!(
            object(&remote_envelope(&server), "l1-user:beta").is_some(),
            "重试后 beta 应落上去"
        );
    }

    /// 对本机删除 + 远端修改的冲突选「保留本地（删除）」：删除必须真的生效，
    /// 墓碑要写出去，而且下一轮不能把同一条冲突再记一遍。
    #[test]
    fn resolved_local_delete_propagates_as_tombstone() {
        let server = fake_dav::Server::start();
        let a = paired(&server.url);
        let b = paired(&server.url);
        add_l1(&a, "alpha", "第一版");
        run_sync(&a).expect("A 推送");
        run_sync(&b).expect("B 拉取");
        // A 改成第二版，远端领先 B 的水位。
        add_l1(&a, "alpha", "第二版");
        run_sync(&a).expect("A 改后推送");
        b.conn
            .lock()
            .expect("锁")
            .execute("DELETE FROM local_memory_items WHERE id = 'alpha'", [])
            .expect("本机删除");

        let report = run_sync(&b).expect("B 同步");
        assert_eq!(report.conflicts, 1, "删除撞上远端修改应记一条冲突");
        assert!(l1_of(&b, "alpha").is_none(), "记忆不能被拉回来复活");
        let conflict = b
            .cloud_sync_conflict_get("l1-user:alpha")
            .expect("读冲突")
            .expect("应有冲突记录");
        assert!(conflict.local_content.is_none(), "本地一侧应记为「已删除」");
        assert!(conflict.remote_content.is_some(), "远端一侧应带着新版本");

        // 用户选「保留本地」：这条命令是 tauri::command，测试里没法注入 State，
        // 这里按 cloud_vault_resolve_conflict 同一分支的原语复刻一遍。
        b.delete_synced_l1_memory("alpha").expect("删本地");
        b.cloud_sync_meta_set("l1-user:alpha", conflict.remote_revision, None, "dirty")
            .expect("置 dirty");
        b.cloud_sync_conflict_delete("l1-user:alpha")
            .expect("清掉冲突");

        let report = run_sync(&b).expect("裁决后第一轮");
        assert!(report.errors.is_empty(), "不该有错误：{:?}", report.errors);
        assert_eq!(report.conflicts, 0, "裁决过的事不该每轮再吵一遍");
        let envelope = remote_envelope(&server);
        assert!(
            object(&envelope, "l1-user:alpha").is_none(),
            "对象应从信封里移除"
        );
        assert!(
            envelope
                .tombstones
                .iter()
                .any(|tomb| tomb.key == "l1-user:alpha"),
            "应留下墓碑"
        );
        assert!(l1_of(&b, "alpha").is_none(), "本机不该再出现这条记忆");
        let report = run_sync(&b).expect("裁决后第二轮");
        assert_eq!(report.conflicts, 0, "第二轮也不该再冒出冲突");
        assert!(
            b.cloud_sync_conflicts().expect("读冲突").is_empty(),
            "冲突列表应已清空"
        );

        // 另一台设备据此删掉。
        let report = run_sync(&a).expect("A 拉取墓碑");
        assert!(report.errors.is_empty(), "A 不该有错误：{:?}", report.errors);
        assert!(l1_of(&a, "alpha").is_none(), "A 也应删掉这条记忆");
    }

    /// 对本机修改 + 远端修改的冲突选「保留本地」：本地版本要推上去，
    /// 既不能被远端覆盖，也不能把同一条冲突再记一遍。
    #[test]
    fn resolved_local_edit_is_pushed() {
        let server = fake_dav::Server::start();
        let a = paired(&server.url);
        let b = paired(&server.url);
        add_l1(&a, "alpha", "第一版");
        run_sync(&a).expect("A 推送");
        run_sync(&b).expect("B 拉取");

        // 两边都改同一个记忆 → B 上记录一条冲突。
        add_l1(&a, "alpha", "远端第二版");
        run_sync(&a).expect("A 改后推送");
        add_l1(&b, "alpha", "本地第二版");
        let report = run_sync(&b).expect("B 同步");
        assert_eq!(report.conflicts, 1, "两侧都改应记一条冲突");

        // 用户在 B 上选「保留本地」。
        let conflict = b
            .cloud_sync_conflict_get("l1-user:alpha")
            .expect("读冲突")
            .expect("应有冲突记录");
        let chosen = conflict.local_content.clone().expect("本地版本应在");
        let local_hash = sha256_hex(chosen.as_bytes());
        b.upsert_synced_l1_memory("alpha", "本地第二版", "fact", &chrono::Utc::now().to_rfc3339())
            .expect("按裁决写回本地");
        b.cloud_sync_meta_set("l1-user:alpha", conflict.remote_revision, Some(&local_hash), "dirty")
            .expect("置 dirty");
        b.cloud_sync_conflict_delete("l1-user:alpha")
            .expect("清掉冲突");

        let report = run_sync(&b).expect("裁决后同步");
        assert!(report.errors.is_empty(), "不该有错误：{:?}", report.errors);
        assert_eq!(report.conflicts, 0, "裁决过的事不该再判一次冲突");
        assert!(
            report.pushed >= 1,
            "裁决为本地版就该把它推上去，实际 pushed={}",
            report.pushed
        );
        assert_eq!(
            l1_of(&b, "alpha").as_deref(),
            Some("本地第二版"),
            "本地裁决的版本不能被远端覆盖"
        );
        let remote = plain_of(
            object(&remote_envelope(&server), "l1-user:alpha").expect("应有 alpha"),
        );
        assert!(remote.contains("本地第二版"), "远端应收到裁决后的版本：{remote}");
    }
}
