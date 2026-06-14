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
