# 可观测性与诊断：技术设计

> 实现标记：本文描述目标设计；当前 `0.1.0` 已实现项和后续边界以 [后端实现状态](../../implementation-status.md) 为准。

## 1. 三大支柱

- Logs：结构化事件和错误上下文。
- Metrics：连接、延迟、队列和故障趋势。
- Traces：一次命令跨 HTTP(S)、Server、WS(S)、Agent、App Server 的路径。

使用 `tracing` 作为 Rust 统一埋点入口，按部署能力输出 JSON 日志和 OpenTelemetry。

## 2. 关联字段

所有关键 span 包含适用字段：

```text
request_id
command_id
event_id
user_id_hash
device_id
project_id
thread_id
turn_id
connection_id
connection_epoch
app_server_generation
protocol_version
transport_security
history_batch_id
```

ID 可以记录，正文和秘密不记录。

## 3. 日志等级

- ERROR：数据无法持久化、协议不变量破坏、关键 supervisor 失败。
- WARN：重连、过载、重放、unknown、兼容性降级。
- INFO：进程启动退出、配对/撤销、连接状态变化、命令终态。
- DEBUG：请求/事件类型和耗时，不含 payload。
- TRACE：默认关闭；即使开启仍不记录秘密和正文。

高频 delta 不逐条写 INFO。

## 4. Metrics

### Server

- `http_requests_total/duration`
- `sse_connections`
- `sse_reconnects_total`
- `sse_resync_required_total`
- `device_tunnel_connections`
- `device_tunnel_reconnects_total`
- `commands_by_status`
- `command_delivery_seconds`
- `pending_commands_depth/oldest_seconds`
- `history_batches_total`
- `history_backfill_oldest_seconds`
- `history_partial_threads`
- `history_items_persisted_total`
- `directory_live_queries_total/duration`
- `sqlite_pool_*`

### Agent

- `app_server_health/restarts_total`
- `app_server_request_duration`
- `device_inbox_depth/oldest_seconds`
- `device_outbox_depth/oldest_seconds`
- `device_tunnel_state/reconnects_total`（`transport_security` 仅使用 secure/insecure 两个低基数值）
- `sqlite_busy_total/wal_bytes`
- `event_replay_total`
- `unknown_commands`
- `history_outbox_depth/oldest_seconds`
- `history_backfill_checkpoint_age_seconds`

高基数字段如 thread ID 不作为 metric label。

## 5. Trace 传播

- Browser HTTP 产生 request ID 和 command ID。
- Server command 行保存 traceparent/correlation metadata。
- WS(S) command envelope 携带 trace context；该字段只做关联，不因使用 WSS 就被当作授权信息。
- Agent 创建 child span。
- App Server adapter span 记录 method、request ID、耗时和结果类别。
- Event 用 causation ID 回链原 command。

不把 trace context 作为授权依据。

## 6. 健康检查

### Server

```text
/healthz  进程 event loop 活着
/readyz   Server SQLite 可用、迁移兼容、未 draining
```

### Agent 本地

```text
/healthz  Agent 活着
/readyz   SQLite 可写、本地 API ready
/status   详细分层状态，需要本地认证
```

App Server 不可用不一定使 Agent `/readyz` 失败，但 overall capability 标记 degraded。

## 7. 告警

第一版最小告警：

- Server not ready。
- Server SQLite 连接/磁盘/备份失败。
- Outbox oldest age 超阈值。
- 大量设备同时重连。
- App Server crash-loop。
- Agent SQLite critical/disk full。
- unknown command 数增长。
- SSE resync 比例异常。
- 实时历史同步失败、backfill oldest age 超阈值或 partial Thread 数持续增长。
- 历史回填影响 P0/P1 command delivery latency。

## 8. Doctor 实现

每个检查实现：

```rust
DiagnosticCheck {
    id,
    severity,
    timeout,
    run() -> DiagnosticResult
}
```

- 检查独立超时并有限并发。
- 一个失败不终止全部。
- 输出 pass/warn/fail/skipped。
- 诊断包使用 manifest 列出文件和脱敏规则。
- 导出前运行 secret scanner。

## 9. 数据保留

- 本地日志按大小和时间滚动。
- Server 日志交给部署日志系统。
- Trace 采样，错误/unknown 提高采样率。
- 审计与普通运行日志分离。
- 清理任务自身也有 metric 和日志。

## 10. 测试

- 日志字段和脱敏 snapshot tests。
- Metric label 基数检查。
- Trace command 跨链路 contract test。
- health/ready/draining 测试。
- Doctor 部分失败和 timeout 测试。
- 诊断包 secret scanning 测试。
- secure/insecure transport 状态、HTTP 风险告警和 WSS 不降级测试。
- History batch 关联、回填 lag 告警和正文不入 telemetry 测试。
