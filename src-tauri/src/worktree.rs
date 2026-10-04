//! Agent Git worktree 隔离（P1）。
//!
//! 配置了 `worktree_repo` 的 Agent 在启动/打开终端时惰性创建独立工作树
//! （`git worktree add`），让多个 Agent 并行修改同一仓库而互不踩踏
//! （各工作树的未提交改动彼此隔离，共享 `.git` 对象库）。
//!
//! v1 语义：
//! - 工作树位置固定为源仓库同级的 `<repo 名>.worktrees/<agent-id>` 目录
//!   （同一文件系统、紧邻用户代码，便于查找）
//! - 幂等：已注册的 worktree 直接复用（不切换分支，保留工作树当前状态）
//! - 目录已存在但未注册 → 报错拒绝，绝不覆盖非托管内容
//! - 停止/删除 Agent 不自动清理（避免误删未提交改动）；
//!   需要时手动 `git worktree remove <path>` 后 `git worktree prune`

use crate::process_util::no_window;
use std::path::{Path, PathBuf};

/// 运行 git 子进程（可选仓库目录）。成功返回 stdout；失败返回带 stderr
/// 摘要的错误（不包含环境变量等敏感信息）。
pub(crate) fn run_git(repo: Option<&Path>, args: &[&str]) -> Result<String, String> {
    let mut cmd = std::process::Command::new("git");
    if let Some(dir) = repo {
        cmd.current_dir(dir);
    }
    cmd.args(args);
    no_window(&mut cmd);
    let output = cmd
        .output()
        .map_err(|e| format!("无法启动 git: {e}。请确认 git 已安装并在 PATH 中"))?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        let action = args.first().copied().unwrap_or("");
        Err(if stderr.is_empty() {
            format!("git {action} 失败（exit code {:?}）", output.status.code())
        } else {
            format!("git {action} 失败: {stderr}")
        })
    }
}

/// agent id → 安全的目录名片段（保留字母数字、-、_，其余替换为 -）。
fn sanitize_id(agent_id: &str) -> String {
    let cleaned: String = agent_id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect();
    if cleaned.is_empty() {
        "agent".to_string()
    } else {
        cleaned
    }
}

/// worktree 目标目录：<repo 同级>/<repo 名>.worktrees/<agent-id>。
fn worktree_dest(repo: &Path, agent_id: &str) -> Result<PathBuf, String> {
    let parent = repo.parent().ok_or_else(|| format!("无法定位仓库 {} 的上级目录", repo.display()))?;
    let repo_name = repo
        .file_name()
        .ok_or_else(|| format!("无法解析仓库 {} 的名称", repo.display()))?
        .to_string_lossy();
    Ok(parent.join(format!("{repo_name}.worktrees")).join(sanitize_id(agent_id)))
}

/// 解析 `git worktree list --porcelain` 输出，判断 dest 是否已注册。
fn is_registered(porcelain: &str, dest: &Path) -> bool {
    porcelain
        .lines()
        .filter_map(|line| line.strip_prefix("worktree "))
        .any(|path| Path::new(path) == dest)
}

/// 校验分支名（git check-ref-format --branch），拒绝空名与非法字符。
fn valid_branch(repo: &Path, branch: &str) -> bool {
    if branch.trim().is_empty() {
        return false;
    }
    run_git(
        Some(repo),
        &["check-ref-format", "--branch", branch],
    )
    .is_ok()
}

/// 幂等确保 Agent 的独立工作树存在，返回其绝对路径。
///
/// 错误在 Agent 启动/终端打开时同步抛给上层（写入日志或返回给前端），
/// 不存在半创建状态：`git worktree add` 失败时 git 自行回滚。
pub(crate) fn ensure_worktree(
    agent_id: &str,
    repo: &str,
    branch: Option<&str>,
) -> Result<PathBuf, String> {
    let repo = Path::new(repo.trim());
    if repo.as_os_str().is_empty() || !repo.is_dir() {
        return Err(format!("worktree 源仓库不存在或不是目录: {}", repo.display()));
    }
    // rev-parse 校验确为 git 仓库（工作树或仓库内均可）。
    run_git(Some(repo), &["rev-parse", "--git-common-dir"])?;

    let dest = worktree_dest(repo, agent_id)?;
    let registered = is_registered(
        &run_git(Some(repo), &["worktree", "list", "--porcelain"])?,
        &dest,
    );
    if registered {
        // 已注册：直接复用。不切换分支——工作树里可能有进行中的改动。
        return Ok(dest);
    }
    if dest.exists() {
        return Err(format!(
            "{} 已存在但不是注册的 git worktree，未覆盖；请手动处理该目录后重试",
            dest.display()
        ));
    }
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("创建 worktrees 目录 {} 失败: {e}", parent.display()))?;
    }
    let dest_str = dest.to_string_lossy().to_string();

    match branch {
        Some(branch) if valid_branch(repo, branch) => {
            let exists = run_git(
                Some(repo),
                &["rev-parse", "--verify", "--quiet", &format!("refs/heads/{branch}")],
            )
            .is_ok();
            if exists {
                run_git(Some(repo), &["worktree", "add", &dest_str, branch])?;
            } else {
                // 分支不存在：自当前 HEAD 创建同名分支。
                run_git(Some(repo), &["worktree", "add", "-b", branch, &dest_str])?;
            }
        }
        Some(branch) => {
            return Err(format!("worktree 分支名无效: {branch:?}"));
        }
        None => {
            // 未指定分支：分离 HEAD（隔离快照，不占用分支名）。
            run_git(Some(repo), &["worktree", "add", "--detach", &dest_str])?;
        }
    }
    Ok(dest)
}

// ── 单元测试（纯函数，不触磁盘/子进程） ─────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dest_lives_beside_repo_named_by_agent() {
        let dest = worktree_dest(Path::new("/work/agent-manager"), "abc123").unwrap();
        assert_eq!(dest, Path::new("/work/agent-manager.worktrees/abc123"));
        let dest = worktree_dest(Path::new("/work/agent-manager"), "脏 id/斜杠").unwrap();
        assert_eq!(dest, Path::new("/work/agent-manager.worktrees/--id---"));
        assert_eq!(sanitize_id(""), "agent");
    }

    #[test]
    fn porcelain_registration_matches_dest_paths() {
        let porcelain = "worktree /work/agent-manager\nHEAD abc\n\nworktree /work/agent-manager.worktrees/abc123\nHEAD def\nbranch refs/heads/x\n";
        assert!(is_registered(porcelain, Path::new("/work/agent-manager.worktrees/abc123")));
        assert!(is_registered(porcelain, Path::new("/work/agent-manager")));
        assert!(!is_registered(porcelain, Path::new("/work/other")));
        assert!(!is_registered("", Path::new("/work/agent-manager")));
    }

    #[test]
    fn repo_without_parent_or_name_is_rejected() {
        // 相对路径也能推导（parent 为空路径段）。
        assert!(worktree_dest(Path::new("relative-repo"), "a").is_ok());
        // 根路径没有 parent 与 file_name，必须拒绝（跨平台一致）。
        assert!(worktree_dest(Path::new("/"), "a").is_err());
    }
}
