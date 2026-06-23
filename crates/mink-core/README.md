# mink-core

`mink-core` 是对外发布的 Rust 包；库 crate 名为 `mink`。

这个子包只承载可嵌入的 agent runtime 和核心能力，不包含 REPL/TUI 的具体终端实现，也不生成
`mink` / `mink-core` 二进制入口。二进制入口位于 workspace 内部包
[`mink-cli`](../mink-cli/README.md)。

## 包内容

- `mink::runtime` / `mink::prelude`：Rust 嵌入式入口，提供 `AgentRuntime`、`AgentOptions`、流式事件和 turn outcome。
- `mink::config`：与 CLI 共用的完整配置结构和解析辅助函数。
- `mink::ui`：`Display` trait、`ToolResultDisplay` 和状态快照协议。具体 REPL/TUI 渲染不在本包。
- `mink::sdk_protocol`：Agent JSONL 协议类型和 SDK 适配。
- `mink::sandbox`：进程级沙箱 re-exec 能力。
- `src/agent`、`src/tools`、`src/session`、`src/llm`：mink 的主循环、工具、持久化和 LLM 流式客户端核心。

## 依赖方式

```toml
[dependencies]
mink = { package = "mink-core", version = "0.1.8", default-features = false, features = ["runtime"] }
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
`skill://`, `session://`, and HTTP(S) bypass the VFS.

Virtual reads are read-only and therefore do not produce anchored Edit
snapshots. Glob and Grep requests are validated by `mink-core`, while backends
return structured results for common formatting. Backends must honor the
request limits; the exported collection helpers implement those limits for
iterator-based stores. See [`examples/redb_vfs.rs`](examples/redb_vfs.rs) for a
complete redb adapter; redb is an example-only dependency and is not linked
into `mink-core`.

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
- 工具参考：[../../docs/tools.md](../../docs/tools.md)
- 架构说明：[../../docs/ARCHITECTURE.md](../../docs/ARCHITECTURE.md)
