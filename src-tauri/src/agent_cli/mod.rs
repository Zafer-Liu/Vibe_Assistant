//! Read-only client executable discovery used to validate MCP/Skill equipment targets.
use serde::Serialize;
use std::collections::{HashSet};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize)]
pub struct DetectedAgentCli {
    pub id: String,
    pub name: String,
    pub command: String,
    pub imported: bool,
}

pub(crate) struct AgentCliCandidate {
    pub(crate) id: &'static str,
    pub(crate) name: &'static str,
    pub(crate) command: &'static str,
}

// Product IDs, not config IDs. In particular, Qoder's desktop executable is not
// a candidate, and nothing from this list is ever executed during onboarding.
pub(crate) const AGENT_CLI_CANDIDATES: &[AgentCliCandidate] = &[
    AgentCliCandidate {
        id: "claude",
        name: "Claude Code",
        command: "claude",
    },
    AgentCliCandidate {
        id: "codex",
        name: "Codex CLI",
        command: "codex",
    },
    AgentCliCandidate {
        id: "gemini",
        name: "Gemini CLI",
        command: "gemini",
    },
    AgentCliCandidate {
        id: "qodercli",
        name: "Qoder CLI",
        command: "qodercli",
    },
    AgentCliCandidate {
        id: "kimi",
        name: "Kimi Code",
        command: "kimi",
    },
    AgentCliCandidate {
        id: "copilot",
        name: "GitHub Copilot",
        command: "copilot",
    },
];

fn unquote_cli_value(value: &str) -> &str {
    let value = value.trim();
    if value.len() >= 2
        && ((value.starts_with('"') && value.ends_with('"'))
            || (value.starts_with('\'') && value.ends_with('\'')))
    {
        &value[1..value.len() - 1]
    } else {
        value
    }
}

fn npmrc_cli_prefix(contents: &str) -> Option<String> {
    let mut prefix = None;
    for line in contents.lines() {
        // npm's INI comments can follow a value, but not inside quoted paths.
        let mut quote = None;
        let end = line
            .char_indices()
            .find_map(|(index, ch)| {
                if ch == '\'' || ch == '"' {
                    if quote == Some(ch) {
                        quote = None;
                    } else if quote.is_none() {
                        quote = Some(ch);
                    }
                }
                ((ch == '#' || ch == ';') && quote.is_none()).then_some(index)
            })
            .unwrap_or(line.len());
        if let Some((key, value)) = line[..end].split_once('=') {
            if key.trim().eq_ignore_ascii_case("prefix") {
                let value = unquote_cli_value(value);
                prefix = (!value.is_empty()).then(|| value.to_string());
            }
        }
    }
    prefix
}

fn npm_cli_prefix_path(value: &str, home: &Path) -> Result<PathBuf, String> {
    // Expand npm-style ${ENV} placeholders, never shell syntax or commands.
    let mut expanded = String::new();
    let mut rest = unquote_cli_value(value);
    while let Some(start) = rest.find("${") {
        expanded.push_str(&rest[..start]);
        let variable = &rest[start + 2..];
        let end = variable
            .find('}')
            .ok_or_else(|| "Invalid npm prefix: unclosed environment placeholder".to_string())?;
        let name = &variable[..end];
        let value = std::env::var(name)
            .map_err(|error| format!("Cannot expand npm prefix variable {name}: {error}"))?;
        expanded.push_str(&value);
        rest = &variable[end + 1..];
    }
    expanded.push_str(rest);
    if expanded.is_empty() {
        return Err("Invalid npm prefix: empty path".to_string());
    }
    if expanded == "~" {
        return Ok(home.to_path_buf());
    }
    if let Some(rest) = expanded.strip_prefix("~/").or_else(|| {
        if cfg!(windows) {
            expanded.strip_prefix("~\\")
        } else {
            None
        }
    }) {
        return Ok(home.join(rest));
    }
    let path = PathBuf::from(expanded);
    Ok(if path.is_absolute() {
        path
    } else {
        home.join(path)
    })
}

pub(crate) fn agent_cli_home() -> Result<PathBuf, String> {
    dirs_next::home_dir()
        .filter(|home| home.is_absolute())
        .ok_or_else(|| {
            "Cannot determine the user's home directory for agent CLI detection/import".to_string()
        })
}

#[cfg(windows)]
fn expand_windows_cli_path(value: &str) -> String {
    let mut result = String::new();
    let mut rest = unquote_cli_value(value);
    while let Some(start) = rest.find('%') {
        result.push_str(&rest[..start]);
        let tail = &rest[start + 1..];
        let Some(end) = tail.find('%') else {
            result.push_str(&rest[start..]);
            return result;
        };
        let name = &tail[..end];
        match std::env::var(name) {
            Ok(value) => result.push_str(&value),
            Err(_) => result.push_str(&rest[start..start + end + 2]),
        }
        rest = &tail[end + 1..];
    }
    result.push_str(rest);
    result
}

#[cfg(windows)]
fn current_windows_cli_dirs() -> Vec<PathBuf> {
    use winreg::enums::{HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE};
    use winreg::RegKey;
    let mut dirs = Vec::new();
    // A desktop app can have an old PATH after the CLI installer updated the
    // registry. Read the current values without running PowerShell or the CLI.
    for (hive, key) in [
        (HKEY_CURRENT_USER, "Environment"),
        (
            HKEY_LOCAL_MACHINE,
            r"SYSTEM\CurrentControlSet\Control\Session Manager\Environment",
        ),
    ] {
        if let Ok(environment) = RegKey::predef(hive).open_subkey(key) {
            if let Ok(path) = environment.get_value::<String, _>("Path") {
                dirs.extend(
                    path.split(';')
                        .filter(|entry| !entry.trim().is_empty())
                        .map(|entry| PathBuf::from(expand_windows_cli_path(entry)))
                        .filter(|path| path.is_absolute()),
                );
            }
        }
    }
    dirs
}

fn agent_cli_search_dirs(home: &Path) -> Result<Vec<PathBuf>, String> {
    let mut dirs = Vec::new();
    #[cfg(windows)]
    dirs.extend(current_windows_cli_dirs());
    if let Some(path) = std::env::var_os("PATH") {
        // Ignore empty PATH components rather than implicitly trusting the cwd.
        for dir in std::env::split_paths(&path).filter(|dir| !dir.as_os_str().is_empty()) {
            dirs.push(if dir.is_absolute() {
                dir
            } else {
                std::env::current_dir()
                    .map_err(|error| format!("Cannot resolve a relative PATH entry: {error}"))?
                    .join(dir)
            });
        }
    }
    let mut prefixes = Vec::new();
    for key in ["npm_config_prefix", "NPM_CONFIG_PREFIX"] {
        if let Some(value) = std::env::var_os(key) {
            let value = value
                .into_string()
                .map_err(|_| format!("Cannot read {key}: path is not valid Unicode"))?;
            if !value.trim().is_empty() {
                prefixes.push(npm_cli_prefix_path(&value, home)?);
            }
        }
    }
    let npmrc = home.join(".npmrc");
    match std::fs::read_to_string(&npmrc) {
        Ok(contents) => {
            if let Some(prefix) = npmrc_cli_prefix(&contents) {
                prefixes.push(npm_cli_prefix_path(&prefix, home)?);
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(format!(
                "Cannot read {} for CLI detection: {error}",
                npmrc.display()
            ))
        }
    }
    for prefix in prefixes {
        if cfg!(windows) {
            dirs.push(prefix.clone());
        }
        dirs.push(prefix.join("bin"));
    }
    for variable in [
        "PNPM_HOME",
        "NVM_SYMLINK",
        "VOLTA_HOME",
        "BUN_INSTALL",
        "NVM_HOME",
        "FNM_DIR",
    ] {
        if let Some(root) = std::env::var_os(variable)
            .map(PathBuf::from)
            .filter(|root| root.is_absolute())
        {
            dirs.push(root.clone());
            dirs.push(root.join("bin"));
            if matches!(variable, "NVM_HOME" | "FNM_DIR") {
                let versions = if variable == "FNM_DIR" {
                    root.join("node-versions")
                } else {
                    root
                };
                let mut installed: Vec<_> = std::fs::read_dir(versions)
                    .into_iter()
                    .flatten()
                    .filter_map(Result::ok)
                    .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
                    .collect();
                installed.sort_by_key(|entry| {
                    std::cmp::Reverse(entry.metadata().and_then(|meta| meta.modified()).ok())
                });
                for entry in installed.into_iter().take(64) {
                    dirs.push(entry.path());
                    dirs.push(entry.path().join("installation"));
                    dirs.push(entry.path().join("bin"));
                }
            }
        }
    }
    for relative in [
        ".local/bin",
        ".npm-global/bin",
        ".npm/bin",
        ".npm-packages/bin",
        ".volta/bin",
        ".bun/bin",
    ] {
        dirs.push(home.join(relative));
    }
    #[cfg(windows)]
    {
        if let Some(appdata) = std::env::var_os("APPDATA") {
            let appdata = PathBuf::from(appdata);
            if appdata.is_absolute() {
                dirs.push(appdata.join("npm"));
            }
        }
        dirs.push(home.join("AppData/Roaming/npm"));
        dirs.push(home.join(".npm-global"));
        dirs.push(home.join(".npm-packages"));
        dirs.push(home.join("scoop/shims"));
        if let Some(localappdata) = std::env::var_os("LOCALAPPDATA") {
            let localappdata = PathBuf::from(localappdata);
            if localappdata.is_absolute() {
                dirs.push(localappdata.join("Microsoft/WinGet/Links"));
                dirs.push(localappdata.join("Programs/claude"));
                // Codex's desktop distribution also contains a standalone CLI.
                let versions = localappdata.join("OpenAI/Codex/bin");
                let mut installed: Vec<_> = std::fs::read_dir(versions)
                    .into_iter()
                    .flatten()
                    .filter_map(Result::ok)
                    .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
                    .collect();
                installed.sort_by_key(|entry| {
                    std::cmp::Reverse(entry.metadata().and_then(|meta| meta.modified()).ok())
                });
                dirs.extend(installed.into_iter().take(64).map(|entry| entry.path()));
            }
        }
    }
    #[cfg(unix)]
    for dir in [
        "/usr/local/bin",
        "/opt/homebrew/bin",
        "/usr/bin",
        "/bin",
        "/opt/local/bin",
    ] {
        dirs.push(PathBuf::from(dir));
    }
    if let Some(dir) = dirs.iter().find(|dir| !dir.is_absolute()) {
        return Err(format!(
            "Cannot resolve CLI search directory to an absolute path: {}",
            dir.display()
        ));
    }
    let mut seen = HashSet::new();
    dirs.retain(|dir| {
        seen.insert(if cfg!(windows) {
            dir.to_string_lossy().to_lowercase()
        } else {
            dir.to_string_lossy().into_owned()
        })
    });
    Ok(dirs)
}

pub(crate) fn scan_agent_clis(home: &Path) -> Result<Vec<DetectedAgentCli>, String> {
    let dirs = agent_cli_search_dirs(home)?;
    let extensions: &[&str] = if cfg!(windows) {
        &[".exe", ".cmd", ".bat"]
    } else {
        &[""]
    };
    let mut detected = Vec::new();
    for candidate in AGENT_CLI_CANDIDATES {
        'search: for dir in &dirs {
            for extension in extensions {
                let path = dir.join(format!("{}{extension}", candidate.command));
                let metadata = match std::fs::metadata(&path) {
                    Ok(metadata) => metadata,
                    Err(error)
                        if matches!(
                            error.kind(),
                            std::io::ErrorKind::NotFound
                                | std::io::ErrorKind::PermissionDenied
                                | std::io::ErrorKind::NotADirectory
                        ) =>
                    {
                        continue
                    }
                    Err(error) => {
                        return Err(format!(
                            "Cannot inspect CLI candidate {}: {error}",
                            path.display()
                        ))
                    }
                };
                if !metadata.is_file() {
                    continue;
                }
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    if metadata.permissions().mode() & 0o111 == 0 {
                        continue;
                    }
                }
                let command = path
                    .to_str()
                    .ok_or_else(|| format!("CLI path is not valid Unicode: {}", path.display()))?
                    .to_string();
                detected.push(DetectedAgentCli {
                    id: candidate.id.to_string(),
                    name: candidate.name.to_string(),
                    command,
                    imported: false,
                });
                break 'search;
            }
        }
    }
    Ok(detected)
}

/// Check a client installation without importing, starting or managing it.
pub(crate) fn resolve_installed_agent_cli(name: &str) -> Option<PathBuf> {
    let home = agent_cli_home().ok()?;
    scan_agent_clis(&home)
        .ok()?
        .into_iter()
        .find(|cli| cli.id == name)
        .map(|cli| PathBuf::from(cli.command))
}
