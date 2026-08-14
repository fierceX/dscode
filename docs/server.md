# mink-server：Server 与 Web 前端

> 更新日期：2026-08-13

---

## 1. 概述

`mink-server` 延续 Mink 的轻量定位：单二进制、低运行时依赖，把同一个 runtime 暴露为 Web 服务——与 CLI/TUI/Python SDK 共享内核，多端行为一致：

- **REST API**：会话管理（列表/创建/删除/打开/关闭/中断）、conversation/plan/todo/artifacts/files 读取
- **SSE 实时流**：`/api/sessions/{id}/stream` 转发 core `AgentEvent` envelope（带 `stream_sequence` 传输序号、心跳与 gap 对账）
- **嵌入前端**：Vue SPA 构建产物直接嵌入二进制，单文件分发
- **共享会话布局**：与 TUI/CLI 使用同一 `~/.mink/projects` 目录，终端与浏览器无缝交接

```
┌──────────────────────────────────────────────┐
│ mink-server（单二进制）                        │
│  ├─ REST 路由（axum）                         │
│  ├─ SSE stream（SessionRuntime 广播转发）      │
│  ├─ Session registry（fs2 文件锁 lease）       │
│  ├─ Session runtime（阶段机 + 超时保护）       │
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
| turn 超时 | `MINK_SERVER_TURN_TIMEOUT` | — | `1200`（秒） |

`~/.minkrc` 与 TUI/CLI 共享同一配置文件（TOML 顶层字段，`model` 已生效；更多字段随 runtime 配置扩展）。

## 4. REST API

| 方法 | 路径 | 说明 |
|------|------|------|
| GET | `/api/sessions` | 会话列表（含 tokens/费用/context 汇总） |
| POST | `/api/sessions` | 创建会话 `{name, cwd?}`（对 active alias 幂等，不重复创建） |
| GET | `/api/sessions/{id}` | 会话状态（open/running） |
| DELETE | `/api/sessions/{id}` | 删除会话（先持有系统文件锁，阻止并发删除） |
| POST | `/api/sessions/{id}/open` | 打开（建立 session lease） |
| POST | `/api/sessions/{id}/close` | 关闭（释放 lease；任务继续后台由 idle reaper 兜底） |
| POST | `/api/sessions/{id}/turn` | 发送消息执行 turn |
| POST | `/api/sessions/{id}/interrupt` | 中断当前 turn |
| GET | `/api/sessions/{id}/events` | events.jsonl 分页（`from_seq`/`limit`/`tail`/`before_seq`） |
| GET | `/api/sessions/{id}/conversation` | conversation.jsonl 轮次历史（同分页参数） |
| GET | `/api/sessions/{id}/plan` / `todo` / `artifacts` | 计划/Todo/Artifacts 读取 |
| GET | `/api/sessions/{id}/files?path=...&raw=true` | 文件树/内容（raw 取正文） |
| GET | `/api/sessions/{id}/stream` | **SSE 实时事件流** |

- **project 消歧**：所有 session 路由接受可选 `?project=`（0.4.0 起，用于跨项目同名 session）；不传时按 id 唯一匹配。
- **分页约束**：`limit` 必须在 `1..=2000`，超出返回 400；`tail`/`before_seq` 取“最接近目标”的末尾段，`from_seq` 前向读取保留开头；响应注入真实行号 `seq` 作为稳定 key。
- **错误映射**（typed `RegistryError`）：404 NotFound；409 Ambiguous / Locked / Busy；429 Capacity；500 Internal。

## 5. SSE 事件

`/stream` 订阅 SessionRuntime 的广播通道（容量 1024），原样转发 core `AgentEvent` 的
`{turn_id, sequence, kind}` envelope，并附加 `stream_sequence` 传输序号。事件名与
`AgentEventKind` 的 serde 命名一致（core 的 `final` 在 SSE 层改写为 `turn_final`）。
30s 无事件时发送 `: ping` 心跳注释帧防止中间代理断开；广播落后时收到
`stream_gap {missed}` 后连接结束，客户端应走权威恢复（见下）。

| type | 字段 | 说明 |
|------|------|------|
| `turn_started` | — | turn 开始 |
| `thinking` / `text` | content | 流式输出 |
| `tool_call` | id/name/summary/input | 工具调用（含完整参数，前端结构化渲染） |
| `tool_result` | tool_use_id/tool_name/content/success/exit_code/result_kind/presentation/artifacts | 工具结果（presentation 携带 Plan/Todo 结构化状态） |
| `signal` | signal_kind/severity/message | 信念信号 |
| `stop` / `retry` / `error` / `info` / `prompt` / `clear_line` | reason/message | 生命周期事件 |
| `title_update` | model/stats | 轮结束权威统计（`StatsSnapshot` 快照） |
| `sub_agent_status` / `sub_agent_output` | session_id/status/thinking/text/in_tokens/out_tokens | 子代理状态与输出 |
| `turn_final` | outcome | 权威终态：完整 `TurnOutcome`（含 status/error/usage） |
| `turn_error` | message | 超时/取消后的强制错误终态 |
| `stream_gap` | missed | 广播落后 N 条；随后连接结束 |

- **语义分工**：`turn_started` / `turn_final` 与 `stop` 职责分离——stop 只记录结束原因，
  final 持有权威运行态（含 outcome error）；外部 shutdown 竞争下先发布 Closed 相关终态，
  再补 timeout stop/final，且每个终态最多发布一次。
- **协议一致性**：`crates/mink-server/protocol-fixtures/agent-events.json` 是 core 与
  server 共享的协议 fixture，mink-core 测试对其做反序列化 round-trip，保证两端口径一致。
- 前端 reducer 以 `turn_final` 为权威状态；历史展示来自 `/conversation` 与 `/events`
  （注入 `seq`），实时事件与历史通过 `seq` / `live:{stream_sequence}` 区分 key。

## 6. 静态资源与嵌入

- **自动构建**：`build.rs` 在 `cargo build` 时自动执行 `npm run build`（web/），产物复制到 `OUT_DIR/assets` 并生成 `assets.rs`（`include_str!` 内容表）
- **嵌入服务**：默认从二进制内容服务静态资源（content-type 映射、`index.html` no-cache、静态资源 immutable 缓存、SPA fallback 到 index.html）
- **开发模式**：`MINK_SERVER_DEV_WEB=1` 时回退磁盘 `web/dist`（前端热迭代，改完强刷即可）
- E2E 使用 `MINK_SERVER_DEV_WEB=1` 保证测试服务最新构建产物

## 7. 生命周期与并发语义

- **SessionRuntime 阶段机**：`Idle → Running → Cancelling → Closing → Closed`；
  interrupt 只作用于 Running/Cancelling，随后等待 turn 有界退出并复位。
- **forced terminal**：turn 超时或外部 shutdown 竞争时，先登记强制终态（saw_stop/saw_final
  对账），发布缺失的 `stop`/`turn_final`/`turn_error`，保证客户端一定看到终态且不重复。
- **graceful shutdown**：Ctrl+C → axum serve 停止 → idle reaper 终止 →
  `registry.shutdown_all()`（逐会话 interrupt + 有界 join + runtime shutdown）。
- **idle reaper**：每 30s 扫描，自动关闭超过 `idle_close_secs` 未活动的会话。
- **session lease**：fs2 advisory file lock——锁文件永久保留，独占打开的文件句柄持有
  lease；删除会话前先获取并持有同一把系统文件锁，阻止其他 Registry 或进程删除使用中的
  会话（Locked/409）。TUI/CLI 不持锁，同一会话避免多端并发写。

## 8. 部署注意

- **单二进制分发**：`target/release/mink-server` 即可（含前端），无需 node/npm
- **重启更新前端**：嵌入产物随 `cargo build` 更新——修改前端后需**重新构建 mink-server** 再重启
- **多进程一致性**：lease 锁文件是跨进程稳定对象，不依赖 PID 存活判断；两个 server 进程
  同时操作同一会话时按文件锁顺序串行，冲突返回 409
- **超时保护**：`MINK_SERVER_TURN_TIMEOUT`（默认 1200s）防止 LLM/工具挂起卡死 running 状态；
  超时后进入 forced terminal 并关闭该会话 runtime
- **安全**：单用户部署假设；`sk-fake`/受限 key 可用于测试环境

## 9. 测试

```bash
cd crates/mink-server/web
npx playwright test      # E2E 14 用例（真实浏览器 + 真实 server）
npx vitest run           # 单元测试（reducer 39 / sessionController 7 / sse 1 / session 8）
cargo test -p mink-server # 服务端测试（registry/lease/config/runtime/SSE envelope，14 用例）
```

E2E 通过 global-setup 构造隔离临时 home + 模板会话，serial 模式顺序执行，失败产出
trace/error-context 供 AI 自愈；测试产物（test-results/）已加入 .gitignore。
