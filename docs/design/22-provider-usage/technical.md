# 模型套餐额度：技术设计

## 全链路

```text
Client hourly scheduler / provider_usage_refresh command
  -> Rust collector reads local OpenAI/Kimi credentials
  -> upstream usage APIs
  -> provider.usage.reported durable event (one per Provider)
  -> Client event_outbox
  -> device WebSocket tunnel
  -> Server transaction: provider_usage_reports + event_journal
  -> EventAck
  -> user SSE invalidates providerUsage query
  -> Settings renders latest projection
```

采集器位于 `client/src/provider_usage.rs`，是原 `~/bin/scripts/usage/usage.py` 网络采集逻辑的 Rust 实现，不启动 Python 子进程。Client 首次启动会立即采集，此后按 UTC 小时运行；设备 ID 生成 0–299 秒稳定抖动，避免整点同时访问上游。

## Provider 采集

### OpenAI

1. 从 `$CODEX_HOME/auth.json`（默认 `~/.codex/auth.json`）读取 OAuth token；兼容 API Key 格式。
2. 从 `config.toml` 读取可选 `chatgpt_base_url`。
3. 请求 `/wham/usage`；自定义 Codex 服务请求 `/api/codex/usage`。
4. 映射 `primary_window` 为 5 小时窗口、`secondary_window` 为 7 天窗口，并保存上游实际 `limit_window_seconds`。
5. 单独请求 `/wham/rate-limit-reset-credits`，保存可用重置卡数量和最早的未来到期时间。
6. 只解析 ID Token 中经过白名单允许的账户/会员字段；Access Token 与 ID Token 本身不会上传。

### Kimi Code

按以下顺序回退：

1. `~/.kimi-code/credentials/kimi-code.json` 或 `~/.kimi/...` 的 CLI 凭据；
2. `KIMI_CODE_API_KEY` 或 CodexBar Provider API Key；
3. `kimi-auth` Web 凭据和 `FEATURE_CODING` usage API。

周额度映射为 7 天窗口，limits 中的短周期映射为 5 小时窗口。Kimi 当前接口没有提供可靠会员截止日时，`subscriptionExpiresAt` 保持 `null`，JWT/凭据截止日只写入 `credentialExpiresAt`。

## Wire Schema

`ProviderUsageReport` 包含：

- `schemaVersion`、全局唯一 `reportId`、`provider`、`sampledAt`、`source`、`status`；
- 白名单账户字段和 `entitlementPlan`；
- `windows.fiveHour`、`windows.sevenDay`，每个窗口包含时长、使用百分比、绝对用量及重置时间；
- OpenAI credit balance、reset credit 数量及到期时间；
- 稳定的 `warningCode` / `errorCode`，不包含上游响应正文。

Server 在落库前校验枚举、长度、RFC 3339 时间、有限数值及百分比范围。报告必须是 device-scoped event。

## 存储与查询

迁移 `0012_provider_usage.sql` 创建专用的 append-only `provider_usage_reports`。表同时保存：

- `sampled_at`：Client 实际采样时间；
- `received_at`：Server 接收时间；
- `created_at`：数据库创建时间；
- 常用账户、窗口、credit 字段的类型化列；
- 完整规范化报告 `payload_json`，用于版本兼容和最新视图还原。

报告插入和 `event_journal` 插入位于同一 `BEGIN IMMEDIATE` 事务。`event_id` 与 `report_id` 唯一约束提供重放幂等性。维护任务不清理这张表；它是明确的业务历史表，不属于通用原始事件保留。

`GET /api/v1/provider-usage` 使用 `(user_id, device_id, provider)` 分组的相关子查询，只返回按 `received_at, report_id` 排序的最新一行。没有记录时自然返回空数组。

## 手动刷新

`POST /api/v1/provider-usage` 要求 Web Session、CSRF 和 `Idempotency-Key`。Server 为每台未撤销设备派生稳定的子幂等键并入队 `provider_usage_refresh`。命令有效期 24 小时，允许离线设备重连后执行。Client 执行时重新采集两个 Provider，并通过相同 durable event/outbox 路径汇报。

## 前端

额度视图只存在于 Server 设置页，不增加独立路由。TanStack Query 获取最新投影；`provider.usage.reported` SSE 到达后使查询失效。UI 按设备分组，Provider 卡片展示账户、状态、套餐、窗口进度、重置卡/余额和到期信息；小于 560px 时卡片改为单列。

## 测试边界

- 采集器单元测试覆盖 OpenAI 窗口、Kimi 绝对/百分比额度以及“会员到期”和“凭据到期”不混用。
- Store 测试覆盖空查询、事件幂等、两次快照追加为两行，以及 latest projection 返回第二条。
- 协议测试固定 `provider_usage_refresh` wire tag。
