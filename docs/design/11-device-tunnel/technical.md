# 设备 WS/WSS 隧道：技术设计

> 实现标记：本文描述目标设计；当前 `0.1.0` 已实现项和后续边界以 [后端实现状态](../../implementation-status.md) 为准。

## 1. Endpoint 与子协议

```text
GET /api/v1/device-tunnel
Upgrade: websocket
Authorization: Bearer <short-lived-device-token>
Sec-WebSocket-Protocol: nuntius.device.v1
```

Endpoint 在两种传输档位复用同一协议：

- `secure`：浏览器入口为 HTTPS，设备 URL 派生为 `wss://`；入口代理必须透传 Upgrade、Authorization 和选定 subprotocol。
- `trusted-http`：浏览器入口为 HTTP，设备 URL 派生为 `ws://`；Server 只有在非 loopback 显式配置 `allow_insecure_http=true` 时才启动，并把 `transportSecurity=insecure` 返回给 Agent 和页面。

不得在两种档位间静默降级。配置为 WSS 时，TLS 或证书校验失败必须失败关闭，不能自动改连 WS。

## 2. 帧模型

第一版使用 UTF-8 JSON text frame：

```json
{
  "v": 1,
  "type": "command|event|ack|heartbeat|control",
  "messageId": "msg_...",
  "connectionEpoch": 18,
  "payload": {}
}
```

选择 JSON 的原因是可诊断、与公共 Schema 直接对应。压缩或二进制编码只有在性能数据证明需要时引入。

限制：

- 最大 frame 和 message 大小。
- 最大嵌套深度、数组长度和字符串长度。
- 大对象通过对应档位的 HTTP(S) 独立传输，不通过 WS(S)。

## 3. Server 连接组件

- `TunnelAcceptor`：Upgrade、Token 和 subprotocol。
- `ConnectionRegistry`：device -> active connection。
- `TunnelReader`：解析、校验和分发 Agent 消息。
- `TunnelWriter`：优先级有界队列。
- `HeartbeatMonitor`：Ping/Pong 和应用心跳。
- `TunnelSession`：epoch、cursor 和取消令牌。

Reader 和 Writer 任一结束都取消整个 TunnelSession。

## 4. Agent 连接组件

- `CredentialProvider`：challenge 和短期 Token。
- `TunnelConnector`：DNS、TCP、可选 TLS、WS(S) 建连。
- `ReconnectPolicy`：错误分类和 full-jitter backoff。
- `ResumeHandshake`：hello、版本和 cursor。
- `InboundCommandWorker`：写 SQLite inbox。
- `OutboundEventWorker`：从 SQLite outbox 发送。

## 5. Hello/Welcome

Agent 首帧必须是 hello：

```json
{
  "v": 1,
  "type": "hello",
  "payload": {
    "deviceId": "dev_...",
    "agentVersion": "...",
    "protocolMin": 1,
    "protocolMax": 1,
    "instanceId": "ins_...",
    "previousEpoch": 17,
    "lastCommandSeq": 105,
    "eventAcks": {},
    "historyCursors": {},
    "capabilities": ["history.v1", "directory-browser.v1"],
    "transportSecurity": "secure"
  }
}
```

Server 验证后原子分配新 epoch，返回 welcome：

```json
{
  "type": "welcome",
  "payload": {
    "connectionId": "conn_...",
    "connectionEpoch": 18,
    "protocolVersion": 1,
    "serverTime": "...",
    "resume": {}
  }
}
```

完成控制命令和高优先级事件的 resume sync 后 Agent 发送 `sync.complete`，Server 才将设备标记 online。历史回填游标单独协商，允许在 online 后以 P3/P4 继续，不得拉长设备恢复时间。

## 6. Heartbeat

- WebSocket Ping：建议 15 秒。
- 无 Pong/有效帧超时：建议 45 秒。
- 应用 heartbeat：建议 30 秒，携带 health 和 queue summary。
- TCP keepalive：更长周期兜底。
- 配置可调，但必须保证 Proxy timeout 明显更长。
- Heartbeat 与 Ping/Pong 使用独立高优先级发送队列；健康摘要采集、SQLite outbox 读取、
  ACK 落库和命令持久化不得运行在 WebSocket reader 中，也不得延迟下一次 heartbeat。
- 健康摘要读取超时只把 storage 标记为 busy/degraded，仍须使用上一次摘要准时发送
  heartbeat；本地积压或 SQLite 连接池耗尽不能让设备被误判离线。

应用 heartbeat 也用于复查 device key version 和 Server epoch。

Client 进程重启时先绑定本地 API、建立 Tunnel 并启动独立 heartbeat，再在后台执行 SQLite
projection 修复和运行中 Thread 恢复。命令可以先持久化，但 CommandExecutor 必须等启动恢复
事务完成后再消费；数据库体积增长不能线性拉长设备离线窗口。

## 7. 重连分类

| 错误 | 策略 |
|---|---|
| DNS/TCP/5xx，或 secure 档位 TLS 失败 | full-jitter 退避；TLS 失败不得降级到 WS |
| 401 Token expired | 重新 challenge 一次，再退避 |
| 403 device revoked | 停止自动重试，提示重新配对 |
| 426/incompatible | 停止重试，提示升级 |
| 429/503 + Retry-After | 遵守服务端时间 |
| 正常 draining | 随机短延迟后重连 |
| protocol violation | 熔断并要求诊断 |

## 8. Connection Epoch

Server 注册新连接时：

1. 为 Device 生成递增 epoch。
2. 原子替换 registry active entry。
3. 取消旧 session。
4. 所有入站帧校验 epoch。
5. Router 发送命令前再次读取 active entry。

epoch 可在 Agent SQLite 保存 previous value用于诊断，但 Server 分配值是权威。

## 9. 流控

- WS(S) writer 使用 P0/P1/P2/P3 有界队列。
- Durable 消息先落 DB/outbox，队列只做通知。
- Writer 发送超时关闭连接，不无限等待。
- Reader 对消息速率和大小限流，且只做解帧、活动时间更新和无阻塞分发。
- ACK 可批量发送 cursor，减少帧数；Client 收到 ACK 后也应批量事务清理本地 outbox，
  避免 FULL synchronous SQLite 为每帧单独 fsync。
- 高频 delta 可合并，但终态独立帧。
- P0：Approval、Interrupt、安全撤销和终态；P1：当前 Turn、命令 ACK；P2：delta 与目录 live query；P3：历史回填、索引刷新和非关键诊断。
- 目录查询为短期 live request，设独立并发上限和截止时间；不写长期 outbox，也不能抢占 P0/P1。
- 历史实时终态和回填共用 History 协议，但使用独立优先级、批大小和游标。

## 10. 传输安全和认证

- secure 档位由成熟入口管理 TLS 证书；Agent 校验证书链和主机名，可配置企业 CA。
- 不提供跳过 TLS 校验的普通配置，也不允许从 WSS 自动回退 WS。
- trusted-http 档位没有 TLS，Bearer Token、历史正文、路径元数据和控制指令都可能被窃听或篡改；启用条件、告警和审计必须明确记录。
- Token 只通过 Authorization Header。
- hello device ID 必须与 Token subject 一致。
- Server 不接受浏览器 Cookie 建立 Device Tunnel。
- Token 只解决应用层身份校验，不能弥补 HTTP/WS 缺失的传输机密性与服务端身份认证。

## 11. 测试

- Upgrade/subprotocol/auth 测试。
- HTTP/WS 与 HTTPS/WSS 功能等价、URL 派生和 capability 测试。
- WSS 证书失败不降级、HTTP 非 loopback 未显式授权拒绝启动测试。
- 两连接 epoch 竞争和迟到帧测试。
- Ping/Pong 半开连接测试。
- 全类型断线重连分类测试。
- WS(S) writer 慢消费者和队列上限测试。
- Proxy Server 滚动重启 E2E。
- 帧 fuzz 和大小限制测试。
- 历史回填压力下 P0/P1 延迟和目录 live query 截止时间测试。
