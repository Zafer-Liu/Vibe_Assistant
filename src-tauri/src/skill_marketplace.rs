//! Curated public Skill marketplace backed by official GitHub repositories.
//!
//! Listing is cached locally. Installation downloads a bounded package into a
//! temporary directory, validates every relative path, and then imports it into
//! the existing managed Skill registry. Downloaded scripts are never executed.

use futures_util::stream::{self, StreamExt};
use serde::{Deserialize, Serialize};
use std::path::{Component, Path, PathBuf};

const MARKETPLACE_CACHE_KEY: &str = "skill_marketplace_catalog_v1";
const CACHE_TTL_HOURS: i64 = 6;
const MAX_PACKAGE_FILES: usize = 200;
const MAX_PACKAGE_BYTES: u64 = 10 * 1024 * 1024;
const MAX_FILE_BYTES: u64 = 2 * 1024 * 1024;

#[derive(Clone, Copy)]
struct MarketplaceSource {
    id: &'static str,
    label: &'static str,
    owner: &'static str,
    repo: &'static str,
    revision: &'static str,
    prefix: &'static str,
}

const SOURCES: [MarketplaceSource; 2] = [
    MarketplaceSource {
        id: "openai",
        label: "OpenAI",
        owner: "openai",
        repo: "skills",
        revision: "main",
        prefix: "skills/.curated",
    },
    MarketplaceSource {
        id: "anthropic",
        label: "Anthropic",
        owner: "anthropics",
        repo: "skills",
        revision: "main",
        prefix: "skills",
    },
];

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct MarketplaceSkill {
    pub id: String,
    pub source: String,
    pub source_label: String,
    pub name: String,
    pub description: String,
    pub repository_url: String,
    pub skill_url: String,
    pub revision: String,
    pub files: Vec<String>,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
struct MarketplaceCache {
    items: Vec<MarketplaceSkill>,
    fetched_at: String,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub struct MarketplaceCatalog {
    pub items: Vec<MarketplaceSkill>,
    pub fetched_at: String,
    pub from_cache: bool,
    pub warning: Option<String>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub struct MarketplaceSkillPreview {
    pub item: MarketplaceSkill,
    pub content: String,
}

#[derive(Deserialize)]
struct GitTreeResponse {
    tree: Vec<GitTreeEntry>,
    truncated: bool,
}

#[derive(Clone, Deserialize)]
struct GitTreeEntry {
    path: String,
    #[serde(rename = "type")]
    kind: String,
    sha: String,
    mode: String,
    size: Option<u64>,
}

fn source_by_id(id: &str) -> Result<MarketplaceSource, String> {
    SOURCES
        .iter()
        .copied()
        .find(|source| source.id == id)
        .ok_or_else(|| "unsupported marketplace source".to_string())
}

fn marketplace_client() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .user_agent("Agent-Manager/1.0 skill-marketplace")
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .map_err(|error| error.to_string())
}

async fn fetch_tree(
    client: &reqwest::Client,
    source: MarketplaceSource,
) -> Result<Vec<GitTreeEntry>, String> {
    let url = format!(
        "https://api.github.com/repos/{}/{}/git/trees/{}?recursive=1",
        source.owner, source.repo, source.revision
    );
    let response = client
        .get(url)
        .send()
        .await
        .map_err(|error| format!("读取 {} 技能索引失败：{error}", source.label))?
        .error_for_status()
        .map_err(|error| format!("读取 {} 技能索引失败：{error}", source.label))?
        .json::<GitTreeResponse>()
        .await
        .map_err(|error| format!("解析 {} 技能索引失败：{error}", source.label))?;
    if response.truncated {
        return Err(format!(
            "{} 技能索引过大，GitHub 返回了不完整结果",
            source.label
        ));
    }
    Ok(response.tree)
}

fn skill_names(source: MarketplaceSource, tree: &[GitTreeEntry]) -> Vec<String> {
    let prefix = format!("{}/", source.prefix);
    let mut names = tree
        .iter()
        .filter(|entry| entry.kind == "blob")
        .filter_map(|entry| {
            let rest = entry.path.strip_prefix(&prefix)?;
            let name = rest.strip_suffix("/SKILL.md")?;
            if name.is_empty() || name.contains('/') || !is_safe_slug(name) {
                return None;
            }
            Some(name.to_string())
        })
        .collect::<Vec<_>>();
    names.sort();
    names.dedup();
    names
}

fn is_safe_slug(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 80
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
}

fn files_for_skill(source: MarketplaceSource, name: &str, tree: &[GitTreeEntry]) -> Vec<String> {
    let root = format!("{}/{name}/", source.prefix);
    let mut files = tree
        .iter()
        .filter(|entry| entry.kind == "blob")
        .filter_map(|entry| entry.path.strip_prefix(&root).map(str::to_string))
        .filter(|path| safe_relative_path(path).is_some())
        .collect::<Vec<_>>();
    files.sort();
    files
}

fn raw_url(source: MarketplaceSource, path: &str) -> String {
    format!(
        "https://raw.githubusercontent.com/{}/{}/{}/{}",
        source.owner, source.repo, source.revision, path
    )
}

fn repository_url(source: MarketplaceSource) -> String {
    format!("https://github.com/{}/{}", source.owner, source.repo)
}

fn parse_description(content: &str) -> String {
    let Some(frontmatter) = content
        .strip_prefix("---")
        .and_then(|rest| rest.split_once("\n---").map(|(head, _)| head))
    else {
        return String::new();
    };
    let lines = frontmatter.lines().collect::<Vec<_>>();
    for (index, line) in lines.iter().enumerate() {
        let Some(value) = line.trim_end().strip_prefix("description:") else {
            continue;
        };
        let value = value.trim();
        if !value.is_empty() && !matches!(value, "|" | ">" | "|-" | ">-" | "|+" | ">+") {
            return value.trim_matches(['\'', '"']).trim().to_string();
        }
        let mut parts = Vec::new();
        for next in lines.iter().skip(index + 1) {
            if next.trim().is_empty() {
                continue;
            }
            if next.starts_with(' ') || next.starts_with('\t') {
                parts.push(next.trim().to_string());
            } else {
                break;
            }
        }
        return parts.join(" ");
    }
    String::new()
}

async fn build_item(
    client: &reqwest::Client,
    source: MarketplaceSource,
    name: String,
    tree: &[GitTreeEntry],
) -> MarketplaceSkill {
    let skill_path = format!("{}/{name}/SKILL.md", source.prefix);
    let content = match client.get(raw_url(source, &skill_path)).send().await {
        Ok(response) => response.text().await.unwrap_or_default(),
        Err(_) => String::new(),
    };
    let revision = tree
        .iter()
        .find(|entry| entry.path == skill_path)
        .map(|entry| entry.sha.clone())
        .unwrap_or_default();
    MarketplaceSkill {
        id: format!("{}:{name}", source.id),
        source: source.id.to_string(),
        source_label: source.label.to_string(),
        name,
        description: parse_description(&content),
        repository_url: repository_url(source),
        skill_url: format!(
            "https://github.com/{}/{}/blob/{}/{}",
            source.owner, source.repo, source.revision, skill_path
        ),
        revision,
        files: files_for_skill(source, &skill_path_name(&skill_path), tree),
    }
}

fn skill_path_name(skill_path: &str) -> String {
    Path::new(skill_path)
        .parent()
        .and_then(Path::file_name)
        .and_then(|name| name.to_str())
        .unwrap_or_default()
        .to_string()
}

async fn fetch_source_catalog(source: MarketplaceSource) -> Result<Vec<MarketplaceSkill>, String> {
    let client = marketplace_client()?;
    let tree = fetch_tree(&client, source).await?;
    let names = skill_names(source, &tree);
    let items = stream::iter(names.into_iter().map(|name| {
        let client = client.clone();
        let tree = tree.clone();
        async move { build_item(&client, source, name, &tree).await }
    }))
    .buffer_unordered(8)
    .collect::<Vec<_>>()
    .await;
    Ok(items)
}

async fn fetch_catalog_remote() -> Result<Vec<MarketplaceSkill>, String> {
    let results =
        futures_util::future::join_all(SOURCES.into_iter().map(fetch_source_catalog)).await;
    let mut items = Vec::new();
    let mut errors = Vec::new();
    for result in results {
        match result {
            Ok(mut source_items) => items.append(&mut source_items),
            Err(error) => errors.push(error),
        }
    }
    if items.is_empty() {
        return Err(errors.join("；"));
    }
    items.sort_by(|a, b| a.source.cmp(&b.source).then(a.name.cmp(&b.name)));
    Ok(items)
}

fn read_cache() -> Option<MarketplaceCache> {
    crate::telemetry_store::shared_store()?.app_setting_get(MARKETPLACE_CACHE_KEY)
}

fn cache_is_fresh(cache: &MarketplaceCache) -> bool {
    chrono::DateTime::parse_from_rfc3339(&cache.fetched_at)
        .map(|fetched| {
            chrono::Utc::now()
                .signed_duration_since(fetched.with_timezone(&chrono::Utc))
                .num_hours()
                < CACHE_TTL_HOURS
        })
        .unwrap_or(false)
}

#[tauri::command]
pub async fn skill_marketplace_list(refresh: bool) -> Result<MarketplaceCatalog, String> {
    if !refresh {
        if let Some(cache) = read_cache().filter(cache_is_fresh) {
            return Ok(MarketplaceCatalog {
                items: cache.items,
                fetched_at: cache.fetched_at,
                from_cache: true,
                warning: None,
            });
        }
    }
    match fetch_catalog_remote().await {
        Ok(items) => {
            let fetched_at = chrono::Utc::now().to_rfc3339();
            if let Some(store) = crate::telemetry_store::shared_store() {
                let _ = store.app_setting_set(
                    MARKETPLACE_CACHE_KEY,
                    &MarketplaceCache {
                        items: items.clone(),
                        fetched_at: fetched_at.clone(),
                    },
                );
            }
            Ok(MarketplaceCatalog {
                items,
                fetched_at,
                from_cache: false,
                warning: None,
            })
        }
        Err(error) => {
            if let Some(cache) = read_cache() {
                Ok(MarketplaceCatalog {
                    items: cache.items,
                    fetched_at: cache.fetched_at,
                    from_cache: true,
                    warning: Some(error),
                })
            } else {
                Err(error)
            }
        }
    }
}

async fn selected_item(
    source_id: &str,
    name: &str,
) -> Result<(MarketplaceSource, MarketplaceSkill, Vec<GitTreeEntry>), String> {
    if !is_safe_slug(name) {
        return Err("invalid marketplace skill name".into());
    }
    let source = source_by_id(source_id)?;
    let client = marketplace_client()?;
    let tree = fetch_tree(&client, source).await?;
    if !skill_names(source, &tree).iter().any(|item| item == name) {
        return Err("marketplace skill not found".into());
    }
    let item = build_item(&client, source, name.to_string(), &tree).await;
    Ok((source, item, tree))
}

#[tauri::command]
pub async fn skill_marketplace_preview(
    source: String,
    name: String,
) -> Result<MarketplaceSkillPreview, String> {
    let (source_config, item, _) = selected_item(&source, &name).await?;
    let path = format!("{}/{name}/SKILL.md", source_config.prefix);
    let content = marketplace_client()?
        .get(raw_url(source_config, &path))
        .send()
        .await
        .map_err(|error| error.to_string())?
        .error_for_status()
        .map_err(|error| error.to_string())?
        .text()
        .await
        .map_err(|error| error.to_string())?;
    Ok(MarketplaceSkillPreview { item, content })
}

fn safe_relative_path(value: &str) -> Option<PathBuf> {
    let path = Path::new(value);
    if path.is_absolute() || value.is_empty() {
        return None;
    }
    if path
        .components()
        .any(|component| !matches!(component, Component::Normal(_)))
    {
        return None;
    }
    Some(path.to_path_buf())
}

#[tauri::command]
pub async fn skill_marketplace_install(
    source: String,
    name: String,
) -> Result<crate::skill_registry::SkillItem, String> {
    let (source_config, _item, tree) = selected_item(&source, &name).await?;
    let package_root = format!("{}/{name}/", source_config.prefix);
    let entries = tree
        .iter()
        .filter(|entry| entry.kind == "blob" && entry.path.starts_with(&package_root))
        .collect::<Vec<_>>();
    if entries.len() > MAX_PACKAGE_FILES {
        return Err(format!("技能包含 {} 个文件，超过安全上限", entries.len()));
    }
    if entries
        .iter()
        .any(|entry| !matches!(entry.mode.as_str(), "100644" | "100755"))
    {
        return Err("技能包包含不支持的符号链接或特殊文件".into());
    }
    let total_size = entries
        .iter()
        .map(|entry| entry.size.unwrap_or(0))
        .sum::<u64>();
    if total_size > MAX_PACKAGE_BYTES {
        return Err("技能包超过 10 MB 安全上限".into());
    }
    if entries
        .iter()
        .any(|entry| entry.size.unwrap_or(0) > MAX_FILE_BYTES)
    {
        return Err("技能包包含超过 2 MB 的单个文件".into());
    }

    let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
    let skill_dir = temp.path().join(&name);
    let client = marketplace_client()?;
    let mut downloaded_bytes = 0_u64;
    for entry in entries {
        let relative = entry
            .path
            .strip_prefix(&package_root)
            .and_then(safe_relative_path)
            .ok_or_else(|| format!("技能包包含不安全路径：{}", entry.path))?;
        let target = skill_dir.join(relative);
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
        }
        let bytes = client
            .get(raw_url(source_config, &entry.path))
            .send()
            .await
            .map_err(|error| format!("下载 {} 失败：{error}", entry.path))?
            .error_for_status()
            .map_err(|error| format!("下载 {} 失败：{error}", entry.path))?
            .bytes()
            .await
            .map_err(|error| format!("下载 {} 失败：{error}", entry.path))?;
        if bytes.len() as u64 > MAX_FILE_BYTES {
            return Err(format!("文件 {} 超过 2 MB 安全上限", entry.path));
        }
        downloaded_bytes += bytes.len() as u64;
        if downloaded_bytes > MAX_PACKAGE_BYTES {
            return Err("技能包实际下载内容超过 10 MB 安全上限".into());
        }
        std::fs::write(&target, bytes).map_err(|error| error.to_string())?;
    }
    crate::skill_registry::import_marketplace_skill(
        &format!("market-{}", source_config.id),
        &name,
        &skill_dir,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_top_level_skill_documents_become_catalog_items() {
        let tree = vec![
            GitTreeEntry {
                path: "skills/demo/SKILL.md".into(),
                kind: "blob".into(),
                sha: "1".into(),
                mode: "100644".into(),
                size: Some(1),
            },
            GitTreeEntry {
                path: "skills/demo/references/SKILL.md".into(),
                kind: "blob".into(),
                sha: "2".into(),
                mode: "100644".into(),
                size: Some(1),
            },
            GitTreeEntry {
                path: "skills/.hidden/SKILL.md".into(),
                kind: "blob".into(),
                sha: "3".into(),
                mode: "100644".into(),
                size: Some(1),
            },
        ];
        assert_eq!(skill_names(SOURCES[1], &tree), vec!["demo"]);
    }

    #[test]
    fn rejects_path_traversal() {
        assert!(safe_relative_path("scripts/run.py").is_some());
        assert!(safe_relative_path("../secret").is_none());
        assert!(safe_relative_path("/rooted").is_none());
    }

    #[test]
    fn parses_frontmatter_description() {
        assert_eq!(
            parse_description("---\nname: demo\ndescription: 'Useful demo'\n---\n"),
            "Useful demo"
        );
    }

    #[tokio::test]
    #[ignore = "requires GitHub network access"]
    async fn official_catalog_smoke_test() {
        let items = fetch_source_catalog(SOURCES[0]).await.unwrap();
        assert!(!items.is_empty());
        assert!(items
            .iter()
            .all(|item| item.files.iter().any(|file| file == "SKILL.md")));
        assert!(items.iter().any(|item| !item.description.is_empty()));
    }
}
