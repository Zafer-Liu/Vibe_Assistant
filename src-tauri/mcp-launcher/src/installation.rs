//! Install the tiny sidecar at a versioned, stable path for Agent MCP configs.
//! A new app version never overwrites a binary still held open by an Agent.
use sha2::{Digest, Sha256};
use std::io::Write;
use std::path::PathBuf;

pub fn launcher_executable() -> Result<String, String> {
    install_sidecar("mcp-launcher", "mcp-launchers", "MCP fallback launcher")
}

/// The observed stdio relay sidecar. It is installed under its own hashed name
/// so terminating the GUI `agent-manager` process cannot take active relays
/// down with it.
pub fn relay_executable() -> Result<String, String> {
    install_sidecar("mcp-relay", "mcp-relays", "MCP relay")
}

fn install_sidecar(base: &str, subdir: &str, label: &str) -> Result<String, String> {
    let name = if cfg!(windows) { format!("{base}.exe") } else { base.to_string() };
    let current = std::env::current_exe().map_err(|_| "无法定位应用可执行文件")?;
    let sibling = current.parent().ok_or("应用可执行文件目录无效")?.join(&name);
    let mut candidates = vec![sibling];
    if cfg!(debug_assertions) {
        let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        // mcp-relay is a bin of the main app package; mcp-launcher of its own.
        let (binaries_dir, target_dir) = if base == "mcp-relay" {
            (manifest_dir.join("binaries"), manifest_dir.join("target").join("debug"))
        } else {
            (manifest_dir.join("binaries"), manifest_dir.join("mcp-launcher").join("target").join("debug"))
        };
        // The dev build script refreshes these copies after building both
        // sidecars. Tauri's executable directory can retain an older copy (or
        // the relay bootstrap placeholder), so it must not shadow that output.
        candidates.insert(0, binaries_dir.join(&name));
        candidates.push(target_dir.join(&name));
    }
    let bundled = candidates.into_iter().find(|path| path.is_file())
        .ok_or_else(|| format!("{label} 不存在，请先构建 {base}"))?;
    let bytes = std::fs::read(&bundled).map_err(|_| format!("无法读取 {label}"))?;
    let digest = format!("{:x}", Sha256::digest(&bytes));
    let install_dir = dirs_next::config_dir().ok_or("无法定位应用数据目录")?
        .join("agent-manager").join(subdir);
    std::fs::create_dir_all(&install_dir).map_err(|_| format!("无法创建 {label} 目录"))?;
    let file_name = if cfg!(windows) {
        format!("{base}-{digest}.exe")
    } else {
        format!("{base}-{digest}")
    };
    let installed = install_dir.join(file_name);
    if installed.is_file() {
        let existing = std::fs::read(&installed).map_err(|_| format!("无法验证 {label}"))?;
        if Sha256::digest(&existing).as_slice() != Sha256::digest(&bytes).as_slice() {
            return Err(format!("{label} 文件校验失败"));
        }
    } else {
        let mut temporary = tempfile::NamedTempFile::new_in(&install_dir)
            .map_err(|_| format!("无法准备 {label} 文件"))?;
        temporary.write_all(&bytes).map_err(|_| format!("无法写入 {label} 文件"))?;
        temporary.as_file().set_permissions(std::fs::metadata(&bundled)
            .map_err(|_| format!("无法读取 {label} 权限"))?.permissions())
            .map_err(|_| format!("无法设置 {label} 权限"))?;
        temporary.as_file().sync_all().map_err(|_| format!("无法同步 {label} 文件"))?;
        match temporary.persist_noclobber(&installed) {
            Ok(_) => {}
            Err(error) if error.error.kind() == std::io::ErrorKind::AlreadyExists => {
                let existing = std::fs::read(&installed).map_err(|_| format!("无法验证 {label}"))?;
                if Sha256::digest(&existing).as_slice() != Sha256::digest(&bytes).as_slice() {
                    return Err(format!("{label} 文件校验失败"));
                }
            }
            Err(_) => return Err(format!("无法安装 {label}")),
        }
    }
    let absolute = installed.canonicalize().map_err(|_| format!("无法定位 {label} 安装路径"))?;
    Ok(absolute.to_string_lossy().into_owned())
}
