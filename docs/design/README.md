# Nuntius 详细设计索引

> 当前后端 `0.1.0` 的逐模块完成情况、实际协议和后续边界见 [后端实现状态与设计边界](../implementation-status.md)。本目录是完整目标设计；标为后续范围的能力不是当前代码已经具备的能力。

本目录将 [PRD](../prd.md) 和 [技术架构](../tech.md) 展开为可直接指导实现、测试和验收的子领域设计。每个子领域都包含：

- `functional.md`：用户能力、业务规则、状态、流程、异常和验收标准。
- `technical.md`：组件边界、数据模型、API/命令/事件、并发一致性、故障恢复、安全和测试。

跨模块命令、事件、状态投影和端到端时序统一定义在 [跨模块契约](./contracts.md)。实现单个模块前应同时阅读该契约，避免局部设计正确但全链路无法闭环。

## 设计原则

1. **简洁有效**：第一版采用模块化单体 Server 和单体 Agent；两端只复制并通过 wire test 对齐必要协议类型，不建立第三个共享源码项目，不拆微服务。
2. **双层真相**：设备是项目路径和 Codex 可执行状态的权威；Server 是跨设备完整历史查询的权威，并从设备可靠同步。
3. **连接不是会话**：浏览器、Server、Agent 或 App Server 断线，不直接删除业务会话。
4. **先持久化再确认**：有副作用的命令在可靠存储提交后才返回已接受。
5. **至少一次加幂等**：不虚假承诺 exactly-once；无法判定的执行进入 `unknown`。
6. **快照纠正事件**：事件流用于实时更新，状态快照用于恢复和最终收敛。
7. **稳定接口隔离变化**：Codex App Server 协议只存在于适配层，不泄漏到公网协议和 UI。
8. **传输档位显式**：HTTPS/WSS 是安全默认；HTTP/WS 只保证功能兼容，必须显式开启、持续告警且禁止静默降级。
9. **归属不悬空**：每个 Thread 都属于一个 Device 和 Project；无法匹配 cwd 时使用无路径、只读的 system unassigned Project。

## 子领域列表

| 编号 | 子领域 | 功能设计 | 技术设计 |
|---|---|---|---|
| 00 | 系统基础与全链路 | [functional](./00-system-foundation/functional.md) | [technical](./00-system-foundation/technical.md) |
| 01 | 用户身份与访问控制 | [functional](./01-identity-access/functional.md) | [technical](./01-identity-access/technical.md) |
| 02 | 设备管理 | [functional](./02-device-management/functional.md) | [technical](./02-device-management/technical.md) |
| 03 | 设备 Agent 与 CLI 运行时 | [functional](./03-agent-runtime/functional.md) | [technical](./03-agent-runtime/technical.md) |
| 04 | Codex App Server 适配 | [functional](./04-app-server-adapter/functional.md) | [technical](./04-app-server-adapter/technical.md) |
| 05 | 项目管理 | [functional](./05-project-management/functional.md) | [technical](./05-project-management/technical.md) |
| 06 | Thread 会话管理 | [functional](./06-thread-management/functional.md) | [technical](./06-thread-management/technical.md) |
| 07 | Turn、消息与执行事件 | [functional](./07-turn-messaging/functional.md) | [technical](./07-turn-messaging/technical.md) |
| 08 | 审批 | [functional](./08-approval/functional.md) | [technical](./08-approval/technical.md) |
| 09 | 浏览器命令与查询 API | [functional](./09-browser-api/functional.md) | [technical](./09-browser-api/technical.md) |
| 10 | 浏览器 SSE 事件流 | [functional](./10-browser-events/functional.md) | [technical](./10-browser-events/technical.md) |
| 11 | 设备 WS/WSS 隧道 | [functional](./11-device-tunnel/functional.md) | [technical](./11-device-tunnel/technical.md) |
| 12 | 可靠消息与状态同步 | [functional](./12-reliable-messaging/functional.md) | [technical](./12-reliable-messaging/technical.md) |
| 13 | 本地控制台 | [functional](./13-local-console/functional.md) | [technical](./13-local-console/technical.md) |
| 14 | 远程移动控制台 | [functional](./14-remote-console/functional.md) | [technical](./14-remote-console/technical.md) |
| 15 | 数据存储与生命周期 | [functional](./15-storage-lifecycle/functional.md) | [technical](./15-storage-lifecycle/technical.md) |
| 16 | 安全 | [functional](./16-security/functional.md) | [technical](./16-security/technical.md) |
| 17 | 可观测性与诊断 | [functional](./17-observability/functional.md) | [technical](./17-observability/technical.md) |
| 18 | 安装、发布与运维 | [functional](./18-release-operations/functional.md) | [technical](./18-release-operations/technical.md) |
| 19 | 测试与质量保障 | [functional](./19-testing-quality/functional.md) | [technical](./19-testing-quality/technical.md) |
| 20 | 会话历史汇总与同步 | [functional](./20-history-aggregation/functional.md) | [technical](./20-history-aggregation/technical.md) |
| 21 | 远程目录浏览与项目创建 | [functional](./21-directory-browser/functional.md) | [technical](./21-directory-browser/technical.md) |

## 模块依赖方向

```text
系统基础
  ├─ 身份 ──> 设备 ──> 项目 ──> Thread ──> Turn ──> 审批
  ├─ Agent ──> App Server Adapter
  ├─ Browser API ──> Browser SSE
  ├─ Device Tunnel ──> Reliable Messaging ──> Storage
  ├─ Directory Browser ──> Project
  ├─ History Aggregation ──> Thread/Turn/Storage
  ├─ Local Console / Remote Console
  └─ Security / Observability / Release / Testing（横切）
```

依赖规则：

- UI 只依赖 Browser API、Browser Events 和公共视图模型。
- 公网 Server 不直接依赖 Codex App Server 类型。
- App Server Adapter 只存在于设备 Agent 内。
- Reliable Messaging 可以依赖 Storage，领域模块不能直接操作 outbox 表。
- Security、Observability 通过中间件和公共接口进入各模块，不能复制业务逻辑。

## 全链路主流程

一次手机端新建对话贯穿以下模块：

1. 身份模块验证网页登录会话。
2. Browser API 校验请求和幂等键。
3. Device/Project 模块校验目标归属和在线能力。
4. Reliable Messaging 在 Server SQLite 中保存命令；非终态 command 行本身就是 durable dispatch source。
5. Device Tunnel 通过 WS/WSS 投递命令。
6. Agent 将命令写入 SQLite inbox 后确认。
7. Thread/Turn 模块经 App Server Adapter 执行 `thread/start`、`turn/start`。
8. Agent 将 App Server 通知规范化为 Nuntius Event 并写入本地 outbox。
9. History Aggregation 将完成的消息和结构化 Item 写入 Agent history outbox。
10. Device Tunnel 把事件和历史批次传回 Server。
11. Server History Store 幂等写入 Thread/Turn/Item/Content 并 ACK。
12. Browser Events 通过 SSE 推送实时变化到手机。
13. Remote Console 按 `stream_id + seq` 聚合 UI，并从 Server 分页读取全部历史。
14. 发生断线时由 Reliable Messaging、历史 cursor 和快照同步恢复。

## 公共状态词汇

| 状态 | 含义 |
|---|---|
| `online` | 当前连接已认证并完成同步 |
| `syncing` | 已连接，正在重放或核对状态 |
| `offline` | 没有有效设备连接 |
| `stale` | UI 展示的是缓存数据，真实性待设备确认 |
| `degraded` | 部分能力可用，但依赖异常或队列积压 |
| `unknown` | 操作可能已生效，但系统无法安全判定 |

## 目标设计完成定义

一个模块只有在以下条件同时满足时才算完成：

- 功能设计中的验收标准已有自动化或明确人工测试。
- 技术设计中的状态机、幂等、错误和恢复分支均已实现。
- API、命令和事件 Schema 已纳入兼容性测试。
- 日志无敏感正文，关键路径有指标和 Trace。
- 与上游、下游模块的契约测试通过。
- 没有使用无限队列、静默吞错或无归属后台任务。
