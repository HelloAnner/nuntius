# Nuntius

Nuntius 把多台工作电脑上的本地编码代理（Codex、Kimi）接入同一台公网 Server，并提供两套彼此独立的浏览器页面：

- `client`：安装在工作电脑上的单二进制后台 Agent，同时提供只监听 loopback 的本地管理页。
- `server`：部署在公网服务器上的单二进制控制服务，同时提供手机、平板访问的远程控制页。
- `ops`：独立发布控制器，在指定构建机监听 GitHub、构建两端产物并通过 SSH/SCP 部署。

Rust 后端源码位于 `client/`、`server/`、`updater/` 和 `ops/`。两套前端入口分别位于
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
ops/
├── src/
└── docker/
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
target/release/nuntius-ops
target/release/nuntius-server
```

## 自动构建与下载

每次代码推送到 GitHub 后，[Build binaries](https://github.com/HelloAnner/nuntius/actions/workflows/build-binaries.yml)
会自动生成以下三个压缩包：

- `nuntius-server-linux-x86_64.tar.gz`：Linux AMD64 Server
- `nuntius-client-macos-arm64.tar.gz`：macOS Apple Silicon Client
- `nuntius-ops-macos-arm64.tar.gz`：macOS Apple Silicon 发布控制器

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

推送 `v*` 版本 tag（或从 Actions 页面手动运行）时，云端流水线仍会更新 GitHub
`continuous` 滚动通道，作为旧 Client 的迁移兼容通道。正式自动部署由
`nuntius-ops` 执行：它以 `git ls-remote` 监听 `main`，每次在全新 checkout 中生成
前端，然后并行构建 macOS ARM Client 和 CentOS 7 / glibc 2.17 Linux AMD64 Server。
队列最多保留一个待处理版本；构建期间出现多个提交时只部署最新提交。

Ops 将不可变 Server/Client 包通过 SCP 上传。Server 二进制先在目标机运行
`build-info`，确认可以执行和 commit/target/sequence 完全一致后再原子替换并由
systemd 重启；`/api/v1/info` 和 Client 包下载校验失败时自动回滚。Server 启动后
持久读取 `data/releases/desired-client.json`，向在线 Client 广播，并在每次设备重连
时补发。因此离线 Client 不会错过版本。Client 只下载自己的包，通过 SHA-256 和
内嵌构建身份校验后原子替换，启动失败回滚到 `.previous`。

Client 可控制是否接受以及失败后的重试间隔：

```toml
auto_update = true
update_interval_seconds = 60
```

## Ops

Ops 运行在常在线的 macOS ARM 构建机，需要 Bun、rustup、Docker、Git、SSH 和 SCP。
初始化默认配置：

```bash
nuntius-ops init
$EDITOR ~/.nuntius-ops/config.toml
nuntius-ops once --force
nuntius-ops run
nuntius-ops status
```

默认配置监听 `HelloAnner/nuntius` 的 `main`，使用当前机器原生构建 Client，并在
`linux/amd64` manylinux2014 Docker builder 中构建 Server；Cargo target 和 registry
缓存位于 `~/.nuntius-ops/cache`，源码 checkout 每次都是全新的。SSH 必须可以免交互
连接目标机。`releaseSequence` 由 Ops 以 epoch milliseconds 和持久状态共同生成，
不会再依赖 GitHub run ID。

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

新设备从源码安装时，在仓库根目录执行：

```bash
make device-setup
```

命令会先把 release 二进制安装到 `~/.local/bin/nuntius-client`。编译完成并出现输入
提示后，在 Server 的“设置 → 设备配对”中生成一次性配对码并输入；随后命令会自动
初始化、注册和启动 Client。配对码不会进入命令参数或 shell 历史。等到提示出现后
再生成配对码，可以避免首次源码编译时间占用配对码的十分钟有效期。
默认 Server 地址为 `http://47.97.154.221:8765/`；如需临时覆盖，可执行
`make device-setup NUNTIUS_SERVER_URL=https://example.com/`。

> 当前默认地址是公网 HTTP，验证码和设备注册流量不受 TLS 保护。迁移到 HTTPS 前，
> 只应在可信网络或受保护的隧道中执行设备配对。

也可以继续分步执行：

```bash
nuntius-client init
# 在 Server 页面生成一次性 pairing code
nuntius-client pair --code <PAIRING_CODE> --server-url https://example.com/
nuntius-client start
nuntius-client status
```

macOS 上的 `start` 会安装并加载当前用户的 LaunchAgent，登录后自动启动，并在 Client
异常退出时按 5 秒限速重新拉起；`stop` 会卸载并移除 LaunchAgent，因此显式停服后不会
在下次登录时意外启动。升级自旧版后台进程时，首次需执行一次
`nuntius-client stop && nuntius-client start`，后续自更新会继续处于 launchd 守护之下。
`nuntius-client run` 仍保留为不安装系统服务的前台调试方式。

Client 同时安装独立的 Agent Host。Codex App Server、Kimi 服务和 Codex 事件短期日志由
Agent Host 持有，因此 Client 收到并校验新版本后会立即切换，不等待运行中会话结束。
新 Client 启动时先恢复全部 active/recovering 会话和待审批状态，再开放本地 HTTP、处理
命令并连接公网 Server；Agent Host 自身只在 provider 全部空闲后轮换版本。

自更新候选版本需连续运行 60 秒且关键后台任务保持存活后才会标记健康。候选版本在观察
期失败后会回滚到 `.previous`，并把失败 commit 写入
`~/.nuntius/run/rejected-client-release.json`，相同版本不会被 Server 的 desired release
反复触发。

停服后可运行 `nuntius-client backup`，数据库和设备端图片缓存会一起写入 `~/.nuntius/backups/`。

前台调试使用 `nuntius-client run`；后台日志位于 `~/.nuntius/logs/nuntius-client.log`，本地页面默认访问 `http://127.0.0.1:7331/`。Client 通过本机 Unix socket 连接 Agent Host，再由 provider 层管理 `codex app-server`，并通过带 bearer token 的 loopback REST/WebSocket 连接 `kimi web`；这些端点都不会直接暴露到公网。

每个新会话可在页面选择 Codex（默认）或 Kimi。Kimi 默认由 Agent Host 通过 `kimi web --no-open --port 58627` 启动，地址可用 `kimi_server_url` 配置，命令和参数可用 `kimi_command`、`kimi_args` 覆盖；地址必须保持为 loopback HTTP。Client 更新时 Kimi 进程和正在生成的会话保持运行，新的 Client 会重新订阅并核对会话状态。

若 Server 是非 loopback HTTP，配对前还需显式允许：

```bash
nuntius-client pair --code <PAIRING_CODE> \
  --server-url http://server.example:8080/ \
  --allow-insecure-http
```

详细需求、协议与稳定性设计见 [docs/prd.md](docs/prd.md)、[docs/tech.md](docs/tech.md) 和 [docs/design/](docs/design/README.md)；当前后端的精确完成范围见 [docs/implementation-status.md](docs/implementation-status.md)。
