// Included in skill_registry so the shared registry remains the authority.
fn equipment_tree_hash(root: &Path) -> Result<String, String> {
    use std::io::Read;
    let canonical = std::fs::canonicalize(root).map_err(|e| e.to_string())?;
    let mut stack = vec![root.to_path_buf()];
    let mut count = 0;
    let mut total = 0u64;
    while let Some(path) = stack.pop() {
        count += 1;
        if count > 4000 {
            return Err("Skill 文件过多".into());
        }
        let meta = std::fs::symlink_metadata(&path).map_err(|e| e.to_string())?;
        if meta.file_type().is_symlink()
            || std::fs::canonicalize(&path).map_err(|e| e.to_string())?
                != canonical.join(path.strip_prefix(root).map_err(|e| e.to_string())?)
        {
            return Err("Skill 包含链接或越界路径".into());
        }
        if meta.is_dir() {
            for e in std::fs::read_dir(path).map_err(|e| e.to_string())? {
                let e = e.map_err(|e| e.to_string())?;
                if e.file_name() != ".versions" {
                    stack.push(e.path());
                }
            }
        } else if meta.is_file() {
            total += meta.len();
            if total > 32 * 1024 * 1024 {
                return Err("Skill 超过方案部署上限 32 MB".into());
            }
        } else {
            return Err("Skill 包含非普通文件".into());
        }
    }
    // Traverse only validated package files; bound reads even if a file grows.
    let mut digest = Sha256::new();
    for (relative, path) in skill_dir_files(root)? {
        let mut data = Vec::new();
        std::fs::File::open(path)
            .map_err(|e| e.to_string())?
            .take(32 * 1024 * 1024 + 1)
            .read_to_end(&mut data)
            .map_err(|e| e.to_string())?;
        if data.len() > 32 * 1024 * 1024 {
            return Err("Skill 文件过大".into());
        }
        digest.update(relative.as_bytes());
        digest.update([0]);
        digest.update(hash(&data).as_bytes());
        digest.update([0]);
    }
    Ok(format!("{:x}", digest.finalize()))
}
pub(crate) fn equipment_skill_state(
    source: &str,
    name: &str,
    target: &str,
) -> Result<(String, String), String> {
    let path = managed_skill_path(source, name)?;
    let item = managed_skill_item(source, name)?;
    let source_hash = equipment_tree_hash(path.parent().ok_or("Skill 目录无效")?)?;
    let dest = target_root(target)?.join(safe_name(name));
    let local = if std::fs::symlink_metadata(&dest).is_ok() {
        Some(equipment_tree_hash(&dest)?)
    } else {
        None
    };
    let action = if item.status != "published" {
        "blocked"
    } else if local.as_ref() == Some(&source_hash) {
        "keep"
    } else if local.is_none() {
        "add"
    } else {
        "conflict"
    };
    let revision = hash(
        &serde_json::to_vec(&(
            source_hash,
            local,
            &item.assigned_agents,
            &item.status,
            dest,
        ))
        .map_err(|e| e.to_string())?,
    );
    Ok((action.into(), revision))
}
pub(crate) fn equipment_skill_equip(source: &str, name: &str, target: &str) -> Result<(), String> {
    let (state, _) = equipment_skill_state(source, name, target)?;
    if !matches!(state.as_str(), "add" | "keep") {
        return Err("Skill 有冲突或未发布".into());
    }
    if state == "add" {
        let source_path = managed_skill_path(source, name)?;
        let dest = target_root(target)?.join(safe_name(name));
        let parent = dest.parent().ok_or("目标目录无效")?;
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        let staged = tempfile::tempdir_in(parent).map_err(|e| e.to_string())?;
        let source_dir = source_path.parent().ok_or("源目录无效")?;
        let expected = equipment_tree_hash(source_dir)?;
        copy_dir_contents(source_dir, staged.path())?;
        if equipment_tree_hash(staged.path())? != expected {
            return Err("源 Skill 在复制期间变化，未发布".into());
        }
        if std::fs::symlink_metadata(&dest).is_ok() {
            return Err("目标已变化，未覆盖".into());
        }
        std::fs::rename(staged.path(), &dest).map_err(|e| e.to_string())?;
        if target == "kimi" {
            ensure_kimi_skill_plugin()?;
        }
    }
    let item = apply_assignment(source, name, target, true)?;
    cache_upsert_skill(&item);
    Ok(())
}

#[cfg(test)]
mod equipment_package_tests {
    use super::*;
    #[test]
    fn package_identity_tracks_assets_and_ignores_registry_metadata() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("SKILL.md"), "# Review").unwrap();
        let first = equipment_tree_hash(root.path()).unwrap();
        std::fs::write(root.path().join("manifest.json"), "{}").unwrap();
        std::fs::create_dir(root.path().join(".versions")).unwrap();
        std::fs::write(root.path().join(".versions/old"), "old").unwrap();
        assert_eq!(equipment_tree_hash(root.path()).unwrap(), first);
        std::fs::write(root.path().join("script.py"), "print(1)").unwrap();
        assert_ne!(equipment_tree_hash(root.path()).unwrap(), first);
    }
}
