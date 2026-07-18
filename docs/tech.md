# Nuntius 技术架构与稳定性设计

> 当前 `0.1.0` 后端已经实现的范围与后续演进项见 [后端实现状态与设计边界](./implementation-status.md)。本文保留完整目标架构；OpenTelemetry、doctor、自动更新、服务管理器模板和高级磁盘压力治理属于后续范围，不应被理解为当前二进制已提供。

## 1. 文档信息

- 文档状态：技术方案初稿
- 更新日期：2026-07-18
- 关联需求：[产品需求文档](./prd.md)
- 核心目标：在手机网络切换、浏览器休眠、服务器重启、设备断网、CLI 重启和 Codex App Server 重启等情况下，尽可能做到命令不丢失、事件可恢复、重复消息不重复执行、状态可重新收敛。

## 2. 技术结论摘要

Nuntius 不应在所有链路上强行使用同一种实时协议。稳定性来自“针对每段链路选择合适的传输协议，并在传输之上实现持久化、幂等、确认、重放和状态同步”，而不是来自 SSE 或 WebSocket 中任意一个协议本身。

最终选型如下：

| 链路 | 上行协议 | 下行协议 | 最终选择 |
|---|---|---|---|
| 手机/平板浏览器 ↔ 公网服务器 | HTTPS/HTTP JSON API | SSE | **HTTP(S) + SSE** |
| 本地浏览器 ↔ 本机 CLI | Loopback HTTP JSON API | SSE | **HTTP + SSE** |
| 设备 CLI ↔ 公网服务器 | WSS/WS | WSS/WS | **单条双向 WebSocket 长连接** |
| 设备 CLI ↔ Codex App Server | stdin JSONL | stdout JSONL | **stdio JSON-RPC** |
| 文件、图片等大对象上传 | HTTP(S) multipart 或流式 PUT | HTTP(S) 下载 | **独立 HTTP 传输** |

其中：

- 手机向服务器发送消息、审批、终止等命令时使用普通 HTTP(S) 请求。
- 服务器向手机持续推送 Codex 增量消息、状态和审批请求时使用 SSE。
- 设备 CLI 和公网服务器之间使用一条由设备主动建立的 WebSocket 全双工长连接：安全模式为 WSS，HTTP 兼容模式为 WS。
- CLI 将 Codex App Server 作为受监管的本地子进程，通过默认的 stdio JSONL/JSON-RPC 协议交互。

这份技术方案细化了 PRD 中泛化描述的“浏览器实时通信”。浏览器实时下行最终选择 SSE，而不是让浏览器长期依赖 WebSocket。

### 2.1 传输部署档位

| 档位 | 浏览器 | Agent | 适用场景 | 安全结论 |
|---|---|---|---|---|
| `secure` | HTTPS + SSE | WSS | 公网、生产环境 | 推荐；具有 TLS 机密性、完整性和服务端身份认证 |
| `trusted-http` | HTTP + SSE | WS | 可信局域网、VPN、SSH 隧道 | 功能完整，但 HTTP/WS 本身不加密 |
| `local` | localhost HTTP + SSE | 不经过公网或按上面档位 | 本机控制台 | loopback 可用；仍需防 localhost CSRF/DNS rebinding |

Server 根据 `public_base_url` 自动派生协议：

```text
https:// -> https API + SSE over HTTPS + wss device tunnel
http://  -> http API + SSE over HTTP  + ws device tunnel
```

非 loopback 的 `http://` 必须同时显式配置 `allow_insecure_http = true`，否则 Server 拒绝启动远程控制入口。HTTP 模式的功能、幂等、SSE 游标、WS ACK 和重放能力与安全模式相同，但不提供网络窃听和中间人攻击防护。

这不是可以通过前端应用层加密彻底弥补的问题：首次通过 HTTP 下载的 JavaScript 本身可以被中间人替换。WSS 的 TLS 保护价值由 WebSocket 标准明确包含机密性、完整性和服务端身份认证；HTTP Cookie 的 `Secure` 属性也只会在安全通道发送。[RFC 6455](https://www.rfc-editor.org/rfc/rfc6455) / [RFC 6265](https://www.rfc-editor.org/rfc/rfc6265)

## 3. 为什么不能只讨论 SSE 和 WSS

SSE 和 WSS 都只能解决“连接上的数据如何传输”，无法独立保证以下目标：

- 连接断开前最后一个数据包到底有没有被对方处理。
- 网络恢复后从哪个事件继续。
- 同一条命令被重试两次时是否会重复执行。
- 服务端收到命令后宕机，重启后是否还能继续投递。
- CLI 把命令发送给 App Server 后立即崩溃，该命令是否已经生效。
- 浏览器在后台休眠十分钟后，页面状态是否仍然可信。
- 接收方处理速度较慢时，内存队列是否无限增长。

因此，Nuntius 的稳定性建设分为四层：

```text
业务状态层
  Thread / Turn / Approval / Command 状态机
                     ▲
可靠消息层
  command_id / event_id / seq / ACK / outbox / replay
                     ▲
传输层
  HTTP(S) + SSE / WS(S) / stdio JSONL
                     ▲
运行层
  进程监管 / Client/Server SQLite / 心跳 / 监控 / 备份
```

连接可以随时断开，但业务会话不能依赖某一条连接的生命周期。

## 4. SSE 与 WSS 的详细比较

### 4.1 能力对比

| 维度 | SSE | WebSocket |
|---|---|---|
| 通信方向 | 服务器到浏览器单向 | 全双工 |
| 浏览器上行 | 另用 HTTP POST/PUT/DELETE | 在同一连接发送 |
| 浏览器原生重连 | `EventSource` 原生支持 | 业务代码自行实现 |
| 续传游标 | 原生支持事件 `id` 和 `Last-Event-ID` | 必须自定义协议 |
| 每条命令的 HTTP 状态 | 有，命令走独立 HTTP 请求 | 没有，需要自定义响应和超时 |
| 消息格式 | UTF-8 文本事件 | 文本或二进制帧 |
| 心跳 | SSE 注释行/事件 | Ping/Pong 加应用心跳 |
| 代理兼容性 | 基于普通 HTTP 流，通常较好；需关闭缓冲 | 通常良好，但依赖 Upgrade 支持 |
| 背压 | 依赖 HTTP 流和应用队列 | 需要显式控制发送队列 |
| 连接恢复 | 规范定义了自动重连行为 | RFC 建议异常关闭后退避重连，但恢复语义由应用定义 |
| 适合场景 | 事件通知、日志、Agent 流式输出 | 双向实时控制、设备隧道、游戏、协作编辑 |

WHATWG 的 EventSource 规范定义了断线重连以及携带 `Last-Event-ID` 的行为，这正适合手机页面恢复事件流。[WHATWG Server-Sent Events](https://html.spec.whatwg.org/multipage/server-sent-events.html)

WebSocket 标准定义了 Close、Ping 和 Pong 控制帧，并建议异常断线后采用逐步增加的重连延迟；但事件游标、消息确认和业务重放仍需应用自己实现。[RFC 6455](https://www.rfc-editor.org/rfc/rfc6455)

### 4.2 为什么浏览器链路选择 HTTP(S) + SSE

手机浏览器的操作模式本质上是：

1. 用户偶尔向服务器提交一条明确命令。
2. 服务器持续向用户推送大量事件。
3. 手机可能锁屏、切换 Wi-Fi/蜂窝网络或被操作系统暂停后台页面。
4. 页面恢复后需要快速得到一份可信状态，而不是假设旧连接仍然有效。

使用 HTTP(S) + SSE 有以下稳定性优势：

- 每条上行命令都是独立 HTTP 请求，可以获得明确的 `2xx/4xx/5xx` 结果。
- 命令可以携带 `Idempotency-Key`，超时后安全重试。
- SSE 原生包含事件 ID 和自动重连机制。
- 手机从后台恢复时，可以先请求状态快照，再从快照游标继续订阅。
- 上传图片、文件等大对象时可以继续使用成熟的 HTTP 上传机制，不阻塞实时事件连接。
- 登录、限流、审计和错误处理可以复用标准 HTTP 中间件。
- 浏览器页面不需要自行维护复杂的 WebSocket 发送队列和请求响应映射。

浏览器发送以下操作时统一使用 HTTP(S) API：

- 创建或恢复会话。
- 发送用户消息。
- `turn/steer` 追加指导。
- 中断 Turn。
- 批准或拒绝操作。
- 新增、编辑或移除项目。
- 查询设备、项目、会话和状态快照。

服务器通过 SSE 推送：

- Agent 消息增量。
- Turn 生命周期事件。
- Item 开始、增量和完成事件。
- 命令执行与工具调用进度。
- 文件修改事件。
- 审批请求及审批状态。
- 设备状态变化。
- 重同步指令。

### 4.3 为什么浏览器链路不首选 WSS

WSS 完全可以实现该功能，但它对当前产品没有形成足够收益，反而会把以下责任全部推给前端：

- 自行实现请求 ID、响应超时和错误码。
- 自行判断一个上行消息断线前是否已送达。
- 自行保存未发送队列并处理页面刷新。
- 自行实现事件游标和缺口重放。
- 自行处理登录过期后的连接更新。
- 自行处理后台休眠后看似连接存在、实际上已失效的问题。

在 Nuntius 中，浏览器上行频率远低于下行事件频率，并不需要真正的对称全双工通道。将命令和事件拆开，反而能获得更清晰的确认与恢复语义。

如果未来加入亚百毫秒级协作编辑、音视频或高频双向输入，再单独评估 WebSocket 或 WebTransport；当前 Codex 控制场景不需要为这些未来能力提前增加复杂度。

### 4.4 为什么设备 CLI 链路选择 WS/WSS

设备 CLI 与公网服务器之间是另一种场景：

- CLI 需要持续上传 Codex 事件。
- 服务器需要随时向 CLI 下发命令和审批结果。
- 连接由内网设备主动发起，需要穿越 NAT 和常见防火墙。
- CLI 是原生程序，可以完整控制握手请求头、Ping/Pong、重连和本地持久化。
- 一台设备可能同时承载多个项目和会话，需要在一条连接上多路复用。

因此，这一段使用 WebSocket 最合适。安全部署使用 WSS；HTTP 兼容部署使用 WS。相比“两个 SSE 加多组 HTTP 请求”，单条 WebSocket 可以自然地承载双向消息，并减少设备侧连接数量。

不首选其他方案的原因：

- **原始 TCP/TLS**：自定义协议和代理穿透成本更高。
- **gRPC 双向流**：类型系统和 HTTP/2 流控很好，但在某些反向代理、企业网络和部署平台上的可达性不如标准 WSS，且浏览器侧并不复用它。
- **QUIC/WebTransport**：能力先进，但运维、代理和生态兼容性不是第一版的最佳选择。
- **MQTT**：自带消息语义，但需要额外 Broker，协议模型也无法直接覆盖 Nuntius 的请求、响应、事件快照和 App Server 适配。

WS/WSS 只是底层载体。Nuntius 必须在其上实现自己的连接会话、ACK、重放、幂等和流控协议。

### 4.5 为什么 CLI 到 Codex App Server 选择 stdio

Codex App Server 官方文档说明：

- App Server 使用省略 `jsonrpc: "2.0"` 字段的 JSON-RPC 风格消息。
- 默认传输是换行分隔的 stdio JSONL。
- WebSocket 传输仍是实验性且不受支持的接口。
- 客户端连接后必须执行 `initialize` 和 `initialized` 握手。
- App Server 提供 Thread、Turn、Item、审批和流式通知。
- 可以按当前 Codex 版本生成 TypeScript 或 JSON Schema。

因此 CLI 应把 `codex app-server` 作为本地受监管子进程，通过 stdin/stdout 通信，不直接对公网暴露 App Server，也不依赖其实验性 WebSocket 传输。[Codex App Server 官方文档](https://learn.chatgpt.com/docs/app-server)

stdio 的优势：

- 不占用本地 TCP 端口。
- 不需要处理端口冲突和本地网络认证。
- CLI 可以明确拥有 App Server 子进程生命周期。
- stdout 专用于协议，stderr 专用于日志，故障边界清晰。
- App Server 退出时可以由 CLI 立即感知并重启。
- Windows、macOS 和 Linux 都支持相同的子进程模型。

## 5. 总体技术架构

```text
┌─────────────────────────────────────────────────────────────┐
│ Mobile / Tablet Browser                                     │
│ React + TypeScript                                          │
│                                                             │
│ HTTP(S) commands ─────────────┐   ┌──── SSE events           │
└───────────────────────────────┼───┼──────────────────────────┘
                                ▼   ▲
┌─────────────────────────────────────────────────────────────┐
│ Public Server                                               │
│ Axum + Tokio + SQLx + SQLite                                 │
│                                                             │
│ HTTP API │ SSE Gateway │ Command Store │ Device Router       │
└───────────────────────────────┬─────────────────────────────┘
                                │ WS(S), full duplex
                                │ device initiated
                                ▼
┌─────────────────────────────────────────────────────────────┐
│ Device CLI / Agent                                          │
│ Tokio + Axum(local) + SQLx/SQLite                           │
│                                                             │
│ WS Client │ Inbox/Outbox │ Project/History │ Process Supervisor│
└───────────────────────────────┬─────────────────────────────┘
                                │ stdio JSONL / JSON-RPC
                                ▼
┌─────────────────────────────────────────────────────────────┐
│ Codex App Server                                            │
│ Thread / Turn / Item / Approval / Stream Events             │
└─────────────────────────────────────────────────────────────┘
```

## 6. 技术框架选型

### 6.1 Rust Workspace

后端和 CLI 使用同一个 Cargo Workspace，但源码边界只保留 `client/` 和 `server/` 两个独立项目：

```text
Cargo.toml                 # 只负责 Workspace 和统一构建参数
client/
├─ src/                    # CLI、后台 Agent、本地 API、App Server adapter
├─ api/                    # 本地页面 OpenAPI
├─ migrations/             # Client SQLite migration
└─ frontend/               # 本地管理页；dist 嵌入 nuntius-client
server/
├─ src/                    # 公网 API、SSE、设备隧道、聚合存储
├─ api/                    # 移动控制页 OpenAPI
├─ migrations/             # Server SQLite migration
└─ frontend/               # 移动控制页；dist 嵌入 nuntius-server
docs/                      # 产品和技术设计，不参与运行时
```

拆分原则：

- 传输层不得直接修改领域状态，必须通过领域服务。
- Client 与 Server 各自保存一份最小协议类型，依靠相同协议版本、线格式测试和 E2E 测试防止漂移；不能在根目录增加第三个共享源码项目。
- App Server 类型只存在于适配层，不泄漏到公网协议。
- 两个二进制各自封装本项目的 SQLite Repository，不跨项目直接访问数据库。
- 状态机转换集中定义，避免多个 Handler 各自解释状态。

### 6.2 异步运行时：Tokio

选择 Tokio 作为 Rust 异步运行时，负责：

- HTTP(S)/SSE/WS(S) 网络任务。
- App Server 子进程 stdin/stdout。
- 心跳、重连、超时和定时清理。
- 有界 channel 和任务协作。
- 进程信号与优雅退出。

每一个长期任务都必须保留 `JoinHandle` 或归属明确的连接生命周期。收到退出信号后，先停止 HTTP 接入和设备隧道，再终止 App Server 子进程，最后取消 maintenance/discovery/event pump；不能让后台任务阻止二进制退出。规模扩大后可把同一模式收敛为 `CancellationToken` + `TaskTracker`。[Tokio Graceful Shutdown](https://tokio.rs/tokio/topics/shutdown)

允许脱离 WS 连接继续完成的只有已经落盘的命令执行任务；结果必须写回 inbox，发送 ACK 失败时依靠重连重放。查询类任务和定时任务必须有界且可取消。

### 6.3 公网 Web 框架：Axum

选择 Axum：

- 与 Tokio、Tower、Hyper 生态直接集成。
- 原生提供 SSE 响应类型和 keep-alive 支持。
- 原生提供 WebSocket Upgrade 和双向流拆分。
- 中间件适合统一实现认证、限流、Trace ID、超时和请求大小限制。
- 类型化 extractor 有利于把协议校验放在进入业务层之前。

Axum 的 SSE 模块直接提供 `Sse`、`Event` 和 `KeepAlive`，WebSocket 模块支持将读写半边拆开并发处理。[Axum SSE](https://docs.rs/axum/latest/axum/response/sse/) / [Axum WebSocket](https://docs.rs/axum/latest/axum/extract/ws/)

建议使用：

- `axum`：HTTP(S) 后端、SSE 和 WS(S) 入口。
- `tower` / `tower-http`：超时、并发限制、追踪、压缩和 CORS。
- `serde` / `serde_json`：公网协议和 App Server JSON。
- `thiserror`：库错误类型。
- `anyhow`：仅用于二进制入口和不可恢复的上下文错误。
- `tracing` / OpenTelemetry：结构化日志、指标和链路追踪。

### 6.4 设备本地 Web 服务

本地页面同样使用 Axum，绑定地址必须限制为：

```text
127.0.0.1
::1
```

本地页面使用 HTTP JSON + SSE。两套前端保持项目级独立，不跨 `client/frontend` 与 `server/frontend` 引用源码；两边分别按各自 OpenAPI 实现恢复逻辑。

不能因为绑定 localhost 就忽略安全：

- 严格检查 `Host`，防止 DNS rebinding。
- 严格检查 `Origin`。
- 当前实现以 loopback bind、严格 Host 和 Origin 校验作为本地安全边界。
- 若未来允许非 loopback 或提升本地隔离，再增加随机本地会话与 HttpOnly/SameSite Cookie；不能在未实现时依赖该层保护。
- 修改操作要求 CSRF Token。
- 不允许绑定 `0.0.0.0`，除非未来提供显式且有认证的局域网模式。

### 6.5 数据库

#### 设备端：SQLite

设备端选择 SQLite，保存：

- 设备配置和项目索引。
- App Server Thread 映射。
- 收到但尚未完成的命令 inbox。
- 尚未确认的事件 outbox。
- 完整历史同步 outbox、每 Thread checkpoint 和 content hash。
- 短期不透明 directory_ref 映射及 allowed root 配置。
- 短期事件重放日志和各流 ACK 游标。
- 进程恢复所需状态。

使用 WAL 模式，使读取状态与写入 outbox 可以更好地并发。SQLite 官方文档说明 WAL 模式下 checkpoint 需要考虑长读事务，因此所有读取必须短事务化，不能让页面查询长期持有读事务。[SQLite WAL](https://sqlite.org/wal.html)

推荐设置：

```text
journal_mode = WAL
synchronous = FULL            # 稳定性优先；性能验证后再评估 NORMAL
foreign_keys = ON
busy_timeout = 5000ms
```

关键命令状态写入必须在事务提交后才能向服务器返回 `device_accepted`。

#### 服务端：SQLite（单活）

服务端第一版同样选择 SQLite，但它与 Client SQLite 是两个完全独立的数据库。选择它的前提是 Server 明确采用单活进程，且用户要求配置、数据库、密钥、日志和备份都收敛在一个指定数据目录。理由是：

- 不依赖外部数据库服务，单二进制和单目录即可部署、迁移与备份。
- 事务、唯一约束和外键足以实现第一版 durable command、幂等键与 History Batch 去重。
- WAL 允许多个短读与单写并行，适合单用户、多设备的负载模型。
- `synchronous=FULL`、显式事务和 ACK-after-commit 提供清晰的崩溃恢复边界。
- 数据目录可整体做一致性备份和恢复演练，运维面显著小于引入独立数据库。

Server SQLite 保存用户、设备、项目索引、命令状态、幂等键、路由游标，以及所有设备同步上来的规范化 Thread、Turn、Item、用户消息和 Agent 回复。高频流式 delta 只作为短期重放数据；Item 完成后保存聚合后的完整正文。SQLite 使用 WAL、`synchronous=FULL`、外键、短事务和 busy timeout；备份必须使用 SQLite backup API 或停写后的 DB/WAL 一致性快照。[SQLite WAL](https://sqlite.org/wal.html)

这个选择明确不支持多个 Server 实例同时挂载同一个数据库文件。只有当真实并发或高可用目标要求多实例时，才把 Server Repository 迁移到 PostgreSQL 等网络数据库；第一版不同时维护两套后端。

数据库访问选择 SQLx：

- Client 与 Server 都使用 SQLite，但 schema、连接池和迁移完全隔离。
- 支持异步连接池和事务。
- 支持参数绑定、migration 和明确事务边界；当前查询使用运行时 `sqlx::query`，由临时真实 SQLite 的 migration/事务测试校验。
- 可以复用同一种可靠性和故障注入方法。

生产构建必须在 CI 中运行 migration 与事务测试；若后续切换 `query!` 宏，再增加 SQLx 离线元数据校验。数据库迁移必须有向前兼容阶段，不能在同一发布中先删除旧版本仍需读取的字段。

### 6.6 前端

建议选择：

- React + TypeScript。
- Vite 作为前端构建工具。
- Bun 负责安装依赖、脚本执行和构建。
- TanStack Query 管理 HTTP 快照、缓存失效和命令 mutation。
- 浏览器原生 `EventSource` 管理 SSE。
- Zod 或生成的 Schema 校验服务端消息。
- CSS Variables 加轻量组件层实现移动端主题和响应式布局。

前端状态分成两类：

- **服务器状态**：设备、项目、Thread/Turn/Item 历史、审批和同步完整度，由 HTTP 快照和 SSE 事件驱动。
- **纯 UI 状态**：当前选中项、面板展开、草稿等，保留在组件状态或轻量 Store。

不得把 SSE 增量直接无约束追加到一个无限数组。Agent 文本增量应合并到当前 Item，完成后固化为最终消息；旧事件只保留渲染所需的聚合结果。

### 6.7 HTTP/HTTPS 入口与代理

安全模式推荐只暴露 TCP 443：

- TLS 1.2 以上，优先 TLS 1.3。
- 使用 Caddy、云负载均衡器或等价的成熟入口负责证书自动续期。
- SSE 路径必须禁用响应缓冲和内容变换。
- SSE 和 WS(S) 路径设置合理的长连接空闲超时。
- Rust Server 仅监听私网或 loopback 端口。

HTTP 兼容模式允许直接暴露配置的 HTTP 端口，但必须：

- 非 loopback 绑定同时设置 `allow_insecure_http = true`。
- Server 启动日志、CLI status 和所有远程页面持续显示不安全标记。
- Cookie 保留 HttpOnly 和 SameSite，但不能设置 Secure；明确提示其可被网络窃听或篡改。
- 设备隧道自动使用 `ws://`，浏览器仍使用 HTTP API + SSE。
- 禁止启用 HSTS，并关闭依赖 Secure Context 的 PWA/Service Worker、WebAuthn 等功能；localhost 可被浏览器视作 potentially trustworthy，但远程普通 HTTP 不属于同一条件。[W3C Secure Contexts](https://www.w3.org/TR/secure-contexts/)
- 推荐只在可信 LAN、WireGuard/Tailscale 类 VPN 或 SSH 隧道内使用。

HTTP 和 HTTPS 复用同一 Axum Router、Schema、幂等和恢复实现，差异集中在 `TransportProfile`、Cookie 属性、URL 派生、安全告警和功能 capability 中。

第一版采用单活 Server 加内置 SQLite，避免过早引入 Redis、消息队列和多活路由带来的运维复杂度。需要水平扩容时，必须先迁移 Server Repository，再评估 NATS JetStream 或等价的持久消息总线；不能让多个实例共享 SQLite 文件。

### 6.8 全局会话历史存储

Server 维护一套跨设备只读历史模型：

```text
User
└─ Device
   └─ Project
      └─ Thread
         └─ Turn
            └─ Item
               └─ Content / Structured Detail
```

- Agent 是本地执行事实的生产者。
- Server 的历史副本是手机跨设备查询和设备离线阅读的权威来源。
- 无法安全映射 cwd 的 Thread 归入每台设备稳定的 system unassigned Project；同步历史但不上传 cwd，归类前只读，从而保持 Project 层级非空且覆盖全部历史。
- 继续 Thread、Steer、Interrupt 和 Approval 仍必须路由到原设备 App Server。
- Item 使用稳定 ID、revision 和 content hash 幂等 upsert。
- 新事件实时增量同步；既有会话由后台 backfill worker 分页补齐。
- 每个 Thread 保存 `history_cursor`、`sync_state`、`last_synced_at` 和 `completeness`。
- 历史同步使用独立低优先级队列，不能挤占 Approval、Interrupt 和实时 Turn 事件。

### 6.9 远程目录选择

手机选定在线 Device 后，通过短期 live query 浏览目录：

```text
Browser -> HTTP directory query
Server -> active WS(S) tunnel
Agent -> allowed-root policy + directory listing
Agent -> signed opaque directory_ref
Server -> Browser
Browser -> project.create(directory_ref)
Agent -> resolve + revalidate + create Project
```

目录浏览只返回目录名、是否可进入、Git 标记、权限摘要和短期引用，不返回文件正文。Agent 默认只暴露本地配置的 workspace roots，并显式排除 `.ssh`、钥匙串、Nuntius/Codex 状态目录等敏感位置。创建 Project 时不能相信旧列表结果，必须重新解析引用并检查目录仍在允许根下。

## 7. 可靠消息协议

### 7.1 基本原则

1. TCP、WS/WSS 或 SSE 连接断开不等于业务会话结束。
2. 任何需要执行副作用的命令必须有全局唯一 `command_id` 和 `idempotency_key`。
3. 任何需要恢复顺序的事件流必须有单调递增 `seq`。
4. 接收方必须先持久化，再确认接收。
5. 发送方只有收到 ACK 后才允许清理 outbox。
6. 重放采用至少一次投递，接收方负责去重。
7. 不宣称端到端 exactly-once；对无法判定的操作进入 `unknown`，禁止盲目重试。

### 7.2 命令 Envelope

```json
{
  "version": 1,
  "commandId": "cmd_01...",
  "idempotencyKey": "idem_01...",
  "userId": "usr_01...",
  "deviceId": "dev_01...",
  "projectId": "prj_01...",
  "threadId": "thr_01...",
  "kind": "turn.start",
  "issuedAt": "2026-07-18T10:00:00Z",
  "expiresAt": "2026-07-18T10:05:00Z",
  "payload": {}
}
```

要求：

- `commandId` 使用 UUIDv7 或等价的时序唯一 ID。
- `idempotencyKey` 在同一用户和操作作用域内建立唯一约束。
- 所有可产生副作用的 POST 都要求幂等键。
- `expiresAt` 防止设备长时间离线后执行过期命令。
- 审批和中断命令使用较短有效期。
- 设备端 inbox 对 `commandId` 建立唯一索引。

### 7.3 事件 Envelope

```json
{
  "version": 1,
  "eventId": "evt_01...",
  "streamId": "dev_01.../thr_01.../turn_01...",
  "seq": 42,
  "causationId": "cmd_01...",
  "correlationId": "turn_01...",
  "kind": "item.agent_message.delta",
  "durability": "replayable",
  "occurredAt": "2026-07-18T10:00:01Z",
  "payload": {}
}
```

要求：

- `streamId + seq` 唯一。
- `seq` 由最靠近事实来源的设备 CLI 生成。
- 浏览器按 `streamId + seq` 去重并检测缺口。
- `causationId` 关联触发该事件的命令。
- `correlationId` 关联整个 Turn 或审批流程。
- 协议版本与事件类型分开版本化。

### 7.4 事件分级

| 等级 | 示例 | 策略 |
|---|---|---|
| Durable | 命令接受、Turn 完成、审批结果、归档结果 | 必须持久化、ACK、重放，不可丢弃 |
| Replayable | Agent delta、命令输出 delta、工具进度 | 设备本地短期保存，可合并，可按游标重放 |
| Snapshot-only | 当前设备负载、队列深度 | 丢失后通过新快照恢复 |
| Best-effort | typing、瞬时 UI 提示 | 过载时允许丢弃 |

禁止因为 Agent delta 流量过大而丢弃 Turn 完成、审批和错误事件。

## 8. 命令投递与 Outbox/Inbox

### 8.1 手机命令到设备

完整流程：

```text
Browser
  │ POST + Idempotency-Key
  ▼
Server HTTP API
  │ Server SQLite transaction:
  │ 1. 校验权限
  │ 2. 插入 command（该行同时是 durable dispatch source）
  │ 3. commit
  ▼
202 Accepted + command_id
  │
Server Router ──WS(S)──> Device CLI
                       │ SQLite transaction:
                       │ 1. inbox 去重
                       │ 2. 保存 command
                       │ 3. commit
                       ▼
                 device_accepted ACK
                       │
                       ▼
                  App Server Adapter
```

第一版不再为相同 payload 维护一张重复的 `server_outbox`：`commands` 的非终态行就是待投递集合，连接建立时按 `server_sequence` 重放。只有 Server SQLite 事务提交成功后，HTTP API 才能返回 `202 Accepted`。如果浏览器超时，可以使用同一个幂等键重试；服务端返回原 `command_id`，不能创建第二条命令。

只有设备 SQLite 提交成功后，设备才能确认 `device_accepted`。服务器未收到该 ACK 时允许重发；设备通过唯一索引去重。

### 8.2 设备事件到手机

```text
Codex App Server
  │ stdout notification
  ▼
Device CLI
  │ SQLite transaction:
  │ 1. 分配 stream seq
  │ 2. 写入 local_event_log/outbox
  │ 3. commit
  ▼
WS(S) send/replay
  ▼
Server Router
  │ SSE event: id=<stream cursor>
  ▼
Browser
```

服务端完整保存规范化的会话历史，但不把每一个流式 delta 永久保存为历史行：

- 用户消息、最终 Agent 消息正文必须持久化。
- Turn/Item 生命周期、命令/工具/文件变化和审批保存结构化记录及受控内容。
- 高频 delta 在 Item 完成前用于 SSE 和短期重放；完成时折叠成最终 Item 内容。
- 大命令输出按大小策略完整保存、压缩/外置或明确截断，截断必须可见，不能静默丢失。

设备在本地保留可重放事件和历史同步记录，直到：

- Turn 已进入终态；并且
- 服务端确认收到；并且
- 超过最低保留窗口。

建议 Replayable Event 第一版最低保留 24 小时，最终值通过磁盘占用和恢复测试确定。Codex 本地状态负责继续执行和原始恢复；Server SQLite 负责跨设备完整历史阅读。

### 8.3 命令状态机

```text
received
   │ Server SQLite committed
   ▼
queued
   │ sent over WS(S)
   ▼
routed
   │ device inbox committed
   ▼
device_accepted
   │ App Server dispatch
   ▼
applying ────────> completed
   │                 failed
   │                 rejected
   └───────────────> unknown
```

`unknown` 是必要状态，不是普通错误。当 CLI 无法判断某个非幂等 App Server 请求是否已经生效时，必须停止自动重试并执行状态核对。

## 9. 各链路的断线恢复

### 9.1 手机浏览器 ↔ 服务器

#### 正常连接

1. 页面先调用 `GET /api/sync` 获取设备、项目、Thread/Turn 快照和 `snapshot_cursor`。
2. 页面建立 `/api/events?after=<snapshot_cursor>` 的 EventSource。
3. 服务端先补发 cursor 之后的事件，再切换到实时流。
4. 页面按 `streamId + seq` 去重。

#### SSE 断线重连

- 每条 SSE 都设置 `id`。
- 浏览器自动重连时携带 `Last-Event-ID`。
- SSE 每 15 秒发送注释 keep-alive，避免中间代理因无流量关闭连接。
- 服务端不得把 keep-alive 当业务在线状态；页面活跃状态另行统计。
- 如果游标仍在可重放范围内，服务端补发缺失事件。
- 如果游标太旧，服务端发送 `resync_required` 事件并关闭流。
- 页面收到 `resync_required` 后重新请求 `/api/sync`，不尝试猜测缺失状态。

#### 手机后台与网络切换

移动系统可能直接暂停 JavaScript，因此不能依赖后台页面持续发送心跳。

页面在 `visibilitychange` 恢复可见、`online` 事件触发或 SSE 重新建立后：

1. 将当前 UI 标记为“正在同步”。
2. 请求轻量状态快照。
3. 对比服务器 cursor 和本地 cursor。
4. 补发或全量同步。
5. 状态收敛后再允许发送危险操作。

不要用 Service Worker 维持长期 SSE 连接；仅在 HTTPS 或浏览器认可的 localhost secure context 中，将 Service Worker 用于静态资源缓存和未来的推送通知。远程 HTTP 模式不注册它。

### 9.2 设备 CLI ↔ 公网服务器

#### WS/WSS 握手

CLI 连接成功后发送：

```json
{
  "type": "hello",
  "protocolVersion": 1,
  "deviceId": "dev_01...",
  "instanceId": "ins_01...",
  "connectionEpoch": 18,
  "lastServerCommandSeq": 105,
  "eventAcks": {
    "stream-a": 42,
    "stream-b": 9
  },
  "historyCursors": {
    "thr_01": "hcur_..."
  },
  "transportSecurity": "secure",
  "capabilities": ["history.v1", "directory-browser.v1"]
}
```

服务端返回：

- 协商后的协议版本。
- 新的连接会话 ID。
- 服务端已确认的事件 cursor。
- 尚未被设备确认的命令列表或命令 cursor。
- Server 已确认的 history checkpoint 和回填请求。
- 协商后的 transport security/capability；两端判断不一致时拒绝 online。
- 是否要求完整 resync。

#### 心跳和半开连接

- 每 15 秒发送 WebSocket Ping。
- 45 秒没有收到 Pong 或任何有效应用消息时判定连接失效。
- 每 30 秒发送应用级 heartbeat，包含当前连接 epoch、App Server 状态和队列摘要。
- TCP keepalive 作为更慢的底层兜底，不能替代应用心跳。
- 收到新连接后，服务端使同一设备的旧 connection epoch 失效，避免双主连接同时消费命令。

具体时间值必须可配置，并根据真实公网和代理环境调整。

#### 重连退避

使用带 full jitter 的指数退避：

```text
base = 500ms
cap  = 30s
delay = random(0, min(cap, base * 2^attempt))
```

- 认证失败不无限快速重试，应进入凭证刷新或人工重新配对流程。
- 协议版本不兼容时停止重试并报告升级要求。
- DNS、连接超时和 5xx 可以退避重试。
- secure 档位 TLS/证书错误可以退避或提示修复，但绝不能自动改连 `ws://`。
- 服务器返回明确 `retry_after` 时优先遵守。
- 网络恢复事件可以触发一次立即重试，但仍需防止重连风暴。

#### 重放

WS/WSS 重连后：

1. CLI 与服务端交换各自 ACK cursor。
2. 服务端重发设备未确认的 Durable Command。
3. CLI 重发服务端未确认的 Durable/Replayable Event。
4. 双方按唯一 ID 去重。
5. 完成控制命令和高优先级事件游标同步后，设备状态从 `syncing` 进入 `online`。
6. 历史 backfill 按独立 cursor 在 online 后以 P3/P4 继续，不能延迟审批和当前 Turn。

设备处于 `connecting` 或 `syncing` 时，页面不能显示为完全在线。

### 9.3 CLI ↔ Codex App Server

CLI 作为 App Server supervisor：

1. 使用 `tokio::process::Command` 启动 `codex app-server --listen stdio://`。
2. stdin、stdout、stderr 分开处理。
3. 完成 `initialize` 请求和 `initialized` 通知。
4. 记录 App Server 版本、能力和 Schema 版本。
5. 只有初始化完成后才开始分发业务命令。

#### 子进程异常退出

- 立即停止向旧 stdin 写入。
- 所有尚未获得 JSON-RPC 响应的请求进入待核对状态。
- 使用带 jitter 的退避重启，防止崩溃循环。
- 达到失败阈值后熔断，并向页面报告 `app_server_unavailable`。
- 重启成功后重新执行初始化。
- 通过 App Server Thread API 重新读取或恢复会话状态。
- 重新建立 Thread 与本地项目映射。

不能承诺正在运行的 Turn 一定跨 App Server 进程重启继续。若官方协议无法证明请求结果：

- 不自动重复发送可能产生副作用的 `turn/start`。
- 将命令标记为 `unknown`。
- 查询 Thread/Turn 当前状态进行核对。
- 无法核对时在 UI 中明确提示用户，而不是静默重复执行。

#### 协议健壮性

- 每行 stdout 必须设置最大长度。
- 非法 JSON、未知响应 ID、重复响应和未知通知都要记录指标。
- 未知通知默认记录并忽略，不能使整个 CLI 崩溃。
- stderr 只进入脱敏日志，不能作为协议输入。
- 每个 JSON-RPC 请求有超时，但超时不等于请求没有生效。
- App Server 返回过载错误时使用指数退避和 jitter，不能立即循环重试。

## 10. 幂等与 Exactly-once 边界

分布式系统无法仅依靠网络协议保证端到端 exactly-once。Nuntius 的承诺应是：

| 操作 | 投递语义 | 处理策略 |
|---|---|---|
| 浏览器到服务器的命令 | 至少一次 | Idempotency-Key + Server SQLite 唯一约束 |
| 服务器到设备的命令 | 至少一次 | Device inbox + command_id 去重 |
| 设备到服务器的事件 | 至少一次 | stream seq + event_id 去重 |
| 服务器到浏览器的事件 | 至少一次 | SSE id + Last-Event-ID + 前端去重 |
| CLI 到 App Server 请求 | 依 App Server 方法而定 | 不确定时标记 unknown，不盲目重试 |

### 10.1 安全重试的操作

- 查询设备、项目和会话快照。
- 获取命令状态。
- 使用同一幂等键重新提交尚未确认的 HTTP 命令。
- WS/WSS Durable 消息重发。
- SSE 事件重放。

### 10.2 不得盲目重试的操作

- 已经发送给 App Server 但未收到响应的 `turn/start`。
- 无法判断是否已生效的审批决定。
- 任何可能重复执行 shell 命令或文件修改的操作。
- Thread 创建结果未持久化时再次创建并立即发送相同 Turn。

对于新建 Thread，可以先创建空 Thread，成功保存 `app_server_thread_id` 后，再发送首个 Turn。这样即使创建响应后的持久化窗口发生故障，最多产生一个可清理的空 Thread，不会重复执行用户任务。

## 11. 顺序、并发与会话隔离

### 11.1 顺序保证

- 单个 `streamId` 内严格按 `seq` 应用。
- 不承诺不同 Thread 之间的全局顺序。
- 控制事件和 delta 事件使用同一 Turn stream，确保 `turn.completed` 不会在缺失前置事件时被提前应用。
- 若发现 seq 缺口，前端暂停应用该 stream 的后续事件并请求 replay 或 snapshot。

### 11.2 命令串行化

每个 Thread 使用一个逻辑 actor/串行命令队列：

- `turn.start`、`turn.steer`、`turn.interrupt` 和审批按 Thread 排序。
- 不同 Thread 可以并行。
- 项目级修改操作按 Project 串行。
- 设备级启动、停止和升级操作按 Device 串行。

### 11.3 多标签页

- 多个浏览器标签页可以同时订阅。
- 命令幂等不能依赖单个页面内存。
- 每个标签页有独立 `client_instance_id`。
- 危险审批使用 Compare-And-Set：只有状态仍是 `pending` 时才能提交结果。
- 一个标签页完成审批后，其他标签页通过 SSE 收到终态并禁用按钮。

## 12. 背压与过载保护

所有异步队列必须有界。禁止使用无限 channel 保存网络事件。

建议优先级：

```text
P0：认证失效、审批、interrupt、turn completed、致命错误
P1：命令 ACK、生命周期、item completed、当前历史终态、目录 live query
P2：Agent 文本 delta、命令输出 delta、工具进度
P3：最近 Thread 历史回填
P4：旧归档历史回填、presence、统计和临时 UI 提示
```

处理策略：

- P0/P1 不允许静默丢弃；队列满时暂停低优先级生产者或拒绝新命令。
- P2 可以合并相邻 delta，但必须保留最终聚合结果。
- P3/P4 回填可暂停但不能伪造 ACK；presence、统计和临时提示可在过载时丢弃。
- 每个设备、用户、Thread 设置独立队列和配额，避免单个 Turn 拖垮全局。
- WS/WSS 发送器与接收器分任务运行，但共享取消令牌和连接 epoch。
- SSE 慢消费者超过缓冲阈值时发送 `resync_required` 并主动断开，而不是无限积压。
- HTTP 命令入口在过载时返回 `429` 或 `503` 和 `Retry-After`。
- 单条消息、单个 payload、单个上传和单分钟事件数都必须有限制。

## 13. 状态快照与最终一致性

事件流只负责快速更新，状态快照负责纠正错误。

页面必须定期或在以下场景请求快照：

- 首次加载。
- SSE 重连完成。
- 页面从后台恢复。
- 发现 seq 缺口。
- 收到 `resync_required`。
- 操作超时或命令进入 `unknown`。
- 设备重新上线。

快照至少包含：

- 设备连接状态和 `connection_epoch`。
- App Server 状态。
- 当前项目和 Thread 摘要。
- 活跃 Turn 状态。
- 待审批列表。
- 最近命令状态。
- 每个事件流的 cursor。

UI 中的状态必须带 freshness：

```text
live       已通过当前连接确认
syncing    正在重放或请求快照
stale      来自缓存，设备当前离线
unknown    无法判断操作是否已生效
```

## 14. 安全设计

### 14.1 浏览器认证

原生 EventSource 不适合携带自定义 Authorization Header，因此远程页面使用：

- HTTPS 模式使用 Secure + HttpOnly + SameSite Cookie。
- HTTP 模式使用 HttpOnly + SameSite Cookie，但不能声称具备网络机密性或完整性；页面必须显示永久风险横幅。
- 同源部署，不开放宽泛 CORS。
- 所有修改请求要求 CSRF Token。
- SSE 连接校验用户会话和资源权限。
- 会话过期时终止 SSE，并由页面完成重新登录。

### 14.2 设备认证

建议配对时在设备本地生成不可导出的 Ed25519 密钥对：

1. 配对码只在短时间内有效且只能使用一次。
2. 服务器保存设备公钥。
3. CLI 使用私钥对服务器 challenge 签名。
4. 服务器签发短期设备访问令牌。
5. CLI 在 WS/WSS Upgrade 请求中使用 `Authorization: Bearer`。
6. 设备被撤销后，服务器拒绝新连接并关闭旧连接。

私钥保存在系统钥匙串；不可用时使用权限严格的本地密钥文件，并在 UI 中明确安全等级。

### 14.3 历史数据安全与最小化

- Server SQLite 长期保存用户消息、Agent 回复和结构化会话历史，这是明确产品能力而非临时缓存。
- 不保存项目源代码和任意文件正文；文件 Item 只保存路径、操作、摘要和按策略允许的 diff。
- 原始逐 token delta 完成后折叠，避免无意义重复存储。
- 命令输出设置清晰的大小、压缩、外置或截断策略，UI 必须标记完整性。
- 日志默认不记录消息 payload。
- 指标只记录类型、大小、耗时、状态和匿名标识。
- 临时事件缓冲必须有 TTL 和容量上限。
- 崩溃报告在上传前脱敏。

## 15. 版本兼容性

### 15.1 Nuntius 协议

WS/WSS hello 交换：

- `protocolVersion`。
- CLI 版本。
- Server 最低/最高兼容版本。
- 支持的事件和命令能力。

兼容策略：

- 新增可选字段保持向后兼容。
- 未知字段必须忽略。
- 未知 Durable 命令不能忽略，必须返回 `unsupported_command`。
- 危险语义变化必须升级主协议版本。
- Server 至少支持当前版本和前一个版本，给 CLI 留出升级窗口。

### 15.2 App Server 协议

- 构建或测试时运行 `codex app-server generate-json-schema`。
- 将支持的 Codex 版本与生成 Schema 建立映射。
- App Server 启动后记录实际版本和能力。
- 版本不兼容时禁止执行写操作，但仍允许诊断和升级。
- 实验性 App Server 字段放在独立 feature gate 中。
- 不直接解析 Codex 内部 SQLite 文件，避免内部存储升级破坏兼容性。

## 16. 进程监管与优雅退出

### 16.1 CLI Supervisor

CLI 顶层 supervisor 管理：

- 本地 HTTP/SSE 服务。
- WS/WSS 连接管理器。
- Command inbox worker。
- Event outbox worker。
- App Server 子进程。
- SQLite checkpoint/清理任务。

任一任务异常结束必须通知 supervisor。关键任务不能静默退出。

### 16.2 优雅退出顺序

```text
1. 状态切换为 draining
2. 停止接受新的本地/远程命令
3. 通知服务器设备正在下线
4. 等待正在提交的 Client/Server SQLite 事务
5. 刷新内存中的 durable outbox
6. 关闭 App Server stdin，等待子进程退出
7. 发送 WS/WSS Close
8. 关闭数据库连接池
9. 超时后强制退出并保留恢复标记
```

服务端发布和重启时同样进入 draining：停止接受新的 WS/WSS 设备连接，将 HTTP readiness 置为失败，完成短期请求后关闭连接。设备使用 jitter 重连，避免同时冲击新实例。

## 17. 故障场景矩阵

| 故障 | 检测方式 | 恢复策略 | 用户表现 |
|---|---|---|---|
| 手机切换网络 | SSE error/online 事件 | EventSource 重连 + cursor replay | 短暂显示“正在同步” |
| 手机页面被系统休眠 | visibilitychange + 快照时间过期 | 恢复可见后先 sync | 旧状态标记 stale |
| SSE 代理超时 | keep-alive/连接关闭 | Last-Event-ID 重连 | 自动恢复 |
| 公网服务器重启 | WS(S)/SSE 同时断开 | Browser SSE 重连；CLI jitter 重连 | 短暂离线后收敛 |
| Server SQLite busy/只读/损坏 | 健康检查/事务错误 | 不接受新命令，返回 503 | 不显示虚假成功 |
| 设备断网 | 心跳超时 | CLI 本地继续保留 outbox，恢复后重放 | 设备标记离线 |
| CLI 崩溃 | WS(S) 断开/操作系统服务管理器 | 自动重启，SQLite 恢复 inbox/outbox | 设备重新同步 |
| App Server 崩溃 | child exit/stdout EOF | 退避重启、initialize、状态核对 | 当前操作可能显示 unknown |
| 重复命令 | command_id 唯一约束 | 返回已存在结果 | 不重复执行 |
| 事件乱序或缺口 | stream seq | 缓存、重放或请求快照 | 页面短暂 syncing |
| 慢浏览器 | SSE 队列达到阈值 | resync_required 后断开 | 重载快照，不拖垮服务端 |
| 磁盘满 | Client/Server SQLite 写失败 | 停止确认、进入 degraded | 明确提示，不丢命令 |
| 版本不兼容 | hello/initialize 能力检测 | 拒绝写操作并提示升级 | 可诊断，不冒险执行 |

## 18. 可观测性

### 18.1 结构化关联字段

所有日志和 Trace 使用：

- `request_id`
- `command_id`
- `event_id`
- `device_id`
- `project_id`
- `thread_id`
- `turn_id`
- `connection_id`
- `connection_epoch`
- `protocol_version`

不记录 prompt、完整命令输出、文件正文、Token 或私钥。

### 18.2 核心指标

- 当前在线、连接中、同步中设备数。
- WS/WSS 建连成功率、传输安全档位和重连次数。
- SSE 在线连接数和重连次数。
- 命令从 HTTP accepted 到 device accepted 的延迟。
- 命令从 device accepted 到终态的延迟。
- inbox/outbox 深度及最老消息年龄。
- 每个事件流的 ACK lag。
- replay 数量和 resync_required 数量。
- App Server 重启次数、初始化失败次数和未知响应数。
- Client/Server SQLite 事务失败率。
- 各优先级队列使用率和丢弃/合并数量。
- 历史批次写入率、backfill 最老积压、partial Thread 数和 checkpoint age。
- 目录 live query 延迟、超时率和并发拒绝数。

### 18.3 健康检查

服务端：

- `/healthz`：进程存活。
- `/readyz`：可以接受新请求，数据库可用，迁移版本兼容。

CLI：

- 本地状态 API 展示 CLI、Server Connection、App Server 和 SQLite 四层状态。
- 当前 `status` 与本地 `/api/v1/info` 输出脱敏状态；`doctor` 诊断包属于后续范围。

健康检查不能只返回进程是否存在，必须区分 degraded 和 ready。

## 19. 稳定性测试策略

### 19.1 单元与性质测试

- Command 和 Event 状态机转换测试。
- 幂等键唯一性和重复投递测试。
- seq 去重、缺口和乱序测试。
- 指数退避范围测试。
- 协议向后兼容测试。
- 使用 property-based testing 生成重复、乱序和丢失消息序列。

### 19.2 集成测试

- Mock App Server 验证 JSON-RPC 初始化、响应、通知和异常输出。
- 真 Codex App Server 的版本兼容 smoke test。
- Server/Client SQLite 事务崩溃恢复测试。
- Browser EventSource 断线与 Last-Event-ID 重放测试。
- WS/WSS 握手、心跳、连接 epoch 和双连接竞争测试。
- HTTP/WS 与 HTTPS/WSS 功能等价、风险 capability 和禁止静默降级测试。
- 远程目录选择、`directory_ref` 安全边界和 Project 创建 E2E。
- 完整历史实时同步、既有历史回填、离线读取和重复/乱序批次测试。

### 19.3 故障注入

在以下时间点强制 kill 进程：

- Server 写入 command 前后。
- Server 返回 202 前后。
- Device 写入 inbox 前后。
- Device ACK 前后。
- CLI 向 App Server 写入请求前后。
- CLI 收到 App Server 响应前后。
- Device 写入 event outbox 前后。

模拟：

- 丢包、延迟、抖动、重复和连接重置。
- Wi-Fi 与蜂窝网络切换。
- 反向代理 idle timeout。
- Server SQLite 写锁竞争、主机重启、磁盘只读和连接池耗尽。
- SQLite busy、磁盘满和 WAL checkpoint 延迟。
- 浏览器慢消费者和后台冻结。
- CLI/App Server 崩溃循环。

可以使用 Toxiproxy、Linux `tc/netem` 或等价工具，但测试断言必须基于业务状态，不只检查连接是否恢复。

### 19.4 长稳测试

- 至少 24 小时持续运行多个并发 Thread。
- 周期性断开 WS/WSS/SSE。
- 周期性重启 Server、CLI 和 Mock App Server。
- 检查内存、文件句柄、任务数、SQLite WAL 和 outbox 是否无界增长。
- 检查最终 Thread/Turn 状态与事件聚合结果一致。
- 检查 Server History 与 Agent 权威历史的稳定 ID/revision/content hash 一致。

## 20. 初始稳定性目标

以下为第一版工程目标，后续根据真实数据调整：

| 指标 | 初始目标 |
|---|---|
| HTTP 返回 202 后命令在服务端丢失 | 0 |
| Durable Command 重复导致重复业务执行 | 0 |
| 稳定网络下 SSE 意外断开后的恢复时间 | P95 < 5 秒 |
| 稳定网络下设备 WS/WSS 恢复时间 | P95 < 10 秒 |
| Server 滚动重启后设备状态重新收敛 | P95 < 30 秒 |
| CLI 重启后 inbox/outbox 恢复 | 100% |
| 事件缺口被静默忽略 | 0 |
| App Server 不确定请求被自动盲目重试 | 0 |
| 单连接队列无界增长 | 0 |
| 历史回填导致 P0/P1 操作超出目标延迟 | 0 |
| 已标记 complete 的 Thread 缺失已发现 Item | 0 |

这些是工程验收目标，不是第一版对外 SLA。

## 21. 发布与运维

### 21.1 服务端部署

- 单活 Rust Server。
- 启动时必须传入固定 `--data-dir`；`config.toml`、SQLite DB/WAL、`secrets/`、`logs/`、`run/` 和 `backups/` 全部位于该目录，不读取散落的系统路径。
- 数据目录内置 SQLite 文件，禁止两个 Server 进程同时使用同一目录。
- 推荐成熟 TLS 入口代理；无 TLS 时允许显式 HTTP/WS 兼容入口并持续告警。
- systemd 或容器编排负责进程重启。
- 数据库每日备份并定期验证恢复。
- 发布前运行数据库迁移兼容检查。
- 服务端先兼容新旧 CLI，再发布新 CLI，最后移除旧协议。

### 21.2 CLI 发布

- Rust 单二进制加嵌入式前端资源。
- 当前发布物是 Cargo 构建的两个单二进制；发布包签名、校验和分发、自动更新、更新前 drain 与自动回滚属于后续发布工程。
- Codex 版本不兼容时给出明确诊断。

### 21.3 暂不引入的基础设施

第一版不强制引入：

- Redis。
- Kafka。
- 多区域部署。
- 多活 Server。
- Kubernetes。
- 自建 PKI 和强制 mTLS。

这些组件并不会天然提高单用户系统的稳定性，反而增加故障点。只有当并发、跨实例路由或可用性数据证明需要时再引入。

## 22. 分阶段实施顺序

### 阶段一：协议与本地闭环

- 定义 Command/Event/ACK Envelope。
- 实现本地 HTTP + SSE 页面。
- 实现 App Server stdio adapter 和 supervisor。
- 实现 SQLite inbox/outbox。
- 验证 Thread 创建、恢复、消息、审批和中断。

### 阶段二：公网设备链路

- 实现设备配对和短期令牌。
- 实现 WS/WSS hello、heartbeat、epoch、ACK 和 replay。
- 实现 secure/trusted-http 档位和不降级约束。
- 实现单活 Server SQLite durable command store。
- 完成单设备远程控制。

### 阶段三：移动端恢复

- 实现 `/api/sync` 快照。
- 实现 SSE cursor、Last-Event-ID 和 resync_required。
- 处理后台恢复、网络切换和多标签页。
- 实现移动端审批。
- 实现受控目录浏览、短期 `directory_ref` 和远程 Project 创建。

### 阶段四：全局历史

- 实现规范化 Thread/Turn/Item History Store。
- 实现实时历史批次、checkpoint、revision/content hash 去重。
- 实现已有历史低优先级 backfill、完整度和设备离线阅读。

### 阶段五：多设备与长稳

- 多设备和多 Thread 并发。
- 背压、优先级和限流。
- 故障注入和 24 小时长稳测试。
- 指标、告警、备份和恢复演练。

## 23. 最终决策

Nuntius 的协议选择为：

1. **手机/平板到公网服务器**：HTTPS 为推荐模式；HTTP 为显式兼容模式。两者都使用 JSON API 发送命令。
2. **公网服务器到手机/平板**：使用带事件 ID 的 SSE，运行在对应 HTTP 或 HTTPS 连接上。
3. **本地浏览器到设备 CLI**：复用 localhost HTTP JSON + SSE 模型。
4. **设备 CLI 到公网服务器**：HTTPS 部署使用 WSS；HTTP 部署使用 WS。
5. **设备 CLI 到 Codex App Server**：使用默认 stdio JSONL/JSON-RPC。
6. **历史存储**：Server 完整保存规范化会话历史；设备保存 Codex 可执行状态并可靠回填既有历史。
7. **可靠性语义**：持久化 outbox/inbox、至少一次投递、幂等去重、游标重放、状态快照和显式 `unknown`，不虚假承诺 exactly-once。

这个组合充分利用了每种协议最成熟的部分：SSE 负责移动浏览器可恢复的下行事件流，HTTP(S) 负责可确认、可幂等的用户命令，WS(S) 负责设备与服务器之间真正需要的双向长连接，stdio 负责 CLI 与本地 App Server 之间最简单、最可控的进程通信。HTTP 模式保证功能兼容，但只有 HTTPS/WSS 才提供标准 TLS 安全属性。
