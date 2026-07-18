# Nuntius 跨模块契约

> 本文同时约束当前核心链路和后续扩展；当前 `0.1.0` 的实际完成范围以 [后端实现状态](../implementation-status.md) 与两份 OpenAPI 为准。

## 1. 目的

本文约束各子领域如何衔接。它不增加新的运行组件，而是统一：

- 谁创建命令。
- 命令在哪一层持久化。
- 谁真正执行。
- 谁产生事实事件。
- 哪一层拥有权威状态。
- 页面如何通过快照和事件恢复。

若单模块文档与本文的跨模块流向冲突，以 [技术架构](../tech.md) 的协议决策和本文的接口边界为准，并同步修正文档。

## 2. 数据真相矩阵

| 数据 | 权威来源 | Server 可保存 | UI 获取方式 |
|---|---|---|---|
| 用户、Web Session | Server SQLite / Identity | 完整必要状态 | HTTP(S) |
| 设备归属与撤销 | Server SQLite / Device | 完整必要状态 | HTTP(S) + SSE |
| 设备当前连接 | Server Connection Registry | 临时状态 + last seen | HTTP(S) + SSE |
| Project 路径与配置 | Agent SQLite / Project | 全局索引、脱敏路径和配置副本 | HTTP(S) 快照 + SSE |
| Thread 可执行状态 | Codex App Server 本地状态 | 不作为执行权威 | 在线操作经 Agent |
| Thread 完整可读历史 | Server SQLite / History Aggregation | 完整规范化记录 | HTTP(S) 分页查询 |
| Nuntius Thread 映射 | Agent SQLite / Thread | 全局映射和同步状态 | HTTP(S) + SSE |
| Turn/Item 执行事实 | App Server，经 Agent 规范化 | 完整历史、聚合内容和同步 cursor | SSE + 历史 API |
| Approval 当前状态 | Agent SQLite / Approval | 安全摘要、路由状态和历史结果 | HTTP(S) + SSE |
| 远程 Command | Server SQLite / Reliable Messaging | 完整状态、必要 payload | HTTP(S) + SSE |
| Replayable delta | Agent SQLite 短期日志 | 有界临时缓冲 | SSE/replay/snapshot |

Server 历史不能反向伪造 App Server 执行状态；Agent 同步的 revision/cursor 只能单调前进。设备离线时 Server 历史是可读权威，但继续执行必须回到设备。

## 3. 公共目标引用

所有跨链路命令使用统一 Target：

```json
{
  "deviceId": "dev_...",
  "projectId": "prj_...",
  "threadId": "thr_...",
  "turnId": "turn_...",
  "approvalId": "apr_..."
}
```

字段按命令类型可选，但有以下规则：

- `deviceId` 对所有远程设备命令必填。
- `projectId` 必须属于 device。
- `threadId` 必须属于 device 和 project；未归类 Thread 的 project 是该设备 system unassigned Project。
- `turnId` 必须属于 thread。
- `approvalId` 必须属于当前 turn。
- Server 和 Agent 两端都校验关联，不能只信浏览器。

## 4. 命令契约矩阵

| Command | 产生入口 | Server 持久化 | Agent 执行模块 | 明确终态事件 |
|---|---|---|---|---|
| `device.refresh` | Browser API | durable command | Device | `device.summary_updated` |
| `directory.list_roots` | Browser live query | 不持久化目录正文 | Directory Browser | query response |
| `directory.list` | Browser live query | 不持久化目录正文 | Directory Browser | query response |
| `project.create` | Browser API | durable command | Directory Browser + Project | `project.created/failed` |
| `project.update` | Browser API | durable command | Project | `project.updated` |
| `project.pause/resume` | Browser API | durable command | Project | `project.updated` |
| `thread.create` | Browser API | durable command | Thread + App Adapter | `thread.created/failed/unknown` |
| `thread.archive` | Browser API | durable command | Thread + App Adapter | `thread.archived/failed/unknown` |
| `thread.unarchive` | Browser API | durable command | Thread + App Adapter | `thread.unarchived/failed/unknown` |
| `turn.start` | Browser API | durable command | Turn + App Adapter | `turn.completed/failed/interrupted/unknown` |
| `turn.steer` | Browser API | durable command | Turn + App Adapter | `command.completed/failed/unknown` |
| `turn.interrupt` | Browser API | durable command | Turn + App Adapter | `turn.interrupted/completed/failed/unknown` |
| `approval.decide` | Browser API | durable command | Approval + App Adapter | `approval.approved/denied/expired/unknown` |

本地控制台命令不经过公网 Server 的 SQLite，但仍使用相同 Command Envelope，由同一业务处理器执行；远程命令必须先写 Agent SQLite inbox。

### 4.1 不经远程命令的本地操作

以下操作第一版只允许本地入口：

- 直接输入任意绝对路径的 `project.create`；远程创建只能使用 Agent 签发的短期 `directory_ref`。
- Project 手动关联未归类 Thread。
- Codex 交互式登录。
- 修改 Agent Server URL、App Server binary 等本地安全设置。
- 导出本地诊断包。

本地完成后通过摘要事件同步 Server，不在 Server 伪造对应本地状态。

## 5. 事件消费者矩阵

| Event | 生产者 | Server 行为 | Browser 行为 |
|---|---|---|---|
| `device.online/offline/degraded` | Device/Presence | 更新 Presence/摘要，发布 SSE | 更新设备卡片和可操作性 |
| `project.created/updated/removed` | Project | 按 summary version 更新 | 更新项目列表 |
| `thread.created/updated/archived` | Thread | 按 summary version 更新 | 更新列表和当前详情 |
| `turn.started` | Turn Adapter | 更新 active Turn 摘要 | 消息页进入 running |
| `item.delta` | Turn Adapter | 路由，不长期存正文 | 按 stream seq 聚合 |
| `item.completed` | Turn Adapter | 路由并由 History Aggregation 保存最终规范化内容 | 固化 Item |
| `approval.requested` | Approval Adapter | 保存安全摘要，高优先 SSE | 展示审批和全局计数 |
| `approval.*terminal` | Approval | 更新摘要和 command 状态 | 关闭审批交互 |
| `turn.*terminal` | Turn | 更新 Thread/Turn 摘要 | 固化终态、停止输入态 |
| `history.item_upserted` | History Aggregation | 幂等写 Thread/Turn/Item/Content | 更新离线可读历史和同步进度 |
| `history.sync_progress` | History Aggregation | 更新 cursor/completeness | 展示正在回填/已完整 |
| `command.status_changed` | Reliable Messaging | 权威保存并 SSE | 更新 Receipt 状态 |
| `resync_required` | Browser Events | 不作为领域事实保存 | 触发 `/sync` |

## 6. Command 状态投影

```text
Server SQLite commit             -> accepted
Server command waiting_device         -> waiting_device
Agent inbox commit ACK        -> device_accepted
Agent handler claim           -> applying
领域/App Server 明确结果       -> completed / failed / rejected
无法安全判定                   -> unknown
超过 expires_at 且未执行       -> expired
```

规则：

- UI Command 状态不能从 WS(S) “send success”推断。
- Device ACK 必须发生在 SQLite commit 后。
- `unknown` 只能通过 Reconciler 变成明确终态，不能自动回到 queued。
- Domain 终态和 Command 终态在同一 Agent 事务或可靠事件链中关联。

## 7. 新建 Thread 并发送首条消息

```text
Remote Console
  -> POST thread.create + Idempotency-Key
Browser API
  -> Server SQLite command commit commit
  <- 202 CommandReceipt
Device Tunnel
  -> WS(S) command
Agent Reliable Messaging
  -> SQLite inbox commit
  <- device.persisted ACK
Thread Service
  -> create Nuntius Thread(creating)
App Server Adapter
  -> thread/start
  <- app_server_thread_id
Thread Service
  -> SQLite mapping commit
  -> thread.created Event
Turn Service
  -> 独立 turn.start 子命令/步骤
App Server Adapter
  -> turn/start
  <- notifications
Turn Service
  -> normalized Events + SQLite outbox
History Sync
  -> completed Item/history revision
Device Tunnel
  -> WS(S) events
Server History Store
  -> Server SQLite upsert Thread/Turn/Item/Content
Browser Events
  -> SSE
Remote Console
  -> stream reducer
```

关键不变量：首个 Turn 只在 `app_server_thread_id` 持久化后发送。

## 8. 恢复已有 Thread

1. Browser 从 Server SQLite 分页读取完整规范化历史。
2. 页面显示该 Thread 的 `history_completeness` 和 `last_synced_at`。
3. 只读浏览不要求设备在线。
4. 需要继续对话时，设备必须 online。
5. Agent Thread Service 查本地映射。
6. App Adapter `thread/resume/read` 获取可执行状态。
7. Agent 返回实时 ThreadSnapshot 和 cursor。
8. Browser 将实时状态叠加到 Server 历史，再接 SSE 增量。

### 8.1 手机远程创建 Project

```text
Remote Console
  -> GET directory roots for selected device
Browser API
  -> short-lived live query
Device Tunnel
  -> Agent Directory Browser
  <- paged directory entries + opaque directory_ref
Remote Console
  -> POST project.create(directory_ref) + Idempotency-Key
Reliable Messaging
  -> durable command
Directory Browser
  -> resolve ref + revalidate allowed root
Project Service
  -> SQLite Project commit + project.created
History/Index Sync
  -> Server global Project index
```

目录查询响应不进入长期历史；Project 创建结果必须持久化。Server 和 Browser 不能把显示路径替换成任意路径提交。

### 8.2 已有历史回填

1. Agent 获取 App Server Thread inventory。
2. History Backfill 按 Thread 分页读取可用历史。
3. 规范化 Thread/Turn/Item 和内容，计算 revision/content hash。
4. 写 Agent history outbox 和 checkpoint。
5. 通过 WS(S) 低优先级批量上传。
6. Server 在 Server SQLite 事务内幂等 upsert，并返回连续 cursor ACK。
7. Agent 保存 ACK 后继续下一页。
8. 全部完成后 Thread 标记 `complete`；缺页或不支持的 Item 标记 `partial` 并说明原因。

## 9. Approval 往返

```text
App Server -> server-initiated approval request
Adapter -> ApprovalNormalizer
Approval -> SQLite pending + Durable Event
Tunnel -> WS(S)
Server -> safe summary + SSE
Browser -> POST approval.decide
Server -> durable command
Tunnel -> Agent inbox
Approval -> CAS pending -> responding
Adapter -> App Server response
Approval -> terminal Event
Tunnel -> Server -> SSE -> all tabs
```

关键不变量：只有 Agent SQLite CAS 成功的第一个决策可以调用 App Server。

## 10. Browser 恢复

```text
SSE disconnected
-> EventSource reconnect + Last-Event-ID
-> Server 能补发：replay then live
-> Server 不能补发：resync_required
-> Browser GET /sync
-> apply snapshot cursor
-> reconnect SSE after cursor
```

Browser 恢复不触发新的业务 Command。

## 11. Device Tunnel 恢复

```text
WS(S) disconnected
-> Agent full-jitter reconnect
-> auth + hello
-> Server allocates new epoch
-> exchange command/event cursors
-> replay Server Durable Commands
-> replay Agent Durable/Replayable Events
-> resume history backfill cursors
-> sync summaries
-> sync.complete
-> Device online
```

旧 epoch 连接不能参与 replay 或处理新命令。

## 12. App Server 恢复

```text
stdio EOF/process exit
-> Adapter generation closed
-> pending requests classified
-> Supervisor backoff restart
-> initialize/initialized
-> Thread/Turn/Approval reconcile
-> explicit terminal or unknown
```

App Server 重启不改变 Device Tunnel epoch；Agent health 先变 degraded，核对完成后恢复。

## 13. 快照契约

所有快照都包含：

```text
snapshot_version
captured_at
source: live | cache | server_history
freshness: live | syncing | stale
history_completeness: complete | backfilling | partial
cursor(s)
resource versions
```

应用规则：

- 更旧 summary version 不覆盖新状态。
- 快照可以覆盖同版本以前的事件聚合。
- 快照后只应用 cursor 之后的事件。
- 事件 seq gap 触发局部或全局快照。

## 14. 错误所有权

| 故障 | 首要检测模块 | 对外错误所有者 | 恢复模块 |
|---|---|---|---|
| Web Session 过期 | Browser API/SSE | Identity | Remote Console 重新登录 |
| Server SQLite 不可用 | Browser API/Storage | Server Health | Operations/Storage |
| Device WS/WSS 断开 | Tunnel | Device | Tunnel + Reliable Messaging |
| 历史同步中断/缺口 | History Aggregation | Thread completeness | History Backfill |
| 目录引用过期/越界 | Directory Browser | Project create error | 重新浏览和选择 |
| Agent SQLite 不可写 | Agent/Storage | Device Health | Storage/Agent |
| Project 路径失效 | Project | Project | Project validate |
| App Server 崩溃 | Adapter | App Server Health | Adapter Supervisor/Reconciler |
| SSE gap | Browser reducer | Browser Events | Events + Snapshot |
| 非幂等结果不确定 | Domain Adapter | Command `unknown` | Reconciler |

## 15. 跨模块安全不变量

1. Browser 不能直接构造 App Server method。
2. Server 不能直接构造 shell 命令。
3. Device Command 必须是公共协议中已知枚举。
4. Agent 在本地重新验证所有 Target 关联和 expires_at。
5. 任何 Approval Option 必须来自当前 pending Approval。
6. Project path 只在设备本地解析；远程只提交短期不透明 `directory_ref`。
7. Codex 凭证不进入 Nuntius Server。
8. 每个 Server History Thread 必须有且只有一个 Device 和 Project 归属；未归类也使用系统 Project，不使用悬空关系。

## 16. 实现顺序契约

建议按垂直闭环实现：

1. 公共 ID、Command/Event、错误和状态机。
2. Agent SQLite + App Server Adapter。
3. 本地 Project -> Thread -> Turn -> Event -> Local SSE 闭环。
4. Approval 本地闭环。
5. Server Identity/Device + Server SQLite durable command。
6. Device WS/WSS + ACK/replay。
7. Remote HTTP(S)/SSE + 手机页面。
8. 远程目录浏览和 project.create 闭环。
9. 会话历史实时汇总、既有历史回填和离线阅读。
10. 故障注入、长稳、发布和诊断。

每一步都应有可运行的端到端切片，避免先搭建大量基础设施却迟迟无法完成一个真实 Turn。
