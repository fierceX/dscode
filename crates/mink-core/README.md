# mink-core

> 更新日期：2026-08-10

`mink-core` 是对外发布的 Rust 包；库 crate 名为 `mink`。

这个子包只承载可嵌入的 agent runtime 和核心能力，不包含 REPL/TUI 的具体终端实现，也不生成
`mink` / `mink-core` 二进制入口。二进制入口位于 workspace 内部包
[`mink-cli`](../mink-cli/README.md)。

## 包内容

- `mink::runtime` / `mink::prelude`：Rust 嵌入式入口，提供 `AgentRuntime`、`AgentOptions`、流式事件和 turn outcome。
- `mink::config`：与 CLI 共用的完整配置结构和解析辅助函数。
- `mink::ui`：`Display` trait、结构化工具调用/结果 presentation 和状态快照协议。具体
  REPL/TUI 渲染不在本包。
- `mink::sdk_protocol`：Agent JSONL 协议类型和 SDK 适配。
- `mink::sandbox`：进程级沙箱 re-exec 能力。
- `src/agent`、`src/tools`、`src/session`、`src/llm`：Mink 的主循环、工具、持久化和 LLM 流式客户端核心。

## 上下文与会话历史

`conversation.jsonl` 以只追加方式完整保留对话。压缩只推进
`context-state.json.active_start`，不重写历史。运行时只缓存活跃后缀，因此长期运行的嵌入式
agent 在压缩后不会让冷历史持续驻留内存。恢复 session 时会流式解析并校验 JSONL，但只保留
活跃消息。

压缩策略由显式配置控制，包括触发百分比、主请求响应预留、压缩后热尾部和摘要输出预算。
所有压缩统一调用 LLM 生成滚动摘要，并将摘要作为动态消息以保持 system/tools 前缀稳定。
摘要请求使用当前活动模型，并通过 runtime 注入的共享 LLM backend 发送。
Agent JSONL、Python SDK 和 Rust API 均可直接配置上下文窗口及五个压缩参数；runtime 会在创建
session 前拒绝 reserve、热尾部或摘要输出预算与窗口不相容的组合。
开启摘要输入降噪后，会在摘要请求前删除 thinking、压缩工具参数和结果，同时保留用户请求、
assistant 文本、错误证据和 artifact 引用。Provider 在产生可见输出前报告上下文溢出时，最多
触发一次压缩和一次重试。

Fork 子代理会在 runtime 初始化前克隆父 session 的完整状态。Artifact ID 从克隆后的索引继续
分配，正文文件使用独占创建，从而保持继承的 `artifact://` 引用稳定。

Plan 和 Todo 使用独立的 session 状态文件。确认后的计划在每次 LLM 请求时投影为动态
`<current-plan>` system message，不进入 immutable prefix 或 conversation。Todo 的权威完整
快照保存在 `todos.json`；成功变更以增量事件和紧凑 active 投影追加到 conversation，并使用
revision 和稳定 ID 防止 stale write。

## 依赖方式

```toml
[dependencies]
mink = { package = "mink-core", version = "0.3.3", default-features = false, features = ["runtime"] }
```

```rust
use mink::prelude::{AgentOptions, AgentRuntime, UsageSummary};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let rt = AgentRuntime::start_with_options(
        AgentOptions::new("/tmp/mink-session", ".")
            .with_api_key(std::env::var("DEEPSEEK_API_KEY")?)
            .with_model("flash"),
    ).await?;

    let outcome = rt.run_turn("解释这段代码").await?;

    // 本轮 LLM 请求的 Token 汇总
    let u = &outcome.usage;
    println!("billing_turn_id: {}", outcome.billing_turn_id);
    println!("input: {}, cache_read: {}, output: {}",
             u.tokens.input_tokens, u.tokens.cache_read_tokens, u.tokens.output_tokens);
    println!("cost: {} 纳元 (≈ {:.4} 元)", u.cost_nano_cny,
             u.cost_nano_cny as f64 / 1_000_000_000.0);

    // 每笔 LLM 请求明细
    for record in &outcome.usage_records {
        println!("  request {}: kind={:?}, status={:?}",
                 record.request_id, record.kind, record.status);
    }

    // usage.jsonl 文件路径（完整历史记录）
    println!("usage file: {}", outcome.session.usage_path.display());

    rt.shutdown().await?;
    Ok(())
}
```

流式 turn 使用 `try_stream_turn()` 逐条消费事件，结束时通过 `outcome()` 取回结果：

```rust
use mink::prelude::{AgentEvent, AgentRuntime};

let mut stream = rt.try_stream_turn("explain")?;
while let Some(ev) = stream.recv().await {
    match ev {
        AgentEvent::Text { content } => print!("{content}"),
        AgentEvent::Final { .. } => break,
        _ => {}
    }
}
let outcome = stream.outcome().await?;
```

## Read-only virtual filesystem

Embedded services can replace the ordinary-path backend used by `Read`,
`Glob`, and `Grep` without registering new tools:

```rust
let options = AgentOptions::new(session_home, cwd)
    .with_resource_session_id("tenant-task-001")
    .with_read_only_file_system(my_vfs);
```

Implement `mink::runtime::ReadOnlyFileSystem`. Every synchronous operation
receives a `VfsScope` containing both the inherited knowledge-base
`resource_session_id` and the concrete `agent_session_id`. Child agents inherit
the former. Without an injected backend, all three tools continue through their
original local implementations. Resource URLs such as `artifact://`,
`skill://`, `rule://`, and `session://` bypass the VFS.

Virtual reads are read-only and therefore do not produce anchored Edit
snapshots. Glob and Grep requests are validated by `mink-core`, while backends
return structured results for common formatting. Backends must honor the
request limits themselves; `mink-core` does not provide a second VFS search
implementation. See [`examples/redb_vfs.rs`](examples/redb_vfs.rs) for a
complete redb adapter; redb is an example-only dependency and is not linked
into `mink-core`.

## Custom LLM backend

By default, `AgentRuntime` uses the built-in OpenAI-compatible streaming
backend configured by `api_key`, `base_url`, model aliases, and OpenAI option
fields. The default backend supports `openai_tool_choice` and
`openai_extra_body` for provider-specific Chat Completions extension fields.
Use `AgentOptions::with_openai_reasoning_effort`,
`with_openai_include_usage`, `with_openai_token_param`,
`with_openai_tool_choice`, and `with_openai_extra_body` to configure the
built-in backend from embedded Rust code.
Embedded Rust applications can replace that backend without forking the agent
loop when the protocol itself is not OpenAI-compatible:

```rust
use std::sync::Arc;
use mink::prelude::{AgentOptions, AgentRuntime};

let runtime = AgentRuntime::start_with_options(
    AgentOptions::new("/tmp/mink-session", ".")
        .with_model("local")
        .with_llm_backend(Arc::new(MyLlmBackend::new())),
).await?;
```

Implement `mink::runtime::LlmBackend` and return a stream of `LlmEvent` values.
`LlmRequest.model` is the resolved provider model name, while
`LlmRequest.model_alias` preserves the requested alias such as `flash`, `pro`,
or a custom alias from `config.model_aliases`. On dispatch failure, return
`LlmRequestFailure { attempt_count, error }.into()` so usage accounting can
record the number of attempts.

See [`examples/custom_llm_backend.rs`](examples/custom_llm_backend.rs) for a
complete no-network backend:

```bash
cargo run -p mink-core --example custom_llm_backend
```

Custom model names are accepted as-is. Usage tokens are still recorded when the
backend emits `LlmEvent::Usage`; built-in price calculation only applies to the
default `flash` / `pro` DeepSeek tiers, so private model cost is reported as
zero unless the host computes pricing separately.

每次真实 LLM 请求都会追加到 session `usage.jsonl`。`TurnOutcome.usage_records` 只包含当前
`billing_turn_id` 的主 Agent、自动压缩和子代理明细，`TurnOutcome.usage` 是这些记录的汇总。

## Feature

| Feature | 说明 |
|---------|------|
| `runtime` | 默认启用。构建可嵌入 runtime、工具核心、session、LLM 客户端和协议层 |
| `python-sandbox` | 启用 `PythonSandbox` WASI 工具，额外引入 wasmtime 依赖 |
| `web-api` | 仅用于 `examples/web_api.rs` 示例，启用 axum |
| `slow-tests` | 启用重型测试开关 |

最小 runtime 构建：

```bash
cargo check -p mink-core --no-default-features --features runtime
```

## 沙箱说明

同进程 `AgentRuntime` 不会自动 sandbox 当前宿主进程。需要完整进程级沙箱时，应由业务服务
spawn worker 子进程，worker 先调用 `mink::sandbox::reexec_in_sandbox()` 进入沙箱，再创建
`AgentRuntime`。参考 `examples/web_api.rs`。

## 相关文档

- 根项目总览：[../../README.md](../../README.md)
- 使用手册：[../../docs/USAGE.md](../../docs/USAGE.md)
- 嵌入与 SDK：[../../docs/EMBEDDING.md](../../docs/EMBEDDING.md)
- 机器协议：[../../docs/PROTOCOL.md](../../docs/PROTOCOL.md)
- 工具参考：[../../docs/tools.md](../../docs/tools.md)
- 架构说明：[../../docs/ARCHITECTURE.md](../../docs/ARCHITECTURE.md)
