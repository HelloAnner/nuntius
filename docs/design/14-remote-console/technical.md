# 远程移动控制台：技术设计

> 实现标记：本文描述目标设计；当前 `0.1.0` 已实现项和后续边界以 [后端实现状态](../../implementation-status.md) 为准。

## 1. 前端栈

- React + TypeScript。
- Bun 管理依赖和运行脚本。
- Vite 构建。
- TanStack Query 管理 HTTP Server State。
- 原生 EventSource 订阅 SSE。
- Zod/生成 Schema 做运行时校验。
- CSS Variables + 小型内部组件层，不引入沉重桌面 UI 框架。

依赖使用 lockfile 固定，并定期而非自动无审查升级。

## 2. 应用层次

```text
app-shell
├─ api-client
├─ auth-session
├─ sync-controller
├─ event-source-controller
├─ domain-cache/reducers
├─ routes
│  ├─ devices
│  ├─ projects
│  │  └─ directory-picker
│  ├─ threads
│  ├─ approvals
│  └─ settings
└─ remote-only UI components
```

组件不直接解析 SSE；Event Controller 校验并转换后更新领域缓存。

## 3. 状态管理

### Server State

- Query Cache：设备、项目、Thread 分页和详情快照。
- History Cache：来自 Server SQLite 的 Thread/Turn/Item 分页，按设备和项目分区。
- Event Reducer：按 event 更新对应 cache。
- Sync Controller：在 gap、恢复和 resync 时重取快照。

### UI State

- 当前路由和选中项。
- 草稿。
- 展开/折叠。
- 滚动锚点。
- 当前目录选择会话、breadcrumbs 和短期 `directory_ref`；页面刷新后不假设引用仍有效。

不要把完整 Query Cache 复制到另一个全局 Store。

## 4. Command Mutation

提交命令：

1. 生成并持久当前 action 的 idempotency key。
2. HTTP POST。
3. 收到 receipt 后缓存 command ID。
4. 由 SSE command status 更新 UI。
5. HTTP 超时用同 key 查询/重试。
6. 终态后清理 action key。

乐观更新只用于可安全回滚的显示字段。Turn、Approval 等副作用不提前伪造成功。

## 5. SSE Controller

状态机：

```text
idle -> connecting -> live
live -> disconnected -> reconnecting
reconnecting -> syncing -> live
任意 -> auth_required | incompatible
```

- 维护 last cursor 和 last seq by stream。
- 连接恢复后触发 lightweight sync。
- gap 只冻结受影响 stream，必要时全局 sync。
- 对重放事件幂等 reducer。
- 页面隐藏过久时主动关闭/重建由实现测试决定，不依赖连接仍活着。

## 6. 消息渲染性能

- Thread 历史 cursor 分页。
- 长列表使用窗口化，但需兼顾可访问性和动态高度。
- delta 合并在 requestAnimationFrame 或小时间窗批处理。
- Item completed 后用最终内容替换 delta buffer。
- 命令输出只保留可见尾部，完整内容按需分页读取。
- 每 Thread 缓存设条目/字节上限。

## 7. 草稿

以 `(device_id, project_id, thread_id)` 作为 key 存 session/local storage：

- 不同步 Server。
- 提交成功后清理。
- 登录退出时清除或加密敏感草稿；第一版建议清除。
- 切换 Thread 不串稿。

## 8. Service Worker

Service Worker 只在 HTTPS 或浏览器认可的 localhost secure context 注册。普通远程 HTTP 下不注册，并通过 capability 隐藏“可安装/离线壳”能力；这不影响 HTTP API、SSE、目录浏览、历史读取和对话控制。

只缓存：

- 版本化静态资源。
- 应用 shell。

不缓存：

- 登录响应。
- `/api/*` 动态敏感数据。
- SSE。

新版本资源就绪后提示刷新；活跃 Turn 时不强制 reload。

## 9. 安全

- 不把 Session Token 存 localStorage。
- Cookie 由 Server 管理。
- CSP 禁止不必要第三方脚本。
- 所有 Agent/命令文本按纯文本渲染；Markdown 使用严格 sanitizer。
- 外部链接明确提示并使用安全 rel 属性。
- 审批内容不能通过 HTML 注入改变按钮语义。
- App 启动时读取 Server `transportSecurity` capability；HTTP/WS 档位在所有业务页面持续显示不可关闭的“不安全传输”标记，登录成功不能消除该提示。
- 远程 Project 创建只提交 `directory_ref`，前端不得提供任意绝对路径输入框或把浏览过的完整目录树写入持久缓存。

## 10. 可访问性和响应式

- 语义化 button/nav/list。
- 状态使用文字和图标。
- live region 只播报重要终态，避免 delta 持续打扰读屏。
- 支持 prefers-reduced-motion。
- 断点以内容为准；手机单栏、平板可双栏。
- 自动化 axe 类检查加关键流程人工读屏测试。

## 11. 测试

- API/Event reducer 单元测试。
- command idempotency UI 测试。
- SSE 重复、gap、resync 和 auth expiry 测试。
- 远程目录浏览、directory_ref 过期和 Project 创建测试。
- 设备离线时完整历史分页查询测试。
- Playwright 移动视口 E2E。
- 锁屏/visibility/network offline 模拟。
- 长 Thread 性能和内存测试。
- XSS/Markdown sanitizer 测试。
- 多标签页 Approval 竞态 E2E。
