//! DeepSeek Harness 统一桥接插件（`dsh-vibe-assistant-bridge`）的接入管理。
//!
//! 独立开源插件仓库以 npm bundle 形态发布。desktop profile 被 dsh 桌面版
//! （Electron）独占管理——`dsh plugin add` CLI 会拒绝写入——因此安装走
//! 桌面 UI 内部的同一机制：在 profile 目录用 pnpm 装包 + 写
//! `dsh.profile.bundles` 声明（等价于 Plugins → Add plugin 的落盘结果）。
//! 智管不再往 profile 写插件本体，只负责：
//!
//! - 调 pnpm 安装/卸载（`--save-exact` 精确版本，规避 pnpm 24h 最新版延迟）；
//! - 迁移：移除旧内嵌注入/观测插件与记忆 MCP 注册（用户已拍板默认移除，
//!   逃生口是随时可在 MCP 页重装记忆服务器）；
//! - 特性覆盖：按 cordis「后层按行整体替换 config」的分层规则，在 profile
//!   用户层 `cordis.patch.yml` 写同 id 覆盖行，调整 endpoint/快照/特性开关；
//! - 状态探测：读 profile `package.json` 的 bundles 声明与 node_modules 安装态。
//!
//! 快照文件路径沿用旧注入插件的位置（`plugins/vibe-assistant-memory-snapshot.md`），
//! 迁移时先卸载旧插件（会连带删除快照）再重写，保证 bridge 的离线回退不断档。

use std::io::Read;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::process_util::no_window;

/// npm 包名（与独立仓库一致，社区目录按该名 + `dsh-plugin` topic 收录）。
const PLUGIN_PACKAGE: &str = "dsh-vibe-assistant-bridge";
/// 智管锁定安装的版本；升级随智管发版 bump，避免 preview 期宿主不兼容。
const PINNED_VERSION: &str = "0.1.0";
/// cordis.patch.yml 覆盖行与 bundle patch 共用的插件实例 id。
const BRIDGE_ENTRY_ID: &str = "vibe-assistant-bridge";
/// pnpm 冷启动要解析依赖树，给足首次安装时间。
const CLI_TIMEOUT: Duration = Duration::from_secs(300);
/// 子进程输出的保留尾部（报错时给用户看的是末段，不是开头）。
const OUTPUT_TAIL_BYTES: usize = 8 * 1024;

// ── 路径 ──────────────────────────────────────────────────────────────────────

fn profile_dir() -> Option<PathBuf> {
    crate::agent_sources::dsh_home()
        .map(|home| home.join("profiles").join("desktop"))
}

fn profile_package_json() -> Option<PathBuf> {
    profile_dir().map(|dir| dir.join("package.json"))
}

fn installed_package_json() -> Option<PathBuf> {
    profile_dir().map(|dir| dir.join("node_modules").join(PLUGIN_PACKAGE).join("package.json"))
}

fn snapshot_path() -> Option<PathBuf> {
    profile_dir().map(|dir| dir.join("plugins").join("vibe-assistant-memory-snapshot.md"))
}

// ── 特性开关 ──────────────────────────────────────────────────────────────────

/// 插件六能力中可独立启停的五组（注入、观测、用量上报、收集触发、原生工具）。
/// 覆盖行未写或缺失时默认全开，与插件自身默认一致。
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub struct DshBridgeFeatures {
    pub injection: bool,
    pub observation: bool,
    pub usage: bool,
    pub collect: bool,
    pub tools: bool,
}

impl Default for DshBridgeFeatures {
    fn default() -> Self {
        Self {
            injection: true,
            observation: true,
            usage: true,
            collect: true,
            tools: true,
        }
    }
}

/// 渲染 profile 用户层的覆盖行：同 id 整体替换 bundle 默认 config。
fn render_features_entry(endpoint: &str, snapshot: &str, features: &DshBridgeFeatures) -> String {
    format!(
        "- insert:\n    - id: {BRIDGE_ENTRY_ID}\n      name: {PLUGIN_PACKAGE}\n      config:\n        endpoint: {}\n        snapshotPath: {}\n        features:\n          injection: {}\n          observation: {}\n          usage: {}\n          collect: {}\n          tools: {}\n",
        crate::dsh_mcp::yaml_quote(endpoint),
        crate::dsh_mcp::yaml_quote(snapshot),
        features.injection,
        features.observation,
        features.usage,
        features.collect,
        features.tools,
    )
}

/// 移除 patch 顶层中所有指向 bridge 覆盖行的条目（id 定位，其余原样保留）。
fn rewrite_bridge_items(text: &str) -> Result<String, String> {
    let mut output = String::new();
    for item in crate::dsh_mcp::split_top_items(text) {
        let block = crate::dsh_mcp::item_block(text, &item)?;
        if block.contains(&format!("id: {BRIDGE_ENTRY_ID}")) {
            continue;
        }
        output.push_str(&block);
    }
    Ok(output)
}

/// 读取当前生效的特性（仅解析智管自己写的规范格式覆盖行）。
fn read_features() -> DshBridgeFeatures {
    let mut features = DshBridgeFeatures::default();
    let Some(text) = crate::dsh_mcp::patch_path().and_then(|path| std::fs::read_to_string(path).ok())
    else {
        return features;
    };
    for item in crate::dsh_mcp::split_top_items(&text) {
        let Ok(block) = crate::dsh_mcp::item_block(&text, &item) else {
            continue;
        };
        if !block.contains(&format!("id: {BRIDGE_ENTRY_ID}")) {
            continue;
        }
        // 智管写入的覆盖行是受控格式，布尔值只有 true/false 两种字面量；
        // 命中 false 才关，缺失即保持默认全开。
        if block.contains("injection: false") {
            features.injection = false;
        }
        if block.contains("observation: false") {
            features.observation = false;
        }
        if block.contains("usage: false") {
            features.usage = false;
        }
        if block.contains("collect: false") {
            features.collect = false;
        }
        if block.contains("tools: false") {
            features.tools = false;
        }
        break;
    }
    features
}

/// 写入/更新特性覆盖行（幂等：先移除旧覆盖行再追加）。
fn write_features(features: &DshBridgeFeatures) -> Result<(), String> {
    let snapshot = snapshot_path().ok_or("无法定位 DeepSeek Harness 配置目录（~/.dsh）")?;
    let patch = crate::dsh_mcp::patch_path_or_error()?;
    let endpoint = format!("http://127.0.0.1:{}", crate::agent_http::AGENT_HTTP_PORT);
    let text = match std::fs::read_to_string(&patch) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            crate::dsh_mcp::new_file_header().to_string()
        }
        Err(error) => return Err(format!("读取 {} 失败：{error}", patch.display())),
    };
    let updated = crate::dsh_mcp::append_item(
        rewrite_bridge_items(&text)?,
        &render_features_entry(&endpoint, &snapshot.to_string_lossy(), features),
    );
    crate::dsh_mcp::write_atomically(&patch, &updated)
}

// ── 状态探测 ──────────────────────────────────────────────────────────────────

#[derive(Clone, Serialize)]
pub struct DshBridgeStatus {
    pub installed: bool,
    pub version: Option<String>,
    pub detail: String,
    pub features: DshBridgeFeatures,
    /// 旧内嵌注入插件仍在位（应执行一键接入完成迁移）。
    pub legacy_memory_inject: bool,
    /// 旧内嵌观测插件仍在位。
    pub legacy_mcp_observe: bool,
}

/// `dsh.profile.bundles` 里是否声明了插件包（接受裸名与 `name@version` 两种形态）。
fn bundle_declared(profile_pkg: &Value) -> bool {
    profile_pkg
        .get("dsh")
        .and_then(|dsh| dsh.get("profile"))
        .and_then(|profile| profile.get("bundles"))
        .and_then(|bundles| bundles.as_array())
        .map(|entries| {
            entries.iter().any(|entry| match entry {
                Value::String(name) => {
                    name == PLUGIN_PACKAGE || name.starts_with(&format!("{PLUGIN_PACKAGE}@"))
                }
                Value::Object(object) => {
                    object.get("name").and_then(|name| name.as_str()) == Some(PLUGIN_PACKAGE)
                }
                _ => false,
            })
        })
        .unwrap_or(false)
}

fn status() -> DshBridgeStatus {
    let mut status = DshBridgeStatus {
        installed: false,
        version: None,
        detail: String::new(),
        features: DshBridgeFeatures::default(),
        legacy_memory_inject: crate::dsh_injection::status().0,
        legacy_mcp_observe: crate::dsh_observe::status().installed,
    };
    let Some(dir) = profile_dir() else {
        status.detail = "无法定位 DeepSeek Harness 配置目录（~/.dsh）".into();
        return status;
    };
    status.features = read_features();
    let declared = profile_package_json()
        .and_then(|path| std::fs::read_to_string(path).ok())
        .and_then(|text| serde_json::from_str::<Value>(&text).ok())
        .map(|pkg| bundle_declared(&pkg))
        .unwrap_or(false);
    let version = installed_package_json()
        .and_then(|path| std::fs::read_to_string(path).ok())
        .and_then(|text| serde_json::from_str::<Value>(&text).ok())
        .and_then(|pkg| {
            pkg.get("version")
                .and_then(|version| version.as_str())
                .map(str::to_string)
        });
    status.installed = declared && version.is_some();
    status.version = version;
    status.detail = if status.installed {
        format!(
            "已安装于 {}（node_modules/{}）",
            dir.display(),
            PLUGIN_PACKAGE
        )
    } else if declared {
        "bundle 已声明但 node_modules 缺失；重试一键接入可修复".into()
    } else {
        "未安装；一键接入会经 dsh 官方插件通道（npx pnpm）安装，也可在 dsh 的 Plugins 页手动添加".into()
    };
    status
}

// ── pnpm 目录级安装 ───────────────────────────────────────────────────────────

/// 持续排空子进程输出并只保留尾部，防止管道写满导致子进程卡死。
fn spawn_tail_capture<R: Read + Send + 'static>(reader: R) -> std::sync::mpsc::Receiver<String> {
    let (sender, receiver) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut reader = reader;
        let mut buf = Vec::new();
        let _ = reader.read_to_end(&mut buf);
        let tail: Vec<u8> = if buf.len() > OUTPUT_TAIL_BYTES {
            buf[buf.len() - OUTPUT_TAIL_BYTES..].to_vec()
        } else {
            buf
        };
        let _ = sender.send(String::from_utf8_lossy(&tail).into_owned());
    });
    receiver
}

/// 在 desktop profile 目录内运行 pnpm（等价于 dsh 桌面版 Add plugin 的落盘
/// 行为：装包进 node_modules + 写 dependencies；bundles 声明由我们补上）。
fn run_pnpm(args: &[&str]) -> Result<String, String> {
    let dir = profile_dir().ok_or("无法定位 DeepSeek Harness 配置目录（~/.dsh）")?;
    let cli_args: Vec<String> = args.iter().map(|arg| (*arg).to_string()).collect();
    let mut command = crate::mcp_stdio_relay::child_command("pnpm", &cli_args)?;
    command
        .current_dir(&dir)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .stdin(std::process::Stdio::null());
    no_window(&mut command);
    let mut child = command.spawn().map_err(|error| {
        format!(
            "无法启动 pnpm（{error}）。请确认已安装 pnpm（Node.js 附带 corepack 可启用），或在 dsh 桌面版的 Plugins → Add plugin 中手动添加 {PLUGIN_PACKAGE}@{PINNED_VERSION}"
        )
    })?;
    let stdout_rx = spawn_tail_capture(child.stdout.take().expect("pnpm stdout 已接管"));
    let stderr_rx = spawn_tail_capture(child.stderr.take().expect("pnpm stderr 已接管"));
    let deadline = Instant::now() + CLI_TIMEOUT;
    loop {
        match child.try_wait() {
            Ok(Some(exit)) => {
                let stdout = stdout_rx.recv().unwrap_or_default();
                let stderr = stderr_rx.recv().unwrap_or_default();
                let tail = if stderr.trim().is_empty() { stdout } else { stderr };
                if exit.success() {
                    return Ok(tail);
                }
                return Err(format!(
                    "pnpm 失败（退出码 {:?}）：{}。可改为在 dsh 桌面版的 Plugins → Add plugin 中手动添加 {PLUGIN_PACKAGE}@{PINNED_VERSION}",
                    exit.code(),
                    tail.trim()
                ));
            }
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(format!(
                        "pnpm 超时（{} 秒）。可在 dsh 桌面版的 Plugins → Add plugin 中手动添加 {PLUGIN_PACKAGE}@{PINNED_VERSION}",
                        CLI_TIMEOUT.as_secs()
                    ));
                }
                std::thread::sleep(Duration::from_millis(200));
            }
            Err(error) => return Err(format!("等待 pnpm 退出失败：{error}")),
        }
    }
}

/// pnpm 只写 dependencies，不写 `dsh.profile.bundles`——后者是 dsh 的概念，
/// 由这里补齐（保序追加，幂等）。
fn declare_bundle() -> Result<(), String> {
    let path = profile_package_json().ok_or("无法定位 desktop profile 的 package.json")?;
    let text = std::fs::read_to_string(&path)
        .map_err(|error| format!("读取 {} 失败：{error}", path.display()))?;
    let mut pkg: Value = serde_json::from_str(&text)
        .map_err(|error| format!("解析 {} 失败：{error}", path.display()))?;
    let bundles = pkg
        .pointer_mut("/dsh/profile/bundles")
        .and_then(Value::as_array_mut)
        .ok_or("desktop profile package.json 缺少 dsh.profile.bundles")?;
    let declared = bundles.iter().any(|entry| match entry {
        Value::String(name) => {
            name == PLUGIN_PACKAGE || name.starts_with(&format!("{PLUGIN_PACKAGE}@"))
        }
        Value::Object(object) => {
            object.get("name").and_then(|name| name.as_str()) == Some(PLUGIN_PACKAGE)
        }
        _ => false,
    });
    if !declared {
        bundles.push(Value::String(PLUGIN_PACKAGE.to_string()));
        let updated = serde_json::to_string_pretty(&pkg).map_err(|error| error.to_string())?;
        crate::dsh_mcp::write_atomically(&path, &(updated + "\n"))?;
    }
    Ok(())
}

/// 卸载时移除 bundles 声明（dependencies 由 pnpm remove 清理）。
fn undeclare_bundle() -> Result<(), String> {
    let path = profile_package_json().ok_or("无法定位 desktop profile 的 package.json")?;
    let Ok(text) = std::fs::read_to_string(&path) else {
        return Ok(());
    };
    let mut pkg: Value = match serde_json::from_str(&text) {
        Ok(pkg) => pkg,
        Err(_) => return Ok(()),
    };
    let Some(bundles) = pkg
        .pointer_mut("/dsh/profile/bundles")
        .and_then(Value::as_array_mut)
    else {
        return Ok(());
    };
    bundles.retain(|entry| match entry {
        Value::String(name) => {
            name != PLUGIN_PACKAGE && !name.starts_with(&format!("{PLUGIN_PACKAGE}@"))
        }
        Value::Object(object) => {
            object.get("name").and_then(|name| name.as_str()) != Some(PLUGIN_PACKAGE)
        }
        _ => true,
    });
    let updated = serde_json::to_string_pretty(&pkg).map_err(|error| error.to_string())?;
    crate::dsh_mcp::write_atomically(&path, &(updated + "\n"))
}

// ── 安装 / 卸载 / 迁移 ────────────────────────────────────────────────────────

/// 迁移旧内嵌插件与记忆 MCP 注册。各项失败只记录，不阻断安装结果。
fn migrate_legacy() -> Vec<String> {
    let mut notes = Vec::new();
    if let Err(error) = crate::dsh_observe::uninstall() {
        notes.push(format!("移除旧观测插件失败：{error}"));
    }
    if let Err(error) = crate::dsh_injection::uninstall() {
        notes.push(format!("移除旧注入插件失败：{error}"));
    }
    // 旧注入插件的卸载会连带删除快照；这里按最新会话上下文重写，
    // 保证 bridge 注入特性的离线回退立即有内容可用。
    if let Err(error) = crate::dsh_injection::refresh_snapshot(
        &crate::memory_mcp::hook_session_start_context("dsh").0,
    ) {
        notes.push(format!("刷新记忆快照失败：{error}"));
    }
    // 用户决策：插件原生工具接管召回后，默认移除 dsh 里的记忆 MCP 注册
    // （逃生口：MCP 页随时可重装）。
    for name in [crate::memory_mcp::server_name(), crate::memory_mcp::legacy_server_name()] {
        if let Err(error) = crate::dsh_mcp::remove_server(name) {
            notes.push(format!("移除记忆 MCP 注册（{name}）失败：{error}"));
        }
    }
    notes
}

fn install_impl() -> Result<DshBridgeStatus, String> {
    run_pnpm(&[
        "add",
        "--save-exact",
        &format!("{PLUGIN_PACKAGE}@{PINNED_VERSION}"),
    ])?;
    declare_bundle()?;
    let notes = migrate_legacy();
    let mut result = status();
    if !result.installed {
        return Err(format!(
            "pnpm 报告成功但未检测到安装（{}）。请在 dsh 桌面版的 Plugins 页确认后重试",
            result.detail
        ));
    }
    if !notes.is_empty() {
        result.detail = format!("{}；迁移提示：{}", result.detail, notes.join("；"));
    }
    Ok(result)
}

fn uninstall_impl() -> Result<DshBridgeStatus, String> {
    run_pnpm(&["remove", PLUGIN_PACKAGE])?;
    undeclare_bundle()?;
    // 覆盖行与离线快照随插件一起清理；记忆 MCP 注册不自动恢复
    //（需要时在 MCP 页重新一键安装即可）。
    if let Some(patch) = crate::dsh_mcp::patch_path() {
        if let Ok(text) = std::fs::read_to_string(&patch) {
            let _ = crate::dsh_mcp::write_atomically(&patch, &rewrite_bridge_items(&text)?);
        }
    }
    if let Some(snapshot) = snapshot_path() {
        let _ = std::fs::remove_file(snapshot);
    }
    Ok(status())
}

// ── Tauri 命令 ────────────────────────────────────────────────────────────────

#[tauri::command]
pub fn dsh_bridge_status() -> DshBridgeStatus {
    status()
}

#[tauri::command]
pub async fn dsh_bridge_set(enabled: bool) -> Result<DshBridgeStatus, String> {
    tauri::async_runtime::spawn_blocking(move || {
        if enabled {
            install_impl()
        } else {
            uninstall_impl()
        }
    })
    .await
    .map_err(|_| "dsh 桥接任务中断".to_string())?
}

#[tauri::command]
pub async fn dsh_bridge_features(features: DshBridgeFeatures) -> Result<DshBridgeStatus, String> {
    tauri::async_runtime::spawn_blocking(move || {
        write_features(&features)?;
        Ok(status())
    })
    .await
    .map_err(|_| "dsh 桥接任务中断".to_string())?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn features_entry_contains_stable_id_and_defaults() {
        let entry = render_features_entry(
            "http://127.0.0.1:9420",
            r"C:\Users\t\.dsh\profiles\desktop\plugins\vibe-assistant-memory-snapshot.md",
            &DshBridgeFeatures::default(),
        );
        assert!(entry.contains(&format!("id: {BRIDGE_ENTRY_ID}")));
        assert!(entry.contains(&format!("name: {PLUGIN_PACKAGE}")));
        assert!(entry.contains("endpoint: \"http://127.0.0.1:9420\""));
        // Windows 路径必须被引号转义
        assert!(entry.contains("snapshotPath: \"C:\\\\Users\\\\t\\\\.dsh"));
        assert!(entry.contains("injection: true"));
        assert!(entry.contains("tools: true"));
    }

    #[test]
    fn rewrite_removes_only_bridge_entries() {
        let text = format!(
            "# 用户注释\n- id: ui-chat\n  name: \"@deepseek-ai/dsh-client-ui-chat\"\n  config:\n    transcriptView: standard\n\n{ }",
            render_features_entry("http://127.0.0.1:9420", "/tmp/snapshot.md", &DshBridgeFeatures::default())
        );
        let updated = rewrite_bridge_items(&text).unwrap();
        assert!(updated.contains("ui-chat"));
        assert!(!updated.contains(&format!("id: {BRIDGE_ENTRY_ID}")));
    }

    #[test]
    fn parse_features_reads_override_and_defaults_without_it() {
        let override_text = render_features_entry(
            "http://127.0.0.1:9420",
            "/tmp/snapshot.md",
            &DshBridgeFeatures {
                injection: true,
                observation: false,
                usage: true,
                collect: false,
                tools: true,
            },
        );
        let features = parse_features_from(&override_text);
        assert!(features.injection);
        assert!(!features.observation);
        assert!(!features.collect);
        assert!(features.usage);
        assert!(features.tools);
        // 没有覆盖行时默认全开
        let defaults = parse_features_from("# 仅注释\n- id: other\n  name: other-plugin\n");
        assert_eq!(defaults.injection, DshBridgeFeatures::default().injection);
        assert!(defaults.observation);
    }

    /// 与 `read_features` 同构，但接受文本输入以便离线测试。
    fn parse_features_from(text: &str) -> DshBridgeFeatures {
        let mut features = DshBridgeFeatures::default();
        for item in crate::dsh_mcp::split_top_items(text) {
            let Ok(block) = crate::dsh_mcp::item_block(text, &item) else {
                continue;
            };
            if !block.contains(&format!("id: {BRIDGE_ENTRY_ID}")) {
                continue;
            }
            if block.contains("injection: false") {
                features.injection = false;
            }
            if block.contains("observation: false") {
                features.observation = false;
            }
            if block.contains("usage: false") {
                features.usage = false;
            }
            if block.contains("collect: false") {
                features.collect = false;
            }
            if block.contains("tools: false") {
                features.tools = false;
            }
            break;
        }
        features
    }

    #[test]
    fn bundle_declaration_accepts_bare_and_versioned_names() {
        let declared = serde_json::json!({
            "dsh": {"profile": {"bundles": ["@deepseek-ai/dsh-base", "dsh-vibe-assistant-bridge"]}}
        });
        assert!(bundle_declared(&declared));
        let versioned = serde_json::json!({
            "dsh": {"profile": {"bundles": [{"name": "dsh-vibe-assistant-bridge"}]}}
        });
        assert!(bundle_declared(&versioned));
        let absent = serde_json::json!({"dsh": {"profile": {"bundles": ["@deepseek-ai/dsh-base"]}}});
        assert!(!bundle_declared(&absent));
        assert!(!bundle_declared(&serde_json::json!({})));
    }
}
