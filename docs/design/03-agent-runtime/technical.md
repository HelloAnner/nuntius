# 设备 Agent 与 CLI 运行时：技术设计

> 实现标记：本文描述目标设计；当前 `0.1.0` 已实现项和后续边界以 [后端实现状态](../../implementation-status.md) 为准。

## 1. 进程模型

一个 `nuntius` 二进制包含 CLI 和 daemon 模式：

```text
nuntius <command>
nuntius agent run --foreground
```

系统服务启动 `agent run`。普通 CLI 命令通过本地控制 socket/HTTP API 与 daemon 通信；daemon 不存在时，安全的只读命令可以直接读取配置，修改命令提示启动 daemon。

## 2. Supervisor 树

```text
AgentSupervisor
├─ LocalHttpSupervisor
├─ TunnelSupervisor
├─ AppServerSupervisor
├─ CommandWorkerSupervisor
├─ EventOutboxSupervisor
├─ HistorySyncSupervisor
├─ DirectoryQuerySupervisor
├─ ProjectScannerSupervisor
└─ MaintenanceSupervisor
```

每个 supervisor：

- 暴露 health。
- 使用 CancellationToken。
- 设置重启退避和熔断阈值。
- 不静默吞掉 panic 或 JoinError。
- 将关键退出上报顶层 supervisor。

## 3. 本地目录

遵循平台用户数据目录，不与 `CODEX_HOME` 混用：

```text
NUNTIUS_HOME/
├─ config.toml
├─ nuntius.db
├─ run/
│  ├─ agent.pid
│  └─ local-endpoint.json
├─ logs/
└─ diagnostics/
```

- 数据目录权限限制为当前用户。
- 原子写配置：写临时文件、fsync、rename。
- PID 文件仅作诊断，不能作为唯一进程互斥；使用 OS 锁或本地 socket 绑定判断。

## 4. 配置模型

```toml
server_url = "https://example.com"
auto_start = true
local_bind = "127.0.0.1:0"
event_retention_hours = 24
event_disk_limit_mb = 512

[remote]
allow_insecure_http = false
history_backfill_enabled = true
history_backfill_max_mbps = 2

[directory_browser]
home_root_enabled = true
allow_hidden = false

[app_server]
binary = "codex"
startup_timeout_seconds = 20
```

设备私钥不写入普通 TOML。环境变量只覆盖非持久部署参数或秘密引用。

## 5. 启动顺序

1. 获取单实例锁。
2. 解析和验证配置。
3. 打开 SQLite 并运行迁移。
4. 检查数据库可写和磁盘空间。
5. 启动本地 HTTP 服务。
6. 启动 App Server supervisor。
7. 启动 inbox/outbox worker。
8. 若已配对，根据 Server URL 启动 WS/WSS tunnel。
9. 完成状态核对后标记 Agent ready。

本地页面可以在 App Server 尚未 ready 时启动，以展示诊断。

## 6. CLI 到 Daemon 通信

优先方案：

- macOS/Linux 使用 Unix Domain Socket。
- Windows 后续使用 Named Pipe。
- 若实现复用成本过高，第一版可以使用绑定 loopback 的随机端口加本地令牌。

接口仅提供管理操作，不暴露任意命令执行。

## 7. 任务和队列

- 每个长期 worker 都使用有界 `mpsc`。
- Durable 工作来源是 SQLite，不以内存 channel 为真相。
- channel 只用于唤醒 worker；通知丢失时定时扫描仍可恢复。
- worker 使用 lease/状态字段 claim 工作，崩溃后 lease 过期可继续。
- 数据库写入集中在短事务中。
- Approval/Interrupt/当前 Turn 为 P0/P1；历史回填为 P2/P3；目录 live query 有独立并发和短超时。

## 8. 系统服务

平台适配：

- macOS：LaunchAgent。
- Linux：systemd user service。
- Windows：后续使用当前用户可管理的 Service/Startup Task。

服务配置包含：

- 自动重启。
- 合理重启间隔。
- 明确工作目录和数据目录。
- 最小环境变量。
- stdout/stderr 进入受控日志。

## 9. 优雅退出

1. readiness 变为 false。
2. 停止接受新远程命令。
3. 停止项目扫描。
4. 等待当前数据库事务。
5. 确保持久消息已落 SQLite。
6. 通知 Server draining。
7. 关闭 App Server。
8. 关闭 Tunnel 和本地服务。
9. checkpoint/关闭 SQLite。

设置总超时；超时强制退出时写 `unclean_shutdown` 标记，下一次启动执行额外核对。

## 10. 升级和兼容

- 二进制启动时检查数据库 schema version。
- 只执行向前迁移，不自动降级数据库。
- 新版本先兼容旧配置字段。
- 更新过程中保留上一二进制和配置备份以便回滚。
- 活跃 Turn 时默认延迟自动更新。

## 11. 健康状态

```rust
AgentHealth {
    process,
    database,
    local_http,
    tunnel,
    app_server,
    inbox_lag,
    outbox_lag,
    history_backfill_lag,
    history_completeness,
    transport_security,
    overall,
}
```

overall 是派生值；每层原始状态仍返回 UI。

## 12. 测试

- 重复 init/start/stop 幂等测试。
- 单实例锁测试。
- 配置原子写和损坏恢复测试。
- Supervisor crash-loop 和熔断测试。
- 进程 kill 后 SQLite 恢复测试。
- 系统服务模板 snapshot test。
- 公网断线时本地闭环 E2E。
- HTTP/WS 与 HTTPS/WSS URL 派生、风险状态和禁止降级测试。
- Agent 重启后 history outbox/checkpoint 与短期 directory_ref 恢复测试。
