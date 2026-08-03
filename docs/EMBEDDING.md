# 嵌入与 SDK 使用

> 更新日期：2026-08-03

本文面向把 Mink 作为**库或 SDK** 集成的开发者：Rust 嵌入式 runtime（`mink::runtime`）
和 Python SDK（`mink-agent`），以及跨端统一的 Token 用量与费用访问。终端用户的
CLI 交互、配置和会话管理见 [使用手册](USAGE.md)；机器协议（`--print` / `--agent-jsonl`）
见 [机器协议](PROTOCOL.md)；架构和模块职责见 [ARCHITECTURE.md](ARCHITECTURE.md)。

---

[TOC]

---

## Rust 库嵌入

Rust 发布包为 `mink-core`，库 crate 名为 `mink`。发布包只包含可嵌入 runtime 和
`Display` 协议层；终端 REPL/TUI 和二进制入口在 `mink-cli` workspace 包。
服务端嵌入时推荐只启用 runtime：

```toml
[dependencies]
mink = { package = "mink-core", version = "0.3.0", default-features = false, features = ["runtime"] }
```

稳定入口：`mink::prelude`、`mink::runtime`、`mink::config`、`mink::sandbox`、
`mink::sdk_protocol`；其他公开模块不承诺稳定 API。

### 最小示例

```rust
use mink::prelude::{AgentOptions, AgentRuntime};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let rt = AgentRuntime::start_with_options(
        AgentOptions::new("/tmp/mink-session", ".")
            .with_api_key(std::env::var("DEEPSEEK_API_KEY")?)
            .with_model("flash"),
    ).await?;

    let outcome = rt.run_turn("hello").await?;
    println!("{}", outcome.text);

    rt.shutdown().await?;
    Ok(())
}
```

### 流式 turn

`try_stream_turn()` 逐条消费事件，结束时通过 `outcome()` 取回完整结果：

```rust
use mink::prelude::{AgentEvent, AgentRuntime};

let mut stream = rt.try_stream_turn("explain")?;
while let Some(ev) = stream.recv().await {
    match ev {
        AgentEvent::Text { content } => print!("{content}"),
        AgentEvent::Thinking { content } => eprint!("{content}"),
        AgentEvent::ToolCall { name, summary } => eprintln!("[tool] {name} {summary}"),
        AgentEvent::Final { .. } => break,
        AgentEvent::Error { message } => eprintln!("error: {message}"),
        _ => {}
    }
}
let outcome = stream.outcome().await?;
```

### AgentOptions 配置速查

`AgentOptions` 是 ergonomic builder，底层持有完整 `Config`，可通过 `config_mut()`
逃生口访问任何未提供 builder 的字段。常用方法：

| 类别 | 方法 |
|------|------|
| 基础 | `with_api_key()` / `with_base_url()` / `with_model()` / `with_session()` / `with_session_layout()`（或 `with_isolated_sessions()` 等快捷方法） |
| 工具 | `with_enabled_tools()`（唯一工具选择入口；空列表禁用全部） |
| Edit | `with_edit_mode()` / `with_edit_fuzzy_match()` / `with_edit_fuzzy_threshold()` / `with_edit_enforce_seen_lines()` |
| 压缩 | `with_max_context_tokens()` / `with_context_compact_pct()` / `with_context_reserve_tokens()` / `with_context_compact_tail_tokens()` / `with_context_compact_max_output_tokens()` / `with_context_compact_input_reduction()` |
| 超时 | `with_tool_timeout_secs()` / `with_sub_agent_timeout_secs()` / `with_llm_timeouts(first, idle, heartbeat)` |
| 输出限制 | `with_tool_result_max_bytes()` / `with_file_write_max_bytes()` / `with_search_limits(max_files, max_results)` / `with_max_tokens()` / `with_max_turns()` |
| OpenAI | `with_openai_reasoning_effort()` / `with_openai_tool_choice()` / `with_openai_extra_body()` / `with_openai_token_param()` / `with_openai_include_usage()` |
| 能力 | `with_mission_content()` / `with_selected_skills()` / `with_runtime_skill_content()` / `with_skill_discovery_policy()` / `with_resource_handler()` / `with_read_only_file_system()` / `with_resource_session_id()` |
| 后端 | `with_llm_backend()` / `with_sandbox()` / `with_sandbox_python()` |

### 自定义 LLM backend

默认 OpenAI-compatible backend 支持 `openai_extra_body` 和 `openai_tool_choice`
适配大多数兼容端点：

```rust
use std::collections::BTreeMap;
use mink::prelude::{AgentOptions, AgentRuntime};
use serde_json::json;

let runtime = AgentRuntime::start_with_options(
    AgentOptions::new("/tmp/mink-session", ".")
        .with_model("local")
        .with_openai_reasoning_effort("high")
        .with_openai_tool_choice("auto")
        .with_openai_extra_body(BTreeMap::from([
            ("custom_budget".to_string(), json!(8192)),
        ])),
).await?;
```

非 OpenAI-compatible 协议可实现 `mink::runtime::LlmBackend` 注入：

```rust
use std::sync::Arc;
use mink::prelude::{AgentOptions, AgentRuntime};

let runtime = AgentRuntime::start_with_options(
    AgentOptions::new("/tmp/mink-session", ".")
        .with_model("local")
        .with_llm_backend(Arc::new(MyLlmBackend::new())),
).await?;
```

实现要点：
- 从 `LlmRequest` 读取 system prompt、messages、tools、取消 token 和模型名
- `LlmRequest.model` 是解析后的真实模型名；`LlmRequest.model_alias` 保留用户请求的别名
- 失败时返回 `LlmRequestFailure { attempt_count, error }`，usage 日志可记录重试次数

完整示例：

```bash
cargo run -p mink-core --example custom_llm_backend
```

### 嵌入式只读 VFS

私有化服务可替换 `Read`、`Glob`、`Grep` 的普通路径后端为数据库，而不注册新工具：

```rust
use std::sync::Arc;
use mink::prelude::{AgentOptions, AgentRuntime};

let vfs = Arc::new(MyReadOnlyFileSystem::open("knowledge.db")?);
let runtime = AgentRuntime::start_with_options(
    AgentOptions::new("/tmp/mink-session", ".")
        .with_resource_session_id("tenant-task-001")
        .with_read_only_file_system(vfs),
).await?;
```

实现同步的 `mink::runtime::ReadOnlyFileSystem` trait。每个操作收到 `VfsScope`：
- `resource_session_id`：知识库数据分区；子代理继承该值
- `agent_session_id`：实际发起调用的主代理或子代理 session id

虚拟 Read 是只读的，不产生 Hashline snapshot；`Write`/`Edit` 仍操作本地文件。VFS runtime
不会暴露 Edit；显式要求 Edit 会在启动时失败。
`artifact://`、`skill://`、`rule://`、`session://` 不进入 VFS。
完整 redb 示例见
[`crates/mink-core/examples/redb_vfs.rs`](../crates/mink-core/examples/redb_vfs.rs)。

### 沙箱

同进程 `AgentRuntime` 不会自动 sandbox 当前进程。需要完整进程级沙箱时，由业务服务
spawn 自身 worker 子进程：worker 先调用 `mink::sandbox::reexec_in_sandbox()` 进入沙箱，
再创建 `AgentRuntime`（hidden worker 模式，参考
`crates/mink-core/examples/web_api.rs` 的完整实现）。

---

## Python SDK

SDK wheel 内置无 TUI 的 `mink-core` 二进制，无需额外安装：

```bash
pip install mink-agent
```

### 最小示例

```python
from mink_agent import AgentSession, SandboxConfig

session = AgentSession(SandboxConfig(
    api_key="sk-...",               # 或设置 DEEPSEEK_API_KEY 环境变量
    read_dirs=["src"],
    signal_mode="full",             # "full" 启用信号系统，"off" 关闭
))
result = session.run("scan this repo and summarize")
print(result["text"])
session.close()
```

单次快捷调用：

```python
from mink_agent import quick_run

result = quick_run("解释这段代码", read_dirs=["/path/to/project"], api_key="sk-...")
print(result["text"])
```

每次 `run()` 启动一个新的 `mink-core --agent-jsonl` 进程；持续交互通过相同的
`mink_home + session_id` 复用磁盘 session。同一 `AgentSession` 实例不支持并发调用；
并发任务应创建多个实例或由外层应用排队。

### SandboxConfig 关键配置

| 类别 | 字段 | 说明 |
|------|------|------|
| 路径 | `mink_home` / `session_layout` / `cwd` | session 根目录与布局（SDK 默认 `home`） |
| 文件系统 | `read_dirs` / `write_dirs` | agent 可读/可写目录 |
| 工具 | `enabled_tools` | 精确工具选择；`None` 用默认集合，显式列出 `PythonSandbox` 才启用它 |
| Edit | `edit_mode` / `edit_fuzzy_match` / `edit_fuzzy_threshold` / `edit_enforce_seen_lines` | 与 Rust/CLI 相同的双模式配置 |
| 信号 | `signal_mode` | `"full"` / `"off"` / `None`（继承 `MINK_SIGNAL_MODE`） |
| 提示词 | `mission_file` / `mission_content` | MISSION.md 文件或内联内容（二选一，内联避免临时文件） |
| 技能 | `skills` / `inline_skills` / `skill_discovery_policy` | 技能选择与注入 |
| 压缩 | `max_context` / `context_compact_*` | 与 Rust/CLI 同一组压缩参数 |
| 超时 | `timeout_secs` / `tool_timeout` / `sub_agent_timeout` / `llm_*` | 各层超时限制 |
| 沙箱 | `sandbox_backend` | `"auto"` / `"nsjail"` / `"bwrap"` / `"sandbox-exec"` / `"off"` |
| API | `api_key` / `api_url` / `model` | DeepSeek 配置 |

### 结果字段

`run()` 返回 dict，与 Rust `TurnOutcome` 对应：

| 字段 | 说明 |
|------|------|
| `text` / `thinking` | agent 回复与推理 |
| `tool_calls` / `tool_results` | 工具调用与结果事件 |
| `status` / `error` / `exit_code` | 执行状态 |
| `session_id` / `home` / `cwd` | session 信息 |
| `events_path` / `conversation_path` / `artifacts_dir` / `summary_path` / `usage_path` | session 文件路径 |
| `billing_turn_id` / `usage_records` / `usage` | Token 用量与费用（见下） |

### 流式事件

```python
for event in session.stream_events("解释这段代码"):
    if event.type == "thinking_delta":
        ...
    elif event.type == "answer_delta":
        ...
```

`AgentStreamEvent.type` 常见值：`thinking_delta` / `answer_delta` / `tool_call` /
`tool_result` / `final`。原始 Agent JSONL 事件可用 `raw_stream()` 逐条获取。

---

## Token 用量与费用

每轮 `run_turn()` 结束后，`TurnOutcome` 携带本轮所有 LLM 请求的 Token 消耗和人民币费用。

### 字段说明

| Rust 字段 | Python 字段 | 说明 |
|-----------|------------|------|
| `billing_turn_id` | `billing_turn_id` | 本轮稳定标识；Agent、压缩、子代理共用 |
| `usage_records` | `usage_records` | 每笔 LLM 请求明细 |
| `usage` | `usage` | `UsageSummary` 汇总：请求数、attempt 数、Token、纳元费用 |
| `session.usage_path` | `usage_path` | `usage.jsonl` 路径 |

### UsageSummary

| 字段 | 说明 |
|------|------|
| `request_count` | 本轮逻辑请求数 |
| `reported_request_count` | 返回 usage 的请求数 |
| `unreported_request_count` | 未返回 usage 的请求数 |
| `attempt_count` | HTTP 重试合计 |
| `tokens` | [TokenUsage](#tokenusage) |
| `cost_nano_cny` | 预估费用（纳元，`1 元 = 10⁹ 纳元`） |

### TokenUsage

| 字段 | 说明 |
|------|------|
| `input_tokens` | 输入 Token（已减缓存命中） |
| `cache_read_tokens` | 缓存命中 Token（按折扣价） |
| `cache_creation_tokens` | 新增缓存写入 Token（按全价） |
| `output_tokens` | 输出 Token |

### 采集路径

```text
Turn / Compaction / SubAgent → MeteredStream → usage.jsonl
→ OrchActor::finish_usage() → TurnOutcome
```

Agent 工具循环、自动压缩、子代理共享同一 `billing_turn_id`。手动压缩使用 `operation-*`。

### 定价模型

DeepSeek API 官方单价（纳元整数运算）：

| 模型 | 输入（纳元/token） | 输出（纳元/token） | 缓存读取（纳元/token） |
|------|-------------------|-------------------|----------------------|
| Flash | 1,000 | 2,000 | 20 |
| Pro | 3,000 | 6,000 | 25 |

计算公式：`input × input_nano + cache_creation × input_nano + cache_read × cache_read_nano + output × output_nano`

未报告 usage 的请求 `cost_nano_cny` 为 `None`。未知模型只记录 Token，费用按 0 统计。

### usage.jsonl 格式

```json
{"version":2,"billing_turn_id":"turn-...","request_id":"request-...",
 "kind":"agent","model":"deepseek-v4-flash","attempt_count":1,"status":"reported",
 "tokens":{"input_tokens":100,"cache_read_tokens":40,...},
 "cost_nano_cny":140800,"completed_at":"2026-06-18T00:00:00Z"}
```

### Rust 库中访问

```rust
use mink::prelude::{AgentOptions, AgentRuntime, UsageSummary};

let outcome = AgentRuntime::start_with_options(
    AgentOptions::new("/tmp/mink-session", ".")
        .with_api_key(std::env::var("DEEPSEEK_API_KEY")?)
        .with_model("flash"),
).await?.run_turn("解释这段代码").await?;

println!("input: {}, cost: {} 纳元",
    outcome.usage.tokens.input_tokens, outcome.usage.cost_nano_cny);
for record in &outcome.usage_records {
    println!("  {}: kind={:?}, status={:?}", record.request_id, record.kind, record.status);
}
```

### Python SDK 中访问

```python
from mink_agent import AgentSession, SandboxConfig

session = AgentSession(SandboxConfig(api_key="sk-...", read_dirs=["."]))
result = session.run("解释这段代码")
print(f"cost: {result['usage']['cost_nano_cny']} nano-cny")
for record in result['usage_records']:
    print(f"  {record['request_id']}: kind={record['kind']}")
session.close()
```

### CLI 中查看

```bash
mink -m flash --print "hello" | jq 'select(.type=="final") | {billing_turn_id, usage}'
cat ~/.mink/projects/<project_key>/<session_id>/usage.jsonl | jq -c
```

---

## 相关文档

- 终端用户手册：[USAGE.md](USAGE.md)
- 机器协议：[PROTOCOL.md](PROTOCOL.md)
- 工具协议参考：[tools.md](tools.md)
- Rust 库包说明：[crates/mink-core/README.md](../crates/mink-core/README.md)
- Python SDK 说明：[mink_agent/README.md](../mink_agent/README.md)
