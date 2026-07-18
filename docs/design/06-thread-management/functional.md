# Thread 会话管理：功能设计

> 实现标记：本文描述目标设计；当前 `0.1.0` 已实现项和后续边界以 [后端实现状态](../../implementation-status.md) 为准。

## 1. 目标

让用户按设备和项目查看、创建、恢复、归档 Codex Thread。Thread 是长期对话容器，不随浏览器连接或单次 Turn 结束而消失。

## 2. Thread 信息

列表展示：

- 标题。
- 所属设备和项目。
- 最近一条活动摘要。
- 最近活动时间。
- 当前状态：空闲、执行中、等待审批、归档、未知。
- 当前模型/配置摘要。
- 数据新鲜度。

远程 Server 保存 Thread 全局索引和完整规范化历史；设备负责 App Server 的可执行状态和继续会话。

## 3. 创建 Thread

用户需要选择：

- 目标 Device。
- 目标 Project。
- 可选的模型、sandbox 等允许覆盖项。
- 可选首条消息。

创建过程：

1. 页面显示“正在创建会话”。
2. 设备先创建空 Thread。
3. 保存 App Server Thread 映射。
4. 页面进入 Thread。
5. 有首条消息时再创建 Turn。

若首条消息失败，Thread 仍然存在，并允许用户重发；不重复创建 Thread。

## 4. 历史 Thread 发现

Agent 启动或用户刷新时，通过 App Server 获取已有 Thread：

- 已有 Nuntius 映射：更新摘要。
- 可按 cwd 唯一匹配 Project：自动导入。
- 无法关联：进入该设备系统“未归类”Project。
- App Server 中已归档：同步归档状态。

远程页面也展示未归类 Thread 的已同步历史，但不展示 cwd，也不允许继续执行。归类动作只在本地页面完成，避免 Server 通过任意路径重绑定。

## 5. 恢复 Thread

- 打开 Thread 时直接从 Server 分页展示已同步完整历史，并显示同步完整性。
- 若设备在线，Agent 调用 App Server resume/read 能力。
- 若设备离线，仍可只读查看 Server 已同步的完整规范化历史，但不能继续对话。
- 恢复失败时保留映射并显示原因，不自动创建替代 Thread。

## 6. 归档与取消归档

- 归档不会删除会话。
- 活跃 Turn 存在时不能归档，除非先中断。
- 归档后从默认列表隐藏，可在归档筛选中查看。
- 取消归档恢复到原 Project。
- 操作最终以设备 App Server 状态为准。

第一版不提供远程永久删除 Thread。

## 7. 标题

- 优先使用 App Server 提供的标题。
- 无标题时使用首条用户消息的本地短摘要。
- 用户可在 Nuntius 层设置显示标题；不要求回写 App Server。
- 标题不得包含未经处理的多行文本或过长 prompt。

## 8. Thread 状态

```text
creating -> ready -> active -> ready
                  -> waiting_approval -> active/ready
ready/active -> archiving -> archived
archived -> unarchiving -> ready
任意操作态 -> unknown
映射失效 -> orphaned
```

- `orphaned`：Nuntius 有映射，但 App Server 找不到对应 Thread。
- `unknown`：最近操作结果无法判定，需核对。

## 9. 分页和筛选

- 按最近活动时间倒序。
- 支持 active、archived、unassigned 筛选。
- 第一版支持按标题搜索；远程由 Server 全局 Thread 索引执行，设备离线也可用。完整消息全文搜索暂不实现，避免首版引入额外索引复杂度。
- 列表使用 cursor 分页，不使用不稳定 offset。

## 10. 验收标准

1. 新建 Thread 的映射保存后才发送首条 Turn。
2. 首条 Turn 失败不会重复创建 Thread。
3. 历史 Thread 能按 cwd 自动关联或进入系统未归类 Project，并在不泄露 cwd 的前提下出现在全局历史。
4. 离线设备可展示 Server 已同步的完整历史，并明确标记最后同步时间和 `complete/backfilling/partial`。
5. 归档不会删除 Thread。
6. orphaned/unknown 有明确状态和核对入口。
7. 列表分页在新增 Thread 时不会重复或跳过已有项。
8. 多设备历史都遵循 Device -> Project -> Thread 归属，不能通过修改 ID 跨设备读取。
