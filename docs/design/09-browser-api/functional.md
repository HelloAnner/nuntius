# 浏览器命令与查询 API：功能设计

> 实现标记：本文描述目标设计；当前 `0.1.0` 已实现项和后续边界以 [后端实现状态](../../implementation-status.md) 为准。

## 1. 目标

为本地控制台和远程控制台提供一致、可确认、可幂等的 HTTP 接口。查询返回状态快照，命令返回持久化接受结果；实时变化由 SSE 模块负责。

## 2. API 使用模型

```text
查询：GET -> 当前快照
命令：POST/PATCH/DELETE + Idempotency-Key -> Command Receipt
实时：EventSource -> 增量事件
恢复：GET /sync -> 新快照 + cursor
```

浏览器不通过 SSE 发送命令，也不通过单个长连接模拟所有请求响应。

## 3. 查询能力

- 当前登录用户和会话。
- 设备列表、详情和能力。
- 项目列表、详情和可用状态。
- Thread 列表、详情和归档筛选。
- 跨设备完整会话历史、Turn 和 Item 分页详情。
- 当前 Turn 快照和 Item 聚合。
- 待审批列表。
- 命令状态。
- 全局同步快照。
- 诊断摘要。
- 在线设备的受控目录根和子目录列表。

查询结果必须带来源和新鲜度：

- `live`：目标设备本次连接确认。
- `server_history`：Server 已持久化的规范化完整历史，可在设备离线时读取。
- `cache`：Server 持久索引或短期状态摘要。
- `stale`：目标设备离线或实时状态过期；不能用于否定 `server_history` 的可读性。
- `historyCompleteness`：`complete/backfilling/partial`，与设备在线状态分开表达。

## 4. 命令能力

- 创建/修改/暂停/移除项目允许的部分。
- 使用 Agent 签发的短期目录引用远程创建项目。
- 创建、恢复、归档 Thread。
- 启动 Turn、Steer、Interrupt。
- 处理 Approval。
- 刷新设备/项目/Thread 摘要。
- 撤销设备。

所有有副作用请求都返回 `CommandReceipt`，而不是等待 Codex 完成。

```json
{
  "commandId": "cmd_...",
  "status": "accepted",
  "acceptedAt": "...",
  "statusUrl": "/api/v1/commands/cmd_..."
}
```

## 5. 幂等体验

- 页面第一次提交命令时生成 Idempotency-Key。
- 请求超时但页面未收到响应时，使用同一个 Key 重试。
- Server 对同一作用域和 Key 返回原 Command Receipt。
- 用户明确发起第二次相同意图时生成新 Key。
- 页面刷新后保留尚未确认命令的 Key 和 command ID。

## 6. HTTP 状态语义

| 状态 | 含义 |
|---|---|
| `200` | 查询成功或幂等命令已有最终同步结果 |
| `201` | 仅用于 Server 本地立即创建的资源 |
| `202` | 命令已持久化，等待设备处理 |
| `400` | 请求格式错误 |
| `401` | 未登录或会话过期 |
| `403` | 无权访问目标资源 |
| `404` | 资源不存在或不属于当前用户 |
| `409` | 当前状态冲突，如活跃 Turn 时归档 |
| `410` | 命令或游标已过期 |
| `422` | 参数格式正确但业务不可执行 |
| `429` | 限流 |
| `503` | Server 依赖不可用或过载 |

客户端不能把 `202` 显示为“执行成功”。

## 7. 离线行为

- 设备离线时设备/项目实时状态标记 stale，但 Server 已汇总的完整历史仍可分页读取。
- 第一版大多数副作用命令直接返回 `device_offline`，不创建长期队列。
- 短暂断线期间已持久化且未过期的命令可以等待设备重连。
- 命令是否允许等待由命令类型决定，不由前端自由选择。

## 8. 同步快照

`GET /api/v1/sync` 用于：

- 页面首次加载。
- SSE 重连后校正。
- 页面从后台恢复。
- 事件 gap 或 resync_required。

快照包含用户视野内的设备状态、当前导航资源摘要、待审批和事件 cursor。大列表仍分页返回，不能把所有历史消息塞入一个响应。

## 9. 错误体验

每个错误包含稳定 code、用户可读 message、request ID、是否可重试和必要 details。前端按 code 决定动作，不解析 message 文本。

## 10. 验收标准

1. 所有副作用 API 都有幂等键和 Command Receipt。
2. `202` 与完成状态在 UI 中区分。
3. 设备离线查询带 stale 标记。
4. 未登录用户不能通过错误差异探测资源存在性。
5. 页面可通过 command status API 恢复超时请求。
6. sync 快照足以纠正 SSE 事件缺口。
7. 本地和远程前端使用相同视图模型。
8. HTTP 和 HTTPS 复用相同 API Schema/幂等语义，并返回准确的 transport security capability。
9. 历史查询不依赖设备在线，目录查询必须依赖目标设备 live response。
