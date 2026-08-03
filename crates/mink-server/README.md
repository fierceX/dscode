# mink-server

Web 工作区服务器：单二进制（REST + SSE + 嵌入前端）。

- 详细文档：[docs/server.md](../../docs/server.md)
- 快速开始：`cargo build -p mink-server && ./target/debug/mink-server`（默认 8765 端口）
- 开发模式：`MINK_SERVER_DEV_WEB=1`（服务磁盘 web/dist，前端热迭代）
- 配置：环境变量 > `mink-server.toml` > `~/.minkrc` > 默认
