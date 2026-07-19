# 图片输入、分析与展示

## 范围

远程控制台支持从手机相册或文件选择器上传 JPEG、PNG、WebP 图片，可只发图片，也可与文字一起发送。图片先落到 Server 私有数据目录，再由目标 Client 通过设备身份下载、校验并转换为 Codex App Server 的 `localImage` 输入。远程和本地历史页面都使用同一套缩略图消息组件展示附件。

## 数据流

1. 浏览器向 `POST /api/v1/threads/{threadId}/attachments` 发送单文件 multipart 请求，并携带 CSRF 与幂等键。
2. Server 按文件魔数识别格式，限制 20 MiB、单条消息 4 张、5000 万像素，完成解码后生成 360 px WebP 缩略图；原图与缩略图写入 `<server-data>/attachments/<userId>/<attachmentId>/`。
3. 浏览器发送 Turn 时只提交 `attachmentIds` 和稳定的 `clientMessageId`。Server 校验附件属于当前用户、会话和目标设备，并在保存命令的同一个 SQLite 事务中建立引用。
4. Client 收到带完整附件元数据的设备命令后，使用短期设备 Bearer Token 下载原图，核对长度、SHA-256、格式和尺寸，再原子写入 `~/.nuntius/attachments/<threadId>/<attachmentId>/`。
5. Client 调用 Codex App Server 的 `turn/start` 或 `turn/steer`，输入数组由可选 `text` 和一个或多个 `{type: "localImage", path, detail: "auto"}` 组成。
6. `turn.started` / `turn.steered` 事件和历史同步都携带附件展示元数据。共享前端先显示乐观缩略图，再通过 `clientMessageId` 与设备事件归并，避免图片消息重复。

## 存储与访问控制

- Server 和 Client 只保存规范化扩展名，不使用上传文件名构造路径；所有目录、文件按私有权限创建。
- 浏览器原图/缩略图接口要求当前 Web Session；设备下载接口要求短期设备令牌，且附件必须绑定到该设备。
- 原始文件名仅用于展示，会去除路径和控制字符，并按 UTF-8 字节数截断。
- 未被命令或历史引用的暂存附件可由浏览器删除；一旦进入持久命令或消息历史，就不能通过暂存删除接口移除。
- `nuntius-server backup` 与 `nuntius-client backup` 生成目录型备份，其中同时包含 SQLite 快照和附件目录。

## 兼容性

设备与服务能力列表新增 `image-input.v1`。文字协议仍保持兼容：`attachmentIds`、`clientMessageId` 和历史项 `attachments` 都有默认值；不发送图片的旧请求沿用原有处理路径。
