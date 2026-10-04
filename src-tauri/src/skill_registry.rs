//! File-based, version-aware Skill registry.
//!
//! The shared directory is the source of truth.  We only copy complete
//! `SKILL.md` files after a preview, never infer deployable skills from tool
//! traces or overwrite a target without an explicit `overwrite` flag.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct SkillItem {
    pub source: String,
    pub name: String,
    pub description: String,
    pub path: String,
    pub hash: String,
    pub version: u32,
    pub status: String,
    pub assigned_agents: Vec<String>,
    /// Skill 目录内打包发布的全部文件（相对路径，含 SKILL.md）。
    pub files: Vec<String>,
    /// 技能性质：builtin（厂商内置）/ plugin（agent 插件市场安装）/
    /// local（用户本地或 Vibe Assistant 部署）/ marketplace（技能市场导入）。
    /// 旧缓存与旧 manifest 无此字段时按 local 处理，重新扫描后回填。
    #[serde(default = "default_skill_origin")]
    pub origin: String,
}

fn default_skill_origin() -> String {
    "local".into()
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
struct SkillManifest {
    version: u32,
    status: String,
    assigned_agents: Vec<String>,
    current_hash: String,
    updated_at: String,
    /// 扫描时判定的技能性质（见 SkillItem::origin），随 manifest 持久化，
    /// 共享库目录重读时据此恢复。
    #[serde(default)]
    origin: String,
}

const SKILL_CATALOG_SETTING_KEY: &str = "skill_catalog";

#[derive(Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub struct SkillSyncPreview {
    pub target: String,
    pub create: Vec<SkillItem>,
    pub update: Vec<SkillItem>,
    pub unchanged: Vec<SkillItem>,
    pub conflict: Vec<SkillItem>,
    /// 已发布、应装备到该 target，但 target 本地无副本的 Skill。
    /// 「部署状态」据此显示「缺失」徽标，部署时与 create 一并下发。
    pub missing: Vec<SkillItem>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub struct SkillDocument {
    pub item: SkillItem,
    pub content: String,
}

/// 一份漂移对比文件：共享库版本与 Agent 本地版本的内容。`None` 表示该侧
/// 不存在此文件，或内容不是 UTF-8 文本（二进制，无法做行级对比）。
#[derive(Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub struct SkillDriftFile {
    pub path: String,
    pub shared_text: Option<String>,
    pub local_text: Option<String>,
}

fn shared_root() -> PathBuf {
    dirs_next::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("agent-manager")
        .join("shared-skills")
}

fn is_managed_skill_source(source: &str) -> bool {
    crate::agent_sources::is_supported_source(source)
        || matches!(source, "market-openai" | "market-anthropic")
}

fn manifest_path(skill_path: &Path) -> PathBuf {
    skill_path
        .parent()
        .unwrap_or(skill_path)
        .join("manifest.json")
}

fn version_root(skill_path: &Path) -> PathBuf {
    skill_path.parent().unwrap_or(skill_path).join(".versions")
}

fn default_manifest(hash: &str) -> SkillManifest {
    SkillManifest {
        version: 1,
        status: "draft".into(),
        assigned_agents: Vec::new(),
        current_hash: hash.into(),
        updated_at: chrono::Utc::now().to_rfc3339(),
        origin: String::new(),
    }
}

fn read_manifest(skill_path: &Path, hash: &str) -> SkillManifest {
    std::fs::read_to_string(manifest_path(skill_path))
        .ok()
        .and_then(|text| serde_json::from_str::<SkillManifest>(&text).ok())
        .filter(|manifest| manifest.current_hash == hash)
        .unwrap_or_else(|| default_manifest(hash))
}

fn write_manifest(skill_path: &Path, manifest: &SkillManifest) -> Result<(), String> {
    let path = manifest_path(skill_path);
    let parent = path.parent().ok_or("skill manifest has no parent")?;
    std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    std::fs::write(
        path,
        serde_json::to_string_pretty(manifest).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())
}

fn with_manifest(mut item: SkillItem, skill_path: &Path) -> SkillItem {
    let manifest = read_manifest(skill_path, &item.hash);
    item.version = manifest.version;
    item.status = manifest.status;
    item.assigned_agents = manifest.assigned_agents;
    if !manifest.origin.is_empty() {
        item.origin = manifest.origin;
    }
    item
}

fn path_components(path: &Path) -> Vec<&str> {
    path.components()
        .filter_map(|c| c.as_os_str().to_str())
        .collect()
}

/// 依据扫描时的原始路径判定技能性质（见 SkillItem::origin）。市场命名
/// 空间只看 source 前缀且必须最先判定：市场技能落库后再读取时，路径已
/// 换成共享库路径，路径启发式会失效。
fn classify_origin(source: &str, path: &Path) -> &'static str {
    if source.starts_with("market-") {
        return "marketplace";
    }
    let components = path_components(path);
    // MiniMax 的厂商内置技能根。
    if components.iter().any(|c| *c == ".builtin-skills") {
        return "builtin";
    }
    if let Some(index) = components.iter().position(|c| *c == "plugins") {
        // plugins[/cache]/<marketplace>/...：官方内置或捆绑的 marketplace
        // （workbuddy-builtin、qoder-bundler 等）归 builtin，其余为用户经
        // agent 插件市场安装的归 plugin。
        let after = &components[index + 1..];
        let marketplace = if after.first() == Some(&"cache") {
            after.get(1)
        } else {
            after.first()
        };
        if let Some(marketplace) = marketplace {
            let lower = marketplace.to_lowercase();
            if lower.contains("builtin") || lower.contains("bundler") {
                return "builtin";
            }
        }
        return "plugin";
    }
    "local"
}

/// Vibe Assistant 同步到 Kimi 的托管插件目录（plugins/managed/...）是部署
/// 产物而非用户配置，扫描时跳过，避免以 kimi:<name> 身份回声入库。
fn is_agent_manager_managed(path: &Path) -> bool {
    let components = path_components(path);
    components.windows(2).any(|w| w == ["plugins", "managed"])
}

/// plugins 层级下直接落在版本目录里的 SKILL.md（如
/// `plugins/cache/.../tencent-docx/5.6.2-wb.39298511.../SKILL.md`）是插件
/// 自述文件而非独立技能；返回其安全化目录名，即旧版扫描逻辑误入库的
/// 条目名，用于跳过与清理。
fn plugin_version_artifact(path: &Path) -> Option<String> {
    if !path_components(path).iter().any(|c| *c == "plugins") {
        return None;
    }
    let dir = path.parent()?.file_name()?.to_str()?;
    let looks_versioned = dir.starts_with(|c: char| c.is_ascii_digit()) && dir.contains('-');
    if !looks_versioned {
        return None;
    }
    let safe = safe_name(dir);
    (!safe.is_empty()).then_some(safe)
}

/// 每个来源需扫描的根目录列表（多根 Agent 如 Qoder/MiniMax 一次覆盖）。
fn scan_roots() -> Vec<(&'static str, Vec<PathBuf>)> {
    crate::agent_sources::AGENT_SOURCE_IDS
        .iter()
        .map(|&id| (id, crate::agent_sources::skill_scan_roots(id)))
        .collect()
}

/// 同步目标目录，委托给 agent_sources 统一路径解析。
fn target_root(target: &str) -> Result<PathBuf, String> {
    crate::agent_sources::skill_target_root(target)
        .ok_or_else(|| format!("unsupported skill target: {target}"))
}

/// 部署共享 Skill 到目标 Agent：Kimi 额外确保托管插件清单与 installed.json
/// 注册项存在，否则 Kimi 不会发现 skills 目录下的任何内容。
fn deploy_shared_skill(target: &str, source_dir: &Path, dest: &Path) -> Result<(), String> {
    replace_dir(source_dir, dest)?;
    if target == "kimi" {
        ensure_kimi_skill_plugin()?;
    }
    Ok(())
}

/// Kimi 的 Skill 发现机制：只加载 `kimi.plugin.json` 声明 `skills` 字段、且
/// 登记在 `plugins/installed.json`（enabled: true）的插件。这里把共享库同步
/// 的全部 Skill 收拢进托管插件「agent-manager-skills」，并保证清单与注册
/// 项存在；重复调用幂等，只刷新 updatedAt。
fn ensure_kimi_skill_plugin() -> Result<(), String> {
    let plugin_root = crate::agent_sources::kimi_skill_plugin_root()
        .ok_or_else(|| "kimi skill plugin root not found".to_string())?;
    std::fs::create_dir_all(plugin_root.join("skills")).map_err(|e| e.to_string())?;
    let manifest = plugin_root.join("kimi.plugin.json");
    if !manifest.is_file() {
        let content = serde_json::json!({
            "$schema": "https://kimi.com/schemas/kimi.plugin.schema.json",
            "name": "agent-manager-skills",
            "version": "1.0.0",
            "description": "Skills published from the Vibe Assistant shared library",
            "skills": "./skills/",
        });
        std::fs::write(
            &manifest,
            serde_json::to_string_pretty(&content).map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())?;
    }
    let Some(home) = plugin_root.ancestors().nth(3).map(Path::to_path_buf) else {
        return Err("cannot resolve kimi home from plugin root".into());
    };
    let installed_path = home.join("plugins").join("installed.json");
    let mut installed: serde_json::Value = std::fs::read_to_string(&installed_path)
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_else(|| serde_json::json!({"version": 1, "plugins": []}));
    if installed
        .get("plugins")
        .and_then(|p| p.as_array())
        .is_none()
    {
        installed["plugins"] = serde_json::json!([]);
    }
    let plugins = installed["plugins"].as_array_mut().unwrap();
    let existing = plugins
        .iter()
        .position(|p| p.get("id").and_then(|v| v.as_str()) == Some("agent-manager-skills"))
        .map(|index| plugins[index].clone())
        .unwrap_or_else(|| {
            serde_json::json!({
                "id": "agent-manager-skills",
                "source": "local-path",
                "enabled": true,
            })
        });
    let mut merged = existing;
    {
        let obj = merged
            .as_object_mut()
            .ok_or("installed.json 插件项格式异常")?;
        let now = chrono::Utc::now().to_rfc3339();
        let root_string = plugin_root.to_string_lossy().to_string();
        obj.insert("root".into(), serde_json::json!(root_string.clone()));
        obj.insert("enabled".into(), serde_json::json!(true));
        obj.insert("updatedAt".into(), serde_json::json!(now.clone()));
        obj.entry("installedAt".to_string())
            .or_insert_with(|| serde_json::json!(now));
        obj.entry("originalSource".to_string())
            .or_insert_with(|| serde_json::json!(root_string));
    }
    match plugins
        .iter()
        .position(|p| p.get("id").and_then(|v| v.as_str()) == Some("agent-manager-skills"))
    {
        Some(index) => plugins[index] = merged,
        None => plugins.push(merged),
    }
    std::fs::write(
        &installed_path,
        serde_json::to_string_pretty(&installed).map_err(|e| e.to_string())?,
    )
    .map_err(|e| format!("写入 {} 失败：{e}", installed_path.display()))?;
    Ok(())
}

fn safe_name(name: &str) -> String {
    let value: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_') {
                c
            } else {
                '-'
            }
        })
        .collect();
    value
        .trim_matches('-')
        .to_string()
        .chars()
        .take(80)
        .collect()
}

fn hash(content: &[u8]) -> String {
    Sha256::digest(content)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// Skill 目录内属于打包内容的文件（排除注册表自身的 manifest.json 与
/// .versions/ 快照）。返回按相对路径排序的 (相对路径, 绝对路径) 列表。
fn skill_dir_files(root: &Path) -> Result<Vec<(String, PathBuf)>, String> {
    let mut files = Vec::new();
    collect_dir_files_inner(root, root, &mut files)?;
    files.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(files)
}

fn collect_dir_files_inner(
    root: &Path,
    dir: &Path,
    output: &mut Vec<(String, PathBuf)>,
) -> Result<(), String> {
    for entry in std::fs::read_dir(dir).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if name == ".versions"
            || (name == "manifest.json" && dir == root)
            // 运行时产物不随 Skill 打包。
            || name == "__pycache__"
            || name == ".DS_Store"
            || name.ends_with(".pyc")
        {
            continue;
        }
        if path.is_dir() {
            // 临时目录是原子替换过程的中间产物，不属于打包内容。
            if name.ends_with(".tmp") {
                continue;
            }
            collect_dir_files_inner(root, &path, output)?;
        } else if let Some(relative) = path.strip_prefix(root).ok() {
            output.push((relative.to_string_lossy().replace('\\', "/"), path.clone()));
        }
    }
    Ok(())
}

/// 目录级哈希：对打包文件集合的「相对路径 + 内容哈希」整体做 SHA-256，
/// 附属脚本/资源变化即视为 Skill 内容变化。
fn dir_hash(root: &Path) -> Result<String, String> {
    let mut digest = Sha256::new();
    for (relative, path) in skill_dir_files(root)? {
        let bytes = std::fs::read(&path).map_err(|e| format!("read {}: {e}", path.display()))?;
        digest.update(relative.as_bytes());
        digest.update([0]);
        digest.update(hash(&bytes).as_bytes());
        digest.update([0]);
    }
    Ok(digest
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}

/// 整目录复制（排除 manifest.json 与 .versions/）。直接写入目标：调用方
/// 负责先清理旧目标（见 replace_dir）。
fn copy_dir_contents(src: &Path, dst: &Path) -> Result<(), String> {
    for (relative, path) in skill_dir_files(src)? {
        let target = dst.join(&relative);
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        std::fs::copy(&path, &target)
            .map_err(|e| format!("copy {} → {}：{e}", path.display(), target.display()))?;
    }
    Ok(())
}

/// 原子替换整个 Skill 目录：复制到同级的 `<name>.tmp` 临时目录，完全写入
/// 后再删除旧目录并改名（Windows 不允许 rename 覆盖已存在的目录）。
fn replace_dir(src: &Path, dst: &Path) -> Result<(), String> {
    let temp = dst.with_extension("tmp");
    if temp.exists() {
        std::fs::remove_dir_all(&temp).map_err(|e| e.to_string())?;
    }
    copy_dir_contents(src, &temp)?;
    if dst.exists() {
        std::fs::remove_dir_all(dst).map_err(|e| e.to_string())?;
    }
    std::fs::rename(&temp, dst).map_err(|e| e.to_string())
}

/// 就地替换目标目录的打包内容，但保留 manifest.json 与 .versions/。用于
/// 共享目录内部的内容更新（快照回滚、重新导入），先删旧内容再复制，保证
/// 源侧已删除的附属文件不会残留。
fn replace_contents_within(src: &Path, dst: &Path) -> Result<(), String> {
    if dst.exists() {
        for (_, path) in skill_dir_files(dst)? {
            let _ = std::fs::remove_file(&path);
        }
    }
    copy_dir_contents(src, dst)
}

fn parse_skill(path: &Path, source: &str) -> Result<SkillItem, String> {
    let content = std::fs::read(path).map_err(|e| format!("read {}: {e}", path.display()))?;
    let text = String::from_utf8(content.clone())
        .map_err(|_| format!("{} is not UTF-8", path.display()))?;
    let mut frontmatter_name: Option<String> = None;
    let mut description = String::new();
    if let Some(frontmatter) = text
        .strip_prefix("---")
        .and_then(|rest| rest.split_once("\n---").map(|(head, _)| head))
    {
        let lines = frontmatter.lines().collect::<Vec<_>>();
        let mut index = 0;
        while index < lines.len() {
            let line = lines[index];
            let trimmed = line.trim_end();
            if let Some(value) = trimmed.strip_prefix("name:") {
                frontmatter_name = Some(value.trim().trim_matches(['\'', '"']).to_string());
                index += 1;
            } else if let Some(value) = trimmed.strip_prefix("description:") {
                description = collect_description(value, &lines, &mut index);
            } else {
                index += 1;
            }
        }
    }
    let name = derive_skill_name(path, frontmatter_name.as_deref());
    let skill_dir = path.parent().unwrap_or(path);
    let hash = dir_hash(skill_dir)?;
    let files = skill_dir_files(skill_dir)?
        .into_iter()
        .map(|(relative, _)| relative)
        .collect();
    Ok(SkillItem {
        source: source.to_string(),
        name,
        description,
        path: path.display().to_string(),
        hash,
        version: 1,
        status: "draft".into(),
        assigned_agents: Vec::new(),
        origin: classify_origin(source, path).to_string(),
        files,
    })
}

/// 从 SKILL.md 推导安全的 slug 名。优先采用 frontmatter 的 `name:`（当它是
/// 合法 ASCII slug 时）；否则回退到目录名，跳过纯版本号目录（如 `0.5.9`）。
fn derive_skill_name(path: &Path, frontmatter_name: Option<&str>) -> String {
    if let Some(frontmatter) = frontmatter_name {
        let safe = safe_name(frontmatter);
        if !safe.is_empty() && safe == frontmatter {
            return safe;
        }
    }
    for ancestor in path.ancestors().skip(1) {
        let Some(name) = ancestor.file_name().and_then(|s| s.to_str()) else {
            continue;
        };
        if !name.is_empty() && name.chars().all(|c| c.is_ascii_digit() || c == '.') {
            continue;
        }
        let safe = safe_name(name);
        if !safe.is_empty() {
            return safe;
        }
    }
    "skill".to_string()
}

/// 解析 frontmatter 的 `description:` 值，兼容内联引号、单行与 YAML 块风格。
fn collect_description(first: &str, lines: &[&str], index: &mut usize) -> String {
    *index += 1;
    let first = first.trim();
    if let Some(value) = first
        .strip_prefix('"')
        .and_then(|s| s.strip_suffix('"'))
        .or_else(|| first.strip_prefix('\'').and_then(|s| s.strip_suffix('\'')))
    {
        return value.trim().to_string();
    }
    let is_block = matches!(first, "|" | ">" | "|-" | ">-" | "|+" | ">+");
    if !is_block && !first.is_empty() {
        return first.to_string();
    }
    let mut parts: Vec<String> = Vec::new();
    while *index < lines.len() {
        let line = lines[*index];
        if line.trim().is_empty() {
            *index += 1;
            continue;
        }
        if line.starts_with(' ') || line.starts_with('\t') {
            parts.push(line.trim().to_string());
            *index += 1;
        } else {
            break;
        }
    }
    parts.join(" ")
}

fn collect_skill_files(root: &Path, output: &mut Vec<PathBuf>) -> Result<(), String> {
    if !root.exists() {
        return Ok(());
    }
    for entry in std::fs::read_dir(root).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        let path = entry.path();
        if path.is_dir() {
            // 版本快照目录里的 SKILL.md 是历史副本，不是独立 Skill。
            if path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name == ".versions" || name == "__pycache__")
            {
                continue;
            }
            collect_skill_files(&path, output)?;
        } else if path.file_name().and_then(|name| name.to_str()) == Some("SKILL.md") {
            output.push(path);
        }
    }
    Ok(())
}

fn shared_skills() -> Result<Vec<SkillItem>, String> {
    let root = shared_root();
    let mut skills = Vec::new();
    // 注册锚点固定为 <source>/<name>/SKILL.md 这一层。Skill 目录内嵌套的
    // 其他 SKILL.md（如 lark-tools 打包的 cli-skills 子树）属于附属文件，
    // 不再注册为独立条目，否则同一 Skill 会在列表里重复出现。
    let sources = match std::fs::read_dir(&root) {
        Ok(iter) => iter,
        Err(_) => return Ok(skills),
    };
    for source_entry in sources.flatten() {
        let Some(source) = source_entry.file_name().to_str().map(str::to_string) else {
            continue;
        };
        let Ok(skill_dirs) = std::fs::read_dir(source_entry.path()) else {
            continue;
        };
        for skill_entry in skill_dirs.flatten() {
            let path = skill_entry.path().join("SKILL.md");
            if !path.is_file() {
                continue;
            }
            if let Ok(item) = parse_skill(&path, &source).map(|item| with_manifest(item, &path)) {
                skills.push(item);
            }
        }
    }
    Ok(skills)
}

fn cache_skills(skills: &[SkillItem]) {
    if let Some(store) = crate::telemetry_store::shared_store() {
        let _ = store.app_setting_set(SKILL_CATALOG_SETTING_KEY, &skills);
    }
}

fn refresh_skill_cache() -> Result<Vec<SkillItem>, String> {
    let skills = shared_skills()?;
    cache_skills(&skills);
    Ok(skills)
}

/// Update a single skill in the SQLite cache without re-reading all skill files
/// from disk.  Used after single-item mutations (status/assignment/publish/
/// rollback) so that `skill_list` stays consistent across sessions.
fn cache_upsert_skill(item: &SkillItem) {
    if let Some(store) = crate::telemetry_store::shared_store() {
        if let Some(mut skills) = store.app_setting_get::<Vec<SkillItem>>(SKILL_CATALOG_SETTING_KEY)
        {
            if let Some(existing) = skills
                .iter_mut()
                .find(|s| s.source == item.source && s.name == item.name)
            {
                *existing = item.clone();
            } else {
                skills.push(item.clone());
            }
            let _ = store.app_setting_set(SKILL_CATALOG_SETTING_KEY, &skills);
        }
    }
}

/// Import existing skills into the Vibe Assistant shared directory.  Each
/// source retains its own namespace so equal names never silently overwrite.
#[tauri::command]
pub async fn skill_scan() -> Result<Vec<SkillItem>, String> {
    let root = shared_root();
    std::fs::create_dir_all(&root).map_err(|e| e.to_string())?;
    let mut imported = Vec::new();
    for (source, roots) in scan_roots() {
        let mut files = Vec::new();
        for scan_root in &roots {
            collect_skill_files(scan_root, &mut files)?;
        }
        // Vibe Assistant 托管插件（Kimi 同步产物）不入库，避免回声。
        files.retain(|path| !is_agent_manager_managed(path));
        // 插件版本根的 SKILL.md 不是独立技能：跳过，并清理旧版扫描逻辑
        // 误入库的同名条目（目录名即当时的条目名）。
        let mut artifact_names: Vec<String> = Vec::new();
        files.retain(|path| match plugin_version_artifact(path) {
            Some(name) => {
                artifact_names.push(name);
                false
            }
            None => true,
        });
        for name in &artifact_names {
            let stale = root.join(source).join(name);
            if stale.is_dir() {
                let _ = std::fs::remove_dir_all(&stale);
                eprintln!("[skill] 清理插件版本残留条目：{}", stale.display());
            }
        }
        // 固定处理顺序：路径字典序倒序（新到旧）。plugins/cache 里同一插件的
        // 多版本目录按版本号升序排列，倒序后最新版本最先处理；用户自建的
        // skills/ 根排在 plugins/cache 之后，优先级高于缓存副本。
        files.sort();
        files.reverse();
        // 同一 source 内按 Skill 名去重：多版本插件缓存会让同一
        // (source, name) 收集到多份，而前端列表以 `${source}:${name}` 作为
        // React key，重复条目会破坏列表协调，导致切换筛选时渲染错乱。
        // 首次出现的（最新）版本之后的同名副本全部跳过。
        let mut seen_names = std::collections::HashSet::new();
        for path in files {
            let skill = match parse_skill(&path, source) {
                Ok(skill) => skill,
                Err(error) => {
                    eprintln!("[skill] 跳过无法解析的 {}：{error}", path.display());
                    continue;
                }
            };
            if !seen_names.insert(skill.name.clone()) {
                continue;
            }
            let skill_dir = path
                .parent()
                .ok_or("SKILL.md has no parent directory")?
                .to_path_buf();
            let target_dir = root.join(source).join(&skill.name);
            let target = target_dir.join("SKILL.md");
            std::fs::create_dir_all(&target_dir).map_err(|e| e.to_string())?;
            let next_hash = dir_hash(&skill_dir)?;
            let mut manifest = read_manifest(&target, &next_hash);
            if target.exists() {
                let previous_hash = dir_hash(&target_dir)?;
                if previous_hash != next_hash {
                    let previous_manifest = read_manifest(&target, &previous_hash);
                    // 整目录快照（含附属脚本/资源），旧版单文件快照不受影响。
                    let snapshot = version_root(&target).join(format!(
                        "v{}-{}",
                        previous_manifest.version,
                        &previous_hash[..12]
                    ));
                    copy_dir_contents(&target_dir, &snapshot)?;
                    manifest = previous_manifest;
                    manifest.version += 1;
                    manifest.status = "draft".into();
                    manifest.current_hash = next_hash.clone();
                    manifest.updated_at = chrono::Utc::now().to_rfc3339();
                }
            }
            replace_contents_within(&skill_dir, &target_dir)?;
            // 重新扫描时以最新路径判定回填性质，分类规则升级后无需迁移。
            manifest.origin = skill.origin.clone();
            write_manifest(&target, &manifest)?;
            imported.push(with_manifest(
                SkillItem {
                    path: target.display().to_string(),
                    ..skill
                },
                &target,
            ));
        }
    }
    // File copies remain necessary for external Agents, but the database
    // catalog makes reopening the Skill view an O(1) SQLite read.
    let _ = refresh_skill_cache()?;
    Ok(imported)
}

/// Catalog read shared by the IPC command and the stdio MCP server (which is
/// fully synchronous and must not call async commands).
pub fn skill_list_impl() -> Result<Vec<SkillItem>, String> {
    if let Some(store) = crate::telemetry_store::shared_store() {
        if let Some(skills) = store.app_setting_get(SKILL_CATALOG_SETTING_KEY) {
            return Ok(skills);
        }
    }
    refresh_skill_cache()
}

#[tauri::command]
pub async fn skill_list() -> Result<Vec<SkillItem>, String> {
    skill_list_impl()
}

/// Reads only a skill already registered in the managed shared directory; the
/// frontend never supplies a raw filesystem path.
pub fn skill_read_impl(source: String, name: String) -> Result<SkillDocument, String> {
    let allowed_source = is_managed_skill_source(&source);
    let safe = safe_name(&name);
    if !allowed_source || safe != name || safe.is_empty() {
        return Err("invalid shared skill identifier".into());
    }
    let path = shared_root().join(&source).join(&safe).join("SKILL.md");
    let item = with_manifest(parse_skill(&path, &source)?, &path);
    let content = std::fs::read_to_string(&path).map_err(|e| e.to_string())?;
    Ok(SkillDocument { item, content })
}

#[tauri::command]
pub async fn skill_read(source: String, name: String) -> Result<SkillDocument, String> {
    skill_read_impl(source, name)
}

#[tauri::command]
pub async fn skill_sync_preview(target: String) -> Result<SkillSyncPreview, String> {
    build_preview(&target)
}

#[tauri::command]
pub async fn skill_sync_apply(target: String, overwrite: bool) -> Result<SkillSyncPreview, String> {
    let preview = build_preview(&target)?;
    let destination_root = target_root(&target)?;
    // create 与 missing 都属于「本地无副本、应补齐」的部署语义，一并下发。
    for skill in preview.create.iter().chain(preview.missing.iter()) {
        let source = PathBuf::from(&skill.path)
            .parent()
            .unwrap_or(Path::new(&skill.path))
            .to_path_buf();
        let dest = destination_root.join(&skill.name);
        deploy_shared_skill(&target, &source, &dest)?;
    }
    if overwrite {
        for skill in preview.update.iter().chain(preview.conflict.iter()) {
            let source = PathBuf::from(&skill.path)
                .parent()
                .unwrap_or(Path::new(&skill.path))
                .to_path_buf();
            let dest = destination_root.join(&skill.name);
            deploy_shared_skill(&target, &source, &dest)?;
        }
    }
    Ok(preview)
}

/// 对比一个已装备 Skill 在共享库与目标 Agent 本地的全部差异文件。
/// 只返回两侧哈希不同或仅一侧存在的文件；文本内容双侧给出，供前端做
/// 行级 diff；二进制或缺失侧为 None。
#[tauri::command]
pub async fn skill_drift_detail(
    target: String,
    source: String,
    name: String,
) -> Result<Vec<SkillDriftFile>, String> {
    let _ = target_root(&target)?;
    let shared_skill_path = managed_skill_path(&source, &name)?;
    let shared_dir = shared_skill_path
        .parent()
        .unwrap_or(&shared_skill_path)
        .to_path_buf();
    let local_dir = target_root(&target)?.join(safe_name(&name));
    let shared_files = skill_dir_files(&shared_dir)?;
    let local_files = if local_dir.exists() {
        skill_dir_files(&local_dir)?
    } else {
        Vec::new()
    };
    let mut result = Vec::new();
    for (relative, shared_path) in &shared_files {
        let local_entry = local_files.iter().find(|(rel, _)| rel == relative);
        if let Some((_, local_file)) = local_entry {
            if hash(&std::fs::read(shared_path).map_err(|e| e.to_string())?)
                == hash(&std::fs::read(local_file).map_err(|e| e.to_string())?)
            {
                continue;
            }
        }
        result.push(SkillDriftFile {
            path: relative.clone(),
            shared_text: read_text_optional(shared_path),
            local_text: local_entry.map(|(_, p)| read_text_optional(p)).flatten(),
        });
    }
    for (relative, local_path) in &local_files {
        if shared_files.iter().any(|(rel, _)| rel == relative) {
            continue;
        }
        result.push(SkillDriftFile {
            path: relative.clone(),
            shared_text: None,
            local_text: read_text_optional(local_path),
        });
    }
    result.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(result)
}

/// 采纳目标 Agent 的本地副本后，向其他已装备 Agent 同步的结果。
#[derive(Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub struct SkillAdoptResult {
    pub item: SkillItem,
    pub adopted_agent: String,
    /// 成功用新共享版本覆盖的 Agent（本地此前无修改）。
    pub synced: Vec<String>,
    /// 因自身本地副本也有修改而被跳过、未覆盖的 Agent。
    pub skipped: Vec<String>,
}

fn read_text_optional(path: &Path) -> Option<String> {
    std::fs::read(path)
        .ok()
        .and_then(|bytes| String::from_utf8(bytes).ok())
}

/// 采纳目标 Agent 的本地副本：快照共享库当前版本后，用本地内容整体替换
/// 共享库（版本号+1，status 与装备关系保留），并同步 SQLite 缓存。随后
/// 向其他已装备 Agent 推送新版本，但**只覆盖那些本地副本与采纳前共享库一致
/// 的 Agent**；本地自己也改过的 Agent 会被跳过，避免覆盖其未采纳的改动。
#[tauri::command]
pub async fn skill_adopt_local(
    target: String,
    source: String,
    name: String,
) -> Result<SkillAdoptResult, String> {
    let _ = target_root(&target)?;
    let local_dir = target_root(&target)?.join(safe_name(&name));
    if !local_dir.join("SKILL.md").is_file() {
        return Err(format!(
            "目标 Agent 本地不存在该 Skill：{}",
            local_dir.display()
        ));
    }
    let shared_skill_path = managed_skill_path(&source, &name)?;
    let shared_dir = shared_skill_path
        .parent()
        .unwrap_or(&shared_skill_path)
        .to_path_buf();
    let item = parse_skill(&shared_skill_path, &source)?;
    let mut manifest = read_manifest(&shared_skill_path, &item.hash);
    // 采纳前共享库的哈希就是「旧基线」。其他 Agent 本地等于基线即未改过。
    let baseline_hash = item.hash.clone();
    let next_hash = dir_hash(&local_dir)?;
    if next_hash == baseline_hash {
        return Err("本地副本与共享库内容一致，无需采纳".into());
    }
    // 与 skill_scan 相同的版本化流程：旧内容整目录快照 → 新内容就位 → 版本+1。
    let snapshot = version_root(&shared_skill_path).join(format!(
        "v{}-{}",
        manifest.version,
        &baseline_hash[..12]
    ));
    copy_dir_contents(&shared_dir, &snapshot)?;
    replace_contents_within(&local_dir, &shared_dir)?;
    manifest.version += 1;
    manifest.current_hash = next_hash;
    manifest.updated_at = chrono::Utc::now().to_rfc3339();
    write_manifest(&shared_skill_path, &manifest)?;
    let item = managed_skill_item(&source, &name)?;
    cache_upsert_skill(&item);

    // 向其他已装备 Agent 推送新版本：本地等于基线 → 安全更新；本地不等于基线
    // → 该 Agent 自己也改过，跳过，留给用户单独核对，绝不静默覆盖。
    let mut synced: Vec<String> = Vec::new();
    let mut skipped: Vec<String> = Vec::new();
    for agent in &item.assigned_agents {
        if agent == &target || agent == &source {
            continue;
        }
        let dest = match target_root(agent) {
            Ok(root) => root.join(safe_name(&name)),
            Err(_) => {
                skipped.push(agent.clone());
                continue;
            }
        };
        let local_unmodified = dest.join("SKILL.md").is_file()
            && dir_hash(&dest).map(|h| h == baseline_hash).unwrap_or(false);
        if !local_unmodified {
            skipped.push(agent.clone());
            continue;
        }
        match deploy_shared_skill(agent, &shared_dir, &dest) {
            Ok(()) => synced.push(agent.clone()),
            Err(_) => skipped.push(agent.clone()),
        }
    }
    Ok(SkillAdoptResult {
        item,
        adopted_agent: target,
        synced,
        skipped,
    })
}

/// 单条覆盖部署：把共享库里某个 Skill 部署到目标 Agent（本地被改后丢弃
/// Agent 改动、恢复为共享库版本）。不牵连同一 Agent 的其他待更新项。
#[tauri::command]
pub async fn skill_sync_apply_one(
    target: String,
    source: String,
    name: String,
) -> Result<SkillItem, String> {
    let shared_skill_path = managed_skill_path(&source, &name)?;
    let shared_dir = shared_skill_path
        .parent()
        .unwrap_or(&shared_skill_path)
        .to_path_buf();
    let dest = target_root(&target)?.join(safe_name(&name));
    deploy_shared_skill(&target, &shared_dir, &dest)?;
    let item = managed_skill_item(&source, &name)?;
    cache_upsert_skill(&item);
    Ok(item)
}

/// 已发布 Skill 在某个已装备 Agent 本地的同步状态。
#[derive(Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub struct SkillDriftAgent {
    pub agent: String,
    /// "in_sync" | "modified" | "missing"
    pub state: String,
    /// modified 状态下两侧有差异（含仅一侧存在）的文件数。
    pub changed_files: usize,
}

/// 一个已发布 Skill 在其全部已装备 Agent 上的漂移总览，供「已发布」页仲裁。
#[derive(Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub struct SkillPublishedDrift {
    pub source: String,
    pub name: String,
    pub agents: Vec<SkillDriftAgent>,
}

/// 汇总所有已发布 Skill 在各已装备 Agent 本地的漂移状态：本地无副本 →
/// missing；哈希与共享库不一致 → modified（附差异文件数）；否则 in_sync。
/// 这是「已发布」页版本仲裁的数据来源：任何 Agent 的本地修改都在此浮现。
#[tauri::command]
pub async fn skill_published_drift() -> Result<Vec<SkillPublishedDrift>, String> {
    let mut result = Vec::new();
    for item in shared_skills()?
        .into_iter()
        .filter(|s| s.status == "published")
    {
        let shared_dir = PathBuf::from(&item.path)
            .parent()
            .unwrap_or(Path::new(&item.path))
            .to_path_buf();
        let shared_files = skill_dir_files(&shared_dir)?;
        let mut agents = Vec::new();
        for agent in &item.assigned_agents {
            if agent == &item.source {
                continue;
            }
            let Ok(root) = target_root(agent) else {
                continue;
            };
            let local_dir = root.join(safe_name(&item.name));
            if !local_dir.join("SKILL.md").is_file() {
                agents.push(SkillDriftAgent {
                    agent: agent.clone(),
                    state: "missing".into(),
                    changed_files: 0,
                });
                continue;
            }
            let local_hash = dir_hash(&local_dir)?;
            if local_hash == item.hash {
                agents.push(SkillDriftAgent {
                    agent: agent.clone(),
                    state: "in_sync".into(),
                    changed_files: 0,
                });
                continue;
            }
            let local_files = skill_dir_files(&local_dir)?;
            let changed = shared_files
                .iter()
                .filter(
                    |(rel, shared_path)| match local_files.iter().find(|(r, _)| r == rel) {
                        Some((_, local_file)) => {
                            let a = std::fs::read(shared_path)
                                .map(|b| hash(&b))
                                .unwrap_or_default();
                            let b = std::fs::read(local_file)
                                .map(|b| hash(&b))
                                .unwrap_or_default();
                            a != b
                        }
                        None => true,
                    },
                )
                .count()
                + local_files
                    .iter()
                    .filter(|(rel, _)| !shared_files.iter().any(|(r, _)| r == rel))
                    .count();
            agents.push(SkillDriftAgent {
                agent: agent.clone(),
                state: "modified".into(),
                changed_files: changed,
            });
        }
        result.push(SkillPublishedDrift {
            source: item.source,
            name: item.name,
            agents,
        });
    }
    Ok(result)
}

fn managed_skill_path(source: &str, name: &str) -> Result<PathBuf, String> {
    let allowed_source = is_managed_skill_source(source);
    let safe = safe_name(name);
    if !allowed_source || safe != name || safe.is_empty() {
        return Err("invalid shared skill identifier".into());
    }
    let path = shared_root().join(source).join(&safe).join("SKILL.md");
    if !path.exists() {
        return Err("shared skill not found".into());
    }
    Ok(path)
}

/// Import a reviewed public marketplace package into the same managed registry
/// used by locally discovered Skills. The caller downloads into an isolated
/// temporary directory; this function only accepts the two curated namespaces.
pub(crate) fn import_marketplace_skill(
    namespace: &str,
    expected_name: &str,
    source_dir: &Path,
) -> Result<SkillItem, String> {
    if !matches!(namespace, "market-openai" | "market-anthropic") {
        return Err("unsupported marketplace source".into());
    }
    let safe = safe_name(expected_name);
    if safe.is_empty() || safe != expected_name {
        return Err("invalid marketplace skill name".into());
    }
    let source_skill = source_dir.join("SKILL.md");
    if !source_skill.is_file() {
        return Err("marketplace package does not contain SKILL.md".into());
    }
    let parsed = parse_skill(&source_skill, namespace)?;
    if parsed.name != expected_name {
        return Err(format!(
            "marketplace package name mismatch: expected {expected_name}, got {}",
            parsed.name
        ));
    }

    let target_dir = shared_root().join(namespace).join(expected_name);
    let target_skill = target_dir.join("SKILL.md");
    let next_hash = dir_hash(source_dir)?;
    let mut manifest = if target_skill.is_file() {
        let previous_hash = dir_hash(&target_dir)?;
        let mut previous = read_manifest(&target_skill, &previous_hash);
        if previous_hash != next_hash {
            let snapshot = version_root(&target_skill).join(format!(
                "v{}-{}",
                previous.version,
                &previous_hash[..12]
            ));
            copy_dir_contents(&target_dir, &snapshot)?;
            previous.version += 1;
            previous.status = "draft".into();
            previous.current_hash = next_hash.clone();
            previous.updated_at = chrono::Utc::now().to_rfc3339();
        }
        previous
    } else {
        default_manifest(&next_hash)
    };
    manifest.current_hash = next_hash;
    replace_contents_within(source_dir, &target_dir)?;
    write_manifest(&target_skill, &manifest)?;
    let item = managed_skill_item(namespace, expected_name)?;
    let _ = refresh_skill_cache()?;
    Ok(item)
}

fn managed_skill_item(source: &str, name: &str) -> Result<SkillItem, String> {
    let path = managed_skill_path(source, name)?;
    Ok(with_manifest(parse_skill(&path, source)?, &path))
}

/// 把某个 Skill 的发布状态写入 manifest（不含缓存刷新，供单条/批量共用）。
fn apply_status(source: &str, name: &str, status: &str) -> Result<SkillItem, String> {
    let path = managed_skill_path(source, name)?;
    let item = parse_skill(&path, source)?;
    let mut manifest = read_manifest(&path, &item.hash);
    manifest.status = status.to_string();
    manifest.updated_at = chrono::Utc::now().to_rfc3339();
    write_manifest(&path, &manifest)?;
    managed_skill_item(source, name)
}

/// 把某个 Skill 对目标 Agent 的装备状态写入 manifest（不含缓存刷新）。
fn apply_assignment(
    source: &str,
    name: &str,
    target: &str,
    equipped: bool,
) -> Result<SkillItem, String> {
    let path = managed_skill_path(source, name)?;
    let item = parse_skill(&path, source)?;
    let mut manifest = read_manifest(&path, &item.hash);
    if equipped {
        if !manifest
            .assigned_agents
            .iter()
            .any(|agent| agent.as_str() == target)
        {
            manifest.assigned_agents.push(target.to_string());
            manifest.assigned_agents.sort();
        }
    } else {
        manifest
            .assigned_agents
            .retain(|agent| agent.as_str() != target);
    }
    manifest.updated_at = chrono::Utc::now().to_rfc3339();
    write_manifest(&path, &manifest)?;
    managed_skill_item(source, name)
}

#[tauri::command]
pub async fn skill_set_status(
    source: String,
    name: String,
    status: String,
) -> Result<SkillItem, String> {
    if !matches!(status.as_str(), "draft" | "published") {
        return Err("skill status must be draft or published".into());
    }
    let item = apply_status(&source, &name, &status)?;
    cache_upsert_skill(&item);
    Ok(item)
}

#[tauri::command]
pub async fn skill_set_assignment(
    source: String,
    name: String,
    target: String,
    equipped: bool,
) -> Result<SkillItem, String> {
    let _ = target_root(&target)?;
    let item = apply_assignment(&source, &name, &target, equipped)?;
    cache_upsert_skill(&item);
    Ok(item)
}

/// 批量操作的一组 Skill 引用（source + name）。
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct SkillBulkRef {
    pub source: String,
    pub name: String,
}

#[tauri::command]
pub async fn skill_set_status_bulk(
    items: Vec<SkillBulkRef>,
    status: String,
) -> Result<Vec<SkillItem>, String> {
    if !matches!(status.as_str(), "draft" | "published") {
        return Err("skill status must be draft or published".into());
    }
    let mut updated = Vec::new();
    for item in &items {
        match apply_status(&item.source, &item.name, &status) {
            Ok(skill) => updated.push(skill),
            Err(error) => eprintln!(
                "[skill] 批量设置状态跳过 {}:{}：{error}",
                item.source, item.name
            ),
        }
    }
    if updated.is_empty() && !items.is_empty() {
        return Err("没有成功更新任何 Skill".into());
    }
    let _ = refresh_skill_cache()?;
    Ok(updated)
}

#[tauri::command]
pub async fn skill_set_assignment_bulk(
    items: Vec<SkillBulkRef>,
    target: String,
    equipped: bool,
) -> Result<Vec<SkillItem>, String> {
    let _ = target_root(&target)?;
    let mut updated = Vec::new();
    for item in &items {
        match apply_assignment(&item.source, &item.name, &target, equipped) {
            Ok(skill) => updated.push(skill),
            Err(error) => eprintln!(
                "[skill] 批量装备跳过 {}:{}：{error}",
                item.source, item.name
            ),
        }
    }
    if updated.is_empty() && !items.is_empty() {
        return Err("没有成功更新任何 Skill".into());
    }
    let _ = refresh_skill_cache()?;
    Ok(updated)
}

/// 一步完成「发布 + 设置装备目标」：把 Skill 置为 published，并整体替换其
/// 装备目标列表（来自发布对话框的勾选）。传入的 Agent 会过滤去重。
#[tauri::command]
pub async fn skill_publish(
    source: String,
    name: String,
    assigned_agents: Vec<String>,
) -> Result<SkillItem, String> {
    let path = managed_skill_path(&source, &name)?;
    let item = parse_skill(&path, &source)?;
    let mut manifest = read_manifest(&path, &item.hash);
    manifest.status = "published".to_string();
    let mut targets: Vec<String> = assigned_agents
        .into_iter()
        .filter(|agent| crate::agent_sources::is_supported_source(agent))
        .collect();
    targets.sort();
    targets.dedup();
    manifest.assigned_agents = targets;
    manifest.updated_at = chrono::Utc::now().to_rfc3339();
    write_manifest(&path, &manifest)?;
    let item = managed_skill_item(&source, &name)?;
    cache_upsert_skill(&item);
    Ok(item)
}

#[tauri::command]
pub async fn skill_rollback_latest(source: String, name: String) -> Result<SkillItem, String> {
    let path = managed_skill_path(&source, &name)?;
    let skill_dir = path.parent().unwrap_or(&path).to_path_buf();
    // 兼容两类快照：新版整目录 `v{n}-{hash}` 与旧版单文件 `v{n}-{hash}.md`。
    // 按文件名提取的版本号排序取最新，避免 `v10-…` 的字典序陷阱。
    let mut snapshots = std::fs::read_dir(version_root(&path))
        .map_err(|_| "该 Skill 尚无可回滚版本".to_string())?
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| {
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            name.starts_with('v')
                && (path.is_dir() || path.extension().and_then(|e| e.to_str()) == Some("md"))
        })
        .map(|path| {
            let stem = path.file_stem().and_then(|n| n.to_str()).unwrap_or("");
            let version: u64 = stem
                .strip_prefix('v')
                .and_then(|rest| rest.split('-').next())
                .and_then(|v| v.parse().ok())
                .unwrap_or(0);
            (version, path)
        })
        .collect::<Vec<_>>();
    snapshots.sort_by_key(|(version, _)| *version);
    let (_, snapshot) = snapshots.pop().ok_or("该 Skill 尚无可回滚版本")?;
    let current_hash = dir_hash(&skill_dir)?;
    let mut manifest = read_manifest(&path, &current_hash);
    if snapshot.is_dir() {
        // 先把当前内容备份为新版本目录，再就地把快照内容写回（保留
        // manifest.json 与 .versions/）。
        let backup =
            version_root(&path).join(format!("v{}-{}", manifest.version, &current_hash[..12]));
        copy_dir_contents(&skill_dir, &backup)?;
        replace_contents_within(&snapshot, &skill_dir)?;
    } else {
        let previous = std::fs::read(&snapshot).map_err(|e| e.to_string())?;
        let current = std::fs::read(&path).map_err(|e| e.to_string())?;
        let backup =
            version_root(&path).join(format!("v{}-{}.md", manifest.version, &current_hash[..12]));
        std::fs::create_dir_all(version_root(&path)).map_err(|e| e.to_string())?;
        std::fs::write(backup, current).map_err(|e| e.to_string())?;
        write_safely(&path, &previous)?;
    }
    manifest.version += 1;
    manifest.status = "draft".into();
    manifest.current_hash = dir_hash(&skill_dir)?;
    manifest.updated_at = chrono::Utc::now().to_rfc3339();
    write_manifest(&path, &manifest)?;
    let item = managed_skill_item(&source, &name)?;
    cache_upsert_skill(&item);
    Ok(item)
}

/// 删除共享库中的一个 Skill：整个目录（SKILL.md、附属脚本、manifest、
/// 历史快照）从 shared-skills 中移除。已同步到目标 Agent 的副本不受影响。
#[tauri::command]
pub async fn skill_delete(source: String, name: String) -> Result<(), String> {
    let path = managed_skill_path(&source, &name)?;
    let skill_dir = path.parent().unwrap_or(&path).to_path_buf();
    std::fs::remove_dir_all(&skill_dir)
        .map_err(|e| format!("删除 {} 失败：{e}", skill_dir.display()))?;
    // 缓存同步移除该条目，避免 skill_list 仍返回已删除的 Skill。
    if let Some(store) = crate::telemetry_store::shared_store() {
        if let Some(mut skills) = store.app_setting_get::<Vec<SkillItem>>(SKILL_CATALOG_SETTING_KEY)
        {
            let before = skills.len();
            skills.retain(|s| !(s.source == source && s.name == name));
            if skills.len() != before {
                let _ = store.app_setting_set(SKILL_CATALOG_SETTING_KEY, &skills);
            }
        }
    }
    Ok(())
}

fn build_preview(target: &str) -> Result<SkillSyncPreview, String> {
    let destination_root = target_root(target)?;
    let mut create = Vec::new();
    let mut update = Vec::new();
    let mut unchanged = Vec::new();
    let mut conflict = Vec::new();
    let mut missing = Vec::new();
    let mut seen: HashMap<String, String> = HashMap::new();
    for skill in shared_skills()? {
        // A target's own skills are already present; the registry is for
        // sharing skills across harnesses, not copying a file onto itself.
        if skill.source == target || skill.status != "published" {
            continue;
        }
        let assigned = skill.assigned_agents.iter().any(|agent| agent == target);
        if !assigned {
            // 已发布但未装备到该 target：记为「缺失」，便于在部署状态里
            // 核对出"本应部署却没装备"的 Skill，部署时一并补齐。
            missing.push(skill);
            continue;
        }
        if let Some(existing_source) = seen.insert(skill.name.clone(), skill.source.clone()) {
            if existing_source != skill.source {
                conflict.push(skill);
                continue;
            }
        }
        let dest = destination_root.join(&skill.name);
        if !dest.join("SKILL.md").exists() {
            create.push(skill);
        } else {
            // 目录级比较：附属脚本/资源变化也会体现为 update。
            let existing_hash = dir_hash(&dest)?;
            if existing_hash == skill.hash {
                unchanged.push(skill);
            } else {
                update.push(skill);
            }
        }
    }
    Ok(SkillSyncPreview {
        target: target.to_string(),
        create,
        update,
        unchanged,
        conflict,
        missing,
    })
}

fn write_safely(path: &Path, content: &[u8]) -> Result<(), String> {
    let parent = path.parent().ok_or("skill path has no parent")?;
    std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    let temp = path.with_extension("md.tmp");
    std::fs::write(&temp, content).map_err(|e| e.to_string())?;
    // Windows does not allow `rename` to replace an existing destination.
    // The temporary file is fully written before the requested replacement is
    // made, so a failed write never corrupts the prior skill.
    if path.exists() {
        std::fs::remove_file(path).map_err(|e| e.to_string())?;
    }
    std::fs::rename(&temp, path).map_err(|e| e.to_string())
}

#[cfg(test)]
mod origin_tests {
    use super::*;
    use std::path::PathBuf;

    /// 测试夹具按 Windows 风格书写（真实数据形态），在非 Windows 运行器
    /// （CI 的 Linux/macOS）上重组为当前平台的分隔符——否则整串是单个
    /// path 组件，组件级分类逻辑无从匹配。
    fn fixture(windows_style: &str) -> PathBuf {
        let rest = windows_style.trim_start_matches(r"C:\");
        let mut path = PathBuf::from(if cfg!(windows) { r"C:\" } else { "/" });
        for part in rest.split('\\') {
            path.push(part);
        }
        path
    }

    #[test]
    fn classifies_builtin_plugin_local_marketplace() {
        let wb_builtin = fixture(
            r"C:\u\.workbuddy\plugins\cache\workbuddy-builtin\tencent-docx\5.6.2-wb.1\skills\html-review\SKILL.md",
        );
        assert_eq!(classify_origin("workbuddy", &wb_builtin), "builtin");

        let wb_installed = fixture(
            r"C:\u\.workbuddy\plugins\cache\codebuddy-plugins-official\foo\1.0.0\skills\bar\SKILL.md",
        );
        assert_eq!(classify_origin("workbuddy", &wb_installed), "plugin");

        let qoder_builtin = fixture(r"C:\u\.qoder\plugins\cache\qoder-bundler\p\1.0\skills\s\SKILL.md");
        assert_eq!(classify_origin("qoder", &qoder_builtin), "builtin");

        let minimax_builtin = fixture(r"C:\u\.minimax\.builtin-skills\code-review\SKILL.md");
        assert_eq!(classify_origin("minimax", &minimax_builtin), "builtin");

        let minimax_plugin = fixture(r"C:\u\.minimax\plugins\some-plugin\skills\s\SKILL.md");
        assert_eq!(classify_origin("minimax", &minimax_plugin), "plugin");

        let local = fixture(r"C:\u\.claude\skills\hyperframes\SKILL.md");
        assert_eq!(classify_origin("claude", &local), "local");

        let market = fixture(r"C:\tmp\download\docx\SKILL.md");
        assert_eq!(classify_origin("market-anthropic", &market), "marketplace");
    }

    #[test]
    fn detects_version_artifacts_and_managed_dirs() {
        let version_root = fixture(
            r"C:\u\.workbuddy\plugins\cache\workbuddy-builtin\tencent-docx\5.6.2-wb.39298511.g37a65c0b.he233403f909a\SKILL.md",
        );
        let name = plugin_version_artifact(&version_root).expect("version root is an artifact");
        assert_eq!(name, "5-6-2-wb-39298511-g37a65c0b-he233403f909a");

        let real_skill = fixture(
            r"C:\u\.workbuddy\plugins\cache\workbuddy-builtin\tencent-docx\5.6.2-wb.1\skills\html-review\SKILL.md",
        );
        assert!(plugin_version_artifact(&real_skill).is_none());

        let managed = fixture(
            r"C:\u\.kimi\plugins\managed\agent-manager-skills\skills\docx\SKILL.md",
        );
        assert!(is_agent_manager_managed(&managed));
        assert!(!is_agent_manager_managed(&real_skill));
    }
}
