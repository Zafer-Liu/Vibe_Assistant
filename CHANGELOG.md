# 更新日志 | Changelog

本项目所有重要变更均记录在此文件中。每个版本列出其发布标签与上一标签
之间的变更。格式基于 [Keep a Changelog](https://keepachangelog.com/zh-CN/1.1.0/)。
每个版本先列中文，再列英文。

All notable changes to this project are documented in this file. Each version
lists the changes from the interval between its release tag and the previous
one. The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).
Each version lists Chinese first, then English.

## [v1.3.1] - 2026-11-18

### 新增

- 百度网盘作为云端 Vault 目标：本地 OpenList 部署、授权、挂载与 Vault 同步。

### 变更

- 改进浏览器 URL 激活与远端同步处理；更新文档。

### Added

- Baidu Netdisk as a cloud vault target: local OpenList deployment, authorization, mounting, and vault synchronization.

### Changed

- Improved browser URL activation and remote sync handling; documentation updates.

## [v1.3.0] - 2026-11-17

### 新增

- 可审阅的记忆精炼：标签范围精炼、生成的冲突提案、精炼历史与撤销。保留 L1 替换身份、标签重定向与云端删除状态。
- 检索工具：共享检索、来源证据访问、追踪与合成评测。

### 变更

- 限制语义工作量；优化分页读取、虚拟列表与页面加载。
- 加固转录提取与 MCP JSONL 编码及刷盘。
- 补充工作流文档；新增可复用的合成 UI 验证夹具。

### Added

- Reviewable memory refinement: label-scoped refinement, generated conflict proposals, refinement history, and undo. L1 replacement identities, label redirects, and cloud deletion state are preserved.
- Retrieval tooling: shared retrieval, source-evidence access, tracing, and synthetic evaluation.

### Changed

- Bounded semantic work; optimized paged reads, virtual lists, and page loading.
- Hardened transcript extraction and MCP JSONL encoding and flushing.
- Documented workflows; added reusable synthetic UI verification fixtures.

## [v1.2.4] - 2026-11-07

### 新增

- 云端用量账本修正校验：保留带签名的 Codex 计数器修正，校验会话级 token 总量，提供不暴露记录 ID 的字段级错误；匿名会话身份在导出与导入间保持稳定。

### 变更

- 遥测分析：日历维度用量查询与共享缓存快照、遥测工作移出 UI 线程、前端请求合并。
- 转录恢复：改进缺失原生转录的恢复，同时保留读取错误与安全限制。
- MCP 观察与启动流程加固：稳定客户端绑定与探测代际分离、安全的探测发布与恢复、启动就绪与记忆加载改进、不兼容 MCP 参数给出重配置指引；统一命令解析、运行时发布、遥测跟踪与数据库迁移行为。

### Added

- Validation of cloud usage ledger corrections: signed Codex counter corrections are preserved, session-level token totals validated, and field-specific errors reported without exposing record IDs. Anonymous session identities are stable across exports and imports.

### Changed

- Telemetry analytics: calendar-specific usage queries with shared cached snapshots, telemetry work moved off the UI thread, and coalesced frontend requests.
- Transcript recovery: improved recovery of missing native transcripts while preserving read errors and safety limits.
- MCP observation and startup flows: stable client bindings separated from probe generations, safe probe publication and recovery, improved startup readiness and memory loading, and reconfiguration guidance for incompatible MCP arguments. Unified command resolution, runtime publication, telemetry tracking, and database migration behavior.

## [v1.2.3] - 2026-11-06

### 新增

- 精确云端用量账本同步：跨设备同步明细 token 用量记录，为旧客户端保留每日聚合；分析区分仅每日总计并防止重复计数。

### 变更

- 改进远端请求超时与错误上报；同步后发出刷新事件。
- 加固发布工作流：标签与提交校验、最小权限的草稿发布、安装器上传与上传失败时的产物保留。
- CI 与发布的依赖安装跳过不必要的 ONNX Runtime 下载（WebView 推理使用 onnxruntime-web）。

### Added

- Precise cloud usage ledger sync: detailed token usage records synced across devices, with legacy daily aggregates preserved for older clients. Analytics distinguish daily-only totals and prevent duplicate counting.

### Changed

- Improved remote request timeouts and error reporting; refresh events emitted after sync.
- Hardened the release workflow: tag and commit validation, draft releases with scoped permissions, installer uploads, and artifact preservation on upload failures.
- CI and release installs skip unnecessary ONNX Runtime downloads (WebView inference uses onnxruntime-web).

## [v1.2.2] - 2026-11-05

### 新增

- 可选的模型配置 WebDAV/S3 同步：模型连接、加密 API Key、决策模型与语义嵌入设置，支持跨设备合并、删除墓碑与本地密钥重封存。
- 首次启动引导的第一步改为选择语言；界面即时切换，已选过语言时跳过该步。
- 破坏性环境变量变更前需要确认。

### 变更

- UI 现代化：可复用的弹窗与确认组件替换原生对话框；改进响应式布局、主题处理、可访问性、排版与本地化文案。
- Windows 安装器品牌化：16–256px 图标梯度、NSIS 安装与卸载器图标及头部、侧边位图；同版本重装与升级改为原地覆盖，不再先要求卸载。

### 修复

- 云端分类重放顺序：先应用记忆对象再应用标签覆盖，保留已完成检查点与手动标签，数据库初始化时一次性重放受影响的同步元数据。

### Added

- Optional model configuration sync over WebDAV/S3: model connections, encrypted API keys, decision models, and semantic embedding settings, with cross-device merge, deletion tombstones, and local key resealing.
- Language selection as the first onboarding step on first launch; the interface switches immediately and the step is skipped when a language is already chosen.
- Confirmation guard before destructive environment variable changes.

### Changed

- UI modernization: reusable modal and confirmation components replace native dialogs; improved responsive layouts, theme handling, accessibility, typography, and localized strings.
- Windows installer branding: 16–256px icon ladder, NSIS installer and uninstaller icons plus header and sidebar bitmaps; same-version reinstall and upgrade now overwrite in place instead of asking to uninstall first.

### Fixed

- Cloud classification replay ordering: memory objects are applied before tag overrides, completed checkpoints and manual tags are preserved, and affected sync metadata is replayed once during database initialization.

## [v1.2.1] - 2026-11-05

### 新增

- 每设备云镜像纳入完整提取记忆聚合：会话级提取记忆（保留持久性标签）；移除记忆与用量聚合的固定大小限制。
- 扩展云端记忆同步覆盖：跨设备同步记忆域、来源信息、时间戳、用户状态与接收端重打标；冲突安全迁移、目录墓碑、备份身份轮换与原子导入。

### 修复

- 旧版云端分类合并。

### Added

- Full extracted memory aggregates in per-device cloud mirrors: session-scoped extracted memories with durability labels; fixed size limits for memory and usage aggregates removed.
- Expanded cloud memory sync coverage: memory domains, provenance, timestamps, user state, and receiver retagging across devices, with conflict-safe migrations, catalog tombstones, backup identity rotation, and atomic imports.

### Fixed

- Legacy cloud classification merging.

## [v1.2.0] - 2026-11-04

### 新增

- 域路由记忆管理：基于决策模型的记忆域，支持 JEV 与 Ollama；作用域召回路由、域打标与过滤、分类工作流与本地化设置界面。
- 跨设备提取记忆镜像（含删除墓碑）。

### 变更

- 改进 L1 提取恢复与云同步行为。

### 修复

- 前端 locales 与 MemoryCenter 的 TypeScript 错误、图谱标签测量中的控制字符正则 lint、Rust 测试缺失的 session reader 导入。

### Added

- Domain-routed memory management: decision-model-backed memory domains with JEV and Ollama support, scoped recall routing, domain tagging and filtering, classification workflows, and localized settings UI.
- Cross-device extracted-memory mirrors with deletion tombstones.

### Changed

- Improved L1 extraction recovery and cloud sync behavior.

### Fixed

- Frontend TypeScript errors in locales and Memory Center, control-character regex lint in graph label measurement, and missing session reader imports in Rust tests.

## [v1.1.1] - 2026-11-02

### 新增

- DeepSeek Harness 桥接集成：以统一的桥接安装与功能管理替换旧版 dsh 插件，涵盖记忆注入、MCP 观察、用量上报、采集触发与原生 Skill 工具；新增本地 HTTP 桥接端点、幂等遥测摄取与即时会话采集。

### 变更

- 安装器更新：可用时直接使用 Windows 安装器资产，通过系统浏览器打开更新 URL，并显示编译版本号。
- README 动画改为仓库内 GIF，GitHub 可直接展示。

### 修复

- 条件写入保留原始 ETag：If-Match 头使用服务器原始返回值，避免同步期间反复出现 412 失败。

### Added

- DeepSeek Harness bridge integration: replaces the legacy dsh plugins with unified bridge installation and feature management, including memory injection, MCP observation, usage reporting, collection triggers, and native skill tools. Adds local HTTP bridge endpoints, idempotent telemetry ingestion, and immediate session collection.

### Changed

- Installer updates use direct Windows installer assets when available, open update URLs through the system browser, and display the compiled app version.
- README animations rendered as in-repo GIFs that GitHub can display.

### Fixed

- Raw ETags preserved for conditional writes: If-Match headers now use the original server-provided form, preventing repeated 412 failures during sync.

## [v1.1.0] - 2026-10-24

聚焦重构版本 —— 应用转型为记忆、Skill 与 MCP 的统一工作区。

The refocus release — the app becomes one workspace for memory, Skills, and MCP.

### 新增

- DeepSeek Harness 作为 Agent 来源：转录扫描时就地解码 zstd 压缩会话。
- 本地语义搜索设置与后端。
- 管理模型连接的模型注册表；复用配置的装备档案。
- 导航搜索、应用内帮助文档与窗口关闭行为设置。
- 重写的 MCP 库：配置仲裁、导入与装备对话框。

### 变更

- 遥测存储模块化（schema、transcripts、tests）。
- CI 与发布中 MCP 边车二进制通过共享启动脚本构建。
- 默认主题由深色改为浅色。

### 移除

- Agent 进程管理：启动/监控、流式日志、内嵌 Web UI 多标签页、交互式终端与 GitHub 仓库安装。
- 代理发布与分享：Cloudflare Tunnel 临时链接与 Caddy 反向代理托管。

### Added

- DeepSeek Harness as an agent source: zstd-compressed sessions are decoded in place during transcript scanning.
- Local semantic search settings and backend.
- Model registry for managing model connections; equipment profiles for reusing setups.
- Navigation search, in-app help documents, and window close-behavior settings.
- Rewritten MCP Library with configuration arbitration, import, and equip dialogs.

### Changed

- Telemetry store modularized into schema, transcript, and test modules.
- MCP sidecar binaries are built through the shared launcher script in CI and releases.
- Default theme changed from dark to light.

### Removed

- Agent process management: launch/monitor, streaming logs, embedded Web UI tabs, interactive terminal, and GitHub repository install.
- Proxy publishing and sharing: Cloudflare Tunnel temporary links and Caddy reverse proxy hosting.

## [v1.0.0] - 2026-10-09

1.0 正式版；该标签聚合了内部 v1.0.0–v1.0.2 系列。

The 1.0 release; this tag aggregates the internal v1.0.0–v1.0.2 series.

### 新增

- MCP 库：跨 Agent 的 MCP 服务器中央注册表，支持装备/卸载、漂移状态与从 Agent 客户端导入（8+1 个目标）。
- ZCode（Claude Code 兼容引擎）Agent 来源：多账号镜像目录与跨来源会话去重的转录扫描、Skill 目录与共享记忆 MCP。
- 云端备份 Vault 服务（axum + SQLite，令牌哈希鉴权），桌面端通过 WebDAV/S3 云同步与备份还原。
- 环境变量页：用户/系统两级注册表读写、PATH 分条编辑、REG_EXPAND_SZ 展开预览、WM_SETTINGCHANGE 广播与关键变量删除保护。
- L2/L3 后台定时重算（默认 L2 每 7 天、L3 每 30 天，可在设置调整；L3 重建后自动发布）。
- 首次引导向导、可展开的侧边栏分组与记忆中心面包屑。
- 可取消的手动记忆提取，含运行中/取消中状态与中断结果提示。
- 88API 作为内置赞助商模型入口，支持自动获取模型列表。
- 启动参数：`--page`、`--lang`、`--select`、`--start`、`--open-ui`。

### 变更

- 项目更名 Agent Manager → Vibe Assistant（显示名、提示词、文档、窗口标题；保留协议标识与数据目录）。
- Skill 中心性能：13 个同步命令转为异步，发布页漂移检查移出主线程，React memo 与 keep-mounted 渲染优化。
- 内置模型预设收敛为 88API 入口；升级时清理旧内置记录与失效的记忆提取选择。
- 中英文 README 重写，默认语言改为英文，内嵌功能截图与 Mermaid 重绘的中英双语架构图。
- 页面重组：移除 Dashboard、Manager Agent 与 McpAgent 页面，MCP 管理并入 MCP 库。
- Windows 下 20+ 处进程启动点统一隐藏控制台窗口（CREATE_NO_WINDOW）。

### 修复

- 记忆中心白屏回归（hooks 顺序）。
- 云端完整拉取、云端合并与记忆中心同步问题；MiniMax 区域鉴权与整理超时；已保存 PAT 的连接测试。
- L1 整理解析加固：截断 JSON 逐元素补救、memory 单数键、错误附模型原文。
- Settings 中 License 显示修正为 Apache 2.0；统一 productName 与 updater 仓库名。
- macOS Intel Release Runner；Linux CI 测试夹具按平台分隔符重组。

### 移除

- 工作流功能（前端页面与后端模块）。

### Added

- MCP Library: a central registry of MCP servers across agents, with equip/unequip, drift status, and import from agent clients (8+1 targets).
- ZCode (Claude Code-compatible engine) agent source: transcript scanning with multi-account mirror directories and cross-source session dedup, a skill directory, and the shared-memory MCP.
- Cloud backup Vault service (axum + SQLite, token-hash authentication) with desktop cloud sync and backup restore over WebDAV/S3.
- Environment Variables page: user/system registry editing, a PATH entry editor, REG_EXPAND_SZ expansion preview, WM_SETTINGCHANGE broadcast, and deletion protection for critical variables.
- Scheduled L2/L3 background recomputation (defaults: L2 every 7 days, L3 every 30 days, configurable; L3 auto-publishes after rebuild).
- First-run onboarding wizard, sidebar grouping with expandable subpages, and Memory Center breadcrumbs.
- Cancellable manual memory extraction with running/cancelling states and interrupted-result messaging.
- 88API as the built-in sponsored model provider, with automatic model list fetching.
- Startup arguments: `--page`, `--lang`, `--select`, `--start`, `--open-ui`.

### Changed

- Renamed Agent Manager to Vibe Assistant (display name, prompts, docs, window title; protocol identifiers and data directories are preserved).
- Skill Center performance: 13 synchronous commands made async, publish-page drift checks moved off the main thread, React memoization and keep-mounted rendering.
- Built-in model presets consolidated to the 88API entry; old built-in records and stale memory-extraction selections are cleaned up on upgrade.
- README rewritten in English and Chinese with English as the default language, embedded feature screenshots, and redrawn bilingual Mermaid architecture diagrams.
- Page reorganization: Dashboard, Manager Agent, and McpAgent pages removed; MCP management merged into the MCP Library.
- Windows console windows hidden across 20+ process spawn points (CREATE_NO_WINDOW).

### Fixed

- Memory Center white-screen regression (hook ordering).
- Cloud full pull, cloud merge, and Memory Center sync issues; MiniMax region authentication and organize timeout; saved-PAT connection tests.
- L1 organize parsing hardened: truncated JSON salvaged element by element, singular `memory` keys, errors carry the model's original text.
- Settings license display corrected to Apache 2.0; productName and updater repository unified.
- macOS Intel release runner; Linux CI test fixtures reorganized by platform separator.

### Removed

- Workflow feature (frontend pages and backend modules).

## [v1.0.0_beta] - 2026-08-21

### 新增

- 记忆中心：跨 Agent 分层记忆（L1 可检索记忆、L2 近 30 天工作记忆、L3 长期 Profile），从本机编码 Agent 对话自动提取，支持自定义记忆编辑与语义清洗去重。
- 双通道记忆注入：面向支持 Hook 的 Agent 的会话启动注入（L3 + L2），以及按需语义召回的共享记忆 MCP。
- Skill 库：扫描各 Agent 的 SKILL.md 汇入共享库，按内容哈希预览新增、更新与冲突。
- 按供应商计费口径聚合本机转录的 token 用量；遥测端点以本地 SQLite 事件账本为底座。
- Agent 来源基础设施与重构的 LLM 设置页。

### Added

- Memory Center: cross-agent layered memory (L1 retrievable memories, L2 30-day working memory, L3 long-term Profile) automatically extracted from local coding-agent conversations, with custom memory editing and semantic cleaning and deduplication.
- Dual-channel memory injection: session-start injection (L3 + L2) for hook-capable agents, and a shared-memory MCP with on-demand semantic recall.
- Skill library: SKILL.md files scanned across agents into a shared library, with content-hash previews of additions, updates, and conflicts.
- Token usage aggregation from local transcripts aligned with provider billing, plus telemetry endpoints backed by a local SQLite event ledger.
- Agent sources infrastructure and a reworked LLM settings page.

## [v0.3.0] - 2026-07-12

### 新增

- 工作流引擎：可视化工作流构建器、实时运行视图、运行历史、定时调度器与外部触发器。
- 数据分析工作流：数据结构识别、SQL 生成与执行、图表推荐与生成、业务洞察分析、Excel/Word/PPT 报告导出。
- 面向外部访问 Agent 的 HTTP 端点。

### 变更

- MCP Agent 与服务模块改进。

### Added

- Workflow engine: visual workflow builder, live run view, runs history, a scheduler for timed runs, and external triggers.
- Data-analysis workflows: data-structure recognition, SQL generation and execution, chart recommendation and generation, business insight analysis, and Excel/Word/PPT report export.
- HTTP endpoint for external access to agents.

### Changed

- MCP agent and service module improvements.

## [v0.2.3] - 2026-06-22

智管-Agent Manager 首个公开发布版本。

Initial public release of 智管-Agent Manager.

### 新增

- 本地 AI Agent 统一管理：一键启动/停止、流式日志、PID/端口状态、内嵌 Web UI 多标签页与交互式终端。
- Manager Agent：用自然语言指挥所有 Agent。
- MCP Agent 工具对话。
- 一键生成临时公网分享链接（Cloudflare Tunnel）与 Caddy 反向代理长期托管。
- 端口管理。
- CI 与手动、按标签触发的发布流程（打包全平台）；应用内更新检查与自动更新器。
- 可调整大小的面板、日志与操作。

### Added

- Unified management for local AI agents: one-click start/stop with streaming logs, PID/port status, embedded Web UI tabs, and an interactive terminal.
- Manager Agent: natural-language control of all agents.
- MCP Agent tool conversations.
- One-click temporary public share links (Cloudflare Tunnel) and Caddy reverse-proxy publishing for long-term hosting.
- Port Manager.
- CI and a manual, tag-driven release workflow bundling all platforms; in-app update checker and auto-updater.
- Resizable panels, logs, and actions.

[v1.3.1]: https://github.com/Zafer-Liu/Vibe_Assistant/compare/v1.3.0...v1.3.1
[v1.3.0]: https://github.com/Zafer-Liu/Vibe_Assistant/compare/v1.2.4...v1.3.0
[v1.2.4]: https://github.com/Zafer-Liu/Vibe_Assistant/compare/v1.2.3...v1.2.4
[v1.2.3]: https://github.com/Zafer-Liu/Vibe_Assistant/compare/v1.2.2...v1.2.3
[v1.2.2]: https://github.com/Zafer-Liu/Vibe_Assistant/compare/v1.2.1...v1.2.2
[v1.2.1]: https://github.com/Zafer-Liu/Vibe_Assistant/compare/v1.2.0...v1.2.1
[v1.2.0]: https://github.com/Zafer-Liu/Vibe_Assistant/compare/v1.1.1...v1.2.0
[v1.1.1]: https://github.com/Zafer-Liu/Vibe_Assistant/compare/v1.1.0...v1.1.1
[v1.1.0]: https://github.com/Zafer-Liu/Vibe_Assistant/compare/v1.0.0...v1.1.0
[v1.0.0]: https://github.com/Zafer-Liu/Vibe_Assistant/compare/v1.0.0_beta...v1.0.0
[v1.0.0_beta]: https://github.com/Zafer-Liu/Vibe_Assistant/compare/v0.3.0...v1.0.0_beta
[v0.3.0]: https://github.com/Zafer-Liu/Vibe_Assistant/compare/v0.2.3...v0.3.0
[v0.2.3]: https://github.com/Zafer-Liu/Vibe_Assistant/releases/tag/v0.2.3
