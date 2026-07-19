# 多控制端命令编排

## 目标

同一会话可以同时被本地网页、远程网页、手机和后续的终端入口控制。所有写操作都使用同一套命令语义，并在断线、重连和进程重启后给出可追踪的确定状态。

系统采用 **at-least-once 传输 + 幂等命令 + 单目标串行执行**。不承诺跨 App Server 的 exactly-once；执行中进程退出时，不能安全确认副作用是否发生，命令进入 `unknown`，由用户决定是否重试。

## 数据流

```text
控制端
  │  POST + Idempotency-Key
  ▼
Server commands（持久化命令队列）
  │  WebSocket；断线后按 queue_epoch 重放未终态命令
  ▼
Client command_inbox（持久化收件箱）
  │  target_key 分区；分区内 FIFO，分区间有界并发
  ▼
Thread / Project / Device actor
  │  恢复 App Server thread，再执行 start / steer / interrupt
  ▼
CommandAck（persisted / applying / terminal）
  │
  ▼
Server event_journal → 用户级 SSE → 所有在线控制端
```

App Server 的 token delta、审批和 Turn 状态通过独立的事件 outbox 传输；历史快照通过独立的 history outbox 传输。命令反馈不会与大体积历史同步互相阻塞。

## 队列边界

### Server commands

- HTTP 202 只在 SQLite 事务提交后返回，提交点是命令的接受边界。
- `Idempotency-Key` 和请求指纹保证客户端超时重发不会产生第二条命令。
- `queue_epoch + server_sequence` 是传输身份。数据库替换或序列重新开始时会产生新 epoch，避免客户端旧收件箱与新命令碰撞。
- Server 重放所有非终态命令，不依赖一个可能跨过“空洞”的最大序号来判断完成情况。

### Client command_inbox

- 远程命令和本地 API 命令先写入同一张收件箱，再由同一调度器消费。
- `target_key` 优先使用 thread，其次 project，最后 device。一个 target 同时只允许一个命令执行；不同 target 最多并发执行 8 个。
- 同一 target 严格按入队顺序执行。interrupt 和 approval 只影响不同 target 之间的调度优先级，不越过同一会话中已经接受的命令。
- 隧道连接只负责收发，不拥有执行任务；网络重连不会取消正在执行的命令。
- 启动时遗留的 `applying` 进入 `unknown`，不会盲目重复执行可能已经产生副作用的命令。

### 反馈与实时事件

- 命令状态为 `accepted → waiting_device → device_accepted → applying → terminal`。
- terminal 包括 `completed / failed / rejected / unknown / expired`，并携带稳定错误码和可展示的错误信息。
- Server 把 ACK 写入命令状态后再发布 `command.status_changed`；网页同时使用 SSE 和命令状态轮询兜底。
- App Server 流式 delta 使用独立 SSE 路径。前端按动画帧合并 token 更新，保持连续输出并减少布局抖动。

## 会话输入规则

浏览器不再根据可能滞后的页面状态选择 `turn/start` 或 `turn/steer`。所有普通输入都提交为统一的 thread input：

1. Thread actor 调用 `thread/resume` 恢复并读取权威状态。
2. 存在 `inProgress` Turn 时调用 `turn/steer`。
3. Thread idle 时调用 `turn/start`。
4. interrupt 在没有活动 Turn 时幂等成功。

这样，电脑和手机几乎同时发送时，判断发生在同一个会话串行执行边界内，而不是发生在两个浏览器各自的旧快照上。

## 扩展边界

当前部署是单 Server SQLite 和单设备 Client SQLite，因此不引入外部 broker。若 Server 以后横向扩容，`commands` 与 `event_journal` 的语义保持不变，消费协调需要迁移到支持行锁/租约的共享数据库，或用 NATS JetStream、Kafka 等 durable broker 实现；`target_key` 仍是会话有序性的分区键。
