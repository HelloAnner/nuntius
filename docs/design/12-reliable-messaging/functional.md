# 可靠消息与状态同步：功能设计

> 实现标记：本文描述目标设计；当前 `0.1.0` 已实现项和后续边界以 [后端实现状态](../../implementation-status.md) 为准。

## 1. 目标

保证用户命令和关键状态在浏览器、Server、Agent、App Server 多段故障中可追踪、可重放、可去重，并最终得到明确终态或明确的 `unknown`。

## 2. 用户可理解的确认层级

```text
已接受     Server 已持久化命令
等待设备   尚未被设备确认
已送达     Agent 已写入本地 inbox
执行中     本地领域处理/App Server 已开始
已完成     收到明确终态
未知       可能执行过，但无法安全判定
```

UI 不展示底层 ACK 数字，但所有状态必须来自这些真实确认。

## 3. 消息分类

### Durable Command

必须持久化和去重：

- Thread/Turn 创建。
- Archive/Unarchive。
- Approval decision。
- Interrupt/Steer。
- Project 修改。

### Durable Event

必须到达或可由快照恢复：

- Command 状态。
- Turn/Item 终态。
- 规范化的用户消息、完整 Agent 消息和结构化执行记录。
- Approval 请求和终态。
- Device revoke/online 状态变化。

### Replayable Event

- Agent 文本 delta。
- 命令输出 delta。
- 工具进度。

### Best-effort

- typing、短期 presence、非关键统计。

## 4. 离线策略

- 用户提交时设备已明确 offline：拒绝副作用命令。
- 设备在 Server 接受命令后短暂断线：命令在 expires_at 前等待重连。
- 命令过期：标记 expired，不再投递。
- 不允许用户在第一版选择“离线后自动执行”。

## 5. 重复和重试体验

- 用户重复点击：同幂等键返回同一命令。
- Server 重发：Agent inbox 去重。
- Agent 重发事件：Server/Browser 按 event ID 和 seq 去重。
- 页面重复收到 SSE：不会重复文本或状态转换。

## 6. Gap 和重同步

- 页面发现事件 seq 缺口时显示同步中。
- Server 优先从自己的事件日志或在线 Agent 请求缺失事件。
- 超出短期事件重放范围则使用 Server 已持久化历史快照；Server 历史本身不完整时，再向 Agent 请求规范化快照或回填。
- 快照完成后恢复 live。
- 不能恢复某个非幂等操作结果时显示 unknown。
- 历史页面同时显示 `complete/backfilling/partial`，禁止把尚未回填完成误呈现为“全部历史”。

## 7. 数据保留

- Server durable command 保留用于审计和恢复的合理期限。
- Agent event outbox 至少保留到 Server ACK 和 Turn 终态之后。
- Codex 本地状态保存可执行原始会话；Server 长期保存规范化 Thread/Turn/Item 完整历史，供跨设备和离线阅读。
- 原始 token delta 是短期重放材料；在完整 Item 落库并确认后可按保留策略清理。
- 临时重放数据超过磁盘上限时优先清理已确认且可由快照重建的数据。

## 8. 验收标准

1. HTTP 202 后 Server 重启不会丢命令。
2. WS/WSS 断开重连不会重复执行业务命令。
3. Agent 重启后 inbox/outbox 可恢复。
4. SSE 重放不会重复应用事件。
5. 命令过期后不再执行。
6. 事件缺口不会静默忽略。
7. 无法证明未执行的 App Server 请求不会自动重试。
8. 实时消息和历史回填重复到达不会在 Server 生成重复 Item。
9. 低优先级历史回填不会阻塞审批、Interrupt 和当前 Turn 终态。
