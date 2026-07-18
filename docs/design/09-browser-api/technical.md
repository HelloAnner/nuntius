# 浏览器命令与查询 API：技术设计

> 实现标记：本文描述目标设计；当前 `0.1.0` 已实现项和后续边界以 [后端实现状态](../../implementation-status.md) 为准。

## 1. 路由分组

```text
/api/v1/auth/*
/api/v1/sync
/api/v1/devices/*
/api/v1/projects/*
/api/v1/threads/*
/api/v1/turns/*
/api/v1/approvals/*
/api/v1/commands/*
/api/v1/events                 SSE，详见 10-browser-events
/api/v1/diagnostics/*
/api/v1/history/*
/api/v1/devices/{device_id}/directories/*
```

本地 Agent 尽量实现相同路径；不适用的身份和多设备接口返回明确 capability，而不是假成功。

## 2. Axum 层次

```text
HTTP or TLS Proxy
-> Request ID
-> Trace
-> Body/Concurrency Limit
-> Session Authentication
-> CSRF（修改请求）
-> Authorization
-> Typed Extractor
-> Application Service
-> Response Mapper
```

Handler 不直接访问 SQL 或 WS/WSS connection registry。

## 3. Command API 模式

所有远程副作用统一调用 `CommandSubmissionService`：

1. 解析 Idempotency-Key。
2. 验证资源归属和静态业务规则。
3. 计算 command type、target 和 expires_at。
4. Server SQLite 事务插入 command commit。
5. 提交后唤醒 router。
6. 返回 receipt。

`Idempotency-Key` 唯一约束建议：

```text
(user_id, endpoint_operation, idempotency_key)
```

相同 key 但 request fingerprint 不同返回 `409 idempotency_conflict`。

## 4. Request Fingerprint

对规范化后的：

- HTTP method。
- endpoint operation。
- target IDs。
- 业务 payload。

计算稳定 hash。忽略 request ID、时间戳等非业务字段。Server 保存 fingerprint，防止客户端误用相同幂等键提交不同命令。

## 5. 查询 API

查询服务组合：

- Server SQLite 持久摘要。
- 内存 Presence。
- 可选设备 live query 结果。

完整历史查询直接访问 Server History Repository；目录查询必须通过短超时 Device live query，不进入长期目录缓存。

不得在普通 HTTP 请求中无限等待设备。Live query 使用短超时；超时回退缓存并标记 stale，或对必须 live 的操作返回 unavailable。

### Cursor 分页

- Cursor 包含排序键和资源作用域。
- 使用 URL-safe Base64 编码并签名/校验。
- 改变筛选条件后旧 cursor 无效。
- 设置 page size 默认值和硬上限。

## 6. Sync API

```json
{
  "snapshotId": "snap_...",
  "cursor": "cur_...",
  "capturedAt": "...",
  "devices": [],
  "activeContext": {},
  "pendingApprovals": [],
  "recentCommands": [],
  "freshness": {}
}
```

为避免“快照后、订阅前”丢事件：

1. Server 先取得当前事件 cursor。
2. 在同一逻辑读点生成快照。
3. 返回 cursor。
4. SSE 以 `after=cursor` 补发之后事件。

单活 Server 可用事件 journal cursor；未来多实例由消息总线 cursor 支撑。

## 7. 错误类型

Rust 使用领域错误枚举映射 HTTP：

```rust
ApiError::Unauthenticated
ApiError::Forbidden
ApiError::NotFound
ApiError::Conflict { code, details }
ApiError::Unavailable { retry_after }
ApiError::UnknownOutcome { command_id }
```

内部错误链写入脱敏 Trace，客户端只收到稳定 code。

## 8. 超时

- 普通数据库查询：短超时。
- 命令提交：只等待 Server SQLite commit，不等待设备。
- Live snapshot：明确较短超时。
- 上传：独立较长超时和大小限制。
- Tower timeout 触发时必须确认事务 Future 的取消安全；数据库提交结果不确定时通过幂等键查询。

## 9. Schema

- OpenAPI 作为 HTTP 契约。
- Rust DTO 与领域对象分离。
- TypeScript client 从 OpenAPI/JSON Schema 生成。
- 响应字段新增保持兼容；删除字段需主版本升级。
- 枚举提供 `unknown` 防护，但危险操作遇到未知枚举默认拒绝。

## 10. 安全

- Cookie Session + CSRF。
- 严格同源和 Origin 检查。
- 资源查询始终包含 user_id scope。
- 错误对无权资源返回统一 not_found/forbidden 策略。
- Body、字符串、数组和嵌套深度限制。
- 命令输入在进入 outbox 前校验。

## 11. 测试

- 每个 endpoint 的 auth/ownership contract tests。
- Idempotency 重试和 fingerprint 冲突测试。
- 事务 commit 前后 crash 测试。
- Cursor 篡改、分页插入和边界测试。
- Sync 快照与 SSE race 测试。
- HTTP 状态与领域错误映射 snapshot tests。
