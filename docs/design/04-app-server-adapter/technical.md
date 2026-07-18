# Codex App Server 适配：技术设计

> 实现标记：本文描述目标设计；当前 `0.1.0` 已实现项和后续边界以 [后端实现状态](../../implementation-status.md) 为准。

## 1. 组件

- `AppServerSupervisor`：进程生命周期、退避和熔断。
- `JsonlTransport`：stdin 写入、stdout 分行读取、stderr 日志。
- `JsonRpcDispatcher`：请求 ID、pending map、响应和超时。
- `AppServerClient`：Thread/Turn/Approval 类型化方法。
- `EventNormalizer`：原始通知转 Nuntius Event。
- `HistoryNormalizer`：Thread/Turn/Item 快照转长期稳定 DTO。
- `HistoryInventoryReader`：按支持 Schema 枚举可发现历史并维护本地扫描 checkpoint。
- `CapabilityMapper`：版本与功能能力。
- `Reconciler`：重启后状态核对。

## 2. 子进程启动

```text
codex app-server --listen stdio:// --strict-config
```

实际参数以受支持版本为准。环境处理：

- 默认继承当前用户用于 Codex 的必要环境。
- 删除 Nuntius Server Token 等与 Codex 无关秘密。
- 不覆盖 `CODEX_HOME`，除非用户显式配置。
- stdout 必须只用于协议。
- stderr 进入单独脱敏日志流。

## 3. 初始化状态机

```text
spawned
  -> send initialize(id=0, clientInfo, capabilities)
  -> wait response
  -> send initialized notification
  -> ready
```

- 初始化有独立超时。
- 连接内只允许一次 initialize。
- `clientInfo.name` 使用稳定标识，例如 `nuntius_agent`。
- 默认不启用 experimentalApi；需要时由明确 feature gate 开启。

## 4. JSONL Transport

- 每条请求序列化为单行 JSON，加 `\n` 后写 stdin。
- 单 writer task 保证行不交叉。
- stdout reader 设置最大行长，例如 16 MiB，可配置上限。
- 非法 UTF-8、超长行、非法 JSON 触发 protocol error。
- 一次协议错误先隔离当前消息；连续错误超过阈值重启进程。
- 写入 flush 失败立即标记 transport closed。

## 5. 请求分发

```rust
PendingRequest {
    request_id: u64,
    command_id: Option<CommandId>,
    method,
    sent_at,
    timeout_at,
    retry_class,
    response_tx,
}
```

- request ID 在单 App Server 进程实例内单调递增。
- 进程重启后创建新的 dispatcher generation。
- 响应必须匹配 generation + request ID。
- 未知响应 ID 记录指标并忽略。
- timeout 只结束等待，不自动断言请求失败。

## 6. 方法重试分类

| 分类 | 示例 | 超时处理 |
|---|---|---|
| ReadOnly | list/read/status | 新连接后可安全重试 |
| IdempotentState | archive/unarchive，需核对状态 | 先读取状态，再决定 |
| NonIdempotent | thread/start、turn/start | 不盲重试，进入 reconcile |
| Ephemeral | steer/interrupt/approval | 核对活动状态，过期则失败/unknown |

具体分类随支持的 App Server Schema 固化为代码，不由调用者随意指定。

## 7. Thread 两阶段创建

```text
1. 本地创建 Nuntius Thread(status=creating)
2. 调用 App Server thread/start
3. 获得 app_server_thread_id
4. SQLite 保存映射并 commit
5. Nuntius Thread(status=ready)
6. 如有首条消息，再调用 turn/start
```

若 2 成功但 4 前崩溃，恢复时使用启动时间、cwd 和 App Server 列表尝试识别空 Thread；无法唯一识别则记录 orphan 候选，不自动发送首个 Turn。

## 8. Event Normalizer

Normalizer 输入原始 notification，输出零到多个领域事件：

- 保存原始 method 名称作为诊断 metadata，但不透传任意 payload。
- 将 App Server Thread/Turn/Item ID 映射到 Nuntius ID。
- delta 按 Item 分配稳定 stream。
- completed 事件包含归一化终态、可阅读的最终消息内容和受控结构化执行详情；超限或源数据不可读时带明确 completeness/truncation。
- 敏感字段在进入公网协议前过滤。

流式 Normalizer 和 History Normalizer 产生相同的稳定实体键、source revision 与 content hash。这样实时终态先到、旧历史扫描后到时，Server 可以安全判定重复或旧版本。

未知事件策略：

- 可忽略扩展：计数并忽略。
- 影响状态完整性的未知 lifecycle：标记 compatibility degraded 并触发 snapshot。
- 无法解析的审批：禁止自动批准，显示不支持。

## 9. Schema 与版本

- 对每个支持的 Codex 版本生成 JSON Schema。
- CI 对 Adapter 类型和 Schema 做校验。
- 运行时读取 Codex version，选择兼容 mapper。
- 支持范围外进入 `incompatible`。
- 新字段默认宽松读取，危险枚举默认拒绝。
- App Server 官方标为实验性的传输和字段不进入核心功能。
- 构建时运行 `codex app-server generate-json-schema`；生成物与 Codex 版本一起固定，升级时通过 golden diff 人工确认危险枚举和字段语义。

## 10. 历史读取与回填适配

1. 先使用受支持 Schema 中公开的 Thread 列表/读取能力，不直接读取 Codex 内部 SQLite。
2. 每页转成 `HistoryBatch`，保留 `device_id/project_id/thread_id` 归属和 source cursor。
3. 无法唯一映射 Project 的 Thread 归入设备 system unassigned project；同步规范化历史但不上传意外路径，用户只能在本地重归类。
4. 部分 Item 类型无法稳定归一化时保留 `unknown_item` 类型、官方 ID、时间和 completeness，不伪造成已完整。
5. App Server 版本升级导致 cursor 无效时重新扫描；稳定 ID、revision/content hash 保证 Server 幂等。
6. 回填 Reader 使用低优先级和有界批次，不能阻塞当前 Turn stdout reader、Approval 或 Interrupt。

## 11. 重启与核对

App Server 重启后：

1. 新 generation initialize。
2. 读取已映射 Thread。
3. 对本地标记 active/unknown 的 Turn 查询最新状态。
4. 已有明确终态则补发规范化终态事件。
5. 仍在运行且协议允许恢复则恢复观察。
6. 无法判断则保持 unknown 并通知 UI。

Supervisor 使用 full-jitter backoff，并在连续失败超过阈值后熔断。用户手动重试或配置变化可解除熔断。

## 12. 安全

- App Server 仅作为当前用户子进程。
- 不监听公网端口。
- 不把 Nuntius 远程凭证传入子进程。
- 原始命令和文件内容不写普通日志。
- App Server 版本和二进制路径进入诊断但做路径脱敏。

## 13. 测试

- Mock JSONL server 覆盖 initialize、响应、通知、乱序和非法行。
- request ID generation 和 pending timeout 测试。
- 进程退出、stdin broken pipe 和 stdout EOF 测试。
- 两阶段 Thread 创建 crash-point 测试。
- Event Normalizer golden tests。
- 支持版本 Schema contract tests。
- NonIdempotent 请求超时不重试测试。
- 多版本 History Normalizer golden tests、分页 checkpoint 和重新扫描去重测试。
- 实时 completed 与旧历史回填乱序时 stable ID/revision/content hash 一致性测试。
- 不直接依赖 App Server WebSocket 或 Codex 内部 SQLite 的架构约束测试。
