# 项目管理：技术设计

> 实现标记：本文描述目标设计；当前 `0.1.0` 已实现项和后续边界以 [后端实现状态](../../implementation-status.md) 为准。

## 1. 组件

- `ProjectService`：创建、修改、暂停、恢复和移除。
- `PathValidator`：规范化、权限和危险路径检查。
- `GitInspector`：有界、可取消的仓库摘要扫描。
- `ProjectRepository`：Agent SQLite 数据访问。
- `ProjectSyncPublisher`：向 Server 发布摘要版本。
- `ProjectPolicy`：执行前可用性判断。

## 2. Agent 数据模型

### projects

```text
id
device_id
kind: workspace | system_unassigned
display_name
input_path nullable
canonical_path nullable unique where kind = workspace and removed_at is null
status
paused_at nullable
removed_at nullable
created_at
updated_at
last_validated_at
last_used_at nullable
summary_version integer
defaults_json
```

每台 Device 对 `kind=system_unassigned` 建唯一约束。该行的 path、Git 和 defaults 为空，使用稳定 ID，并在 Project Service 中禁止普通 create/update/remove。

### project_git_snapshots

```text
project_id primary key
repo_root_hash
repo_name
branch nullable
head_short nullable
is_dirty nullable
remote_host nullable
captured_at
scan_error_code nullable
```

不保存 remote URL 中的用户名、Token 和完整私有路径。

## 3. 路径处理

添加流程：

1. 解析用户路径并转换绝对路径。
2. `symlink_metadata` 检查输入本身。
3. `canonicalize` 获得真实路径。
4. 检查目录、权限和危险根目录策略。
5. 用平台合适的大小写规则做唯一性判断。
6. 在 SQLite 事务内插入 Project。

危险路径最低限度禁止：

- 文件系统根目录。
- 用户主目录整体。
- Nuntius 数据目录。
- Codex 状态根目录整体。

用户确有需要时未来提供显式高级 override；第一版不提供远程 override。

## 4. 本地 API

```text
GET    /api/v1/projects
POST   /api/v1/projects
GET    /api/v1/projects/{project_id}
PATCH  /api/v1/projects/{project_id}
POST   /api/v1/projects/{project_id}/validate
POST   /api/v1/projects/{project_id}/pause
POST   /api/v1/projects/{project_id}/resume
DELETE /api/v1/projects/{project_id}
```

远程 POST create 只接受 Agent 签发的短期 `directory_ref`，不接受路径字符串；Agent 解析引用、重新 canonicalize 和校验 allowed root 后调用同一 `ProjectService`。远程 PATCH 只允许 display name 和安全默认项，通过 Device Command 执行。

## 5. Server 摘要模型

```text
device_id
project_id
summary_version
display_name
path_hint nullable
repo_name nullable
branch nullable
is_dirty nullable
status
thread_count
last_activity_at
captured_at
removed
```

Server 以 `(device_id, project_id, summary_version)` 做单调更新，忽略旧版本摘要。

## 6. Git 扫描

- 优先使用 `git` CLI 或成熟 Rust 库，选择在实现 spike 后确定。
- 每次扫描有超时和输出上限。
- 不递归读取项目文件内容。
- 触发时机：添加、打开详情、Turn 完成后低频刷新、用户手动刷新。
- 同一 Project 同时只允许一个扫描。
- 扫描失败不改变 Project 核心可用性。

## 7. 执行前校验

每次 `thread.start` 或 `turn.start` 前：

1. Project 为 `kind=workspace` 且未 removed/paused。
2. canonical path 仍存在且为目录。
3. 目录可读；需要 workspace-write 时可写。
4. 不与记录 canonical path 发生意外变化。
5. App Server ready。

校验失败更新 Project status 并产生事件，不能把命令送入 App Server。

## 8. Thread 自动关联

Reconciler 获取 App Server Thread cwd 后：

- cwd 等于 Project canonical path：直接关联。
- cwd 在某 Project 根目录之下：只有唯一匹配时关联最深根目录。
- 多个候选或 cwd 缺失：关联设备保留的 system unassigned project，不保存/上传可识别 cwd。
- 自动关联写入审计来源 `auto_by_cwd`。
- 用户本地手动调整来源为 `manual`，后续扫描不覆盖。
- system unassigned project 使用稳定 ID，可进入 Server Project 索引，但 `canonical_path` 永远为空且执行前校验永远拒绝。

## 9. 一致性与删除

- 移除使用 tombstone，直到 Server 确认收到 removed summary。
- tombstone 保留最低同步期后可清理。
- 有 active Turn 时事务返回 conflict。
- 项目更新与 summary_version 增加同一事务。
- Summary Publisher 从 SQLite outbox 投递，不依赖内存通知。

## 10. 安全与测试

- 路径遍历、符号链接变化和大小写冲突测试。
- 危险根目录拒绝测试。
- 移动硬盘消失/恢复测试。
- Git 命令超时和超大输出测试。
- Summary 乱序版本测试。
- Project remove 与 Turn start 并发测试。
- system unassigned project 不可执行、不可移除、无路径泄露及手动重归类测试。
- 断言删除操作不调用文件删除 API。
