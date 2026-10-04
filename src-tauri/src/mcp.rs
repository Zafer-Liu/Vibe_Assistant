use crate::llm::LlmProvider;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::Path;

fn truncate_chars(value: &str, max_chars: usize) -> String {
    value.chars().take(max_chars).collect()
}

fn parse_llm_json(content: &str) -> Result<Value, String> {
    let clean = content
        .trim()
        .trim_start_matches("```json")
        .trim_start_matches("```")
        .trim_end_matches("```")
        .trim();

    if let Ok(value) = serde_json::from_str(clean) {
        return Ok(value);
    }

    let start = clean
        .find('{')
        .ok_or_else(|| "LLM returned invalid JSON - try again or fill manually".to_string())?;
    let end = clean
        .rfind('}')
        .ok_or_else(|| "LLM returned invalid JSON - try again or fill manually".to_string())?;
    serde_json::from_str(&clean[start..=end])
        .map_err(|_| "LLM returned invalid JSON - try again or fill manually".to_string())
}

fn llm_error_message(body: &str) -> String {
    serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|value| value["error"]["message"].as_str().map(str::to_string))
        .filter(|message| !message.is_empty())
        .unwrap_or_else(|| body.trim().to_string())
}

// ── Data model ───────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpServer {
    pub name: String,
    pub command: String,
    pub args: Vec<String>,
    pub env: HashMap<String, String>,
    // Remote transport fields
    #[serde(default)]
    pub transport: String, // "stdio" | "sse" | "http"
    #[serde(default)]
    pub url: String,
    #[serde(default)]
    pub headers: HashMap<String, String>,
    #[serde(default)]
    pub description: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpScanResult {
    pub transport: String,
    pub name: String,
    pub command: String,
    pub args: Vec<String>,
    pub env: HashMap<String, String>,
    pub url: String,
    pub headers: HashMap<String, String>,
    pub description: String,
    pub warnings: Vec<String>,
    pub confidence: u8,
}

// ── Read-only legacy path ────────────────────────────────────────────────────

/// 仅供一次性迁移及兼容路径查询；与外部 MCP 读写共用路径选择。
pub(crate) fn legacy_config_path() -> Result<std::path::PathBuf, String> {
    crate::memory_mcp::desktop_config_path("claude_desktop")?
        .ok_or_else(|| "无法定位 Claude Desktop 配置文件".into())
}

// ── Central catalog compatibility commands ────────────────────────────────────

/// 只读遗留路径查询，不再代表当前可编辑的 MCP 库配置。
#[tauri::command]
pub fn get_mcp_config_path() -> Result<String, String> {
    Ok(legacy_config_path()?.to_string_lossy().to_string())
}

#[tauri::command]
pub fn list_mcp_servers() -> Result<Vec<McpServer>, String> {
    crate::mcp_registry::read_runtime_servers()
}

#[tauri::command]
pub fn save_mcp_server(server: McpServer) -> Result<(), String> {
    crate::mcp_registry::upsert_catalog_entry(compatibility_entry(server)).map(|_| ())
}

fn compatibility_entry(server: McpServer) -> crate::mcp_registry::McpCatalogEntry {
    crate::mcp_registry::McpCatalogEntry {
        name: server.name,
        description: server.description,
        transport: server.transport,
        command: server.command,
        args: server.args,
        env: server.env.into_iter().collect(),
        url: server.url,
        headers: server.headers.into_iter().collect(),
        ..Default::default()
    }
}

#[tauri::command]
pub fn delete_mcp_server(name: String) -> Result<(), String> {
    crate::mcp_registry::delete_catalog_entry(&name)
}

// ── Local directory scan ─────────────────────────────────────────────────────

#[tauri::command]
pub async fn scan_mcp_local(
    dir: String,
    provider: Option<LlmProvider>,
) -> Result<McpScanResult, String> {
    let base = Path::new(&dir);
    if !base.exists() || !base.is_dir() {
        return Err("Directory not found".to_string());
    }

    let mut warnings: Vec<String> = vec![];

    // Find package.json
    let pkg_path = find_package_json(base);
    if pkg_path.is_none() {
        return Err("No package.json found — not an npm MCP package".to_string());
    }
    let pkg_path = pkg_path.unwrap();
    let pkg_dir = pkg_path.parent().unwrap();

    let pkg_text = std::fs::read_to_string(&pkg_path).map_err(|e| e.to_string())?;
    let pkg: Value = serde_json::from_str(&pkg_text).map_err(|e| e.to_string())?;

    if !is_mcp_package(&pkg) {
        return Err("Not an MCP package — no MCP dependency (@modelcontextprotocol/sdk, @scope/mcp, *-mcp …) found".to_string());
    }

    // 优先方案：如果根 package.json 依赖了一个【已发布的 npm MCP 包】（如 @playwright/mcp），
    // 直接用标准的 `npx -y <包名>` 启动，而不是去 node_modules 里翻 cli.js 用深路径调用。
    // 这样配置更规范、可移植，且不依赖 node_modules 是否存在。
    let published_pkg =
        find_mcp_dep(&pkg).filter(|name| name != "@modelcontextprotocol/sdk" && name != "fastmcp");

    // Read README（从根目录或所选目录）
    let readme = ["README.md", "readme.md", "README.txt", "README"]
        .iter()
        .find_map(|name| std::fs::read_to_string(pkg_dir.join(name)).ok())
        .unwrap_or_default();

    if let Some(npm_pkg) = published_pkg {
        // 用 npx 启动已发布包
        let label = title_case(&mcp_pkg_label(&npm_pkg));

        let (description, env, llm_warnings) = if let Some(p) = provider {
            match llm_enrich(&p, &pkg, &readme).await {
                Ok((d, e, w)) => (d, e, w),
                Err(e) => {
                    warnings.push(format!("LLM enrichment failed (skipped): {}", e));
                    (
                        pkg["description"].as_str().unwrap_or("").to_string(),
                        HashMap::new(),
                        vec![],
                    )
                }
            }
        } else {
            (
                pkg["description"].as_str().unwrap_or("").to_string(),
                HashMap::new(),
                vec![],
            )
        };
        warnings.extend(llm_warnings);

        return Ok(McpScanResult {
            transport: "stdio".to_string(),
            name: label.to_lowercase().replace(' ', "_"),
            command: "npx".to_string(),
            args: vec!["-y".to_string(), npm_pkg.clone()],
            env,
            url: String::new(),
            headers: HashMap::new(),
            description,
            warnings,
            confidence: 90,
        });
    }

    // 回退方案：本地源码 MCP 项目，推断入口文件用 node 启动

    let (entry, confidence) = infer_node_entry(pkg_dir, &pkg);
    if entry.is_empty() {
        warnings.push(
            "Entry file not found — dist/ may not be built yet. Run 'npm run build' first."
                .to_string(),
        );
    }

    // Derive name and label
    let pkg_name = pkg["name"]
        .as_str()
        .unwrap_or(base.file_name().and_then(|n| n.to_str()).unwrap_or("mcp"));
    let bare = pkg_name.split('/').last().unwrap_or(pkg_name);
    let label = bare
        .replace("-mcp", "")
        .replace("mcp-", "")
        .replace('-', " ");
    let label = title_case(&label);

    // LLM enrichment
    let (description, env, llm_warnings) = if let Some(p) = provider {
        match llm_enrich(&p, &pkg, &readme).await {
            Ok((d, e, w)) => (d, e, w),
            Err(e) => {
                warnings.push(format!("LLM enrichment failed (skipped): {}", e));
                (
                    pkg["description"].as_str().unwrap_or("").to_string(),
                    HashMap::new(),
                    vec![],
                )
            }
        }
    } else {
        (
            pkg["description"].as_str().unwrap_or("").to_string(),
            HashMap::new(),
            vec![],
        )
    };
    warnings.extend(llm_warnings);

    if confidence < 70 {
        warnings.push(format!(
            "Entry point confidence low ({}%) — verify the path is correct.",
            confidence
        ));
    }

    let args = if entry.is_empty() {
        vec![]
    } else {
        vec![entry.clone()]
    };

    Ok(McpScanResult {
        transport: "stdio".to_string(),
        name: label.to_lowercase().replace(' ', "_"),
        command: if entry.is_empty() {
            String::new()
        } else {
            "node".to_string()
        },
        args,
        env,
        url: String::new(),
        headers: HashMap::new(),
        description,
        warnings,
        confidence,
    })
}

fn find_package_json(base: &Path) -> Option<std::path::PathBuf> {
    // Direct package.json
    let direct = base.join("package.json");
    if direct.exists() {
        if let Ok(text) = std::fs::read_to_string(&direct) {
            if let Ok(pkg) = serde_json::from_str::<Value>(&text) {
                if is_mcp_package(&pkg) {
                    return Some(direct);
                }
            }
        }
    }
    // Look inside node_modules
    let nm = base.join("node_modules");
    if nm.is_dir() {
        if let Ok(entries) = std::fs::read_dir(&nm) {
            for entry in entries.flatten() {
                let path = entry.path();
                let pkg_json = path.join("package.json");
                if pkg_json.exists() {
                    if let Ok(text) = std::fs::read_to_string(&pkg_json) {
                        if let Ok(pkg) = serde_json::from_str::<Value>(&text) {
                            if is_mcp_package(&pkg) {
                                return Some(pkg_json);
                            }
                        }
                    }
                }
            }
        }
    }
    None
}

/// 判断一个依赖 key 是否像 MCP 相关包。
/// 命中：精确 sdk 标记，或 key 含 "mcp"（如 @playwright/mcp、xxx-mcp、mcp-xxx、fastmcp）。
fn dep_key_is_mcp(key: &str) -> bool {
    let k = key.to_lowercase();
    if k == "@modelcontextprotocol/sdk" || k == "fastmcp" {
        return true;
    }
    // @scope/mcp、@scope/mcp-foo、foo-mcp、mcp-foo、含 mcp 段
    k == "mcp"
        || k.ends_with("/mcp")
        || k.contains("/mcp-")
        || k.ends_with("-mcp")
        || k.starts_with("mcp-")
}

/// 在 dependencies / devDependencies 中找到第一个 MCP 相关依赖的包名。

fn find_mcp_dep(pkg: &Value) -> Option<String> {
    for field in ["dependencies", "devDependencies"] {
        if let Some(obj) = pkg.get(field).and_then(|v| v.as_object()) {
            for key in obj.keys() {
                if dep_key_is_mcp(key) {
                    return Some(key.clone());
                }
            }
        }
    }
    None
}

/// 从已发布的 MCP 包名推断一个易读的标签。
/// @playwright/mcp        → playwright   （包名就是 mcp，用 scope）
/// @scope/mcp-foo         → foo
/// @scope/foo-mcp         → foo
/// brave-search-mcp       → brave search
/// mcp-server-filesystem  → server filesystem
fn mcp_pkg_label(npm_pkg: &str) -> String {
    let stripped = npm_pkg.strip_prefix('@').unwrap_or(npm_pkg);
    let parts: Vec<&str> = stripped.split('/').collect();
    let (scope, last) = if parts.len() >= 2 {
        (parts[0], parts[parts.len() - 1])
    } else {
        ("", parts[0])
    };

    // 包名本身就是 "mcp"（如 @playwright/mcp）→ 用 scope 名
    if last.eq_ignore_ascii_case("mcp") {
        if !scope.is_empty() {
            return scope.replace('-', " ");
        }
        return "mcp".to_string();
    }

    // 去掉 mcp 前后缀
    let cleaned = last
        .replace("-mcp", "")
        .replace("mcp-", "")
        .replace('_', " ")
        .replace('-', " ")
        .trim()
        .to_string();

    if cleaned.is_empty() && !scope.is_empty() {
        return scope.replace('-', " ");
    }
    if cleaned.is_empty() {
        return "mcp".to_string();
    }
    cleaned
}

fn is_mcp_package(pkg: &Value) -> bool {
    if find_mcp_dep(pkg).is_some() {
        return true;
    }
    let name = pkg["name"].as_str().unwrap_or("").to_lowercase();
    let desc = pkg["description"].as_str().unwrap_or("").to_lowercase();
    name.contains("mcp") || desc.contains("mcp")
}

fn infer_node_entry(pkg_dir: &Path, pkg: &Value) -> (String, u8) {
    // Priority 1: bin field
    if let Some(bin) = pkg.get("bin") {
        if let Some(s) = bin.as_str() {
            let p = pkg_dir.join(s);
            if p.exists() {
                return (p.to_string_lossy().to_string(), 95);
            }
        } else if let Some(obj) = bin.as_object() {
            for (_, v) in obj {
                if let Some(s) = v.as_str() {
                    let p = pkg_dir.join(s);
                    if p.exists() {
                        return (p.to_string_lossy().to_string(), 90);
                    }
                }
            }
        }
    }
    // Priority 2: main field
    if let Some(main) = pkg["main"].as_str().filter(|s| !s.is_empty()) {
        let p = pkg_dir.join(main);
        if p.exists() {
            return (p.to_string_lossy().to_string(), 75);
        }
    }
    // Priority 3: common fallbacks
    for fallback in &[
        "dist/index.js",
        "index.js",
        "build/index.js",
        "out/index.js",
    ] {
        let p = pkg_dir.join(fallback);
        if p.exists() {
            return (p.to_string_lossy().to_string(), 50);
        }
    }
    (String::new(), 0)
}

async fn llm_enrich(
    provider: &LlmProvider,
    pkg: &Value,
    readme: &str,
) -> Result<(String, HashMap<String, String>, Vec<String>), String> {
    let system = r#"You are helping configure an MCP (Model Context Protocol) server.
Given a package summary and README excerpt, extract:
1. A one-line description of what this MCP server does (in English, ≤80 chars)
2. Environment variables the server needs (name → placeholder value)
Reply ONLY with JSON: {"description": "...", "env": {"VAR": "YOUR_VALUE_HERE"}}"#;

    let summary = json!({
        "name": pkg["name"],
        "description": pkg["description"],
        "scripts": pkg["scripts"],
    });
    let user_content = format!(
        "package.json:\n{}\n\nREADME (first 500 chars):\n{}",
        serde_json::to_string_pretty(&summary).unwrap_or_default(),
        truncate_chars(readme, 500)
    );

    let body = json!({
        "model": provider.model,
        "messages": [
            {"role": "system", "content": system},
            {"role": "user", "content": user_content}
        ],
        "max_tokens": 256,
        "temperature": 0
    });

    let client = reqwest::Client::new();
    let url = format!(
        "{}/chat/completions",
        provider.base_url.trim_end_matches('/')
    );
    let resp = client
        .post(&url)
        .header("Authorization", format!("Bearer {}", provider.api_key))
        .json(&body)
        .timeout(std::time::Duration::from_secs(20))
        .send()
        .await
        .map_err(|e| e.to_string())?;

    let status = resp.status();

    let text = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        return Err(format!(
            "LLM error {}: {}",
            status,
            llm_error_message(&text)
        ));
    }
    let val: Value = serde_json::from_str(&text).map_err(|e| e.to_string())?;
    let content = val["choices"][0]["message"]["content"]
        .as_str()
        .unwrap_or("");
    // Strip markdown fences
    let clean = content
        .trim()
        .trim_start_matches("```json")
        .trim_start_matches("```")
        .trim_end_matches("```")
        .trim();
    let result: Value = serde_json::from_str(clean).unwrap_or(json!({}));

    let desc = result["description"].as_str().unwrap_or("").to_string();
    let env: HashMap<String, String> = result["env"]
        .as_object()
        .map(|o| {
            o.iter()
                .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
                .collect()
        })
        .unwrap_or_default();

    Ok((desc, env, vec![]))
}

// ── Smart text parse ─────────────────────────────────────────────────────────

#[tauri::command]
pub async fn parse_mcp_text(text: String, provider: LlmProvider) -> Result<McpScanResult, String> {
    if text.trim().is_empty() {
        return Err("Text is empty".to_string());
    }

    // Quick heuristic: if it looks like a plain URL, treat as SSE
    let trimmed = text.trim();
    if trimmed.starts_with("http://") || trimmed.starts_with("https://") {
        if !trimmed.contains('\n') && !trimmed.contains(' ') {
            return Ok(McpScanResult {
                transport: "sse".to_string(),
                name: "remote_mcp".to_string(),
                command: String::new(),
                args: vec![],
                env: HashMap::new(),
                url: trimmed.to_string(),
                headers: HashMap::new(),
                description: "Remote MCP server".to_string(),
                warnings: vec![],
                confidence: 80,
            });
        }
    }

    let system = r#"You are a configuration parser for MCP (Model Context Protocol) servers.
Parse the given text (npm command, JSON snippet, README excerpt, etc.) and return ONLY JSON:
{
  "transport": "stdio" or "sse",
  "name": "<server_id snake_case>",
  "description": "<one-line description>",
  "command": "<npx|uvx|node|python|python3|uv|deno — stdio only>",
  "args": ["arg1", "arg2"],
  "env": {"KEY": "PLACEHOLDER_VALUE"},
  "url": "<SSE URL or empty>",
  "headers": {}
}
Rules:
- For unknown env values use placeholders like YOUR_API_KEY_HERE
- Never include shell metacharacters in args: ; & | ` $ < > ( )
- Return valid JSON only, no markdown"#;

    let body = json!({
        "model": provider.model,
        "messages": [
            {"role": "system", "content": system},
            {"role": "user", "content": format!("Parse:\n\n{}", truncate_chars(&text, 4000))}
        ],
        "max_tokens": 512,
        "temperature": 0
    });

    let client = reqwest::Client::new();
    let url = format!(
        "{}/chat/completions",
        provider.base_url.trim_end_matches('/')
    );
    let resp = client
        .post(&url)
        .header("Authorization", format!("Bearer {}", provider.api_key))
        .json(&body)
        .timeout(std::time::Duration::from_secs(30))
        .send()
        .await
        .map_err(|e| e.to_string())?;

    let status = resp.status();

    let resp_text = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        return Err(format!(
            "LLM error {}: {}",
            status,
            llm_error_message(&resp_text)
        ));
    }
    let val: Value = serde_json::from_str(&resp_text).map_err(|e| e.to_string())?;
    let content = val["choices"][0]["message"]["content"]
        .as_str()
        .unwrap_or("");
    if content.trim().is_empty() {
        return Err("LLM returned an empty response".to_string());
    }
    let result = parse_llm_json(content)?;

    let transport = result["transport"].as_str().unwrap_or("stdio").to_string();
    Ok(McpScanResult {
        transport: transport.clone(),
        name: result["name"].as_str().unwrap_or("mcp_server").to_string(),
        command: if transport == "sse" {
            String::new()
        } else {
            result["command"].as_str().unwrap_or("").to_string()
        },
        args: result["args"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(|s| s.to_string()))
                    .collect()
            })
            .unwrap_or_default(),
        env: result["env"]
            .as_object()
            .map(|o| {
                o.iter()
                    .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
                    .collect()
            })
            .unwrap_or_default(),
        url: result["url"].as_str().unwrap_or("").to_string(),
        headers: result["headers"]
            .as_object()
            .map(|o| {
                o.iter()
                    .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
                    .collect()
            })
            .unwrap_or_default(),
        description: result["description"].as_str().unwrap_or("").to_string(),
        warnings: vec![],
        confidence: 85,
    })
}

fn title_case(s: &str) -> String {
    s.split_whitespace()
        .map(|w| {
            let mut c = w.chars();
            match c.next() {
                None => String::new(),
                Some(f) => f.to_uppercase().to_string() + c.as_str(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::{compatibility_entry, parse_llm_json, truncate_chars, McpServer};
    use std::collections::HashMap;

    #[test]
    fn compatibility_save_preserves_http_fields_and_original_name() {
        let entry = compatibility_entry(McpServer {
            name: "Old MCP".into(),
            transport: "http".into(),
            command: "node".into(),
            args: vec!["server.js".into()],
            env: HashMap::from([("TOKEN".into(), "env-value".into())]),
            url: "https://example.com/mcp".into(),
            headers: HashMap::from([("Authorization".into(), "header-value".into())]),
            description: "remote".into(),
        });
        assert_eq!(entry.name, "Old MCP");
        assert_eq!(entry.transport, "http");
        assert_eq!(entry.command, "node");
        assert_eq!(entry.args, vec!["server.js"]);
        assert_eq!(entry.env["TOKEN"], "env-value");
        assert_eq!(entry.url, "https://example.com/mcp");
        assert_eq!(entry.headers["Authorization"], "header-value");
        assert_eq!(entry.description, "remote");
    }

    #[test]
    fn truncates_unicode_on_character_boundaries() {
        assert_eq!(truncate_chars("智能解析MCP", 4), "智能解析");
    }

    #[test]
    fn parses_json_from_fenced_or_explanatory_output() {
        let fenced = parse_llm_json("```json\n{\"name\":\"demo\"}\n```").unwrap();
        assert_eq!(fenced["name"], "demo");

        let explained = parse_llm_json("Result: {\"transport\":\"stdio\"} done").unwrap();
        assert_eq!(explained["transport"], "stdio");
    }
}
