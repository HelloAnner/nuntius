# Thread 会话管理：技术设计

> 实现标记：本文描述目标设计；当前 `0.1.0` 已实现项和后续边界以 [后端实现状态](../../implementation-status.md) 为准。

## 1. 组件

- `ThreadService`：创建、恢复、归档和显示标题。
- `ThreadRepository`：本地映射和摘要。
- `ThreadReconciler`：与 App Server 对账。
- `ThreadIndexPublisher`：向 Server 同步索引变更；完整内容交给 History Aggregation。
- `ThreadQueryService`：本地分页、搜索和详情快照。

## 2. Agent 数据模型

### threads

```text
id
device_id
project_id                 # 始终存在；未映射时为 system unassigned project
app_server_thread_id nullable unique
display_title nullable
title_source: app_server | derived | user
status
archive_status
last_turn_id nullable
last_activity_at
created_at
updated_at
summary_version
mapping_source: created | auto_by_cwd | system_unassigned | manual
last_reconciled_at nullable
```

### thread_snapshots

```text
thread_id primary key
app_server_status
cwd_hash nullable
model nullable
active_turn_id nullable
pending_approval_count
message_preview nullable
captured_at
```

Server 保存对应全局索引，并由 History Aggregation 表保存完整规范化 Turn/Item/Content；本地该表仍只保存恢复和当前 UI 所需的有限聚合。

## 3. 创建命令

```text
POST /api/v1/devices/{device}/projects/{project}/threads
Idempotency-Key: ...
```

Server 命令：`thread.create`。

Agent 执行：

1. inbox 去重。
2. 校验 Project。
3. 创建本地 Nuntius Thread，状态 creating。
4. 调用 Adapter `thread/start`。
5. 保存 `app_server_thread_id`，状态 ready。
6. 发布 `thread.created`。
7. 如果命令含首条消息，派生独立 `turn.start` 子命令，复用原 correlation ID。

步骤 4 到 5 的 crash window 按 App Server Adapter 的 orphan 核对策略处理。

## 4. 查询与分页

本地排序键：

```text
(last_activity_at DESC, thread_id DESC)
```

cursor 编码最后一项的两个值并签名或校验，避免客户端修改。远程 Server 从全局 Thread/History Repository 查询，结果带 `history_completeness`、`last_synced_at` 和 device freshness；只有执行状态需要区分 device live/stale。

## 5. Reconcile 算法

触发：Agent 启动、App Server 重启、用户刷新、固定低频周期。

1. 分页读取 App Server Thread 列表。
2. 以 `app_server_thread_id` 匹配本地映射。
3. 更新状态、归档和活动时间。
4. 新 Thread 按 Project cwd 规则尝试关联。
5. 本地映射未在完整 App Server 列表出现时先标记 missing candidate。
6. 再次完整扫描仍不存在才标记 orphaned。
7. 增加 summary_version 并投递摘要。

避免一次短暂读取失败就把所有 Thread 标记 orphaned。

## 6. 归档状态机

归档命令先检查无 active Turn，然后调用 App Server：

```text
ready -> archiving -> archived
archiving timeout -> reconciling -> archived | ready | unknown
```

重复归档通过读取当前 archive_status 幂等处理。取消归档同理。

## 7. 标题生成

- Adapter 标题优先。
- derived 标题取首条用户文本规范化后前若干 Unicode grapheme。
- 去除换行、控制字符和疑似秘密模式。
- 用户标题存本地并具有最高显示优先级。
- 标题索引 Publisher 只接收最终 display title；消息正文由独立 History Aggregation DTO 同步。

## 8. 事件

- `thread.created`
- `thread.updated`
- `thread.active`
- `thread.archived`
- `thread.unarchived`
- `thread.orphaned`
- `thread.reconcile_required`
- `thread.summary_updated`

所有 Thread 事件带 `device_id`、`project_id`、`thread_id` 和 summary version。

## 9. 并发和一致性

- 每个 Thread 的修改命令串行。
- Create 以 command id 和 idempotency key 去重。
- Archive 与 Turn Start 通过 Thread 状态 CAS，不能同时成功。
- Project remove 与 Thread create 使用 Project 级串行域。
- Server Thread 索引只按更高 summary_version 更新；History Item 另按 source revision/content hash 防旧回填覆盖新终态。

## 10. 测试

- 两阶段 create 每个 crash point。
- 首个 Turn 失败不重复 Thread。
- Reconcile 分页失败和重复页测试。
- 自动 Project 关联歧义测试。
- Archive/Turn Start 竞态测试。
- Cursor 分页稳定性测试。
- Server 旧 summary 覆盖保护测试。
- Server 离线历史分页、完整度和跨设备授权测试。
