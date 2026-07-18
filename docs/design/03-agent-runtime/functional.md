# 设备 Agent 与 CLI 运行时：功能设计

> 实现标记：本文描述目标设计；当前 `0.1.0` 已实现项和后续边界以 [后端实现状态](../../implementation-status.md) 为准。

## 1. 目标

在每台工作电脑上提供一个轻量、可诊断、可自动恢复的本地运行时，连接公网 Server、管理项目和 Codex App Server，并提供本地控制台。

Agent 是后台能力，CLI 是用户操作入口，两者可以由同一个二进制以不同子命令运行。

## 2. CLI 命令范围

第一版建议：

```text
nuntius init                 初始化本地配置
nuntius pair                 使用配对码接入 Server
nuntius start                启动后台 Agent
nuntius stop                 停止后台 Agent
nuntius restart              重启 Agent
nuntius status               查看各层状态
nuntius open                 打开本地控制台
nuntius projects add <path>  添加项目
nuntius projects list        列出项目
nuntius doctor               生成脱敏诊断
nuntius logs                 查看本地日志
nuntius unpair               撤销本地凭证并断开
nuntius version              输出版本和协议范围
```

CLI 命令命名在实现前可调整，但能力边界保持一致。

## 3. 初始化

- 检查支持的操作系统和架构。
- 创建本地数据目录和数据库。
- 检测 Codex CLI 是否存在。
- 检查本地端口和系统服务能力。
- 不自动登录 Codex，不复制 Codex 凭证。
- 重复运行 `init` 是幂等的，不覆盖有效配置。

## 4. Agent 运行状态

```text
stopped -> starting -> syncing -> running
                    ├-> degraded
                    └-> failed
running -> draining -> stopped
```

状态详情分层展示：

- Agent Process。
- Local Database。
- Public Server Connection。
- Codex App Server。
- Local Web Server。

某一层异常不应被简化为“Agent 失败”。例如公网连接异常时，本地功能仍可运行。

## 5. 本地独立能力

即使未配对或公网 Server 离线，Agent 仍能：

- 提供本地控制台。
- 管理项目。
- 启动 Codex App Server。
- 创建和恢复本地 Thread。
- 处理本地审批。

未配对状态只禁用远程连接，不影响本地功能。

## 6. 后台运行

- 安装时可注册用户级系统服务。
- 登录系统后自动启动是可选项，默认由用户选择。
- 进程异常退出由系统服务管理器重启。
- 活跃 Turn 存在时，普通退出需要提示或进入 draining。
- 强制退出不删除 inbox/outbox，重启后恢复。

## 7. 配置体验

用户可配置：

- Server URL。
- 当前传输档位；`http://` 明确标记不安全并建议 VPN/SSH，`https://` 失败不自动降级。
- 本地页面端口或自动端口。
- 是否开机启动。
- 日志级别和保留天数。
- 本地事件重放保留期和磁盘上限。
- App Server 可执行文件路径（高级）。
- 手机目录浏览 allowed roots、隐藏目录和 symlink 策略。
- 历史回填带宽/暂停开关和同步状态；不允许关闭实时终态同步后仍显示 complete。

远程 Server 不能未经本地明确允许修改 Agent 安全配置。

## 8. 错误和恢复

- Codex 未安装：本地项目仍可管理，执行能力禁用并提供安装提示。
- Codex 未登录：显示 App Server 认证问题和本地处理方式。
- SQLite 不可写：停止确认新远程命令，避免命令丢失。
- Server 不可达：本地继续工作，后台退避重连。
- App Server 崩溃：自动退避重启，当前不确定操作明确提示。
- 配置损坏：保留原文件并报告具体字段，不静默重置。

## 9. 第一版不做

- 远程升级操作系统。
- 远程任意 shell 管理入口。
- 多个系统用户共享一个 Agent。
- 同一 Agent 管理多个 Codex 身份/Profile。
- 自动修复 Codex 登录。

## 10. 验收标准

1. 公网离线时本地控制台仍能操作本机 Codex。
2. Agent 异常退出后可由系统服务管理器恢复。
3. 重启后 inbox/outbox 和项目数据不丢失。
4. `status` 能区分数据库、Server、App Server 和本地 Web 状态。
5. `init` 和 `start` 可重复执行且无破坏。
6. `doctor` 不包含 Token、prompt 和文件正文。
7. SQLite 不可写时 Agent 不确认新命令。
8. Agent 在 HTTP/WS 模式持续报告 insecure，并在 HTTPS/TLS 失败时不降级。
9. 历史回填和目录浏览 worker 受 supervisor、有界队列和优先级管理。
