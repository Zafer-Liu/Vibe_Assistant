//! 本机网络信息：网卡接口列表、实时吞吐采样、公网出口 IP 与带宽测速。
//!
//! 与端口管理（ports.rs）互补：端口管理看监听端口，这里看网卡与流量。
//! 吞吐采样复用同一个全局 `Networks` 实例——sysinfo 的
//! `received()/transmitted()` 语义是“距上次 refresh 的增量”，
//! 配合两次调用之间的真实间隔即可得到实时速率。

use futures_util::StreamExt;
use serde::Serialize;
use std::net::IpAddr;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use sysinfo::Networks;
use tauri::Emitter;

/// 距上次采样超过该阈值（页面隐藏后恢复的第一轮）时返回 0 速率，
/// 避免把长时间间隔平均出来的假速率当成实时速度展示。
const STALE_SAMPLE_MS: u64 = 30_000;

#[derive(Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub struct NetworkInterface {
    pub name: String,
    pub mac: Option<String>,
    /// 0 表示平台未提供。
    pub mtu: u64,
    /// 形如 "192.168.1.10/24" 的 CIDR 列表。
    pub ipv4: Vec<String>,
    pub ipv6: Vec<String>,
    pub is_loopback: bool,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub struct InterfaceThroughput {
    pub name: String,
    pub is_loopback: bool,
    pub rx_bytes_per_sec: f64,
    pub tx_bytes_per_sec: f64,
    pub rx_total_bytes: u64,
    pub tx_total_bytes: u64,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub struct ThroughputSnapshot {
    pub elapsed_ms: u64,
    pub interfaces: Vec<InterfaceThroughput>,
    /// 仅统计非回环接口，回环流量不代表真实网络使用。
    pub total_rx_bytes_per_sec: f64,
    pub total_tx_bytes_per_sec: f64,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub struct ProxyIp {
    /// 检测到的代理服务器地址（host:port 或带协议原样展示）。
    pub server: String,
    pub ipv4: Option<String>,
    pub ipv6: Option<String>,
}

#[derive(Clone, Default, Serialize)]
#[serde(rename_all = "snake_case")]
pub struct PublicIp {
    pub ipv4: Option<String>,
    pub ipv6: Option<String>,
    /// 检测到系统代理时的出口 IP；未配置代理为 None。
    pub proxy: Option<ProxyIp>,
}

struct NetworkSampler {
    networks: Networks,
    sampled_at: Instant,
}

static SAMPLER: Mutex<Option<NetworkSampler>> = Mutex::new(None);

fn is_loopback_interface(name: &str) -> bool {
    let lowered = name.trim().to_lowercase();
    lowered == "lo"
        || lowered.starts_with("lo@")
        || lowered == "lo0"
        || lowered.contains("loopback")
}

/// 列出本机网卡接口：名称、MAC、MTU 与 IPv4/IPv6 地址（CIDR 形式）。
#[tauri::command]
pub fn network_interfaces() -> Result<Vec<NetworkInterface>, String> {
    let networks = Networks::new_with_refreshed_list();
    let mut list: Vec<NetworkInterface> = networks
        .iter()
        .map(|(name, data)| {
            let mut ipv4 = Vec::new();
            let mut ipv6 = Vec::new();
            for network in data.ip_networks() {
                let entry = format!("{}/{}", network.addr, network.prefix);
                match network.addr {
                    IpAddr::V4(_) => ipv4.push(entry),
                    IpAddr::V6(_) => ipv6.push(entry),
                }
            }
            let mac = data.mac_address();
            NetworkInterface {
                name: name.clone(),
                mac: if mac.is_unspecified() {
                    None
                } else {
                    Some(mac.to_string())
                },
                mtu: data.mtu(),
                ipv4,
                ipv6,
                is_loopback: is_loopback_interface(name),
            }
        })
        .collect();
    // 有地址的非回环接口排最前，纯虚拟/回环接口沉底。
    list.sort_by(|a, b| {
        let rank = |i: &NetworkInterface| {
            (
                i.is_loopback,
                !(i.ipv4.len() + i.ipv6.len() > 0),
                i.name.clone(),
            )
        };
        rank(a).cmp(&rank(b))
    });
    Ok(list)
}

/// 实时吞吐快照。第一次调用只建立基线（速率为 0），
/// 之后每次调用返回距上次的增量速率与累计流量。
#[tauri::command]
pub fn network_throughput() -> Result<ThroughputSnapshot, String> {
    let mut guard = SAMPLER.lock().map_err(|_| "network sampler poisoned")?;
    let now = Instant::now();

    let (elapsed_ms, factor) = match guard.as_mut() {
        None => {
            *guard = Some(NetworkSampler {
                networks: Networks::new_with_refreshed_list(),
                sampled_at: now,
            });
            (0, None)
        }
        Some(sampler) => {
            let elapsed = now.duration_since(sampler.sampled_at);
            sampler.networks.refresh(true);
            sampler.sampled_at = now;
            let elapsed_ms = elapsed.as_millis() as u64;
            let seconds = elapsed.as_secs_f64();
            // 间隔过短或过长（页面隐藏后恢复的第一轮）没有可信增量，
            // 本轮返回 0 速率，下一轮即为准确值。
            let usable = seconds > 0.05 && elapsed_ms <= STALE_SAMPLE_MS;
            (elapsed_ms, usable.then(|| 1.0 / seconds))
        }
    };

    let sampler = guard
        .as_ref()
        .expect("network sampler was just initialized");
    let rate = factor.unwrap_or(0.0);
    let mut interfaces: Vec<InterfaceThroughput> = sampler
        .networks
        .iter()
        .map(|(name, data)| InterfaceThroughput {
            name: name.clone(),
            is_loopback: is_loopback_interface(name),
            rx_bytes_per_sec: data.received() as f64 * rate,
            tx_bytes_per_sec: data.transmitted() as f64 * rate,
            rx_total_bytes: data.total_received(),
            tx_total_bytes: data.total_transmitted(),
        })
        .collect();
    interfaces.sort_by(|a, b| {
        let rank = |i: &InterfaceThroughput| {
            (
                i.is_loopback,
                (i.rx_bytes_per_sec + i.tx_bytes_per_sec) <= 0.0,
                i.name.clone(),
            )
        };
        rank(a).cmp(&rank(b))
    });
    let (total_rx, total_tx) = interfaces
        .iter()
        .filter(|i| !i.is_loopback)
        .fold((0.0, 0.0), |(rx, tx), i| {
            (rx + i.rx_bytes_per_sec, tx + i.tx_bytes_per_sec)
        });

    Ok(ThroughputSnapshot {
        elapsed_ms,
        total_rx_bytes_per_sec: total_rx,
        total_tx_bytes_per_sec: total_tx,
        interfaces,
    })
}

const IPV4_SERVICES: &[&str] = &[
    "https://4.ipw.cn",
    "https://api.ipify.org",
    "https://ipinfo.io/ip",
];
const IPV6_SERVICES: &[&str] = &["https://6.ipw.cn", "https://api6.ipify.org"];

/// 公网出口 IP：直连（显式绕过代理）与经系统代理两个视角并发探测。
/// 用户开着代理时，网站看到的往往是代理出口 IP 而非宽带出口 IP，
/// 两者都展示才能完整描述当前网络。
#[tauri::command]
pub async fn network_public_ip() -> Result<PublicIp, String> {
    // 直连视角必须显式绕开代理，否则拿到的是代理出口而非本机网络。
    let direct = reqwest::Client::builder()
        .timeout(Duration::from_secs(3))
        .no_proxy()
        .build()
        .map_err(|error| error.to_string())?;

    let proxy_target = detect_system_proxy();
    let proxy_client = proxy_target.as_ref().and_then(|(url, _)| {
        reqwest::Client::builder()
            .timeout(Duration::from_secs(5))
            .proxy(reqwest::Proxy::all(url).ok()?)
            .build()
            .ok()
    });

    let (ipv4, ipv6, proxy_ipv4, proxy_ipv6) = tokio::join!(
        fetch_public_ip(&direct, IPV4_SERVICES, false),
        fetch_public_ip(&direct, IPV6_SERVICES, true),
        async {
            match proxy_client.as_ref() {
                Some(client) => fetch_public_ip(client, IPV4_SERVICES, false).await,
                None => None,
            }
        },
        async {
            match proxy_client.as_ref() {
                Some(client) => fetch_public_ip(client, IPV6_SERVICES, true).await,
                None => None,
            }
        },
    );

    let proxy = proxy_target.map(|(_, display)| ProxyIp {
        server: display,
        ipv4: proxy_ipv4,
        ipv6: proxy_ipv6,
    });
    let proxy_reachable = proxy
        .as_ref()
        .is_some_and(|p| p.ipv4.is_some() || p.ipv6.is_some());

    if ipv4.is_none() && ipv6.is_none() && !proxy_reachable {
        return Err("no public IP service reachable".to_string());
    }
    Ok(PublicIp { ipv4, ipv6, proxy })
}

/// 依次尝试多个只返回纯 IP 文本的服务，取第一个符合预期地址族的合法地址。
/// 走系统代理时 v6 服务可能回退成 v4 文本，校验地址族避免放错位置。
async fn fetch_public_ip(
    client: &reqwest::Client,
    urls: &[&str],
    expect_ipv6: bool,
) -> Option<String> {
    for url in urls {
        if let Ok(response) = client.get(*url).send().await {
            if let Ok(text) = response.text().await {
                let candidate = text.trim();
                match candidate.parse::<IpAddr>() {
                    Ok(IpAddr::V6(_)) if expect_ipv6 => return Some(candidate.to_string()),
                    Ok(IpAddr::V4(_)) if !expect_ipv6 => return Some(candidate.to_string()),
                    _ => {}
                }
            }
        }
    }
    None
}

/// 检测系统代理：优先环境变量（HTTPS_PROXY/ALL_PROXY），再查系统代理设置。
/// 返回 (reqwest 代理 URL, 展示用地址)。
fn detect_system_proxy() -> Option<(String, String)> {
    if let Some(found) = proxy_from_env() {
        return Some(found);
    }
    proxy_from_registry()
}

fn proxy_from_env() -> Option<(String, String)> {
    for key in ["HTTPS_PROXY", "https_proxy", "ALL_PROXY", "all_proxy"] {
        if let Ok(value) = std::env::var(key) {
            if let Some(found) = normalize_proxy_url(&value) {
                return Some(found);
            }
        }
    }
    None
}

/// 读取 Windows 系统代理（Clash/v2ray 等开启"系统代理"时写入的注册表项）。
/// reqwest 默认不读该设置，需要在这里显式取出来再交给 reqwest。
#[cfg(windows)]
fn proxy_from_registry() -> Option<(String, String)> {
    let (enabled, server) = read_registry_proxy()?;
    if !enabled {
        return None;
    }
    pick_registry_proxy(&server).and_then(normalize_proxy_url)
}

/// 宽松版注册表代理：无视 ProxyEnable，只要 ProxyServer 有值。
/// 用于测速等可选走代理的场景——常见状态是用户关掉了系统代理开关，
/// 但本地代理内核仍在运行、端口仍可连。
#[cfg(windows)]
fn proxy_from_registry_loose() -> Option<(String, String)> {
    let (_, server) = read_registry_proxy()?;
    pick_registry_proxy(&server).and_then(normalize_proxy_url)
}

#[cfg(windows)]
fn read_registry_proxy() -> Option<(bool, String)> {
    use std::process::Command;

    let output = Command::new("reg")
        .args([
            "query",
            r"HKCU\Software\Microsoft\Windows\CurrentVersion\Internet Settings",
        ])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let mut enabled = false;
    let mut server: Option<String> = None;
    for line in text.lines() {
        let mut tokens = line.split_whitespace();
        let name = tokens.next().unwrap_or_default();
        let kind = tokens.next().unwrap_or_default();
        let value = tokens.collect::<Vec<_>>().join(" ");
        if value.is_empty() {
            continue;
        }
        match name {
            "ProxyEnable" if kind == "REG_DWORD" => {
                enabled =
                    u32::from_str_radix(value.to_ascii_lowercase().trim_start_matches("0x"), 16)
                        .map(|raw| raw != 0)
                        .unwrap_or(false);
            }
            "ProxyServer" if kind == "REG_SZ" => server = Some(value),
            _ => {}
        }
    }
    Some((enabled, server?))
}

#[cfg(not(windows))]
fn proxy_from_registry() -> Option<(String, String)> {
    None
}

#[cfg(not(windows))]
fn proxy_from_registry_loose() -> Option<(String, String)> {
    None
}

/// 解析注册表 ProxyServer：裸 "host:port" 直接用；"http=…;https=…"
/// 形式优先 https，其次 http，最后其他（如 socks）兜底。
fn pick_registry_proxy(raw: &str) -> Option<&str> {
    if !raw.contains('=') {
        let trimmed = raw.trim();
        return (!trimmed.is_empty()).then_some(trimmed);
    }
    let mut https = None;
    let mut http = None;
    let mut fallback = None;
    for part in raw.split(';') {
        let Some((key, value)) = part.split_once('=') else {
            continue;
        };
        let value = value.trim();
        if value.is_empty() {
            continue;
        }
        match key.trim().to_ascii_lowercase().as_str() {
            "https" => {
                if https.is_none() {
                    https = Some(value);
                }
            }
            "http" => {
                if http.is_none() {
                    http = Some(value);
                }
            }
            _ => {
                if fallback.is_none() {
                    fallback = Some(value);
                }
            }
        }
    }
    https.or(http).or(fallback)
}

/// 把 "host:port" / "socks5://host:port" 规范化为 reqwest 可用的代理 URL。
/// 返回 (请求用 URL, 展示用地址)。
fn normalize_proxy_url(raw: &str) -> Option<(String, String)> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    let display = trimmed.to_string();
    let url = if trimmed.contains("://") {
        display.clone()
    } else {
        format!("http://{trimmed}")
    };
    Some((url, display))
}

// ── 带宽测速 ────────────────────────────────────────────────────────────────
//
// 标准测速流程（Ookla/Cloudflare 同款方法论）：
// 1. 延迟——对邻近服务连续小请求取样；
// 2. 下行——多连接并行流式下载，按总字节数/耗时得带宽；
// 3. 上行——并行上传固定块，同样按字节/耗时计。
// 一次点击自动依次测两组：直连（no_proxy，测本机链路）→ 代理出口
// （显式走系统代理）；未检测到可用代理时跳过代理组而不是整体报错。
// 注意：fake-ip/TUN 类接管下「直连」流量也会被劫持进代理进程，
// 此时直连组测到的是代理分流的线路，属网络环境现状而非测量错误。

const LATENCY_SAMPLES: usize = 5;
const DOWNLOAD_SECS: u64 = 6;
const DOWNLOAD_STREAMS: usize = 4;
const MAX_DOWNLOAD_BYTES: u64 = 1_500_000_000;
const UPLOAD_SECS: u64 = 5;
const UPLOAD_STREAMS: usize = 2;
const UPLOAD_CHUNK_BYTES: usize = 4 * 1024 * 1024;
const UPLOAD_URL: &str = "https://speed.cloudflare.com/__up";
/// 至少要测到这个量级的字节数才算有效，否则视为链路不可用。
const MIN_VALID_BYTES: u64 = 100_000;

static SPEED_TEST_RUNNING: AtomicBool = AtomicBool::new(false);

/// 单条链路（直连或代理）的测速结果。
#[derive(Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub struct SpeedTestModeResult {
    pub latency_ms: Option<f64>,
    pub download_bps: Option<f64>,
    pub upload_bps: Option<f64>,
    /// 该链路未测的原因代码：no_proxy = 未检测到可用代理，
    /// client_unavailable = HTTP 客户端构建失败（极端罕见）。
    /// None 表示已正常测量，个别指标不可测由对应字段为 None 表达。
    pub skipped: Option<&'static str>,
}

impl SpeedTestModeResult {
    fn skipped(code: &'static str) -> Self {
        Self {
            latency_ms: None,
            download_bps: None,
            upload_bps: None,
            skipped: Some(code),
        }
    }
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub struct SpeedTestResult {
    pub direct: SpeedTestModeResult,
    pub proxy: SpeedTestModeResult,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "snake_case")]
struct SpeedTestProgress {
    /// 进度归属链路：direct / proxy。
    mode: &'static str,
    phase: &'static str,
    current_bps: f64,
    progress: f64,
}

/// 带宽测速：一次调用自动依次测「直连 → 代理」两组，
/// 每组内部走 延迟 → 下行 → 上行，进度通过 `network-speed-progress`
/// 事件实时上报（mode 字段区分归属链路）。仅手动触发，从不自动运行。
#[tauri::command]
pub async fn network_speed_test(app: tauri::AppHandle) -> Result<SpeedTestResult, String> {
    if SPEED_TEST_RUNNING.swap(true, Ordering::SeqCst) {
        return Err("speed test already running".to_string());
    }
    let outcome = run_combined_speed_test(&app).await;
    SPEED_TEST_RUNNING.store(false, Ordering::SeqCst);
    outcome
}

async fn run_combined_speed_test(app: &tauri::AppHandle) -> Result<SpeedTestResult, String> {
    // 直连组占进度前半段（0 → 0.5）。
    let direct = match direct_speed_client() {
        Ok(client) => run_mode_test(app, "direct", client, 0.0).await,
        Err(_) => SpeedTestModeResult::skipped("client_unavailable"),
    };

    // 代理组占进度后半段（0.5 → 1.0）。未检测到可用代理时整组跳过。
    let proxy = match build_proxy_speed_client().await {
        Some(client) => run_mode_test(app, "proxy", client, 0.5).await,
        None => {
            // 仍发一次 done 事件，让前端进度条走满而不是停在 50%。
            emit_progress(app, "proxy", "done", 0.0, 1.0);
            SpeedTestModeResult::skipped("no_proxy")
        }
    };

    if direct.skipped.is_some() && proxy.skipped.is_some() {
        return Err(
            "no speed test route available: direct client failed and no usable proxy".to_string(),
        );
    }
    Ok(SpeedTestResult { direct, proxy })
}

/// 单组测速：延迟 → 下行 → 上行。`progress_base` 是该组在总进度中的
/// 起点（直连 0.0，代理 0.5），组内再按 5% 延迟 / 60% 下行 / 35% 上行细分。
async fn run_mode_test(
    app: &tauri::AppHandle,
    mode: &'static str,
    client: reqwest::Client,
    progress_base: f64,
) -> SpeedTestModeResult {
    // 组内阶段进度映射到总进度的对应半段。
    let at = |fraction: f64| (progress_base + fraction * 0.5).clamp(0.0, 1.0);

    emit_progress(app, mode, "latency", 0.0, at(0.0));
    let latency_ms = measure_latency(&client).await;
    emit_progress(app, mode, "latency", 0.0, at(0.05));

    emit_progress(app, mode, "download", 0.0, at(0.05));
    let download_bps = measure_download(app, &client, mode, at(0.05), at(0.65)).await;

    emit_progress(app, mode, "upload", 0.0, at(0.65));
    let upload_bps = measure_upload(app, &client, mode, at(0.65), at(1.0)).await;
    emit_progress(app, mode, "done", 0.0, at(1.0));

    SpeedTestModeResult {
        latency_ms,
        download_bps,
        upload_bps,
        skipped: None,
    }
}

fn emit_progress(
    app: &tauri::AppHandle,
    mode: &'static str,
    phase: &'static str,
    current_bps: f64,
    progress: f64,
) {
    let _ = app.emit(
        "network-speed-progress",
        SpeedTestProgress {
            mode,
            phase,
            current_bps,
            progress: progress.clamp(0.0, 1.0),
        },
    );
}

/// 直连测速客户端：显式绕过 HTTP 代理。
/// 注意 fake-ip/TUN 接管下流量仍会被网络层劫持，这是环境现状。
fn direct_speed_client() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .no_proxy()
        .build()
        .map_err(|error| error.to_string())
}

/// 代理模式测速客户端：env 代理优先，注册表宽松模式兜底
/// （无视 ProxyEnable，只要 ProxyServer 端口可连）。端口不通直接放弃。
async fn build_proxy_speed_client() -> Option<reqwest::Client> {
    let (url, _) = proxy_from_env().or_else(proxy_from_registry_loose)?;
    if let Some((host, port)) = proxy_endpoint(&url) {
        let connect = tokio::time::timeout(
            Duration::from_secs(2),
            tokio::net::TcpStream::connect((host.as_str(), port)),
        )
        .await;
        if !matches!(connect, Ok(Ok(_))) {
            return None;
        }
    }
    reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .proxy(reqwest::Proxy::all(&url).ok()?)
        .build()
        .ok()
}

/// 从 "http://host:port" / "socks5://host:port" 里取 (host, port)。
fn proxy_endpoint(url: &str) -> Option<(String, u16)> {
    let rest = url.rsplit_once("://").map(|(_, rest)| rest).unwrap_or(url);
    let (host, port) = rest.rsplit_once(':')?;
    Some((host.to_string(), port.parse().ok()?))
}

/// 延迟探测端点：小体积、可高频访问。多个端点并行竞速，
/// 取最先成功响应的一个来取样——fake-ip/TUN 环境下个别
/// 端点可能被分流丢弃，单端点方案会整段测不出。
const LATENCY_TARGETS: &[&str] = &[
    "https://speed.cloudflare.com/__down?bytes=1",
    "https://www.baidu.com",
    "https://4.ipw.cn",
];

/// 延迟：先选可用端点，再连续小请求取样，取最小值（排除建连抖动）。
async fn measure_latency(client: &reqwest::Client) -> Option<f64> {
    let target = pick_latency_target(client).await?;

    // 预热一次，让 TLS/连接复用就位，不计入样本。
    let _ = client.get(target).send().await;
    let mut samples: Vec<f64> = Vec::new();
    for _ in 0..LATENCY_SAMPLES {
        let start = Instant::now();
        let attempt = tokio::time::timeout(Duration::from_secs(3), client.get(target).send()).await;
        if let Ok(Ok(response)) = attempt {
            if response.status().is_success() {
                samples.push(start.elapsed().as_secs_f64() * 1000.0);
            }
        }
    }
    samples.iter().copied().fold(None, |best, value| {
        Some(best.map_or(value, |current: f64| current.min(value)))
    })
}

/// 并行探测所有延迟端点，返回最先成功的一个。
async fn pick_latency_target(client: &reqwest::Client) -> Option<&'static str> {
    // select_all 要求 future 实现 Unpin，async 块需要 Box::pin 固定。
    let attempts: Vec<_> = LATENCY_TARGETS
        .iter()
        .map(|&url| {
            let client = client.clone();
            Box::pin(async move {
                match tokio::time::timeout(Duration::from_secs(3), client.get(url).send()).await {
                    Ok(Ok(response)) if response.status().is_success() => Some(url),
                    _ => None,
                }
            })
        })
        .collect();
    let (fastest, _index, _remaining) = futures_util::future::select_all(attempts).await;
    fastest
}

/// 选择可用的下载源：优先国内 CDN（结果更能代表真实可用带宽），
/// 回退 Cloudflare 测速端点。各读取 64KB 验证链路可用。
async fn pick_download_url(client: &reqwest::Client) -> Option<String> {
    let candidates = [
        "https://dldir1.qq.com/weixin/Windows/WeChatSetup.exe",
        "https://speed.cloudflare.com/__down?bytes=104857600",
    ];
    for url in candidates {
        let request = client
            .get(url)
            .header(reqwest::header::RANGE, "bytes=0-524287")
            .send();
        let Ok(Ok(response)) = tokio::time::timeout(Duration::from_secs(3), request).await else {
            continue;
        };
        if !response.status().is_success() {
            continue;
        }
        let mut stream = response.bytes_stream();
        let mut received = 0usize;
        while received < 64 * 1024 {
            match tokio::time::timeout(Duration::from_secs(2), stream.next()).await {
                Ok(Some(Ok(chunk))) => received += chunk.len(),
                _ => break,
            }
        }
        if received >= 64 * 1024 {
            return Some(url.to_string());
        }
    }
    None
}

/// 下行带宽：多条连接并行流式下载直到时限，按累计字节/真实耗时计算。
/// `progress_from`/`progress_to` 是本阶段在总进度里的区间，随模式（直连/代理）平移。
async fn measure_download(
    app: &tauri::AppHandle,
    client: &reqwest::Client,
    mode: &'static str,
    progress_from: f64,
    progress_to: f64,
) -> Option<f64> {
    let url = pick_download_url(client).await?;
    let bytes = Arc::new(AtomicU64::new(0));
    let deadline = Instant::now() + Duration::from_secs(DOWNLOAD_SECS);

    let mut handles = Vec::new();
    for _ in 0..DOWNLOAD_STREAMS {
        let client = client.clone();
        let url = url.clone();
        let bytes = Arc::clone(&bytes);
        handles.push(tokio::spawn(async move {
            loop {
                if Instant::now() >= deadline {
                    return;
                }
                let Ok(response) = client.get(&url).send().await else {
                    tokio::time::sleep(Duration::from_millis(200)).await;
                    continue;
                };
                let mut stream = response.bytes_stream();
                while let Some(chunk) = stream.next().await {
                    match chunk {
                        Ok(data) => {
                            bytes.fetch_add(data.len() as u64, Ordering::Relaxed);
                            if Instant::now() >= deadline
                                || bytes.load(Ordering::Relaxed) >= MAX_DOWNLOAD_BYTES
                            {
                                return;
                            }
                        }
                        Err(_) => break, // 中断后由外层重新发起连接
                    }
                }
            }
        }));
    }

    let start = Instant::now();
    let mut last = 0u64;
    while Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(500)).await;
        let current = bytes.load(Ordering::Relaxed);
        emit_progress(
            app,
            mode,
            "download",
            (current - last) as f64 * 8.0 / 0.5,
            progress_from
                + (progress_to - progress_from)
                    * (start.elapsed().as_secs_f64() / DOWNLOAD_SECS as f64),
        );
        last = current;
        if current >= MAX_DOWNLOAD_BYTES {
            break;
        }
    }
    for handle in handles {
        let _ = handle.await;
    }

    let total = bytes.load(Ordering::Relaxed);
    if total < MIN_VALID_BYTES {
        return None;
    }
    let elapsed = start.elapsed().as_secs_f64().max(0.5);
    Some(total as f64 * 8.0 / elapsed)
}

/// 上行带宽：并行向 Cloudflare 上传端点反复 POST 固定块，按完成块计速。
/// 国内没有稳定的公共上传测速端点，上行只能经 Cloudflare 测量；
/// 不可达时返回 None，前端显示「不可测」而不是给一个假数字。
/// `progress_from`/`progress_to` 是本阶段在总进度里的区间，随模式（直连/代理）平移。
async fn measure_upload(
    app: &tauri::AppHandle,
    client: &reqwest::Client,
    mode: &'static str,
    progress_from: f64,
    progress_to: f64,
) -> Option<f64> {
    // 小块探测端点可用性，失败则整个上行阶段跳过。
    let probe = tokio::time::timeout(
        Duration::from_secs(4),
        client.post(UPLOAD_URL).body(vec![0u8; 64 * 1024]).send(),
    )
    .await;
    match probe {
        Ok(Ok(response)) if response.status().is_success() => {}
        _ => return None,
    }

    let chunk = Arc::new(vec![0u8; UPLOAD_CHUNK_BYTES]);
    let bytes = Arc::new(AtomicU64::new(0));
    let deadline = Instant::now() + Duration::from_secs(UPLOAD_SECS);

    let mut handles = Vec::new();
    for _ in 0..UPLOAD_STREAMS {
        let client = client.clone();
        let chunk = Arc::clone(&chunk);
        let bytes = Arc::clone(&bytes);
        handles.push(tokio::spawn(async move {
            loop {
                if Instant::now() >= deadline {
                    return;
                }
                let request = client.post(UPLOAD_URL).body((*chunk).clone());
                match request.send().await {
                    Ok(response) if response.status().is_success() => {
                        bytes.fetch_add(UPLOAD_CHUNK_BYTES as u64, Ordering::Relaxed);
                    }
                    _ => {
                        tokio::time::sleep(Duration::from_millis(200)).await;
                    }
                }
            }
        }));
    }

    let start = Instant::now();
    let mut last = 0u64;
    while Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(500)).await;
        let current = bytes.load(Ordering::Relaxed);
        emit_progress(
            app,
            mode,
            "upload",
            (current - last) as f64 * 8.0 / 0.5,
            progress_from
                + (progress_to - progress_from)
                    * (start.elapsed().as_secs_f64() / UPLOAD_SECS as f64),
        );
        last = current;
    }
    for handle in handles {
        let _ = handle.await;
    }

    let total = bytes.load(Ordering::Relaxed);
    if total < MIN_VALID_BYTES {
        return None;
    }
    let elapsed = start.elapsed().as_secs_f64().max(0.5);
    Some(total as f64 * 8.0 / elapsed)
}
