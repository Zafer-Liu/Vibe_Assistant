//! 跨 Agent 的 MCP 服务器中央注册表（「MCP 库」）。
//!
//! 与 Skill 库不同，MCP 条目没有文件负载——配置本身就是数据，因此整个
//! 目录存放在 SQLite `app_setting`（键 `mcp_catalog`）。装备 = 把条目写入
//! 各 Agent 的 MCP 配置（文件型 JSON，或 codex/claude CLI 的用户级注册表），
//! 卸载 = 移除对应键。状态检查只读配置文件（毫秒级），不 spawn CLI；
//! 写 CLI 型 Agent 时走子进程（与 memory_mcp 的安装流程一致）。

use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_json::{json, Map, Value};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use sha2::{Digest, Sha256};

use crate::memory_mcp::{
    agent_label, cli_detail, desktop_config_path, run_agent_cli, servers_map, servers_map_mut,
};

const MCP_CATALOG_SETTING_KEY: &str = "mcp_catalog";
const MCP_MIGRATION_SETTING_KEY: &str = "mcp_catalog_legacy_migration_v1";
/// 已命名的「启用 MCP 服务器集合」档案（工作流运行侧选择快照）。
const MCP_PROFILES_SETTING_KEY: &str = "mcp_profiles";
/// 共享记忆 MCP 的服务器名：导入时跳过，避免把开发期运行副本收编入库。
const MEMORY_SERVER_NAME: &str = "Vibe-Assistant-memory";
/// 旧服务名：中央目录中的历史条目在加载时自动改名为新名称。
const LEGACY_MEMORY_SERVER_NAME: &str = "agent-manager-memory";

#[derive(Serialize, Deserialize)]
struct CatalogMigration {
    completed: bool,
    report: Vec<String>,
}

// ── 数据模型 ──────────────────────────────────────────────────────────────────

/// MCP 库的一个条目。name 即各 Agent 配置里的服务器 id。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct McpCatalogEntry {
    pub name: String,
    #[serde(default)]
    pub description: String,
    /// "stdio" | "sse" | "http"；空串按 stdio 处理。
    #[serde(default)]
    pub transport: String,
    /// stdio 启动命令（如 npx / node / uvx）。
    #[serde(default)]
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    /// sse / http 远程地址。
    #[serde(default)]
    pub url: String,
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
    /// 已装备的 Agent（MemoryMcpTarget id）。codex_cli/codex_desktop 共用
    /// 同一 codex 用户级注册表，装备时二者等价，这里保留用户勾选的原样。
    #[serde(default)]
    pub assigned_agents: Vec<String>,
    #[serde(default)]
    pub created_at: String,
    #[serde(default)]
    pub updated_at: String,
}

/// 一个库条目在某个已装备 Agent 上的状态。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub struct McpAgentStatus {
    pub name: String,
    pub agent: String,
    /// "installed" | "missing" | "differs" | "unavailable"
    pub state: String,
    /// Only field names; never include credential values in status payloads.
    pub differing_fields: Vec<String>,
    pub error: Option<String>,
}

/// 「从 Agent 导入」发现的候选：来自某个 Agent 的现有配置。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub struct McpImportCandidate {
    pub agent: String,
    pub entry: McpCatalogEntry,
    pub already_in_catalog: bool,
    /// "new" | "matches" | "differs", relative to the central catalog.
    pub catalog_state: String,
    pub differing_fields: Vec<String>,
    pub variant: usize,
    pub variant_count: usize,
    pub peer_differing_fields: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct McpImportIssue {
    pub agent: String,
    pub name: Option<String>,
    pub message: String,
}

#[derive(Debug, Default, Serialize)]
pub struct McpImportScan {
    pub candidates: Vec<McpImportCandidate>,
    pub issues: Vec<McpImportIssue>,
}

#[derive(Debug, Clone, Serialize)]
pub struct McpComparisonSource {
    pub agent: String,
    pub entry: Option<McpCatalogEntry>,
    pub state: String,
    pub assigned: bool,
    pub observed: bool,
    pub error: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct McpConfigurationComparison {
    pub name: String,
    pub catalog: Option<McpCatalogEntry>,
    pub sources: Vec<McpComparisonSource>,
    pub revision: String,
    pub blocked_reason: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct McpArbitrationResult {
    pub entry: McpCatalogEntry,
    pub catalog_updated: bool,
    pub applied_agents: Vec<String>,
    pub errors: Vec<McpImportIssue>,
}

const MCP_TARGETS: [&str; 10] = [
    "claude_cli", "claude_desktop", "codex_cli", "codex_desktop",
    "qoder", "workbuddy", "minimax", "kimi", "zcode", "dsh",
];

// ── 目标与 transport 能力 ─────────────────────────────────────────────────────

fn is_mcp_target(agent: &str) -> bool {
    matches!(
        agent,
        "codex_cli"
            | "claude_cli"
            | "codex_desktop"
            | "claude_desktop"
            | "qoder"
            | "workbuddy"
            | "minimax"
            | "kimi"
            | "zcode"
            | "dsh"
    )
}

/// stdio 所有目标可用；sse/http 仅 Claude 系支持（其余 Agent 的 mcp.json
/// 远程 MCP 格式未文档化，v1 不冒险写入；zcode 同理，仅 stdio）。
/// DeepSeek Harness 例外：dsh-mcp-client 原生支持 Streamable HTTP
/// （registry 的 "http"），不支持 sse。
fn supports_transport(agent: &str, transport: &str) -> bool {
    match transport {
        "stdio" => is_mcp_target(agent),
        "sse" => matches!(agent, "claude_cli" | "claude_desktop"),
        "http" => matches!(agent, "claude_cli" | "claude_desktop" | "dsh"),
        _ => false,
    }
}

pub(crate) fn normalize_transport(transport: &str) -> &str {
    if transport.is_empty() {
        "stdio"
    } else {
        transport
    }
}

fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
}

// ── 目录存取（SQLite app_setting） ───────────────────────────────────────────

// Store 的 Option 读取接口会吞掉数据库/JSON 错误；此处必须区分不存在与损坏。
fn read_setting<T: DeserializeOwned>(conn: &Connection, key: &str) -> Result<Option<T>, String> {
    let text: Option<String> = conn
        .query_row(
            "SELECT value_json FROM app_settings WHERE setting_key = ?1",
            params![key],
            |row| row.get(0),
        )
        .optional()
        .map_err(|error| format!("读取 MCP 设置失败：{error}"))?;
    text.map(|text| {
        serde_json::from_str(&text).map_err(|_| format!("MCP 设置 {key} 已损坏，未修改目录"))
    })
    .transpose()
}

fn write_setting<T: Serialize + ?Sized>(
    conn: &Connection,
    key: &str,
    value: &T,
) -> Result<(), String> {
    let text = serde_json::to_string(value).map_err(|_| "MCP 设置序列化失败".to_string())?;
    conn.execute(
        "INSERT INTO app_settings (setting_key, value_json, updated_at)
         VALUES (?1, ?2, ?3)
         ON CONFLICT(setting_key) DO UPDATE SET
           value_json = excluded.value_json, updated_at = excluded.updated_at",
        params![key, text, chrono::Utc::now().to_rfc3339()],
    )
    .map_err(|error| format!("保存 MCP 设置失败：{error}"))?;
    Ok(())
}

/// 连接锁 + IMMEDIATE 事务串行化首次迁移及所有目录读改写。
/// 目录和完成标记同事务提交；任意读取/写入失败均回滚，不能把损坏当成空库。
fn with_catalog<T>(
    write: bool,
    action: impl FnOnce(&mut Vec<McpCatalogEntry>, &CatalogMigration) -> Result<T, String>,
) -> Result<T, String> {
    let store = crate::telemetry_store::shared_store().ok_or("本地数据库不可用")?;
    let mut conn = store.conn.lock().map_err(|_| "MCP 数据库锁不可用")?;
    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|error| format!("打开 MCP 目录事务失败：{error}"))?;
    let stored: Option<Vec<McpCatalogEntry>> = read_setting(&tx, MCP_CATALOG_SETTING_KEY)?;
    let previous: Option<CatalogMigration> = read_setting(&tx, MCP_MIGRATION_SETTING_KEY)?;
    let migrated = previous.as_ref().is_some_and(|state| state.completed);
    if migrated && stored.is_none() {
        return Err("MCP 迁移已完成但中央目录缺失，未修改目录".into());
    }
    let mut catalog = stored.unwrap_or_default();
    let state = if migrated {
        previous.ok_or("MCP 迁移状态缺失")?
    } else {
        // Reuse this transaction's connection; locking shared_store again would deadlock.
        let path = crate::memory_mcp::claude_desktop_config_path_with_connection(&tx)?;
        let legacy = match std::fs::read_to_string(&path) {
            Ok(text) => parse_legacy_config(&text)?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(error) => return Err(format!("读取旧 MCP 配置失败：{error}")),
        };
        CatalogMigration {
            completed: true,
            report: merge_legacy_entries(&mut catalog, legacy, &chrono::Utc::now().to_rfc3339()),
        }
    };
    // Older builds registered a disposable memory runtime in the catalog. Keep the
    // assignment list, but migrate that specific built-in entry to the stable launcher.
    let memory_migrated = migrate_legacy_memory_entry(&mut catalog)?;
    let result = action(&mut catalog, &state)?;
    if write || !migrated || memory_migrated {
        write_setting(&tx, MCP_CATALOG_SETTING_KEY, &catalog)?;
    }
    if !migrated {
        write_setting(&tx, MCP_MIGRATION_SETTING_KEY, &state)?;
    }
    tx.commit().map_err(|error| format!("提交 MCP 目录失败：{error}"))?;
    Ok(result)
}

fn migrate_legacy_memory_entry(catalog: &mut [McpCatalogEntry]) -> Result<bool, String> {
    let Some(entry) = catalog.iter_mut().find(|entry| entry.name == MEMORY_SERVER_NAME || entry.name == LEGACY_MEMORY_SERVER_NAME) else {
        return Ok(false);
    };
    let legacy_runtime = Path::new(&entry.command).file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.starts_with("agent-manager-memory-") && name.ends_with(".exe"));
    let changed = entry.name == LEGACY_MEMORY_SERVER_NAME;
    if normalize_transport(&entry.transport) != "stdio" || !legacy_runtime
        || entry.args != ["--mcp-memory"] || !entry.env.is_empty()
    {
        if changed {
            entry.name = MEMORY_SERVER_NAME.into();
            entry.updated_at = chrono::Utc::now().to_rfc3339();
            return Ok(true);
        }
        return Ok(false);
    }
    entry.name = MEMORY_SERVER_NAME.into();
    entry.command = crate::memory_mcp::registered_command()?;
    entry.updated_at = chrono::Utc::now().to_rfc3339();
    Ok(true)
}

fn read_catalog() -> Result<Vec<McpCatalogEntry>, String> {
    with_catalog(false, |catalog, _| Ok(catalog.clone()))
}

/// Workflow/旧 IPC 共用中央目录；不再从 Claude 文件维护第二份运行配置。
pub(crate) fn read_runtime_servers() -> Result<Vec<crate::mcp::McpServer>, String> {
    Ok(read_catalog()?.into_iter().map(runtime_server).collect())
}

fn runtime_server(entry: McpCatalogEntry) -> crate::mcp::McpServer {
    crate::mcp::McpServer {
        name: entry.name,
        transport: normalize_transport(&entry.transport).to_string(),
        command: entry.command,
        args: entry.args,
        env: entry.env.into_iter().collect(),
        url: entry.url,
        headers: entry.headers.into_iter().collect(),
        description: entry.description,
    }
}

fn same_configuration(a: &McpCatalogEntry, b: &McpCatalogEntry) -> bool {
    normalize_transport(&a.transport) == normalize_transport(&b.transport)
        && a.command == b.command
        && a.args == b.args
        && a.env == b.env
        && a.url == b.url
        && a.headers == b.headers
}

fn merge_legacy_entries(
    catalog: &mut Vec<McpCatalogEntry>,
    mut legacy: Vec<McpCatalogEntry>,
    now: &str,
) -> Vec<String> {
    legacy.sort_by(|a, b| a.name.cmp(&b.name));
    let mut conflicts = Vec::new();
    // 先保留所有原名，避免冲突副本抢占尚未迁入的 legacy-* 原名。
    for mut entry in legacy {
        match catalog.iter_mut().find(|existing| existing.name == entry.name) {
            Some(existing) if same_configuration(existing, &entry) => {
                if existing.description.is_empty() {
                    existing.description = entry.description;
                }
                // 中央条目的 assigned_agents/时间戳保持原样。
            }
            Some(_) => conflicts.push(entry),
            None => {
                entry.created_at = now.to_string();
                entry.updated_at = now.to_string();
                catalog.push(entry);
            }
        }
    }
    let mut report = Vec::new();
    for mut entry in conflicts {
        let original = entry.name.clone();
        let base = format!("legacy-{original}");
        let mut candidate = base.clone();
        let mut suffix = 2;
        loop {
            match catalog.iter().find(|existing| existing.name == candidate) {
                // 即使重试时目录已包含副本，也复用配置相同的稳定名称。
                Some(existing) if same_configuration(existing, &entry) => break,
                Some(_) => {
                    candidate = format!("{base}-{suffix}");
                    suffix += 1;
                }
                None => {
                    entry.name = candidate.clone();
                    entry.created_at = now.to_string();
                    entry.updated_at = now.to_string();
                    catalog.push(entry);
                    break;
                }
            }
        }
        // 不输出 env/header/url/command 或描述，报告只包含名称映射。
        report.push(format!(
            "旧 MCP {original:?} 与中央目录同名但配置不同，已保留为 {candidate:?}；请核对引用原名的旧工作流。"
        ));
    }
    report
}

fn find_entry(catalog: &[McpCatalogEntry], name: &str) -> Result<McpCatalogEntry, String> {
    catalog
        .iter()
        .find(|entry| entry.name == name)
        .cloned()
        .ok_or_else(|| format!("MCP 库中不存在 {name}"))
}

/// 把 agent 加入/移出条目的 assigned_agents（去重排序）后写回目录。
fn save_assignment(name: &str, agent: &str, equipped: bool) -> Result<McpCatalogEntry, String> {
    with_catalog(true, |catalog, _| {
        let entry = catalog.iter_mut().find(|entry| entry.name == name)
            .ok_or_else(|| format!("MCP 库中不存在 {name}"))?;
        if equipped {
            if !entry.assigned_agents.iter().any(|a| a == agent) {
                entry.assigned_agents.push(agent.to_string());
                entry.assigned_agents.sort();
                entry.assigned_agents.dedup();
            }
        } else {
            entry.assigned_agents.retain(|a| a != agent);
        }
        entry.updated_at = chrono::Utc::now().to_rfc3339();
        Ok(entry.clone())
    })
}

// ── 各 Agent 配置的读写 ──────────────────────────────────────────────────────

/// 文件型 JSON 配置的位置。claude_desktop/qoder/... 复用 memory_mcp 的解析
/// （含跨设备目录覆盖）；claude_cli 的 user scope 存在 `~/.claude.json`。
fn json_config_path(agent: &str) -> Result<Option<PathBuf>, String> {
    match agent {
        "claude_desktop" | "qoder" | "workbuddy" | "minimax" | "kimi" | "zcode" => {
            desktop_config_path(agent)
        }
        "claude_cli" => Ok(dirs_next::home_dir().map(|home| home.join(".claude.json"))),
        _ => Ok(None),
    }
}

fn codex_config_path() -> Option<PathBuf> {
    if let Ok(home) = std::env::var("CODEX_HOME") {
        if !home.trim().is_empty() {
            return Some(PathBuf::from(home).join("config.toml"));
        }
    }
    dirs_next::home_dir().map(|home| home.join(".codex").join("config.toml"))
}

/// 读某 Agent 当前的全部 MCP 服务器定义（name → 配置 Value）。
/// 只读文件：codex 解析 config.toml，其余读 JSON 的服务器映射节点
/// （zcode 嵌套在 mcp.servers，其余为根级 mcpServers）；
/// dsh 解析 cordis.patch.yml 中的 dsh-mcp-client 条目（规范 JSON 形状）。
fn read_agent_mcp_servers(agent: &str) -> Result<Map<String, Value>, String> {
    if agent == "dsh" {
        return crate::dsh_mcp::read_servers();
    }
    if agent.starts_with("codex") {
        let path = codex_config_path().ok_or("无法定位 Codex MCP 配置")?;
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Map::new()),
            Err(error) => return Err(format!("读取 Codex MCP 配置失败：{error}")),
        };
        let table = text.parse::<toml::Table>().map_err(|_| "Codex MCP 配置 TOML 损坏")?;
        let Some(servers) = table.get("mcp_servers") else {
            return Ok(Map::new());
        };
        serde_json::to_value(servers).map_err(|_| "Codex MCP 配置转换失败")?
            .as_object().cloned().ok_or_else(|| "Codex mcp_servers 必须是对象".into())
    } else {
        let path = json_config_path(agent)?.ok_or("无法定位 Agent MCP 配置")?;
        let config = read_json_config(&path)?;
        let container = if agent == "zcode" {
            match config.get("mcp") {
                Some(value) if !value.is_object() => return Err("zcode mcp 必须是对象".into()),
                Some(value) => value.get("servers"),
                None => None,
            }
        } else {
            config.get("mcpServers")
        };
        if container.is_some_and(|value| !value.is_object()) {
            return Err(format!("{agent} MCP 服务器映射必须是对象"));
        }
        Ok(servers_map(&config, agent).cloned().unwrap_or_default())
    }
}

/// 库条目 → 某 Agent JSON 配置里的 mcpServers 条目形状。
/// minimax 需要 type/enabled；claude 系远程用 {type,url,headers}。
fn agent_entry_value(agent: &str, entry: &McpCatalogEntry) -> Value {
    let transport = normalize_transport(&entry.transport);
    let mut value = json!({});
    if transport == "stdio" {
        value["command"] = json!(entry.command);
        value["args"] = json!(entry.args);
        if !entry.env.is_empty() {
            value["env"] = json!(entry.env);
        }
        if agent == "minimax" {
            value["type"] = json!("stdio");
            value["enabled"] = json!(true);
        }
    } else {
        value["type"] = json!(transport);
        value["url"] = json!(entry.url);
        if !entry.headers.is_empty() {
            value["headers"] = json!(entry.headers);
        }
        if agent == "minimax" {
            value["enabled"] = json!(true);
        }
    }
    if !entry.description.is_empty() && agent != "zcode" && agent != "dsh" {
        // zcode 与 dsh-mcp-client 的服务器 schema 均未文档化 description
        // 扩展键，保持最小形状。
        value["description"] = json!(entry.description);
    }
    value
}

fn config_field<T: DeserializeOwned + Default>(value: &Value, key: &str) -> Result<T, String> {
    match value.get(key) {
        None => Ok(T::default()),
        Some(field) => serde_json::from_value(field.clone())
            .map_err(|_| format!("MCP 字段 {key} 类型无效")),
    }
}

fn checked_transport(transport: &str) -> Result<&str, String> {
    match normalize_transport(transport) {
        transport @ ("stdio" | "sse" | "http") => Ok(transport),
        _ => Err("transport 仅支持 stdio、sse 或 http".into()),
    }
}

fn config_transport(value: &Value) -> Result<String, String> {
    let transport: String = config_field(value, "transport")?;
    let kind: String = config_field(value, "type")?;
    if !transport.is_empty() {
        checked_transport(&transport)?;
    }
    if !kind.is_empty() {
        checked_transport(&kind)?;
    }
    if !transport.is_empty() && !kind.is_empty() && transport != kind {
        return Err("MCP transport 与 type 不一致".into());
    }
    if !transport.is_empty() {
        return Ok(transport);
    }
    if !kind.is_empty() {
        return Ok(kind);
    }
    let url: String = config_field(value, "url")?;
    Ok(if url.is_empty() { "stdio" } else { "http" }.into())
}

fn validate_configuration(entry: &McpCatalogEntry) -> Result<(), String> {
    let transport = checked_transport(&entry.transport)?;
    if transport == "stdio" && entry.command.trim().is_empty() {
        return Err("stdio 服务器必须提供启动命令".into());
    }
    if transport != "stdio" && entry.url.trim().is_empty() {
        return Err("远程服务器必须提供 URL".into());
    }
    Ok(())
}

fn parse_config_entry(name: &str, value: &Value) -> Result<McpCatalogEntry, String> {
    if !value.is_object() {
        return Err("MCP 条目必须是对象".into());
    }
    // 严格解析所有运行字段，不按 transport 丢弃另一组字段，也不静默滤掉坏值。
    let entry = McpCatalogEntry {
        name: name.to_string(),
        transport: config_transport(value)?,
        description: config_field(value, "description")?,
        command: config_field(value, "command")?,
        args: config_field(value, "args")?,
        env: config_field(value, "env")?,
        url: config_field(value, "url")?,
        headers: config_field(value, "headers")?,
        ..Default::default()
    };
    validate_configuration(&entry)?;
    Ok(entry)
}

fn parse_json_config(text: &str) -> Result<Value, String> {
    // serde 的错误消息可能包含配置值，因此不把反序列化详情带给 UI。
    let config: Value = serde_json::from_str(text)
        .map_err(|_| "MCP 配置为空或 JSON 损坏，未修改配置".to_string())?;
    if !config.is_object() {
        return Err("MCP 配置根节点必须是对象".into());
    }
    Ok(config)
}

fn read_json_config(path: &Path) -> Result<Value, String> {
    match std::fs::read_to_string(path) {
        Ok(text) => parse_json_config(&text),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(json!({})),
        Err(error) => Err(format!("读取 MCP 配置失败：{error}")),
    }
}

fn parse_legacy_config(text: &str) -> Result<Vec<McpCatalogEntry>, String> {
    let config = parse_json_config(text)?;
    let Some(servers) = config.get("mcpServers") else {
        return Ok(Vec::new()); // 合法的 Claude 配置可能尚无 MCP 节点。
    };
    let servers = servers.as_object().ok_or("旧 MCP 配置 mcpServers 必须是对象")?;
    servers.iter().map(|(name, value)| {
        parse_config_entry(name, value)
            .map_err(|error| format!("旧 MCP {name:?} 无法迁移：{error}"))
    }).collect()
}

/// 从 Agent 配置反推库条目；transport/type/URL 推断与 legacy 迁移一致。
fn entry_from_config(name: &str, agent: &str, value: &Value) -> Result<McpCatalogEntry, String> {
    let mut entry = parse_config_entry(name, value)
        .map_err(|error| format!("{agent} MCP {name:?} 无法导入：{error}"))?;
    // 显式导入才标记装备；legacy 管理文件不代表当前 Agent 分发状态。
    entry.assigned_agents = vec![agent.to_string()];
    Ok(entry)
}

/// Normalize only unambiguous Windows absolute paths. Relative paths, URLs,
/// option values and arbitrary strings retain their original meaning.
fn normalized_windows_path(value: &str) -> String {
    // Extended/device namespaces do not share ordinary Win32 slash semantics.
    if value.starts_with(r"\\?\") || value.starts_with(r"\\.\") { return value.to_string(); }
    let bytes = value.as_bytes();
    let drive_absolute = bytes.len() >= 3 && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':' && matches!(bytes[2], b'/' | b'\\');
    if drive_absolute || value.starts_with("\\\\") {
        value.replace('\\', "/")
    } else { value.to_string() }
}

fn simple_npx_token(value: &str) -> bool {
    !value.is_empty() && !value.chars().any(|c| c.is_whitespace()
        || matches!(c, '&' | '|' | '<' | '>' | '^' | '%' | '!' | '"' | '\'' | '(' | ')'))
}

/// cmd /c npx is a common Windows launch shim. Do not unwrap general shells,
/// command strings, shell expansion, extra flags or executable paths from PATH.
fn normalized_stdio_launch(value: &Value) -> (Value, Value) {
    let mut command = value.get("command").cloned().unwrap_or(json!(""));
    let mut args = value.get("args").cloned().unwrap_or(json!([]));
    if let (Some(executable), Some(tokens)) = (command.as_str(), args.as_array()) {
        if matches!(executable.to_ascii_lowercase().as_str(), "cmd" | "cmd.exe")
            && tokens.len() >= 2
            && tokens[0].as_str().is_some_and(|token| token.eq_ignore_ascii_case("/c"))
            && tokens[1].as_str().is_some_and(|token| matches!(token, "npx" | "npx.cmd"))
            && tokens.iter().skip(2).all(|token| token.as_str().is_some_and(simple_npx_token))
        {
            command = json!("npx");
            args = Value::Array(tokens[2..].to_vec());
        }
    }
    if let Some(executable) = command.as_str() {
        command = json!(normalized_windows_path(executable));
    }
    if let Some(tokens) = args.as_array_mut() {
        for token in tokens {
            if let Some(text) = token.as_str() { *token = json!(normalized_windows_path(text)); }
        }
    }
    (command, args)
}

/// 忽略 description/enabled 等噪音，只保留语义字段并补默认值的形状。
fn normalized_shape(value: &Value) -> Value {
    let transport = match config_transport(value) {
        Ok(transport) => transport,
        Err(_) => return json!({"invalid_transport": true}),
    };
    let mut shape = json!({ "type": transport });
    if transport == "stdio" {
        let (command, args) = normalized_stdio_launch(value);
        shape["command"] = command;
        shape["args"] = args;
        shape["env"] = value.get("env").cloned().unwrap_or(json!({}));
    } else {
        shape["url"] = value.get("url").cloned().unwrap_or(json!(""));
        shape["headers"] = value.get("headers").cloned().unwrap_or(json!({}));
    }
    shape
}

/// 语义相等：对象按键比较（与键序无关），数组与标量按值比较。
fn value_equal(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Object(x), Value::Object(y)) => {
            x.len() == y.len()
                && x.iter()
                    .all(|(k, v)| y.get(k).is_some_and(|w| value_equal(v, w)))
        }
        _ => a == b,
    }
}

fn entry_matches_config(agent: &str, entry: &McpCatalogEntry, configured: &Value) -> bool {
    let expected = agent_entry_value(agent, entry);
    value_equal(&normalized_shape(&expected), &normalized_shape(configured))
}

fn configuration_differences(expected: &Value, configured: &Value) -> Vec<String> {
    let expected = normalized_shape(expected);
    let configured = normalized_shape(configured);
    let fields: std::collections::BTreeSet<_> = expected.as_object().unwrap().keys()
        .chain(configured.as_object().unwrap().keys()).collect();
    fields.into_iter().filter(|key| expected.get(*key) != configured.get(*key))
        .map(|key| if key == "type" { "transport".into() } else { key.clone() }).collect()
}

/// Every physical configuration is read once, including files not managed by us.
fn read_mcp_snapshots() -> HashMap<String, Result<Map<String, Value>, String>> {
    let mut snapshots = HashMap::new();
    for agent in MCP_TARGETS {
        let physical = physical_target(agent);
        snapshots.entry(physical.to_string()).or_insert_with(|| read_agent_mcp_servers(physical));
    }
    snapshots
}

fn collect_agent_statuses(
    catalog: &[McpCatalogEntry],
    snapshots: &HashMap<String, Result<Map<String, Value>, String>>,
    observation: &[ObservationManifest],
) -> Vec<McpAgentStatus> {
    let mut result = Vec::new();
    for entry in catalog {
        for agent in MCP_TARGETS {
            let assigned = entry.assigned_agents.iter().any(|target| target == agent);
            let Some(snapshot) = snapshots.get(physical_target(agent)) else { continue };
            let (state, differing_fields, error) = match snapshot {
                Err(error) => ("unavailable", Vec::new(), Some(error.clone())),
                Ok(servers) => match servers.get(&entry.name) {
                    None if !assigned => continue,
                    None => ("missing", Vec::new(), None),
                    Some(configured) => {
                        let observed_match = observation.iter().any(|manifest| manifest.name == entry.name
                            && manifest.agent == physical_target(agent)
                            && observation_matches_entry(manifest, entry, configured));
                        let matches = observed_match || entry_matches_config(agent, entry, configured);
                        let fields = if matches { Vec::new() } else {
                            configuration_differences(&agent_entry_value(agent, entry), configured)
                        };
                        (if matches { "installed" } else { "differs" }, fields, None)
                    }
                },
            };
            result.push(McpAgentStatus {
                name: entry.name.clone(), agent: agent.into(), state: state.into(), differing_fields, error,
            });
        }
    }
    result
}

fn collect_import_candidates(
    catalog: &[McpCatalogEntry],
    snapshots: &HashMap<String, Result<Map<String, Value>, String>>,
    observation: &[ObservationManifest],
) -> McpImportScan {
    let mut result = Vec::new();
    let mut issues = Vec::new();
    // Each name keeps every source. Codex CLI/Desktop deliberately share one source.
    for agent in MCP_TARGETS.into_iter().filter(|agent| *agent != "codex_desktop") {
        let Some(snapshot) = snapshots.get(agent) else { continue };
        let servers = match snapshot {
            Ok(servers) => servers,
            Err(error) => {
                issues.push(McpImportIssue { agent: agent.into(), name: None, message: error.clone() });
                continue;
            }
        };
        for (name, value) in servers {
            if name == MEMORY_SERVER_NAME || name == LEGACY_MEMORY_SERVER_NAME { continue }
            if is_observation_proxy_node(value) || observation.iter().any(|manifest| manifest.name == *name
                && manifest.agent == physical_target(agent) && proxy_matches(value, manifest)) {
                continue;
            }
            let entry = match entry_from_config(name, agent, value) {
                Ok(entry) => entry,
                Err(error) => {
                    issues.push(McpImportIssue { agent: agent.into(), name: Some(name.clone()), message: error });
                    continue;
                }
            };
            let central = catalog.iter().find(|existing| existing.name == *name);
            let fields = central.map(|central| configuration_differences(&agent_entry_value(agent, central), value))
                .unwrap_or_default();
            result.push(McpImportCandidate {
                agent: agent.into(), entry, already_in_catalog: central.is_some(),
                catalog_state: if central.is_none() { "new" } else if fields.is_empty() { "matches" } else { "differs" }.into(),
                differing_fields: fields, variant: 0, variant_count: 0, peer_differing_fields: Vec::new(),
            });
        }
    }
    let mut variants: HashMap<String, Vec<Value>> = HashMap::new();
    for candidate in &mut result {
        let shape = normalized_shape(&agent_entry_value(&candidate.agent, &candidate.entry));
        let shapes = variants.entry(candidate.entry.name.clone()).or_default();
        let index = match shapes.iter().position(|existing| value_equal(existing, &shape)) {
            Some(index) => index,
            None => { shapes.push(shape); shapes.len() - 1 }
        };
        candidate.variant = index + 1;
    }
    for candidate in &mut result {
        let shapes = &variants[&candidate.entry.name];
        candidate.variant_count = shapes.len();
        candidate.peer_differing_fields = shapes.iter().skip(1)
            .flat_map(|shape| configuration_differences(&shapes[0], shape))
            .collect::<std::collections::BTreeSet<_>>().into_iter().collect();
    }
    result.sort_by(|a, b| a.entry.name.cmp(&b.entry.name).then(a.agent.cmp(&b.agent)));
    McpImportScan { candidates: result, issues }
}

// ── 装备 / 卸载的写入路径 ────────────────────────────────────────────────────

fn write_json_entry(agent: &str, name: &str, entry_value: &Value) -> Result<(), String> {
    let path = json_config_path(agent)?.ok_or("无法定位 MCP 配置文件")?;
    let mut config = read_json_config(&path)?;
    let servers = servers_map_mut(&mut config, agent)?;
    servers.insert(name.to_string(), entry_value.clone());
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    std::fs::write(
        &path,
        serde_json::to_string_pretty(&config).map_err(|error| error.to_string())?,
    )
    .map_err(|error| format!("写入 {} 失败：{error}", path.display()))
}

fn remove_json_entry(agent: &str, name: &str) -> Result<(), String> {
    let path = json_config_path(agent)?.ok_or("无法定位 MCP 配置文件")?;
    let mut config = read_json_config(&path)?;
    servers_map_mut(&mut config, agent)?.remove(name);
    std::fs::write(
        &path,
        serde_json::to_string_pretty(&config).map_err(|error| error.to_string())?,
    )
    .map_err(|error| format!("写入 {} 失败：{error}", path.display()))
}

/// claude/codex CLI 型装备：先移除旧配置再 add（覆盖式，与
/// memory_mcp_install 一致）。Codex env 走 `--env K=V`，Claude 走 `-e K=V`；
/// 远程走 `--transport`。
fn cli_equip(agent: &str, entry: &McpCatalogEntry) -> Result<(), String> {
    let cli = if agent.starts_with("codex") {
        "codex"
    } else {
        "claude"
    };
    let transport = normalize_transport(&entry.transport);
    let _ = run_agent_cli(cli, &["mcp".into(), "remove".into(), entry.name.clone()]);
    let mut args = vec!["mcp".into(), "add".into()];
    if agent == "claude_cli" {
        args.push("--scope".into());
        args.push("user".into());
        if transport != "stdio" {
            args.push("--transport".into());
            args.push(transport.to_string());
        }
    } else if transport != "stdio" {
        return Err("Codex 暂不支持远程 MCP 装备".into());
    }
    for (key, value) in &entry.env {
        args.push(if cli == "codex" { "--env" } else { "-e" }.into());
        args.push(format!("{key}={value}"));
    }
    args.push(entry.name.clone());
    if transport == "stdio" {
        args.push("--".into());
        args.push(entry.command.clone());
        args.extend(entry.args.iter().cloned());
    } else {
        args.push(entry.url.clone());
        for (key, value) in &entry.headers {
            args.push("--header".into());
            args.push(format!("{key}: {value}"));
        }
    }
    let output = run_agent_cli(cli, &args)?;
    if !output.status.success() {
        return Err(format!(
            "配置 {} MCP 失败：{}",
            agent_label(agent),
            cli_detail(&output)
        ));
    }
    Ok(())
}

fn apply_equip(agent: &str, entry: &McpCatalogEntry) -> Result<(), String> {
    validate_configuration(entry)?;
    if agent == "claude_cli" || agent.starts_with("codex") {
        cli_equip(agent, entry)
    } else if agent == "dsh" {
        let server = crate::dsh_mcp::DshMcpServer::from_catalog(entry)?;
        crate::dsh_mcp::write_server(&server)
    } else {
        write_json_entry(agent, &entry.name, &agent_entry_value(agent, entry))
    }
}

fn apply_unequip(agent: &str, name: &str) -> Result<(), String> {
    if agent == "claude_cli" || agent.starts_with("codex") {
        let cli = if agent.starts_with("codex") {
            "codex"
        } else {
            "claude"
        };
        let output = run_agent_cli(cli, &["mcp".into(), "remove".into(), name.to_string()])?;
        if !output.status.success() {
            return Err(format!(
                "移除 {} MCP 失败：{}",
                agent_label(agent),
                cli_detail(&output)
            ));
        }
        Ok(())
    } else if agent == "dsh" {
        crate::dsh_mcp::remove_server(name)
    } else {
        remove_json_entry(agent, name)
    }
}

// ── MCP stdio observation control plane ────────────────────────────────────

/// The relay receives only an opaque id. Its original command and environment
/// come from the catalog after checking the pinned fingerprint; the manifest
/// therefore never duplicates credentials in a separate, less protected file.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct ObservationManifest {
    id: String,
    name: String,
    agent: String,
    config_path: PathBuf,
    fingerprint: String,
    proxy_command: String,
    #[serde(default)]
    relay_command: Option<String>,
    #[serde(default)]
    previous_proxy_command: Option<String>,
    #[serde(default)]
    previous_relay_command: Option<String>,
    #[serde(default)]
    omitted_args: bool,
    phase: String, // preparing | active | recovery
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub struct McpObservationStatus {
    pub name: String,
    pub agent: String,
    pub state: String,
    pub active_connections: i64,
}

pub struct RelayLaunch {
    pub server_name: String,
    pub command: String,
    pub args: Vec<String>,
    pub env: BTreeMap<String, String>,
}

fn observation_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

fn physical_target(agent: &str) -> &str {
    if matches!(agent, "codex_cli" | "codex_desktop") { "codex_cli" } else { agent }
}

fn observation_root() -> Result<PathBuf, String> {
    dirs_next::config_dir()
        .map(|dir| dir.join("agent-manager").join("mcp-observation"))
        .ok_or_else(|| "无法定位 MCP observation 数据目录".into())
}

/// 旧版本曾允许对共享记忆 MCP（agent-manager-memory）启用观测：配置被包成
/// relay 代理，而记忆服务器不在 MCP 目录里，应用换代后 assignment 无法再
/// 解析，客户端便断连，且状态页永远卡在「配置已变化」。这些遗留观测一律
/// 退役——把配置节点恢复为稳定启动器直连，再删除观测状态。共享记忆的调用
/// 统计由服务端自己记录，不需要也不允许观测（见 `mcp_observation_set`）。
/// Restore only orphaned shared-memory relays installed by this application's launcher.
/// A live assignment stays untouched, as does any relay whose provenance is unclear.
pub(crate) fn repair_orphaned_memory_relay(agent: &str, launcher: &str) -> Result<(), String> {
    if !matches!(agent, "claude_desktop" | "kimi" | "qoder" | "workbuddy" | "minimax" | "zcode") {
        return Ok(());
    }
    let _guard = observation_lock().lock().map_err(|_| "MCP observation 锁不可用")?;
    let path = observation_config_path(agent)?;
    if !path.is_file() { return Ok(()); }
    let Some(current) = read_observation_node(agent, &path, MEMORY_SERVER_NAME)? else { return Ok(()) };
    let Some(args) = current.get("args").and_then(Value::as_array) else { return Ok(()) };
    if args.len() != 2 || args[0] != "--mcp-relay" { return Ok(()); }
    let Some(id) = args[1].as_str().filter(|id| uuid::Uuid::parse_str(id).is_ok()) else { return Ok(()) };
    let Some(command) = current.get("command").and_then(Value::as_str) else { return Ok(()) };
    let registered = Path::new(launcher);
    let proxy = Path::new(command);
    let owned_launcher = proxy.parent() == registered.parent()
        && proxy.file_name().and_then(|name| name.to_str())
            .is_some_and(|name| name.starts_with("mcp-launcher-") && name.ends_with(".exe"));
    if !owned_launcher || observation_root()?.join(format!("{id}.json")).exists() {
        return Ok(());
    }
    let manifest = ObservationManifest {
        id: id.to_string(), name: MEMORY_SERVER_NAME.into(), agent: agent.into(),
        config_path: path, fingerprint: String::new(), proxy_command: command.into(),
        relay_command: None, previous_proxy_command: None, previous_relay_command: None, omitted_args: false,
        phase: "recovery".into(),
    };
    let replacement = crate::memory_mcp::file_server_entry(agent, launcher);
    patch_observation_node(&manifest, &current, &replacement)
}

fn retire_memory_observation() {
    let Ok(manifests) = observation_manifests() else { return };
    for manifest in manifests {
        if manifest.name != MEMORY_SERVER_NAME && manifest.name != LEGACY_MEMORY_SERVER_NAME { continue; }
        let Ok(launcher) = crate::memory_mcp::registered_command() else { continue };
        let mut node = crate::memory_mcp::file_server_entry(&manifest.agent, &launcher);
        if manifest.agent.starts_with("codex") {
            if let Some(fields) = node.as_object_mut() { fields.remove("description"); }
        }
        if observation_config_path(&manifest.agent).ok().as_ref() != Some(&manifest.config_path)
            || crate::telemetry_store::shared_store()
                .and_then(|store| store.proxy_active_connections(&manifest.id).ok()) != Some(0)
            || fallback_connections(&manifest.id) > 0 { continue; }
        let current = read_observation_node(&manifest.agent, &manifest.config_path, &manifest.name);
        match current {
            Ok(Some(current)) if proxy_matches(&current, &manifest) => {
                if patch_observation_node(&manifest, &current, &node).is_err() { continue; }
            }
            Ok(Some(current)) if current == node => {},
            _ => continue,
        }
        let _ = remove_observation(&manifest);
    }
}

fn observation_manifests() -> Result<Vec<ObservationManifest>, String> {
    let root = observation_root()?;
    let files = match std::fs::read_dir(root) {
        Ok(files) => files,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(_) => return Err("读取 MCP observation 状态失败".into()),
    };
    let mut manifests = Vec::new();
    for file in files {
        let file = file.map_err(|_| "读取 MCP observation 状态失败")?;
        if file.path().extension().is_some_and(|ext| ext == "json") {
            if file.file_type().map_err(|_| "读取 MCP observation 状态失败")?.is_symlink() {
                return Err("MCP observation 状态路径无效".into());
            }
            let bytes = std::fs::read(file.path()).map_err(|_| "读取 MCP observation 状态失败")?;
            let manifest: ObservationManifest = serde_json::from_slice(&bytes)
                .map_err(|_| "MCP observation 状态已损坏，请手动恢复")?;
            if uuid::Uuid::parse_str(&manifest.id).is_err()
                || file.file_name().to_string_lossy() != format!("{}.json", manifest.id)
                || !matches!(manifest.phase.as_str(), "preparing" | "active" | "recovery")
            {
                return Err("MCP observation 状态已损坏，请手动恢复".into());
            }
            manifests.push(manifest);
        }
    }
    Ok(manifests)
}

fn save_observation(manifest: &ObservationManifest) -> Result<(), String> {
    let root = observation_root()?;
    std::fs::create_dir_all(&root).map_err(|_| "无法创建 MCP observation 状态目录")?;
    let mut temporary = tempfile::NamedTempFile::new_in(&root)
        .map_err(|_| "无法准备 MCP observation 状态")?;
    serde_json::to_writer(&mut temporary, manifest)
        .map_err(|_| "无法编码 MCP observation 状态")?;
    use std::io::Write;
    temporary.flush().map_err(|_| "无法保存 MCP observation 状态")?;
    temporary.as_file().sync_all().map_err(|_| "无法保存 MCP observation 状态")?;
    let path = root.join(format!("{}.json", manifest.id));
    temporary.persist(path).map_err(|_| "无法保存 MCP observation 状态")?;
    Ok(())
}

fn remove_observation(manifest: &ObservationManifest) -> Result<(), String> {
    std::fs::remove_file(observation_root()?.join(format!("{}.json", manifest.id)))
        .map_err(|_| "无法移除 MCP observation 状态".into())
}

fn observation_canonical(agent: &str, entry: &McpCatalogEntry) -> Value {
    let mut value = agent_entry_value(agent, entry);
    if agent.starts_with("codex") { value.as_object_mut().unwrap().remove("description"); }
    value
}

fn observation_fingerprint(agent: &str, entry: &McpCatalogEntry) -> String {
    let canonical = observation_canonical(agent, entry);
    let bytes = serde_json::to_vec(&canonical).unwrap_or_default();
    format!("{:x}", Sha256::digest(bytes))
}

fn observation_config_path(agent: &str) -> Result<PathBuf, String> {
    if agent.starts_with("codex") {
        codex_config_path().ok_or_else(|| "无法定位 Codex MCP 配置".into())
    } else {
        json_config_path(agent)?.ok_or_else(|| "无法定位 Agent MCP 配置".into())
    }
}

fn proxy_entry(manifest: &ObservationManifest) -> Value {
    let mut value = json!({"command": manifest.proxy_command, "args": ["--mcp-relay", manifest.id]});
    if manifest.agent == "minimax" {
        value["type"] = json!("stdio");
        value["enabled"] = json!(true);
    }
    value
}

fn is_observation_proxy_node(value: &Value) -> bool {
    value.get("args").and_then(Value::as_array)
        .is_some_and(|args| args.first().is_some_and(|arg| arg == "--mcp-relay"))
}

fn proxy_matches(value: &Value, manifest: &ObservationManifest) -> bool {
    value_equal(value, &proxy_entry(manifest))
}

/// Keep the pinned fingerprint exact: normalization is for comparing direct
/// configurations, never for silently changing a running relay's upstream.
fn observation_matches_entry(manifest: &ObservationManifest, entry: &McpCatalogEntry, node: &Value) -> bool {
    manifest.phase == "active" && normalize_transport(&entry.transport) == "stdio"
        && observation_fingerprint(&manifest.agent, entry) == manifest.fingerprint
        && entry.assigned_agents.iter().any(|agent| physical_target(agent) == manifest.agent)
        && proxy_matches(node, manifest)
}

fn canonical_for_manifest(manifest: &ObservationManifest, entry: &McpCatalogEntry) -> Value {
    let mut canonical = observation_canonical(&manifest.agent, entry);
    if manifest.omitted_args { canonical.as_object_mut().unwrap().remove("args"); }
    canonical
}

fn canonical_matches(agent: &str, entry: &McpCatalogEntry, value: &Value) -> bool {
    // Exact equality avoids losing optional fields and custom behavior.
    let canonical = observation_canonical(agent, entry);
    if value_equal(value, &canonical) { return true; }
    // Codex CLI commonly omits args when empty. Record that shape so disable
    // restores the exact original JSON/TOML value.
    if agent.starts_with("codex") && entry.args.is_empty() {
        let mut omitted = canonical;
        omitted.as_object_mut().unwrap().remove("args");
        return value_equal(value, &omitted);
    }
    false
}

fn read_observation_node(agent: &str, path: &Path, name: &str) -> Result<Option<Value>, String> {
    if agent.starts_with("codex") {
        let text = std::fs::read_to_string(path).map_err(|_| "读取 Codex MCP 配置失败")?;
        let config: toml::Table = text.parse().map_err(|_| "Codex MCP 配置 TOML 损坏")?;
        let node = config.get("mcp_servers").and_then(|value| value.as_table()).and_then(|servers| servers.get(name));
        node.map(|node| serde_json::to_value(node).map_err(|_| "Codex MCP 配置转换失败".into())).transpose()
    } else {
        let config = read_json_config(path)?;
        Ok(servers_map(&config, agent).and_then(|servers| servers.get(name)).cloned())
    }
}

fn toml_header_path(line: &str) -> Option<Vec<String>> {
    let header = line.trim();
    if !header.starts_with('[') || header.starts_with("[[") || !header.ends_with(']') { return None; }
    let sample = format!("{header}\n_observation_probe = true\n");
    let parsed: toml::Table = sample.parse().ok()?;
    let mut table = &parsed;
    let mut path = Vec::new();
    loop {
        if table.contains_key("_observation_probe") { return Some(path); }
        let (key, child) = table.iter().next()?;
        path.push(key.clone());
        table = child.as_table()?;
    }
}

fn toml_literal(value: &str) -> String { toml::Value::String(value.to_string()).to_string() }

fn codex_node_lines(manifest: &ObservationManifest, node: &Value) -> Result<String, String> {
    let command = node.get("command").and_then(Value::as_str).ok_or("MCP command 格式无效")?;
    let mut lines = format!("[mcp_servers.{}]\ncommand = {}\n", toml_literal(&manifest.name), toml_literal(command));
    if let Some(args) = node.get("args") {
        let args = args.as_array().ok_or("MCP args 格式无效")?;
        let args: Option<Vec<String>> = args.iter().map(|arg| arg.as_str().map(toml_literal)).collect();
        lines.push_str(&format!("args = [{}]\n", args.ok_or("MCP args 格式无效")?.join(", ")));
    }
    if let Some(env) = node.get("env") {
        let env = env.as_object().ok_or("MCP env 格式无效")?;
        let fields: Option<Vec<String>> = env.iter().map(|(key, value)|
            value.as_str().map(|value| format!("{} = {}", toml_literal(key), toml_literal(value)))
        ).collect();
        lines.push_str(&format!("env = {{ {} }}\n", fields.ok_or("MCP env 格式无效")?.join(", ")));
    }
    Ok(lines)
}

fn patch_codex_node(text: &str, manifest: &ObservationManifest, replacement: &Value) -> Result<String, String> {
    // Locate a dedicated [mcp_servers.name] section and all of its child
    // tables; preserve every byte outside that section (including comments).
    let mut offset = 0;
    let mut start = None;
    let mut end = None;
    let mut in_target = false;
    for line in text.split_inclusive('\n') {
        let trimmed = line.trim();
        if trimmed.starts_with('[') {
            let path = toml_header_path(trimmed).ok_or("Codex MCP 表头不受支持，请手动恢复")?;
            let target = path.len() >= 2 && path[0] == "mcp_servers" && path[1] == manifest.name;
            if target && start.is_none() {
                if path.len() != 2 { return Err("Codex MCP 表头不受支持，请手动恢复".into()); }
                start = Some(offset);
            } else if in_target && !target && end.is_none() {
                end = Some(offset);
            } else if target && end.is_some() {
                return Err("Codex MCP 表分散，无法安全修改".into());
            }
            in_target = target;
        }
        offset += line.len();
    }
    let start = start.ok_or("Codex MCP 必须使用独立服务器表，无法安全修改")?;
    let end = end.unwrap_or(text.len());
    let mut next = String::with_capacity(text.len() + 128);
    next.push_str(&text[..start]);
    next.push_str(&codex_node_lines(manifest, replacement)?);
    if text[start..end].ends_with("\n\n") {
        next.push('\n');
    }
    next.push_str(&text[end..]);
    next.parse::<toml::Table>().map_err(|_| "Codex MCP 修改未通过 TOML 验证")?;
    Ok(next)
}

fn atomic_observation_write(path: &Path, text: &str) -> Result<(), String> {
    let parent = path.parent().ok_or("MCP 配置目录无效")?;
    let mut temporary = tempfile::NamedTempFile::new_in(parent)
        .map_err(|_| "无法准备 MCP 配置更新")?;
    use std::io::Write;
    let permissions = std::fs::metadata(path).map_err(|_| "无法读取 MCP 配置权限")?.permissions();
    temporary.as_file().set_permissions(permissions).map_err(|_| "无法保持 MCP 配置权限")?;
    temporary.write_all(text.as_bytes()).map_err(|_| "无法写入 MCP 配置更新")?;
    temporary.as_file().sync_all().map_err(|_| "无法同步 MCP 配置更新")?;
    temporary.persist(path).map_err(|_| "无法提交 MCP 配置更新")?;
    Ok(())
}

fn patch_observation_node(manifest: &ObservationManifest, expected: &Value, replacement: &Value) -> Result<(), String> {
    let path = &manifest.config_path;
    for _ in 0..3 {
        let original = std::fs::read_to_string(path).map_err(|_| "读取 MCP 配置失败")?;
        let next = if manifest.agent.starts_with("codex") {
            let config: toml::Table = original.parse().map_err(|_| "Codex MCP 配置 TOML 损坏")?;
            let current = config.get("mcp_servers").and_then(toml::Value::as_table)
                .and_then(|servers| servers.get(&manifest.name))
                .map(|node| serde_json::to_value(node).map_err(|_| "Codex MCP 配置转换失败".to_string()))
                .transpose()?;
            if current.as_ref() != Some(expected) { return Err("MCP 配置已漂移，未覆盖用户修改".into()); }
            patch_codex_node(&original, manifest, replacement)?
        } else {
            let mut config = parse_json_config(&original)?;
            let servers = servers_map_mut(&mut config, &manifest.agent)?;
            if servers.get(&manifest.name) != Some(expected) { return Err("MCP 配置已漂移，未覆盖用户修改".into()); }
            servers.insert(manifest.name.clone(), replacement.clone());
            serde_json::to_string_pretty(&config).map_err(|_| "无法编码 MCP 配置".to_string())?
        };
        if std::fs::read_to_string(path).map_err(|_| "读取 MCP 配置失败")? != original { continue; }
        atomic_observation_write(path, &next)?;
        let installed = read_observation_node(&manifest.agent, path, &manifest.name)?;
        if installed.as_ref() != Some(replacement) { return Err("MCP 配置更新验证失败".into()); }
        return Ok(());
    }
    Err("MCP 配置在更新时持续变化，请重试".into())
}

fn activate_observation_node(manifest: &ObservationManifest, canonical: &Value, mut pause: impl FnMut()) -> Result<(), String> {
    let proxy = proxy_entry(manifest);
    for attempt in 0..2 {
        patch_observation_node(manifest, canonical, &proxy)?;
        for _ in 0..3 {
            pause();
            let current = read_observation_node(&manifest.agent, &manifest.config_path, &manifest.name)?;
            if current.as_ref() == Some(&proxy) { continue; }
            if current.as_ref() == Some(canonical) {
                if attempt == 0 { break; }
                return Err("MCP 配置被反复恢复为原命令，观测未启用".into());
            }
            return Err("MCP 配置已变化，未覆盖用户修改".into());
        }
        // Only retry a node that reverted exactly to its recorded original.
        if read_observation_node(&manifest.agent, &manifest.config_path, &manifest.name)?.as_ref() == Some(&proxy) {
            return Ok(());
        }
    }
    Err("MCP 配置被反复恢复为原命令，观测未启用".into())
}

fn activate_observation(manifest: &ObservationManifest, canonical: &Value) -> Result<(), String> {
    activate_observation_node(manifest, canonical, || std::thread::sleep(std::time::Duration::from_millis(120)))
}

fn observation_catalog() -> Result<Vec<McpCatalogEntry>, String> {
    if crate::telemetry_store::shared_store().is_some() { read_catalog() }
    else {
        // The --mcp-relay process does not initialize Tauri's shared store.
        // Its read only lookup uses a separate connection to the same database.
        let store = crate::telemetry_store::TelemetryStore::new()
            .map_err(|_| "无法读取 MCP 目录")?;
        let conn = store.conn.lock().map_err(|_| "MCP 数据库锁不可用")?;
        read_setting(&conn, MCP_CATALOG_SETTING_KEY)?.ok_or_else(|| "MCP 目录缺失".into())
    }
}

pub fn relay_launch(assignment_id: &str) -> Result<RelayLaunch, String> {
    let id = uuid::Uuid::parse_str(assignment_id).map_err(|_| "MCP relay id 无效")?;
    let manifest = observation_manifests()?.into_iter()
        .find(|manifest| manifest.id == id.to_string()).ok_or("MCP relay assignment 不存在")?;
    if manifest.phase != "active" { return Err("MCP relay assignment 尚未就绪".into()); }
    if manifest.relay_command.is_some() {
        let fallback = crate::mcp_fallback_resolver::resolve_fallback_for_relay(assignment_id)
            .map_err(|_| "MCP relay assignment 已变化".to_string())?;
        let expected = std::fs::canonicalize(manifest.relay_command.as_deref().unwrap_or_default())
            .map_err(|_| "MCP relay 程序不可用")?;
        if std::env::current_exe().and_then(std::fs::canonicalize).map_err(|_| "MCP relay 程序不可用")? != expected {
            return Err("MCP relay 程序已变化".into());
        }
        return Ok(RelayLaunch { server_name: manifest.name, command: fallback.command, args: fallback.args, env: fallback.env });
    }
    let entry = find_entry(&observation_catalog()?, &manifest.name)
        .map_err(|_| "MCP relay 原始配置已缺失")?;
    if !entry.assigned_agents.iter().any(|agent| physical_target(agent) == manifest.agent) {
        return Err("MCP relay 装备状态已改变".into());
    }
    if observation_fingerprint(&manifest.agent, &entry) != manifest.fingerprint
        || normalize_transport(&entry.transport) != "stdio"
    { return Err("MCP relay 原始配置已改变".into()); }
    if observation_config_path(&manifest.agent)? != manifest.config_path
        || !read_observation_node(&manifest.agent, &manifest.config_path, &manifest.name)?
            .as_ref().is_some_and(|node| proxy_matches(node, &manifest))
    { return Err("MCP relay 配置已漂移".into()); }
    Ok(RelayLaunch { server_name: entry.name, command: entry.command, args: entry.args, env: entry.env })
}

fn guarded_observation(name: &str, agent: Option<&str>) -> Result<std::sync::MutexGuard<'static, ()>, String> {
    let guard = observation_lock().lock().map_err(|_| "MCP observation 锁不可用")?;
    retire_detached_observations(&read_catalog()?)?;
    if observation_manifests()?.iter().any(|manifest| manifest.name == name &&
        agent.is_none_or(|agent| physical_target(agent) == manifest.agent)) {
        return Err("MCP 正在观察或等待恢复，请先关闭观察".into());
    }
    Ok(guard)
}

fn upgrade_observation(manifest: &ObservationManifest, catalog: &[McpCatalogEntry]) -> Result<ObservationManifest, String> {
    if manifest.phase != "active" { return Ok(manifest.clone()); }
    // Refresh only between connections. A live relay still validates its pinned
    // executable against this manifest; leave it intact until the next idle retry.
    // Unknown telemetry is not evidence that switching is safe.
    let Some(active) = crate::telemetry_store::shared_store()
        .and_then(|store| store.proxy_active_connections(&manifest.id).ok()) else {
        return Ok(manifest.clone());
    };
    if active > 0 || fallback_connections(&manifest.id) > 0 { return Ok(manifest.clone()); }
    let entry = catalog.iter().find(|entry| entry.name == manifest.name)
        .ok_or("MCP 原始配置缺失")?;
    if !manifest.config_path.is_absolute() { return Err("MCP 配置路径必须是绝对路径".into()); }
    if observation_fingerprint(&manifest.agent, entry) != manifest.fingerprint
        || normalize_transport(&entry.transport) != "stdio"
        || !entry.assigned_agents.iter().any(|agent| physical_target(agent) == manifest.agent)
        || observation_config_path(&manifest.agent)? != manifest.config_path {
        return Err("MCP 原始配置已变化，无法升级代理".into());
    }
    let old = proxy_entry(manifest);
    if read_observation_node(&manifest.agent, &manifest.config_path, &manifest.name)?.as_ref() != Some(&old) {
        return Err("MCP 配置已漂移，无法升级代理".into());
    }
    let launcher = crate::mcp_launcher_installation::launcher_executable()?;
    let relay = crate::mcp_launcher_installation::relay_executable()?;
    upgrade_observation_node(manifest, launcher, relay, save_observation)
}

fn upgrade_observation_node(
    manifest: &ObservationManifest, launcher: String, relay: String,
    mut save: impl FnMut(&ObservationManifest) -> Result<(), String>,
) -> Result<ObservationManifest, String> {
    if manifest.proxy_command == launcher && manifest.relay_command.as_ref() == Some(&relay) {
        return Ok(manifest.clone());
    }
    let old = proxy_entry(manifest);
    let mut next = manifest.clone();
    next.proxy_command = launcher;
    next.relay_command = Some(relay);
    next.previous_proxy_command = Some(manifest.proxy_command.clone());
    next.previous_relay_command = manifest.relay_command.clone();
    let replacement = proxy_entry(&next);
    // Keep both exact endpoints in the preparing record until the config
    // switch commits. On restart we can distinguish either side from drift.
    next.phase = "preparing".into();
    save(&next)?;
    if let Err(error) = patch_observation_node(&next, &old, &replacement) {
        if read_observation_node(&next.agent, &next.config_path, &next.name).ok().flatten().as_ref() == Some(&old) {
            let _ = save(manifest);
        }
        return Err(error);
    }
    next.phase = "active".into();
    next.previous_proxy_command = None;
    next.previous_relay_command = None;
    save(&next)?;
    Ok(next)
}

fn recover_upgrade(manifest: &ObservationManifest) -> Result<ObservationManifest, String> {
    if manifest.phase != "preparing" || manifest.previous_proxy_command.is_none() || manifest.relay_command.is_none() {
        return Ok(manifest.clone());
    }
    if manifest.config_path != observation_config_path(&manifest.agent)? {
        return Err("MCP 配置路径已变化，无法恢复代理升级".into());
    }
    let current = read_observation_node(&manifest.agent, &manifest.config_path, &manifest.name)?;
    let recovered = recover_upgrade_node(manifest, current.as_ref())?;
    save_observation(&recovered)?;
    Ok(recovered)
}

fn recover_upgrade_node(manifest: &ObservationManifest, current: Option<&Value>) -> Result<ObservationManifest, String> {
    let mut active = manifest.clone();
    active.phase = "active".into();
    active.previous_proxy_command = None;
    active.previous_relay_command = None;
    // Prefer the installed endpoint when both launcher paths are identical:
    // a relay-only refresh has no distinct config switch to roll back.
    if current == Some(&proxy_entry(manifest)) {
        return Ok(active);
    }
    let mut old = active;
    old.proxy_command = manifest.previous_proxy_command.clone().unwrap_or_default();
    old.relay_command = manifest.previous_relay_command.clone();
    if current == Some(&proxy_entry(&old)) {
        return Ok(old);
    }
    Err("MCP 配置已漂移，无法恢复代理升级".into())
}

fn fallback_connections(assignment_id: &str) -> i64 {
    #[derive(Deserialize)]
    struct Heartbeat { assignment_id: String, pid: u32, last_seen_unix_ms: u64 }
    let Ok(root) = observation_root() else { return 0; };
    let Ok(files) = std::fs::read_dir(root.join("fallback").join(assignment_id)) else { return 0; };
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)
        .map(|value| value.as_millis()).unwrap_or_default();
    files.filter_map(Result::ok).filter(|file| {
        if !file.file_type().ok().is_some_and(|kind| kind.is_file()) { return false; }
        let Some(pid) = file.path().file_stem().and_then(|name| name.to_str()).and_then(|name| name.parse::<u32>().ok()) else { return false; };
        let Some(state) = std::fs::read(file.path()).ok().and_then(|bytes| serde_json::from_slice::<Heartbeat>(&bytes).ok()) else { return false; };
        state.assignment_id == assignment_id && state.pid == pid && state.last_seen_unix_ms > 0
            && now >= u128::from(state.last_seen_unix_ms) && now - u128::from(state.last_seen_unix_ms) <= 10_000
    }).count() as i64
}

/// A detached manifest can be retired without touching the Agent file. Keep
/// unknown/live sessions, custom nodes, unreadable files and either endpoint of
/// an interrupted proxy upgrade recoverable.
fn detached_observation(
    manifest: &ObservationManifest, entry: Option<&McpCatalogEntry>,
    current: Option<&Value>, active: Option<i64>, fallback: i64,
) -> bool {
    if active != Some(0) || fallback != 0 { return false; }
    let Some(current) = current else { return true; };
    if proxy_matches(current, manifest) { return false; }
    if let Some(command) = &manifest.previous_proxy_command {
        let mut previous = manifest.clone();
        previous.proxy_command = command.clone();
        if proxy_matches(current, &previous) { return false; }
    }
    // A relay-shaped node must never become an adoptable direct source.
    if is_observation_proxy_node(current) { return false; }
    entry.is_some_and(|entry| entry_matches_config(&manifest.agent, entry, current))
}

/// Caller holds observation_lock. Only delete unused manifests; never rewrite
/// custom configurations, switch relay endpoints or restart a live connection.
fn retire_detached_observations(catalog: &[McpCatalogEntry]) -> Result<(), String> {
    for manifest in observation_manifests()? {
        if observation_config_path(&manifest.agent).ok().as_ref() != Some(&manifest.config_path) { continue; }
        let current = match std::fs::metadata(&manifest.config_path) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(_) => continue,
            Ok(_) => match read_observation_node(&manifest.agent, &manifest.config_path, &manifest.name) {
                Ok(current) => current,
                Err(_) => continue,
            },
        };
        let active = crate::telemetry_store::shared_store()
            .and_then(|store| store.proxy_active_connections(&manifest.id).ok());
        if detached_observation(&manifest, catalog.iter().find(|entry| entry.name == manifest.name),
            current.as_ref(), active, fallback_connections(&manifest.id)) {
            remove_observation(&manifest)?;
        }
    }
    Ok(())
}

fn observation_status(manifest: &ObservationManifest, catalog: &[McpCatalogEntry]) -> McpObservationStatus {
    let entry = catalog.iter().find(|entry| entry.name == manifest.name);
    let valid = entry.is_some_and(|entry| observation_fingerprint(&manifest.agent, entry) == manifest.fingerprint
        && normalize_transport(&entry.transport) == "stdio"
        && entry.assigned_agents.iter().any(|agent| physical_target(agent) == manifest.agent))
        && observation_config_path(&manifest.agent).ok().as_ref() == Some(&manifest.config_path);
    let current = read_observation_node(&manifest.agent, &manifest.config_path, &manifest.name).ok().flatten();
    let proxy = current.as_ref().is_some_and(|node| proxy_matches(node, manifest));
    let canonical = entry.is_some_and(|entry| current.as_ref().is_some_and(|node| value_equal(node, &canonical_for_manifest(manifest, entry))));
    let state = if manifest.phase != "active" && valid && canonical { "recovery" }
        else if !valid || (!proxy && !canonical) || !Path::new(&manifest.proxy_command).is_file() { "drifted" }
        else if manifest.phase != "active" { "recovery" }
        else if !proxy { "drifted" }
        else { "awaiting_connection" };
    let active_connections = if state == "awaiting_connection" {
        crate::telemetry_store::shared_store()
            .and_then(|store| store.proxy_active_connections(&manifest.id).ok())
            .unwrap_or(0)
    } else { 0 };
    McpObservationStatus {
        name: manifest.name.clone(), agent: manifest.agent.clone(),
        state: if state == "awaiting_connection" && fallback_connections(&manifest.id) > 0 { "fallback" }
            else if active_connections > 0 { "observing" }
            else { state }.into(),
        active_connections,
    }
}

#[tauri::command]
pub async fn mcp_observation_status_all() -> Result<Vec<McpObservationStatus>, String> {
    let _guard = observation_lock().lock().map_err(|_| "MCP observation 锁不可用")?;
    retire_memory_observation();
    let catalog = read_catalog()?;
    retire_detached_observations(&catalog)?;
    let manifests = observation_manifests()?;
    let mut statuses = Vec::with_capacity(manifests.len());
    for manifest in &manifests {
        let recovered = recover_upgrade(manifest).unwrap_or_else(|_| manifest.clone());
        let current = upgrade_observation(&recovered, &catalog).unwrap_or(recovered);
        statuses.push(observation_status(&current, &catalog));
    }
    for entry in &catalog {
        for agent in &entry.assigned_agents {
            let physical = physical_target(agent);
            if statuses.iter().any(|status| status.name == entry.name && status.agent == physical) { continue; }
            statuses.push(McpObservationStatus {
                name: entry.name.clone(), agent: physical.to_string(),
                state: if normalize_transport(&entry.transport) == "stdio" && is_mcp_target(agent) && agent != "dsh" {
                    "off"
                } else { "unsupported" }.into(),
                active_connections: 0,
            });
        }
    }
    statuses.sort_by(|a, b| (&a.name, &a.agent).cmp(&(&b.name, &b.agent)));
    Ok(statuses)
}

#[tauri::command]
pub async fn mcp_observation_set(name: String, agent: String, enabled: bool) -> Result<McpObservationStatus, String> {
    let _guard = observation_lock().lock().map_err(|_| "MCP observation 锁不可用")?;
    if !is_mcp_target(&agent) { return Err("此 Agent 不支持 MCP observation".into()); }
    if agent == "dsh" {
        // dsh 的 MCP 调用观测来自本地转录扫描（tool/call × tool/result 配对），
        // stdio 中继观测没有增量价值，也不往 YAML patch 里包代理。
        return Err("DeepSeek Harness 的 MCP 调用由本地转录扫描观测，无需 stdio 中继".into());
    }
    if name == MEMORY_SERVER_NAME {
        return Err("共享记忆 MCP 不支持观测：调用统计由服务端自行记录".into());
    }
    let physical = physical_target(&agent).to_string();
    let catalog = read_catalog()?;
    let entry = find_entry(&catalog, &name)?;
    if normalize_transport(&entry.transport) != "stdio" { return Err("仅支持 stdio MCP observation".into()); }
    if !entry.assigned_agents.iter().any(|assigned| physical_target(assigned) == physical) {
        return Err("此 MCP 尚未装备到该 Agent".into());
    }
    retire_detached_observations(&catalog)?;
    let existing = observation_manifests()?.into_iter()
        .find(|manifest| manifest.name == name && manifest.agent == physical);
    if enabled {
        if let Some(manifest) = existing {
            let recovered = recover_upgrade(&manifest)?;
            let current = upgrade_observation(&recovered, &catalog)?;
            if current.phase != "active" || current.config_path != observation_config_path(&physical)?
                || current.fingerprint != observation_fingerprint(&physical, &entry)
                || !Path::new(&current.proxy_command).is_file()
                || current.relay_command.as_ref().is_none_or(|command| !Path::new(command).is_file())
            {
                return Err("MCP 原始配置或观测程序已变化，无法重试观测".into());
            }
            let node = read_observation_node(&physical, &current.config_path, &name)?;
            if node.as_ref().is_some_and(|node| proxy_matches(node, &current)) {
                return Ok(observation_status(&current, &catalog));
            }
            let canonical = canonical_for_manifest(&current, &entry);
            if node.as_ref() != Some(&canonical) {
                return Err("MCP 配置已变化，未覆盖用户修改".into());
            }
            if let Err(error) = activate_observation(&current, &canonical) {
                // The original node is unambiguous; a custom edit or unreadable
                // config keeps the manifest so that disable can recover safely.
                if read_observation_node(&physical, &current.config_path, &name).ok().flatten().as_ref() == Some(&canonical) {
                    remove_observation(&current)?;
                }
                return Err(error);
            }
            return Ok(observation_status(&current, &catalog));
        }
        let path = observation_config_path(&physical)?;
        let mut manifest = ObservationManifest {
            id: uuid::Uuid::new_v4().to_string(), name: name.clone(), agent: physical.clone(),
            config_path: path.clone(), fingerprint: observation_fingerprint(&physical, &entry),
            proxy_command: crate::mcp_launcher_installation::launcher_executable()?,
            relay_command: Some(crate::mcp_launcher_installation::relay_executable()?),
            previous_proxy_command: None, previous_relay_command: None, omitted_args: false, phase: "preparing".into(),
        };
        let current = read_observation_node(&manifest.agent, &path, &name)?
            .ok_or("源 Agent 中的 MCP 已不存在")?;
        if !canonical_matches(&manifest.agent, &entry, &current) {
            return Err("MCP 配置与目录不一致，未启用观察".into());
        }
        manifest.omitted_args = current.get("args").is_none();
        save_observation(&manifest)?;
        if let Err(error) = activate_observation(&manifest, &current) {
            // Keep uncertain states recoverable; only remove a manifest when
            // the node is exactly the original configuration it recorded.
            if read_observation_node(&manifest.agent, &path, &name).ok().flatten().as_ref() == Some(&current) {
                remove_observation(&manifest)?;
            }
            return Err(error);
        }
        let mut ready = manifest;
        ready.phase = "active".into();
        save_observation(&ready)?;
        Ok(observation_status(&ready, &catalog))
    } else if let Some(manifest) = existing {
        let mut manifest = recover_upgrade(&manifest)?;
        if manifest.fingerprint != observation_fingerprint(&manifest.agent, &entry)
            || manifest.config_path != observation_config_path(&manifest.agent)? {
            return Err("MCP 原始配置已漂移，未覆盖用户修改".into());
        }
        let current = read_observation_node(&manifest.agent, &manifest.config_path, &name)?;
        let canonical = canonical_for_manifest(&manifest, &entry);
        if current.as_ref().is_some_and(|node| proxy_matches(node, &manifest)) {
            manifest.phase = "recovery".into();
            save_observation(&manifest)?;
            patch_observation_node(&manifest, current.as_ref().unwrap(), &canonical)?;
        } else if current.as_ref() != Some(&canonical) {
            return Err("MCP 配置已漂移，未覆盖用户修改".into());
        }
        remove_observation(&manifest)?;
        Ok(McpObservationStatus { name, agent: physical, state: "off".into(), active_connections: 0 })
    } else {
        Ok(McpObservationStatus { name, agent: physical, state: "off".into(), active_connections: 0 })
    }
}

// ── Tauri 命令 ────────────────────────────────────────────────────────────────

#[tauri::command]
pub async fn mcp_catalog_list() -> Result<Vec<McpCatalogEntry>, String> {
    read_catalog()
}

#[tauri::command]
pub async fn mcp_catalog_migration_report() -> Result<Vec<String>, String> {
    with_catalog(false, |_, state| Ok(state.report.clone()))
}

#[tauri::command]
pub async fn mcp_catalog_upsert(entry: McpCatalogEntry) -> Result<McpCatalogEntry, String> {
    upsert_catalog_entry(entry)
}

/// Explicit import re-reads the selected source and records its existing assignment.
/// Ordinary edits cannot create or restore stale assignment claims.
#[tauri::command]
pub async fn mcp_catalog_import(name: String, agent: String) -> Result<McpCatalogEntry, String> {
    if !is_mcp_target(&agent) || name == MEMORY_SERVER_NAME {
        return Err("不支持导入该 MCP 配置".into());
    }
    let _observation_guard = guarded_observation(&name, None)?;
    let servers = read_agent_mcp_servers(&agent)?;
    let value = servers.get(&name).ok_or("源 Agent 中的 MCP 已不存在，请重新扫描")?;
    let entry = entry_from_config(&name, &agent, value)?;
    with_catalog(true, |catalog, _| {
        import_entry(catalog, entry, &agent, &chrono::Utc::now().to_rfc3339())
    })
}

fn import_entry(
    catalog: &mut Vec<McpCatalogEntry>,
    mut entry: McpCatalogEntry,
    agent: &str,
    now: &str,
) -> Result<McpCatalogEntry, String> {
    validate_configuration(&entry)?;
    if entry.name.trim().is_empty() || !is_mcp_target(agent) {
        return Err("MCP 名称或来源无效".into());
    }
    if catalog.iter().any(|existing| existing.name == entry.name) {
        entry = upsert_entry(catalog, entry, now)?;
        if !entry.assigned_agents.iter().any(|assigned| assigned == agent) {
            entry.assigned_agents.push(agent.to_string());
            entry.assigned_agents.sort();
        }
        if let Some(slot) = catalog.iter_mut().find(|existing| existing.name == entry.name) {
            *slot = entry.clone();
        }
    } else {
        // Imported names may predate the lowercase naming convention.
        entry.created_at = now.to_string();
        entry.updated_at = now.to_string();
        entry.assigned_agents = vec![agent.to_string()];
        catalog.push(entry.clone());
    }
    Ok(entry)
}

pub(crate) fn upsert_catalog_entry(entry: McpCatalogEntry) -> Result<McpCatalogEntry, String> {
    let _observation_guard = guarded_observation(&entry.name, None)?;
    with_catalog(true, |catalog, _| {
        upsert_entry(catalog, entry, &chrono::Utc::now().to_rfc3339())
    })
}

fn upsert_entry(
    catalog: &mut Vec<McpCatalogEntry>,
    entry: McpCatalogEntry,
    now: &str,
) -> Result<McpCatalogEntry, String> {
    // 先精确匹配，保留迁入旧名称的大小写、空格等；仅新建名称执行规则。
    let name = if catalog.iter().any(|existing| existing.name == entry.name) {
        entry.name.clone()
    } else {
        entry.name.trim().to_string()
    };
    let existing = catalog.iter().find(|existing| existing.name == name);
    if existing.is_none() && !valid_name(&name) {
        return Err("名称只能包含小写字母、数字、- 和 _".into());
    }
    validate_configuration(&entry)?;
    let transport = normalize_transport(&entry.transport).to_string();
    let created_at = existing
        .map(|entry| entry.created_at.clone())
        .filter(|time| !time.is_empty())
        .unwrap_or_else(|| now.to_string());
    // 编辑配置（包括旧 IPC）不能抹掉实际已分发的关系；卸载由 unequip 负责。
    let assigned = if let Some(existing) = existing {
        existing.assigned_agents.clone()
    } else {
        let mut assigned: Vec<String> = entry.assigned_agents.into_iter()
            .filter(|agent| is_mcp_target(agent) && supports_transport(agent, &transport))
            .collect();
        assigned.sort();
        assigned.dedup();
        assigned
    };
    let next = McpCatalogEntry {
        name: name.clone(),
        description: entry.description.trim().to_string(),
        transport,
        command: entry.command.trim().to_string(),
        args: entry.args,
        env: entry.env,
        url: entry.url.trim().to_string(),
        headers: entry.headers,
        assigned_agents: assigned,
        created_at,
        updated_at: now.to_string(),
    };
    match catalog.iter_mut().find(|e| e.name == name) {
        Some(slot) => *slot = next.clone(),
        None => catalog.push(next.clone()),
    }
    Ok(next)
}

/// 删除库条目。已写入各 Agent 的配置不动（与 Skill 删除语义一致），
/// 需要先在装备对话框逐个卸载。
#[tauri::command]
pub async fn mcp_catalog_delete(name: String) -> Result<(), String> {
    delete_catalog_entry(&name)
}

pub(crate) fn delete_catalog_entry(name: &str) -> Result<(), String> {
    let _observation_guard = guarded_observation(name, None)?;
    with_catalog(true, |catalog, _| {
        let before = catalog.len();
        catalog.retain(|entry| entry.name != name);
        if catalog.len() == before {
            return Err(format!("MCP 库中不存在 {name}"));
        }
        Ok(())
    })
}

/// 装备：写入目标 Agent 配置，并把 agent 记入 assigned_agents。
#[tauri::command]
pub async fn mcp_equip(name: String, agent: String) -> Result<McpCatalogEntry, String> {
    if !is_mcp_target(&agent) {
        return Err(format!("暂不支持 {agent} 的 MCP 配置"));
    }
    let catalog = read_catalog()?;
    let entry = find_entry(&catalog, &name)?;
    if !supports_transport(&agent, normalize_transport(&entry.transport)) {
        return Err(format!("{} 不支持该 transport", agent_label(&agent)));
    }
    let _observation_guard = guarded_observation(&name, Some(&agent))?;
    apply_equip(&agent, &entry)?;
    save_assignment(&name, &agent, true)
}

/// 卸载：从目标 Agent 配置移除，并从 assigned_agents 去掉。
#[tauri::command]
pub async fn mcp_unequip(name: String, agent: String) -> Result<McpCatalogEntry, String> {
    if !is_mcp_target(&agent) {
        return Err(format!("暂不支持 {agent} 的 MCP 配置"));
    }
    // 先确认中央目录可读且条目存在，再产生外部文件/CLI 副作用。
    find_entry(&read_catalog()?, &name)?;
    let _observation_guard = guarded_observation(&name, Some(&agent))?;
    apply_unequip(&agent, &name)?;
    save_assignment(&name, &agent, false)
}

/// 比较目录与所有 Agent 的现有配置；未纳管但同名的配置也参与比较。
#[tauri::command]
pub async fn mcp_status_all() -> Result<Vec<McpAgentStatus>, String> {
    let catalog = read_catalog()?;
    let snapshots = read_mcp_snapshots();
    let observation = observation_manifests()?;
    Ok(collect_agent_statuses(&catalog, &snapshots, &observation))
}

/// 保留每个 Agent 的同名服务器，并标记相对中央目录的差异及配置版本。
#[tauri::command]
pub async fn mcp_import_from_agents() -> Result<McpImportScan, String> {
    let catalog = read_catalog()?;
    let snapshots = read_mcp_snapshots();
    let observation = observation_manifests()?;
    Ok(collect_import_candidates(&catalog, &snapshots, &observation))
}

fn comparison_revision(
    name: &str,
    catalog: Option<&McpCatalogEntry>,
    snapshots: &HashMap<String, Result<Map<String, Value>, String>>,
    observation: &[ObservationManifest],
) -> Result<String, String> {
    // Hash the reviewed service only, in fixed physical-source order. Never log values.
    let sources: Vec<Value> = MCP_TARGETS.into_iter().filter(|agent| *agent != "codex_desktop")
        .map(|agent| match snapshots.get(agent) {
            Some(Ok(servers)) => json!({"agent": agent, "value": servers.get(name)}),
            Some(Err(error)) => json!({"agent": agent, "error": error}),
            None => json!({"agent": agent, "unread": true}),
        }).collect();
    let mut manifests: Vec<_> = observation.iter().filter(|manifest| manifest.name == name).collect();
    manifests.sort_by(|a, b| a.agent.cmp(&b.agent).then(a.id.cmp(&b.id)));
    let bytes = serde_json::to_vec(&json!({"catalog": catalog, "sources": sources, "observation": manifests}))
        .map_err(|_| "无法生成 MCP 对比版本".to_string())?;
    Ok(format!("{:x}", Sha256::digest(bytes)))
}

fn configuration_comparison(
    name: &str,
    catalog: Option<McpCatalogEntry>,
    snapshots: &HashMap<String, Result<Map<String, Value>, String>>,
    observation: &[ObservationManifest],
) -> Result<McpConfigurationComparison, String> {
    let revision = comparison_revision(name, catalog.as_ref(), snapshots, observation)?;
    let mut sources = Vec::new();
    for agent in MCP_TARGETS.into_iter().filter(|agent| *agent != "codex_desktop") {
        let assigned = catalog.as_ref().is_some_and(|entry| entry.assigned_agents.iter()
            .any(|target| physical_target(target) == agent));
        let manifest = observation.iter().find(|manifest| manifest.name == name && manifest.agent == agent);
        let mut source = McpComparisonSource {
            agent: agent.into(), entry: None, state: "missing".into(), assigned,
            observed: manifest.is_some(), error: None,
        };
        match snapshots.get(agent) {
            Some(Err(error)) => { source.state = "unavailable".into(); source.error = Some(error.clone()); }
            Some(Ok(servers)) => if let Some(value) = servers.get(name) {
                if is_observation_proxy_node(value) || manifest.is_some_and(|manifest| proxy_matches(value, manifest)) {
                    // The relay is not an adoptable upstream configuration.
                    if manifest.is_some_and(|manifest| catalog.as_ref()
                        .is_some_and(|entry| observation_matches_entry(manifest, entry, value))) {
                        source.state = "observed".into();
                        source.entry = catalog.clone();
                    } else {
                        source.state = "differs".into();
                        source.error = Some("观测代理的原始配置已变化，请先关闭观察或恢复配置".into());
                    }
                } else {
                    match entry_from_config(name, agent, value) {
                        Ok(entry) => {
                            source.state = if catalog.is_none() { "unmanaged".into() }
                            else if catalog.as_ref().is_some_and(|central| entry_matches_config(agent, central, value)) {
                                "installed".into()
                            } else { "differs".into() };
                            source.entry = Some(entry);
                        }
                        Err(error) => { source.state = "unavailable".into(); source.error = Some(error); }
                    }
                }
            },
            None => { source.state = "unavailable".into(); source.error = Some("未读取该来源".into()); }
        }
        sources.push(source);
    }
    let blocked_reason = observation.iter().any(|manifest| manifest.name == name)
        .then(|| "此服务正在观察或等待恢复，请先在服务管理中关闭观察，再重新读取对比。".into());
    Ok(McpConfigurationComparison { name: name.into(), catalog, sources, revision, blocked_reason })
}

#[tauri::command]
pub async fn mcp_configuration_compare(name: String) -> Result<McpConfigurationComparison, String> {
    if name.trim().is_empty() || name == MEMORY_SERVER_NAME || name == LEGACY_MEMORY_SERVER_NAME {
        return Err("不支持仲裁该 MCP 服务".into());
    }
    let _guard = observation_lock().lock().map_err(|_| "MCP observation 锁不可用")?;
    let catalog = read_catalog()?;
    retire_detached_observations(&catalog)?;
    let catalog = catalog.into_iter().find(|entry| entry.name == name);
    configuration_comparison(&name, catalog, &read_mcp_snapshots(), &observation_manifests()?)
}

/// Explicit version arbitration. Only reviewed target files may be replaced.
#[tauri::command]
pub async fn mcp_configuration_resolve(
    name: String, revision: String, source: Option<String>, targets: Vec<String>,
) -> Result<McpArbitrationResult, String> {
    if name.trim().is_empty() || name == MEMORY_SERVER_NAME || name == LEGACY_MEMORY_SERVER_NAME {
        return Err("不支持仲裁该 MCP 服务".into());
    }
    let _guard = guarded_observation(&name, None)?;
    let catalog = read_catalog()?.into_iter().find(|entry| entry.name == name);
    let snapshots = read_mcp_snapshots();
    let observation = observation_manifests()?;
    if revision != comparison_revision(&name, catalog.as_ref(), &snapshots, &observation)? {
        return Err("配置已在对比后发生变化，请重新读取对比再应用，未执行本次仲裁。".into());
    }
    let source = source.map(|agent| physical_target(&agent).to_string());
    let selected = match source.as_deref() {
        None => catalog.clone().ok_or("资源库中没有该服务，请选择 Agent 版本")?,
        Some(agent) => {
            if !is_mcp_target(agent) { return Err("无效的 MCP 仲裁来源".into()); }
            let servers = snapshots.get(agent).ok_or("未读取仲裁来源")?
                .as_ref().map_err(|_| "仲裁来源不可读，请重新检查")?;
            let node = servers.get(&name).ok_or("来源服务已不存在")?;
            if is_observation_proxy_node(node) { return Err("观测代理不能作为原始 MCP 配置导入".into()); }
            entry_from_config(&name, agent, node)?
        }
    };
    let mut targets: Vec<String> = targets.into_iter().map(|agent| physical_target(&agent).to_string()).collect();
    targets.sort();
    targets.dedup();
    targets.retain(|agent| Some(agent.as_str()) != source.as_deref());
    if source.is_none() && targets.is_empty() { return Err("请选择需要同步资源库版本的 Agent".into()); }
    for agent in &targets {
        if !is_mcp_target(agent) || !supports_transport(agent, normalize_transport(&selected.transport)) {
            return Err(format!("{} 不支持所选 MCP 配置，未执行本次仲裁", agent_label(agent)));
        }
        let servers = snapshots.get(agent).and_then(|snapshot| snapshot.as_ref().ok())
            .ok_or_else(|| format!("{} 配置不可读，未执行本次仲裁", agent_label(agent)))?;
        if let Some(value) = servers.get(&name) {
            entry_from_config(&name, agent, value)
                .map_err(|_| format!("{} 的现有服务配置无效，请先修复后重新对比", agent_label(agent)))?;
        }
    }
    let catalog_updated = source.is_some();
    let mut saved = with_catalog(catalog_updated, |entries, _| {
        let current = entries.iter().find(|entry| entry.name == name);
        let current_value = serde_json::to_value(current).map_err(|_| "无法校验当前资源库配置")?;
        let reviewed_value = serde_json::to_value(catalog.as_ref()).map_err(|_| "无法校验对比版本")?;
        if current_value != reviewed_value {
            return Err("资源库已变化，请重新读取对比，未执行本次仲裁。".into());
        }
        if let Some(agent) = source.as_deref() {
            import_entry(entries, selected.clone(), agent, &chrono::Utc::now().to_rfc3339())
        } else { Ok(selected.clone()) }
    })?;
    let mut applied_agents = Vec::new();
    let mut errors = Vec::new();
    for agent in targets {
        let apply = (|| -> Result<McpCatalogEntry, String> {
            let before = snapshots.get(&agent).and_then(|snapshot| snapshot.as_ref().ok())
                .and_then(|servers| servers.get(&name));
            let current = read_agent_mcp_servers(&agent)?;
            if current.get(&name) != before { return Err("目标配置已变化，请重新对比；未覆盖此 Agent".into()); }
            if !current.get(&name).is_some_and(|value| entry_matches_config(&agent, &selected, value)) {
                apply_equip(&agent, &selected)?;
            }
            let configured = read_agent_mcp_servers(&agent)?;
            if !configured.get(&name).is_some_and(|value| entry_matches_config(&agent, &selected, value)) {
                return Err("写入后校验不一致，请重新检查该 Agent 配置".into());
            }
            save_assignment(&name, &agent, true)
        })();
        match apply {
            Ok(entry) => { saved = entry; applied_agents.push(agent); }
            Err(message) => errors.push(McpImportIssue { agent, name: Some(name.clone()), message }),
        }
    }
    Ok(McpArbitrationResult { entry: saved, catalog_updated, applied_agents, errors })
}

/// 一键同步：把 assigned 给该 Agent 的所有条目按库内配置重写（漂移修复）。
#[tauri::command]
pub async fn mcp_sync_agent(agent: String) -> Result<(), String> {
    if !is_mcp_target(&agent) {
        return Err(format!("暂不支持 {agent} 的 MCP 配置"));
    }
    let _observation_guard = observation_lock().lock().map_err(|_| "MCP observation 锁不可用")?;
    let catalog = read_catalog()?;
    if observation_manifests()?.iter().any(|manifest| manifest.agent == physical_target(&agent)) {
        return Err("此 Agent 存在正在观察的 MCP，请先关闭观察".into());
    }
    let assigned: Vec<&McpCatalogEntry> = catalog
        .iter()
        .filter(|entry| entry.assigned_agents.iter().any(|a| a == &agent))
        .collect();
    if assigned.is_empty() {
        return Err("没有装备到该 Agent 的条目".into());
    }
    let mut errors = Vec::new();
    for entry in &assigned {
        if !supports_transport(&agent, normalize_transport(&entry.transport)) {
            errors.push(format!(
                "{}：{} 不支持该 transport",
                entry.name,
                agent_label(&agent)
            ));
            continue;
        }
        if let Err(error) = apply_equip(&agent, entry) {
            errors.push(format!("{}：{error}", entry.name));
        }
    }
    if !errors.is_empty() {
        return Err(errors.join("；"));
    }
    Ok(())
}

// ── 配置档案（Profiles：已命名的启用服务器集合） ─────────────────────────────

/// 一个 MCP 配置档案：记住一组「启用的服务器名」，一键切换工作流运行侧的
/// MCP 上下文。只保存名称引用，不复制服务器配置（配置以中央目录为准）。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct McpProfile {
    pub name: String,
    #[serde(default)]
    pub description: String,
    /// 启用的服务器名（中央目录条目名）。允许引用暂时缺失的名称。
    #[serde(default)]
    pub servers: Vec<String>,
    #[serde(default)]
    pub created_at: String,
    #[serde(default)]
    pub updated_at: String,
}

fn valid_profile_name(name: &str) -> bool {
    let trimmed = name.trim();
    !trimmed.is_empty()
        && trimmed.chars().count() <= 64
        && !trimmed.chars().any(char::is_control)
}

fn read_profiles() -> Result<Vec<McpProfile>, String> {
    let store = crate::telemetry_store::shared_store().ok_or("本地数据库不可用")?;
    let conn = store.conn.lock().map_err(|_| "MCP 数据库锁不可用")?;
    Ok(read_setting(&conn, MCP_PROFILES_SETTING_KEY)?.unwrap_or_default())
}

/// 连接锁 + IMMEDIATE 事务串行化档案读改写。
fn with_profiles<T>(
    action: impl FnOnce(&mut Vec<McpProfile>) -> Result<T, String>,
) -> Result<T, String> {
    let store = crate::telemetry_store::shared_store().ok_or("本地数据库不可用")?;
    let mut conn = store.conn.lock().map_err(|_| "MCP 数据库锁不可用")?;
    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|error| format!("打开 MCP profiles 事务失败：{error}"))?;
    let mut profiles: Vec<McpProfile> =
        read_setting(&tx, MCP_PROFILES_SETTING_KEY)?.unwrap_or_default();
    let result = action(&mut profiles)?;
    write_setting(&tx, MCP_PROFILES_SETTING_KEY, &profiles)?;
    tx.commit().map_err(|error| format!("提交 MCP profiles 失败：{error}"))?;
    Ok(result)
}

fn upsert_profile(
    profiles: &mut Vec<McpProfile>,
    profile: McpProfile,
    now: &str,
) -> Result<McpProfile, String> {
    let name = profile.name.trim().to_string();
    if !valid_profile_name(&name) {
        return Err("档案名称不能为空、超过 64 个字符或包含控制字符".into());
    }
    // 保序去重：名称顺序对集合语义无影响，但保持用户输入顺序便于核对。
    let mut seen = HashSet::new();
    let servers: Vec<String> = profile
        .servers
        .into_iter()
        .map(|name| name.trim().to_string())
        .filter(|name| !name.is_empty() && seen.insert(name.clone()))
        .collect();
    let created_at = profiles
        .iter()
        .find(|existing| existing.name == name)
        .map(|existing| existing.created_at.clone())
        .filter(|time| !time.is_empty())
        .unwrap_or_else(|| now.to_string());
    let next = McpProfile {
        name: name.clone(),
        description: profile.description.trim().to_string(),
        servers,
        created_at,
        updated_at: now.to_string(),
    };
    match profiles.iter_mut().find(|existing| existing.name == name) {
        Some(slot) => *slot = next.clone(),
        None => profiles.push(next.clone()),
    }
    Ok(next)
}

#[tauri::command]
pub async fn mcp_profile_list() -> Result<Vec<McpProfile>, String> {
    read_profiles()
}

#[tauri::command]
pub async fn mcp_profile_save(profile: McpProfile) -> Result<McpProfile, String> {
    with_profiles(|profiles| upsert_profile(profiles, profile, &chrono::Utc::now().to_rfc3339()))
}

#[tauri::command]
pub async fn mcp_profile_delete(name: String) -> Result<(), String> {
    with_profiles(|profiles| {
        let before = profiles.len();
        profiles.retain(|profile| profile.name != name);
        if profiles.len() == before {
            return Err(format!("MCP 档案中不存在 {name}"));
        }
        Ok(())
    })
}

// ── 单元测试 ─────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn snapshots(fixtures: &[(&str, Value)]) -> HashMap<String, Result<Map<String, Value>, String>> {
        fixtures.iter().map(|(agent, servers)|
            (agent.to_string(), Ok(servers.as_object().unwrap().clone()))).collect()
    }

    #[test]
    fn scan_preserves_same_name_sources_and_marks_versions_without_leaking_secrets() {
        let sources = snapshots(&[
            ("claude_cli", json!({"demo": {"command": "node", "env": {"TOKEN": "secret-a"}}})),
            ("qoder", json!({"demo": {"command": "node", "env": {"TOKEN": "secret-b"}}})),
            ("kimi", json!({"demo": {"type": "stdio", "command": "node", "env": {"TOKEN": "secret-a"}, "description": "noise"}})),
        ]);
        let catalog = vec![entry_from_config("demo", "claude_cli", &sources["claude_cli"].as_ref().unwrap()["demo"]).unwrap()];
        let found = collect_import_candidates(&catalog, &sources, &[]).candidates;
        assert_eq!(found.len(), 3);
        assert!(found.iter().all(|candidate| candidate.variant_count == 2 && candidate.already_in_catalog));
        let qoder = found.iter().find(|candidate| candidate.agent == "qoder").unwrap();
        assert_eq!(qoder.catalog_state, "differs");
        assert_eq!(qoder.differing_fields, vec!["env"]);
        assert_eq!(qoder.peer_differing_fields, vec!["env"]);
        let claude = found.iter().find(|candidate| candidate.agent == "claude_cli").unwrap();
        let kimi = found.iter().find(|candidate| candidate.agent == "kimi").unwrap();
        assert_eq!(claude.variant, kimi.variant);
        assert_ne!(claude.variant, qoder.variant);
        assert_eq!(kimi.catalog_state, "matches");
        let statuses = collect_agent_statuses(&catalog, &sources, &[]);
        let serialized = serde_json::to_string(&statuses).unwrap();
        assert!(!serialized.contains("secret-a") && !serialized.contains("secret-b"));
        assert_eq!(statuses.iter().find(|status| status.agent == "qoder").unwrap().state, "differs");
    }

    #[test]
    fn scan_keeps_new_conflicting_versions_and_skips_both_memory_names() {
        let sources = snapshots(&[
            ("claude_cli", json!({"demo": {"command": "node"}, MEMORY_SERVER_NAME: {"command": "node"}, LEGACY_MEMORY_SERVER_NAME: {"command": "node"}})),
            ("qoder", json!({"demo": {"command": "npx"}})),
        ]);
        let found = collect_import_candidates(&[], &sources, &[]).candidates;
        assert_eq!(found.len(), 2);
        assert!(found.iter().all(|candidate| candidate.catalog_state == "new" && candidate.variant_count == 2));
    }

    #[test]
    fn statuses_include_unmanaged_matches_but_only_assigned_missing_configs() {
        let catalog = vec![McpCatalogEntry {
            name: "demo".into(), command: "node".into(), assigned_agents: vec!["claude_cli".into()], ..Default::default()
        }];
        let sources = snapshots(&[
            ("claude_cli", json!({})), ("qoder", json!({"demo": {"command": "node"}})), ("kimi", json!({})),
        ]);
        let statuses = collect_agent_statuses(&catalog, &sources, &[]);
        assert_eq!(statuses.len(), 2);
        assert_eq!(statuses.iter().find(|status| status.agent == "claude_cli").unwrap().state, "missing");
        assert_eq!(statuses.iter().find(|status| status.agent == "qoder").unwrap().state, "installed");
    }

    #[test]
    fn unreadable_source_is_unavailable_and_does_not_hide_other_statuses() {
        let catalog = vec![McpCatalogEntry { name: "demo".into(), command: "node".into(), ..Default::default() }];
        let mut sources = snapshots(&[("qoder", json!({"demo": {"command": "npx"}}))]);
        sources.insert("kimi".into(), Err("invalid JSON".into()));
        let statuses = collect_agent_statuses(&catalog, &sources, &[]);
        assert_eq!(statuses.iter().find(|status| status.agent == "qoder").unwrap().state, "differs");
        let unreadable = statuses.iter().find(|status| status.agent == "kimi").unwrap();
        assert_eq!(unreadable.state, "unavailable");
        assert!(unreadable.error.is_some());
        let scan = collect_import_candidates(&catalog, &sources, &[]);
        assert_eq!(scan.candidates.len(), 1);
        assert_eq!(scan.candidates[0].agent, "qoder");
        assert_eq!(scan.issues.len(), 1);
        assert_eq!(scan.issues[0].agent, "kimi");
        assert!(scan.issues[0].name.is_none());
    }

    #[test]
    fn malformed_server_does_not_hide_healthy_servers_from_the_same_source() {
        let sources = snapshots(&[("kimi", json!({
            "broken": {"args": ["x"], "env": {"TOKEN": "do-not-expose"}},
            "healthy": {"command": "node"}
        }))]);
        let scan = collect_import_candidates(&[], &sources, &[]);
        assert_eq!(scan.candidates.len(), 1);
        assert_eq!(scan.candidates[0].entry.name, "healthy");
        assert_eq!(scan.issues.len(), 1);
        assert_eq!(scan.issues[0].agent, "kimi");
        assert_eq!(scan.issues[0].name.as_deref(), Some("broken"));
        assert!(!serde_json::to_string(&scan.issues).unwrap().contains("do-not-expose"));
    }

    #[test]
    fn scan_with_only_failed_sources_returns_issues_rather_than_a_clean_empty_result() {
        let sources = HashMap::from([
            ("codex_cli".into(), Err("invalid TOML".into())),
            ("qoder".into(), Err("invalid JSON".into())),
        ]);
        let scan = collect_import_candidates(&[], &sources, &[]);
        assert!(scan.candidates.is_empty());
        assert_eq!(scan.issues.len(), 2);
        assert!(scan.issues.iter().all(|issue| issue.name.is_none()));
        assert_eq!(scan.issues.iter().filter(|issue| issue.agent == "codex_cli").count(), 1);
    }

    #[test]
    fn codex_shared_config_is_one_import_source_and_two_status_targets() {
        let sources = snapshots(&[("codex_cli", json!({"demo": {"command": "node"}}))]);
        let found = collect_import_candidates(&[], &sources, &[]).candidates;
        assert_eq!(found.len(), 1);
        let statuses = collect_agent_statuses(&[found[0].entry.clone()], &sources, &[]);
        assert_eq!(statuses.len(), 2);
        assert!(statuses.iter().any(|status| status.agent == "codex_desktop" && status.state == "installed"));
    }

    #[test]
    fn differences_compare_connection_fields_and_ignore_order_and_metadata() {
        let first = json!({"command": "node", "args": ["a", "b"], "env": {"B": "2", "A": "1"}});
        let same = json!({"type": "stdio", "command": "node", "args": ["a", "b"], "env": {"A": "1", "B": "2"}, "enabled": false, "description": "noise"});
        assert!(configuration_differences(&first, &same).is_empty());
        let changed = json!({"command": "npx", "args": ["b", "a"]});
        assert_eq!(configuration_differences(&first, &changed), vec!["args", "command", "env"]);
        assert_eq!(configuration_differences(
            &json!({"type": "sse", "url": "https://one", "headers": {"Authorization": "secret"}}),
            &json!({"type": "http", "url": "https://two", "headers": {"Authorization": "changed"}})), vec!["headers", "transport", "url"]);
    }

    #[test]
    fn windows_launch_equivalence_is_shared_by_status_scan_and_comparison() {
        let entry = McpCatalogEntry {
            name: "demo".into(), command: "npx".into(),
            args: vec!["-y".into(), "@playwright/mcp".into(), r"C:\Tools\browser.exe".into()],
            env: BTreeMap::from([("TOKEN".into(), "same".into())]),
            assigned_agents: vec!["qoder".into()], ..Default::default()
        };
        let node = json!({"command": "cmd.exe", "args": ["/c", "npx", "-y", "@playwright/mcp", "C:/Tools/browser.exe"], "env": {"TOKEN": "same"}});
        assert!(entry_matches_config("qoder", &entry, &node));
        assert!(configuration_differences(&agent_entry_value("qoder", &entry), &node).is_empty());
        let sources = snapshots(&[("qoder", json!({"demo": node}))]);
        assert_eq!(collect_agent_statuses(&[entry.clone()], &sources, &[])[0].state, "installed");
        assert_eq!(collect_import_candidates(&[entry.clone()], &sources, &[]).candidates[0].catalog_state, "matches");
        let comparison = configuration_comparison("demo", Some(entry), &sources, &[]).unwrap();
        assert_eq!(comparison.sources.iter().find(|source| source.agent == "qoder").unwrap().state, "installed");
        assert!(configuration_differences(&json!({"command": r"C:\Tools\node.exe"}),
            &json!({"command": "C:/Tools/node.exe"})).is_empty());
    }

    #[test]
    fn shell_expansion_custom_wrappers_and_environment_changes_are_not_equivalent() {
        let direct = json!({"command": "npx", "args": ["-y", "demo"]});
        for wrapped in [
            json!({"command": "cmd", "args": ["/c", "npx -y demo"]}),
            json!({"command": "cmd", "args": ["/d", "/c", "npx", "-y", "demo"]}),
            json!({"command": "powershell", "args": ["-Command", "npx", "-y", "demo"]}),
            json!({"command": "cmd", "args": ["/c", "npx", "-y", "demo"], "env": {"TOKEN": "changed"}}),
        ] { assert!(!configuration_differences(&direct, &wrapped).is_empty()); }
        for token in ["%PACKAGE%", "!PACKAGE!", "demo&exit", "a b", "demo|exit", "demo^x", "(demo)"] {
            let direct = json!({"command": "npx", "args": [token]});
            let wrapped = json!({"command": "cmd", "args": ["/c", "npx", token]});
            assert!(!configuration_differences(&direct, &wrapped).is_empty());
        }
        assert!(!configuration_differences(&json!({"command": "node", "args": [r"relative\file.js"]}),
            &json!({"command": "node", "args": ["relative/file.js"]})).is_empty());
    }

    #[test]
    fn detached_observations_require_confirmed_idle_and_preserve_custom_or_proxy_nodes() {
        let (_directory, manifest, canonical) = synthetic_observation();
        let entry = McpCatalogEntry { name: "demo".into(), command: "node".into(), ..Default::default() };
        for current in [None, Some(&canonical)] {
            assert!(detached_observation(&manifest, Some(&entry), current, Some(0), 0));
            assert!(!detached_observation(&manifest, Some(&entry), current, None, 0));
            assert!(!detached_observation(&manifest, Some(&entry), current, Some(1), 0));
            assert!(!detached_observation(&manifest, Some(&entry), current, Some(0), 1));
        }
        assert!(!detached_observation(&manifest, Some(&entry), Some(&proxy_entry(&manifest)), Some(0), 0));
        assert!(!detached_observation(&manifest, Some(&entry), Some(&json!({"command": "custom"})), Some(0), 0));
        let mut preparing = manifest.clone();
        preparing.phase = "preparing".into();
        preparing.previous_proxy_command = Some("old-launcher".into());
        let mut previous = preparing.clone();
        previous.proxy_command = "old-launcher".into();
        assert!(!detached_observation(&preparing, Some(&entry), Some(&proxy_entry(&previous)), Some(0), 0));
        let sources = snapshots(&[("claude_desktop", json!({"demo": canonical}))]);
        assert_eq!(collect_import_candidates(&[entry], &sources, &[manifest]).candidates.len(), 1);
    }

    #[test]
    fn orphan_proxy_is_never_an_adoptable_upstream_configuration() {
        let node = json!({"command": "old-launcher", "args": ["--mcp-relay", "orphan"]});
        let sources = snapshots(&[("qoder", json!({"demo": node}))]);
        assert!(collect_import_candidates(&[], &sources, &[]).candidates.is_empty());
        let comparison = configuration_comparison("demo", None, &sources, &[]).unwrap();
        let source = comparison.sources.iter().find(|source| source.agent == "qoder").unwrap();
        assert_eq!(source.state, "differs");
        assert!(source.entry.is_none());
    }

    #[test]
    fn stale_proxy_fingerprint_is_not_installed_or_adoptable_in_comparison() {
        let (_directory, manifest, _) = synthetic_observation();
        let entry = McpCatalogEntry { name: "demo".into(), command: "changed".into(),
            assigned_agents: vec!["claude_desktop".into()], ..Default::default() };
        let sources = snapshots(&[("claude_desktop", json!({"demo": proxy_entry(&manifest)}))]);
        assert_eq!(collect_agent_statuses(&[entry.clone()], &sources, &[manifest.clone()])[0].state, "differs");
        let comparison = configuration_comparison("demo", Some(entry), &sources, &[manifest]).unwrap();
        let source = comparison.sources.iter().find(|source| source.agent == "claude_desktop").unwrap();
        assert_eq!(source.state, "differs");
        assert!(source.entry.is_none());
        assert!(comparison.blocked_reason.is_some());
    }

    #[test]
    fn managed_observation_proxy_is_not_reported_as_a_config_variant() {
        let (_directory, manifest, _) = synthetic_observation();
        let entry = McpCatalogEntry {
            name: "demo".into(), command: "node".into(),
            assigned_agents: vec!["claude_desktop".into()], ..Default::default()
        };
        let sources = snapshots(&[("claude_desktop", json!({"demo": proxy_entry(&manifest)}))]);
        let status = collect_agent_statuses(&[entry.clone()], &sources, &[manifest.clone()]);
        assert_eq!(status[0].state, "installed");
        assert!(status[0].differing_fields.is_empty());
        assert!(collect_import_candidates(&[entry.clone()], &sources, &[manifest.clone()]).candidates.is_empty());
        let mut changed = entry;
        changed.args.push("updated.js".into());
        let status = collect_agent_statuses(&[changed], &sources, &[manifest]);
        assert_eq!(status[0].state, "differs");
    }

    #[test]
    fn arbitration_revision_changes_only_when_the_reviewed_service_changes() {
        let original = snapshots(&[("qoder", json!({
            "demo": {"command": "node", "env": {"TOKEN": "private-value"}},
            "other": {"command": "old"}
        }))]);
        let revision = comparison_revision("demo", None, &original, &[]).unwrap();
        let mut changed = original.clone();
        changed.get_mut("qoder").unwrap().as_mut().unwrap().insert("other".into(), json!({"command": "new"}));
        assert_eq!(comparison_revision("demo", None, &changed, &[]).unwrap(), revision);
        changed.get_mut("qoder").unwrap().as_mut().unwrap().insert("demo".into(), json!({"command": "npx"}));
        assert_ne!(comparison_revision("demo", None, &changed, &[]).unwrap(), revision);
        assert_eq!(revision.len(), 64);
        assert!(!revision.contains("private-value"));
    }

    #[test]
    fn arbitration_comparison_preserves_unimported_sources_and_unreadable_states() {
        let mut sources = snapshots(&[
            ("qoder", json!({"demo": {"command": "node"}})),
            ("kimi", json!({"demo": {"command": "npx"}})),
        ]);
        sources.insert("claude_cli".into(), Err("invalid JSON".into()));
        let comparison = configuration_comparison("demo", None, &sources, &[]).unwrap();
        assert!(comparison.catalog.is_none());
        assert_eq!(comparison.sources.iter().filter(|source| source.state == "unmanaged").count(), 2);
        let unreadable = comparison.sources.iter().find(|source| source.agent == "claude_cli").unwrap();
        assert_eq!(unreadable.state, "unavailable");
        assert!(unreadable.entry.is_none() && unreadable.error.is_some());
    }

    #[test]
    fn arbitration_proxy_is_not_an_adoptable_source_and_revision_order_is_stable() {
        let (_directory, manifest, _) = synthetic_observation();
        let entry = McpCatalogEntry {
            name: "demo".into(), command: "node".into(),
            assigned_agents: vec!["claude_desktop".into()], ..Default::default()
        };
        let sources = snapshots(&[("claude_desktop", json!({"demo": proxy_entry(&manifest)}))]);
        let mut other = manifest.clone();
        other.agent = "qoder".into();
        other.id = "other".into();
        let first = configuration_comparison("demo", Some(entry.clone()), &sources, &[manifest.clone(), other.clone()]).unwrap();
        let second = configuration_comparison("demo", Some(entry), &sources, &[other, manifest]).unwrap();
        assert_eq!(first.revision, second.revision);
        assert!(first.blocked_reason.is_some());
        let proxy = first.sources.iter().find(|source| source.agent == "claude_desktop").unwrap();
        assert_eq!(proxy.state, "observed");
        assert!(proxy.observed);
    }

    fn legacy(text: &str) -> Vec<McpCatalogEntry> {
        parse_legacy_config(text).unwrap()
    }

    #[test]
    fn migration_merges_same_configuration_without_losing_assignments() {
        let entries = legacy(r#"{"mcpServers":{"Demo":{"command":"node","args":["server.js"],"env":{"B":"2","A":"1"},"description":"legacy"}}}"#);
        let mut central = entries[0].clone();
        central.transport.clear(); // 空 transport 与 stdio 等价。
        central.description = "central".into();
        central.created_at = "before".into();
        central.assigned_agents = vec!["qoder".into(), "claude_cli".into()];
        let mut catalog = vec![central.clone()];
        assert!(merge_legacy_entries(&mut catalog, entries, "now").is_empty());
        assert_eq!(catalog.len(), 1);
        assert_eq!(serde_json::to_value(&catalog[0]).unwrap(), serde_json::to_value(central).unwrap());
    }

    #[test]
    fn migration_conflicts_keep_both_and_reserve_original_names() {
        let entries = legacy(r#"{"mcpServers":{
            "Demo":{"command":"node","env":{"TOKEN":"env-secret"},"headers":{"Authorization":"header-secret"}},
            "legacy-Demo":{"command":"reserved"}
        }}"#);
        let mut central = entries[0].clone();
        central.name = "Demo".into();
        central.command = "central".into();
        central.assigned_agents = vec!["qoder".into()];
        let mut catalog = vec![central.clone()];
        let report = merge_legacy_entries(&mut catalog, entries, "now");
        assert_eq!(catalog.len(), 3);
        assert_eq!(find_entry(&catalog, "Demo").unwrap().command, "central");
        assert_eq!(find_entry(&catalog, "Demo").unwrap().assigned_agents, central.assigned_agents);
        assert_eq!(find_entry(&catalog, "legacy-Demo").unwrap().command, "reserved");
        let copy = find_entry(&catalog, "legacy-Demo-2").unwrap();
        assert_eq!(copy.env["TOKEN"], "env-secret");
        assert_eq!(copy.headers["Authorization"], "header-secret");
        assert!(copy.assigned_agents.is_empty());
        assert_eq!(report.len(), 1);
        assert!(report[0].contains("\"Demo\""));
        assert!(report[0].contains("\"legacy-Demo-2\""));
        assert!(!report[0].contains("env-secret"));
        assert!(!report[0].contains("header-secret"));
    }

    #[test]
    fn migration_retry_reuses_existing_conflict_copy() {
        let entries = legacy(r#"{"mcpServers":{"Demo":{"command":"legacy"},"legacy-Demo":{"command":"reserved"}}}"#);
        let mut catalog = vec![McpCatalogEntry {
            name: "Demo".into(), command: "central".into(), ..Default::default()
        }];
        let first_report = merge_legacy_entries(&mut catalog, entries.clone(), "first");
        // 模拟目录已持久化、迁移标记尚未写入的重试；配置匹配仍能去重。
        let persisted = serde_json::to_string(&catalog).unwrap();
        let mut retry: Vec<McpCatalogEntry> = serde_json::from_str(&persisted).unwrap();
        let retry_report = merge_legacy_entries(&mut retry, entries, "retry");
        assert_eq!(retry_report, first_report);
        assert_eq!(serde_json::to_string(&retry).unwrap(), persisted);
    }

    #[test]
    fn migration_preserves_case_and_old_names_remain_editable() {
        let entries = legacy(r#"{"mcpServers":{"Demo":{"command":"node"},"demo":{"command":"node"}," Old MCP ":{"command":"node"}}}"#);
        let mut catalog = Vec::new();
        assert!(merge_legacy_entries(&mut catalog, entries, "created").is_empty());
        assert_eq!(catalog.len(), 3);
        assert!(find_entry(&catalog, "Demo").is_ok());
        assert!(find_entry(&catalog, "demo").is_ok());
        let old = catalog.iter_mut().find(|entry| entry.name == " Old MCP ").unwrap();
        old.assigned_agents = vec!["qoder".into()];
        let update = McpCatalogEntry {
            name: old.name.clone(), command: "updated".into(), ..Default::default()
        };
        let updated = upsert_entry(&mut catalog, update, "edited").unwrap();
        assert_eq!(updated.name, " Old MCP ");
        assert_eq!(updated.created_at, "created");
        assert_eq!(updated.assigned_agents, vec!["qoder"]);
        assert_eq!(catalog.len(), 3);
        assert!(upsert_entry(&mut catalog, McpCatalogEntry {
            name: "New MCP".into(), command: "node".into(), ..Default::default()
        }, "now").is_err());
    }

    #[test]
    fn migration_preserves_remote_and_auxiliary_fields() {
        for (fields, expected) in [
            (json!({"transport":"sse", "type":"sse"}), "sse"),
            (json!({"transport":"sse"}), "sse"),
            (json!({"type":"http"}), "http"),
            (json!({}), "http"),
        ] {
            let mut config = json!({
                "url":"https://example.com/mcp", "headers":{"Authorization":"secret"},
                "command":"node", "args":["a","b"], "env":{"TOKEN":"value"}, "description":"remote"
            });
            config.as_object_mut().unwrap().extend(fields.as_object().unwrap().clone());
            let text = json!({"mcpServers":{"Remote":config}}).to_string();
            let entry = legacy(&text).remove(0);
            assert_eq!(entry.transport, expected);
            let runtime = runtime_server(entry.clone());
            assert_eq!(runtime.name, "Remote");
            assert_eq!(runtime.transport, expected);
            assert_eq!(runtime.url, "https://example.com/mcp");
            assert_eq!(runtime.headers["Authorization"], "secret");
            assert_eq!(runtime.command, "node");
            assert_eq!(runtime.args, vec!["a", "b"]);
            assert_eq!(runtime.env["TOKEN"], "value");
            assert_eq!(runtime.description, "remote");
            let mut changed = entry.clone();
            changed.headers.insert("Authorization".into(), "different".into());
            assert!(!same_configuration(&entry, &changed));
            let mut changed = entry.clone();
            changed.env.insert("TOKEN".into(), "different".into());
            assert!(!same_configuration(&entry, &changed));
        }
    }

    #[test]
    fn migration_rejects_corrupt_or_lossy_configuration_without_exposing_values() {
        for text in ["", "   ", "{", "null", "[]", r#"{"mcpServers":null}"#,
                     r#"{"mcpServers":[]}"#, r#"{"mcpServers":{"bad":{}}}"#,
                     r#"{"mcpServers":{"bad":{"command":"node","args":[1]}}}"#,
                     r#"{"mcpServers":{"bad":{"command":"node","env":{"TOKEN":7}}}}"#,
                     r#"{"mcpServers":{"bad":{"command":"node","headers":"header-secret"}}}"#,
                     r#"{"mcpServers":{"bad":{"type":"ftp","url":"https://example.com"}}}"#,
                     r#"{"mcpServers":{"bad":{"transport":"sse","type":"http","url":"https://example.com"}}}"#] {
            let error = parse_legacy_config(text).unwrap_err();
            assert!(!error.contains("header-secret"));
        }
        assert!(legacy("{}").is_empty());
        assert!(legacy(r#"{"mcpServers":{}}"#).is_empty());
    }

    #[test]
    fn upsert_accepts_http_and_rejects_unknown_transport() {
        let mut catalog = Vec::new();
        let entry = McpCatalogEntry {
            name: "remote".into(), transport: "http".into(),
            url: "https://example.com/mcp".into(), ..Default::default()
        };
        assert_eq!(upsert_entry(&mut catalog, entry.clone(), "now").unwrap().transport, "http");
        let invalid = McpCatalogEntry { transport: "ftp".into(), ..entry };
        assert!(upsert_entry(&mut catalog, invalid, "later").is_err());
        assert_eq!(catalog[0].transport, "http");
        assert!(!supports_transport("claude_cli", "ftp"));
    }

    #[test]
    fn explicit_import_preserves_old_names_and_merges_assignments() {
        let entry = legacy(r#"{"mcpServers":{" Old MCP ":{"command":"node"}}}"#).remove(0);
        let mut catalog = Vec::new();
        let imported = import_entry(&mut catalog, entry.clone(), "qoder", "first").unwrap();
        assert_eq!(imported.name, " Old MCP ");
        let imported = import_entry(&mut catalog, entry.clone(), "claude_desktop", "second").unwrap();
        assert_eq!(imported.assigned_agents, vec!["claude_desktop", "qoder"]);
        let repeated = import_entry(&mut catalog, entry, "claude_desktop", "third").unwrap();
        assert_eq!(repeated.assigned_agents, imported.assigned_agents);
        assert_eq!(catalog.len(), 1);
        assert_eq!(repeated.created_at, "first");
        let edited = upsert_entry(&mut catalog, McpCatalogEntry {
            name: " Old MCP ".into(), command: "updated".into(), ..Default::default()
        }, "edited").unwrap();
        assert_eq!(edited.assigned_agents, imported.assigned_agents);
    }

    #[test]
    fn names_must_be_lowercase_ids() {
        assert!(valid_name("playwright"));
        assert!(valid_name("my-mcp_2"));
        assert!(!valid_name(""));
        assert!(!valid_name("My MCP"));
        assert!(!valid_name("服务器"));
    }

    #[test]
    fn minimax_stdio_entry_has_type_and_enabled() {
        let entry = McpCatalogEntry {
            name: "demo".into(),
            transport: "stdio".into(),
            command: "npx".into(),
            args: vec!["-y".into(), "@playwright/mcp".into()],
            ..Default::default()
        };
        let value = agent_entry_value("minimax", &entry);
        assert_eq!(value["type"], "stdio");
        assert_eq!(value["enabled"], true);
        assert_eq!(value["command"], "npx");
        // 非 minimax 的文件型 Agent 不带 type/enabled
        let plain = agent_entry_value("qoder", &entry);
        assert!(plain.get("type").is_none());
        assert!(plain.get("enabled").is_none());
    }

    #[test]
    fn zcode_entry_keeps_minimal_shape() {
        let entry = McpCatalogEntry {
            name: "demo".into(),
            transport: "stdio".into(),
            command: "npx".into(),
            args: vec!["-y".into(), "x".into()],
            description: "描述".into(),
            ..Default::default()
        };
        let value = agent_entry_value("zcode", &entry);
        assert_eq!(value["command"], "npx");
        // zcode schema 未文档化扩展键：不写 description/type/enabled。
        assert!(value.get("description").is_none());
        assert!(value.get("type").is_none());
        // 用户手工配置的最小 command/args 形状应视为已安装。
        let configured = json!({"command": "npx", "args": ["-y", "x"]});
        assert!(entry_matches_config("zcode", &entry, &configured));
        // zcode 仅支持 stdio。
        assert!(supports_transport("zcode", "stdio"));
        assert!(!supports_transport("zcode", "sse"));
    }

    #[test]
    fn claude_desktop_remote_entry_uses_type_url() {
        let entry = McpCatalogEntry {
            name: "remote".into(),
            transport: "sse".into(),
            url: "https://example.com/sse".into(),
            ..Default::default()
        };
        let value = agent_entry_value("claude_desktop", &entry);
        assert_eq!(value["type"], "sse");
        assert_eq!(value["url"], "https://example.com/sse");
    }

    #[test]
    fn shape_comparison_ignores_noise_and_key_order() {
        let expected = json!({"type": "stdio", "command": "npx", "args": ["-y", "x"], "env": {"A": "1", "B": "2"}});
        // 配置侧：多了 enabled/description、env 键序不同、type 缺省 → 仍视为一致
        let configured = json!({"command": "npx", "args": ["-y", "x"], "env": {"B": "2", "A": "1"}, "enabled": true, "description": "whatever"});
        assert!(value_equal(
            &normalized_shape(&expected),
            &normalized_shape(&configured)
        ));
        // 参数不同 → 不一致
        let drifted = json!({"type": "stdio", "command": "npx", "args": ["-y", "y"], "env": {"A": "1", "B": "2"}});
        assert!(!value_equal(
            &normalized_shape(&expected),
            &normalized_shape(&drifted)
        ));
    }

    #[test]
    fn imports_stdio_and_remote_shapes() {
        let stdio = json!({"command": "npx", "args": ["-y", "@playwright/mcp"], "env": {"K": "V"}});
        let entry = entry_from_config("playwright", "claude_cli", &stdio).unwrap();
        assert_eq!(entry.transport, "stdio");
        assert_eq!(entry.command, "npx");
        assert_eq!(entry.env.get("K").map(String::as_str), Some("V"));
        assert_eq!(entry.assigned_agents, vec!["claude_cli".to_string()]);

        let remote = json!({"type": "sse", "url": "https://example.com/sse"});
        let entry = entry_from_config("remote", "claude_desktop", &remote).unwrap();
        assert_eq!(entry.transport, "sse");
        assert_eq!(entry.url, "https://example.com/sse");

        // 畸形条目必须向调用方报告，不能伪装成扫描无结果。
        let error = entry_from_config("broken", "kimi", &json!({"args": ["x"]})).unwrap_err();
        assert!(error.contains("kimi"));
        assert!(error.contains("broken"));
    }

    #[test]
    fn profile_upsert_normalizes_and_preserves_created_at() {
        let mut profiles = vec![McpProfile {
            name: "dev".into(),
            description: "old".into(),
            servers: vec!["playwright".into()],
            created_at: "first".into(),
            updated_at: "first".into(),
        }];
        let saved = upsert_profile(
            &mut profiles,
            McpProfile {
                name: " dev ".into(),
                description: " 日常开发 ".into(),
                servers: vec!["playwright".into(), "".into(), "fetch ".into(), "playwright".into()],
                created_at: "ignored".into(),
                updated_at: "ignored".into(),
            },
            "now",
        )
        .unwrap();
        assert_eq!(saved.name, "dev");
        assert_eq!(saved.description, "日常开发");
        assert_eq!(saved.servers, vec!["playwright", "fetch"]);
        assert_eq!(saved.created_at, "first");
        assert_eq!(saved.updated_at, "now");
        assert_eq!(profiles.len(), 1);
        // 全新档案使用当前时间作为 created_at。
        let created = upsert_profile(
            &mut profiles,
            McpProfile {
                name: "data".into(),
                servers: vec!["duckdb".into()],
                ..Default::default()
            },
            "now",
        )
        .unwrap();
        assert_eq!(created.created_at, "now");
        assert_eq!(profiles.len(), 2);
    }

    #[test]
    fn profile_names_reject_empty_long_and_control_characters() {
        let mut profiles = Vec::new();
        for bad in ["", "   ", "a\u{7}b"] {
            assert!(upsert_profile(
                &mut profiles,
                McpProfile { name: bad.into(), ..Default::default() },
                "now",
            )
            .is_err());
        }
        let long: String = std::iter::repeat('x').take(65).collect();
        assert!(upsert_profile(
            &mut profiles,
            McpProfile { name: long, ..Default::default() },
            "now",
        )
        .is_err());
        let max: String = std::iter::repeat('x').take(64).collect();
        assert!(upsert_profile(
            &mut profiles,
            McpProfile { name: max, ..Default::default() },
            "now",
        )
        .is_ok());
        // 空服务器集合是合法档案（一键清空启用集）。
        let empty_servers = upsert_profile(
            &mut profiles,
            McpProfile { name: "bare".into(), ..Default::default() },
            "now",
        )
        .unwrap();
        assert!(empty_servers.servers.is_empty());
    }

    fn synthetic_observation() -> (tempfile::TempDir, ObservationManifest, Value) {
        let directory = tempfile::tempdir().unwrap();
        let config_path = directory.path().join("agent.json");
        let entry = McpCatalogEntry {
            name: "demo".into(), command: "node".into(),
            assigned_agents: vec!["claude_desktop".into()], ..Default::default()
        };
        let canonical = observation_canonical("claude_desktop", &entry);
        std::fs::write(&config_path, serde_json::to_string(&json!({
            "mcpServers": {"demo": canonical}, "theme": "light"
        })).unwrap()).unwrap();
        let manifest = ObservationManifest {
            id: uuid::Uuid::new_v4().to_string(), name: "demo".into(), agent: "claude_desktop".into(),
            config_path, fingerprint: observation_fingerprint("claude_desktop", &entry),
            proxy_command: directory.path().join("launcher").to_string_lossy().into_owned(),
            relay_command: None, previous_proxy_command: None, previous_relay_command: None, omitted_args: false, phase: "active".into(),
        };
        (directory, manifest, canonical)
    }

    #[test]
    fn observation_upgrade_refreshes_legacy_and_sidecar_endpoints() {
        for previous_relay in [None, Some("old-relay".to_string())] {
            let (_directory, mut manifest, canonical) = synthetic_observation();
            manifest.relay_command = previous_relay.clone();
            patch_observation_node(&manifest, &canonical, &proxy_entry(&manifest)).unwrap();
            let mut saved = Vec::new();
            let next = upgrade_observation_node(&manifest, "new-launcher".into(), "new-relay".into(), |state| {
                saved.push(state.clone());
                Ok(())
            }).unwrap();
            assert_eq!(saved.len(), 2);
            assert_eq!(saved[0].phase, "preparing");
            assert_eq!(saved[0].previous_proxy_command.as_ref(), Some(&manifest.proxy_command));
            assert_eq!(saved[0].previous_relay_command, previous_relay);
            assert_eq!(next.phase, "active");
            assert_eq!(next.proxy_command, "new-launcher");
            assert_eq!(next.relay_command.as_deref(), Some("new-relay"));
            assert!(next.previous_proxy_command.is_none());
            assert!(next.previous_relay_command.is_none());
            assert_eq!(next.id, manifest.id);
            assert_eq!(next.fingerprint, manifest.fingerprint);
            assert_eq!(read_json_config(&manifest.config_path).unwrap()["theme"], "light");
            let node = read_observation_node(&manifest.agent, &manifest.config_path, &manifest.name).unwrap();
            assert_eq!(node, Some(proxy_entry(&next)));
            let rollback = recover_upgrade_node(&saved[0], Some(&proxy_entry(&manifest))).unwrap();
            assert_eq!(rollback.proxy_command, manifest.proxy_command);
            assert_eq!(rollback.relay_command, manifest.relay_command);
            let committed = recover_upgrade_node(&saved[0], node.as_ref()).unwrap();
            assert_eq!(committed.relay_command, next.relay_command);
            assert_eq!(committed.phase, "active");
            let unchanged = upgrade_observation_node(&next, "new-launcher".into(), "new-relay".into(), |_| {
                panic!("unchanged endpoints must not rewrite the manifest")
            }).unwrap();
            assert_eq!(unchanged.proxy_command, next.proxy_command);
        }
    }

    #[test]
    fn observation_relay_only_upgrade_recovers_after_final_save_failure() {
        let (_directory, mut manifest, canonical) = synthetic_observation();
        manifest.relay_command = Some("old-relay".into());
        patch_observation_node(&manifest, &canonical, &proxy_entry(&manifest)).unwrap();
        let mut preparing = None;
        let result = upgrade_observation_node(&manifest, manifest.proxy_command.clone(), "new-relay".into(), |state| {
            if state.phase == "preparing" {
                preparing = Some(state.clone());
                Ok(())
            } else {
                Err("simulated final save failure".into())
            }
        });
        assert!(result.is_err());
        let current = read_observation_node(&manifest.agent, &manifest.config_path, &manifest.name).unwrap();
        assert_eq!(current, Some(proxy_entry(&manifest)));
        let recovered = recover_upgrade_node(&preparing.unwrap(), current.as_ref()).unwrap();
        assert_eq!(recovered.phase, "active");
        assert_eq!(recovered.relay_command.as_deref(), Some("new-relay"));
        assert!(recovered.previous_relay_command.is_none());
    }

    #[test]
    fn observation_upgrade_rejects_config_drift_and_restores_old_manifest_shape() {
        let (_directory, mut manifest, _canonical) = synthetic_observation();
        manifest.relay_command = Some("old-relay".into());
        let mut saved = Vec::new();
        assert!(upgrade_observation_node(&manifest, "new-launcher".into(), "new-relay".into(), |state| {
            saved.push(state.clone());
            Ok(())
        }).is_err());
        assert_eq!(saved.len(), 1);
        assert!(recover_upgrade_node(&saved[0], Some(&json!({"command": "custom"}))).is_err());
        let mut legacy = serde_json::to_value(&saved[0]).unwrap();
        legacy.as_object_mut().unwrap().remove("previous_relay_command");
        let legacy: ObservationManifest = serde_json::from_value(legacy).unwrap();
        let old = recover_upgrade_node(&legacy, Some(&proxy_entry(&manifest))).unwrap();
        assert!(old.relay_command.is_none());
        assert_eq!(old.phase, "active");
        assert_eq!(read_json_config(&manifest.config_path).unwrap()["mcpServers"]["demo"]["command"], "node");
    }

    #[test]
    fn observation_retry_recovers_original_and_preserves_other_fields() {
        let (_directory, manifest, canonical) = synthetic_observation();
        let mut checks = 0;
        activate_observation_node(&manifest, &canonical, || {
            checks += 1;
            if checks == 1 {
                let mut config = read_json_config(&manifest.config_path).unwrap();
                config["mcpServers"]["demo"] = canonical.clone();
                config["unrelated"] = json!("saved");
                std::fs::write(&manifest.config_path, serde_json::to_string(&config).unwrap()).unwrap();
            }
        }).unwrap();
        let config = read_json_config(&manifest.config_path).unwrap();
        assert_eq!(config["mcpServers"]["demo"], proxy_entry(&manifest));
        assert_eq!(config["unrelated"], "saved");
        patch_observation_node(&manifest, &proxy_entry(&manifest), &canonical).unwrap();
        assert_eq!(read_observation_node(&manifest.agent, &manifest.config_path, &manifest.name).unwrap(), Some(canonical));
    }

    #[test]
    fn observation_retry_stops_after_repeated_reverts() {
        let (_directory, manifest, canonical) = synthetic_observation();
        let result = activate_observation_node(&manifest, &canonical, || {
            let mut config = read_json_config(&manifest.config_path).unwrap();
            config["mcpServers"]["demo"] = canonical.clone();
            std::fs::write(&manifest.config_path, serde_json::to_string(&config).unwrap()).unwrap();
        });
        assert!(result.unwrap_err().contains("反复恢复"));
        assert_eq!(read_observation_node(&manifest.agent, &manifest.config_path, &manifest.name).unwrap(), Some(canonical));
    }

    #[test]
    fn observation_retry_rejects_custom_node() {
        let (_directory, manifest, canonical) = synthetic_observation();
        let result = activate_observation_node(&manifest, &canonical, || {
            let mut config = read_json_config(&manifest.config_path).unwrap();
            config["mcpServers"]["demo"] = json!({"command": "custom"});
            std::fs::write(&manifest.config_path, serde_json::to_string(&config).unwrap()).unwrap();
        });
        assert!(result.unwrap_err().contains("未覆盖用户修改"));
        assert_eq!(read_json_config(&manifest.config_path).unwrap()["mcpServers"]["demo"]["command"], "custom");
        assert!(patch_observation_node(&manifest, &canonical, &proxy_entry(&manifest)).is_err());
    }

    #[test]
    fn observation_preserves_codex_shape_and_other_tables() {
        let entry = McpCatalogEntry {
            name: "demo".into(), command: "node".into(),
            env: BTreeMap::from([("TOKEN".into(), "private".into())]),
            ..Default::default()
        };
        let manifest = ObservationManifest {
            id: uuid::Uuid::new_v4().to_string(), name: entry.name.clone(),
            agent: "codex_cli".into(), config_path: PathBuf::from("config.toml"),
            fingerprint: observation_fingerprint("codex_cli", &entry),
            proxy_command: "relay.exe".into(), relay_command: None,
            previous_proxy_command: None, previous_relay_command: None, omitted_args: true, phase: "active".into(),
        };
        let original = "# keep this comment\n[mcp_servers.demo]\ncommand = \"node\"\nenv = { TOKEN = \"private\" }\n\n[features]\nflag = true\n";
        let canonical = canonical_for_manifest(&manifest, &entry);
        assert!(canonical.get("args").is_none());
        let wrapped = patch_codex_node(original, &manifest, &proxy_entry(&manifest)).unwrap();
        assert!(wrapped.contains("# keep this comment"));
        assert!(wrapped.contains("[features]\nflag = true"));
        let restored = patch_codex_node(&wrapped, &manifest, &canonical).unwrap();
        assert_eq!(restored, original.replace("[mcp_servers.demo]\ncommand = \"node\"\nenv = { TOKEN = \"private\" }\n", &codex_node_lines(&manifest, &canonical).unwrap()));
        assert_eq!(physical_target("codex_desktop"), "codex_cli");
    }

    // Run explicitly against an existing Claude Desktop configuration after
    // registering web-search-prime in the central catalog for this smoke test.
    #[tokio::test]
    #[ignore]
    async fn smoke_claude_desktop_observation_toggle() {
        assert_eq!(std::env::var("VIBE_OBSERVATION_TOGGLE_SMOKE").as_deref(), Ok("1"));
        crate::telemetry_store::init_shared(crate::telemetry_store::TelemetryStore::new().unwrap());
        let path = observation_config_path("claude_desktop").unwrap();
        let original = read_json_config(&path).unwrap();
        let original_node = original["mcpServers"]["web-search-prime"].clone();
        assert!(original_node.is_object());
        assert!(!observation_manifests().unwrap().iter().any(|m|
            m.name == "web-search-prime" && m.agent == "claude_desktop"));

        let enabled = mcp_observation_set("web-search-prime".into(), "claude_desktop".into(), true).await;
        let mut checked = Err("Enabling observation failed".to_string());
        if let Ok(status) = &enabled {
            checked = (|| -> Result<(), String> {
                if status.state != "awaiting_connection" { return Err("Unexpected enabled status".into()); }
                let manifests = observation_manifests()?;
                let manifest = manifests.iter().find(|m|
                    m.name == "web-search-prime" && m.agent == "claude_desktop")
                    .ok_or("Observation manifest missing")?;
                if manifest.phase != "active" || manifest.relay_command.is_none() {
                    return Err("Observation manifest not active".into());
                }
                let current = read_json_config(&path)?;
                if current["mcpServers"]["web-search-prime"] != proxy_entry(manifest) {
                    return Err("Launcher entry not installed".into());
                }
                let mut expected = original.clone();
                expected["mcpServers"]["web-search-prime"] = proxy_entry(manifest);
                if current != expected { return Err("Unrelated configuration changed on enable".into()); }
                Ok(())
            })();
        }
        // Always attempt the real disable path, even if an intermediate check fails.
        let disabled = if observation_manifests().unwrap().iter().any(|m|
            m.name == "web-search-prime" && m.agent == "claude_desktop") {
            mcp_observation_set("web-search-prime".into(), "claude_desktop".into(), false).await
        } else { Err("Observation manifest missing after enable".into()) };
        let final_config = read_json_config(&path).unwrap();
        assert_eq!(final_config, original, "Configuration values changed after disable");
        assert_eq!(final_config["mcpServers"]["web-search-prime"], original_node);
        assert!(!observation_manifests().unwrap().iter().any(|m|
            m.name == "web-search-prime" && m.agent == "claude_desktop"));
        assert!(checked.is_ok(), "{}", checked.unwrap_err());
        assert_eq!(disabled.unwrap().state, "off");
        println!("observation_toggle: enabled_launcher=true, disabled_original_node=true, other_nodes_unchanged=true, manifest_removed=true");
    }

    #[test]
    fn observation_canonical_rejects_extra_fields_and_keeps_minimax_shape() {
        let entry = McpCatalogEntry { name: "demo".into(), command: "node".into(), ..Default::default() };
        let canonical = observation_canonical("minimax", &entry);
        assert_eq!(canonical["type"], "stdio");
        assert_eq!(canonical["enabled"], true);
        assert!(canonical_matches("minimax", &entry, &canonical));
        let mut altered = canonical;
        altered["extra"] = json!("custom");
        assert!(!canonical_matches("minimax", &entry, &altered));
    }

    #[test]
    fn profile_roundtrips_through_serde_json() {
        let profile = McpProfile {
            name: "dev".into(),
            description: "开发".into(),
            servers: vec!["playwright".into(), "fetch".into()],
            created_at: "t1".into(),
            updated_at: "t2".into(),
        };
        let value = serde_json::to_value(&profile).unwrap();
        assert_eq!(
            value,
            json!({
                "name": "dev",
                "description": "开发",
                "servers": ["playwright", "fetch"],
                "created_at": "t1",
                "updated_at": "t2"
            })
        );
        // 缺省字段（旧数据/精简形态）按默认值读回。
        let minimal: McpProfile =
            serde_json::from_str(r#"{"name":"bare"}"#).unwrap();
        assert_eq!(minimal.name, "bare");
        assert!(minimal.servers.is_empty());
        assert!(minimal.description.is_empty());
    }
}
