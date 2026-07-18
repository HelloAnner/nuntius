# Turn、消息与执行事件：技术设计

> 实现标记：本文描述目标设计；当前 `0.1.0` 已实现项和后续边界以 [后端实现状态](../../implementation-status.md) 为准。

## 1. 组件

- `TurnService`：start、steer、interrupt 和状态转换。
- `ThreadActor`：同一 Thread 命令串行化。
- `ItemAggregator`：delta 聚合为 UI 快照。
- `TurnRepository`：本地 Turn/Item 索引和恢复状态。
- `TurnEventNormalizer`：App Server 事件映射。
- `TurnSnapshotService`：重连状态快照。

## 2. Agent 数据模型

### turns

```text
id
thread_id
app_server_turn_id nullable
start_command_id unique
status
started_at nullable
completed_at nullable
last_event_seq
terminal_reason nullable
uncertainty_reason nullable
created_at
updated_at
```

### items

```text
id
turn_id
app_server_item_id nullable
kind
status
ordinal
summary_json
content_ref nullable
started_at nullable
completed_at nullable
updated_at
```

完整长期内容仍以 App Server/Codex 会话为准；Nuntius 保存恢复和当前 UI 所需的有限聚合。

## 3. 命令

```text
turn.start {
  thread_id,
  input: [{type: text, text}],
  overrides
}

turn.steer { thread_id, active_turn_id, input }
turn.interrupt { thread_id, active_turn_id }
```

- start 的 idempotency scope 是 user + thread + key。
- steer/interrupt 必须携带预期 active_turn_id，防止作用到新 Turn。
- 命令有 expires_at。

## 4. Thread Actor

每个 Thread 逻辑上只有一个命令执行器：

- 从 SQLite inbox 按 server sequence 读取。
- 校验当前 Thread/Turn 状态。
- 执行一个状态转换并持久化。
- 调用 Adapter 时不持有 SQLite 事务。
- 响应回来后用 compare-and-set 更新预期状态。

Agent 重启后 actor 从数据库重建，不依赖内存 mailbox 保存 durable 命令。

## 5. Turn Start

```text
1. command inbox 已提交
2. CAS thread: ready -> starting
3. 创建 local turn(starting)
4. 调用 App Server turn/start
5. 保存 app_server_turn_id（若同步响应提供）
6. 等待 turn/started notification
7. 状态 running
```

若 4 超时：

- Turn 进入 unknown-start。
- 不自动再次调用 turn/start。
- Reconciler 查询 Thread 当前活动 Turn。
- 唯一匹配则补齐 mapping；否则等待用户核对。

## 6. Event Stream

每个 Turn 使用稳定 `stream_id`。Agent 在 SQLite 事务中：

1. 读取并增加 `last_event_seq`。
2. 更新 Turn/Item 聚合状态。
3. 写 local event outbox。
4. 提交。

然后才通过 WS/WSS 发送，并由 History Aggregation 将完成后的规范化 Item 持久化到 Server。

### delta 聚合

- App Server item ID 映射到本地 item ID。
- delta 带自己的 event seq。
- Aggregator 追加前检查 event ID/seq。
- 相邻小 delta 可合并成网络帧，但不能改变事件顺序。
- Item completed 包含最终完整文本或受控内容引用、content hash 和 revision，用于 Server 历史固化。

## 7. 事件类型

- `turn.queued`
- `turn.device_accepted`
- `turn.started`
- `turn.waiting_approval`
- `turn.completed`
- `turn.failed`
- `turn.interrupted`
- `turn.unknown`
- `item.started`
- `item.delta`
- `item.completed`

公网协议使用稳定类型，App Server 原 method 只作为 debug metadata。

## 8. Backpressure

- 每个 Turn 有独立 delta 聚合缓冲。
- Agent stdout reader 不能被慢 WS/WSS 或历史回填直接阻塞；先快速写有界处理队列/SQLite。
- SQLite 写入滞后达到阈值时暂停向 App Server 发新 Turn，并标记 degraded。
- 命令输出 delta 可按块合并。
- 最终 Item 和 Turn 终态永不因队列满而丢弃。

## 9. 快照

`TurnSnapshot` 包含：

```text
thread_id, turn_id, status
last_event_seq
aggregated_items[]
pending_approvals[]
started_at, completed_at
freshness
```

快照生成读取短事务；大内容按分页或单独 endpoint 读取。

## 10. 并发竞态

- Start vs Archive：Thread CAS 只有一个成功。
- Interrupt vs Complete：若已完成，interrupt 返回 already_terminal。
- Steer vs Complete：检查 active_turn_id，完成后拒绝。
- 两个 Start：Thread Actor 串行，第二个返回 active_turn_conflict。
- Approval vs Interrupt：以 App Server 最终 Turn/Approval 事件收敛。

## 11. 测试

- Start HTTP 重复与 Agent 重放测试。
- App Server 响应丢失后的 unknown/reconcile 测试。
- delta 重复、乱序、缺口和合并测试。
- Interrupt/Complete、Steer/Complete 竞态测试。
- Agent crash 后 active Turn 快照恢复测试。
- 10 万 delta 下内存和队列上限测试。
