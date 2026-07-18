# Server Frontend

手机、平板与桌面访问的远程控制前端。它是 `server/` 项目内部独立的
React 18 + TypeScript + Vite 工程；进入本目录执行 `bun install && bun run build`，
产物输出到 `dist/`，由 `nuntius-server` 在编译期嵌入二进制。

设计系统、协议类型、SSE 归并器与消息组件位于本目录的 `src/shared/`
（`@/shared`），不依赖仓库根目录或 Client 前端源码。开发调试在本目录
执行 `bun run dev`（127.0.0.1:5180，`/api` 代理到 127.0.0.1:8080）。
