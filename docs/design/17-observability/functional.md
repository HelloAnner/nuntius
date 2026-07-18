# 可观测性与诊断：功能设计

> 实现标记：本文描述目标设计；当前 `0.1.0` 已实现项和后续边界以 [后端实现状态](../../implementation-status.md) 为准。

## 1. 目标

当用户遇到“手机没消息”“设备显示离线”“Codex 卡住”等问题时，系统能准确指出故障所在层，并提供不泄露敏感内容的诊断信息。

## 2. 用户状态视图

### Server 状态

- Web API 可用。
- 当前传输档位与“不安全 HTTP”告警。
- SSE 连接状态。
- Server SQLite 可用。
- History Store 写入与回填积压。
- 当前 Server 版本。

### Device 状态

- WS/WSS 连接阶段和传输安全状态。
- 最后心跳。
- Agent 版本。
- inbox/outbox 积压。
- 实时历史同步、backfill checkpoint 和最老未同步年龄。

### App Server 状态

- 是否安装、启动、初始化。
- 版本与兼容性。
- 最近重启和错误。

### Project/Thread 状态

- 路径可用性。
- 当前 Turn。
- 待审批。
- 最近 unknown 命令。
- 历史完整度和最后成功同步时间。
- 目录浏览 allowed roots 可用性与最近 live query 错误；不输出实际敏感路径。

## 3. `nuntius status`

默认输出简洁摘要；`--json` 提供机器可读格式。返回码区分：

- 全部正常。
- 可用但降级。
- 不可用。
- 配置/版本问题。

## 4. `nuntius doctor`

检查：

- 配置格式和目录权限。
- SQLite 打开、完整性和 WAL 状态。
- Server DNS/TCP/HTTP(S)/WS(S) 可达性；HTTP 兼容档位明确报告“功能可用但传输不安全”。
- Device Credential 是否存在和有效摘要。
- Codex CLI/App Server 版本和 initialize。
- 项目路径抽样状态。
- inbox/outbox 积压。
- 磁盘空间和日志状态。

生成诊断包前展示包含的数据类别，默认脱敏。

## 5. 错误展示

- 用户错误：说明如何修正。
- 暂时故障：说明自动重试和下次时间。
- 版本问题：说明需要升级哪一端。
- unknown：说明为什么不能安全重试。
- 内部错误：提供 request ID/command ID 供排查，不展示堆栈。

## 6. Server 管理视图

第一版可通过受保护 endpoint/日志查看：

- 在线设备数。
- 连接和重连。
- pending command。
- oldest outbox age。
- SSE 连接和 resync 次数。
- 数据库健康。
- history backfill lag、partial Thread 数和失败批次。

不需要建设大型管理后台。

## 7. 验收标准

1. status 能区分浏览器、Server、Tunnel、Agent、App Server 和 Project 故障。
2. doctor 在依赖离线时仍能完成其他检查。
3. 诊断包不包含 Token、私钥、prompt 和文件正文。
4. request/command/event ID 可以贯穿相关日志。
5. 关键积压和崩溃循环有可见告警。
6. 健康检查区分 alive、ready 和 degraded。
7. status/doctor 不会把 HTTP 可达误报为安全连接，也不会在输出中泄露完整会话正文或目录路径。
8. 能区分实时控制积压与低优先级历史回填积压。
