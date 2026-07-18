# 审批：技术设计

> 实现标记：本文描述目标设计；当前 `0.1.0` 已实现项和后续边界以 [后端实现状态](../../implementation-status.md) 为准。

## 1. 组件

- `ApprovalNormalizer`：App Server 请求转稳定视图。
- `ApprovalService`：状态、决策和超时。
- `ApprovalRepository`：Agent SQLite 当前审批。
- `ApprovalPolicy`：风险等级和展示规则。
- `ApprovalReconciler`：Turn/App Server 重启后核对。

## 2. Agent 数据模型

### approvals

```text
id
thread_id
turn_id
app_server_request_id
kind
risk_level
status
options_json
summary_json
requested_at
expires_at nullable
decision_command_id nullable unique
decided_at nullable
updated_at
```

完整原始 payload 仅在本地短期存储或内存中保留，公网摘要经过白名单映射。

## 3. 规范化视图

```rust
ApprovalView {
    approval_id,
    target: {device_id, project_id, thread_id, turn_id},
    kind,
    risk_level,
    title,
    summary,
    affected_paths,
    cwd_hint,
    options: Vec<ApprovalOption>,
    status,
    requested_at,
    expires_at,
    truncated,
}
```

`ApprovalOption` 包含稳定 option ID 和展示文本，Adapter 负责映射回 App Server 所需响应。

## 4. 事件与命令

事件：

- `approval.requested`
- `approval.responding`
- `approval.approved`
- `approval.denied`
- `approval.expired`
- `approval.cancelled`
- `approval.unknown`

命令：

```text
approval.decide {
  approval_id,
  expected_turn_id,
  option_id
}
```

命令必须包含幂等键和短 expires_at。

## 5. Compare-And-Set

Agent 收到决策命令后在 SQLite 中执行：

```sql
UPDATE approvals
SET status = 'responding', decision_command_id = ?
WHERE id = ?
  AND turn_id = ?
  AND status = 'pending';
```

- 更新 1 行：该命令获得处理权。
- 更新 0 行：读取当前状态并返回 already_decided/expired/not_found。
- 同一个 command 重放返回原处理状态。

## 6. App Server 响应

1. CAS 获得处理权。
2. 调用 Adapter 使用原 `app_server_request_id` 响应。
3. 收到明确成功或后续状态通知后写 approved/denied。
4. App Server 明确拒绝则回到相应失败/过期状态，不自动换用其他 option。
5. 请求超时且无法核对则写 unknown。

审批决定不得仅因为 stdin write 成功就标记 approved。

## 7. 与 Turn 状态联动

- `approval.requested` 将 Turn 派生状态设为 waiting_approval。
- 同一 Turn 可存在多个审批；全部非 pending 后才离开 waiting_approval。
- Turn 进入终态时，将 pending/responding 审批核对为 cancelled/unknown。
- interrupt 与 approval decision 串行进入 Thread Actor，但最终以 App Server 事件为准。

## 8. 重连和重启

- Agent SQLite 保存 pending 审批摘要。
- WS/WSS 重连后优先重放 approval Durable Events。
- Browser SSE 重连通过快照恢复 pending 列表。
- App Server 重启后查询活动 Turn；无法恢复原 request ID 时审批标记 cancelled/unknown，禁止用旧 ID 响应。
- Server 保存审批路由状态和规范化历史结果；审批中不需要长期阅读的原始敏感 payload 仍只留本地或按白名单过滤。

## 9. 安全

- Server 每次决策校验用户、设备、Thread、Turn 和 approval 归属。
- UI option ID 必须属于当前 approval options，不能透传任意字符串。
- risk_level 由 Adapter 白名单规则计算，不相信浏览器输入。
- 高风险二次确认是 UI 防误触，真正授权仍由后端状态和 App Server 决定。
- 审批摘要字段长度和内容类型有限制。

## 10. 优先级和背压

- Approval requested/decision/terminal 属 P0/P1。
- WS/WSS 和 SSE 队列必须优先于普通 delta 与历史回填。
- 队列满时可以合并 Agent 文本，不能丢审批。
- Pending 审批数量有上限；异常过量时设备进入 degraded 并拒绝新 Turn。

## 11. 测试

- 本地与远程并发批准 CAS 测试。
- 批准与拒绝并发测试。
- 决策重放幂等测试。
- Turn complete/interrupt 与 approval 竞态测试。
- App Server response 超时 unknown 测试。
- 未知 option 和未识别审批安全拒绝测试。
- 敏感字段白名单和日志脱敏测试。
