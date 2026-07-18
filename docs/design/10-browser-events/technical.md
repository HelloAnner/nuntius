# 浏览器 SSE 事件流：技术设计

> 实现标记：本文描述目标设计；当前 `0.1.0` 已实现项和后续边界以 [后端实现状态](../../implementation-status.md) 为准。

## 1. Endpoint

```text
GET /api/v1/events?clientInstanceId=...&after=...
Accept: text/event-stream
Cookie: web_session=...
Last-Event-ID: ...
```

响应头：

```text
Content-Type: text/event-stream
Cache-Control: no-cache, no-transform
X-Accel-Buffering: no
Connection: keep-alive            # HTTP/1.1 时
```

入口代理必须禁用该路由的缓冲和压缩聚合。

## 2. SSE 格式

```text
id: cur_...
event: nuntius
data: {"version":1,"eventId":"evt_...",...}

```

- `id` 是 Server 用户事件 journal cursor，不只包含某个 Turn seq。
- payload 内保留原 `stream_id + seq`，用于业务顺序和去重。
- `event` 类型保持少量稳定值，例如 `nuntius`、`resync_required`、`server_notice`。
- 每 15 秒发送 `: keepalive` 注释，不产生业务事件。

## 3. 两级 Cursor

### Server Cursor

用于恢复“该用户 SSE 从哪里继续”，单调且短期可重放。

### Domain Cursor

`stream_id + seq` 由 Agent 生成，用于检测具体 Turn/Device 流缺口。

Server cursor 丢失时先从 Server Durable State 与 History Store 获取快照；只有缺失的实时态或尚未回填历史才向在线设备请求重放/补齐。

## 4. Event Journal

第一版单活 Server：

- Durable 状态事件和命令状态可从 Server SQLite 重建。
- 活跃连接事件保存在有界内存 ring buffer。
- Agent 保留 Replayable 事件正文。
- Server Event Journal 不永久存储逐 token Agent delta；History Aggregation 在 Item 完成后保存聚合的完整正文。

若 Last-Event-ID 不在 ring buffer：

1. 检查能否从 Durable 状态和在线 Agent 重建。
2. 不能无损补发则发送 `resync_required`。
3. 页面请求快照。

不为了强行补每个 delta 而在 Event Journal 重复存储完整会话；长期历史由独立的 History 表负责。

## 5. 订阅模型

```rust
BrowserSubscription {
    user_id,
    web_session_id,
    client_instance_id,
    focused_thread_id: Option,
    include_device_lifecycle: true,
    created_at,
}
```

通过 HTTP 更新 focused Thread：

```text
PUT /api/v1/event-subscriptions/{client_instance_id}
```

Server 只向该连接发送有权限且符合范围的事件。

## 6. 连接建立算法

1. 校验 Web Session。
2. 注册 client instance，旧同 ID 连接被替换。
3. 解析 `Last-Event-ID`，优先于 query `after`。
4. 建立 live subscription，记录边界 cursor。
5. 补发 after 到边界的 journal。
6. 切换 live，使用 cursor 去重 race 期间重复。

“先订阅再补发”避免补发和 live 之间出现空窗。

## 7. 有界发送队列

每连接分优先级：

- control：resync、auth expiry、server notice。
- durable：approval、turn terminal、command status、history completeness/item committed。
- replayable：delta。

策略：

- 有界总字节数和事件数。
- delta 按 item 合并。
- Durable 入队失败时标记连接需要 resync。
- writer 超时或 socket error 时取消连接所有任务。
- 不允许一个连接阻塞全局广播。

## 8. EventSource 认证

原生 EventSource 使用 Cookie Session：

- 同源部署。
- HTTPS 模式使用 Secure/HttpOnly/SameSite Cookie；HTTP 模式取消 Secure 并返回 `transportSecurity=insecure` 风险能力。
- 握手检查 Origin 和用户状态。
- Session revoke 后通过 registry 主动关闭连接。
- 定期轻量复查 Session 过期时间。

不要把 access token 放 query 参数。

## 9. 前端处理

前端维护：

```text
lastServerCursor
lastSeqByStream
connectionState
snapshotVersion
```

收到事件：

1. Schema 校验。
2. 检查协议版本。
3. 检查 stream seq。
4. 重复则忽略。
5. gap 则暂停该 stream 并触发 sync。
6. 正常则 reducer 应用。
7. 定期将轻量 cursor 写 session storage。

## 10. 代理和超时

- Proxy read timeout 大于 heartbeat 间隔多倍。
- CDN 不缓存 SSE。
- 关闭 proxy buffering。
- Server graceful shutdown 前发送 server_notice，并关闭连接。
- 客户端使用浏览器原生 retry；Server 可通过 SSE `retry:` 建议重连间隔。

## 11. 测试

- 快照到订阅 race 测试。
- Last-Event-ID 重放测试。
- ring buffer 过期触发 resync 测试。
- 多标签页订阅隔离测试。
- Session revoke 关闭连接测试。
- 慢消费者内存上限测试。
- 代理缓冲和 idle timeout E2E。
- HTTP 与 HTTPS 模式的 EventSource、Cookie capability 和风险标记 E2E。
- 设备离线且 SSE 重连时从 Server History Snapshot 恢复测试。
