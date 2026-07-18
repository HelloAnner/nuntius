# 可靠消息与状态同步：技术设计

> 实现标记：本文描述目标设计；当前 `0.1.0` 已实现项和后续边界以 [后端实现状态](../../implementation-status.md) 为准。

## 1. 语义

Nuntius 提供：

- 网络段：at-least-once delivery。
- 业务层：幂等去重或显式 unknown。
- 顺序：单 stream 单调 seq。
- 一致性：事件加快更新，快照最终纠正。

不承诺端到端 exactly-once。

## 2. Server 表

### commands

```text
id
user_id
device_id
kind
idempotency_scope
idempotency_key
request_fingerprint
status
issued_at
expires_at
device_accepted_at nullable
completed_at nullable
result_summary nullable
error_code nullable
```

第一版不建立重复保存命令 payload 的 `server_outbox` 表。`commands` 表本身同时承担业务状态与 durable dispatch source：`accepted/waiting_device/device_accepted/applying` 是可重放集合，终态行保留查询和幂等结果。这样仍然只有一个事务提交边界，也减少双表不一致和清理复杂度。若未来改为多实例、需要 lease/priority 调度，再把 dispatch source 独立成 outbox。

## 3. Agent 表

### device_inbox

```text
command_id primary key
server_sequence
kind
payload
status
received_at
expires_at
processing_started_at nullable
completed_at nullable
result_json nullable
```

### device_outbox

```text
event_id primary key
stream_id
seq
kind
durability
payload
priority
created_at
server_acked_at nullable
browser_delivery_hint nullable
```

### stream_cursors

```text
stream_id primary key
next_seq
server_acked_seq
retained_from_seq
updated_at
```

## 4. Server Outbox Worker

1. 短事务 claim `available_at <= now` 的消息并设置 lease。
2. 事务外查 active Tunnel。
3. 无连接：若未过期，更新下一次 available_at；过期则完成 expired。
4. 有连接：放入有界 WS(S) queue。
5. 收到 device persisted ACK 后更新 command 状态和 outbox delivered。
6. worker 崩溃后 lease 到期可重新 claim。

“写入 WS(S) socket 成功”不能作为 delivered。

## 5. Agent Inbox Worker

接收 WS(S) Command：

1. 验证 schema、device target、epoch 和 expires_at。
2. SQLite `INSERT OR IGNORE` inbox。
3. 若已存在，读取原状态。
4. 事务 commit。
5. 返回 `device.persisted` ACK。
6. Thread/Project actor 从 inbox claim 执行。

数据库不可写时不 ACK，并让 Agent 状态 degraded。

## 6. Agent Event Outbox

事件生成在一个 SQLite 事务中：

1. 原子取得 stream `next_seq`。
2. 更新领域聚合状态。
3. 插入 outbox event。
4. 增加 next_seq。
5. commit。

WS(S) sender 顺序读取，Server ACK 可使用每 stream 的连续最高 seq。存在 gap 时不能越过 gap 清理。

历史同步使用独立的 `history_stream_id` 和 checkpoint。完成后的规范化 Item 既写入 Agent outbox，也由 History Aggregation 在 Server 事务内写入历史表和 history inbox 去重记录，成功提交后才返回 `server.history_persisted`。实时同步与旧历史回填使用相同幂等身份，不会产生两份 Item。

## 7. ACK 类型

| ACK | 含义 |
|---|---|
| `server.accepted` | Server command 已持久化，对浏览器由 HTTP 202 表示 |
| `device.persisted` | Agent inbox 已提交 |
| `device.applied` | 本地领域命令有明确结果 |
| `server.event_persisted` | Server 已保存必要 Durable 状态/游标 |
| `server.history_persisted` | Server 已保存一个连续历史批次及其 checkpoint |
| `stream.acked` | 某 stream 连续 seq 已确认 |

ACK 本身可重复，按 message ID 幂等处理。

## 8. 重放协议

WS(S) resume 交换：

- Agent 已持久化的最高 server command sequence。
- Server 对各 Agent stream 已确认的连续 seq。
- Agent retained_from_seq。
- 各历史分区的 `history_checkpoint`、revision 和 retained snapshot 范围。

若 Server 请求的 seq 早于 retained_from：

- Agent 返回 `replay_unavailable`。
- Server 对浏览器发布 resync_required。
- 使用 Thread/Turn snapshot 收敛。

历史缺口与 UI 事件缺口分别处理：事件 gap 可以用 Server History Snapshot 恢复界面；history gap 必须由 Agent 重新读取 App Server/本地 Codex 状态，按 Domain 20 的批次协议补齐。目录浏览结果不进入重放协议，因为它是短期、可重新查询的 live data。

## 9. 事件清理

只有满足以下条件才清理 Replayable Event：

- Server 已确认连续 cursor 越过该事件。
- 对应 Item/Turn 已有可重建快照或终态。
- 超过最短保留时间。

Durable 终态在 Server History Store 长期保留。Agent 磁盘压力下：

1. 清理已 ACK Best-effort。
2. 清理已由 Server 确认且能从完整 Item 重建的 Replayable delta。
3. 保留 Server 尚未确认的历史批次、其他 Durable 和 inbox 未终态命令。
4. 仍不足则停止接受新 Turn，不能删除未确认 Durable 数据。

## 10. Unknown 处理

unknown 记录包含：

```text
command_id
last_known_stage
uncertainty_reason
app_server_generation
reconciliation_attempts
next_action
```

Reconciler 只能通过权威状态将 unknown 变成明确终态。用户点击“重试”默认触发核对；只有明确确认未执行后才创建新命令和新幂等键。

## 11. 过载

- Worker 批量大小有限。
- 数据库 lease 防多 worker 重复活锁。
- 指数退避加 jitter。
- 每 Device 公平调度。
- P0/P1 先于 P2；历史回填位于 P3/P4，可暂停但不能伪造 ACK。
- oldest pending age 超阈值产生告警。

## 12. 测试

- Server SQLite commit/202 crash matrix。
- WS/WSS send/ACK crash matrix。
- SQLite inbox commit/ACK crash matrix。
- Event seq 分配 crash matrix。
- lease worker 并发 claim 测试。
- cursor gap 和 retained boundary 测试。
- 磁盘压力清理优先级测试。
- unknown 不盲重试属性测试。
- 实时历史与 backfill 竞态、revision 覆盖和 content hash 去重测试。
- 历史 P3/P4 队列饱和时 P0/P1 延迟上限测试。
