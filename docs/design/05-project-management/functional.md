# 项目管理：功能设计

> 实现标记：本文描述目标设计；当前 `0.1.0` 已实现项和后续边界以 [后端实现状态](../../implementation-status.md) 为准。

## 1. 目标与定义

Project 是 Nuntius 的一等业务对象，表示“某台具体设备上的一个本地工作目录”。它为 Codex Thread 提供工作目录、显示名称、仓库摘要和默认运行配置。

```text
Project = Device + Local Root Path + Metadata + Defaults + Threads
```

同一个 Git 仓库在两台设备上的两个 checkout 是两个 Project。第一版不合并为跨设备逻辑项目。

唯一例外是每台设备自动创建的系统“未归类”Project：它没有本地路径，只用于给无法安全匹配 cwd 的既有 Thread 提供稳定归属，不可用于执行。

## 2. 功能范围

- 在本地 CLI 或本地控制台添加项目。
- 校验目录是否存在、是否可访问。
- 设置显示名称。
- 展示 Git 仓库、分支和工作区状态摘要。
- 设置项目级 Codex 默认参数。
- 查看项目关联 Thread。
- 暂停、恢复或移除项目索引。
- 向远程 Server 同步不敏感摘要。

## 3. 添加项目

### 3.1 入口

第一版提供两类添加入口：

- `nuntius projects add <path>`。
- 本地控制台目录输入或原生目录选择器。
- 手机选定在线 Device 后，通过受控目录选择器选择 Agent 允许暴露的目录。

远程页面不能直接输入任意绝对路径。目录选择器按需向在线 Agent 查询允许根和子目录，并使用 Agent 签发的短期 `directory_ref` 创建项目；详细安全边界见 [远程目录浏览](../21-directory-browser/functional.md)。

### 3.2 校验

添加时检查：

- 路径存在且为目录。
- 当前用户有读取权限。
- 路径规范化后未与已有 Project 重复。
- 路径不是 Nuntius 数据目录或明显危险的系统根目录。
- 是否是 Git 仓库；不是 Git 仓库也允许添加。
- Codex 是否能以该目录作为 cwd，不能则给出具体原因。

对符号链接：显示用户输入路径和规范化路径，最终以规范化路径去重；不得悄悄跨到用户未预期的敏感目录。

### 3.3 默认名称

- Git 仓库优先使用仓库目录名。
- 普通目录使用最后一级目录名。
- 重名允许，但 UI 同时展示设备和短路径。

## 4. 项目信息

本地完整信息：

- `project_id`。
- 显示名称。
- 原始路径和规范化路径。
- 目录可用状态。
- Git root、remote 名称摘要、当前分支、dirty 状态。
- 默认模型、sandbox、approval 等可选配置。
- 创建、最近扫描和最近使用时间。

远程摘要默认只包含：

- Project ID、显示名称。
- 可选的相对/脱敏路径提示。
- Git 仓库名、分支、dirty 状态。
- 可用状态、最近活动时间和 Thread 数量。

完整绝对路径默认不上传；用户可在高级设置中选择显示。

## 5. 项目状态

```text
active
missing          路径不存在
unreadable       权限不足
incompatible     当前平台/App Server 无法使用
paused           用户暂停远程控制
removed          已从 Nuntius 索引移除
system_unassigned 系统未归类，仅历史只读
```

- `missing/unreadable` 项目保留 Thread 映射，但不能新建 Turn。
- `paused` 项目可以查看历史摘要，不能远程执行。
- `system_unassigned` 没有 cwd，不能创建 Thread/Turn、不能删除，只能把其中 Thread 在本地重归类。
- 路径恢复后可以重新验证回到 active。

## 6. 项目默认配置

第一版支持少量、明确的默认项：

- 默认模型：可选，未设置则跟随 Codex 配置。
- 默认 sandbox：可选。
- 默认 approval policy：可选。
- 默认 personality：若 App Server 稳定支持则提供。

实际 Turn 可以覆盖允许覆盖的项目默认值。UI 必须显示最终生效值来自项目还是全局默认。

不在项目中复制完整 Codex `config.toml`，避免形成第二套配置系统。

## 7. 移除与暂停

### 暂停

- 不删除映射。
- 阻止新的远程副作用命令。
- 本地用户可以恢复。

### 移除

- 只删除或 tombstone Nuntius 项目索引。
- 不删除目录、Git 仓库、Codex Thread 或任何文件。
- 有活动 Turn 时禁止直接移除，需先中断或等待完成。
- 移除后 Server 摘要同步为 removed。

## 8. 项目与 Thread 关联

- 新建 Thread 必须选 Project。
- 已有 Codex Thread 可以按 cwd 自动关联。
- 每台设备自动拥有一个不对应 cwd 的系统“未归类”Project；无法自动关联的 Thread 先归入其中，保证 Server 仍有完整 Device -> Project -> Thread 归属。
- 未归类 Thread 的规范化历史会同步到 Server，但不上传 cwd；远程只读，用户在本地选择真实 Project 后才能继续执行。
- 一个 Thread 第一版只属于一个 Project。
- Project 路径变更不自动迁移 Thread 的历史 cwd，只影响未来 Turn。

## 9. 异常场景

- 移动硬盘未挂载：Project 显示 missing，不删除。
- Git 扫描超时：Project 仍可使用，只隐藏 Git 摘要。
- 路径权限变化：下一次执行前重新校验。
- Project 在远程缓存中存在但 Agent 已移除：同步后显示 removed。
- 项目名冲突：不阻止创建。

## 10. 验收标准

1. 用户能在本机添加普通目录或 Git 仓库。
2. 重复规范化路径不能创建两个 active Project。
3. 远程页面只能浏览 Agent 允许根下的目录，不能探测任意路径或读取文件正文。
4. missing/unreadable/paused Project 不能新建 Turn。
5. 移除 Project 不删除任何本地文件或 Codex Thread。
6. 已有 Thread 能按 cwd 关联或进入未归类列表。
7. Server 只获得经过数据最小化的项目摘要。
8. 未归类 Project 不能创建新 Thread/Turn、不能被移除，也不会暴露原始 cwd。
