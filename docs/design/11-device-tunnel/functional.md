# 设备 WS/WSS 隧道：功能设计

> 实现标记：本文描述目标设计；当前 `0.1.0` 已实现项和后续边界以 [后端实现状态](../../implementation-status.md) 为准。

## 1. 目标

让每台位于 NAT/防火墙之后的 Agent 主动连接公网 Server，形成可认证、可重连、可恢复、多路复用的双向控制通道。

安全部署使用 `wss://`；没有 TLS 条件时允许显式启用 `ws://` 兼容档位。两种档位具有相同的 ACK、重放和业务功能，但 `ws://` 不提供传输机密性、完整性或服务端身份保护，只适合可信局域网、VPN、SSH 隧道或用户明确接受风险的环境。

## 2. 用户可见能力

- 设备无需公网 IP。
- 配对后自动连接。
- 网络切换后自动恢复。
- Server 重启后自动重连。
- 同一连接承载多个 Project/Thread。
- 同一连接承载目录实时查询和完整历史增量/回填。
- 页面看到连接中、同步中、在线、降级和离线。
- 页面和 CLI 能明确看到当前是 secure 还是 insecure transport。

用户不需要配置入站端口或暴露本地 App Server。

## 3. 连接阶段

```text
disconnected
-> connecting
-> authenticating
-> negotiating
-> syncing
-> online
-> draining
-> disconnected
```

- 只有 online 可以接收正常新命令。
- syncing 恢复命令、事件、历史回填游标和 pending 工作；完成控制面同步后即可 online，低优先级历史回填可继续运行。
- 认证或版本失败进入明确错误，不无限快速重连。

## 4. 通道内容

Server 到 Agent：

- Durable Command。
- 受控目录实时查询请求。
- 命令取消/过期通知。
- Event ACK。
- 历史批次 ACK、补传和摘要刷新请求。
- Server draining/upgrade notice。

Agent 到 Server：

- Hello、heartbeat 和健康摘要。
- Command ACK/状态。
- Durable/Replayable Event。
- 设备、项目和 Thread 索引。
- 规范化的 Thread/Turn/Item 完整历史批次。
- 受控目录查询结果，只含允许目录元数据和短期引用。
- 诊断状态；日志仍不得携带凭证或未经策略允许的文件正文。

## 5. 多设备和双连接

- 每台 Device 在 Server 上只有一个 active connection epoch。
- Agent 重连建立新 epoch 后，旧连接失去路由资格。
- 旧连接迟到消息携带旧 epoch，被 Server 丢弃并记录。
- 多台设备连接相互隔离，单设备过载不能拖慢其他设备。

## 6. 网络变化

- Wi-Fi 切蜂窝、VPN 切换、睡眠唤醒均视为旧连接失效并重建。
- Agent 使用指数退避加 jitter。
- 网络恢复可触发立即尝试，但失败后回到退避。
- 用户可以通过 `nuntius status` 查看下次重试和最近错误。

## 7. Server 维护

- Server 优雅重启时发送 draining notice。
- Agent 随机延迟重连，避免重连风暴。
- 在途 Durable 消息依靠 ACK 和 outbox 重发。
- 页面在 Server 恢复后通过快照重新收敛。

## 8. 第一版不做

- 设备之间直接 P2P。
- 公网 Server 反向访问任意设备端口。
- 通用 TCP 隧道。
- 局域网自动发现。
- QUIC/WebTransport 备用链路。

## 9. 验收标准

1. 安全档位下 Agent 只需主动访问公网 443 即可工作；HTTP 兼容档位可使用显式配置的 HTTP 端口。
2. 网络断开和 Server 重启后自动恢复。
3. 同一设备两个连接只有新 epoch 可消费命令。
4. 连接恢复后未确认 Durable 消息会重放。
5. 心跳超时能发现半开连接。
6. 认证失败、版本不兼容和网络失败有不同状态。
7. 设备隧道不能用于任意端口转发。
8. `ws://` 和 `wss://` 的业务契约一致，且非安全档位在 CLI 和页面持续显示风险。
9. 历史回填和目录查询不能阻塞审批、Interrupt 或当前 Turn 事件。
