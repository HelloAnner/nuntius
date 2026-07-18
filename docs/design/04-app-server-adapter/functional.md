# Codex App Server 适配：功能设计

> 实现标记：本文描述目标设计；当前 `0.1.0` 已实现项和后续边界以 [后端实现状态](../../implementation-status.md) 为准。

## 1. 目标

把 Codex App Server 的认证、Thread、Turn、Item、审批和流式通知能力转换为稳定的 Nuntius 本地领域接口，使上层不依赖 App Server 的具体协议版本。

能力边界以 [Codex App Server 官方文档](https://learn.chatgpt.com/docs/app-server) 和目标 Codex 版本生成的 Schema 为准。核心集成使用默认 stdio JSONL；官方 WebSocket 传输仍属实验能力，不作为 Nuntius Agent 的核心依赖或公网链路。

## 2. 用户可见能力

- 检测 Codex CLI/App Server 是否安装。
- 查看 Codex 版本和登录状态摘要。
- 启动、停止和自动重启 App Server。
- 列出、创建、恢复和归档 Thread。
- 在指定项目目录启动 Turn。
- 流式查看消息、命令、工具和文件事件。
- 追加指导和中断当前 Turn。
- 处理 App Server 发出的审批请求。
- 在 App Server 异常后核对会话状态。
- 枚举已有 Thread，并把既有与新增 Thread/Turn/Item 归一化为可幂等同步到 Server 的完整历史。

## 3. 状态

```text
not_installed
stopped -> starting -> initializing -> ready
                    ├-> auth_required
                    ├-> incompatible
                    └-> failed
ready -> restarting -> initializing
```

状态含义：

- `auth_required`：Codex 安装正常，但需要用户在本机完成登录。
- `incompatible`：版本或 Schema 不在支持范围。
- `failed`：启动或运行错误，可查看诊断。
- `ready`：已完成 initialize，可接受命令。

## 4. 认证边界

- Nuntius 不接管、上传或复制 Codex 登录凭证。
- Codex 登录在设备本地完成。
- 远程页面只展示“已登录/需要登录/未知”，不展示 Token。
- 需要交互式登录时提示用户回到目标电脑处理。

## 5. Thread 操作

- 新建：指定 Project，使用其 `cwd` 和默认配置。
- 恢复：使用本地保存的 App Server Thread ID。
- 列表：按项目和最近活动过滤。
- 归档/取消归档：保留 Nuntius 映射。
- 分叉：作为后续或受能力控制功能，不影响核心恢复。

Thread 创建与首个 Turn 分两步完成，避免无法保存 Thread ID 时重复执行首个用户任务。

## 6. Turn 操作

- `turn.start`：只有 App Server ready 且 Project 有效时执行。
- `turn.steer`：仅对允许 steering 的活动 Turn 开放。
- `turn.interrupt`：对活动 Turn 开放，结果以完成通知为准。
- 每个请求显示等待、已发送、执行中和终态。

## 7. 事件规范化

上层只使用 Nuntius 事件类型，例如：

- `turn.started`
- `turn.completed`
- `item.user_message`
- `item.agent_message.delta`
- `item.agent_message.completed`
- `item.command.started`
- `item.command.output_delta`
- `item.command.completed`
- `item.file_change.completed`
- `approval.requested`

未知 App Server 通知不能导致 Agent 崩溃；记录兼容性指标后安全忽略，若影响完整性则触发重新同步。

`item.*.completed` 规范化事件必须含可长期阅读所需的最终内容或明确的截断/不可用标记，不能只保留摘要。原始 token delta 仅用于流式体验和短期重放。

## 8. 异常和恢复

- 启动超时：终止子进程，退避后重试。
- stdout EOF：视为子进程退出，所有 pending 请求进入核对。
- 单请求超时：不直接重发副作用请求。
- App Server 过载：按官方建议退避加 jitter。
- 重启后：重新 initialize，读取 Thread/Turn 状态并恢复订阅映射。
- 活跃 Turn 无法恢复：标记 `unknown` 或失败，提示用户本地核对。

## 9. 第一版不做

- 修改 App Server 源码。
- 远程暴露 App Server WebSocket。
- 直接读取 Codex 内部 SQLite。
- 管理多个 Codex Profile。
- 依赖实验性 API 作为核心路径。

## 10. 验收标准

1. App Server 只通过本地 stdio 与 Agent 通信。
2. 未完成 initialize 前不发送业务请求。
3. 创建 Thread 成功保存映射后才发送首个 Turn。
4. App Server 退出时 Agent 能检测并退避重启。
5. 未知通知不会导致进程崩溃。
6. 超时但结果不确定的副作用请求不会被自动重复。
7. 上层 UI 不需要理解 App Server 原始 Schema。
8. 已有会话可按 checkpoint 分页归一化并回填，重复扫描不产生重复 Thread/Turn/Item。
9. App Server WebSocket 能力变化不影响 stdio 核心路径。
