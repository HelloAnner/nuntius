# 测试与质量保障：技术设计

> 实现标记：本文描述目标设计；当前 `0.1.0` 已实现项和后续边界以 [后端实现状态](../../implementation-status.md) 为准。

## 1. 测试层次

```text
大量：单元/性质测试
中量：组件/契约/数据库测试
少量：跨进程 E2E
持续：故障注入/长稳/兼容性
```

不只依赖端到端测试定位所有错误，也不只测试纯函数而忽略真实进程边界。

## 2. 单元测试

- 领域状态机合法转换。
- Command/Event 序列化。
- Idempotency fingerprint。
- Cursor 编解码。
- Retry 分类和 backoff 范围。
- Project 路径规则。
- Approval CAS 规则。
- Event reducer 和 delta 聚合。
- History batch revision/content hash 和 completeness 计算。
- `directory_ref` 签发、验证、过期与 root containment。
- TransportProfile URL/Cookie/capability 派生。
- 日志脱敏。

## 3. 性质测试

使用 property-based testing 生成：

- 重复、乱序和缺口事件。
- Command 重放顺序。
- Turn/Interrupt/Approval 竞态序列。
- outbox ACK cursor 组合。
- 任意合法状态机操作序列。

不变量：

- 同 command ID 最多一个业务执行实例。
- stream applied seq 不倒退。
- terminal 状态不回到 running。
- unknown 不自动变成 queued 重试。
- 未 ACK Durable 记录不被清理。
- 旧 backfill revision 不覆盖较新实时 Item。
- 任何有效 `directory_ref` 只能属于一个 device/root/purpose，过期后必定失败。

## 4. 组件测试

### Mock App Server

提供可脚本化 JSONL 进程：

- initialize success/failure。
- 响应延迟、丢失、重复和乱序。
- 通知 flood。
- 非法 JSON/超长行。
- 进程退出和崩溃循环。

### Fake Device/Server

- WS/WSS hello/epoch/heartbeat。
- command/event/ACK 重放。
- history batch/checkpoint ACK、重复和乱序。
- directory live query 超时、取消和 stale reference。
- 慢 writer/reader。
- 认证和协议版本错误。

### Browser Harness

- HTTP timeout/重复提交。
- EventSource 断线和 Last-Event-ID。
- visibility/offline/online。
- 多标签页。

## 5. 数据库测试

- 每个测试使用独立临时目录中的真实 Server SQLite 文件运行 SQLx migration/事务测试。
- SQLite 使用真实文件，不只 `:memory:`，以覆盖 WAL 和崩溃。
- 每个迁移从上一支持版本升级。
- 事务 crash point：commit 前、commit 后、ACK 前。
- 唯一约束并发竞争。
- cleanup 保留不变量。

## 6. 契约测试

- OpenAPI 与生成 TS Client。
- Nuntius WS/WSS JSON Schema。
- SSE Event Envelope。
- Agent 与支持的 App Server Schema。
- local/remote ViewModel 一致性。
- 新 Server + previous Agent、previous Server + new Agent 的兼容窗口。
- HTTP/WS 与 HTTPS/WSS 的业务 Schema、幂等、SSE 游标和历史结果一致。
- HTTP Cookie/安全 capability 与 HTTPS 档位存在预期差异。
- 每个受支持 Codex 版本运行其自身生成的 App Server Schema 契约测试。

## 7. E2E 场景

最小完整拓扑：

```text
Headless Browser
-> HTTP direct or TLS/Proxy
-> Real Server
-> Real SQLite
-> Real Agent
-> Mock or Real App Server
-> Temp Project
```

必须自动化：

- 配对、多设备列表。
- Project 添加和同步。
- 手机受控目录浏览、Project 创建、伪造/过期引用拒绝。
- Thread/Turn/stream。
- Approval 竞争。
- Server/Agent/App Server 分别重启。
- Browser refresh/network loss。
- Archive/Resume。
- 既有多 Thread 历史回填、离线读取和上线后断点续传。
- secure 与 trusted-http 两套拓扑的相同核心旅程及不同风险 UI。

## 8. 故障注入矩阵

对每条 Durable Command 在这些点 kill：

```text
HTTP request received
Server SQLite before commit
Server SQLite after commit / before 202
WS(S) before send
WS(S) after send / before device ACK
SQLite after inbox commit / before ACK
before App Server request
after App Server write / before response
after response / before local commit
event outbox commit / before WS(S) send
WS(S) event send / before Server ACK
history item normalized / before agent outbox commit
history batch send / before Server transaction commit
history transaction commit / before history ACK
directory query returned / before project.create
```

测试断言是最终领域状态和重复执行次数，不只是进程重新上线。

## 9. 网络测试

- 延迟、抖动、丢包和带宽限制。
- TCP reset 和半开连接。
- DNS 暂时失败。
- TLS 证书错误和企业 CA。
- Proxy WS/WSS/SSE idle timeout。
- WSS 证书错误不得自动改连 WS。
- HTTP 非 loopback 未授权启动失败、授权后风险标记持续存在。
- Wi-Fi/蜂窝切换模拟。
- 重连风暴和 full jitter 分布。

工具可使用 Toxiproxy、`tc/netem` 或平台等价能力。

## 10. 性能与容量

基准场景：

- 多设备并发连接。
- 单 Turn 高频 delta。
- 多 Thread 中低频事件。
- 多浏览器慢消费者。
- outbox backlog 恢复。
- 大量既有 Thread 历史回填与实时 Turn 并行。
- 大目录单层分页与目录 live query 并发上限。

观察：

- RSS 和任务数是否稳定。
- 每连接缓冲是否有上限。
- SQLite WAL 是否可控。
- Server SQLite query/busy lock/connection pool/WAL checkpoint。
- SSE/WS(S) 延迟和 command delivery latency。
- History ingest throughput、backfill oldest age、Server SQLite 历史分页延迟。
- P2 backfill 饱和时 P0/P1 command/approval latency。

## 11. 长稳

至少 24 小时：

- 周期创建 Turn。
- 周期断开/恢复 SSE 和 WS/WSS。
- 周期重启 Server、Agent、Mock App Server。
- 周期制造慢消费者。
- 检查无内存、文件句柄、任务、WAL/outbox 无界增长。
- 最终快照与事件聚合一致。
- 最终 Server 历史与 Agent 权威历史的 ID/revision/content hash 一致。

稳定发布前延长到更长周期由实际资源决定。

## 12. 安全测试

- Authn/Authz/IDOR。
- CSRF、CORS、CSP、Host、DNS rebinding。
- Pairing brute-force 和重放。
- Device Token/epoch 重放。
- JSON/Markdown fuzz/XSS。
- Path traversal/symlink race。
- `directory_ref` 伪造、跨设备重放、TOCTOU。
- HTTP/WS 明文风险 capability、Cookie 属性和安全模式不降级。
- History DTO/备份/日志的数据边界与 secret scanner。
- Secret scanner 扫日志、诊断和构建产物。
- 依赖漏洞和 SBOM。

## 13. CI 门禁

每个 PR：

- format/lint/typecheck。
- 单元/性质测试。
- Schema/contract tests。
- Server/Client SQLite integration。
- 前端 component tests。
- 关键 E2E smoke。

Release：

- 全 E2E。
- 故障注入关键矩阵。
- 升级/回滚。
- 多浏览器/平台矩阵。
- 依赖和安全扫描。
- 长稳报告。
- 备份恢复验证。
