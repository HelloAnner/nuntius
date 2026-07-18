# Client Frontend

本目录用于实现工作电脑的本地控制台（loopback 页面）。构建产物输出到
`dist/`，由 `nuntius-client` 在编译期嵌入二进制。

当前只保留占位 `dist/index.html`，后续前端实现也必须完整放在本目录；
它不依赖 Server 前端源码。Client 后端及本地 API 不依赖前端工具链。
