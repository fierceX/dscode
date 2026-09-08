# mink-core

> 更新日期：2026-09-03

`mink-core` 是对外发布的 Rust 包；库 crate 名为 `mink`。

这个子包只承载可嵌入的 agent runtime 和核心能力，不包含 REPL/TUI 的具体终端实现，也不生成
`mink` / `mink-core` 二进制入口。二进制入口位于 workspace 内部包
[`mink-cli`](../mink-cli/README.md)。

## 包内容

- `mink::runtime` / `mink::prelude`：Rust 嵌入式入口，提供唯一 shutdown owner `AgentRuntime`、可克隆 `AgentRuntimeHandle`、异步 `EventSink`、流式事件和 turn outcome。
- `mink::sdk_protocol`：Agent JSONL 协议类型和 SDK 适配。
- `mink::runtime::session`：只读 session 发现、读取与统一 usage 汇总。
- `mink::runtime::prefab`（启用 `prefab` feature）：`ensure_session()` / `PrefabSeed` 会话重组。
- `src/agent`、`src/tools`、`src/session`、`src/llm`：Mink 的主循环、工具、持久化和 LLM 流式客户端核心。

## 上下文与会话历史

`conversation.jsonl` 以只追加方式完整保留对话。压缩只推进
`context-state.json.active_start`，不重写历史。运行时只缓存活跃后缀，因此长期运行的嵌入式
agent 在压缩后不会让冷历史持续驻留内存。恢复 session 时会流式解析并校验 JSONL，但只保留
活跃消息。

压缩策略由显式配置控制，包括触发百分比、主请求响应预留、压缩后热尾部和摘要输出预算。
所有压缩统一调用 LLM 生成滚动摘要，并将摘要作为 internal user checkpoint 以保持
system/tools 前缀稳定。支持 cache projection 的 backend 会让摘要请求复用上一 Agent 请求的
实际 system/tools 与历史公共前缀；无法对齐时自动降级。auto 压力使用最近 provider prompt
usage 校准，preflight 仍完全依赖保守本地估算。摘要请求使用当前活动模型，并通过 runtime
注入的共享 LLM backend 发送。
Agent JSONL、Python SDK 和 Rust API 均可直接配置上下文窗口及五个压缩参数；runtime 会在创建
session 前拒绝 reserve、热尾部或摘要输出预算与窗口不相容的组合。
开启摘要输入降噪后，会在摘要请求前删除 thinking、压缩工具参数和结果，同时保留用户请求、
assistant 文本、错误证据和 artifact 引用。Provider 在产生可见输出前报告上下文溢出时，最多
触发一次压缩和一次重试。

Fork 子代理会在 runtime 初始化前克隆父 session 的完整状态。Artifact ID 从克隆后的索引继续
分配，正文文件使用独占创建，从而保持继承的 `artifact://` 引用稳定。

Plan 和 Todo 使用独立的 session 状态文件。PlanConfirm/PlanClear 在成功工具结果后追加
内部 user transition；仅在历史已压缩时从 `plan.md` 投影稳定的 `<active-plan-checkpoint>`。
Todo 的权威完整
快照保存在 `todos.json`；成功变更以增量事件和紧凑 active 投影追加到 conversation，并使用
revision 和稳定 ID 防止 stale write。

## 依赖方式

```toml
[dependencies]
mink = { package = "mink-core", version = "0.6.0", default-features = false, features = ["runtime"] }
```

```rust
use mink::prelude::{AgentOptions, AgentRuntime, UsageSummary};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let rt = AgentRuntime::start(
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

流式 turn 使用 `stream_turn()` 逐条消费事件，结束时通过 `outcome()` 取回结果：

```rust
use mink::prelude::{AgentEventKind, AgentRuntime};

let mut stream = rt.stream_turn("explain")?;
while let Some(ev) = stream.recv().await {
    match ev.kind {
        AgentEventKind::Text { content } => print!("{content}"),
        AgentEventKind::Final { .. } => break,
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

## Prefab session restructuring

Prefab is implemented as an optional integration layer in the independent
`mink-prefab` crate (`mink-integration` feature), wired through Mink's neutral
extension points `PrefixSource` / `PostInitHook` — `mink-core` itself has no
`mink-prefab` dependency. The host restructures the session after normal
initialization: the hook writes the selected template conversation/events for
a fresh session and records the special prefix as a standard
`prefix_snapshot` event; the prefix source then serves that system prompt /
tool schemas instead of the compiled prompt.

```rust
use mink::prelude::{AgentOptions, AgentRuntime};

let options = AgentOptions::new(home, cwd)
    .with_prefix_source(std::sync::Arc::new(mink_prefab::adapter::PrefabPrefixSource))
    .with_post_init_hook(std::sync::Arc::new(
        mink_prefab::adapter::PrefabRestructureHook::new(
            mink_prefab::adapter::resolve_template("flash")?,
        ),
    ))
    .with_api_key("sk-...");
let runtime = AgentRuntime::start(options).await?;
```

The CLI wires both automatically behind `--prefab[=TEMPLATE]` via
`mink_prefab::adapter::install_template`.

Seeding/restructuring refuses to touch an existing conversation; resuming a
prefab session does not re-run the restructure. A prefab-enabled runtime
rebuilds its prefix from the standard `prefix_snapshot` event in
`events.jsonl`; a normal runtime ignores it.

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

let runtime = AgentRuntime::start(
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
backend emits `LlmEvent::Usage`. Cost accounting has been removed, so
`UsageRecord.cost_nano_cny` is only a compatibility field: `0` for reported
records and `None` for unreported ones. Hosts that need pricing must compute it
themselves.

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
cargo check -p mink-core --no-default-features --features "runtime prefab"
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
