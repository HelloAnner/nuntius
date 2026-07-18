# 远程目录浏览与项目创建：技术设计

> 实现标记：本文描述目标设计；当前 `0.1.0` 已实现项和后续边界以 [后端实现状态](../../implementation-status.md) 为准。

## 1. 组件

### Agent

- `BrowsableRootRegistry`：允许根和平台排除策略。
- `DirectoryQueryService`：列根、列子目录和生成面包屑。
- `DirectoryPolicy`：权限、隐藏目录、符号链接和敏感排除。
- `DirectoryRefService`：签发、持久化、验证和清理短期不透明引用。
- `RemoteProjectCreateService`：引用解析、重新校验和 Project 创建。

### Server

- `DeviceQueryRouter`：把短期只读查询路由到 active Device。
- `DirectoryApi`：授权、限流和响应透传。
- `ProjectCommandService`：持久化 `project.create`。

Server 不拥有路径解析和目录授权规则。

## 2. Live Query 与 Durable Command 分离

目录导航是短期只读查询：

- 不写 durable command。
- 不在设备离线时排队。
- 有短超时和 correlation ID。
- 连接断开即失败，用户重新请求。

Project 创建产生副作用：

- 使用标准 Idempotency-Key。
- Server 写 Server SQLite durable command。
- Agent 写 SQLite inbox 后 ACK。
- 由 Project Service 创建并产生终态事件。

## 3. API

```text
GET  /api/v1/devices/{device_id}/directories/roots
GET  /api/v1/devices/{device_id}/directories?parentRef=...&cursor=...
POST /api/v1/devices/{device_id}/projects
```

目录响应：

```json
{
  "deviceId": "dev_...",
  "parent": {
    "name": "projects",
    "breadcrumb": ["Home", "projects"]
  },
  "entries": [
    {
      "name": "nuntius",
      "directoryRef": "dirref_...",
      "hasChildren": true,
      "gitKind": "repository",
      "projectId": null,
      "selectable": true,
      "symlink": false
    }
  ],
  "nextCursor": null,
  "expiresAt": "..."
}
```

不返回供客户端自行拼接的 canonical path。Breadcrumb 是展示数据，不是授权数据。

## 4. 允许根配置

Agent SQLite/config：

```text
browsable_roots
  id
  display_name
  input_path
  canonical_path
  enabled
  allow_hidden
  allow_symlink_within_root
  created_at
```

- Home root 在初始化时创建，可本地禁用。
- 额外 root 只能由本地 CLI/Console 添加。
- Root canonical path 变化时自动禁用并要求重新确认。
- Server 只接收 root ID 和 display name，不接收完整策略。
- `root.selectable` 与 `root.browsable` 分开；Home 等导航根可浏览但 Project Validator 可禁止直接选择。

## 5. Directory Ref

为了让 `directory_ref` 对 Browser 和 Server 真正不透明，外部 Token 不直接编码 canonical path。Agent SQLite 保存短期记录：

```text
directory_refs
  handle_hash primary key
  device_id
  root_id
  canonical_path
  allowed_actions
  issued_at
  expires_at
  nonce
```

外部表示只包含高熵随机 handle、expiry hint 和 HMAC，例如 `dirref_v1.<handle>.<exp>.<mac>`。HMAC 绑定版本、handle、Device、expiry 和 allowed actions；签名密钥由 Agent 本地生成并保存在安全存储。Server 既看不到 canonical path，也不保存引用映射，只负责原样路由。

要求：

- 默认有效期 5 分钟，可配置有限范围。
- 与当前 Device 绑定。
- 目录条目的 ref 可以允许 `list_children`、`create_project` 中一项或两项；Agent 按实际策略签发，客户端不能扩大 actions。
- 过期后要求重新浏览。
- 短期记录写 SQLite，使已经持久化的 `project.create` 能跨 Agent 重启继续；过期记录批量清理。
- 密钥轮换后旧 ref 失效；SQLite 中只存 handle hash，不存外部 Token 原文。

## 6. 列目录算法

1. 验证 Device 状态和用户归属。
2. Agent 验证 parent ref HMAC、查找 handle hash 并检查 `list_children` action。
3. 重新 canonicalize parent。
4. 验证仍位于 root 内且未命中排除策略。
5. 使用受控目录迭代器，只读取 directory entry metadata。
6. 过滤普通文件、敏感目录和不可读项。
7. 稳定排序：目录显示名 + 平台规范化 key。
8. 按 page size 截断并生成 cursor。
9. 对每个返回目录签发短期 ref。

目录迭代有时间、项数和内存上限。超大目录分页，不能一次传完整树。

## 7. Cursor

Cursor 绑定：

- device ID。
- parent directory ref/hash。
- 排序规则版本。
- last normalized name/inode hint。
- expiry。

目录在分页间变化可能导致少量重复或遗漏；UI 去重，最终 Project 创建重新验证。第一版不为目录列表创建文件系统快照。

## 8. 符号链接和 TOCTOU

- `symlink_metadata` 先识别链接。
- 若策略允许，解析目标并确认仍在同一 allowed root。
- Project create 时再次解析 canonical path。
- 打开 Project/启动 Turn 前 Project 模块再次校验路径。
- 无法完全消除目录选择到使用间的变化，因此每个副作用阶段都重新验证，而不是依赖一次检查。

## 9. Project Create

命令 payload：

```json
{
  "deviceId": "dev_...",
  "directoryRef": "dirref_...",
  "displayName": "nuntius",
  "defaults": {}
}
```

Agent 执行：

1. inbox commit 和幂等检查。
2. 验证 ref HMAC、SQLite handle、`create_project` action、device 和 expiry。
3. canonicalize 并检查 allowed root/排除策略。
4. 检查目录可读和所需写权限。
5. 检查 canonical path 唯一性。
6. Project SQLite 事务创建。
7. 产生 project.created 和全局索引同步记录。

HTTP 超时或 WS/WSS 重放不会重复创建 Project。

## 10. 安全

- Server 不能把路径字符串当成 directory ref。
- Browser 不能通过 `..`、绝对路径或编码绕过 ref。
- 严格限制 query 速率，避免远程枚举磁盘。
- 响应不包含文件内容、owner、精确权限位或不必要系统元数据。
- 普通日志只记录 root ID、结果数和耗时，不记录目录名/路径。
- HTTP 模式明确暴露路径元数据风险。

## 11. 故障与恢复

- Device offline：live query 立即失败，不排队。
- Query timeout：允许用户重试，无副作用。
- Ref expired：返回 `directory_ref_expired`，UI 回到父级刷新。
- Directory moved/deleted：返回 `directory_changed`。
- Agent restart：普通浏览重新请求；已提交 project.create 可用 SQLite 中未过期 ref、inbox 和幂等记录恢复。
- Project 创建成功但响应丢失：Server 从 command 终态和 project.created 收敛。

## 12. 测试

- Path traversal、编码绕过和绝对路径注入。
- HMAC 篡改、过期、跨 Device、错误 action，以及外部 Token 不泄露 canonical path。
- directory_refs 在 Agent 重启后的短期恢复和过期清理测试。
- Symlink 跨根和选择后替换竞态。
- 敏感目录排除策略。
- 超大目录分页、目录变化和超时。
- Device offline 查询不排队。
- 重复 project.create 幂等。
- 断言响应不包含普通文件和文件正文。
- HTTP 与 HTTPS 模式的功能等价和风险标记测试。
