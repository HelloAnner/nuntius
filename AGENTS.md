# AGENTS.md

## 仓库结构

- `client/`：设备端 CLI（`nuntius-client`），Rust。含本地 Web 服务（loopback 7331）、App Server 适配、目录浏览、历史同步。
- `server/`：公网控制服务（`nuntius-server`），Rust。含 Web 认证、设备隧道（WS/SSE）、命令与历史存储（SQLx + SQLite）。
- `shared/`：`@nuntius/shared`，两套前端共享的设计系统（样式 token、协议类型、SSE 归并器 `ThreadLiveStore`、消息与会话组件）。Bun workspace 以源码引用，不单独构建。
- `server/frontend/`：远程控制台（`@nuntius/server-web`），React 18 + TS + Vite + zustand + TanStack Query。
- `client/frontend/`：本地控制台（`@nuntius/client-web`），同栈。

## 构建（云端执行）

```bash
bun install            # 根目录，workspace 安装
bun run build          # 构建两套前端 → 各自 frontend/dist
bun run typecheck      # 两端 tsc --noEmit
cargo build --workspace
cargo test --workspace
```

- 正式构建、前端产物生成和完整测试均由云端流水线执行。Agent 本地开发时**不要运行或等待 `bun run build`、`cargo build` 等构建命令，也不要把本地构建作为 commit / push 的前置条件**；完成需求代码后直接按功能提交并推送，由云端验证。
- Rust 在编译期用 rust-embed 嵌入 `frontend/dist`；云端构建前先生成两套前端产物，再构建 Rust workspace。本地已有或未更新 `dist/` 都不应阻塞 Agent 提交源码。
- 前端开发：`bun run dev:server`（:5180 → :8080）、`bun run dev:client`（:5181 → :7331）。

## 前端约定

- 样式只写 CSS：token 在 `shared/src/styles/tokens.css`（浅/深双主题，`data-theme` 切换），通用组件样式在 `components.css`，布局 chrome 在 `chrome.css`，Markdown/代码高亮在 `markdown.css`。不引入 Tailwind 等框架。
- 两端共用同一份会话渲染（`shared/src/components/ThreadView.tsx`）与状态词汇；新增消息类型时只改共享层。
- Server API：写操作需要 `x-csrf-token` + `Idempotency-Key`，返回 202 receipt，经 SSE `command.status_changed` 与 `/commands/{id}` 轮询追踪。Client API：无鉴权，写操作同步返回。
- 实时数据：单条用户级 SSE（`/api/v1/events`），事件归并进 `ThreadLiveStore`；`resync_required` 时全量 invalidate 查询缓存。

## 后端约定

- 两端 OpenAPI 定义在 `*/api/openapi.yaml`，二进制内嵌于 `/api/v1/openapi.yaml`。
- Server 数据目录由 `--data-dir` 指定；Client 固定 `~/.nuntius/`。测试时用 `HOME=/tmp/xxx` 隔离。
- 不要提交 `node_modules/`；密钥、令牌不进日志与仓库。

## 统一产品版本（强制）

- 根 `Cargo.toml` 的 `[workspace.package].version` 是 Nuntius 唯一产品版本；`nuntius-client` 与 `nuntius-server` 必须继承并使用完全相同的版本，禁止分别设置版本。
- 产品版本从 `0.0.1` 开始。每个进入正式二进制的独立功能或修复提交，默认必须把最后一段严格增加 1，例如 `0.0.1 -> 0.0.2 -> 0.0.3`。同一个功能提交只增加一次，不按文件数量增加。
- Agent 默认只能增加最后一段。任何前两段变化（例如 `0.0.x -> 0.1.0` 或 `0.x -> 1.0.0`）都必须由用户明确指定，Agent 不得自行决定。
- Client、Server、共享前端、Updater、Ops 或发布行为发生会进入正式产物的功能变化时，都视为一次产品更新并同步版本。纯文档、注释或测试数据修改可以不增加版本。
- Client 与 Server 设备隧道必须执行产品版本精确匹配。版本不一致时禁止注册为在线设备、禁止处理业务命令、事件和历史同步；只允许使用受限升级通道传递不兼容原因和签名 Client 更新。
- commit 前必须运行 `bun run version:check`。版本源、Rust workspace 包、前端 workspace metadata 或 lockfile 不一致时禁止 commit 和 push。

## Git 工作流

- 每一个独立的大功能开发完成后，不需要等待用户再次提醒，立即创建一个语义清晰的 Git commit，并自动 push 到当前分支对应的远端；当前分支尚无 upstream 时，设置并 push 到 `origin` 的同名分支。云端流水线负责构建与完整验证。
- commit 只包含该功能范围内的代码、测试、生成物和文档。工作区中用户已有或与本功能无关的改动必须保留，不得顺带提交、覆盖或丢弃。
- 修改 Rust 源码时，commit 前必须使用 Ops 同版本工具链对受影响 crate 运行 `cargo +1.94.0 check -p <package> --all-targets`，至少覆盖类型检查、借用检查和测试目标编译；默认 toolchain 可能低于 SQLx 0.9 的 MSRV，不能用裸 `cargo check` 代替。`cargo check` 在本仓库视为必要的轻量静态检查，不属于正式构建。仍不要运行或等待 `cargo build`、`cargo test` 等正式构建或完整测试命令。
- 代码 commit 并 push 到 `origin/main`，且确认远端已包含该 commit 后，即可结束当前任务并报告完成；不需要运行或等待 `nuntius-ops status`、二进制构建、部署结果或其他云端验证。
- push 失败时保留本地 commit，并明确报告失败原因。
- 用户明确要求暂不 commit、暂不 push 或采用其他 Git 流程时，以用户当次要求为准。密钥、令牌、生产配置和其他敏感信息始终不得进入 commit。
