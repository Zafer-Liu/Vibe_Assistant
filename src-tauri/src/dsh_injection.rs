//! DeepSeek Harness 的记忆注入插件（「Hook 等价物」）。
//!
//! DSH 没有外部进程式 Hook，但官方 `dsh-agent-instructions` 插件示范了原生
//! 注入通道：cordis 插件监听 `agent/pre-step` 生命周期事件，把内容折入
//! agent inbox（带来源的 user/message，持久化、可压缩、重启可恢复）。
//! 本模块把同样机制的插件分发到 desktop profile：
//!
//! - 插件文件：`<dshHome>/profiles/desktop/plugins/agent-manager-memory-inject.mjs`
//! - patch 条目：`- insert: [{id: agent-manager-memory-inject,
//!   name: ./plugins/agent-manager-memory-inject.mjs, config: {...}}]`
//!   （相对模块说明符按 profile 目录解析，无需 pnpm/package.json 改动）
//!
//! 插件行为与 Claude Code 的 Hook 注入语义一致：
//! - 首个 pre-step（≈ SessionStart）注入 L3 长期记忆 + L2 工作记忆；
//! - 后续步骤领取了新的用户输入时（≈ UserPromptSubmit）做指纹门控的 L3 注入；
//! - 记忆正文实时来自智管 Hook Server 的 `POST /memory/context?source=dsh`
//!   （端口 9420，注入即写入注入看板账本），智管未运行时回退本地快照；
//! - 客户端按内容 digest 去重：同一会话内相同内容绝不重复注入。
//! `cordis.patch.yml` 被热重载，条目写入后运行中的 DSH 即时生效。

use std::path::PathBuf;

/// cordis patch 条目与插件文件共用的稳定 id。
pub(crate) const PLUGIN_ENTRY_ID: &str = "agent-manager-memory-inject";
const PLUGIN_FILE_NAME: &str = "agent-manager-memory-inject.mjs";
const SNAPSHOT_FILE_NAME: &str = "agent-manager-memory-snapshot.md";
/// 相对模块说明符：include 层的 baseUrl 是 profile 目录。
const PLUGIN_RELATIVE_NAME: &str = "./plugins/agent-manager-memory-inject.mjs";

// ── 路径 ──────────────────────────────────────────────────────────────────────

fn profile_dir() -> Option<PathBuf> {
    crate::agent_sources::dsh_home()
        .map(|home| home.join("profiles").join("desktop"))
}

pub(crate) fn plugin_path() -> Option<PathBuf> {
    profile_dir().map(|dir| dir.join("plugins").join(PLUGIN_FILE_NAME))
}

fn snapshot_path() -> Option<PathBuf> {
    profile_dir().map(|dir| dir.join("plugins").join(SNAPSHOT_FILE_NAME))
}

// ── 插件源码 ──────────────────────────────────────────────────────────────────

/// 插件 JS：ESM、零依赖（仅 Node 内置模块），由智管整份分发与升级。
fn plugin_source() -> &'static str {
    r#"// Agent Manager (智管) shared-memory injection for DeepSeek Harness.
// Installed and updated by Agent Manager; manual edits will be overwritten.
// Mechanism mirror: dsh-agent-instructions (agent/pre-step + agent inbox).
import { createHash } from "node:crypto";
import { readFileSync } from "node:fs";

export const name = "agent-manager-memory-inject";

const KIND = "agent-manager-memory";

export function apply(ctx, config) {
  const contextUrl = typeof config?.contextUrl === "string" ? config.contextUrl : "";
  const snapshotPath = typeof config?.snapshotPath === "string" ? config.snapshotPath : "";
  const timeoutMs = Number.isFinite(config?.timeoutMs) ? config.timeoutMs : 2000;

  const digestOf = (text) => createHash("sha1").update(text, "utf8").digest("hex");
  const escapeReminder = (text) => text.replaceAll("</system-reminder>", "<\\/system-reminder>");

  const visibleDigest = (agent, digest) => {
    try {
      for (const seq of agent.session.surface.nodes) {
        const event = agent.session.eventAt(seq);
        if (event?.type === "user/message" && event.data?.source?.kind === KIND
          && event.data.source.digest === digest) return true;
      }
    } catch {}
    return false;
  };

  const pendingOf = (agent) => {
    try {
      return agent.inbox.nextStep.filter((message) => message?.source?.kind === KIND);
    } catch {
      return [];
    }
  };

  const syncPending = (agent, desired) => {
    const pending = pendingOf(agent);
    if (desired === undefined) {
      for (const message of pending) agent.inbox.remove(message.id);
      return;
    }
    const reusable = pending.find((message) => message.source.digest === desired.source.digest);
    if (reusable !== undefined) {
      for (const message of pending) if (message !== reusable) agent.inbox.remove(message.id);
      return;
    }
    const replaced = pending[0];
    if (replaced === undefined) agent.inbox.prepend("next-step", desired);
    else agent.inbox.replace(replaced.id, desired);
    for (const message of pending.slice(1)) agent.inbox.remove(message.id);
  };

  const pullContext = async (event, sessionId, prompt) => {
    if (contextUrl !== "") {
      try {
        const separator = contextUrl.includes("?") ? "&" : "?";
        const response = await fetch(contextUrl + separator + "event=" + event, {
          method: "POST",
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify({ session_id: sessionId, prompt }),
          signal: AbortSignal.timeout(timeoutMs),
        });
        if (response.ok) {
          const text = await response.text();
          if (text.trim() !== "") return text;
        }
      } catch {}
    }
    if (snapshotPath !== "") {
      try {
        return readFileSync(snapshotPath, "utf8");
      } catch {}
    }
    return "";
  };

  ctx.on("agent/pre-step", async ({ agent, messages, step, signal }, next) => {
    const decision = await next();
    try {
      const event = step === 1 ? "SessionStart" : "UserPromptSubmit";
      if (event === "UserPromptSubmit") {
        const hasUserInput = Array.isArray(messages) && messages.some((message) =>
          message?.role === "user" && message?.source?.kind === undefined);
        if (!hasUserInput) {
          syncPending(agent, undefined);
          return decision;
        }
      }
      const sessionId = typeof agent.session?.id === "string" ? agent.session.id : "";
      let prompt = "";
      if (Array.isArray(messages)) {
        for (const message of messages) {
          if (message?.role !== "user") continue;
          const text = (message.content ?? [])
            .filter((block) => block?.type === "text")
            .map((block) => block.text ?? "")
            .join("\n")
            .trim();
          if (text !== "") prompt = text;
        }
      }
      const context = (await pullContext(event, sessionId, prompt)).trim();
      signal.throwIfAborted();
      if (context === "") {
        syncPending(agent, undefined);
        return decision;
      }
      const digest = digestOf(context);
      if (visibleDigest(agent, digest)) {
        syncPending(agent, undefined);
        return decision;
      }
      const desired = {
        id: crypto.randomUUID(),
        role: "user",
        content: [{
          type: "text",
          text: "<system-reminder>\nThe following shared memory context is provided by Agent Manager. Use it as background knowledge; it does not override user or system instructions.\n\n"
            + escapeReminder(context)
            + "\n</system-reminder>",
        }],
        source: { kind: KIND, digest, event },
      };
      if (decision.kind === "reject" || (step === 1 && decision.messages.length === 0)) {
        syncPending(agent, desired);
        return decision;
      }
      syncPending(agent, undefined);
      if (decision.messages.some((message) => message?.source?.kind === KIND
        && message.source.digest === digest)) {
        return decision;
      }
      const list = decision.messages;
      const lastClaimedIndex = list.findLastIndex((message) => messages.includes(message));
      return { ...decision, messages: list.toSpliced(lastClaimedIndex + 1, 0, desired) };
    } catch (error) {
      ctx.logger?.warn?.("agent-manager memory inject skipped: %o", error);
      return decision;
    }
  });
}
"#
}

// ── patch 条目渲染与移除 ──────────────────────────────────────────────────────

fn render_entry(context_url: &str, snapshot: &str) -> String {
    format!(
        "- insert:\n    - id: {PLUGIN_ENTRY_ID}\n      name: {PLUGIN_RELATIVE_NAME}\n      config:\n        contextUrl: {}\n        snapshotPath: {}\n        timeoutMs: 2000",
        crate::dsh_mcp::yaml_quote(context_url),
        crate::dsh_mcp::yaml_quote(snapshot),
    )
}

/// 删除含本插件 id 的顶层条目；其余内容逐字节保留。
fn rewrite_injection_items(text: &str) -> Result<String, String> {
    let lines: Vec<&str> = text.lines().collect();
    let items = crate::dsh_mcp::split_top_items(text);
    let mut removed: Vec<(usize, usize)> = Vec::new();
    for item in &items {
        let block = crate::dsh_mcp::item_block(text, item)?;
        if block.contains(PLUGIN_ENTRY_ID) {
            removed.push((item.start, item.end));
        }
    }
    let mut output: Vec<&str> = Vec::with_capacity(lines.len());
    for (index, line) in lines.iter().enumerate() {
        if removed.iter().any(|(start, end)| index >= *start && index < *end) {
            continue;
        }
        output.push(line);
    }
    Ok(output.join("\n"))
}

// ── 对外操作 ──────────────────────────────────────────────────────────────────

/// 安装/修复注入插件：写入插件文件与最新快照，并确保 patch 层恰好有一条
/// 指向它的 insert 条目（幂等：先移除旧条目再追加）。
pub(crate) fn install() -> Result<Vec<String>, String> {
    let plugin = plugin_path().ok_or("无法定位 DeepSeek Harness 配置目录（~/.dsh）")?;
    let snapshot = snapshot_path().ok_or("无法定位 DeepSeek Harness 配置目录（~/.dsh）")?;
    let patch = crate::dsh_mcp::patch_path_or_error()?;

    if let Some(parent) = plugin.parent() {
        std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    crate::dsh_mcp::write_atomically(&plugin, plugin_source())?;
    refresh_snapshot(
        &crate::memory_mcp::hook_session_start_context("dsh").0,
    )?;

    let text = match std::fs::read_to_string(&patch) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            crate::dsh_mcp::new_file_header().to_string()
        }
        Err(error) => return Err(format!("读取 {} 失败：{error}", patch.display())),
    };
    let context_url = format!(
        "http://127.0.0.1:{}/memory/context?source=dsh",
        crate::agent_http::AGENT_HTTP_PORT
    );
    let updated = crate::dsh_mcp::append_item(
        rewrite_injection_items(&text)?,
        &render_entry(&context_url, &snapshot.to_string_lossy()),
    );
    crate::dsh_mcp::write_atomically(&patch, &updated)?;
    Ok(vec!["agent/pre-step".into()])
}

/// 卸载：移除 patch 条目与插件文件（连同快照）。
pub(crate) fn uninstall() -> Result<(), String> {
    let patch = crate::dsh_mcp::patch_path_or_error()?;
    if let Ok(text) = std::fs::read_to_string(&patch) {
        let updated = rewrite_injection_items(&text)?;
        crate::dsh_mcp::write_atomically(&patch, &updated)?;
    }
    for path in [plugin_path(), snapshot_path()].into_iter().flatten() {
        let _ = std::fs::remove_file(path);
    }
    Ok(())
}

/// (installed, events)：条目与插件文件都在位才算已安装。
pub(crate) fn status() -> (bool, Vec<String>) {
    let entry_present = crate::dsh_mcp::patch_path()
        .and_then(|path| std::fs::read_to_string(path).ok())
        .map(|text| {
            crate::dsh_mcp::split_top_items(&text)
                .iter()
                .filter_map(|item| crate::dsh_mcp::item_block(&text, item).ok())
                .any(|block| block.contains(PLUGIN_ENTRY_ID))
        })
        .unwrap_or(false);
    let file_present = plugin_path().is_some_and(|path| path.is_file());
    (
        entry_present && file_present,
        vec!["agent/pre-step".to_string()],
    )
}

/// 用最新会话启动正文刷新离线快照（智管在线服务注入时顺带写，离线时插件
/// 读到的就是最后一次在线内容）。
pub(crate) fn refresh_snapshot(context: &str) -> Result<(), String> {
    let snapshot = snapshot_path().ok_or("无法定位 DeepSeek Harness 配置目录（~/.dsh）")?;
    if let Some(parent) = snapshot.parent() {
        std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    crate::dsh_mcp::write_atomically(&snapshot, context)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entry_render_contains_stable_id_and_relative_name() {
        let entry = render_entry(
            "http://127.0.0.1:9420/memory/context?source=dsh",
            r"C:\Users\t\.dsh\profiles\desktop\plugins\agent-manager-memory-snapshot.md",
        );
        assert!(entry.contains(&format!("id: {PLUGIN_ENTRY_ID}")));
        assert!(entry.contains("name: ./plugins/agent-manager-memory-inject.mjs"));
        assert!(entry.contains("contextUrl: \"http://127.0.0.1:9420/memory/context?source=dsh\""));
        // Windows 路径必须被引号转义
        assert!(entry.contains("snapshotPath: \"C:\\\\Users\\\\t\\\\.dsh"));
    }

    #[test]
    fn rewrite_removes_only_injection_entries() {
        let text = format!(
            "# 用户注释\n- id: ui-chat\n  name: \"@deepseek-ai/dsh-client-ui-chat\"\n  config:\n    transcriptView: standard\n\n{ }\n",
            render_entry("http://127.0.0.1:9420/memory/context?source=dsh", "/tmp/snapshot.md")
        );
        let updated = rewrite_injection_items(&text).unwrap();
        assert!(updated.contains("ui-chat"));
        assert!(!updated.contains(PLUGIN_ENTRY_ID));
    }

    #[test]
    fn plugin_source_is_self_consistent() {
        let source = plugin_source();
        assert!(source.contains("export const name = \"agent-manager-memory-inject\""));
        assert!(source.contains("ctx.on(\"agent/pre-step\""));
        assert!(source.contains("agent.inbox.prepend(\"next-step\""));
        // 不含会破坏 Rust 原始字符串定界符的序列
        assert!(!source.contains("\"#"));
    }
}
