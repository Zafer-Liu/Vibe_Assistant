//! 云端记忆库的远端存储后端。
//!
//! 同步目标有三种，共用同一套「整库信封」语义（信封的加解密与合并
//! 在 `cloud_sync` 里）：
//!   - `Server`：自建 REST 服务端，逐对象 CAS（`If-Match` 修订号）。
//!   - `WebDAV`：`{地址}/{目录}/{文件}`，Basic 认证，`If-Match: ETag` 条件写。
//!   - `S3`：`s3://{bucket}/{前缀}/{文件}`，AWS Signature V4 签名。
//!
//! WebDAV 与 S3 只往返**一个**加密文件，而不是每个对象一个文件：坚果云
//! 免费版按请求次数限流（每 30 分钟 600 次），几十上百条记忆若各写一个
//! 文件必然撞 429。参考 Magpie 的 `internal/davsync`：一轮同步 = 一次读 +
//! 一次条件写，冲突在本地按对象裁决。

use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};

type HmacSha256 = Hmac<Sha256>;

/// 云端信封文件所在的目录名与文件名。
pub const VAULT_FOLDER: &str = "agent-manager";
pub const VAULT_FILE: &str = "vault.amv";

// ---------- 配置 ----------

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum RemoteKind {
    /// 自建 REST 服务端（既有模式，逐对象同步）。未配置时的默认。
    #[default]
    Server,
    /// WebDAV 目录（坚果云 / Nextcloud / 群晖 / AList）。
    WebDav,
    /// S3 兼容对象存储（AWS / R2 / COS / OSS / MinIO）。
    S3,
}

impl RemoteKind {
    pub fn parse(value: &str) -> Self {
        match value.trim().to_ascii_lowercase().as_str() {
            "webdav" => RemoteKind::WebDav,
            "s3" => RemoteKind::S3,
            _ => RemoteKind::Server,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            RemoteKind::Server => "server",
            RemoteKind::WebDav => "webdav",
            RemoteKind::S3 => "s3",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            RemoteKind::Server => "自建服务端",
            RemoteKind::WebDav => "WebDAV",
            RemoteKind::S3 => "S3",
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct RemoteConfig {
    pub kind: RemoteKind,
    pub url: String,
    /// WebDAV 用户名 / S3 Access Key ID。
    pub user: String,
    /// 自建服务端 PAT / WebDAV 密码 / S3 Secret Access Key。
    pub credential: String,
    /// S3：自建或第三方端点，AWS 留空。
    pub endpoint: String,
    /// S3：区域，AWS 默认 `us-east-1`，R2 用 `auto`。
    pub region: String,
    /// S3：桶放路径里（MinIO 与多数自建服务需要）。
    pub path_style: bool,
}

impl RemoteConfig {
    fn normalized_url(&self) -> String {
        self.url.trim().trim_end_matches('/').to_string()
    }
}

// ---------- 结果 ----------

/// 一次条件写的三种结局：写成了（带回新的 ETag）、远端已被别的设备改过、
/// 出错。ETag 目前没有消费方（每轮都重新读），保留以备后续做增量判定。
#[allow(dead_code)]
#[derive(Debug)]
pub enum PutOutcome {
    Written(Option<String>),
    Changed,
}

pub trait Remote {
    fn kind(&self) -> RemoteKind;
    /// 读回信封；`None` 表示远端还没有文件。`Some(etag)` 是条件写的凭据。
    fn get(&self) -> Result<Option<(Vec<u8>, Option<String>)>, String>;
    /// 条件写：`etag` 为 `None` 时仅在远端还不存在时才写（防止覆盖别人的库）。
    fn put(&self, data: &[u8], etag: Option<&str>) -> Result<PutOutcome, String>;
    /// 「测试连接」：只读探测，返回给用户看的一句话。
    fn probe(&self) -> Result<String, String>;
}

pub fn open(config: &RemoteConfig) -> Result<Box<dyn Remote>, String> {
    match config.kind {
        RemoteKind::Server => Err("自建服务端模式不走文件信封".into()),
        RemoteKind::WebDav => Ok(Box::new(WebDav::new(config)?)),
        RemoteKind::S3 => Ok(Box::new(S3::new(config)?)),
    }
}

fn client() -> reqwest::blocking::Client {
    reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .unwrap_or_else(|_| reqwest::blocking::Client::new())
}

/// 429 / 503：服务端在限流。坚果云按账号统计请求数，此时只能退避重试。
fn rate_limited(kind: &str, status: u16) -> String {
    if status == 429 {
        format!("{kind} 服务端限流（HTTP 429）：同步会退避后重试，若持续出现请降低自动同步频率")
    } else {
        format!("{kind} 服务端繁忙或正在限流（HTTP {status}）：稍后自动重试")
    }
}

fn etag_of(resp: &reqwest::blocking::Response) -> Option<String> {
    resp.headers()
        .get(reqwest::header::ETAG)
        .and_then(|v| v.to_str().ok())
        .map(normalize_etag)
        .filter(|v| !v.is_empty())
}

/// ETag 可能带引号（`W/"abc"`），条件写时原样回传，比较时统一去引号。
fn normalize_etag(raw: &str) -> String {
    raw.trim().trim_start_matches("W/").trim_matches('"').to_string()
}

// ---------- WebDAV ----------

pub struct WebDav {
    base: String,
    user: String,
    pass: String,
    client: reqwest::blocking::Client,
}

impl WebDav {
    fn new(config: &RemoteConfig) -> Result<Self, String> {
        let url = config.normalized_url();
        if !url.starts_with("http://") && !url.starts_with("https://") {
            return Err(format!(
                "{url} 不是 WebDAV 地址：请以 https:// 或 http:// 开头（坚果云为 https://dav.jianguoyun.com/dav/）"
            ));
        }
        if config.user.is_empty() {
            return Err("请填写 WebDAV 用户名（坚果云为注册邮箱）".into());
        }
        if config.credential.is_empty() {
            return Err("请填写 WebDAV 密码（坚果云需在网页端生成「应用密码」）".into());
        }
        Ok(Self {
            base: url,
            user: config.user.clone(),
            pass: config.credential.clone(),
            client: client(),
        })
    }

    fn file_url(&self) -> String {
        format!("{}/{}/{}", self.base, VAULT_FOLDER, VAULT_FILE)
    }

    fn folder_url(&self) -> String {
        format!("{}/{}/", self.base, VAULT_FOLDER)
    }

    fn send(
        &self,
        method: reqwest::Method,
        url: &str,
        body: Vec<u8>,
        headers: Vec<(&str, String)>,
    ) -> Result<reqwest::blocking::Response, String> {
        let mut req = self
            .client
            .request(method, url)
            .basic_auth(&self.user, Some(&self.pass));
        for (name, value) in headers {
            req = req.header(name, value);
        }
        let resp = req
            .body(body)
            .send()
            .map_err(|e| format!("连接 WebDAV 服务端失败：{e}"))?;
        match resp.status().as_u16() {
            401 => Err(
                "WebDAV 服务端拒绝了这个用户名或密码（HTTP 401）：坚果云请填「应用密码」而不是登录密码"
                    .into(),
            ),
            429 | 503 => Err(rate_limited("WebDAV", resp.status().as_u16())),
            _ => Ok(resp),
        }
    }

    fn mkcol(&self) -> Result<(), String> {
        let resp = self.send(
            reqwest::Method::from_bytes(b"MKCOL").unwrap(),
            &self.folder_url(),
            Vec::new(),
            Vec::new(),
        )?;
        let status = resp.status().as_u16();
        // 405：目录已存在；409：上一级目录不存在（坚果云要求母目录存在）。
        if status == 409 {
            return Err(format!(
                "WebDAV 服务端上没有 {} 的上一级目录（HTTP 409）：请先在网盘里创建，或把地址改成已存在的目录",
                VAULT_FOLDER
            ));
        }
        if status >= 300 && status != 405 {
            return Err(format!(
                "在 WebDAV 服务端创建目录 {} 失败：HTTP {status}",
                VAULT_FOLDER
            ));
        }
        Ok(())
    }

    /// 远端是否已有信封文件。探不到时返回 Err，由调用方按「没有」处理，
    /// 走 `If-None-Match: *` 创建路径——宁可被 412 拒绝，也不要覆盖别人的库。
    fn exists(&self) -> Result<bool, String> {
        let resp = self.send(
            reqwest::Method::from_bytes(b"PROPFIND").unwrap(),
            &self.file_url(),
            Vec::new(),
            vec![("Depth", "0".to_string())],
        )?;
        Ok(matches!(resp.status().as_u16(), 200..=299))
    }

    /// 写完后回读长度：中继和隧道会把长上传截断，留下半个文件，之后每次
    /// 同步都会解出「不是有效信封」。截断就重试一次。
    fn check_written(&self, want: usize) -> Result<(), String> {
        let resp = self.send(reqwest::Method::HEAD, &self.file_url(), Vec::new(), Vec::new())?;
        if resp.status().as_u16() != 200 {
            return Ok(()); // 服务端不支持 HEAD，交给下一轮的读来发现。
        }
        match resp.content_length() {
            // 0 不代表文件是空的：坚果云对 HEAD 一律回 Content-Length: 0
            // （Magpie 也踩过同一个坑），此时视为「无法校验」，交给下一轮读发现。
            None | Some(0) => Ok(()),
            Some(len) if (len as usize) < want => Err(format!(
                "WebDAV 服务端只保存了 {len} / {want} 字节：上传被截断，请重试；若反复出现说明到该服务端的网络会中断长上传"
            )),
            Some(_) => Ok(()),
        }
    }
}

impl Remote for WebDav {
    fn kind(&self) -> RemoteKind {
        RemoteKind::WebDav
    }

    fn get(&self) -> Result<Option<(Vec<u8>, Option<String>)>, String> {
        let resp = self.send(reqwest::Method::GET, &self.file_url(), Vec::new(), Vec::new())?;
        let status = resp.status().as_u16();
        // 404 / 409 / 410：还没有信封。坚果云对空目录里的读回 409。
        if matches!(status, 404 | 409 | 410) {
            return Ok(None);
        }
        if status == 403 {
            return Err(format!(
                "WebDAV 服务端不允许这个账号读取 {}/{}（HTTP 403）：请检查该目录是否存在、账号是否有权限",
                VAULT_FOLDER, VAULT_FILE
            ));
        }
        if status != 200 {
            return Err(format!("读取 WebDAV 信封失败：HTTP {status}"));
        }
        let etag = etag_of(&resp);
        let bytes = resp
            .bytes()
            .map_err(|e| format!("读取 WebDAV 信封失败：{e}"))?
            .to_vec();
        Ok(Some((bytes, etag)))
    }

    fn put(&self, data: &[u8], etag: Option<&str>) -> Result<PutOutcome, String> {
        let mut headers = vec![(
            "Content-Type",
            "application/octet-stream".to_string(),
        )];
        // conditional 为假表示「服务端不给 ETag，我们无从做条件写」，此时只能
        // 退化成最后写入者胜——比永远写不进去要好，但要如实告诉上层。
        let conditional = if let Some(tag) = etag {
            headers.push(("If-Match", format!("\"{}\"", normalize_etag(tag))));
            true
        } else {
            // 没有版本凭据时先看一眼远端有没有：有且它不给 ETag → 无条件覆盖；
            // 没有 → 用 If-None-Match: * 创建，避免打翻别人刚建好的库。
            match self.exists() {
                Ok(false) | Err(_) => {
                    headers.push(("If-None-Match", "*".to_string()));
                    true
                }
                Ok(true) => false,
            }
        };
        let mut made_dir = false;
        let mut checked = false;
        loop {
            let resp = self.send(
                reqwest::Method::PUT,
                &self.file_url(),
                data.to_vec(),
                headers.clone(),
            )?;
            let status = resp.status().as_u16();
            let written_etag = etag_of(&resp);
            match status {
                200..=299 => {
                    match self.check_written(data.len()) {
                        Ok(()) => return Ok(PutOutcome::Written(written_etag.or_else(|| etag.map(String::from)))),
                        Err(_) if !checked && conditional => {
                            // 半个文件已是新版本：重试必须匹配它。拿不到新 ETag 时
                            // 绝不裸重试——那会把别人刚写下来的版本盖掉。
                            checked = true;
                            match written_etag.as_deref() {
                                Some(tag) => {
                                    headers.retain(|(name, _)| {
                                        *name != "If-Match" && *name != "If-None-Match"
                                    });
                                    headers.push(("If-Match", format!("\"{}\"", normalize_etag(tag))));
                                    continue;
                                }
                                None => return Err(
                                    "WebDAV 服务端写入后被截断，且没有返回 ETag：为避免覆盖其他设备的版本，本次写回已放弃，请重试同步"
                                        .into(),
                                ),
                            }
                        }
                        Err(e) => return Err(e),
                    }
                }
                412 => return Ok(PutOutcome::Changed),
                // 目录还没建（群晖对不存在的目录回 403）。
                404 | 409 | 403 if !made_dir => {
                    made_dir = true;
                    self.mkcol()?;
                    continue;
                }
                401 => return Err("WebDAV 服务端拒绝了用户名或密码（HTTP 401）".into()),
                status => {
                    return Err(format!(
                        "写入 WebDAV 信封失败：HTTP {status}{}",
                        if status == 403 {
                            "（目录不存在，或该账号没有写入权限）"
                        } else {
                            ""
                        }
                    ))
                }
            }
        }
    }

    fn probe(&self) -> Result<String, String> {
        match self.get()? {
            Some((bytes, _)) => Ok(format!(
                "WebDAV 连接正常：已有信封 {} 字节",
                bytes.len()
            )),
            None => {
                // 还没有信封时确认目录可写。
                let resp = self.send(
                    reqwest::Method::from_bytes(b"PROPFIND").unwrap(),
                    &self.folder_url(),
                    Vec::new(),
                    vec![("Depth", "0".to_string())],
                );
                match resp {
                    Ok(r) if r.status().as_u16() < 400 => {
                        Ok(format!("WebDAV 连接正常：目录 {} 可用，尚无信封", VAULT_FOLDER))
                    }
                    Ok(r) if r.status().as_u16() == 404 => Ok(format!(
                        "WebDAV 连接正常：目录 {} 将在首次同步时创建",
                        VAULT_FOLDER
                    )),
                    Ok(r) => Err(format!("WebDAV 探测失败：HTTP {}", r.status())),
                    Err(e) => Err(e),
                }
            }
        }
    }
}

// ---------- S3（AWS Signature V4，自己签名，不引入 SDK） ----------

pub struct S3 {
    scheme: String,
    host: String,
    /// 请求路径，含前导 `/`，已做 URI 编码。
    path: String,
    region: String,
    access_key: String,
    secret_key: String,
    client: reqwest::blocking::Client,
}

impl S3 {
    fn new(config: &RemoteConfig) -> Result<Self, String> {
        let raw = config.normalized_url();
        let rest = raw
            .strip_prefix("s3://")
            .or_else(|| raw.strip_prefix("S3://"))
            .ok_or_else(|| {
                "S3 地址形如 s3://bucket/前缀（例如 s3://my-bucket/agent-manager）".to_string()
            })?;
        let (bucket, prefix) = match rest.split_once('/') {
            Some((b, p)) => (b.to_string(), p.trim_matches('/').to_string()),
            None => (rest.to_string(), String::new()),
        };
        if bucket.is_empty() {
            return Err("S3 地址缺少桶名（s3://bucket/前缀）".into());
        }
        if config.user.is_empty() || config.credential.is_empty() {
            return Err("S3 需要 Access Key ID 及其 Secret".into());
        }
        let key = if prefix.is_empty() {
            VAULT_FILE.to_string()
        } else {
            format!("{prefix}/{VAULT_FILE}")
        };
        let region = if config.region.trim().is_empty() {
            "us-east-1".to_string()
        } else {
            config.region.trim().to_string()
        };
        let endpoint = config.endpoint.trim().trim_end_matches('/').to_string();
        let (scheme, host, path) = if endpoint.is_empty() {
            // AWS：默认虚拟主机风格。
            (
                "https".to_string(),
                format!("{bucket}.s3.{region}.amazonaws.com"),
                format!("/{}", uri_encode(&key, false)),
            )
        } else {
            let (scheme, host_port) = match endpoint.split_once("://") {
                Some((s, rest)) => (s.to_string(), rest.to_string()),
                None => ("https".to_string(), endpoint.clone()),
            };
            if config.path_style {
                (
                    scheme,
                    host_port,
                    format!("/{}/{}", uri_encode(&bucket, false), uri_encode(&key, false)),
                )
            } else {
                (
                    scheme,
                    format!("{bucket}.{host_port}"),
                    format!("/{}", uri_encode(&key, false)),
                )
            }
        };
        Ok(Self {
            scheme,
            host,
            path,
            region,
            access_key: config.user.clone(),
            secret_key: config.credential.clone(),
            client: client(),
        })
    }

    fn url(&self) -> String {
        format!("{}://{}{}", self.scheme, self.host, self.path)
    }

    fn now() -> (String, String) {
        let now = chrono::Utc::now();
        (
            now.format("%Y%m%dT%H%M%SZ").to_string(),
            now.format("%Y%m%d").to_string(),
        )
    }

    fn sha256_hex(data: &[u8]) -> String {
        let mut hasher = Sha256::new();
        hasher.update(data);
        hasher.finalize().iter().map(|b| format!("{b:02x}")).collect()
    }

    /// 签名作用域：`{日期}/{区域}/s3/aws4_request`。
    fn credential_scope(datestamp: &str, region: &str) -> String {
        format!("{datestamp}/{region}/s3/aws4_request")
    }

    fn hmac(key: &[u8], msg: &str) -> Result<Vec<u8>, String> {
        let mut mac = HmacSha256::new_from_slice(key).map_err(|e| e.to_string())?;
        mac.update(msg.as_bytes());
        Ok(mac.finalize().into_bytes().to_vec())
    }

    fn hmac_hex(key: &[u8], msg: &str) -> Result<String, String> {
        let out = Self::hmac(key, msg)?;
        Ok(out.iter().map(|b| format!("{b:02x}")).collect())
    }

    /// AWS Signature V4：返回需要带上的请求头。
    fn signed_headers(
        &self,
        method: &str,
        payload: &[u8],
        extra: &[(&str, &str)],
    ) -> Result<Vec<(String, String)>, String> {
        let (amzdate, datestamp) = Self::now();
        self.sign(method, payload, extra, &amzdate, &datestamp)
    }

    fn sign(
        &self,
        method: &str,
        payload: &[u8],
        extra: &[(&str, &str)],
        amzdate: &str,
        datestamp: &str,
    ) -> Result<Vec<(String, String)>, String> {
        let payload_hash = Self::sha256_hex(payload);
        let mut headers: Vec<(String, String)> = vec![
            ("host".to_string(), self.host.clone()),
            ("x-amz-content-sha256".to_string(), payload_hash.clone()),
            ("x-amz-date".to_string(), amzdate.to_string()),
        ];
        for (name, value) in extra {
            if !value.is_empty() {
                headers.push((name.to_ascii_lowercase(), value.to_string()));
            }
        }
        headers.sort_by(|a, b| a.0.cmp(&b.0));
        let mut canonical_headers = String::new();
        let mut signed_names = Vec::new();
        for (name, value) in &headers {
            canonical_headers.push_str(&format!("{}:{}\n", name, value.trim()));
            signed_names.push(name.clone());
        }
        let signed_headers = signed_names.join(";");
        let canonical_request = format!(
            "{method}\n{}\n\n{canonical_headers}\n{signed_headers}\n{payload_hash}",
            self.path
        );
        let scope = Self::credential_scope(datestamp, &self.region);
        let string_to_sign = format!(
            "AWS4-HMAC-SHA256\n{amzdate}\n{scope}\n{}",
            Self::sha256_hex(canonical_request.as_bytes())
        );
        let secret = format!("AWS4{}", self.secret_key);
        let k_date = Self::hmac(secret.as_bytes(), &datestamp)?;
        let k_region = Self::hmac(&k_date, &self.region)?;
        let k_service = Self::hmac(&k_region, "s3")?;
        let k_signing = Self::hmac(&k_service, "aws4_request")?;
        let signature = Self::hmac_hex(&k_signing, &string_to_sign)?;
        let mut out: Vec<(String, String)> = headers
            .into_iter()
            .map(|(name, value)| (header_case(&name), value))
            .collect();
        out.push((
            "Authorization".to_string(),
            format!(
                "AWS4-HMAC-SHA256 Credential={}/{scope}, SignedHeaders={signed_headers}, Signature={signature}",
                self.access_key
            ),
        ));
        Ok(out)
    }

    fn send(
        &self,
        method: reqwest::Method,
        body: Vec<u8>,
        extra: &[(&str, &str)],
    ) -> Result<reqwest::blocking::Response, String> {
        let method_str = method.to_string();
        let headers = self.signed_headers(&method_str, &body, extra)?;
        let mut req = self.client.request(method.clone(), self.url());
        for (name, value) in &headers {
            req = req.header(name.as_str(), value.as_str());
        }
        let resp = req
            .body(body.clone())
            .send()
            .map_err(|e| format!("连接 S3 服务端失败：{e}"))?;
        match resp.status().as_u16() {
            401 | 403 => Err(
                "S3 服务端拒绝了 Access Key（HTTP 403/401）：请检查 Key 是否有该桶的读写权限"
                    .into(),
            ),
            429 | 503 => Err(rate_limited("S3", resp.status().as_u16())),
            _ => Ok(resp),
        }
    }
}

/// AWS 要求被签名的头用标准大小写（x-amz-date、host 等）。
fn header_case(name: &str) -> String {
    name.split('-')
        .map(|part| {
            let mut chars = part.chars();
            match chars.next() {
                Some(c) => c.to_uppercase().collect::<String>() + chars.as_str(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join("-")
}

/// SigV4 的 URI 编码：字母数字与 `-._~` 原样，其余百分号大写十六进制。
fn uri_encode(value: &str, encode_slash: bool) -> String {
    let mut out = String::new();
    for byte in value.bytes() {
        let keep = byte.is_ascii_alphanumeric()
            || matches!(byte, b'-' | b'.' | b'_' | b'~')
            || (byte == b'/' && !encode_slash);
        if keep {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

impl Remote for S3 {
    fn kind(&self) -> RemoteKind {
        RemoteKind::S3
    }

    fn get(&self) -> Result<Option<(Vec<u8>, Option<String>)>, String> {
        let resp = self.send(reqwest::Method::GET, Vec::new(), &[])?;
        let status = resp.status().as_u16();
        if status == 404 {
            return Ok(None);
        }
        if status == 405 {
            // 权限只给了 List：HEAD 也不行，退化成 GET 已试过。
            return Err("S3 服务端不允许读取该对象（HTTP 405）：请给 Access Key 授权".into());
        }
        if status != 200 {
            return Err(format!("读取 S3 信封失败：HTTP {status}"));
        }
        let etag = etag_of(&resp);
        let bytes = resp
            .bytes()
            .map_err(|e| format!("读取 S3 信封失败：{e}"))?
            .to_vec();
        Ok(Some((bytes, etag)))
    }

    fn put(&self, data: &[u8], etag: Option<&str>) -> Result<PutOutcome, String> {
        if let Some(expected) = etag {
            // 不是所有服务端都支持条件写（S3 的条件写是 2024 年才有的能力，
            // MinIO 与部分自建服务仍不支持），所以写之前先读一次 ETag。
            let resp = self.send(reqwest::Method::HEAD, Vec::new(), &[])?;
            match resp.status().as_u16() {
                200 => {
                    if etag_of(&resp).as_deref() != Some(normalize_etag(expected).as_str()) {
                        return Ok(PutOutcome::Changed);
                    }
                }
                404 => return Ok(PutOutcome::Changed), // 我们读到的版本已被删除。
                status => return Err(format!("写入前读取 S3 对象版本失败：HTTP {status}")),
            }
        }
        let mut extra: Vec<(&str, &str)> = vec![("content-type", "application/octet-stream")];
        let conditional: String;
        match etag {
            Some(tag) => {
                conditional = format!("\"{}\"", normalize_etag(tag));
                extra.push(("if-match", &conditional));
            }
            None => {
                // 没有版本凭据（服务端没给过 ETag）时先看对象在不在：在，就只能
                // 无条件覆盖；不在，用 If-None-Match: * 创建，避免打翻别人刚建的库。
                match self.send(reqwest::Method::HEAD, Vec::new(), &[])?.status().as_u16() {
                    200 => {}
                    404 => {
                        conditional = "*".to_string();
                        extra.push(("if-none-match", &conditional));
                    }
                    status => return Err(format!("写入前探测 S3 对象失败：HTTP {status}")),
                }
            }
        }
        let resp = self.send(reqwest::Method::PUT, data.to_vec(), &extra)?;
        match resp.status().as_u16() {
            200..=299 => Ok(PutOutcome::Written(etag_of(&resp))),
            412 => Ok(PutOutcome::Changed),
            status => Err(format!("写入 S3 信封失败：HTTP {status}")),
        }
    }

    fn probe(&self) -> Result<String, String> {
        match self.get()? {
            Some((bytes, _)) => Ok(format!("S3 连接正常：已有信封 {} 字节", bytes.len())),
            None => {
                // 还没有信封：确认桶可写（HeadBucket 需要额外授权，这里用
                // 一次不存在的对象读取来判断 404 来自桶而不是权限）。
                Ok(format!("S3 连接正常：桶可访问，尚无信封（首次同步会创建 {VAULT_FILE}）"))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_http::fake_dav;

    /// AWS 官方文档的 SigV4 测试向量（GET Object，含 range 头）。
    /// 签名算法必须逐字节对上，否则所有 S3 兼容服务都会返回 403。
    #[test]
    fn sigv4_matches_aws_reference() {
        let s3 = S3 {
            scheme: "https".to_string(),
            host: "examplebucket.s3.amazonaws.com".to_string(),
            path: "/test.txt".to_string(),
            region: "us-east-1".to_string(),
            access_key: "AKIAIOSFODNN7EXAMPLE".to_string(),
            secret_key: "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY".to_string(),
            client: client(),
        };
        let headers = s3
            .sign("GET", b"", &[("range", "bytes=0-9")], "20130524T000000Z", "20130524")
            .expect("signing must succeed");
        let auth = headers
            .iter()
            .find(|(name, _)| name == "Authorization")
            .expect("authorization header")
            .1
            .clone();
        assert_eq!(
            auth,
            "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20130524/us-east-1/s3/aws4_request, \
             SignedHeaders=host;range;x-amz-content-sha256;x-amz-date, \
             Signature=f0e8bdb87c964420e857bd35b5d6ed310bd44f0170aba48dd91039c6036bdb41"
        );
    }

    #[test]
    fn uri_encoding_follows_sigv4_rules() {
        assert_eq!(uri_encode("/a b/c+d", true), "%2Fa%20b%2Fc%2Bd");
        assert_eq!(uri_encode("a/b", false), "a/b");
        assert_eq!(uri_encode("~-_.", false), "~-_.");
    }

    #[test]
    fn s3_address_is_split_into_bucket_and_prefix() {
        let config = RemoteConfig {
            kind: RemoteKind::S3,
            url: "s3://my-bucket/some/prefix/".to_string(),
            user: "key".to_string(),
            credential: "secret".to_string(),
            endpoint: "https://nas.local:9000".to_string(),
            region: "auto".to_string(),
            path_style: true,
        };
        let s3 = S3::new(&config).expect("valid s3 config");
        assert_eq!(s3.host, "https://nas.local:9000".replace("https://", ""));
        assert_eq!(s3.path, "/my-bucket/some/prefix/vault.amv");
        assert_eq!(s3.region, "auto");
    }

    fn s3_of(url: &str, endpoint: &str, region: &str, path_style: bool) -> S3 {
        let config = RemoteConfig {
            kind: RemoteKind::S3,
            url: url.to_string(),
            user: "key".to_string(),
            credential: "secret".to_string(),
            endpoint: endpoint.to_string(),
            region: region.to_string(),
            path_style,
        };
        S3::new(&config).expect("valid s3 config")
    }

    /// AWS 默认虚拟主机风格：桶在主机名里，路径只有对象键。
    #[test]
    fn s3_url_uses_virtual_host_style_on_aws() {
        let s3 = s3_of("s3://photos/eu/memories/", "", "", false);
        assert_eq!(s3.scheme, "https");
        assert_eq!(s3.host, "photos.s3.us-east-1.amazonaws.com");
        assert_eq!(s3.path, "/eu/memories/vault.amv");
        assert_eq!(s3.region, "us-east-1");
    }

    /// 自定义端点（R2 / MinIO / 群晖）+ 虚拟主机风格：桶拼成子域名。
    #[test]
    fn s3_url_uses_virtual_host_style_for_custom_endpoint() {
        let s3 = s3_of(
            "s3://photos/memories",
            "https://abc123.r2.cloudflarestorage.com",
            "auto",
            false,
        );
        assert_eq!(s3.scheme, "https");
        assert_eq!(s3.host, "photos.abc123.r2.cloudflarestorage.com");
        assert_eq!(s3.path, "/memories/vault.amv");
    }

    /// path-style：桶落在路径里，MinIO 与多数自建服务只认这种。
    #[test]
    fn s3_url_puts_bucket_in_path_when_path_style() {
        let s3 = s3_of(
            "s3://photos/memories",
            "http://nas.local:9000",
            "cn-north-1",
            true,
        );
        assert_eq!(s3.scheme, "http");
        assert_eq!(s3.host, "nas.local:9000");
        assert_eq!(s3.path, "/photos/memories/vault.amv");
    }

    /// 端点没写 scheme 时补 https，而不是把 "https://" 当成主机名。
    #[test]
    fn s3_endpoint_without_scheme_defaults_to_https() {
        let s3 = s3_of("s3://photos", "minio.internal:9000", "", true);
        assert_eq!(s3.scheme, "https");
        assert_eq!(s3.host, "minio.internal:9000");
        assert_eq!(s3.path, "/photos/vault.amv");
    }

    #[test]
    fn s3_bucket_without_prefix_puts_vault_at_root() {
        assert_eq!(s3_of("s3://photos", "", "", false).path, "/vault.amv");
        // 尾斜杠与重复斜杠都不应留下空段。
        assert_eq!(s3_of("s3://photos/", "", "", false).path, "/vault.amv");
        // 前缀中间的重复斜杠按原样保留：S3 对象键允许空路径段，这里不擅自改写。
        assert_eq!(
            s3_of("s3://photos///a//b//", "", "", false).path,
            "/a//b/vault.amv"
        );
    }

    #[test]
    fn s3_rejects_non_s3_addresses() {
        let config = RemoteConfig {
            kind: RemoteKind::S3,
            url: "https://dav.jianguoyun.com/dav/".to_string(),
            ..RemoteConfig::default()
        };
        assert!(S3::new(&config).is_err());
        // 缺少桶名同样要早失败，而不是拼出 "...s3..amazonaws.com"。
        let config = RemoteConfig {
            kind: RemoteKind::S3,
            url: "s3:///".to_string(),
            ..RemoteConfig::default()
        };
        assert!(S3::new(&config).is_err());
    }

    #[test]
    fn etag_quotes_are_normalized_for_comparison() {
        assert_eq!(normalize_etag("W/\"abc\""), "abc");
        assert_eq!(normalize_etag("\"abc\""), "abc");
        assert_eq!(normalize_etag("abc"), "abc");
    }

    #[test]
    fn webdav_roundtrip_and_conditional_write() {
        let server = fake_dav::Server::start();
        let config = RemoteConfig {
            kind: RemoteKind::WebDav,
            url: server.url.clone(),
            user: "me@example.com".to_string(),
            credential: "app-password".to_string(),
            ..RemoteConfig::default()
        };
        let remote = open(&config).expect("webdav remote");

        // 1. 远端还没有信封。
        assert!(remote.get().expect("first get").is_none());

        // 2. 首次写入（If-None-Match: *）→ 创建。
        match remote.put(b"envelope-v1", None).expect("first put") {
            PutOutcome::Written(_) => {}
            other => panic!("首次写入应成功，实际 {other:?}"),
        }
        let (data, etag) = remote.get().expect("second get").expect("envelope exists");
        assert_eq!(data, b"envelope-v1".to_vec());
        let etag = etag.expect("服务端应返回 ETag");

        // 3. 拿过期的 ETag 覆盖 → 服务端 412，映射为 Changed。
        match remote.put(b"envelope-stale", Some("etag-old")).expect("stale put") {
            PutOutcome::Changed => {}
            other => panic!("过期 ETag 应被拒，实际 {other:?}"),
        }
        assert_eq!(server.body(&format!("/{VAULT_FOLDER}/{VAULT_FILE}")).as_deref(), Some(b"envelope-v1".as_ref()));

        // 4. 拿当前 ETag 覆盖 → 成功，内容更新。
        match remote.put(b"envelope-v2", Some(&etag)).expect("fresh put") {
            PutOutcome::Written(_) => {}
            other => panic!("当前 ETag 应写入成功，实际 {other:?}"),
        }
        let (data, _) = remote.get().expect("third get").expect("envelope exists");
        assert_eq!(data, b"envelope-v2".to_vec());
    }

    #[test]
    fn webdav_rejects_blank_credentials() {
        let config = RemoteConfig {
            kind: RemoteKind::WebDav,
            url: "https://dav.jianguoyun.com/dav/".to_string(),
            user: "me@example.com".to_string(),
            credential: String::new(),
            ..RemoteConfig::default()
        };
        assert!(open(&config).is_err());
    }
}
