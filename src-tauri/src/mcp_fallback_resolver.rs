//! Read-only resolution of an observed MCP assignment for the independent launcher.
//! The original command and environment stay in the central catalog, never in
//! the Agent's arguments or the assignment manifest.

use rusqlite::{Connection, OpenFlags, OptionalExtension};
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use uuid::Uuid;

#[derive(Deserialize)]
struct Manifest {
    id: String,
    name: String,
    agent: String,
    config_path: PathBuf,
    fingerprint: String,
    proxy_command: String,
    #[serde(default)]
    relay_command: Option<String>,
    phase: String,
}

#[derive(Deserialize)]
struct CatalogEntry {
    name: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    transport: String,
    #[serde(default)]
    command: String,
    #[serde(default)]
    args: Vec<String>,
    #[serde(default)]
    env: BTreeMap<String, String>,
    #[serde(default)]
    assigned_agents: Vec<String>,
}

#[allow(dead_code)] // relay_command/assignment_id are consumed by the launcher crate only
pub struct FallbackLaunch {
    pub command: String,
    pub args: Vec<String>,
    pub env: BTreeMap<String, String>,
    pub relay_command: String,
    pub assignment_id: String,
}

fn physical_target(agent: &str) -> &str {
    if matches!(agent, "codex_cli" | "codex_desktop") { "codex_cli" } else { agent }
}

fn canonical(agent: &str, entry: &CatalogEntry) -> Value {
    let mut value = json!({ "command": entry.command, "args": entry.args });
    if !entry.env.is_empty() { value["env"] = json!(entry.env); }
    if agent == "minimax" {
        value["type"] = json!("stdio");
        value["enabled"] = json!(true);
    }
    if !entry.description.is_empty() && agent != "zcode" && !agent.starts_with("codex") {
        value["description"] = json!(entry.description);
    }
    value
}

fn installed_node(manifest: &Manifest) -> Result<Value, String> {
    if manifest.agent.starts_with("codex") {
        let text = std::fs::read_to_string(&manifest.config_path).map_err(|_| "Agent config unavailable")?;
        let config: toml::Table = text.parse().map_err(|_| "Agent config invalid")?;
        let node = config.get("mcp_servers").and_then(toml::Value::as_table)
            .and_then(|servers| servers.get(&manifest.name)).ok_or("Agent assignment missing")?;
        serde_json::to_value(node).map_err(|_| "Agent config invalid".into())
    } else {
        let bytes = std::fs::read(&manifest.config_path).map_err(|_| "Agent config unavailable")?;
        let config: Value = serde_json::from_slice(&bytes).map_err(|_| "Agent config invalid")?;
        let servers = if manifest.agent == "zcode" {
            config.get("mcp").and_then(|mcp| mcp.get("servers"))
        } else {
            config.get("mcpServers")
        };
        servers.and_then(|servers| servers.get(&manifest.name)).cloned().ok_or("Agent assignment missing".into())
    }
}

fn config_root() -> Result<PathBuf, String> {
    dirs_next::config_dir().map(|root| root.join("agent-manager"))
        .ok_or_else(|| "Observation directory unavailable".into())
}

fn qoder_has_transcripts(home: &Path) -> bool {
    let mut pending = vec![home.join("projects")];
    let mut visited = 0;
    while let Some(directory) = pending.pop() {
        if visited >= 2_000 { break; }
        let Ok(entries) = std::fs::read_dir(directory) else { continue; };
        for entry in entries.flatten() {
            visited += 1;
            let path = entry.path();
            if path.is_dir() { pending.push(path); }
            else if path.extension().and_then(|ext| ext.to_str()) == Some("jsonl") { return true; }
        }
    }
    false
}

fn default_config_path(agent: &str) -> Option<PathBuf> {
    let home = dirs_next::home_dir();
    match agent {
        "minimax" => Some(home?.join(".minimax").join("mcp.json")),
        "zcode" => Some(home?.join(".zcode").join("cli").join("config.json")),
        "qoder" => {
            let home = home?;
            let candidates = [home.join(".qoder"), home.join(".qoder-cn")];
            let score = |directory: &Path| {
                let modified = std::fs::metadata(directory.join("settings.json"))
                    .and_then(|meta| meta.modified()).ok()
                    .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|elapsed| elapsed.as_secs()).unwrap_or(0);
                (qoder_has_transcripts(directory), modified)
            };
            let chosen = if score(&candidates[1]) > score(&candidates[0]) { &candidates[1] } else { &candidates[0] };
            Some(chosen.join("mcp.json"))
        }
        "kimi" => {
            let desktop = std::env::var_os("APPDATA").map(PathBuf::from)
                .map(|path| path.join("kimi-desktop").join("daimon-storage.json"))
                .and_then(|path| std::fs::read(path).ok())
                .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
                .and_then(|value| value.get("shareDir").and_then(Value::as_str).map(PathBuf::from))
                .map(|path| path.join("daimon").join("runtime").join("kimi-code").join("home"));
            let mut candidates = Vec::new();
            if let Some(desktop) = desktop { candidates.push(desktop); }
            if let Some(home) = home { candidates.push(home.join(".kimi-code")); }
            candidates.iter().find(|path| path.is_dir()).cloned()
                .or_else(|| candidates.into_iter().next()).map(|path| path.join("mcp.json"))
        }
        _ => None,
    }
}

fn verified_path(manifest: &Manifest) -> Result<(), String> {
    let expected = match manifest.agent.as_str() {
        "codex_cli" => std::env::var("CODEX_HOME").ok().filter(|home| !home.trim().is_empty())
            .map(|home| PathBuf::from(home).join("config.toml"))
            .or_else(|| dirs_next::home_dir().map(|home| home.join(".codex").join("config.toml"))),
        "claude_cli" => dirs_next::home_dir().map(|home| home.join(".claude.json")),
        "claude_desktop" => {
            let root = config_root()?;
            let db = Connection::open_with_flags(root.join("telemetry.sqlite3"), OpenFlags::SQLITE_OPEN_READ_ONLY)
                .map_err(|_| "MCP catalog unavailable")?;
            let setting: Option<String> = db.query_row(
                "SELECT value_json FROM app_settings WHERE setting_key = 'claude_desktop_mcp_config_path'",
                [], |row| row.get(0),
            ).optional().map_err(|_| "Agent config selection unavailable")?;
            let selected = setting.map(|text| serde_json::from_str::<Option<String>>(&text)
                .map_err(|_| "Agent config selection invalid")).transpose()?.flatten();
            if let Some(selected) = selected { Some(PathBuf::from(selected)) }
            else {
                let mut candidates = Vec::new();
                #[cfg(windows)]
                let config_dir = std::env::var_os("APPDATA").filter(|value| !value.is_empty())
                    .map(PathBuf::from).or_else(dirs_next::config_dir);
                #[cfg(not(windows))]
                let config_dir = dirs_next::config_dir();
                if let Some(dir) = config_dir { candidates.push(dir.join("Claude").join("claude_desktop_config.json")); }
                #[cfg(windows)]
                {
                    let local_dir = std::env::var_os("LOCALAPPDATA").filter(|value| !value.is_empty())
                        .map(PathBuf::from).or_else(dirs_next::data_local_dir);
                    if let Some(dir) = local_dir {
                        let path = dir.join("Claude-3p").join("claude_desktop_config.json");
                        if !candidates.contains(&path) { candidates.push(path); }
                    }
                }
                let existing: Vec<_> = candidates.iter().filter(|path| path.is_file()).collect();
                if existing.len() > 1 { return Err("Agent config selection changed".into()); }
                existing.first().map(|path| (*path).clone()).or_else(|| candidates.into_iter().next())
            }
        }
        "workbuddy" => dirs_next::home_dir().map(|home| home.join(".workbuddy").join(".mcp.json")),
        "qoder" | "minimax" | "kimi" | "zcode" => {
            // Overrides share the same per-user file as the Agent source UI.
            let overrides = config_root()?.join("agent_source_paths.json");
            let settings: Value = std::fs::read(&overrides).ok()
                .and_then(|bytes| serde_json::from_slice(&bytes).ok()).unwrap_or_else(|| json!({}));
            if let Some(home) = settings.get(&manifest.agent).and_then(|entry| entry.get("config_home")).and_then(Value::as_str).filter(|home| !home.trim().is_empty()) {
                let mut path = PathBuf::from(home);
                match manifest.agent.as_str() {
                    "zcode" => path.push("cli/config.json"),
                    _ => path.push("mcp.json"),
                }
                Some(path)
            } else { default_config_path(&manifest.agent) }
        }
        _ => return Err("Unsupported MCP agent".into()),
    };
    if expected.is_none_or(|path| path != manifest.config_path) {
        return Err("Agent config path changed".into());
    }
    Ok(())
}

// Each crate embedding this shared resolver uses only one of the two entry
// points; the other stays referenced by the sibling crate.
#[allow(dead_code)]
pub fn resolve_fallback(assignment_id: &str) -> Result<FallbackLaunch, String> {
    resolve(assignment_id, true)
}

#[allow(dead_code)]
pub fn resolve_fallback_for_relay(assignment_id: &str) -> Result<FallbackLaunch, String> {
    resolve(assignment_id, false)
}

fn resolve(assignment_id: &str, launcher_process: bool) -> Result<FallbackLaunch, String> {
    let id = Uuid::parse_str(assignment_id).map_err(|_| "Invalid MCP assignment")?.to_string();
    let root = config_root()?;
    let observation_dir = root.join("mcp-observation");
    if std::fs::symlink_metadata(&observation_dir).map_err(|_| "Assignment directory unavailable")?.file_type().is_symlink() {
        return Err("Assignment directory invalid".into());
    }
    let path = observation_dir.join(format!("{id}.json"));
    if std::fs::symlink_metadata(&path).map_err(|_| "Assignment unavailable")?.file_type().is_symlink() {
        return Err("Assignment path invalid".into());
    }
    let manifest: Manifest = serde_json::from_slice(&std::fs::read(&path).map_err(|_| "Assignment unavailable")?)
        .map_err(|_| "Assignment invalid")?;
    if manifest.id != id || manifest.phase != "active" || manifest.name.is_empty()
        || !matches!(manifest.agent.as_str(), "codex_cli" | "claude_cli" | "claude_desktop" | "qoder" | "workbuddy" | "minimax" | "kimi" | "zcode")
        || manifest.relay_command.as_ref().is_none_or(|command| command.is_empty())
        || !manifest.config_path.is_absolute()
        || !Path::new(&manifest.proxy_command).is_absolute() {
        return Err("Assignment inactive or invalid".into());
    }
    if launcher_process {
        let executable = std::env::current_exe().and_then(std::fs::canonicalize)
            .map_err(|_| "Launcher path unavailable")?;
        let launcher = std::fs::canonicalize(&manifest.proxy_command).map_err(|_| "Launcher path unavailable")?;
        if executable != launcher { return Err("Assignment launcher changed".into()); }
    }
    verified_path(&manifest)?;
    let db = root.join("telemetry.sqlite3");
    if !Path::new(&db).is_file() { return Err("MCP catalog unavailable".into()); }
    let conn = Connection::open_with_flags(db, OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX)
        .map_err(|_| "MCP catalog unavailable")?;
    let serialized: String = conn.query_row(
        "SELECT value_json FROM app_settings WHERE setting_key = 'mcp_catalog'", [], |row| row.get(0),
    ).optional().map_err(|_| "MCP catalog unavailable")?.ok_or("MCP catalog missing")?;
    let migration: Option<String> = conn.query_row(
        "SELECT value_json FROM app_settings WHERE setting_key = 'mcp_catalog_legacy_migration_v1'",
        [], |row| row.get(0),
    ).optional().map_err(|_| "MCP catalog migration unavailable")?;
    let migration = migration.ok_or("MCP catalog migration missing")?;
    if serde_json::from_str::<Value>(&migration).ok().and_then(|value| value.get("completed").and_then(Value::as_bool)) != Some(true) {
        return Err("MCP catalog migration incomplete".into());
    }
    let entries: Vec<CatalogEntry> = serde_json::from_str(&serialized).map_err(|_| "MCP catalog invalid")?;
    if entries.iter().filter(|entry| entry.name == manifest.name).count() != 1 {
        return Err("Original MCP entry missing or ambiguous".into());
    }
    let entry = entries.into_iter().find(|entry| entry.name == manifest.name)
        .ok_or("Original MCP entry missing")?;
    if !(entry.transport.is_empty() || entry.transport == "stdio")
        || !entry.assigned_agents.iter().any(|agent| physical_target(agent) == manifest.agent)
        || entry.command.is_empty() {
        return Err("Original MCP assignment changed".into());
    }
    let fingerprint = format!("{:x}", Sha256::digest(
        serde_json::to_vec(&canonical(&manifest.agent, &entry)).map_err(|_| "MCP entry invalid")?
    ));
    if fingerprint != manifest.fingerprint { return Err("Original MCP entry changed".into()); }
    let mut expected = json!({
        "command": manifest.proxy_command,
        "args": ["--mcp-relay", manifest.id]
    });
    if manifest.agent == "minimax" {
        expected["type"] = json!("stdio");
        expected["enabled"] = json!(true);
    }
    if installed_node(&manifest)? != expected { return Err("Agent assignment changed".into()); }
    Ok(FallbackLaunch {
        command: entry.command,
        args: entry.args,
        env: entry.env,
        relay_command: manifest.relay_command.unwrap_or_default(),
        assignment_id: id,
    })
}
