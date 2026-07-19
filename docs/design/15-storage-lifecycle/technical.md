# 数据存储与生命周期：技术设计

> 实现标记：本文描述目标设计；当前 `0.1.0` 已实现项和后续边界以 [后端实现状态](../../implementation-status.md) 为准。

## 1. 存储选型

- Agent：SQLite，WAL，`synchronous=FULL`，短事务。
- Server：SQLite，使用 WAL、事务、唯一约束和定期备份。
- Blob：第一版不引入对象存储；未来附件使用对象存储或设备直传的独立设计。
- Cache：第一版使用有界进程内缓存，不引入 Redis。

## 2. Schema 所有权

每张表由一个模块拥有：

| 表类别 | 所有者 |
|---|---|
| users/sessions/pairing/device_keys | identity |
| devices/device_summary | device |
| projects/project_git | project |
| threads/thread_snapshot | thread |
| history_threads/history_turns/history_items/history_batches/history_checkpoints | history aggregation |
| active_turns/approvals | turn/approval |
| commands（含 durable dispatch 状态） | reliable messaging |
| device_inbox/device_outbox/cursors | reliable messaging |
| audit_events | security |

其他模块通过 Repository/Service 访问，不跨模块直接写表。

`history_*` 表按稳定的本地实体 ID + device ID 建唯一约束，并保留 `source_revision`、`content_hash`、`synced_at` 和软删除标志。Server 的历史 DTO 不直接复用某个 Codex 版本的原始 JSON，防止 App Server 升级导致长期数据无法读取。

## 3. SQLite 配置

启动时执行：

```sql
PRAGMA journal_mode=WAL;
PRAGMA synchronous=FULL;
PRAGMA foreign_keys=ON;
PRAGMA busy_timeout=5000;
```

策略：

- 连接池保持较小，写操作集中。
- 不持有跨 await 的事务。
- 页面长列表分页，避免长读事务阻塞 checkpoint。
- 定期 passive checkpoint；维护窗口可 truncate checkpoint。
- 监控 WAL 文件大小和 busy 次数。

## 4. Server SQLite 配置原则

- Server 为单活进程，数据库固定为 `<data-dir>/nuntius-server.db`；不得通过网络文件系统或多个进程共享写入。
- 使用 SQLx 小连接池、WAL、`synchronous=FULL`、外键、5 秒 busy timeout 和短事务。
- durable command、幂等键、History Batch 与 ACK checkpoint 都依靠数据库唯一约束和事务提交建立可靠性边界。
- 对命令 pending 状态、事件 cursor、历史 `(thread_id, ordinal)` 与 `(turn_id, ordinal)` 建覆盖索引。
- 大消息正文与结构化详情和列表摘要分列，列表接口不读取无关大字段。
- event journal、过期 Session、Device Token、Challenge 和 directory ref 由有界维护任务清理。
- 监控 DB、`-wal`、`-shm` 总体积、busy 次数、事务时延和磁盘剩余空间。

## 5. 迁移

迁移规则：

1. 版本化、不可修改已发布迁移。
2. Server/Agent 启动前检测 schema version。
3. 使用 expand-migrate-contract：先新增兼容字段，再迁移数据，最后在旧版本退出支持后删除。
4. 迁移失败不启动写服务。
5. Agent 升级前备份小型 SQLite 文件或使用 SQLite backup API。
6. Server 迁移只能由单个进程执行，并在发布前用生产数据量副本评估锁时间和额外磁盘空间。

## 6. 数据清理 Worker

清理批次化执行，避免长事务：

- 每批限制行数和运行时间。
- 删除前验证 ACK/终态/保留期。
- 历史软删除到期后按 Item -> Turn -> Thread 批次清理；清理 checkpoint/tombstone 前先越过最大重放窗口。
- 记录删除数量、耗时和最老数据年龄。
- 失败退避重试。
- 不与前台关键事务竞争大量锁。

## 7. 磁盘压力状态机

```text
normal -> warning -> critical -> read_only_degraded
```

- warning：加速清理已确认 delta，提示用户。
- critical：拒绝新 Turn，仍允许 interrupt/approval 和状态查询。
- read_only_degraded：无法写 inbox 时断开远程命令能力，不发送虚假 ACK。
- 空间恢复后执行 integrity check 和 outbox 核对再恢复 online。

## 8. Agent 数据库损坏

1. 停止接受远程命令。
2. 复制损坏 DB/WAL 到诊断目录，限制权限。
3. 运行 SQLite integrity check。
4. 尝试官方恢复流程或从备份恢复。
5. 无法恢复时创建新 DB，并从 App Server 重建 Project/Thread 可发现摘要。
6. 未确认命令无法证明时在 Server 标记 unknown。

不删除 Codex state，也不直接修补 Codex 数据库。

## 9. Server 备份恢复

- 停服后使用 `nuntius-server --data-dir <dir> backup` 生成目录型备份：SQLite 通过 `VACUUM INTO` 生成一致性快照，同时复制图片附件目录；禁止仅复制活跃 DB 主文件。未来需要无停机备份时再切换 online backup API。
- 备份包含 schema、用户、设备、命令状态、规范化完整历史和审计元数据。
- 加密保存，权限最小化。
- 定期恢复到隔离实例并运行一致性检查。
- 恢复点之后设备凭证撤销可能回退，因此恢复后需执行安全审计并允许 Owner 全部撤销/重新配对。
- 恢复完成后，将 history checkpoint 标为待核对；Agent 比较 source revision/content hash，只回填缺失或更新的数据。

## 10. 数据最小化实现

- DTO 白名单决定上传字段。
- Project 路径只上传 hash/hint。
- Event payload 按类型保留必要字段；历史 DTO 明确允许完整用户/Agent 消息，但不允许凭证、任意环境变量或源文件正文。
- 命令输出、diff 和工具结果按类型设置字节上限、截断标记和脱敏钩子；未经白名单的原始 payload 不进入长期表。
- Server SQLite 不建立无限保留的通用 raw_event 表。
- 临时内存 buffer 有最大字节数和 TTL。
- 诊断导出默认不含 DB 原文件。

## 11. 历史写入与查询路径

```text
Agent normalized batch
  -> history_inbox 去重
  -> 校验 Device/Project/Thread 归属
  -> upsert Thread/Turn/Item
  -> 更新 history_checkpoint/completeness
  -> 写 replayable event journal
  -> transaction commit
  -> server.history_persisted ACK
```

- 批次事务必须有行数和总字节上限。
- 相同 source revision/content hash 是 no-op；更高 revision 才允许更新可变终态。
- 旧回填批次不得覆盖较新的实时终态。
- 历史列表使用 keyset pagination；正文按 Thread/Turn 分页加载。
- 目录 live query 不写入这些表，只有 Project 创建成功后的稳定索引进入存储。

## 12. 测试

- SQLite WAL crash/recovery tests。
- Server SQLite transaction rollback 与 WAL crash-recovery tests。
- Migration upgrade/rollback compatibility tests。
- Cleanup 不删除未 ACK 数据 property tests。
- 磁盘满和只读文件系统故障注入。
- Backup restore drill 自动校验。
- 数据最小化字段 snapshot tests。
- 历史批次重复、乱序、实时与回填竞态测试。
- 百万级 Item 的 keyset pagination 与备份恢复容量测试。
- 完整消息可读、原始 delta 已清理以及目录树未落库的联合测试。
