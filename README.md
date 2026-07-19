# Nuntius

Nuntius 把多台工作电脑上的本地编码代理（Codex、Kimi）接入同一台公网 Server，并提供两套彼此独立的浏览器页面：

- `client`：安装在工作电脑上的单二进制后台 Agent，同时提供只监听 loopback 的本地管理页。
- `server`：部署在公网服务器上的单二进制控制服务，同时提供手机、平板访问的远程控制页。

Rust 后端源码只有 `client/` 和 `server/` 两个项目。两套前端入口分别位于
`client/frontend` 和 `server/frontend`，构建后的 `dist/` 由对应 Rust
二进制嵌入。当前前端工作区仍存在根目录 `shared/` 依赖；它不参与本轮后端
实现，本轮也没有修改前端。最终前端拆分应在独立前端任务中完成。

```text
client/
├── src/
├── api/openapi.yaml
├── migrations/
└── frontend/
server/
├── src/
├── api/openapi.yaml
├── migrations/
└── frontend/
docs/
Cargo.toml
```

## 构建与测试

要求 Rust 1.90+。Rust 构建直接嵌入各项目已经生成的 `frontend/dist`，
因此构建两个后端二进制时不要求安装 Bun：

```bash
cargo build --workspace
cargo test --workspace
cargo build --release --workspace
```

远程控制页是 `server/frontend` 内的独立 Bun/Vite 工程：

```bash
cd server/frontend
bun install
bun run typecheck
bun run build
```

Client 本地管理页目前只保留独立占位目录，后续也只在
`client/frontend` 内实现，不与 Server 前端共享源码。

最终产物是：

```text
target/release/nuntius-client
target/release/nuntius-server
```

## 自动构建与下载

每次代码推送到 GitHub 后，[Build binaries](https://github.com/HelloAnner/nuntius/actions/workflows/build-binaries.yml)
会自动生成以下两个压缩包：

- `nuntius-server-linux-x86_64.tar.gz`：Linux AMD64 Server
- `nuntius-client-macos-arm64.tar.gz`：macOS Apple Silicon Client

Server 在 CentOS 7 / glibc 2.17 基线上构建，Client 在 macOS ARM64 runner
上构建。打开最新一次成功的运行即可在任务摘要中下载；按提交保存的 Actions
Artifact 保留 14 天。也可以使用 GitHub CLI 下载最新一次成功构建：

```bash
run_id="$(gh run list \
  --repo HelloAnner/nuntius \
  --workflow build-binaries.yml \
  --status success \
  --limit 1 \
  --json databaseId \
  --jq '.[0].databaseId')"
gh run download "$run_id" --repo HelloAnner/nuntius
```

`main` 分支全部测试和构建成功后，还会覆盖
[continuous](https://github.com/HelloAnner/nuntius/releases/tag/continuous) 滚动通道中的
Server、Client 和 `manifest.json` 三个资产。该通道只保留最新资产，不会按提交累积
Release。两个二进制内置自更新器：Server 默认每 60 秒检查一次，Client 默认每
300 秒检查一次；Client 会等待 Server 运行同一 commit 后再升级。下载内容通过
SHA-256 和内嵌构建身份校验，替换失败或新版本未能完成启动时回滚到 `.previous`。

可在各自 `config.toml` 中控制：

```toml
auto_update = true
update_interval_seconds = 60 # Client 默认是 300
```

如果 Server 不能直接访问 GitHub，可以只把一个已配对的 Client 指定为 Server
更新中继。该 Client 仍按 `update_interval_seconds` 检查滚动通道，在确认 Server
版本落后后下载并校验 Linux 产物，再通过配置的 SSH 连接投递给 Server：

```toml
# 仅在负责中继的 Client 上开启；其他 Client 保持 false。
server_update_relay = true
server_update_ssh_command = ["ssh", "moss-dev"]
server_update_ssh_timeout_seconds = 900
server_update_remote_binary = "/var/docker/mysql/nuntius/bin/nuntius-server"
server_update_remote_data_dir = "/var/docker/mysql/nuntius/data"
```

SSH 命令按参数数组直接执行，不经过本地 shell，因此也可以加入 `-p`、`-i`、
`ProxyJump` 等 OpenSSH 参数。连接必须能免交互执行远端二进制；SSH 登录权限就是
中继的授权边界。Client 会把归档写入远端 Server 数据目录的更新收件箱，运行中的
Server 每 5 秒读取一次，并再次校验 SHA-256、目标架构和二进制内嵌的 commit 身份，
随后自行平滑退出、替换和重启。这个流程不需要服务器上的更新脚本。第一次启用时
需要人工部署一次包含 `receive-update` 子命令的新 Server，之后即可自动滚动。
确认中继正常后，可在 Server 的 `config.toml` 中设置 `direct_github_update = false`，
避免 Server 在无法访问 GitHub 的网络中继续发起无效下载；`auto_update` 仍需保持
`true`，用于监听并激活 Client 投递的更新。

远程控制页（`server/frontend`）面向手机、平板和桌面，相关设计系统、
协议类型、SSE 归并器和消息组件全部收在该项目内部。Client 本地页将按
本地 API 单独实现，不建立跨项目的前端源码依赖。

## Server

Server 的所有持久数据都位于显式指定的单个目录。先初始化：

```bash
nuntius-server --data-dir /srv/nuntius init
```

命令会创建 `config.toml`、`nuntius-server.db`、`secrets/`、`logs/`、`run/`、`attachments/` 和 `backups/`。保存终端输出的 bootstrap token，修改配置后启动：

```bash
nuntius-server --data-dir /srv/nuntius serve
```

停服后可生成包含 SQLite 一致性快照和图片附件目录的备份（如果 Server 仍运行，数据目录锁会拒绝操作）：

```bash
nuntius-server --data-dir /srv/nuntius backup
```

默认仅监听 `127.0.0.1:8080`。推荐由 Caddy/Nginx 提供 HTTPS；若确实只能使用公网 HTTP，必须把 `public_base_url` 设为实际 `http://` 地址并显式设置 `allow_insecure_http = true`。HTTP 模式会同时使用 SSE 和 `ws://`，不会从 HTTPS/WSS 静默降级。

## Client

Client 的路径固定为当前用户的 `~/.nuntius/`，配置文件固定为 `~/.nuntius/config.toml`：

```bash
nuntius-client init
# 在 Server 页面生成一次性 pairing code
nuntius-client pair --code <PAIRING_CODE> --server-url https://example.com/
nuntius-client start
nuntius-client status
```

停服后可运行 `nuntius-client backup`，数据库和设备端图片缓存会一起写入 `~/.nuntius/backups/`。

前台调试使用 `nuntius-client run`；后台日志位于 `~/.nuntius/logs/nuntius-client.log`，本地页面默认访问 `http://127.0.0.1:7331/`。Client 通过 provider 层管理 `codex app-server`，并通过带 bearer token 的 loopback REST/WebSocket 连接 `kimi web`；两者都不会直接暴露到公网。

每个新会话可在页面选择 Codex（默认）或 Kimi。Kimi 默认由 `kimi web --keep-alive --no-open --port 58627` 按需启动，地址可用 `kimi_server_url` 配置，命令和参数可用 `kimi_command`、`kimi_args` 覆盖；地址必须保持为 loopback HTTP。

若 Server 是非 loopback HTTP，配对前还需显式允许：

```bash
nuntius-client pair --code <PAIRING_CODE> \
  --server-url http://server.example:8080/ \
  --allow-insecure-http
```

详细需求、协议与稳定性设计见 [docs/prd.md](docs/prd.md)、[docs/tech.md](docs/tech.md) 和 [docs/design/](docs/design/README.md)；当前后端的精确完成范围见 [docs/implementation-status.md](docs/implementation-status.md)。
