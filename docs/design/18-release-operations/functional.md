# 安装、发布与运维：功能设计

> 实现标记：本文描述目标设计；当前 `0.1.0` 已实现项和后续边界以 [后端实现状态](../../implementation-status.md) 为准。

## 1. 目标

让用户可以可靠安装、启动、升级、回滚和卸载 Agent；让 Server 可以在尽量短的中断中发布，并保证新旧版本有明确兼容窗口。

## 2. Agent 安装

安装流程：

1. 下载对应 OS/架构的签名发布包。
2. 校验 checksum/签名。
3. 安装单二进制和静态资源。
4. 运行 `nuntius init`。
5. 可选注册用户级后台服务。
6. 配对 Server。
7. 打开本地控制台验证。

安装不会自动安装或登录 Codex；只检测并给出指引。

## 3. 更新

- 本地和远程页面提示 Agent/Server/Codex 兼容状态。
- Client 只接受已配对 Server 下发的自身目标版本，不参与 Server 构建或部署。
- Server 持久保存 desired Client release；离线设备重连后仍能收到。
- Client 下载失败会按配置退避重试。
- Client 校验完成后立即激活更新，不能因 active/recovering 会话长期阻塞。
- Client 更新期间 Agent Host 保持 provider 任务运行；新 Client 恢复全部运行中会话、
  待审批状态和遗漏事件后再恢复 Server Tunnel。
- 更新失败自动回滚上一二进制。
- 数据库迁移后无法安全回滚时必须在更新前明确提示。

## 4. 卸载

卸载分为：

- 仅移除程序和系统服务，保留数据。
- 同时移除 Nuntius 本地数据，需要二次确认。

无论哪种都不删除项目目录和 Codex 数据。建议先在 Server 撤销设备。

## 5. Server 部署

最小部署包含：

- Rust Server。
- Server SQLite。
- 推荐的 TLS 入口代理；无 TLS 时可显式使用 Server HTTP/WS 兼容入口。
- 持久备份位置。
- systemd 或容器运行方式。

提供明确配置清单、健康检查和备份恢复步骤。HTTP/WS 部署文档必须同时说明：功能完整，但登录凭证、完整会话和目录元数据不受传输加密保护，建议只放在 VPN/SSH 隧道或可信网络内。

## 6. Server 发布体验

- 发布前检查数据库备份、迁移和兼容范围。
- Server 进入 draining，不再接受新设备连接/命令。
- 设备自动重连。
- 页面 SSE 自动恢复并重新同步。
- 设备从 history checkpoint 继续回填，不因发布重复写历史。
- 发布失败回滚应用；数据库迁移遵循兼容迁移策略。

## 7. 版本兼容提示

状态：

- compatible：完整功能。
- upgrade_recommended：可用但建议更新。
- read_only_compatible：只读诊断/摘要可用。
- incompatible：写操作禁用。

提示明确需要更新 Agent、Server 还是 Codex。

## 8. 第一版不做

- 跨平台 Client 灰度通道。
- 多区域多活。
- Kubernetes Operator。
- 在线数据库降级迁移。
- 远程执行任意安装脚本。

## 9. 验收标准

1. 安装和卸载不影响项目/Codex 数据。
2. 更新包有签名/checksum 校验。
3. 活跃 Turn 时不会静默重启 Agent。
4. Server 重启后浏览器和 Agent 自动恢复。
5. 迁移失败时 Server 不进入写 ready。
6. 新旧 Agent 有明确兼容窗口。
7. 备份流程经过实际恢复验证。
8. 同一版本可运行 HTTPS/WSS 与显式 HTTP/WS 两个档位，且不会从安全档位静默降级。
9. Server 恢复后完整历史能够从备份和在线设备 checkpoint 收敛。
