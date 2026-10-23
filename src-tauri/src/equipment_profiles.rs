//! Named reference bundles. Configuration and secrets stay in their existing registries.
use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::sync::LazyLock;
static OPERATIONS: LazyLock<tokio::sync::Mutex<()>> = LazyLock::new(|| tokio::sync::Mutex::new(()));
const KEY: &str = "equipment.profiles.v1";
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct SkillRef {
    pub source: String,
    pub name: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EquipmentProfile {
    pub name: String,
    pub servers: Vec<String>,
    pub skills: Vec<SkillRef>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PlanItem {
    pub kind: String,
    pub source: String,
    pub name: String,
    pub action: String,
    pub revision: String,
}
#[derive(Serialize)]
pub struct ProfilePlan {
    pub profile: EquipmentProfile,
    pub target: String,
    pub revision: String,
    pub items: Vec<PlanItem>,
}
#[derive(Clone, Serialize, Deserialize)]
pub struct ApplyItem {
    pub kind: String,
    pub name: String,
    pub state: String,
    pub detail: String,
}
#[derive(Clone, Serialize, Deserialize)]
pub struct ApplyLog {
    pub id: String,
    pub profile: String,
    pub target: String,
    pub at: String,
    pub items: Vec<ApplyItem>,
}
fn read<T: serde::de::DeserializeOwned + Default>(key: &str) -> Result<T, String> {
    let store = crate::telemetry_store::shared_store().ok_or("本地数据库不可用")?;
    let conn = store.conn.lock().map_err(|_| "数据库锁不可用")?;
    let value: Option<String> = conn
        .query_row(
            "SELECT value_json FROM app_settings WHERE setting_key=?1",
            [key],
            |r| r.get(0),
        )
        .optional()
        .map_err(|e| e.to_string())?;
    value
        .map(|v| serde_json::from_str(&v).map_err(|e| e.to_string()))
        .unwrap_or_else(|| Ok(T::default()))
}
fn save<T: Serialize>(key: &str, value: &T) -> Result<(), String> {
    let store = crate::telemetry_store::shared_store().ok_or("本地数据库不可用")?;
    let conn = store.conn.lock().map_err(|_| "数据库锁不可用")?;
    conn.execute("INSERT INTO app_settings(setting_key,value_json,updated_at) VALUES(?1,?2,?3) ON CONFLICT(setting_key) DO UPDATE SET value_json=excluded.value_json,updated_at=excluded.updated_at",params![key,serde_json::to_string(value).map_err(|e|e.to_string())?,chrono::Utc::now().to_rfc3339()]).map_err(|e|e.to_string())?;
    Ok(())
}
fn hash<T: Serialize>(value: &T) -> Result<String, String> {
    Ok(format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(value).map_err(|e| e.to_string())?)
    ))
}
fn normalize(mut p: EquipmentProfile) -> Result<EquipmentProfile, String> {
    p.name = p.name.trim().into();
    if p.name.is_empty() || p.name.chars().count() > 64 || p.name.chars().any(char::is_control) {
        return Err("方案名称需为 1–64 个可见字符".into());
    }
    if p.servers.len() + p.skills.len() > 100 {
        return Err("每个方案最多 100 项".into());
    }
    p.servers.sort();
    p.servers.dedup();
    p.skills
        .sort_by(|a, b| (&a.source, &a.name).cmp(&(&b.source, &b.name)));
    p.skills.dedup();
    if p.servers.is_empty() && p.skills.is_empty() {
        return Err("至少选择一个 MCP 或 Skill".into());
    }
    if p.skills
        .iter()
        .map(|s| &s.name)
        .collect::<std::collections::HashSet<_>>()
        .len()
        != p.skills.len()
    {
        return Err("同名 Skill 不能来自多个版本，请先选择一个来源".into());
    }
    Ok(p)
}
fn skill_target(target: &str) -> Result<&str, String> {
    match target {
        "claude_cli" => Ok("claude"),
        "codex_cli" => Ok("codex"),
        "qoder" | "workbuddy" | "minimax" | "kimi" | "zcode" | "dsh" => Ok(target),
        _ => Err("该目标不支持 MCP 与 Skill 联合装备".into()),
    }
}
fn installed(target: &str) -> Result<(), String> {
    if target == "dsh" {
        // DSH 是桌面应用而非 CLI：以 desktop profile 目录存在为准。
        let present = crate::dsh_mcp::patch_path()
            .is_some_and(|path| path.parent().is_some_and(|dir| dir.is_dir()));
        if !present {
            return Err("未检测到 DeepSeek Harness（~/.dsh/profiles/desktop），请先安装".into());
        }
        return Ok(());
    }
    let skill = skill_target(target)?;
    if crate::agent_cli::resolve_installed_agent_cli(skill).is_none() {
        return Err("未检测到目标 CLI，请先安装或配置程序路径".into());
    }
    Ok(())
}
async fn available_item(
    kind: &str,
    source: &str,
    name: &str,
    target: &str,
) -> Result<PlanItem, String> {
    if kind == "mcp" {
        let comparison = crate::mcp_registry::mcp_configuration_compare(name.into()).await?;
        let actual = comparison
            .sources
            .iter()
            .find(|s| s.agent == target)
            .ok_or("无法读取目标配置")?;
        let action = if comparison.catalog.is_none() || comparison.blocked_reason.is_some() {
            "blocked"
        } else {
            match actual.state.as_str() {
                "missing" => "add",
                "installed" => "keep",
                _ => "conflict",
            }
        };
        Ok(PlanItem {
            kind: kind.into(),
            source: String::new(),
            name: name.into(),
            action: action.into(),
            revision: comparison.revision,
        })
    } else {
        let (action, revision) =
            crate::skill_registry::equipment_skill_state(source, name, skill_target(target)?)?;
        Ok(PlanItem {
            kind: kind.into(),
            source: source.into(),
            name: name.into(),
            action,
            revision,
        })
    }
}
async fn item(kind: &str, source: &str, name: &str, target: &str) -> Result<PlanItem, String> {
    match available_item(kind, source, name, target).await {
        Ok(item) => Ok(item),
        Err(_) => Ok(PlanItem {
            kind: kind.into(),
            source: source.into(),
            name: name.into(),
            action: "blocked".into(),
            revision: hash(&(kind, source, name, target, "unavailable"))?,
        }),
    }
}
async fn plan(name: &str, target: &str) -> Result<ProfilePlan, String> {
    installed(target)?;
    let profiles: Vec<EquipmentProfile> = read(KEY)?;
    let profile = profiles
        .into_iter()
        .find(|p| p.name == name)
        .ok_or("方案不存在")?;
    let mut items = Vec::new();
    for name in &profile.servers {
        items.push(item("mcp", "", name, target).await?);
    }
    for skill in &profile.skills {
        items.push(item("skill", &skill.source, &skill.name, target).await?);
    }
    let revision = hash(&(&profile, target, &items))?;
    Ok(ProfilePlan {
        profile,
        target: target.into(),
        revision,
        items,
    })
}
#[tauri::command]
pub async fn equipment_profile_list() -> Result<Vec<EquipmentProfile>, String> {
    read(KEY)
}
#[tauri::command]
pub async fn equipment_profile_save(profile: EquipmentProfile) -> Result<EquipmentProfile, String> {
    let _guard = OPERATIONS.lock().await;
    let profile = normalize(profile)?;
    let mut profiles: Vec<EquipmentProfile> = read(KEY)?;
    profiles.retain(|p| p.name != profile.name);
    profiles.push(profile.clone());
    save(KEY, &profiles)?;
    Ok(profile)
}
#[tauri::command]
pub async fn equipment_profile_delete(name: String) -> Result<(), String> {
    let _guard = OPERATIONS.lock().await;
    let mut profiles: Vec<EquipmentProfile> = read(KEY)?;
    profiles.retain(|p| p.name != name);
    save(KEY, &profiles)
}
#[tauri::command]
pub async fn equipment_profile_preview(
    name: String,
    target: String,
) -> Result<ProfilePlan, String> {
    plan(&name, &target).await
}
#[tauri::command]
pub async fn equipment_profile_logs() -> Result<Vec<ApplyLog>, String> {
    read("equipment.logs.v1")
}
fn journal(log: &ApplyLog) -> Result<(), String> {
    let mut logs: Vec<ApplyLog> = read("equipment.logs.v1")?;
    logs.retain(|l| l.id != log.id);
    logs.insert(0, log.clone());
    logs.truncate(50);
    save("equipment.logs.v1", &logs)
}
#[tauri::command]
pub async fn equipment_profile_apply(
    name: String,
    target: String,
    revision: String,
) -> Result<ApplyLog, String> {
    let _guard = OPERATIONS.lock().await;
    let plan = plan(&name, &target).await?;
    if plan.revision != revision {
        return Err("配置或方案已变化，请重新预览".into());
    }
    let mut log = ApplyLog {
        id: uuid::Uuid::new_v4().to_string(),
        profile: name,
        target: target.clone(),
        at: chrono::Utc::now().to_rfc3339(),
        items: plan
            .items
            .iter()
            .map(|i| ApplyItem {
                kind: i.kind.clone(),
                name: i.name.clone(),
                state: "pending".into(),
                detail: String::new(),
            })
            .collect(),
    };
    journal(&log)?;
    for (index, expected) in plan.items.iter().enumerate() {
        if !matches!(expected.action.as_str(), "add" | "keep") {
            log.items[index].state = "skipped".into();
            log.items[index].detail = "此项存在冲突或不可用，未写入；请在原库检查。".into();
            journal(&log)?;
            continue;
        }
        let result = async {
            installed(&target)?;
            let current = item(&expected.kind, &expected.source, &expected.name, &target).await?;
            if current.revision != expected.revision {
                return Err("此项配置已变化，未写入，请重新预览".into());
            }
            if !matches!(current.action.as_str(), "add" | "keep") {
                return Err("存在冲突或条目不可用；请先在原库仲裁".into());
            }
            if expected.kind == "mcp" {
                let result = crate::mcp_registry::mcp_configuration_resolve(
                    expected.name.clone(),
                    current.revision,
                    None,
                    vec![target.clone()],
                )
                .await?;
                if !result.errors.is_empty() || !result.applied_agents.contains(&target) {
                    return Err("配置写入未完成".into());
                }
            } else {
                crate::skill_registry::equipment_skill_equip(
                    &expected.source,
                    &expected.name,
                    skill_target(&target)?,
                )?;
            }
            Ok::<_, String>(())
        }
        .await;
        let entry = &mut log.items[index];
        match result {
            Ok(()) => {
                entry.state = "applied".into();
            }
            Err(_) => {
                entry.state = "failed".into();
                entry.detail = "应用未完成或配置已变化；请回到原库检查此项并重新预览。".into();
            }
        }
        journal(&log)?;
    }
    Ok(log)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn bundle_rejects_ambiguous_skills_and_normalizes_names() {
        let p = EquipmentProfile {
            name: " dev ".into(),
            servers: vec!["one".into(), "one".into()],
            skills: vec![],
        };
        let p = normalize(p).unwrap();
        assert_eq!(p.name, "dev");
        assert_eq!(p.servers.len(), 1);
        assert!(normalize(EquipmentProfile {
            name: "a".into(),
            servers: vec![],
            skills: vec![
                SkillRef {
                    source: "a".into(),
                    name: "same".into()
                },
                SkillRef {
                    source: "b".into(),
                    name: "same".into()
                }
            ]
        })
        .is_err());
    }
    #[test]
    fn revision_binds_target_and_refs() {
        let p = EquipmentProfile {
            name: "dev".into(),
            servers: vec!["a".into()],
            skills: vec![],
        };
        assert_ne!(
            hash(&(&p, "claude_cli")).unwrap(),
            hash(&(&p, "codex_cli")).unwrap()
        );
    }
}
