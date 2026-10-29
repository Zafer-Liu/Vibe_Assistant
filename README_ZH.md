<div align="center">

<img src="./Logo/Banner.png" alt="智管-Vibe Assistant Banner" width="100%" />

# 智管-Vibe Assistant

**一个工作空间，管理记忆、Skill 与 MCP。**

[![版本](https://img.shields.io/badge/版本-v1.1.0-blue.svg)](https://github.com/Zafer-Liu/Vibe_Assistant/releases)
[![License](https://img.shields.io/badge/License-Apache%202.0-yellow.svg)](LICENSE)
[![Stars](https://img.shields.io/github/stars/Zafer-Liu/Vibe_Assistant?style=flat-square)](https://github.com/Zafer-Liu/Vibe_Assistant/stargazers)
[![CI](https://img.shields.io/github/actions/workflow/status/Zafer-Liu/Vibe_Assistant/ci.yml?style=flat-square&label=CI)](https://github.com/Zafer-Liu/Vibe_Assistant/actions)
[![Platform](https://img.shields.io/badge/平台-Windows%20%7C%20macOS%20%7C%20Linux-blue?style=flat-square)](README.md#install)
[![Tauri](https://img.shields.io/badge/Tauri-2-24C8DB?style=flat-square&logo=tauri)](https://tauri.app)
[![React](https://img.shields.io/badge/React-19-61DAFB?style=flat-square&logo=react)](https://react.dev)
[![Rust](https://img.shields.io/badge/Rust-stable-CE422B?style=flat-square&logo=rust)](https://www.rust-lang.org)
[![赞助商 88API](https://img.shields.io/badge/赞助商-88API-FF6B35?style=flat-square&logo=OpenAI&logoColor=white)](https://88api.ai/sign-up?aff=VNNV)

[✨ 项目亮点](#亮点) · [🧬 记忆中心](#记忆中心) · [⚙️ 快速安装](#安装) · [🚀 快速上手](#快速上手) · [❓ FAQ](#faq)

English | **[简体中文](./README_ZH.md)**

</div>

---

<a id="亮点"></a>

<a id="sponsors"></a>

## 💖 赞助商

感谢 **[88API](https://88api.ai/sign-up?aff=VNNV)** 赞助本项目！88API 是一家由香港企业运营、主要面向开发者、创作者与 AI 应用用户的一站式多模型 API 平台，通过统一接口接入文本、图片、语音和视频模型，覆盖 AI 编程、智能翻译、内容创作、配音、图像生成、视频生成等常见工作流。可直接在智管中完成 Provider 添加并切换，支持国际主流付款方式，可提供发票，提供企业级稳定服务。**[通过此链接注册](https://88api.ai/sign-up?aff=VNNV)** 即可获得专属赠送额度！

<p align="center">
  <a href="https://88api.ai/sign-up?aff=VNNV"><img src="frontend/public/88api-banner.jpg" alt="88API — 文本、图片、语音、视频一体化 API" width="600"></a>
</p>

---

## ✨ 项目亮点

**智管-Vibe Assistant** 是基于 Tauri 2 + React 19 + Rust 的本地桌面工作空间，核心是三件事：

- **记忆管理**：采集与检索会话记忆，维护近 30 天工作记忆与长期 Profile，并向已接入客户端注入上下文。
- **Skill 管理**：集中浏览、发布和部署技能，比较共享版本与本地修改，复用装备方案。
- **MCP 管理**：集中维护服务与客户端配置，发现工具，查看调用统计和错误日志。
- 辅助功能：Token 用量、端口、网络、环境变量、备份与云端记忆同步；支持中英文和明暗主题。

本应用不再提供客户端进程启动、终端、内嵌 Web UI、协同任务或代理发布。Claude、Codex 等客户端仍通过原有接入配置使用记忆、Skill 与 MCP。已有记忆、配置和历史产物不会因移除运行管理功能而删除。

---

## 🎬 核心原理

三大系统，一个目标：让每个 agent 拥有同一份记忆、技能与工具——不重复配置、不丢失上下文。

| 系统 | 解决的痛点 | 看原理 |
|------|-----------|--------|
| **🧬 记忆** | 全量注入撑爆上下文；不注入丢失连续性 | [L0→L3 流转](#记忆原理深入) |
| **🔌 MCP** | 每个 agent 要自己的配置格式；跨 10 个客户端排查调用如噩梦 | [统一管理](#mcp原理深入) |
| **📚 Skill** | 在 agent 之间复制 SKILL.md；看不见版本漂移 | [共享库](#skill原理深入) |

---

## 📑 完整目录

- [核心原理](#核心原理)
- [记忆中心](#记忆中心)
  - [记忆原理深入：L0–L3](#记忆原理深入)
- [Skill 管理](#skills)
  - [Skill 原理深入：扫描 → 发布 → 装备](#skill原理深入)
- [MCP 连接中心](#mcp-服务器中心)
  - [MCP 原理深入：一份配置，十个客户端](#mcp原理深入)
- [Skill 管理](#skills)
- [MCP 连接中心](#mcp-服务器中心)
- [系统工具箱](#系统工具箱)
- [安装方式](#安装)
- [快速上手](#快速上手)
- [模型配置](#llm-配置)
- [数据存储](#数据路径)
- [遥测与 Hook 接入](#遥测接入)
- [FAQ](#faq)
- [参与贡献](#参与贡献)
- [License](#license)

---

<a id="mcp-服务器中心"></a>

## 🔌 MCP 服务器中心

本机 MCP 服务器的统一目录——管理一次，处处分发。

<a id="mcp原理深入"></a>

### 🎬 MCP 如何流转：一份配置 → 十个客户端

<video src="./Images/animations/mcp-system.webm" width="100%" autoplay loop muted playsinline></video>

统一目录定义你的 MCP 服务器；Vibe Assistant 写出每个客户端的原生配置格式（Claude JSON、Codex TOML、dsh YAML 等十种）。stdio 中继位于 agent 与服务器之间，观测调用边界（服务器、工具、成功、耗时——绝不记录参数与返回内容），并上报本地遥测账本。配置变更热重载到运行中的客户端，无需重启。

**为什么重要**：不再手改十个配置文件；在一个地方排查「哪个工具何时失败」；你的 prompt 与工具输出绝不离开本机。

![MCP 服务器中心](Images/mcpcenter.png)

**本地服务器管理：**

- **本地扫描**：自动检测 npm 全局安装的 MCP 包
- **智能解析**：粘贴任意文本（官方文档、安装说明），AI 自动提取配置
- **手动添加**：stdio / SSE 配置

**目录与分发：**

- 常用 MCP 服务器集中保存为目录条目
- 一键安装到 Claude Code、Claude Desktop、Codex CLI、Codex Desktop、Qoder、WorkBuddy、MiniMax Code、Kimi、ZCode 等宿主
- 从各 Agent 现有配置一键导入，安装状态实时可视（已安装 / 配置漂移）

---

<a id="系统工具箱"></a>

## 🧰 系统工具箱

<a id="端口管理"></a>

### Port Manager 端口管理

![Port Manager](Images/portcenter.png)

- 查看当前机器上所有正在监听的端口：端口号、协议、PID、进程名
- 一键终止占用指定端口的进程——排查「端口被占用导致启动失败」的最快路径

<a id="网络管理"></a>

### 网络管理

![网络管理](Images/Internetcenter.png)

- **实时流量**：按网卡逐口的下载 / 上传速率，带滚动曲线图
- **网卡列表**：IPv4 / IPv6 地址、MAC、MTU
- **公网 IP**：直连与代理出口分别探测（自动识别代理）
- **带宽测速**：延迟 / 下行 / 上行，直连与代理两条链路各测一遍

<a id="环境变量"></a>

### 环境变量

![环境变量](Images/environmentvariable.png)

等价于 Windows「编辑系统环境变量」对话框，但不用碰注册表编辑器：

- **用户 / 系统**两级作用域分标签管理（系统级修改需要以管理员身份运行应用）
- **PATH 类变量逐条分行编辑**——新增、删除、调整顺序都在可视化列表里完成，不再跟一串分号搏斗
- 含 `%引用%` 的值按 `REG_EXPAND_SZ` 存储，并附展开后的效果预览
- 修改后广播 `WM_SETTINGCHANGE`，新启动的进程立即拿到新值
- 关键变量（PATH、PATHEXT、WINDIR 等）受删除保护，防止误删断送命令行

---

<a id="记忆中心"></a>

## 🧬 记忆中心 —— 所有 Agent 共享一个大脑

让每个编码 Agent 记住你的偏好、决策与当前进展，并让其他所有 Agent 从中受益。

<a id="记忆原理深入"></a>

### 🎬 记忆如何流转：L0 → L3

<video src="./Images/animations/memory-system.webm" width="100%" autoplay loop muted playsinline></video>

每次 agent 完成的会话成为原始事件（L0）；LLM 从每次会话提取 1–3 条带类型的记录（L1）；每 30 天的 L1 证据整编为工作记忆（L2）；L2 的草稿经人工确认后发布为长期 Profile（L3），在每个会话开局注入到所有连接的 agent。分层让上下文预算保持小巧，同时保留回到原始会话的审计链。

**为什么重要**：不用「粘贴 50 万 token 历史」的代价获得持久记忆；人工发布门防止模型错误污染长期偏好。

![记忆中心](Images/memorycentre.png)

### 🧬 四层记忆模型（L0–L3）

| 层级 | 名称 | 来源 | 作用 |
|------|------|------|------|
| L0 | 事件账本 | Hook 事件 / 本地转录扫描原文 | 审计源，整体从不注入 |
| L1 | 可检索记忆 | 每次会话结束自动提取 1–3 条事实 / 决策 / 约束 / 偏好 | 语义 + 关键词按需召回 |
| L2 | 近 30 天工作记忆 | 近期 L1 证据 map-reduce 压缩 | 会话启动时注入 |
| L3 | 长期 Profile | 基于 L2 起草，人工发布闸门 | 每轮提问注入稳定偏好与约束 |

### 📥 自动沉淀（Agent → 记忆）

- 支持 **Codex、Claude Code、Qoder、WorkBuddy、MiniMax Code、Kimi、GitHub Copilot、ZCode、DeepSeek Harness** 九个来源——Codex/Claude/Qoder/WorkBuddy 装 Hook 采集，九个都支持本地转录扫描（DeepSeek Harness 的 zstd 压缩会话直接解压解析），DeepSeek Harness 另有 pre-step 注入插件实现等价的自动注入
- 会话先落盘本地 SQLite 账本再异步提取——离线、模型未配置都不丢采集记录，排队中的提取可取消，失败会话可手动重跑
- 「待提取记忆」「已整理对话」面板随时回看完整对话、提取进度与失败原因（失败原因附带模型原始输出，便于诊断）
- L1 清洗与去重（「记忆清洗」）：本地 BGE 语义候选 + LLM 裁决，带可回滚检查点
- **从文件夹导入**：指定任意转录文件夹，直接提取其中的记忆

### 🔎 搜索、重要度与记忆引擎

- **语义搜索**：记忆中心内直接对所有 L1 记忆做语义 + 关键词混合检索
- **重要度评分**：每条记忆有分数与证据（支撑会话数 / 来源 Agent 数 / 召回次数），可置顶钉住
- **记忆引擎**：可选的本地语义服务，记忆中心可查看状态并按需启停
- 任意记忆可就地编辑 / 删除，也可手动新增条目
- 支持一键重置 L1 重新提取（需要干净重建时）

### ⏱️ L2/L3 定时重算

已发布的 L2 工作记忆与 L3 画像不再依赖手动点按钮：

- 应用常驻时每半小时检查一次：有新记忆且到达设置间隔（L2 默认 **7 天**）才重建
- L2 更新过且到达间隔（L3 默认 **30 天**）时重建草案并**自动发布**
- 间隔与开关在 **设置 → 记忆定时重算**，改动即时生效
- 每次后台运行都记录在记忆注入页；手动整理不受影响，随时可用

### 💉 记忆注入（记忆 → Agent，双通道）

| 通道 | 机制 | 适用场景 |
|------|------|----------|
| Hook 自动注入 | SessionStart 注入一次 L2；UserPromptSubmit 每轮注入 L3（内容指纹门控，未变化轮次零注入成本） | 开局获得工作背景，全程遵循长期偏好 |
| 共享记忆 MCP | Agent 按需调用 `recall_memory` 语义检索 | 只取任务相关记忆，控制上下文体积 |

**一键接入：** 为 **Claude Code、Claude Desktop、Codex CLI、Codex Desktop、Qoder、WorkBuddy、MiniMax Code、Kimi、ZCode、DeepSeek Harness** 十个宿主一键安装 / 卸载共享记忆 MCP，并为 Claude Code、Codex、Qoder、WorkBuddy 四个支持 Hook 的 Agent 管理采集与注入 Hook；数据目录支持按设备自定义。

### 🔗 DeepSeek Harness 桥接插件

[dsh-vibe-assistant-bridge](https://github.com/Zafer-Liu/dsh-vibe-assistant-bridge) 是官方桥接插件，让 DeepSeek Harness 成为 Vibe Assistant 的一等公民——一个 bundle，六项能力：

| 能力 | 作用 |
|------|------|
| **记忆注入** | 每次会话开局注入 L2/L3 共享记忆（离线快照回退） |
| **MCP 调用观测** | 工具调用边界上报本地账本（不记录参数与返回内容） |
| **Token 用量上报** | 逐条消息实时上报，与转录扫描口径幂等去重 |
| **会话结束收集触发** | 会话停止即触发 L0→L1 提取（无需等待扫描周期） |
| **召回工具** | `recall_memory` / `get_user_profile` 注册为 dsh 原生工具 |
| **Skill 工具** | `list_shared_skills` / `read_shared_skill` 注册为 dsh 原生工具 |

**安装：** 在 Vibe Assistant 的 MCP 库点「一键接入」，或在 dsh 桌面版的 **Plugins → Add plugin** 粘贴 `dsh-vibe-assistant-bridge`。需 Vibe Assistant 在本地运行（回环 HTTP 端口 9420）。

**源码与开发：** [Zafer-Liu/dsh-vibe-assistant-bridge](https://github.com/Zafer-Liu/dsh-vibe-assistant-bridge)（MIT）。也可通过 `dsh-plugin` topic 在社区插件目录发现。

### ✍️ 自定义记忆

在「近 30 天工作记忆」或「长期 Profile」卡片下手写补充记忆：只有用户能删改，自动清洗不会碰它们，且始终注入。

<a id="skills"></a>

### 📚 共享 Skill 库与技能市场

<a id="skill原理深入"></a>

### 🎬 Skill 如何流转：扫描 → 发布 → 装备

<video src="./Images/animations/skill-system.webm" width="100%" autoplay loop muted playsinline></video>

每个 agent 的 `SKILL.md` 文件扫描进共享库，按内容哈希识别。任何同步前预览确切的新增（绿）、更新（黄）、冲突（红）。「发布 + 装备」一步把批准的技能写回每个 agent 的目录。版本漂移按 agent 可见；一键回滚最近一次发布，或把本地修改采纳为共享新版。技能市场从 OpenAI 与 Anthropic 官方仓库安装精选技能，下载受限且路径校验。

**为什么重要**：不再问「哪个 agent 有这个技能的最新版」；冲突是显式选择而非静默覆盖；市场技能在落盘前经审计。

![Skill 库](Images/skillcenter.png)

- **Skill 库**：扫描各 Agent 的 `SKILL.md` 汇入共享库，按内容哈希预览新增 / 更新 / 冲突后确认同步；「发布 + 装备」一步完成，支持批量操作、各 Agent 版本漂移检测、最近一次发布一键回滚，以及「采纳本地」把某个 Agent 的本地修改升为共享新版
- **技能市场**：浏览并安装来自 **OpenAI / Anthropic 官方仓库**的精选技能——下载体量受限、每个相对路径都经过校验、下载的脚本绝不执行

### 📊 Token 用量统计

![Token 用量统计](Images/tokencenter.png)

聚合各 Agent 本机转录的真实输入 / 输出 / 缓存用量，口径与供应商计费对齐——覆盖 Codex、Claude Code、Qoder、WorkBuddy、MiniMax Code、Kimi、GitHub Copilot、DeepSeek Harness、Gemini CLI、OpenCode、OpenClaw 等：

- 汇总看板 + 逐会话用量明细 + 实时会话动态
- 注入看板：按来源统计会话 / 每轮注入次数、指纹门控命中率与估算 token 成本

### ☁️ 云保险库 —— 跨设备同步（可选）

多台设备共享同一份大脑：

- 同步范围：L2 / L3 已发布文档 + 用户自定义记忆
- **AES-256-GCM 端到端加密**：密钥派生自同步密码、不落盘，服务端只存密文、对内容零理解
- CAS 冲突检测、增量拉取、墓碑删除；本地 SQLite 永远权威，云端不可达不影响注入
- 三种同步目标：**内置保险库服务端**，或任意 **WebDAV / S3 兼容**存储（`s3://` 地址自动识别；整库打包为单个加密信封上传）
- 单文件自部署服务端，开源在 [`vault/`](vault/README.md)：支持 Docker / Railway / 香橙派 + Tailscale

---

<a id="安装"></a>

## ⚙️ 安装方式

### 下载预构建安装包（推荐）

从 [Releases](https://github.com/Zafer-Liu/Vibe_Assistant/releases) 下载：

| 平台 | 包 |
|------|-----|
| Windows x64 | `-x64-setup.exe`（NSIS 安装包） |
| macOS Apple Silicon | `-aarch64.dmg` |
| macOS Intel | `-x64.dmg` |
| Linux x64 | `-amd64.AppImage` / `-amd64.deb` |

### 从源码构建

**前置依赖**：[Node.js](https://nodejs.org) ≥ 18、[Rust](https://rustup.rs) stable、[Tauri 前置依赖](https://tauri.app/start/prerequisites/)（Windows 需要 VS C++ 生成工具）。

```bash
git clone https://github.com/Zafer-Liu/Vibe_Assistant.git
cd Vibe_Assistant

# 安装依赖
npm install
cd frontend && npm install && cd ..

# 开发模式运行
npm run dev

# 生产构建
npm run build
```

构建产物在 `src-tauri/target/release/bundle/` 下。

Windows 的 `npm run dev` 会将 Cargo 构建目录设为 Medium 完整性，避免继承沙箱工作区的 Low 标签后，程序无法写入 AppData、报 SQLite 只读错误。此预检仅调整构建产物目录，不修改数据库或工作区的权限。

---

<a id="快速上手"></a>

## 🚀 快速上手

1. 首次启动查看记忆中心、Skill、MCP 的模块介绍；也可从 **设置 → 重新打开使用引导** 重看。默认首页为记忆中心。
2. 在记忆中心手动添加项目背景或偏好；需要自动提取时，在设置保存并验证记忆提取模型，再检查记忆采集与注入。
3. 在技能库阅读本地 Skill，选择目标客户端发布；在已发布页面查看部署状态与差异。
4. 在 MCP 连接中心导入或添加服务，检查命令或 URL，为需要的客户端配置，并查看工具与调用记录。

整个流程无需导入或启动客户端进程。

---

<a id="llm-配置"></a>

## 🤖 LLM 配置说明

记忆自动提取与整理需要配置模型；手动记忆管理、Skill 与 MCP 配置可独立使用。在 **设置 → LLM 与记忆提取** 中添加：

| 字段 | 说明 | 示例 |
|------|------|------|
| 名称 | 自定义，随便填 | DeepSeek |
| Base URL | 任意 OpenAI 兼容 API | `https://api.deepseek.com/v1` |
| API Key | 对应的 API Key | `sk-xxx` |
| 模型 | 模型名称 | `deepseek-chat` |

点击 **测试连接**，绿色即配置成功。API Key 落盘前经 AES-256-GCM 加密（机器绑定）。

**内置预设**：**88API**（赞助商入口，Base URL 已预填——填入 Key 即可使用）、DeepSeek（`deepseek-chat`）、OpenAI（`gpt-4o-mini`）或任意自定义兼容端点。任意已填 Key 的提供商均可**一键获取模型列表**，新模型无需手动填写。

**本地模型（Ollama）**：设置页内置 Ollama 模块——连接后自动列出已拉取模型（参数规模 / 量化 / 体积），一键接入为自定义提供商或直接设为记忆提取模型。

---

<a id="数据路径"></a>

## 📁 数据存储路径

| 数据 | Windows | macOS / Linux |
|------|---------|---------------|
| 历史运行配置（保留，不再使用） | `%APPDATA%\agent-manager\agents.json` | `~/Library/Application Support/agent-manager/`（macOS）、`~/.local/share/agent-manager/`（Linux） |
| LLM 提供商 | `%APPDATA%\agent-manager\llm_config.json` | 同目录 |
| 记忆与遥测数据库 | `%APPDATA%\agent-manager\telemetry.sqlite3` | 同目录 |
| Agent 数据源目录覆盖 | `%APPDATA%\agent-manager\agent_source_paths.json` | 同目录 |
| 历史 Caddyfile（不再使用） | `%APPDATA%\agent-manager\Caddyfile` | 同目录 |
| Claude Desktop MCP 配置 | `%APPDATA%\Claude\claude_desktop_config.json` | `~/Library/Application Support/Claude/claude_desktop_config.json` |

「设置」页支持一键 **导出 / 导入配置备份**，方便迁移与灾难恢复。

---

<a id="遥测接入"></a>

## 🔌 遥测与外部接入（实验性）

记忆流水线以本地 SQLite 作为事件账本：Hook 事件先落盘、再异步提取，模型离线不会丢失采集记录。

- **Codex、Claude Code、Qoder**：可在「记忆中心 → 记忆采集」分别安装 / 卸载本机 Hook；端口和鉴权跟随「外部触发」配置
- **WorkBuddy**：不写其 Claude Code 配置避免重复采集；可通过标准遥测端点提交已聚合的会话 / 用量数据
- **Token 口径**：Codex、Claude、WorkBuddy 从本机转录读取真实用量；转录未提供完整供应商用量的来源显示明确标记的本地估算；零散 Hook 事件只保留审计记录不重复累计，适配器可用 `session_usage` 以真实最终值覆盖估算

标准遥测端点：`POST http://127.0.0.1:<hook-port>/telemetry/events/{codex|workbuddy|claude|qoder}`

```json
{
  "session_id": "stable-session-id",
  "event": "session_usage",
  "cwd": "D:/Github-repo/example",
  "usage": { "input_tokens": 120, "output_tokens": 48, "cached_tokens": 60 },
  "usage_scope": "session"
}
```

`input_tokens` 必须是该会话完整输入总量，`cached_tokens` 仅作缓存命中明细展示（不要再加进总量）；同一 `source + session_id` 后续重报覆盖先前值而非叠加。

<a id="版本更新"></a>

## 🗺️ 版本更新

> **当前版本 `v1.1.0`**

**v1.1.0**

- ✅ **云端保险库同步**——除内置保险库服务器外，新增 WebDAV / S3 兼容存储同步加密记忆保险库
- ✅ **88API 内置 Provider**——赞助商入口内置，模型列表自动获取，新模型无需手动填写
- ✅ **记忆提取可取消**——待提取任务可在「待定记忆」面板取消，失败会话仍可重跑
- ✅ **默认浅色主题**——全新安装默认浅色模式，深色模式仍可在设置中切换
- ✅ **功能聚焦瘦身**——进程启动、内嵌终端 / Web UI、协作任务与代理发布下线；记忆、配置与历史产出文件全部保留

**v1.0.x**
- ✅ **项目更名 Vibe Assistant**（智管-Vibe Assistant）——显示名、提示词、文档与窗口标题；协议标识与数据路径未变，既有 Hook、MCP 安装与本地数据全部兼容
- ✅ **环境变量管理**——用户 / 系统两级，PATH 逐条分行编辑，`REG_EXPAND_SZ` 展开预览，关键变量删除保护
- ✅ **L2/L3 记忆定时重算**——间隔可调（默认 L2 7 天 / L3 30 天），有新记忆才调用模型，L3 重建后自动发布
- ✅ **网络管理**——逐网卡实时流量、公网 IP（直连 + 代理出口）、双链路带宽测速
- ✅ **技能市场**——从 OpenAI / Anthropic 官方仓库安装精选技能到共享 Skill 库
- ✅ **移除工作流功能**——可视化工作流构建、任务看板与运行历史下线，让应用更聚焦（历史见各版本发布说明）
- ✅ **L1 提取加固**——截断 JSON 补救、契约漂移容忍、错误附模型原文输出便于诊断

📖 [查看完整 Changelog](https://github.com/Zafer-Liu/Vibe_Assistant/releases)

---

<a id="faq"></a>

## ❓ FAQ

<details>
<summary><b>🧬 记忆中心相关</b></summary>

<details>
<summary><b>会话一直没有被提取成记忆？</b></summary>

按顺序检查：

1. 记忆提取依赖 LLM：确认 **设置 → LLM 与记忆提取** 中至少有一个测试通过的提供商（或已接入 Ollama 本地模型）
2. 打开 **记忆中心 → 待提取记忆**：会话在不在队列里？失败原因是什么？（失败信息附带模型原始输出，可直接诊断）
3. 确认对应 Agent 的 Hook 已安装，或其转录目录配置正确
4. 失败的会话可在面板中手动重跑

</details>

<details>
<summary><b>L3 长期 Profile 修改后没生效？</b></summary>

L3 采用「草案 → 人工发布」闸门：草案必须点击 **确认发布** 才会注入；已发布的 L3 可直接编辑，保存即生效。若 MCP 检索不到新内容，确认宿主的共享记忆 MCP 处于已连接状态。

</details>

<details>
<summary><b>L2 / L3 多久刷新一次？</b></summary>

按计划自动刷新，不用点按钮：应用每半小时检查一次——有新记忆且到达设置间隔（L2 默认 7 天、L3 默认 30 天）才重建。在 **设置 → 记忆定时重算** 可调间隔或关闭；每次后台运行都记录在记忆注入页。

</details>

<details>
<summary><b>云保险库同步冲突了怎么办？</b></summary>

本地 SQLite 永远权威，云端不可达不影响注入。冲突时应用弹出对话框由你选择保留哪侧；服务端只存 AES-256-GCM 密文，无法读取或篡改内容。

</details>

</details>

---



---



---

<details>
<summary><b>⚙️ 安装与运行相关</b></summary>

<details>
<summary><b>Windows 安装包提示"未知发布者"？</b></summary>

点「更多信息」→「仍要运行」——安装包未做 Microsoft 代码签名，属正常现象。

</details>

<details>
<summary><b>macOS 提示"无法验证开发者"？</b></summary>

```bash
xattr -d com.apple.quarantine /Applications/智管-Vibe\ Assistant.app
```

或右键应用 → 「打开」→ 再点「打开」。

</details>

<details>
<summary><b>开发模式 npm run dev 报端口占用？</b></summary>

开发端口为 **1420**（避开 Vite 默认的 5173）：

```powershell
netstat -ano | findstr :1420
taskkill /PID <PID> /F
```

</details>

<details>
<summary><b>应用更名 / 更新后共享记忆 MCP 失效了？</b></summary>

更名安装会改变可执行文件路径，Agent 配置里记录的旧路径随之失效。到 **记忆中心 → 记忆注入** 对受影响宿主重新点一次「连接」即可恢复；此后同版本就地更新不再受影响。

</details>

</details>

---

<a id="参与贡献"></a>

## 🤝 参与贡献

欢迎提交 PR 或 Issue：

1. **Fork** 本仓库
2. 创建特性分支 (`git checkout -b feature/amazing-feature`)
3. 提交更改 (`git commit -m 'feat: add amazing feature'`)
4. 推送 (`git push origin feature/amazing-feature`)
5. 提交 **Pull Request**

Bug 报告或功能建议：[Issues](https://github.com/Zafer-Liu/Vibe_Assistant/issues)。开发环境搭建见[从源码构建](#安装)。

---

<a id="license"></a>

## 📄 License

[Apache 2.0](LICENSE)

---

<div align="center">

## ⭐ 项目目标

**把有用的记忆、可复用的技能和工具连接，留在同一个工作空间。**

</div>
