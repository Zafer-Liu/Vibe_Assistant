# MCP 启动复发核查与修复

日期：2026-11-06，时间均为本机 UTC+08:00。

## 结论

此次 zvec-grep 和 zai-mcp-server 同时失败的共同原因是智管观测入口的代际切换，不是本次 GPU 索引重建或已核验的原始启动命令本身。

旧设计在读取观测状态时自动升级已有 assignment 的 launcher/relay。Claude Desktop、Cowork 与 Code 分别缓存启动命令；manifest 已换代而客户端仍调用旧 launcher 时，严格身份校验在启动上游前拒绝连接。旧连接可以先进入 fallback 并继续存活，恢复流程又因存在活跃 fallback 而拒绝回退，导致新连接持续失败。

没有从现有日志证明是哪个进程最后将客户端文件写回旧值，不能将回写者归咎于 Claude 或其他程序。这个不确定性不影响已证实的入口校验失败与自动升级设计缺陷。

## 证据

| 本机时间 | 观察 |
|---|---|
| 16:46:57 | Claude Desktop 启动。 |
| 16:47:03 | 日志显示两项服务用 f9d8… launcher 启动。 |
| 16:47:04–05 | 两个 manifest 更新为 a60b… launcher / be2b… relay，保留 f9d8… / 6fd0… 为上一代。 |
| 16:47:14–16 | 桌面连接完成 initialize 和 tools/list；随后 Cowork/Code 新建连接报 assignment 解析失败。 |
| 16:48:02 | 新连接重试仍失败；客户端配置仍为 f9d8…。 |
| 核查时 | 两项原始配置指纹均匹配，当前及历史二进制存在且内容哈希正确；旧 fallback 进程仍由 Claude 持有并持续写心跳。 |

启动对照结果：

| 路径 | zvec-grep | zai-mcp-server |
|---|---|---|
| 故障现场的客户端 launcher | 约 0.05 秒退出，assignment 解析失败 | 约 0.04 秒退出，assignment 解析失败 |
| 故障现场 manifest 指向的新 launcher | Agent assignment changed | Agent assignment changed |
| 同一资源库原始命令直连 | 初始化成功，6 个工具 | 初始化成功，8 个工具 |
| 恢复已记录的上一代完整入口对后 | 客户端 launcher 正常，6 个工具 | 客户端 launcher 正常，8 个工具 |

## 现场恢复

只将这两个 manifest 恢复到与客户端现有节点一致的、已记录的完整 launcher/relay 对。新入口对保留为历史，没有猜测或交叉拼接入口。原 manifest 备份在用户数据目录 `agent-manager/mcp-observation/recovery-backup-20261106-170649/`。

未改客户端启动节点、原始命令、API key 或模型配置，没有为本次修复重启 GPU daemon。

## 源码修复

修改 `src-tauri/src/mcp_registry.rs`：

1. 状态查询不再自动升级入口；未记录的旧代理节点也不再通过被动状态刷新自动前向改写。
2. 已有 assignment 的 launcher 固定；需要新 launcher 时显式关闭并重开观察，随后重连客户端。
3. 显式 relay 刷新沿用原 launcher 和 UUID；相同客户端节点不重写文件，以一次 active 原子保存提交，不出现 preparing 状态窗口。
4. 保留完整入口历史、配置指纹及严格身份校验。没有绕过校验或放开任意二进制。

## 已完成验证

- 当前本地开发版自动重新编译并重启；可执行文件更新于 17:20:30，新进程启动于 17:20:31。
- 聚焦 MCP 注册表单元测试：53 通过、0 失败、1 个会改真实客户端的 smoke 用例按原设定忽略。
- 三轮并发客户端启动：两项服务每轮均完成 initialize → initialized → tools/list；每轮结束后配置与 manifest 仍一致。
- 通过恢复后的 zvec 客户端 launcher 执行本地语义搜索成功。
- 核对 Claude Desktop 的 9 条观测入口：未发现配置、manifest、原始配置指纹或二进制缺失问题。
- 没有调用 zai 图像服务；初始化和工具发现成功不作为其所有上游 API 功能或未来网络可用性的保证。

本次已应用源码并验证本地运行版，不是只准备补丁。未提交、推送或制作安装包。Claude 中此前缓存的失败状态需要对相应服务重新连接，才能由客户端连接池显示新的结果。
