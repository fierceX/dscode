# mink-server：Server 与 Web 前端

> 更新日期：2026-08-03
> 版本：0.3.0｜单二进制 Web 工作区服务器（REST + SSE + 嵌入前端）
> 源码：`crates/mink-server/`（Rust）+ `crates/mink-server/web/`（Vue 3 + Vite）

---

## 1. 概述

`mink-server` 延续 Mink 的轻量定位：单二进制、低运行时依赖，把同一个 runtime 暴露为 Web 服务——与 CLI/TUI/Python SDK 共享内核，多端行为一致：

- **REST API**：会话管理（列表/创建/删除/打开/关闭/中断）、conversation/plan/todo/artifacts/files 读取
- **SSE 实时流**：`/api/sessions/{id}/stream` 推送 turn 事件（thinking/text/tool_call/tool_result/usage/title_update/stop/error）
- **嵌入前端**：Vue SPA 构建产物直接嵌入二进制，单文件分发
- **共享会话布局**：与 TUI/CLI 使用同一 `~/.mink/projects` 目录，终端与浏览器无缝交接

```
┌──────────────────────────────────────────────┐
│ mink-server（单二进制）                        │
│  ├─ REST 路由（axum）                         │
│  ├─ SSE stream（broadcast 转发 AgentEvent）    │
│  ├─ Session registry（锁协议 + active map）    │
│  ├─ Session runtime（AgentRuntime 包装）       │
│  └─ 静态资源（嵌入 web dist / 磁盘 ServeDir）  │
└──────────────────────────────────────────────┘
```

## 2. 快速开始

```bash
# 构建（build.rs 自动执行 npm run build 并嵌入前端）
cargo build -p mink-server

# 运行（默认端口 8765，读取 ~/.minkrc）
./target/debug/mink-server

# 指定端口 / 配置
MINK_SERVER_PORT=9000 ./target/debug/mink-server
./target/debug/mink-server path/to/mink-server.toml
```

打开 `http://localhost:8765` 即可使用 Web 界面。

## 3. 配置

优先级：**环境变量 > `mink-server.toml` > `~/.minkrc` > 默认值**。

| 配置项 | 环境变量 | mink-server.toml | 默认 |
|--------|----------|------------------|------|
| 监听地址 | `MINK_SERVER_HOST` | `[server] host` | `0.0.0.0` |
| 端口 | `MINK_SERVER_PORT` | `[server] port` | `8765` |
| Mink home | `MINK_HOME` | `[server] mink_home` | `$HOME` |
| 默认模型 | `MODEL` | `[server] model` / `~/.minkrc` `model` | `flash` |
| 最大并发会话 | `MINK_SERVER_MAX_RUNNING` | `[server] max_running` | `4` |
| 闲置自动关闭 | — | `[server] idle_close_secs` | `1800` |

`~/.minkrc` 与 TUI/CLI 共享同一配置文件（TOML 顶层字段，`model` 已生效；更多字段随 runtime 配置扩展）。

## 4. REST API

| 方法 | 路径 | 说明 |
|------|------|------|
| GET | `/api/sessions` | 会话列表（含 tokens_in/out/cache_read/context/费用 汇总） |
| POST | `/api/sessions` | 创建会话 `{name, cwd?}` |
| GET | `/api/sessions/{id}` | 会话状态（open/running） |
| DELETE | `/api/sessions/{id}` | 删除会话 |
| POST | `/api/sessions/{id}/open` | 打开（建立 runtime 锁） |
| POST | `/api/sessions/{id}/close` | 关闭（释放锁，任务继续后台） |
| POST | `/api/sessions/{id}/turn` | 发送消息执行 turn |
| POST | `/api/sessions/{id}/interrupt` | 中断当前 turn |
| GET | `/api/sessions/{id}/conversation` | 历史（`limit`/`tail`/`before_seq` 分页） |
| GET | `/api/sessions/{id}/plan` / `todo` / `artifacts` | 计划/Todo/Artifacts 读取 |
| GET | `/api/sessions/{id}/files?path=...&raw=true` | 文件树/内容（raw 取正文） |
| GET | `/api/sessions/{id}/stream` | **SSE 实时事件流** |

## 5. SSE 事件

`/stream` 转发 runtime 的 AgentEvent，事件字段与 conversation 一致：

| type | 字段 | 说明 |
|------|------|------|
| `turn_start` | reason | turn 开始（server 广播） |
| `thinking` / `text` | content | 流式输出 |
| `tool_call` | id/name/summary/**input** | 工具调用（含完整参数，前端结构化渲染） |
| `tool_result` | name/content/exit_code | 工具结果 |
| `usage` | input/output/cache_read/context/max_context | 用量（实时累计 + 当前上下文） |
| `title_update` | model/tokens/cost/belief/**cache_read/context** | 轮结束权威统计 |
| `stop` / `turn_error` / `signal` | reason/message | 结束与信号 |

前端 reducer 以 usage 增量累计 + title_update 权威覆盖的方式保证顶部指标行与用量面板一致。

## 6. 静态资源与嵌入

- **自动构建**：`build.rs` 在 `cargo build` 时自动执行 `npm run build`（web/），产物复制到 `OUT_DIR/assets` 并生成 `assets.rs`（`include_str!` 内容表）
- **嵌入服务**：默认从二进制内容服务静态资源（content-type 映射、`index.html` no-cache、静态资源 immutable 缓存、SPA fallback 到 index.html）
- **开发模式**：`MINK_SERVER_DEV_WEB=1` 时回退磁盘 `web/dist`（前端热迭代，改完强刷即可）
- E2E 使用 `MINK_SERVER_DEV_WEB=1` 保证测试服务最新构建产物

## 7. 部署注意

- **单二进制分发**：`target/release/mink-server` 即可（含前端），无需 node/npm
- **重启更新前端**：嵌入产物随 `cargo build` 更新——修改前端后需**重新构建 mink-server** 再重启
- **锁协议**：server 持有会话锁；TUI 不持锁——同一会话避免多端并发写
- **超时保护**：`MINK_SERVER_TURN_TIMEOUT`（默认 1200s）防止 LLM/工具挂起卡死 running 状态
- **安全**：单用户部署假设；`sk-fake`/受限 key 可用于测试环境

## 8. 测试

```bash
cd crates/mink-server/web
npx playwright test      # E2E 14 用例（真实浏览器 + 真实 server）
npx vitest run           # 单元测试 44
cargo test -p mink-server # 服务端测试（registry/config/runtime）
```

E2E 通过 global-setup 构造隔离临时 home + 模板会话，serial 模式顺序执行，失败产出 trace/error-context 供 AI 自愈。
