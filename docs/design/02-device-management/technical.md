# 设备管理：技术设计

> 实现标记：本文描述目标设计；当前 `0.1.0` 已实现项和后续边界以 [后端实现状态](../../implementation-status.md) 为准。

## 1. 组件边界

- `DeviceRegistry`：设备持久信息和 Owner 归属。
- `PresenceService`：当前连接、epoch 和心跳状态。
- `CapabilityService`：Agent/App Server 版本与能力计算。
- `DeviceSummaryService`：组合持久信息和实时状态供 UI 使用。
- `DeviceCommandPolicy`：判断某类命令当前是否可路由。

Presence 只保存在内存并定期写最后在线摘要；Device Registry 存在 Server SQLite。

## 2. 数据模型

### devices

```text
id
user_id
display_name
status: active | revoked
created_at
revoked_at nullable
last_seen_at nullable
last_agent_version nullable
last_codex_version nullable
last_os_family nullable
last_arch nullable
last_summary_at nullable
```

### device_capability_snapshots

```text
device_id primary key
protocol_version
capabilities jsonb
app_server_status
storage_status
project_count
active_turn_count
pending_approval_count
history_sync_state
history_partial_thread_count
history_last_synced_at nullable
transport_security: secure | insecure
captured_at
```

完整实时详情不频繁写 DB，只在变化或固定低频窗口更新摘要。

## 3. Presence 模型

```rust
DevicePresence {
    device_id,
    connection_id,
    connection_epoch,
    phase: authenticating | syncing | online | draining,
    connected_at,
    last_heartbeat_at,
    app_server_health,
    queue_health,
    transport_security,
    history_backfill_health,
}
```

在线计算：

```text
credential active
AND current epoch connection exists
AND phase == online
AND heartbeat age <= timeout
```

降级状态由 `online + health issue` 计算，不写成永久状态，避免状态过期。

## 4. API

```text
GET    /api/v1/devices
GET    /api/v1/devices/{device_id}
PATCH  /api/v1/devices/{device_id}
POST   /api/v1/devices/{device_id}/refresh
DELETE /api/v1/devices/{device_id}
GET    /api/v1/devices/{device_id}/diagnostics-summary
```

`PATCH` 第一版只允许修改 `display_name`。Server SQLite 中的名称是权威值：更新成功后发布
`device.renamed` SSE 事件刷新所有管理页面；支持 `device-display-name-sync.v1` 的在线 Client
立即收到 `device_config`，离线 Client 则在下一次 Tunnel `welcome` 中取得最新名称，并原子更新
`~/.nuntius/config.toml`。`DELETE` 表示 revoke，不执行物理删除。

## 5. Agent 状态上报

WS/WSS hello 和 application heartbeat 上报：

- Agent version、protocol version。
- OS family、arch。
- App Server version/health。
- SQLite 可写状态。
- inbox/outbox 深度摘要。
- active Turn、pending Approval 数量。
- Project 摘要版本。
- History checkpoint/backfill/partial 摘要和 Directory Browser capability。
- secure/insecure transport 状态。

Server 校验字段大小和枚举，未知能力保存为可选值但不直接影响权限。

## 6. 状态事件

Server 产生：

- `device.pairing_started`
- `device.syncing`
- `device.online`
- `device.degraded`
- `device.offline`
- `device.summary_updated`
- `device.revoked`
- `device.upgrade_required`

状态事件经 Browser SSE 推送。连续 heartbeat 不产生 UI 事件，只有派生状态变化才产生。

## 7. 抖动控制

- WS/WSS 断开后立即从 active registry 移除，但 UI 可以显示短暂 `reconnecting` 派生状态。
- 若设备在宽限期内以更高 epoch 恢复，不额外产生长期 offline 审计事件。
- 超过宽限期后写入 `last_seen_at` 并发布 offline。
- 危险命令不使用宽限期：连接一旦失效即停止新路由。

## 8. 命令可用性

`DeviceCommandPolicy` 输入命令类型和设备摘要：

- 查询快照：连接 online/syncing 时可路由。
- 新建 Turn：要求 online、App Server ready、storage writable、协议兼容。
- interrupt：如果旧连接刚断开，可在重连后短期补发且不超过 expires_at。
- approval decision：要求对应 approval 仍 pending 且设备 online。
- refresh summary：online 即可。

## 9. 一致性和故障

- Presence 内存丢失：Server 重启后设备自动重连重建。
- Server SQLite 摘要落后：响应同时返回 `freshness` 和 `captured_at`。
- 设备改名时离线或推送中断：Server 保留权威名称，Client 每次重连都用 `welcome` 快照对账；
  握手期间发生的改名由连接注册表保留最新待同步值，避免旧快照覆盖新名称。
- 两条同设备连接：原子替换 current epoch，旧连接只能关闭，不能消费命令。
- 撤销与重连竞态：认证和每次路由都校验 active device status/key version。
- heartbeat 写库失败：不影响当前连接，但 health 标记 degraded 并重试摘要持久化。

## 10. 测试

- Presence 超时和宽限期测试。
- 同设备双连接 epoch 竞争测试。
- 撤销与握手并发测试。
- 版本能力矩阵测试。
- 状态派生 property tests。
- 设备列表 stale/live 表现 contract tests。
- transport security 与在线状态正交、history completeness 与 presence 正交测试。
