# 安全：技术设计

> 实现标记：本文描述目标设计；当前 `0.1.0` 已实现项和后续边界以 [后端实现状态](../../implementation-status.md) 为准。

## 1. 安全边界

```text
Internet
  [TLS boundary in secure profile / absent in trusted-http]
Public Server
  [Device auth + protocol boundary]
Agent
  [stdio process boundary]
Codex App Server
  [sandbox/approval boundary]
Project Files
```

每个边界独立验证输入，不能因为上一层已验证而信任 payload。

## 2. 传输档位

- `secure`：公网 HTTPS/WSS，TLS 1.2+，优先 TLS 1.3；Agent 严格验证证书和主机名。
- `trusted-http`：HTTP/WS，只在显式 `allow_insecure_http=true` 后监听非 loopback；功能协议不变，但 `transportSecurity=insecure` 必须进入配置状态、握手 capability、页面横幅和安全审计。
- `local`：本地页面只绑定 loopback HTTP，仍执行 Host、Origin、CSRF 和 Session 校验。
- HSTS 只在确认域名永久使用 HTTPS 后启用，绝不能由 HTTP 兼容部署发送。
- 企业 CA 通过显式路径配置，不提供 insecure skip verify；WSS 失败不能自动回退 WS。
- 代理到 Rust Server 走 loopback/私网；若跨主机则再次 TLS。

HTTP/WS 模式下 Bearer Token、Cookie、消息正文和路径元数据可被网络观察者窃取或篡改。应用层签名只能保护被签名的特定消息，无法保护首次加载的前端 JavaScript，因此第一版不把它包装成“安全 HTTP”。推荐无证书用户将 HTTP 入口放在 VPN 或 SSH 端口转发内。

## 3. Web 安全

- HTTPS 使用 `Secure; HttpOnly; SameSite=Strict` Session Cookie。
- HTTP 兼容模式使用 `HttpOnly; SameSite=Strict`，不能设置 `Secure`；响应携带 insecure capability，前端持续提示。Session 生命周期应更短，并建议只在可信隧道内使用。
- CSRF token 绑定 Session。
- 严格 Origin 检查。
- CSP：`default-src 'self'`，按实际资源最小放行。
- `frame-ancestors 'none'` 防 clickjacking。
- `X-Content-Type-Options: nosniff`。
- 敏感 API `Cache-Control: no-store`。
- Markdown 严格 sanitizer；默认不允许 raw HTML。

## 4. Localhost 安全

- bind loopback only。
- Host allow-list。
- 随机一次性 launch token 交换 HttpOnly Session。
- 防 DNS rebinding。
- 本地修改 API 同样 CSRF。
- 随机端口不是安全边界，只是减少冲突。
- 不允许任意 Origin CORS。

## 5. Device Key

- Ed25519 keypair 在设备生成。
- 私钥优先存 OS Keychain/Credential Manager/libsecret。
- fallback 文件权限仅当前用户可读。
- Server 只存 public key。
- challenge 包含随机 nonce、device ID、audience、protocol version、expiry。
- challenge 一次性消费。
- 短期设备 Token 带 key_version。

## 6. 授权

每个 Server 操作校验：

```text
authenticated user
AND resource.user_id == user.id
AND device active
AND command allowed for device/project/thread state
```

Agent 再次校验：

- command target device 等于自身。
- connection epoch 当前有效。
- project/thread 存在并匹配。
- expires_at 未过期。
- command kind 在支持列表。

## 7. 重放防护

- HTTP(S)：Idempotency-Key + request fingerprint。
- WS(S)：message ID、command ID、epoch、expires_at。
- Agent inbox 唯一约束。
- Approval 绑定 approval ID + active Turn ID。
- Pairing challenge nonce 一次性。
- 旧连接 epoch 的帧拒绝。

## 8. 输入限制

- HTTP body、WS(S) frame、JSONL line 大小上限。
- JSON 嵌套深度和数组长度上限。
- 本地入口可以接收 Project path 并 canonicalize；远程入口只能接收 Agent 签发的短期 `directory_ref`。
- `directory_ref` 绑定 device、allowed root、canonical path、purpose、expiry 和 nonce；Agent 创建时重验 symlink、权限和根边界。
- UI 文本按纯数据处理。
- 外部 URL 和 Git metadata 清理凭证。
- 诊断命令不接受任意 shell 参数拼接。

## 9. Secret 管理

- Server 密码哈希、Token signing key、数据库凭证来自 secret file/env secret manager。
- 不把秘密写配置示例、命令行参数或日志。
- 密钥支持轮换，验证阶段短期接受 current/previous key。
- 备份加密并与运行凭证分离。
- 崩溃 dump 默认关闭或确保不包含秘密内存。

## 10. 审计

记录：

- 登录成功/失败摘要。
- Web Session 撤销。
- Pairing 创建/消费/取消。
- Device 撤销和认证失败。
- 高风险 Approval 决策元数据。
- 协议违规、重复旧 epoch、版本不兼容。

不记录 prompt、完整命令、输出和文件内容。

规范化完整历史由 History Store 保存，不通过审计日志保存。两者使用不同 Repository、访问权限和保留策略，防止“为了审计”复制一份无界正文。

## 11. History Store 保护

- Server SQLite 文件及 WAL 只允许 Server 运行身份和备份身份访问；它没有网络监听端口，也不得放在可被 Web Server 直接下载的目录。
- 备份加密并验证恢复；导出和清理都记录不含正文的审计事件。
- History DTO 使用字段白名单：允许用户/Agent 消息和受控执行记录，不允许 Codex access token、设备私钥、Server Token、任意环境变量或项目源文件正文。
- 敏感正文不进入 tracing、metric label、panic、诊断包或普通审计表。
- 第一版 Server 可读取历史，因此不是端到端加密；若未来增加 E2EE，必须单独解决服务端搜索、跨设备授权和密钥恢复，不能在当前协议里半实现。

## 12. 依赖与供应链

- Cargo.lock、bun.lock 固定。
- CI 运行 Rust/JS 依赖漏洞扫描。
- 发布产物签名和 checksum。
- 最小化前端第三方脚本，禁止运行时 CDN。
- 定期升级，但每次升级通过兼容性和长稳测试。
- Codex App Server 二进制由用户安装/指定，Agent 记录版本不替换其认证数据。

## 13. 安全测试

- Authz IDOR 测试。
- CSRF、CORS、Origin 和 Host 测试。
- DNS rebinding 测试。
- WS/WSS token/epoch/replay 测试，以及 secure 档位失败不降级测试。
- HTTP 非 loopback 开关、永久风险横幅、Cookie 属性和 secure-context capability 测试。
- Pairing 并发和 brute-force 限流测试。
- JSON/Markdown fuzz 和 XSS 测试。
- 日志/诊断 secret scanner。
- 依赖漏洞和许可证检查。
- `directory_ref` 伪造、过期、跨设备、symlink swap 和 TOCTOU 测试。
- History DTO allow-list、备份权限和正文不进入日志测试。
