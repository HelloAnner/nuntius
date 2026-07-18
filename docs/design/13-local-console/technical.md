# 本地控制台：技术设计

> 实现标记：本文描述目标设计；当前 `0.1.0` 已实现项和后续边界以 [后端实现状态](../../implementation-status.md) 为准。

## 1. 构建模式

`client/frontend` 独立产出 `local` 构建：

- 编译时 `APP_MODE=local`。
- 只启用本机 Device 视图。
- API base 使用当前 origin。
- 在 Client 项目内维护本地 ViewModel、CommandReceipt 和 Event reducer，不跨项目引用 remote 源码。
- 不打包远程多设备管理和账户管理中的无关代码，或通过 route lazy load 隔离。

## 2. 静态资源

- Bun/Vite 构建带 content hash 的静态资源。
- 构建产物嵌入 Rust 二进制或随安装包放只读资源目录。
- `index.html` no-cache；hash assets 可长期缓存。
- CSP 禁止外部脚本和任意 inline script。
- 本地页面不从 CDN 加载运行时代码。

## 3. 本地 Endpoint 发现

Agent 默认绑定随机 loopback 端口，将地址写入权限受限的 `local-endpoint.json`。`nuntius open`：

1. 验证 endpoint 文件归属和 Agent PID/nonce。
2. 请求一次性 launch token。
3. 打开 `http://127.0.0.1:<port>/open#...` 或更安全的短期交换流程。
4. 页面把一次性 token 交换为 HttpOnly 本地 Session Cookie。
5. URL 中的临时片段立即清理。

长期 token 不放 URL。

## 4. Loopback 安全

- 只绑定 `127.0.0.1` 和可选 `::1`。
- Host allow-list 只接受精确 loopback host/port。
- 检查 Origin。
- 本地 Cookie 使用 HttpOnly、SameSite=Strict；在 HTTP loopback 下 Secure 属性按浏览器能力处理。
- 修改请求要求 CSRF token。
- 拒绝 Private Network Access 的非本地来源。
- 防 DNS rebinding，不相信 Host 指向 localhost 就代表本地页面。

## 5. API 适配

`LocalApiClient` 与 `RemoteApiClient` 实现相同前端接口：

```ts
interface NuntiusApi {
  sync(): Promise<SyncSnapshot>
  listProjects(...): Promise<Page<ProjectView>>
  submitCommand(...): Promise<CommandReceipt>
  getCommand(...): Promise<CommandView>
  updateSubscription(...): Promise<void>
}
```

local command 可直接进入 Agent inbox，但仍生成 command ID、持久化并使用同一状态机，避免本地和远程行为分叉。

## 6. SSE

- 使用本地 `/api/v1/events`。
- 原生 EventSource。
- 同样使用 cursor、seq、gap 和 resync。
- 即使本地网络稳定，也必须覆盖 Agent 页面后台和进程重启场景。

## 7. 页面路由

```text
/
/projects
/projects/:projectId
/threads/:threadId
/unassigned-threads
/connection
/codex
/settings
/diagnostics
```

路由中的 ID 必须经 API 重新鉴权/校验，不能直接映射文件路径。

## 8. 本地目录选择

纯浏览器不能可靠获取任意真实目录路径。第一版方案：

- 页面点击“添加项目”请求 Agent 打开平台原生目录选择器；或
- 用户复制路径输入，由 Agent 校验；CLI 始终可用。

前端不递归扫描本地文件。

## 9. 故障处理

- Agent 重启导致页面断开：EventSource 重连，endpoint 不变则自动恢复；端口变化时显示重开提示。
- App Server 异常：页面仍正常，通过健康视图禁用执行。
- SQLite degraded：页面切只读，保留诊断入口。
- 静态资源与 Agent API 版本不匹配：强制刷新 index，不尝试继续执行写操作。

## 10. 测试

- local/remote ViewModel 契约一致测试。
- DNS rebinding/Host/Origin 测试。
- 一次性 launch token 重放测试。
- Agent 重启和 SSE 恢复 E2E。
- 公网断开下完整本地对话 E2E。
- 目录添加和危险路径 UI 测试。
- CSP 和无外部资源测试。
