# 系统基础与全链路：技术设计

> 实现标记：本文描述目标设计；当前 `0.1.0` 已实现项和后续边界以 [后端实现状态](../../implementation-status.md) 为准。

## 1. 架构形态

第一版采用：

- 一个 Rust 模块化单体公网 Server。
- 每台设备一个 Rust Agent/CLI 进程。
- 两个独立 TypeScript 前端工程，分别构建 local/remote 页面，不跨 Client/Server 引用业务源码。
- Server 使用 SQLite。
- Agent 使用 SQLite。
- Server 与 Agent 共享 Rust 协议和领域类型 crate。

不拆微服务，不引入 Kafka/Redis/Kubernetes。模块边界通过 crate、trait 和数据库事务边界体现。

## 2. 运行边界

```text
Browser Process
  HTTP(S) + SSE
Public Server Process
  WS(S)
Agent Process
  stdio JSONL
Codex App Server Process
```

每个边界都必须有：

- 版本握手或版本字段。
- 超时和大小限制。
- 明确的错误映射。
- 连接断开后的恢复策略。
- 脱敏日志和 Trace 关联 ID。

## 3. 公共标识

| 标识 | 生成方 | 是否持久化 | 用途 |
|---|---|---|---|
| `user_id` | Server | 是 | 数据归属 |
| `device_id` | Server | 双端 | 设备身份 |
| `project_id` | Agent | 双端完整索引 | 本地项目或 system unassigned project |
| `thread_id` | Nuntius | 双端 | 公网稳定 Thread 标识 |
| `app_server_thread_id` | App Server | Agent | App Server 映射 |
| `turn_id` | App Server/Adapter | Agent/Server History | Turn 关联 |
| `item_id` | App Server/Adapter | Agent/Server History | Item 关联与历史幂等 |
| `command_id` | API 入口 | 双端 | 幂等和追踪 |
| `event_id` | Agent | 重放期；终态映射入历史 | 事件去重 |
| `stream_id` | Agent | 重放期 | 顺序域 |
| `connection_id` | Server | 临时 | 连接实例 |
| `connection_epoch` | Server/Agent | Agent | 排除旧连接 |

业务标识使用 UUIDv7 或等价时序唯一 ID。数据库内部可以使用相同 ID，避免额外映射。

## 4. 公共时间规则

- 持久化时间统一为 UTC。
- API 使用 RFC 3339 字符串。
- 超时计算使用单调时钟，不使用墙上时钟差值。
- `expires_at` 由 Server 判定，并允许有限时钟偏差。
- UI 按用户时区显示。
- 状态排序优先使用服务端序列或设备事件 `seq`，不依赖跨机器时间精确一致。

## 5. 公共命令和事件

命令是“请求产生副作用”，事件是“已经发生的事实”。

```rust
CommandEnvelope<T> {
    version,
    command_id,
    idempotency_key,
    actor,
    target,
    issued_at,
    expires_at,
    payload: T,
}

EventEnvelope<T> {
    version,
    event_id,
    stream_id,
    seq,
    causation_id,
    correlation_id,
    occurred_at,
    durability,
    payload: T,
}
```

命令和事件使用显式枚举，不用任意字符串加无约束 JSON。未知可选字段忽略，未知 Durable 命令明确拒绝。

## 6. 错误模型

公共错误响应包含：

```json
{
  "error": {
    "code": "device_offline",
    "message": "目标设备当前离线",
    "requestId": "req_...",
    "retryable": false,
    "details": {}
  }
}
```

错误类别：

- `invalid_request`：输入错误，不重试。
- `unauthenticated` / `forbidden`：身份或权限问题。
- `conflict`：状态冲突或重复审批。
- `offline` / `unavailable`：依赖不可用。
- `overloaded`：有条件退避重试。
- `timeout_unknown`：超时且结果无法判定。
- `incompatible_version`：需要升级。
- `internal`：未分类内部错误，隐藏敏感细节。

## 7. 配置层次

优先级从高到低：

1. CLI 参数，仅用于本次启动。
2. 环境变量，适合部署秘密和容器。
3. 配置文件，保存稳定设置。
4. 编译默认值。

配置必须强类型解析，启动时一次性验证。秘密字段不得输出到 debug dump。

## 8. 事务边界

- Server 接受命令：`commands` 行同时作为 durable dispatch source，在同一 Server SQLite 事务提交。
- Agent 接收命令：device inbox 与命令状态同一 SQLite 事务。
- Agent 产生事件：事件序号分配与 local outbox 同一 SQLite 事务。
- Server 接收历史：history batch 去重、Thread/Turn/Item upsert 与 checkpoint 同一 Server SQLite 事务；提交后写可重放 event journal 并发布 SSE。
- 跨数据库不做分布式事务，依赖至少一次投递和幂等收敛。
- 网络发送不能发生在持有数据库事务期间。

## 9. 并发模型

- Device、Project、Thread 分别有逻辑串行域。
- 不同 Thread 可以并行。
- 同一 Thread 的副作用命令串行处理。
- 数据库唯一约束是最终幂等防线，内存锁只是优化。
- 所有 channel 有容量上限和优先级。
- 所有后台任务由 supervisor 管理并支持取消。

## 10. API 和 Schema 管理

- HTTP API 前缀：`/api/v1`。
- Device WS/WSS 协议独立版本：`nuntius.device.v1`。
- SSE 事件沿用公共 Event Envelope。
- OpenAPI 描述 HTTP 接口。
- JSON Schema 描述命令和事件。
- TypeScript 类型从 Schema 生成，不手工复制 Rust struct。
- App Server Schema 单独生成，只在 Adapter crate 使用。

## 11. 依赖规则

```text
transport -> application service -> domain -> repository trait
adapter   -> external protocol
```

禁止：

- HTTP Handler 直接写 SQL。
- UI 直接理解 App Server 原始通知。
- Server 读取设备 Codex 数据库。
- Agent 将 App Server 原始错误原样发送给用户。
- Repository 触发网络请求。

## 12. 故障恢复基线

- Browser：快照 + SSE cursor。
- Server：SQLite durable command、完整历史、checkpoint + 路由重建。
- Agent：SQLite inbox/outbox + supervisor。
- App Server：重新 initialize + Thread 状态核对。
- 任何无法安全核对的副作用进入 `unknown`。

## 13. 测试要求

- 公共类型序列化 golden tests。
- 新旧协议版本兼容 tests。
- 状态机 property tests。
- 每个跨进程边界有 contract tests。
- 每个事务边界有 crash-point tests。
- 日志脱敏 snapshot tests。
