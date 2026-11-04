# Cloud Memory Vault 服务端

Agent Manager 云端记忆库的独立同步服务（设计见 `../docs/16-cloud-memory-vault-design.md`）。Axum + SQLite 单文件部署，服务端只存 AES-256-GCM 密文，对 payload 零理解。

## API

| 方法 | 路径 | 说明 |
|---|---|---|
| `GET` | `/health` | 存活探测（无需认证） |
| `GET` | `/objects?since_rev=N` | 增量拉取 revision > N 的对象 + 墓碑 |
| `GET` | `/objects/{key}` | 单对象读取 |
| `PUT` | `/objects/{key}` | CAS 写入，请求头 `If-Match: <base_revision>`，冲突返回 409 |
| `DELETE` | `/objects/{key}` | 删除并写墓碑 |

认证：`Authorization: Bearer <PAT>`；可选 `X-Device-Id` 头用于设备活跃统计。

同步对象的载荷和上传请求体不设置应用内固定大小上限；仍校验身份、对象 key 和 CAS 修订号。部署在反向代理或托管平台后时，外层服务自身的容量限制仍然适用。

## 首次启动

tokens 表为空时自动生成一个 PAT 打印到 stdout（仅此一次）；也可用环境变量 `VAULT_TOKENS=tok1,tok2` 预置多个。

## 环境变量

- `VAULT_DB`：SQLite 路径（默认 `vault.sqlite3`；容器/香橙派务必放持久卷，如 `/data/vault.sqlite3`）
- `PORT`：监听端口（默认 `8787`，绑定 `0.0.0.0`）
- `VAULT_TOKENS`：逗号分隔的预置 PAT

## 部署

### 香橙派（推荐：Tailscale 内网直连）

```powershell
# 本机交叉编译（前置：rustup target add aarch64-unknown-linux-musl）
cd deploy; .\build-aarch64.ps1
```

把产物 scp 到香橙派 `/usr/local/bin/agent-manager-vault`，复制 `deploy/vault.service` 到 `/etc/systemd/system/`（数据库目录改为实际持久路径），`systemctl enable --now vault`。客户端填 `http://<tailnet-ip>:8787`。

### Docker / Railway

```bash
docker build -t agent-manager-vault -f deploy/Dockerfile .
docker run -v vault-data:/data -p 8787:8787 agent-manager-vault
```

Railway：连接 Volume 到 `/data`，healthcheck path 填 `/health`，域名即客户端 URL。

## 测试

```bash
cargo test    # CAS 冲突 / 增量拉取 / 墓碑单测
```
