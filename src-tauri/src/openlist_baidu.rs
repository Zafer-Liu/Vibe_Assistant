//! Provision the app-owned OpenList v4.2.9 listener. The caller owns its process and data directory.
//! Wire contracts: `server/handles/{auth,storage,fsread,fsmanage}.go`, `internal/op/storage.go`
//! and `drivers/baidu_netdisk/{meta,driver,util}.go` in the v4.2.9 tag.

use std::io::Read;
use std::sync::{Mutex, TryLockError};
use std::thread;
use std::time::{Duration, Instant};

use reqwest::blocking::Client;
use reqwest::header::{HeaderValue, AUTHORIZATION};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

pub(super) const DAV_PATH: &str = "/dav/baidu/VibeAssistant";
const MOUNT: &str = "/baidu";
const FOLDER: &str = "VibeAssistant";
const DRIVER: &str = "BaiduNetdisk";
const RENEW_API: &str = "https://api.oplist.org/baiduyun/renewapi";
const OWNER_PREFIX: &str = "VibeAssistant/openlist-baidu/v1/";
const CONFLICT: &str = "OpenList 的 /baidu 与其他挂载冲突，请检查本机 OpenList 配置。";
const BAD_MOUNT: &str =
    "百度网盘挂载未就绪，请检查网络或重新获取 OpenList 共享应用的刷新令牌后重试。";
const BAD_PAGE: &str = "OpenList 返回的目录或挂载列表不完整，请重试。";
const POLL: Duration = Duration::from_millis(250);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const INIT_WAIT: Duration = Duration::from_secs(120);
const PROVISION_TIMEOUT: Duration = Duration::from_secs(240);
const STILL_PENDING: &str =
    "仍在等待 OpenList 完成百度网盘挂载。请稍后再点「一键部署」，应用会先检查已有挂载。";
const MAX_REPLY: u64 = 16 * 1024 * 1024;

struct Api {
    client: Client,
    base: String,
    authorization: Option<HeaderValue>,
    request_timeout: Duration,
    deadline: Instant,
}

// A missing reply to a mount mutation does not tell us whether OpenList committed it.
enum CallError {
    Failed(String),
    Unconfirmed(String),
}

impl CallError {
    fn message(self) -> String {
        match self {
            Self::Failed(s) | Self::Unconfirmed(s) => s,
        }
    }
}

#[derive(Deserialize)]
struct Envelope {
    code: u16,
    data: Value,
}

// OpenList normally uses HTTP 200 even for bad credentials and failed driver initialization.
// Its message, storage status, reqwest errors and serde errors can all contain secret input.
fn decode<T: DeserializeOwned>(bytes: &[u8], step: &str) -> Result<T, String> {
    let invalid = || format!("{step}失败，OpenList 返回了无效的数据。");
    let reply: Envelope = serde_json::from_slice(bytes).map_err(|_| invalid())?;
    if reply.code != 200 {
        return Err(format!(
            "{step}失败（OpenList {}），请检查配置或稍后重试。",
            reply.code
        ));
    }
    serde_json::from_value(reply.data).map_err(|_| invalid())
}

impl Api {
    // All routes are private constants, except a numeric database ID. A JSON body selects POST.
    fn call<T: DeserializeOwned>(
        &self,
        route: &str,
        body: Option<&Value>,
        step: &'static str,
    ) -> Result<T, String> {
        self.exchange(route, body, step).map_err(CallError::message)
    }

    // Send each mutation once. An ambiguous transport failure is reconciled by GET/list.
    fn mount_change<T: DeserializeOwned>(
        &self,
        route: &str,
        body: &Value,
        step: &'static str,
    ) -> Result<Option<T>, String> {
        match self.exchange(route, Some(body), step) {
            Ok(reply) => Ok(Some(reply)),
            Err(CallError::Unconfirmed(_)) => Ok(None),
            Err(CallError::Failed(reason)) => Err(reason),
        }
    }

    fn exchange<T: DeserializeOwned>(
        &self,
        route: &str,
        body: Option<&Value>,
        step: &'static str,
    ) -> Result<T, CallError> {
        let url = format!("{}{route}", self.base);
        let login_connect_deadline = Instant::now() + Duration::from_secs(5);
        let response = loop {
            let left = self.deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Err(CallError::Failed(format!("{step}超时，请稍后重试。")));
            }
            let mut req = match body {
                Some(body) => self.client.post(&url).json(body),
                None => self.client.get(&url),
            };
            if let Some(auth) = &self.authorization {
                req = req.header(AUTHORIZATION, auth.clone());
            }
            match req.timeout(left.min(self.request_timeout)).send() {
                Ok(response) => break response,
                // Only retry a connection failure during startup. Repeating a rejected login
                // trips the per-IP lockout. A timed-out create/update may already be persisted.
                Err(e)
                    if self.authorization.is_none()
                        && e.is_connect()
                        && Instant::now() < login_connect_deadline =>
                {
                    thread::sleep(POLL)
                }
                Err(e) if e.is_timeout() => {
                    return Err(CallError::Unconfirmed(format!("{step}超时，请稍后重试。")));
                }
                Err(e) if e.is_connect() => {
                    return Err(CallError::Failed(format!(
                        "{step}失败，无法连接本机 OpenList，请重试。"
                    )));
                }
                Err(_) => {
                    return Err(CallError::Unconfirmed(format!(
                        "{step}失败，本机 OpenList 应答中断。"
                    )))
                }
            }
        };
        let status = response.status();
        if !status.is_success() {
            return Err(CallError::Failed(format!(
                "{step}失败（HTTP {}）。",
                status.as_u16()
            )));
        }
        let mut bytes = Vec::new();
        response
            .take(MAX_REPLY + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| {
                CallError::Unconfirmed(format!("{step}失败，无法读取本机 OpenList 的应答。"))
            })?;
        if bytes.len() as u64 > MAX_REPLY {
            return Err(CallError::Failed(format!(
                "{step}失败，OpenList 应答过大。"
            )));
        }
        decode(&bytes, step).map_err(CallError::Failed)
    }

    fn login(&mut self, password: &str) -> Result<(), String> {
        #[derive(Deserialize)]
        struct Login {
            token: String,
        }

        // model.StaticHash, including the original salt retained in OpenList v4.2.9.
        let hash = Sha256::new()
            .chain_update(password.as_bytes())
            .chain_update(b"-https://github.com/alist-org/alist")
            .finalize();
        let body = json!({"username": "admin", "password": format!("{hash:x}")});
        let login: Login = self.call("/api/auth/login/hash", Some(&body), "登录本机 OpenList")?;
        if login.token.is_empty() || !login.token.bytes().all(|b| b.is_ascii_graphic()) {
            return Err("本机 OpenList 返回了无效的登录令牌。".into());
        }
        let mut header = HeaderValue::from_str(&login.token)
            .map_err(|_| "本机 OpenList 返回了无效的登录令牌。")?;
        header.set_sensitive(true);
        self.authorization = Some(header); // OpenList expects the raw JWT in Authorization.
        Ok(())
    }
}

#[derive(Deserialize)]
struct Page<T> {
    total: usize,
    // fs/list returns JSON null for an empty directory. An absent content field is malformed.
    #[serde(
        deserialize_with = "Option::deserialize",
        bound(deserialize = "T: Deserialize<'de>")
    )]
    content: Option<Vec<T>>,
}

impl<T> Page<T> {
    fn checked(self, limit: usize) -> Result<Vec<T>, String> {
        let content = self.content.unwrap_or_default();
        if content.len() != self.total.min(limit) {
            return Err(BAD_PAGE.into());
        }
        Ok(content)
    }
}

#[derive(Serialize, Deserialize, PartialEq)]
struct StoragePolicy {
    cache_expiration: i64,
    custom_cache_policies: String,
    web_proxy: bool,
    webdav_policy: String,
    down_proxy_url: String,
    disable_index: bool,
    seed_policy: String,
}

impl StoragePolicy {
    fn baidu() -> Self {
        Self {
            // v4.2.9 KeyedCache.SetWithTTL expires zero at insertion time. Each device
            // must see writes made through the other devices' independent gateways.
            cache_expiration: 0,
            custom_cache_policies: String::new(),
            web_proxy: true,
            webdav_policy: "native_proxy".into(),
            down_proxy_url: String::new(),
            disable_index: true,
            seed_policy: "off".into(),
        }
    }
}

#[derive(Deserialize)]
struct Storage {
    id: u64,
    mount_path: String,
    driver: String,
    remark: String,
    disabled: bool,
    status: String,
    addition: String,
    #[serde(flatten)]
    policy: StoragePolicy,
}

fn owner_marker(refresh: &str) -> String {
    // OpenList rotates refresh_token in place. Remember which input authorized this mount
    // so retrying with the original input keeps the rotated credentials in the database.
    format!("{OWNER_PREFIX}{:x}", Sha256::digest(refresh.as_bytes()))
}

fn owned_mount(rows: Vec<Storage>) -> Result<Option<Storage>, String> {
    let mut owned = None;
    for row in rows {
        if row.id == 0 || !row.mount_path.starts_with('/') {
            return Err(BAD_PAGE.into());
        }
        if row.mount_path == MOUNT
            && row.driver == DRIVER
            && owned.is_none()
            && row
                .remark
                .strip_prefix(OWNER_PREFIX)
                .is_some_and(|s| s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit()))
        {
            owned = Some(row);
            continue;
        }
        // GetActualMountPath strips the last .balance suffix. fs.List can return virtual
        // children even on a driver error, so an overlapping storage cannot be a safe probe.
        let actual = row
            .mount_path
            .rsplit_once(".balance")
            .map_or(row.mount_path.as_str(), |p| p.0);
        if actual == MOUNT || actual.starts_with("/baidu/") {
            return Err(CONFLICT.into());
        }
    }
    Ok(owned)
}

fn driver_addition(refresh: &str) -> Value {
    json!({
        "root_folder_path": "/", "order_by": "name", "order_direction": "asc",
        "download_api": "official", "use_online_api": true, "api_url_address": RENEW_API,
        "client_id": "", "client_secret": "", "custom_crack_ua": "netdisk",
        // Init unmarshals into an existing Go driver on update. An omitted AccessToken
        // would leave the previous account active after reauthorization.
        "AccessToken": "", "refresh_token": refresh,
        "upload_thread": "3", "upload_timeout": 60, "upload_api": "https://d.pcs.baidu.com",
        "use_dynamic_upload_api": true, "custom_upload_part_size": 0,
        "low_bandwith_upload_mode": false, "only_list_video_file": false
    })
}

impl Storage {
    // Return the desired driver settings and whether this mount already satisfies them.
    fn plan_grant(&self, marker: &str, refresh: &str) -> Result<(Value, bool), String> {
        let mut wanted = driver_addition(refresh);
        if self.remark != marker {
            return Ok((wanted, false));
        }
        let saved: Value = serde_json::from_str(&self.addition).map_err(|_| BAD_MOUNT)?;
        let access = saved["AccessToken"].as_str().ok_or(BAD_MOUNT)?;
        let rotated = saved["refresh_token"].as_str().ok_or(BAD_MOUNT)?;
        wanted["AccessToken"] = json!(access);
        if !rotated.is_empty() {
            wanted["refresh_token"] = json!(rotated);
        }
        let ready = !self.disabled
            && self.status == "work"
            && self.policy == StoragePolicy::baidu()
            && !access.is_empty()
            && !rotated.is_empty()
            && saved == wanted;
        Ok((wanted, ready))
    }
}

fn listed_storage(api: &Api) -> Result<Option<Storage>, String> {
    let rows: Page<Storage> = api.call(
        "/api/admin/storage/list?page=1&per_page=0",
        None,
        "读取本机 OpenList 挂载列表",
    )?;
    owned_mount(rows.checked(usize::MAX)?)
}

// Create inserts a row before contacting Baidu. If the reply was lost, discover
// its ID by the app-owned mount, then read that row until driver Init completes.
fn initialized_storage(
    api: &Api,
    mut id: Option<u64>,
    marker: &str,
    previous_marker: Option<&str>,
    allow_disabled: bool,
) -> Result<Storage, String> {
    let until = api.deadline.min(Instant::now() + INIT_WAIT);
    let mut delay = POLL;
    while Instant::now() < until {
        let row = match id {
            Some(id) => Some(api.call::<Storage>(
                &format!("/api/admin/storage/get?id={id}"),
                None,
                "读取百度网盘挂载",
            )?),
            None => listed_storage(api)?,
        };
        if let Some(row) = row {
            if id.is_some_and(|id| id != row.id) || row.mount_path != MOUNT || row.driver != DRIVER
            {
                return Err(CONFLICT.into());
            }
            id = Some(row.id);
            if row.remark == marker {
                if (row.disabled && allow_disabled) || (!row.disabled && !row.status.is_empty()) {
                    return Ok(row);
                }
            } else if previous_marker != Some(row.remark.as_str()) {
                return Err(CONFLICT.into());
            }
        }
        thread::sleep(delay.min(until.saturating_duration_since(Instant::now())));
        delay = (delay * 2).min(Duration::from_secs(2));
    }
    Err(STILL_PENDING.into())
}

fn ensure_mount(api: &Api, refresh: &str) -> Result<(), String> {
    let marker = owner_marker(refresh);
    let mut body = json!(StoragePolicy::baidu());
    body["mount_path"] = json!(MOUNT);
    body["driver"] = json!(DRIVER);
    body["remark"] = json!(marker);
    body["status"] = json!("");
    let (id, previous_marker) = if let Some(listed) = listed_storage(api)? {
        let id = Some(listed.id);
        let row = initialized_storage(api, id, &listed.remark, None, true)?;
        let (addition, ready) = row.plan_grant(&marker, refresh)?;
        if ready {
            return Ok(());
        }
        body["id"] = json!(row.id);
        body["disabled"] = json!(row.disabled);
        body["addition"] = Value::String(addition.to_string());
        let updated =
            api.mount_change::<()>("/api/admin/storage/update", &body, "更新百度网盘授权")?;
        if row.disabled {
            // Disabled rows don't run Init on update. Check the saved grant before enabling
            // one if we lost the update reply, especially if it was previously another grant.
            if updated.is_none() {
                let saved = initialized_storage(api, id, &marker, Some(&row.remark), true)?;
                if saved.policy != StoragePolicy::baidu()
                    || serde_json::from_str::<Value>(&saved.addition).ok().as_ref()
                        != Some(&addition)
                {
                    return Err(STILL_PENDING.into());
                }
            }
            api.mount_change::<()>(
                &format!("/api/admin/storage/enable?id={}", row.id),
                &json!({}),
                "启用百度网盘挂载",
            )?;
        }
        (id, Some(row.remark))
    } else {
        #[derive(Deserialize)]
        struct Created {
            id: u64,
        }
        body["addition"] = Value::String(driver_addition(refresh).to_string());
        let id = api
            .mount_change::<Created>("/api/admin/storage/create", &body, "创建百度网盘挂载")?
            .map(|reply| reply.id);
        if id == Some(0) {
            return Err(BAD_MOUNT.into());
        }
        (id, None)
    };
    let row = initialized_storage(api, id, &marker, previous_marker.as_deref(), false)?;
    if !row.plan_grant(&marker, refresh)?.1 {
        return Err(BAD_MOUNT.into());
    }
    Ok(())
}

#[derive(Deserialize)]
struct Entry {
    name: String,
    is_dir: bool,
}

fn needs_mkdir(entries: Vec<Entry>) -> Result<bool, String> {
    let mut found = false;
    for entry in entries {
        if entry.name.is_empty() {
            return Err(BAD_PAGE.into());
        }
        if entry.name.eq_ignore_ascii_case(FOLDER) {
            if entry.name != FOLDER || !entry.is_dir || found {
                return Err(
                    "百度网盘根目录已存在与 VibeAssistant 同名的文件或大小写冲突的条目。".into(),
                );
            }
            found = true;
        }
    }
    Ok(!found)
}

fn ensure_folder(api: &Api) -> Result<(), String> {
    let path = format!("{MOUNT}/{FOLDER}");
    // Get on a mount point can succeed without contacting Baidu. Force one complete remote
    // root listing before deciding to make a directory. PageReq.Validate maps 0 to MaxInt.
    let root: Page<Entry> = api.call(
        "/api/fs/list",
        Some(&json!({"path": MOUNT, "refresh": true, "page": 1, "per_page": 0})),
        "读取百度网盘根目录",
    )?;
    if needs_mkdir(root.checked(usize::MAX)?)? {
        api.call::<()>(
            "/api/fs/mkdir",
            Some(&json!({"path": path})),
            "在百度网盘创建 VibeAssistant 目录",
        )?;
    }
    // Verify visibility and access at the exact sync directory, including after mkdir.
    let target: Page<Entry> = api.call(
        "/api/fs/list",
        Some(&json!({"path": path, "refresh": true, "page": 1, "per_page": 1})),
        "验证百度网盘 VibeAssistant 目录",
    )?;
    target.checked(1)?;
    Ok(())
}

pub(super) fn provision(
    port: u16,
    admin_password: &str,
    refresh_token: &str,
) -> Result<(), String> {
    let refresh_token = refresh_token.trim();
    if port == 0 || admin_password.is_empty() || refresh_token.is_empty() {
        return Err("配置本机 OpenList 需要有效端口、管理员密码和百度网盘刷新令牌。".into());
    }
    // Keep another configuration call from switching accounts between our list and mkdir.
    static PROVISIONING: Mutex<()> = Mutex::new(());
    let _guard = match PROVISIONING.try_lock() {
        Ok(guard) => guard,
        Err(TryLockError::Poisoned(poison)) => poison.into_inner(),
        Err(TryLockError::WouldBlock) => return Err("百度网盘正在配置，请稍后重试。".into()),
    };
    let mut api = Api {
        client: Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(2))
            .timeout(REQUEST_TIMEOUT)
            .build()
            .map_err(|_| "无法创建本机 OpenList 客户端。")?,
        base: format!("http://127.0.0.1:{port}"),
        authorization: None,
        request_timeout: REQUEST_TIMEOUT,
        deadline: Instant::now() + PROVISION_TIMEOUT,
    };
    api.login(admin_password)?;
    ensure_mount(&api, refresh_token)?;
    ensure_folder(&api)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Opt-in: downloads the pinned upstream release into a fresh temp directory.
    /// The Local driver exercises the gateway without a Baidu account or user data.
    #[test]
    #[ignore = "requires the OpenList release download and a local listener"]
    fn real_openlist_gateway_roundtrip() -> Result<(), Box<dyn std::error::Error>> {
        use crate::openlist_runtime as runtime;
        use crate::vault_remote::{PutOutcome, Remote, WebDav};
        use std::path::PathBuf;
        struct Stop;
        impl Drop for Stop {
            fn drop(&mut self) {
                runtime::stop();
            }
        }

        let temp_root = std::env::var_os("VIBE_OPENLIST_SMOKE_ROOT")
            .map(PathBuf::from)
            .unwrap_or_else(std::env::temp_dir);
        let scratch = tempfile::Builder::new()
            .prefix("vibe-openlist-test-")
            .tempdir_in(temp_root)?;
        let root = scratch.path();
        let storage = root.join("local-drive");
        std::fs::create_dir(&storage)?;
        let password = uuid::Uuid::new_v4().to_string();
        runtime::install(root)?;
        let _stop = Stop;
        let port = runtime::start(root, &password)?;
        assert_eq!(runtime::start(root, &password)?, port);
        let mut api = Api {
            client: Client::builder()
                .no_proxy()
                .redirect(reqwest::redirect::Policy::none())
                .build()?,
            base: format!("http://127.0.0.1:{port}"),
            authorization: None,
            request_timeout: REQUEST_TIMEOUT,
            deadline: Instant::now() + PROVISION_TIMEOUT,
        };
        api.login(&password)?;
        let mut local = json!(StoragePolicy::baidu());
        local["mount_path"] = json!(MOUNT);
        local["driver"] = json!("Local");
        local["addition"] =
            json!(json!({"root_folder_path": storage, "mkdir_perm": "700"}).to_string());
        api.call::<Value>("/api/admin/storage/create", Some(&local), "创建测试存储")?;
        ensure_folder(&api)?;
        static LEASE: Mutex<()> = Mutex::new(());
        let connect = |port| {
            WebDav::from_gateway(crate::baidu_gateway::Session {
                url: format!("http://127.0.0.1:{port}{DAV_PATH}"),
                password: zeroize::Zeroizing::new(password.clone()),
                lease: LEASE.lock().unwrap(),
            })
        };
        {
            let dav = connect(port)?;
            dav.probe()?;
            assert!(dav.get()?.is_none());
            assert!(matches!(dav.put(b"v1", None)?, PutOutcome::Written(_)));
            let (first, tag) = dav.get()?.ok_or("missing first upload")?;
            assert_eq!(first, b"v1");
            let tag = tag.ok_or("missing DAV version")?;
            assert!(matches!(
                dav.put(b"second version", Some(&tag))?,
                PutOutcome::Written(_)
            ));
            assert!(matches!(
                dav.put(b"stale", Some(&tag))?,
                PutOutcome::Changed
            ));
        }
        runtime::stop();
        let port = runtime::start(root, &password)?;
        let (second, _) = connect(port)?
            .get()?
            .ok_or("missing upload after restart")?;
        assert_eq!(second, b"second version");
        Ok(())
    }

    fn row(path: &str, remark: &str) -> Storage {
        Storage {
            id: 1,
            mount_path: path.into(),
            driver: DRIVER.into(),
            remark: remark.into(),
            disabled: false,
            status: "work".into(),
            addition: String::new(),
            policy: StoragePolicy::baidu(),
        }
    }

    // Withhold a mutation response until teardown; metadata reads still get replies.
    // This reproduces an HTTP timeout after OpenList's INSERT without Baidu credentials.
    fn mount_scenario(
        steps: Vec<(&'static str, Option<Value>)>,
        refresh: &str,
    ) -> Result<(), String> {
        use std::io::{BufRead, BufReader, Write};
        use std::net::TcpListener;
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        listener.set_nonblocking(true).unwrap();
        let deadline = Instant::now() + Duration::from_secs(6);
        let worker = thread::spawn(move || {
            let mut unacknowledged = Vec::new();
            for (expected, reply) in steps {
                let mut stream = loop {
                    match listener.accept() {
                        Ok((stream, _)) => break stream,
                        Err(e)
                            if e.kind() == std::io::ErrorKind::WouldBlock
                                && Instant::now() < deadline =>
                        {
                            thread::sleep(Duration::from_millis(5));
                        }
                        other => panic!("missing {expected}: {other:?}"),
                    }
                };
                // Accepted sockets inherit nonblocking mode on Windows.
                stream.set_nonblocking(false).unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .unwrap();
                let mut reader = BufReader::new(&stream);
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                assert_eq!(
                    line.split_whitespace()
                        .take(2)
                        .collect::<Vec<_>>()
                        .join(" "),
                    expected
                );
                let mut length = 0;
                loop {
                    line.clear();
                    if reader.read_line(&mut line).unwrap() == 0 || line == "\r\n" {
                        break;
                    }
                    if let Some((name, value)) = line.split_once(':') {
                        if name.eq_ignore_ascii_case("content-length") {
                            length = value.trim().parse::<u64>().unwrap();
                        }
                    }
                }
                std::io::copy(&mut reader.take(length), &mut std::io::sink()).unwrap();
                if let Some(data) = reply {
                    let body = json!({"code": 200, "data": data}).to_string();
                    write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
                } else {
                    unacknowledged.push(stream);
                }
            }
        });
        let api = Api {
            client: Client::builder().no_proxy().build().unwrap(),
            base: format!("http://{address}"),
            authorization: Some(HeaderValue::from_static("test")),
            request_timeout: Duration::from_millis(350),
            deadline,
        };
        let result = ensure_mount(&api, refresh);
        worker.join().unwrap();
        result
    }

    fn snapshot(refresh: &str, status: &str) -> Value {
        let mut storage = json!(StoragePolicy::baidu());
        let mut addition = driver_addition(refresh);
        addition["AccessToken"] = json!("test-access");
        storage["id"] = json!(1);
        storage["mount_path"] = json!(MOUNT);
        storage["driver"] = json!(DRIVER);
        storage["remark"] = json!(owner_marker(refresh));
        storage["status"] = json!(status);
        storage["disabled"] = json!(false);
        storage["addition"] = json!(addition.to_string());
        storage
    }

    const LIST: &str = "GET /api/admin/storage/list?page=1&per_page=0";
    const GET: &str = "GET /api/admin/storage/get?id=1";
    const CREATE: &str = "POST /api/admin/storage/create";

    #[test]
    fn lost_create_reply_recovers_the_same_mount_without_a_second_post() {
        let empty = json!({"total": 0, "content": []});
        let pending = snapshot("grant", "");
        let steps = vec![
            (LIST, Some(empty.clone())),
            (CREATE, None),
            (LIST, Some(empty)),
            (LIST, Some(json!({"total": 1, "content": [pending]}))),
            (GET, Some(snapshot("grant", "work"))),
        ];
        mount_scenario(steps, "grant").unwrap();
    }

    #[test]
    fn lost_reauthorization_reply_waits_for_the_new_grant() {
        let previous = snapshot("previous", "work");
        let steps = vec![
            (
                LIST,
                Some(json!({"total": 1, "content": [previous.clone()]})),
            ),
            (GET, Some(previous.clone())),
            ("POST /api/admin/storage/update", None),
            (GET, Some(previous)),
            (GET, Some(snapshot("new", "work"))),
        ];
        mount_scenario(steps, "new").unwrap();
    }

    #[test]
    fn lost_reply_never_adopts_another_grant_or_failed_mount() {
        for (grant, status, expected) in [
            ("other", "work", CONFLICT),
            ("grant", "upstream error", BAD_MOUNT),
        ] {
            let steps = vec![
                (LIST, Some(json!({"total":0,"content":[]}))),
                (CREATE, None),
                (
                    LIST,
                    Some(json!({"total":1,"content":[snapshot(grant, status)]})),
                ),
            ];
            assert_eq!(mount_scenario(steps, "grant").unwrap_err(), expected);
        }
    }

    #[test]
    fn refuses_foreign_mounts_and_virtual_children() {
        let marker = owner_marker("test-seed");
        assert!(owned_mount(vec![row(MOUNT, "user mount")]).is_err());
        for sibling in ["/baidu.balance2", "/baidu/VibeAssistant/another"] {
            assert!(owned_mount(vec![row(MOUNT, &marker), row(sibling, "")]).is_err());
        }
        assert!(owned_mount(vec![row("/another", ""), row(MOUNT, &marker)])
            .unwrap()
            .is_some());
    }

    #[test]
    fn repeats_keep_rotated_tokens_and_new_grants_clear_old_access() {
        let marker = owner_marker("first");
        let mut existing = row(MOUNT, &marker);
        let mut saved = driver_addition("rotated");
        saved["AccessToken"] = json!("access");
        existing.addition = saved.to_string();
        let (repeat, ready) = existing.plan_grant(&marker, "first").unwrap();
        assert!(ready && repeat == saved);
        let (changed, ready) = existing
            .plan_grant(&owner_marker("second"), "second")
            .unwrap();
        assert!(!ready);
        assert_eq!(changed["AccessToken"], "");
        assert_eq!(changed["refresh_token"], "second");
        existing.status = "failed to init".into();
        assert!(!existing.plan_grant(&marker, "first").unwrap().1);
    }

    #[test]
    fn mkdir_requires_a_complete_unambiguous_listing() {
        fn check(bytes: &[u8]) -> Result<bool, String> {
            let page: Page<Entry> = decode(bytes, "list")?;
            needs_mkdir(page.checked(usize::MAX)?)
        }
        assert_eq!(DAV_PATH, format!("/dav{MOUNT}/{FOLDER}"));
        assert_eq!(
            check(br#"{"code":200,"data":{"total":0,"content":null}}"#),
            Ok(true)
        );
        assert_eq!(check(br#"{"code":200,"data":{"total":1,"content":[{"name":"VibeAssistant","is_dir":true}]}}"#), Ok(false));
        for bad in [
            br#"{"code":500,"message":"test-secret","data":null}"#.as_slice(),
            br#"{"code":200,"data":{"total":0}}"#,
            br#"{"code":200,"data":{"total":2,"content":[]}}"#,
            br#"{"code":200,"data":{"total":1,"content":[{"name":"VibeAssistant","is_dir":false}]}}"#,
        ] {
            assert!(!check(bad).unwrap_err().contains("test-secret"));
        }
    }
}
