# 会话历史汇总与同步：技术设计

> 实现标记：本文描述目标设计；当前 `0.1.0` 已实现项和后续边界以 [后端实现状态](../../implementation-status.md) 为准。

## 1. 组件

### Agent

- `HistoryInventory`：发现 App Server Thread 和本地映射。
- `HistoryExtractor`：分页读取 Thread/Turn/Item。
- `HistoryNormalizer`：转换为稳定 Nuntius History Schema。
- `HistoryBackfillScheduler`：最近优先、可暂停、低优先级回填。
- `HistoryOutbox`：持久化待上传 batch。
- `HistoryCheckpointRepository`：每 Thread cursor 和完整性。

### Server

- `HistoryIngestionService`：校验归属、revision、hash 和批次。
- `HistoryRepository`：Server SQLite upsert 和分页查询。
- `HistoryQueryService`：跨设备筛选和 Thread 详情。
- `HistoryCompletenessService`：计算 complete/partial/stale。
- `HistoryRetentionWorker`：删除和保留策略。

## 2. 稳定历史模型

App Server 原始类型只存在于 Adapter。公网使用：

```rust
HistoryThread
HistoryTurn
HistoryItem {
    item_id,
    turn_id,
    kind,
    ordinal,
    status,
    revision,
    content_hash,
    content,
    structured_detail,
    occurred_at,
    completed_at,
}
```

`kind` 第一版包括 user_message、agent_message、command、tool、file_change、approval、system_status 和 unknown。未知 App Server Item 可以保存安全的 unknown 元数据，不丢失顺序。

### 2.1 稳定 ID

- Nuntius 新建实体直接生成 UUIDv7 并持久化 source mapping。
- 导入既有 Thread 时，优先使用 `(device_id, app_server_thread_id)` 在 Agent SQLite 找到或创建稳定 `thread_id`。
- Turn/Item 优先映射 App Server 的稳定 ID；若目标版本未提供稳定 ID，Adapter 使用父实体 ID + 规范化 source key 生成确定性 ID，并把该版本标记为需要 compatibility golden test。
- ID 不是授权凭证；Server 仍按 user/device/project 外键校验。
- Agent 映射丢失后重新发现时，确定性 source key 必须导向原 Server 记录，不能生成一套重复历史。

## 3. Server SQLite Schema

### history_threads

```text
thread_id primary key
user_id
device_id
project_id                 # non-null，未映射时为 device system unassigned project
app_server_thread_id
title
archive_status
history_revision
sync_state
history_cursor nullable
last_synced_at nullable
completeness_reason nullable
created_at
updated_at
```

### history_turns

```text
turn_id primary key
thread_id
ordinal
status
revision
started_at nullable
completed_at nullable
terminal_reason nullable
created_at
updated_at
unique(thread_id, ordinal)
```

### history_items

```text
item_id primary key
turn_id
ordinal
kind
status
revision
content_hash
content_format
content_text nullable
structured_detail jsonb nullable
content_bytes
is_truncated
occurred_at
completed_at nullable
updated_at
unique(turn_id, ordinal)
```

### history_sync_batches

```text
batch_id primary key
device_id
thread_id
from_cursor
to_cursor
payload_hash
status
record_count
received_at
committed_at nullable
unique(device_id, thread_id, to_cursor)
```

所有表都带 user/device 归属或可通过外键唯一推导，查询仍显式按 user scope。`project_id` 外键指向普通 Project 或该 Device 的 system unassigned Project，禁止 null/悬空 Thread。

## 4. Agent SQLite Schema

### history_checkpoints

```text
thread_id primary key
extract_cursor nullable
server_acked_cursor nullable
sync_state
last_inventory_revision nullable
last_attempt_at nullable
error_code nullable
```

### history_outbox

```text
batch_id primary key
thread_id
from_cursor
to_cursor
payload
payload_hash
priority
created_at
server_acked_at nullable
attempt_count
```

history outbox 与实时 device outbox 可共用调度器，但使用独立消息类型、配额和低优先级。

## 5. 同步批次协议

```json
{
  "type": "history.batch",
  "batchId": "hbatch_...",
  "deviceId": "dev_...",
  "threadId": "thr_...",
  "fromCursor": "...",
  "toCursor": "...",
  "inventoryRevision": 12,
  "records": [],
  "payloadHash": "sha256:..."
}
```

Server 响应：

```json
{
  "type": "history.ack",
  "batchId": "hbatch_...",
  "threadId": "thr_...",
  "ackedCursor": "...",
  "acceptedRevision": 12
}
```

- 批次压缩后有大小和记录数上限。
- Server 必须在 Server SQLite commit 后 ACK。
- 相同 batch/payload hash 重放返回原 ACK。
- 相同 batch ID 不同 hash 是协议冲突并拒绝。
- 新 inventory revision 中若稳定 ID 改变但 `(thread_id, ordinal)` 或
  `(turn_id, ordinal)` 不变，以设备快照为权威替换旧 Turn/Item，不能让次级唯一约束形成
  永久无法 ACK 的毒批次。
- 单个 history batch 校验或写入失败时只保留为未 ACK 并记录诊断，不关闭承载心跳与命令的
  Device Tunnel；后续重试可在数据修复或新版 Server 上继续处理。

## 6. 实时同步

实时路径不等待回填：

1. App Server notification 经 Adapter 规范化。
2. Agent SQLite 同一事务更新 Turn/Item 聚合并写 realtime/history outbox。
3. delta 经 SSE 实时展示。
4. Item completed 生成包含最终正文的 `history.item_upsert`。
5. Server 事务 upsert Item 并更新 Thread/Turn revision。
6. Server ACK 后 Agent 保留最低恢复窗口再清理。

用户消息可以在 Command 被 Server 接受时保存 pending 副本，但只有 Agent/App Server 规范化 Item 到达后才转为历史 confirmed，避免失败命令伪造为已执行会话内容。

## 7. 既有历史回填

调度顺序：

1. 活跃 Thread。
2. 最近 30 天 Thread。
3. 其余未归档 Thread。
4. 归档 Thread。

每轮只处理有界页数，让出资源给实时事件。断线、App Server busy、设备电量/睡眠策略可暂停。无法通过稳定 App Server API 读取的历史必须标记 partial，不直接读取 Codex 内部 SQLite 绕过兼容层。

## 8. Revision 和幂等

- Thread、Turn、Item 使用稳定 Nuntius ID。
- `revision` 对同一记录单调递增。
- `content_hash` 对规范化内容计算。
- incoming revision 小于当前值：忽略并 ACK 当前值。
- revision 相等且 hash 相同：幂等成功。
- revision 相等但 hash 不同：记录 conflict，不覆盖。
- revision 更高：事务更新并记录历史同步事件。

## 9. 内容大小

第一版 Server SQLite 直接保存普通消息正文。限制：

- 单消息正文硬上限。
- 命令输出按块聚合并设置更高但有限上限。
- 超限时优先压缩或写受控外部 Blob Store；若第一版未部署 Blob，则明确截断并保存原始字节数和 hash。
- UI 展示 `is_truncated` 和完整性说明。

不能因为超大输出回滚整个 Turn 历史批次；大内容错误与结构化元数据分离处理。

## 10. 查询 API

```text
GET /api/v1/history/threads
GET /api/v1/history/threads/{thread_id}
GET /api/v1/history/threads/{thread_id}/turns
GET /api/v1/history/turns/{turn_id}/items
GET /api/v1/history/items/{item_id}
POST /api/v1/devices/{device_id}/history-sync
POST /api/v1/threads/{thread_id}/history-sync
DELETE /api/v1/history/threads/{thread_id}
```

列表和 Item 都使用 cursor 分页。跨设备列表索引 `(user_id, last_activity_at, thread_id)`。

## 11. 优先级和背压

优先级：

```text
P0 approval/interrupt/terminal
P1 当前 Turn realtime/final history
P2 当前 Turn delta/交互式 live query（由公共 Tunnel 调度）
P3 最近 Thread backfill
P4 旧归档 Thread backfill
```

- History Worker 有独立带宽和 CPU 配额。
- WS/WSS 队列满时暂停 P3/P4，不丢 P0/P1；P2 delta 可以合并但最终 Item 不可丢。
- Server SQLite 压力高时降低 backfill 并发。
- 每设备公平调度，单台大历史不能饿死其他设备。

## 12. HTTP 模式

HTTP/WS 模式的历史同步协议和幂等不变，但消息正文、设备目录和身份凭证会经过未加密网络。Server 与 UI 必须标记 transport security；测试不能因为功能成功就把 HTTP 标记为安全通过。

## 13. 故障恢复

- Agent 重启：从 history_checkpoints/outbox 继续。
- Server 重启：Server SQLite batch 表去重，Agent 重放未 ACK batch。
- App Server 重启：Backfill 暂停，重新 initialize 后继续 cursor。
- Device offline：Server 历史保持可读，sync_state/completeness 不回退；派生 freshness 变 stale 并保留 last_synced_at。
- Server SQLite busy/只读/损坏：Agent 不清理 history outbox，Server 不 ACK；未持久化事件不能伪装成成功。
- 内容冲突：单 Item 标记 conflict，Thread partial，不阻塞其他 Thread。

## 14. 测试

- 历史批次重复、乱序、丢失和 hash 冲突。
- Agent/Server 在 batch commit/ACK 前后 crash。
- 既有 Thread 分页回填断点续传。
- 实时 Item 与 backfill 同时 upsert 竞态。
- 设备 offline 完整历史查询 E2E。
- 超大输出截断/外置不破坏 Turn。
- 多设备归属和授权隔离。
- system unassigned Project、稳定 ID 重建和无 cwd 泄露测试。
- complete/partial 与 live/stale 两维状态组合测试。
- 长历史分页性能和数据库索引测试。
