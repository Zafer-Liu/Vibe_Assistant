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

use crate::memory_mcp::{
    agent_label, cli_detail, desktop_config_path, run_agent_cli, servers_map, servers_map_mut,
};

const MCP_CATALOG_SETTING_KEY: &str = "mcp_catalog";
const MCP_MIGRATION_SETTING_KEY: &str = "mcp_catalog_legacy_migration_v1";
/// 已命名的「启用 MCP 服务器集合」档案（工作流运行侧选择快照）。
const MCP_PROFILES_SETTING_KEY: &str = "mcp_profiles";
/// 共享记忆 MCP 的服务器名：导入时跳过，避免把开发期运行副本收编入库。
const MEMORY_SERVER_NAME: &str = "agent-manager-memory";

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
    /// "installed" | "missing" | "differs"
    pub state: String,
}

/// 「从 Agent 导入」发现的候选：来自某个 Agent 的现有配置。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub struct McpImportCandidate {
    pub agent: String,
    pub entry: McpCatalogEntry,
    pub already_in_catalog: bool,
}

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
    )
}

/// stdio 所有目标可用；sse/http 仅 Claude 系支持（其余 Agent 的 mcp.json
/// 远程 MCP 格式未文档化，v1 不冒险写入；zcode 同理，仅 stdio）。
fn supports_transport(agent: &str, transport: &str) -> bool {
    match transport {
        "stdio" => is_mcp_target(agent),
        "sse" | "http" => matches!(agent, "claude_cli" | "claude_desktop"),
        _ => false,
    }
}

fn normalize_transport(transport: &str) -> &str {
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
    let result = action(&mut catalog, &state)?;
    if write || !migrated {
        write_setting(&tx, MCP_CATALOG_SETTING_KEY, &catalog)?;
    }
    if !migrated {
        write_setting(&tx, MCP_MIGRATION_SETTING_KEY, &state)?;
    }
    tx.commit().map_err(|error| format!("提交 MCP 目录失败：{error}"))?;
    Ok(result)
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
/// （zcode 嵌套在 mcp.servers，其余为根级 mcpServers）。
fn read_agent_mcp_servers(agent: &str) -> Result<Map<String, Value>, String> {
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
    if !entry.description.is_empty() && agent != "zcode" {
        // zcode 的服务器 schema 未文档化扩展键，保持最小 command/args 形状。
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

/// 忽略 description/enabled 等噪音，只保留语义字段并补默认值的形状。
fn normalized_shape(value: &Value) -> Value {
    let transport = match config_transport(value) {
        Ok(transport) => transport,
        Err(_) => return json!({"invalid_transport": true}),
    };
    let mut shape = json!({ "type": transport });
    if transport == "stdio" {
        shape["command"] = value.get("command").cloned().unwrap_or(json!(""));
        shape["args"] = value.get("args").cloned().unwrap_or(json!([]));
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
/// memory_mcp_install 一致）。env 走 `-e K=V`，远程走 `--transport`。
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
        args.push("-e".into());
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
    } else {
        remove_json_entry(agent, name)
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
    apply_unequip(&agent, &name)?;
    save_assignment(&name, &agent, false)
}

/// 全部已装备条目 × Agent 的状态总览。只读配置文件，不 spawn CLI。
#[tauri::command]
pub async fn mcp_status_all() -> Result<Vec<McpAgentStatus>, String> {
    let catalog = read_catalog()?;
    let mut cache: HashMap<String, Map<String, Value>> = HashMap::new();
    let mut result = Vec::new();
    for entry in &catalog {
        for agent in &entry.assigned_agents {
            if !is_mcp_target(agent) {
                continue;
            }
            let servers = match cache.entry(agent.clone()) {
                std::collections::hash_map::Entry::Occupied(entry) => entry.into_mut(),
                std::collections::hash_map::Entry::Vacant(entry) => {
                    entry.insert(read_agent_mcp_servers(agent)?)
                }
            };
            let state = match servers.get(&entry.name) {
                None => "missing",
                Some(configured) => {
                    if entry_matches_config(agent, entry, configured) {
                        "installed"
                    } else {
                        "differs"
                    }
                }
            };
            result.push(McpAgentStatus {
                name: entry.name.clone(),
                agent: agent.clone(),
                state: state.into(),
            });
        }
    }
    Ok(result)
}

/// 扫描各配置源，收集可导入的现有 MCP 服务器（跳过共享记忆 MCP，
/// 按 name 去重）。
#[tauri::command]
pub async fn mcp_import_from_agents() -> Result<Vec<McpImportCandidate>, String> {
    let catalog_names: HashSet<String> = read_catalog()?
        .iter()
        .map(|entry| entry.name.clone())
        .collect();
    let mut seen: HashSet<String> = HashSet::new();
    let mut result = Vec::new();
    // codex_cli 与 codex_desktop 共用同一份 config.toml，扫一次即可。
    for agent in [
        "claude_cli",
        "claude_desktop",
        "codex_cli",
        "qoder",
        "workbuddy",
        "minimax",
        "kimi",
        "zcode",
    ] {
        for (name, value) in read_agent_mcp_servers(agent)? {
            if name == MEMORY_SERVER_NAME || !seen.insert(name.clone()) {
                continue;
            }
            let entry = entry_from_config(&name, agent, &value)?;
            let already = catalog_names.contains(&name);
            result.push(McpImportCandidate {
                agent: agent.to_string(),
                entry,
                already_in_catalog: already,
            });
        }
    }
    Ok(result)
}

/// 一键同步：把 assigned 给该 Agent 的所有条目按库内配置重写（漂移修复）。
#[tauri::command]
pub async fn mcp_sync_agent(agent: String) -> Result<(), String> {
    if !is_mcp_target(&agent) {
        return Err(format!("暂不支持 {agent} 的 MCP 配置"));
    }
    let catalog = read_catalog()?;
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
