# Nuntius

Nuntius 把多台工作电脑上的 Codex App Server 接入同一台公网 Server，并提供两套彼此独立的浏览器页面：

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

打开最新一次成功的运行即可在任务摘要中下载。构建产物使用 GitHub Actions
Artifact 保存，不创建 Release；Artifact 保留 14 天后自动删除，下载时需要登录
GitHub。也可以使用 GitHub CLI 下载最新一次成功构建：

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

远程控制页（`server/frontend`）面向手机、平板和桌面，相关设计系统、
协议类型、SSE 归并器和消息组件全部收在该项目内部。Client 本地页将按
本地 API 单独实现，不建立跨项目的前端源码依赖。

## Server

Server 的所有持久数据都位于显式指定的单个目录。先初始化：

```bash
nuntius-server --data-dir /srv/nuntius init
```

命令会创建 `config.toml`、`nuntius-server.db`、`secrets/`、`logs/`、`run/` 和 `backups/`。保存终端输出的 bootstrap token，修改配置后启动：

```bash
nuntius-server --data-dir /srv/nuntius serve
```

停服后可生成 SQLite 一致性备份（如果 Server 仍运行，数据目录锁会拒绝操作）：

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

停服后可运行 `nuntius-client backup`，一致性备份会写入 `~/.nuntius/backups/`。

前台调试使用 `nuntius-client run`；后台日志位于 `~/.nuntius/logs/nuntius-client.log`，本地页面默认访问 `http://127.0.0.1:7331/`。Client 通过 stdio JSONL 管理 `codex app-server`，不会把 App Server 本身暴露到网络。

若 Server 是非 loopback HTTP，配对前还需显式允许：

```bash
nuntius-client pair --code <PAIRING_CODE> \
  --server-url http://server.example:8080/ \
  --allow-insecure-http
```

详细需求、协议与稳定性设计见 [docs/prd.md](docs/prd.md)、[docs/tech.md](docs/tech.md) 和 [docs/design/](docs/design/README.md)；当前后端的精确完成范围见 [docs/implementation-status.md](docs/implementation-status.md)。
