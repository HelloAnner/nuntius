# 后端实现状态与设计边界

本文是代码与详细设计之间的权威对照，基于 `0.1.0` 后端实现。`docs/design/` 中的功能设计和技术设计同时包含当前 MVP 契约与后续演进方案；当某段设计与本文状态冲突时，以本文、两份 OpenAPI 和数据库 migration 为当前实现准绳。

## 1. 当前完成范围

当前后端由两个独立 Rust 二进制组成：

- `nuntius-client`：每台工作电脑上的后台 Agent、本地 loopback HTTP API、Codex App Server stdio 适配器、本地 SQLite、设备隧道客户端。
- `nuntius-server`：公网控制 API、网页登录与配对、SSE、设备 WebSocket 隧道、跨设备聚合 SQLite、静态资源嵌入入口。

两套前端入口分别位于 `client/frontend` 与 `server/frontend`，本轮后端对齐不修改前端。仓库当前仍有前端 `shared/` workspace 依赖，这属于后续前端拆分任务，不属于后端完成条件。当前接口以 [Client OpenAPI](../client/api/openapi.yaml) 和 [Server OpenAPI](../server/api/openapi.yaml) 为准。

## 2. 子领域完成矩阵

| 子领域 | 0.1 后端状态 | 已完成的核心能力 | 明确不在 0.1 后端范围 |
|---|---|---|---|
| 系统基础 | 完成 | 两个单二进制、两个 SQLite、配置/数据目录约束、健康检查、请求体上限、优雅退出 | 微服务、多活 |
| 身份访问 | 完成 | bootstrap、Argon2 密码、Cookie Session、CSRF、多标签页 CSRF Token、一次性配对码、Ed25519 challenge、短期设备 Token | OIDC、MFA、找回密码、登录限流内核（应由入口代理补充） |
| 设备管理 | 完成 | 列表/详情/改名/撤销、撤销立即关闭隧道、在线 Presence、心跳健康摘要、刷新与历史同步命令 | 设备分组、能力插件市场 |
| Agent/CLI | 完成 | init/pair/run/start/stop/status/backup/paths、单实例锁、macOS LaunchAgent 自动启动/异常重启、日志、重连、SQLite 恢复 | Linux systemd/Windows Service 安装器、独立救援进程、`doctor` 诊断包 |
| App Server | 完成 | stdio JSONL、initialize、请求/响应/通知、审批反向请求、进程重启、超时 `unknown`、输出与日志脱敏上限 | 多 App Server 实例池、版本自动安装 |
| 项目 | 完成 | allowed roots、目录引用创建项目、canonical path 唯一、项目摘要同步、`system_unassigned` 历史归属 | 项目暂停/恢复/移除、Git 状态扫描 |
| Thread | 完成 | 本地/远程创建、列表、全局索引、归档/取消归档、历史发现、离线读取、limit/offset 分页遍历全部历史 | 搜索、标签、稳定 signed-cursor 分页 |
| Turn/消息 | 完成 | start/steer/interrupt、完成事件后 `thread/read` 对账、用户/Agent/命令/文件项规范化 | 附件上传、语音输入 |
| 审批 | 完成 | 请求持久化、手机/本地决策、Server 与 Client 双层 CAS、重复点击/重放保护、未知结果 | 策略型自动批准、审批委托 |
| Browser API | 完成 | 认证、归属校验、输入大小、幂等指纹、稳定错误体、命令状态查询 | 自动生成 SDK、所有列表的 signed cursor |
| Browser Events | 完成 | SSE keepalive、事件 journal、Last-Event-ID/after 重放、缺口 `resync_required`、快照游标 | 跨实例 SSE 总线 |
| Device Tunnel | 完成 | WS/WSS、子协议、Bearer 鉴权、hello/welcome、心跳、45 秒 watchdog、连接 epoch、旧连接替换、查询关联、2 MiB 上限 | mTLS、二进制帧压缩 |
| 可靠消息 | 完成 | Server durable command、原子设备 sequence、Client durable inbox/outbox、串行执行、ACK 状态机防回退、至少一次投递与业务幂等、断线重放、`unknown` | 优先级多队列、跨节点租约调度 |
| 本地控制台后端 | 完成 | loopback-only bind、Host/Origin 防 DNS rebinding、本地项目/会话/审批/API/SSE | 本地 Cookie 登录；当前安全边界是 loopback + Host/Origin |
| 远程控制台后端 | 完成 | 移动端所需的设备/项目/会话/审批/历史/命令/SSE API | 本轮不评价或修改 UI 实现 |
| 存储生命周期 | 完成 | WAL、FULL synchronous、外键、busy timeout、migration、单目录锁、一致性 backup、journal retention、checkpoint | 磁盘压力自动降级/分层清理、自动恢复损坏 DB |
| 安全 | 完成（MVP） | 所有权约束、CSRF、短期 Token、密钥文件权限、目录边界、symlink/隐藏目录拒绝、输入/帧/正文上限、HTTP 显式开关 | HTTP 链路加密、E2EE、密钥托管；HTTP 不能替代 TLS |
| 可观测性 | 基础完成 | tracing 文本/JSON 日志、healthz/readyz、Client info、设备健康/队列深度 | Prometheus/OpenTelemetry exporter、诊断包 |
| 发布运维 | 自动化完成 | Rust Ops、干净 checkout、latest-wins 队列、双平台构建、证书指纹固定与显式 designated requirement、Ops 变更优先自更新及启动回滚、SCP 原子部署、Server desired Client 推送、Client 60 秒启动观察/失败版本隔离/自更新回滚 | 独立 A/B Guardian、灰度发布、HTTPS 强制 |
| 测试质量 | 核心自动化完成 | protocol wire tag、SQLite migration、inbox/outbox/history、幂等/sequence/ACK 防回退、多 CSRF、历史 hash/归属、目录隐藏/symlink、审批 CAS | 24 小时 soak、跨 OS CI、浏览器 E2E、网络故障矩阵 |
| 历史汇总 | 完成 | 启动发现 active/archived、未归类兜底、批次 hash/大小/条数、revision、防跨设备覆盖、连续 cursor 完整性、服务端离线读取 | 全文搜索、历史导出、内容 E2EE |
| 目录浏览 | 完成 | 实时 query、5 分钟 DB-backed opaque ref、canonicalize、allowed roots、隐藏目录过滤、symlink 拒绝、分页、项目创建时复验 | HMAC action token、可配置 root registry、symlink 白名单 |

## 3. 稳定性闭环

### 3.1 手机到 Server

- 查询和命令走 HTTP；实时通知走 SSE。
- SSE 事件先写 `event_journal`，浏览器用事件 ID 重连；journal 缺口返回 `resync_required`，随后重新读取 `/sync`。
- 修改请求使用 Cookie Session + CSRF；多标签页可各自持有有效 CSRF Token。
- 副作用请求使用 1～128 字节 `Idempotency-Key`；相同键和相同指纹返回原命令，不同指纹返回冲突。

### 3.2 Server 到 Client

- 双向控制与 ACK 使用单条 WS/WSS；Server 同一设备只保留最新 epoch。
- 新副作用只在设备在线时接受；SQLite commit 是接受边界，commit 后发生的断线不改变 202，等待重连重放。
- Server 按设备原子生成单调 sequence；Client 先落 inbox，再发 persisted ACK，并严格串行执行远程命令。
- Server/Client 都保存终态，重复帧只回放终态；延迟 ACK 不可使 completed/failed/unknown/expired 回退。

### 3.3 Client 到 Codex App Server

- App Server 通过 stdio JSONL 管理，不暴露网络端口。
- 请求 ID 与 pending map 支持并发响应；生命周期锁不会跨请求 await，审批反向请求不会与原调用互锁。
- App Server 退出会完成所有 pending 请求；60 秒未确认的副作用进入 `unknown`，不会自动盲目重试。
- Turn 完成后执行 `thread/read(includeTurns=true)` 对账，再生成历史批次，避免仅依赖瞬时通知。

### 3.4 历史汇总

- Client 本地 DB 是执行和工作区真相；Server DB 是移动端跨设备读取投影。
- 每批 1～200 条、序列化记录不超过 768 KiB，携带 SHA-256、revision、from/to cursor。
- Server 重新计算 hash，验证 user/device/project/thread/turn/item 归属；完整标记只在同 revision 的 cursor 链从起点连续到终点时成立。
- 找不到 cwd 或 cwd 不在 allowed roots 的 Codex Thread 进入只读 `system_unassigned`，历史不会被静默跳过。

## 4. HTTP 兼容边界

`allow_insecure_http = true` 时，公网 API、SSE 和设备隧道分别使用 HTTP、SSE-over-HTTP 和 WS，功能与 HTTPS/WSS 保持一致，系统不会在 TLS 失败后自动降级。

HTTP 无法防止同链路攻击者读取或篡改登录密码、Cookie、设备 Token、命令和历史。应用层认证只能证明已持有凭证，不能提供链路保密性；因此公网 HTTP 只适合受信网络、VPN 或 SSH 隧道。若直接暴露公网，应在反向代理补充登录速率限制、连接限制和访问日志脱敏。

## 5. 验证命令

```bash
cargo fmt --all -- --check
cargo check --workspace --all-targets
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace --all-targets
cargo build --release --workspace
```

后续功能只有在代码、migration、OpenAPI、自动化测试和本状态表同时更新后，才能从“明确不在 0.1 范围”移动到“完成”。
