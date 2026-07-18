# Turn、消息与执行事件：功能设计

> 实现标记：本文描述目标设计；当前 `0.1.0` 已实现项和后续边界以 [后端实现状态](../../implementation-status.md) 为准。

## 1. 目标

提供一次用户请求从提交、执行、流式展示到明确终态的完整体验。Turn 是 Thread 中的一次工作单元，包含用户输入、Agent 回复、命令、工具调用、文件变化和审批等待。

## 2. 输入能力

第一版支持：

- 纯文本消息。
- 在活动 Turn 中追加文字指导。
- 中断活动 Turn。

图片和文件附件通过独立上传模块后续接入，不把大对象直接塞入 SSE/WS(S) JSON 消息。

## 3. 发送流程

1. 用户输入文本并点击发送。
2. 页面本地保存草稿并生成幂等键。
3. Server 返回 `202 Accepted + command_id`。
4. 页面展示“等待设备”。
5. 设备确认后展示“已送达”。
6. App Server 开始 Turn 后展示执行中。
7. Agent 消息和执行 Item 实时增量显示。
8. 收到 `turn.completed` 后展示明确终态。

用户关闭页面不影响设备上的 Turn 继续运行。

## 4. Turn 状态

```text
queued
delivered
starting
running
waiting_approval
completed
failed
interrupted
unknown
```

- `queued`：Server 已保存，设备尚未确认。
- `delivered`：设备 inbox 已保存。
- `starting`：正在调用 App Server。
- `running`：App Server 已发出开始事件。
- `waiting_approval`：存在阻塞审批。
- `unknown`：无法确认是否启动或结束。

## 5. Item 展示

### 用户消息

- 展示原始输入。
- 明确发送状态。
- 失败或 unknown 时提供“查询状态”，而不是直接再次发送。

### Agent 消息

- delta 实时合并到同一个消息气泡。
- 完成时固化正文。
- 网络重放的重复 delta 不重复追加。

### 命令执行

- 展示命令摘要、工作目录提示、开始时间和状态。
- 输出默认折叠并限制前端内存。
- 高风险内容不在通知预览中展示。

### 文件变化

- 展示文件数量、路径摘要和变更状态。
- 第一版不实现完整远程代码编辑器。
- Diff 是否展示取决于 App Server 可稳定提供的事件和安全策略。

### 工具调用

- 展示工具名称、进行中/完成/失败。
- 参数和结果按安全策略脱敏。

## 6. Steering

- 只有当前 Turn 仍允许追加输入时展示入口。
- Steering 是新命令，有独立 command ID 和状态。
- 发送失败不影响原 Turn。
- 多条 steering 按 Thread 顺序执行。

## 7. Interrupt

- 用户点击中断后先显示“正在中断”。
- 中断请求送达不等于 Turn 已结束。
- 只有收到 Turn 终态后才显示 interrupted/completed/failed。
- 重复中断幂等处理。
- 设备离线时不缓存长期中断；请求有短 expires_at。

## 8. 重连体验

- 页面重连后先恢复当前 Turn 快照。
- 从 cursor 补发缺失事件。
- delta 缺口无法补齐时，设备返回聚合后的 Item 快照。
- 页面不得因为重连创建新的 Turn。
- 当前 Turn 结果不确定时显示 unknown 和核对操作。

## 9. 输入限制

- 空白消息不能发送。
- 文本长度有明确上限并显示剩余量。
- 防止快速重复点击，依靠同一幂等键兜底。
- 设备 offline/syncing/degraded 时说明不可发送原因。
- 项目不可用或 Thread 已归档时禁止发送。

## 10. 验收标准

1. 一次发送在重复 HTTP 重试时只创建一个 Turn。
2. 页面关闭后 Turn 继续，重新打开可恢复状态。
3. delta 重放不会产生重复文本。
4. interrupt 状态与 Turn 终态明确区分。
5. steering 按 Thread 顺序处理。
6. 事件缺口会触发 replay/snapshot，不静默跳过。
7. unknown 请求不会显示为普通失败或自动重发。
