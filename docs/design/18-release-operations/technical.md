# 安装、发布与运维：技术设计

> 实现标记：本文描述目标设计；当前 `0.1.0` 已实现项和后续边界以 [后端实现状态](../../implementation-status.md) 为准。

## 1. 发布产物

Agent：

```text
nuntius-<version>-<os>-<arch>.archive
checksums.txt
signature
release-manifest.json
```

Manifest 包含：

- 版本。
- Git commit。
- 构建时间。
- 支持 OS/arch。
- Nuntius protocol min/max。
- 支持 Codex 版本范围/Schema 集。
- 数据库 schema version。

## 2. 可复现和供应链

- Cargo.lock 和 Bun lockfile 固定。
- CI 在干净环境构建。
- 产物生成 SBOM。
- 签名密钥与 CI 普通凭证分离。
- 发布前运行依赖漏洞和许可证检查。
- 前端资源 hash 写入 manifest 并嵌入二进制。

## 3. Agent 更新流程

1. 查询 signed manifest。
2. 选择平台产物。
3. 下载到专用临时目录。
4. 校验大小、checksum、签名和版本策略。
5. 检查 active Turn、inbox/outbox 和 schema migration。
6. Agent 进入 draining。
7. 安装新二进制，保留 previous。
8. 启动新版本并执行 self-check。
9. self-check 成功后删除旧版本或保留一个窗口。
10. 失败则回滚并记录诊断。

滚动通道采用 latest-wins desired state，而不是逐 commit FIFO 部署：

- CI 只允许 `main` 当前 HEAD 更新通道指针，过时构建直接跳过发布。
- Server/Client 资产按 commit 和 CI 发布序号使用不可变文件名；manifest 最后发布。
- `releaseSequence` 单调递增，已运行新版更新器的设备拒绝更小或相等序号的不同 commit。
- Server 必须先达到目标 commit，Client 才能安装同一 commit；中继完成后主动触发 Client 检查，周期轮询作为兜底。

下载文件不能直接执行；所有路径必须明确且不使用宽泛临时目录清理。

## 4. 系统服务

- macOS LaunchAgent plist。
- Linux systemd user unit。
- restart on failure，带启动限速。
- graceful stop timeout 与 Agent 自身一致。
- 运行用户即安装用户，不使用 root。
- 环境最小化；Client 固定使用运行用户的 `~/.nuntius/`，不再引入可漂移的 `NUNTIUS_HOME`。

## 5. Server 部署拓扑

推荐安全拓扑：

```text
Internet :443
  -> Caddy/Load Balancer
  -> nuntius-server (loopback/private)
  -> <data-dir>/nuntius-server.db
```

无 TLS 兼容拓扑：

```text
Trusted network/VPN/SSH tunnel :<configured-http-port>
  -> nuntius-server (allow_insecure_http=true)
  -> <data-dir>/nuntius-server.db
```

- SSE 路径禁用缓冲。
- WS/WSS 路径支持 Upgrade。
- healthz/readyz 配置。
- SQLite 是进程内文件，不存在数据库公网端口；`<data-dir>` 只允许 Server 运行账号读取。
- Server 的 `config.toml`、数据库、secrets、logs、run 和 backups 均收敛在同一个显式 `--data-dir`，子目录按用途隔离权限。
- `public_base_url` 的 scheme 决定浏览器 HTTP(S) 与设备 WS(S) URL；不允许 WSS 失败后自动回退 WS。
- HTTP 非 loopback 且未显式授权时启动失败；启用时日志、`/status` 和页面 capability 均显示 insecure。
- PWA/Service Worker 等 secure-context 能力只在 HTTPS 或浏览器认可的 localhost 上启用。

## 6. Server 优雅发布

1. 设置 draining flag。
2. `/readyz` 返回非 ready。
3. HTTP 命令入口停止接受新副作用请求。
4. SSE 发送维护 notice。
5. WS(S) 发送 draining control。
6. 等待短期事务和 outbox flush。
7. 关闭连接和 DB pool。
8. 启动新实例。
9. readiness 通过后接流量。

设备重连使用 jitter，Server 不要求保持旧连接状态。

## 7. 数据库迁移发布

采用 expand/contract：

- Release A：新增 nullable/有默认的新字段，应用双读/双写。
- 后台迁移历史数据。
- Release B：切换读取新字段。
- 确认旧版本退出兼容窗口后才 contract 删除旧字段。

迁移由单活 Server 启动阶段执行；部署层先确保旧进程完全退出，并使用数据目录进程锁防止两个 Server 同时迁移或写同一 SQLite。

## 8. 兼容矩阵

Server 维护：

```text
protocol_current
protocol_previous
minimum_agent_version
recommended_agent_version
supported_app_server_schemas
```

握手不兼容时：

- 不进入 online。
- 允许有限诊断 endpoint。
- 返回结构化升级信息。
- 不能尝试按未知协议继续写操作。

## 9. 备份与灾备

- 第一版停服后运行 `nuntius-server --data-dir <dir> backup`，通过 SQLite `VACUUM INTO` 生成一致性备份；数据目录锁会阻止与在线 Server 并发执行。备份落入 `<data-dir>/backups` 后再复制到异地。
- 备份包含规范化完整历史及 history checkpoint，并按敏感会话数据加密和限制权限。
- 配置/签名密钥有独立安全备份。
- 每月或每个重要发布前恢复演练。
- 记录 RPO/RTO 实测。
- Server 丢失时 Agent 本地 Codex 数据仍在；重建 Server 后需要重新建立账户/设备信任或按恢复数据继续，并通过 revision/content hash 幂等回填缺失历史。

## 10. 测试和发布门禁

- 所有平台安装 smoke test。
- 升级/回滚测试。
- schema migration 前后兼容测试。
- Server draining E2E。
- WS/WSS 与 SSE 在发布中的恢复测试。
- HTTPS/WSS、HTTP/WS 两套部署 smoke test，含 Cookie/Header/capability 和禁止静默降级。
- 大历史迁移期间实时控制优先级、checkpoint 恢复和回填去重测试。
- 发布签名验证负面测试。
- 备份恢复验证通过才能标记稳定发布。
