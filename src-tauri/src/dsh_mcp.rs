//! DeepSeek Harness (DSH) 的 MCP 配置读写（`cordis.patch.yml`）。
//!
//! DSH 桌面版把插件树组成为 patch 层：`package.json` 的 `dsh.profile.bundles`
//! 各层 → 用户 patch 层 `profiles/<profile>/cordis.patch.yml` → `--patch`
//! overlay。patch 条目是「按 id 定位的覆盖/禁用/插入列表」——新增插件实例
//! 必须用顶层 `- insert: [...]` 形式（无 patch 级 id 时插入到树根）。
//! 每台 MCP 服务器是一条 `@deepseek-ai/dsh-mcp-client` 插件实例：
//!
//! ```yaml
//! - insert:
//!     - id: mcp-am-<serverName>
//!       name: "@deepseek-ai/dsh-mcp-client"
//!       config:
//!         serverName: "<serverName>"
//!         transport: "stdio"            # 或 "streamable-http"
//!         command: "<可执行文件>"
//!         args:
//!             - "<参数>"
//!         env:
//!             KEY: "value"
//! ```
//!
//! 桌面宿主硬编码使用 `desktop` profile（`resolveDesktopPaths`），且该文件
//! 在常驻面上被热重载：写入后正在运行的 DSH 会自动加载/卸载对应服务器。
//!
//! 该文件允许 `!!js` 表达式等任意 YAML，因此这里不做全文件反序列化，而是
//! 使用不执行标签的 YAML 事件解析器定位插件，再按原文区间做文本手术。
//! 混合 insert 只移除目标插件，其余内容（注释、`!!js`、其他 patch）保留。

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde_json::{json, Map, Value};
use yaml_rust2::parser::{Event, Parser};
use yaml_rust2::scanner::{Marker, TScalarStyle};

const MCP_CLIENT_PLUGIN: &str = "@deepseek-ai/dsh-mcp-client";
/// 智管写入的插件实例 id 前缀；serverName 限定 `[A-Za-z0-9_-]{1,32}`，拼接后仍是安全 id。
const MANAGED_ID_PREFIX: &str = "mcp-am-";

/// DSH 的 stdio 之外唯一远端 transport（无 sse）。
const TRANSPORT_STREAMABLE_HTTP: &str = "streamable-http";

// ── 路径 ──────────────────────────────────────────────────────────────────────

/// 桌面 profile 的用户 patch 层路径（`<dshHome>/profiles/desktop/cordis.patch.yml`）。
pub(crate) fn patch_path() -> Option<PathBuf> {
    crate::agent_sources::dsh_home().map(|home| {
        home.join("profiles")
            .join("desktop")
            .join("cordis.patch.yml")
    })
}

pub(crate) fn patch_path_or_error() -> Result<PathBuf, String> {
    patch_path().ok_or_else(|| "无法定位 DeepSeek Harness 配置目录（~/.dsh）".to_string())
}

// ── 数据模型 ──────────────────────────────────────────────────────────────────

/// 一台 DSH MCP 服务器的完整可写配置（dsh-mcp-client 的 config 节点）。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct DshMcpServer {
    pub server_name: String,
    /// "stdio" | "streamable-http"。
    pub transport: String,
    pub command: String,
    pub args: Vec<String>,
    pub env: BTreeMap<String, String>,
    pub url: String,
    pub headers: BTreeMap<String, String>,
}

impl DshMcpServer {
    fn validate(&self) -> Result<(), String> {
        let name = &self.server_name;
        let valid = !name.is_empty()
            && name.len() <= 32
            && name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
        if !valid {
            return Err("serverName 需为 1–32 个字母/数字/下划线/连字符".into());
        }
        if self.transport == "stdio" && self.command.trim().is_empty() {
            return Err("stdio 服务器必须提供启动命令".into());
        }
        if self.transport == TRANSPORT_STREAMABLE_HTTP && self.url.trim().is_empty() {
            return Err("远程服务器必须提供 URL".into());
        }
        Ok(())
    }

    /// 智管中央目录条目 → DSH 形状；registry 的 "http" 对应 dsh 的
    /// "streamable-http"，"sse" 不受支持。
    pub(crate) fn from_catalog(
        entry: &crate::mcp_registry::McpCatalogEntry,
    ) -> Result<Self, String> {
        let transport = match crate::mcp_registry::normalize_transport(&entry.transport) {
            "stdio" => "stdio".to_string(),
            "http" => TRANSPORT_STREAMABLE_HTTP.to_string(),
            other => return Err(format!("DeepSeek Harness 不支持 {other} transport")),
        };
        Ok(Self {
            server_name: entry.name.clone(),
            transport,
            command: entry.command.clone(),
            args: entry.args.clone(),
            env: entry.env.clone(),
            url: entry.url.clone(),
            headers: entry.headers.clone(),
        })
    }

    /// 共享记忆服务器：稳定启动器直连，参数固定 `--mcp-memory`。
    pub(crate) fn memory_server(executable: &str) -> Self {
        Self {
            server_name: crate::memory_mcp::server_name().to_string(),
            transport: "stdio".into(),
            command: executable.to_string(),
            args: vec!["--mcp-memory".into()],
            ..Default::default()
        }
    }

    /// 转成中央目录/对比逻辑使用的规范 JSON 形状（type: stdio|http）。
    fn to_canonical_value(&self) -> Value {
        if self.transport == TRANSPORT_STREAMABLE_HTTP {
            let mut value = json!({ "type": "http", "url": self.url });
            if !self.headers.is_empty() {
                value["headers"] = json!(self.headers);
            }
            value
        } else {
            let mut value = json!({ "type": "stdio", "command": self.command, "args": self.args });
            if !self.env.is_empty() {
                value["env"] = json!(self.env);
            }
            value
        }
    }
}

// ── 对外读写 ──────────────────────────────────────────────────────────────────

/// 读取 patch 层中的全部 dsh-mcp-client 服务器：serverName → 规范 JSON 形状。
/// 非 dsh-mcp-client 的 patch 条目不参与；无法解析的 dsh-mcp-client 条目让整
/// 体报错（宁可不可用也不显示错误的装备状态）。
pub(crate) fn read_servers() -> Result<Map<String, Value>, String> {
    let path = patch_path_or_error()?;
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Map::new()),
        Err(error) => return Err(format!("读取 {} 失败：{error}", path.display())),
    };
    let mut servers = Map::new();
    for server in parse_servers(&text).map_err(|error| {
        format!("{} 中 dsh-mcp-client 条目无法解析：{error}", path.display())
    })? {
        servers.insert(server.server_name.clone(), server.to_canonical_value());
    }
    Ok(servers)
}

/// 新增或更新一台服务器：移除同 serverName 的全部既有条目，再以规范的
/// insert 条目追加。混合 insert 中的其他插件及其余内容原样保留。
pub(crate) fn write_server(server: &DshMcpServer) -> Result<(), String> {
    server.validate()?;
    let path = patch_path_or_error()?;
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            new_file_header().to_string()
        }
        Err(error) => return Err(format!("读取 {} 失败：{error}", path.display())),
    };
    let updated = rewrite_items(&text, &server.server_name)?;
    let updated = append_item(updated, &render_item(server));
    document_items(&updated)?;
    write_atomically(&path, &updated)
}

/// 移除一台服务器：删掉同 serverName 的全部插件实例。
pub(crate) fn remove_server(name: &str) -> Result<(), String> {
    let path = patch_path_or_error()?;
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(format!("读取 {} 失败：{error}", path.display())),
    };
    let updated = rewrite_items(&text, name)?;
    write_atomically(&path, &updated)
}

/// 共享记忆服务器的安装状态（与 file_config_status 同口径）。
pub(crate) fn memory_status(executable: &str) -> Result<(bool, String), String> {
    let path = patch_path_or_error()?;
    let servers = read_servers()?;
    let name = crate::memory_mcp::server_name();
    let legacy = crate::memory_mcp::legacy_server_name();
    let entry = servers.get(name).or_else(|| servers.get(legacy));
    let installed = entry.is_some_and(|entry| {
        entry.get("command").and_then(Value::as_str) == Some(executable)
            && entry.get("args") == Some(&json!(["--mcp-memory"]))
    });
    let detail = match entry {
        None => format!("尚未写入 {}", path.display()),
        Some(_) if !installed => {
            format!("配置指向旧启动器或已失效，请重新连接（{}）", path.display())
        }
        Some(_) => format!("已配置到 {}", path.display()),
    };
    Ok((installed, detail))
}

// ── 顶层条目的文本切分与重写 ──────────────────────────────────────────────────

/// 一个顶层 `- ` 条目的行区间（含嵌进行）；条目之后的尾部空行/注释归入与
/// 下一元素之间的「缝隙」，不随条目删除，避免吃掉下一个条目的注释。
#[derive(Debug)]
pub(crate) struct TopItem {
    pub(crate) start: usize,
    pub(crate) end: usize,
}

pub(crate) fn item_block(text: &str, item: &TopItem) -> Result<String, String> {
    let lines: Vec<&str> = text.lines().collect();
    lines
        .get(item.start..item.end)
        .map(|slice| slice.join("\n"))
        .ok_or_else(|| "条目区间越界".to_string())
}

/// 顶层条目从第 0 列以 `- `（或单独 `-`）开始；条目内行必有缩进。
fn is_top_item_start(line: &str) -> bool {
    line == "-" || line.starts_with("- ")
}

pub(crate) fn split_top_items(text: &str) -> Vec<TopItem> {
    // Keep dsh_injection's line-based API, using YAML boundaries when possible.
    if let Ok(nodes) = document_items(text) {
        let lines: Vec<&str> = text.lines().collect();
        return nodes.iter().map(|node| {
            let start = node.start.line() - 1;
            let end = if node.end.index() >= text.chars().count() {
                lines.len()
            } else { node.end.line().saturating_sub(1).min(lines.len()) };
            finish_item(&lines, start, end)
        }).collect();
    }
    let lines: Vec<&str> = text.lines().collect();
    let starts: Vec<usize> = lines.iter().enumerate()
        .filter_map(|(index, line)| is_top_item_start(line).then_some(index)).collect();
    starts.iter().enumerate().map(|(index, start)| {
        finish_item(&lines, *start, starts.get(index + 1).copied().unwrap_or(lines.len()))
    }).collect()
}

/// 收敛条目结束行：条目内容之后的尾部空行与注释行留在缝隙里，不属于条目。
fn finish_item(lines: &[&str], start: usize, next_start: usize) -> TopItem {
    let mut end = next_start;
    while end > start {
        let previous = lines[end - 1].trim();
        if previous.is_empty() || (previous.starts_with('#') && !lines[end - 1].starts_with(' ')) {
            end -= 1;
        } else {
            break;
        }
    }
    TopItem { start, end }
}

/// 删除与 `server_name` 匹配的 dsh-mcp-client 条目；无法解析但疑似同名的
/// 条目让操作整体失败，绝不留下重复服务器。
fn rewrite_items(text: &str, server_name: &str) -> Result<String, String> {
    let items = document_items(text)?;
    let mut edits: Vec<(usize, usize, String)> = Vec::new();
    for item in &items {
        let candidates = plugin_candidates(item)?;
        let mut targets = Vec::new();
        for candidate in &candidates {
            if !is_mcp(candidate)? { continue; }
            let config = candidate.get("config").ok_or("dsh-mcp-client 条目缺少 config")?;
            let name = scalar_of(config, "serverName")
                .ok_or("dsh-mcp-client config 缺少可安全读取的 serverName，未修改文件")?;
            if name == server_name {
                server_from_config(config)?;
                targets.push(*candidate);
            }
        }
        if targets.is_empty() { continue; }
        // Validate every target before considering removal of its wrapper.
        // An inline insert cannot accidentally bypass the flow-syntax guard.
        for target in &targets { block_element_span(text, target)?; }
        let only_insert = matches!(&item.kind, NodeKind::Map(entries) if entries.len() == 1 && entries[0].0 == "insert");
        if only_insert && targets.len() == candidates.len() {
            // Deleting a wrapper is safe only when it has no patch metadata or
            // comments to preserve; otherwise leave a valid empty insert.
            let (start, end) = block_element_span(text, item)?;
            if !text[start..end].lines().any(|line| line.trim_start().starts_with('#')) {
                edits.push((start, end, String::new()));
                continue;
            }
        }
        if item.get("insert").is_some() {
            // Keep patch-level metadata and a valid empty insert. Shared-line
            // flow syntax is readable, but refused for lossless text surgery.
            if targets.len() == candidates.len() {
                for (index, target) in targets.into_iter().enumerate() {
                    let (start, end) = block_element_span(text, target)?;
                    let indent = text[start..].chars().take_while(|c| *c == ' ').count();
                    let replacement = if index == 0 {
                        format!("{}[]\n", " ".repeat(indent))
                    } else { String::new() };
                    edits.push((start, end, replacement));
                }
            } else {
                for target in targets {
                    let (start, end) = block_element_span(text, target)?;
                    edits.push((start, end, String::new()));
                }
            }
        } else {
            let (start, end) = block_element_span(text, item)?;
            edits.push((start, end, String::new()));
        }
    }
    edits.sort_by_key(|edit| edit.0);
    let mut output = text.to_string();
    for (start, end, replacement) in edits.into_iter().rev() {
        output.replace_range(start..end, &replacement);
    }
    // Removing an anchor must not leave dangling aliases.
    document_items(&output)?;
    Ok(output)
}

fn line_offset(text: &str, line: usize) -> usize {
    text.split_inclusive('\n').take(line.saturating_sub(1)).map(str::len).sum()
}

fn block_element_span(text: &str, node: &Node) -> Result<(usize, usize), String> {
    let start = line_offset(text, node.start.line());
    let line = text[start..].lines().next().unwrap_or("");
    let prefix: String = line.chars().take(node.start.col()).collect();
    if prefix.trim() != "-" || node.end.line() <= node.start.line() {
        return Err("该 MCP 使用内联/共享行结构，无法无损移除，未修改文件".into());
    }
    let mut end_line = node.end.line();
    let lines: Vec<&str> = text.lines().collect();
    let dash_indent = prefix.chars().take_while(|c| *c == ' ').count();
    // Comments between siblings belong to the following item. Deeply indented
    // comments can be literal scalar data and must stay inside the deletion.
    while end_line > node.start.line() + 1 {
        let line = lines.get(end_line - 2).copied().unwrap_or("");
        let indent = line.len() - line.trim_start().len();
        if line.trim().is_empty() || (line.trim_start().starts_with('#') && indent <= dash_indent) {
            end_line -= 1;
        } else { break; }
    }
    let end = if node.end.index() >= text.chars().count() && end_line == node.end.line() {
        text.len()
    } else { line_offset(text, end_line) };
    Ok((start, end))
}

pub(crate) fn append_item(mut text: String, item: &str) -> String {
    if !text.is_empty() && !text.ends_with('\n') {
        text.push('\n');
    }
    if !text.is_empty() && !text.ends_with("\n\n") {
        text.push('\n');
    }
    text.push_str(item);
    text.push('\n');
    text
}

pub(crate) fn new_file_header() -> &'static str {
    "# dsh profile patch layer — a top-level YAML array of loader patch entries.\n# MCP servers below are managed by Agent Manager (智管); other entries are preserved as-is."
}

/// 原子写：临时文件 + rename，与 DSH 自身的写入方式一致（其文件被热重载监听）。
pub(crate) fn write_atomically(path: &std::path::Path, content: &str) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    use std::io::Write;
    let parent = path.parent().filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| std::path::Path::new("."));
    let mut temporary = tempfile::NamedTempFile::new_in(parent)
        .map_err(|error| format!("创建临时文件失败：{error}"))?;
    temporary.write_all(content.as_bytes())
        .and_then(|_| temporary.as_file().sync_all())
        .map_err(|error| format!("写入 {} 失败：{error}", path.display()))?;
    temporary.persist(path)
        .map_err(|error| format!("发布 {} 失败：{error}", path.display()))?;
    Ok(())
}

// ── 条目渲染 ──────────────────────────────────────────────────────────────────

fn render_item(server: &DshMcpServer) -> String {
    let mut lines = Vec::new();
    lines.push("- insert:".to_string());
    lines.push(format!(
        "    - id: {MANAGED_ID_PREFIX}{}",
        server.server_name
    ));
    lines.push(format!("      name: \"{MCP_CLIENT_PLUGIN}\""));
    lines.push("      config:".to_string());
    lines.push(format!("        serverName: \"{}\"", server.server_name));
    if server.transport == TRANSPORT_STREAMABLE_HTTP {
        lines.push(format!("        transport: \"{TRANSPORT_STREAMABLE_HTTP}\""));
        lines.push(format!("        url: {}", yaml_quote(&server.url)));
        if !server.headers.is_empty() {
            lines.push("        headers:".to_string());
            for (key, value) in &server.headers {
                lines.push(format!("          {}: {}", yaml_quote(key), yaml_quote(value)));
            }
        }
    } else {
        lines.push("        transport: \"stdio\"".to_string());
        lines.push(format!("        command: {}", yaml_quote(&server.command)));
        if !server.args.is_empty() {
            lines.push("        args:".to_string());
            for argument in &server.args {
                lines.push(format!("          - {}", yaml_quote(argument)));
            }
        }
        if !server.env.is_empty() {
            lines.push("        env:".to_string());
            for (key, value) in &server.env {
                lines.push(format!("          {}: {}", yaml_quote(key), yaml_quote(value)));
            }
        }
    }
    lines.join("\n")
}

/// 双引号 YAML 标量：转义反斜杠、引号与控制字符；非 ASCII 原样输出。
pub(crate) fn yaml_quote(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for character in value.chars() {
        match character {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\x{:02x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

// ── Passive YAML events: tags are data, never constructed or evaluated ────────

#[derive(Debug)]
enum NodeKind {
    Map(Vec<(String, Node)>),
    List(Vec<Node>),
    Scalar(String),
    Null,
    Opaque,
}

#[derive(Debug)]
struct Node {
    kind: NodeKind,
    start: Marker,
    end: Marker,
    tagged: bool,
}

impl Node {
    fn get(&self, key: &str) -> Option<&Node> {
        let NodeKind::Map(entries) = &self.kind else { return None };
        entries.iter().find(|(name, _)| name == key).map(|(_, value)| value)
    }

    fn safe(&self) -> bool {
        !self.tagged && match &self.kind {
            NodeKind::Opaque => false,
            NodeKind::Map(entries) => entries.iter().all(|(_, node)| node.safe()),
            NodeKind::List(items) => items.iter().all(Node::safe),
            _ => true,
        }
    }
}

fn document_items(text: &str) -> Result<Vec<Node>, String> {
    let mut parser = Parser::new_from_str(text);
    let mut events = Vec::new();
    loop {
        let event = parser.next_token().map_err(|error| format!("YAML 无法安全解析，未修改文件：{error}"))?;
        let done = event.0 == Event::StreamEnd;
        events.push(event);
        if done { break; }
    }
    let mut position = 0;
    let mut root = None;
    while position < events.len() {
        match &events[position].0 {
            Event::StreamStart | Event::StreamEnd | Event::DocumentEnd => position += 1,
            Event::DocumentStart => {
                if root.is_some() { return Err("patch 只支持单个 YAML 文档".into()); }
                position += 1;
                let node = parse_node(&events, &mut position, 0)?;
                if node.tagged { return Err("patch 根节点带有标签/锚点，无法安全处理".into()); }
                root = Some(node);
            }
            _ => return Err("patch 文档结构无效".into()),
        }
    }
    match root.map(|node| node.kind) {
        None | Some(NodeKind::Null) => Ok(Vec::new()),
        Some(NodeKind::List(items)) => Ok(items),
        _ => Err("patch 根节点必须是列表".into()),
    }
}

fn parse_node(events: &[(Event, Marker)], position: &mut usize, depth: usize) -> Result<Node, String> {
    if depth > 128 { return Err("YAML 嵌套过深".into()); }
    let (event, mut start) = events.get(*position).ok_or("YAML 节点不完整")?.clone();
    *position += 1;
    let mut end = start;
    let mut tagged = false;
    let kind = match event {
        Event::Scalar(value, style, anchor, tag) => {
            tagged = tag.is_some() || anchor != 0;
            if style == TScalarStyle::Plain && matches!(value.as_str(), "" | "~" | "null" | "Null" | "NULL") {
                NodeKind::Null
            } else { NodeKind::Scalar(value) }
        }
        Event::Alias(_) => NodeKind::Opaque,
        Event::SequenceStart(anchor, tag) => {
            tagged = tag.is_some() || anchor != 0;
            let mut children = Vec::new();
            while !matches!(events.get(*position), Some((Event::SequenceEnd, _))) {
                children.push(parse_node(events, position, depth + 1)?);
            }
            end = events[*position].1;
            *position += 1;
            NodeKind::List(children)
        }
        Event::MappingStart(anchor, tag) => {
            tagged = tag.is_some() || anchor != 0;
            let mut entries: Vec<(String, Node)> = Vec::new();
            while !matches!(events.get(*position), Some((Event::MappingEnd, _))) {
                let key = parse_node(events, position, depth + 1)?;
                // yaml-rust2 marks an implicit block mapping at its first
                // colon, not at the key. Normalize to the key's true start.
                // Flow mappings retain their opening brace in the line prefix
                // and therefore still fail the lossless block-element guard.
                if entries.is_empty() { start = key.start; }
                let value = parse_node(events, position, depth + 1)?;
                let NodeKind::Scalar(key_text) = key.kind else {
                    return Err("patch 映射键必须是标量".into());
                };
                if key.tagged || entries.iter().any(|(existing, _)| existing == &key_text) {
                    return Err("patch 包含带标签的键或重复映射键，无法安全处理".into());
                }
                entries.push((key_text, value));
            }
            end = events[*position].1;
            *position += 1;
            NodeKind::Map(entries)
        }
        _ => return Err("YAML 节点类型无效".into()),
    };
    Ok(Node { kind, start, end, tagged })
}

fn plugin_candidates(node: &Node) -> Result<Vec<&Node>, String> {
    if node.tagged { return Err("patch 条目带有标签/锚点，无法安全处理".into()); }
    match node.get("insert") {
        Some(Node { kind: NodeKind::List(items), tagged: false, .. }) => Ok(items.iter().collect()),
        Some(_) => Err("insert 必须是不带标签的列表".into()),
        None => Ok(vec![node]),
    }
}

fn is_mcp(node: &Node) -> Result<bool, String> {
    if let Some(name) = node.get("name") {
        if !name.safe() || node.tagged {
            return Err("插件 name 带有标签/别名，无法安全判定 MCP 类型".into());
        }
    }
    Ok(scalar_of(node, "name") == Some(MCP_CLIENT_PLUGIN))
}

#[cfg(test)]
fn parse_item(block: &str) -> Result<Option<DshMcpServer>, String> {
    Ok(parse_servers(block)?.into_iter().next())
}

fn parse_servers(text: &str) -> Result<Vec<DshMcpServer>, String> {
    let mut servers = Vec::new();
    for item in document_items(text)? {
        for candidate in plugin_candidates(&item)? {
            if is_mcp(candidate)? {
                servers.push(server_from_config(candidate.get("config").ok_or("缺少 config")?)?);
            }
        }
    }
    Ok(servers)
}

fn scalar_of<'a>(node: &'a Node, key: &str) -> Option<&'a str> {
    if node.tagged { return None; }
    let value = node.get(key)?;
    if value.tagged { return None; }
    match &value.kind {
        NodeKind::Scalar(value) => Some(value),
        _ => None,
    }
}

fn server_from_config(config: &Node) -> Result<DshMcpServer, String> {
    if !config.safe() { return Err("MCP config 包含 YAML 标签、锚点或别名，无法安全解析".into()); }
    let NodeKind::Map(entries) = &config.kind else { return Err("config 必须是映射".into()); };
    let server_name = scalar_of(config, "serverName").ok_or("config 缺少 serverName")?.to_string();
    let transport = scalar_of(config, "transport").unwrap_or("stdio").to_string();
    if transport != "stdio" && transport != TRANSPORT_STREAMABLE_HTTP {
        return Err(format!("不支持的 transport：{transport}"));
    }
    let mut server = DshMcpServer { server_name, transport, ..Default::default() };
    for (key, value) in entries {
        match (key.as_str(), &value.kind) {
            ("command", NodeKind::Scalar(s)) => server.command = s.clone(),
            ("url", NodeKind::Scalar(s)) => server.url = s.clone(),
            ("serverName" | "transport", NodeKind::Scalar(_)) => {}
            ("args", NodeKind::List(items)) => {
                for item in items {
                    let NodeKind::Scalar(s) = &item.kind else { return Err("args 只支持字符串列表".into()); };
                    server.args.push(s.clone());
                }
            }
            ("env" | "headers", NodeKind::Map(pairs)) => {
                let target = if key == "env" { &mut server.env } else { &mut server.headers };
                for (key, value) in pairs {
                    let NodeKind::Scalar(s) = &value.kind else { return Err("env/headers 只支持字符串映射".into()); };
                    target.insert(key.clone(), s.clone());
                }
            }
            ("args" | "env" | "headers", NodeKind::Null) => {}
            ("args" | "env" | "headers" | "command" | "url" | "transport" | "serverName", _) => {
                return Err(format!("MCP 字段 {key} 类型无效"));
            }
            _ => {}
        }
    }
    if server.server_name.is_empty() { return Err("serverName 不能为空".into()); }
    Ok(server)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stdio_server() -> DshMcpServer {
        DshMcpServer {
            server_name: "github".into(),
            transport: "stdio".into(),
            command: "npx".into(),
            args: vec!["-y".into(), "@modelcontextprotocol/server-github".into()],
            env: BTreeMap::from([("GITHUB_TOKEN".into(), "t\"x\\y".into())]),
            ..Default::default()
        }
    }

    #[test]
    fn render_and_parse_round_trip() {
        let server = stdio_server();
        let item = render_item(&server);
        assert!(item.contains("- insert:"));
        assert!(item.contains("name: \"@deepseek-ai/dsh-mcp-client\""));
        let parsed = parse_item(&item).unwrap().expect("server expected");
        assert_eq!(parsed, server);
    }

    #[test]
    fn parse_direct_entry_without_insert_wrapper() {
        let block = r#"- id: mcp-web
  name: "@deepseek-ai/dsh-mcp-client"
  config:
    serverName: web
    transport: streamable-http
    url: http://localhost:3000/mcp"#;
        let parsed = parse_item(block).unwrap().expect("server expected");
        assert_eq!(parsed.transport, "streamable-http");
        assert_eq!(parsed.url, "http://localhost:3000/mcp");
    }

    #[test]
    fn rewrite_preserves_unrelated_entries_and_comments() {
        let text = format!(
            "# 用户注释\n- id: ui-chat\n  name: \"@deepseek-ai/dsh-client-ui-chat\"\n  config:\n    transcriptView: standard\n\n{}\n",
            render_item(&stdio_server())
        );
        let updated = rewrite_items(&text, "github").unwrap();
        assert!(updated.contains("ui-chat"));
        assert!(updated.contains("用户注释"));
        assert!(!updated.contains("serverName: \"github\""));
    }

    #[test]
    fn write_remove_cycle_over_real_file() {
        let directory =
            std::env::temp_dir().join(format!("dsh-mcp-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(&directory).unwrap();
        let file = directory.join("cordis.patch.yml");
        std::fs::write(
            &file,
            "# 用户层\n- id: ui-chat\n  name: \"@deepseek-ai/dsh-client-ui-chat\"\n  config:\n    transcriptView: standard\n",
        )
        .unwrap();

        // 追加（模拟 write_server 的内部组合）。
        let text = std::fs::read_to_string(&file).unwrap();
        let updated = append_item(
            rewrite_items(&text, "github").unwrap(),
            &render_item(&stdio_server()),
        );
        std::fs::write(&file, &updated).unwrap();

        // 读回：独立解析路径应复现同一服务器。
        let final_text = std::fs::read_to_string(&file).unwrap();
        let mut servers = Vec::new();
        for item in split_top_items(&final_text) {
            let block = item_block(&final_text, &item).unwrap();
            if !block.contains(MCP_CLIENT_PLUGIN) {
                continue;
            }
            servers.push(parse_item(&block).unwrap().expect("server expected"));
        }
        assert_eq!(servers, vec![stdio_server()]);
        assert!(final_text.contains("ui-chat"));
        assert!(final_text.contains("用户层"));

        // 再移除：只删 dsh-mcp-client 条目，用户层原样保留。
        let text = std::fs::read_to_string(&file).unwrap();
        let removed = rewrite_items(&text, "github").unwrap();
        assert!(removed.contains("ui-chat"));
        assert!(!removed.contains("serverName: \"github\""));
        std::fs::remove_dir_all(&directory).unwrap();
    }

    #[test]
    fn unparseable_same_name_entry_blocks_rewrite() {
        let text = "- insert:\n    - id: broken\n      name: \"@deepseek-ai/dsh-mcp-client\"\n      config:\n        serverName: github\n        transport: stdio\n        command: npx\n        env:\n          KEY: !!js process.env.KEY\n";
        assert!(rewrite_items(text, "github").is_err());
        // 不同名且不可解析 → 不匹配、不删除、不报错。
        assert!(rewrite_items(text, "other").is_ok());
    }

    #[test]
    fn multi_mcp_mixed_insert_preserves_siblings_and_tags() {
        let text = r#"# 用户层
- id: root
  insert:
    - id: other
      name: custom-plugin
      config:
        expression: !!js process.env.SECRET
    # first MCP
    - name: '@deepseek-ai/dsh-mcp-client'
      config:
        serverName: first
        command: npx
        args: ["-y", 'package,with,commas', "null", "https://host/a#b"] # comment
    # sibling
    - name: '@deepseek-ai/dsh-mcp-client'
      config: {serverName: second, command: python, args: ['-m', 'server']}
  config:
    untouched: true
# trailing
"#;
        let servers = parse_servers(text).unwrap();
        assert_eq!(servers.len(), 2);
        assert_eq!(servers[0].args, vec!["-y", "package,with,commas", "null", "https://host/a#b"]);
        assert_eq!(servers[1].args, vec!["-m", "server"]);
        let updated = rewrite_items(text, "first").unwrap();
        assert_eq!(updated, text.replace(r#"    - name: '@deepseek-ai/dsh-mcp-client'
      config:
        serverName: first
        command: npx
        args: ["-y", 'package,with,commas', "null", "https://host/a#b"] # comment
"#, ""));
        assert_eq!(parse_servers(&updated).unwrap(), vec![servers[1].clone()]);
        assert_eq!(rewrite_items(text, "absent").unwrap(), text);
    }

    #[test]
    fn multiline_scalars_and_yaml_escapes() {
        let text = r#"- insert:
    - name: '@deepseek-ai/dsh-mcp-client'
      config:
        serverName: multiline
        command: "C:\\Tools\\run.exe"
        args: ["ab\
          cd", "first
          second", "\x41\u0042", 'it''s fine']
        env:
          LITERAL: |-
            one
            # literal data
          FOLDED: >-
            first
            second
          SINGLE: 'first
            second'
"#;
        let servers = parse_servers(text).unwrap();
        let server = &servers[0];
        assert_eq!(server.args, vec!["abcd", "first second", "AB", "it's fine"]);
        assert_eq!(server.env["LITERAL"], "one\n# literal data");
        assert_eq!(server.env["FOLDED"], "first second");
        assert_eq!(server.env["SINGLE"], "first second");
        assert_eq!(parse_servers(&render_item(server)).unwrap(), servers);
    }

    #[test]
    fn removal_keeps_empty_insert_and_patch_metadata() {
        let text = "- id: parent\n  insert:\n    - name: '@deepseek-ai/dsh-mcp-client'\n      config: {serverName: one, command: npx}\n  config: {keep: true}\n";
        let updated = rewrite_items(text, "one").unwrap();
        assert!(updated.contains("  insert:\n    []\n  config: {keep: true}"));
        assert!(parse_servers(&updated).unwrap().is_empty());
    }

    #[test]
    fn invalid_fields_and_inline_deletion_fail_without_data_loss() {
        let inline = "- insert: [{name: '@deepseek-ai/dsh-mcp-client', config: {serverName: one, command: npx}}]\n";
        assert_eq!(parse_servers(inline).unwrap().len(), 1);
        assert!(rewrite_items(inline, "one").is_err());
        for field in ["args: bad", "headers: [wrong]", "env: {KEY: !!js process.env.KEY}"] {
            let text = format!("- name: '@deepseek-ai/dsh-mcp-client'\n  config:\n    serverName: one\n    command: npx\n    {field}\n");
            assert!(parse_servers(&text).is_err());
            assert!(rewrite_items(&text, "one").is_err());
        }
    }

    #[test]
    fn duplicate_instances_removed_and_unterminated_file_preserved() {
        let text = format!("{}\n{}", render_item(&stdio_server()), render_item(&stdio_server()));
        let updated = rewrite_items(&text, "github").unwrap();
        assert!(parse_servers(&updated).unwrap().is_empty());
        let unrelated = "# 中文\n- id: other\n  config: !!js process.env.OTHER";
        assert_eq!(rewrite_items(unrelated, "absent").unwrap(), unrelated);
    }

    #[test]
    fn replacement_cycle_does_not_accumulate_wrappers() {
        let text = format!("{}\n", render_item(&stdio_server()));
        let updated = append_item(rewrite_items(&text, "github").unwrap(), &render_item(&stdio_server()));
        assert_eq!(updated, text);
        let removed = rewrite_items(&text, "github").unwrap();
        assert!(removed.is_empty());
    }

    #[test]
    fn http_transport_maps_between_registry_and_dsh() {
        let entry = crate::mcp_registry::McpCatalogEntry {
            name: "web".into(),
            transport: "http".into(),
            url: "http://localhost:3000/mcp".into(),
            ..Default::default()
        };
        let server = DshMcpServer::from_catalog(&entry).unwrap();
        assert_eq!(server.transport, "streamable-http");
        let canonical = server.to_canonical_value();
        assert_eq!(canonical["type"], "http");
        assert_eq!(canonical["url"], "http://localhost:3000/mcp");

        let sse = crate::mcp_registry::McpCatalogEntry {
            name: "old".into(),
            transport: "sse".into(),
            url: "http://localhost/sse".into(),
            ..Default::default()
        };
        assert!(DshMcpServer::from_catalog(&sse).is_err());
    }
}
