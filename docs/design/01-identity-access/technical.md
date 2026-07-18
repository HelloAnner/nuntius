# 用户身份与访问控制：技术设计

> 实现标记：本文描述目标设计；当前 `0.1.0` 已实现项和后续边界以 [后端实现状态](../../implementation-status.md) 为准。

## 1. 组件

- `AuthService`：密码登录、Web Session、退出和撤销。
- `BootstrapService`：首次 Owner 初始化。
- `PairingService`：配对码创建、消费和取消。
- `DeviceAuthService`：设备 challenge、签名验证和短期 Token。
- `AuthMiddleware`：HTTP/SSE 身份解析。
- `DeviceHandshakeAuth`：WS/WSS Upgrade 和 hello 二次校验。

## 2. 数据模型

### users

```text
id
login_name unique
password_hash
status: active | locked
created_at
password_changed_at
```

### web_sessions

```text
id
user_id
token_hash unique
csrf_secret_hash
created_at
last_seen_at
expires_at
revoked_at nullable
user_agent_summary
ip_prefix nullable
```

数据库只存随机 Session Token 的哈希，不存原文。

### pairing_codes

```text
id
user_id
code_hash unique
created_at
expires_at
consumed_at nullable
cancelled_at nullable
created_by_session_id
```

### device_keys

```text
device_id
user_id
public_key
key_version
status: active | revoked
created_at
revoked_at nullable
```

## 3. 密码与 Session

- 密码哈希使用 Argon2id，参数由配置给出并在启动时验证下限。
- 登录成功生成至少 256 bit 随机 Session Token。
- HTTPS 模式 Cookie：`Secure; HttpOnly; SameSite=Strict; Path=/`。
- HTTP 模式 Cookie：`HttpOnly; SameSite=Strict; Path=/`，同时标记 transport insecure；它不具备网络窃听和中间人防护。
- Session 默认有绝对过期时间；可选短期滑动续期，但不得无限续期。
- 修改密码后撤销其他 Web Session。
- CSRF 使用 session-bound token，并校验 Origin。
- 响应 capability 明确给出 `transport_security=secure|insecure`；HTTP 登录与配对成功不能把它改成 secure。

## 4. API

```text
POST   /api/v1/auth/bootstrap
POST   /api/v1/auth/login
POST   /api/v1/auth/logout
GET    /api/v1/auth/sessions
DELETE /api/v1/auth/sessions/{session_id}
POST   /api/v1/pairing-codes
DELETE /api/v1/pairing-codes/{pairing_id}
POST   /api/v1/device-auth/pair
POST   /api/v1/device-auth/challenge
POST   /api/v1/device-auth/token
```

配对 API 与普通网页登录 API 分开限流。

## 5. 配对事务

消费配对码和创建 Device、Device Key 必须在同一 Server SQLite 事务中：

1. `SELECT ... FOR UPDATE` 锁定配对记录。
2. 校验未过期、未消费、未取消。
3. 创建 Device。
4. 写入 Device Public Key。
5. 标记 pairing code consumed。
6. 提交。

唯一约束防止重复公钥或重复消费。

## 6. 设备认证流程

```text
Agent -> Server: device_id, nonce request
Server -> Agent: challenge, expires_at
Agent -> Server: signature(challenge + device_id + protocol_version)
Server: verify public key and device status
Server -> Agent: short-lived access token
Agent -> WS(S): Authorization: Bearer <token>
```

短期 Token 包含：

- `sub = device_id`
- `user_id`
- `key_version`
- `iat/exp`
- `aud = nuntius-device-tunnel`

WS/WSS hello 中的 `device_id` 必须与 Token 一致。

## 7. 撤销传播

撤销设备的 Server SQLite 事务完成后：

1. Device 状态改为 revoked。
2. Device Key 状态改为 revoked 或 key_version 增加。
3. 发布本进程内部 `DeviceRevoked` 事件。
4. Tunnel Registry 关闭当前 WS/WSS，使用专用 close code。
5. 新 Token 和新连接均被拒绝。

第一版单活 Server 用进程内通知即可；多实例时再引入跨实例通知总线。

HTTP/WS 模式沿用完全相同的签名、Token audience、过期和撤销检查，但这些机制只证明应用身份，不能阻止链路窃听或中间人替换请求。Agent 配置为 `https://` 时，任何 TLS 失败都必须终止配对/认证，禁止自动尝试 `http://`。

## 8. 限流

- 登录：按 IP 前缀和 login_name 双维度限流。
- 配对码验证：按 IP 和 code hash 前缀限流。
- Token challenge：按 device_id 限流。
- 限流状态可先使用进程内有界缓存；Server 重启导致计数清空是第一版可接受风险。
- 认证失败不写入包含凭证原文的日志。

## 9. 故障处理

- Server SQLite 不可用：不签发新 Session/Pairing/Device Token。
- Session Store 查询失败：默认拒绝，不能 fail-open。
- 撤销通知发送失败：数据库状态仍是权威；Tunnel 心跳时复查 key version。
- Agent challenge 过期：重新获取，不能复用。
- Server 重启：Web Session 仍可由 DB 验证；短期设备 Token 可继续到过期或通过 key version 拒绝。

## 10. 安全测试

- Cookie 属性和 CSRF 测试。
- Session fixation 测试。
- 配对码并发消费测试。
- 撤销连接竞态测试。
- Token audience、expiry、key version 测试。
- 认证日志脱敏测试。
- 密码哈希参数回归测试。
- HTTP/HTTPS Cookie 差异、insecure capability、配对告警和禁止 TLS 降级测试。
