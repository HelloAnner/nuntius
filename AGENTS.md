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

## Git 工作流

- 每一个独立的大功能开发完成后，不需要等待用户再次提醒，立即创建一个语义清晰的 Git commit，并自动 push 到当前分支对应的远端；当前分支尚无 upstream 时，设置并 push 到 `origin` 的同名分支。云端流水线负责构建与完整验证。
- commit 只包含该功能范围内的代码、测试、生成物和文档。工作区中用户已有或与本功能无关的改动必须保留，不得顺带提交、覆盖或丢弃。
- commit 前可做必要的代码审查或轻量静态检查，但不要运行本地构建，也不要因未执行本地构建而延迟 commit / push。push 后以云端检查结果为准；云端失败时再针对失败项修复并继续提交。
- push 后确认远端已包含该 commit；如果 CI 会被触发，应提供 commit、分支或 CI 链接。push 失败时保留本地 commit，并明确报告失败原因。
- 用户明确要求暂不 commit、暂不 push 或采用其他 Git 流程时，以用户当次要求为准。密钥、令牌、生产配置和其他敏感信息始终不得进入 commit。
