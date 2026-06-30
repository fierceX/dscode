# mink

**简体中文**

极简 AI coding agent。Rust 原生实现，默认面向 DeepSeek / OpenAI-compatible API，
同时支持 Rust 嵌入式场景注入自定义 LLM backend。

默认交付为 `mink` 终端二进制，也可构建 SDK 精简二进制 `mink-core`，或作为 Rust
库嵌入到其他服务中编排。

---

## 特性

- **OpenAI-compatible 默认后端** — 默认适配 DeepSeek / OpenAI-compatible streaming API，支持 reasoning、usage 和工具调用
- **可注入 LLM backend** — Rust runtime 可由宿主注入私有化模型、内网网关、厂商 SDK 或非 HTTP transport
- **两种终端模式** — REPL（`-i`，rustyline 行编辑）和 TUI（`--tui`，ratatui 全屏界面）
- **信号驱动的信念系统** — 自动检测工具执行错误，低信念时注入修正提示并约束恢复首步；可用 `MINK_SIGNAL_MODE=off` 关闭
- **自适应上下文压缩** — 三级压缩，自动摘要，保持上下文在窗口内
- **维修流水线** — Scavenge → Truncation → Storm Breaker，三段自动修复
- **Session 持久化** — JSONL 格式，`--continue` 无缝恢复
- **子代理（SubAgent）** — 隔离或 fork 上下文，并发执行
- **嵌入式只读 VFS** — Rust runtime 可为 Read/Glob/Grep 注入数据库后端，并按 resource session 隔离知识库
- **技能系统** — 按需加载 skill 文件，不污染后续 prompt
- **注册式资源与能力快照** — `Read` 通过 ResourceRouter 读取 artifact/skill/rule/session 资源，prompt 和 SDK 注入共享同一 capability snapshot
- **自定义提示词** — `--mission` 加载 MISSION.md 文件，替换默认系统提示词，自由定义 agent 目标和行为
- **Python SDK** — `mink-agent` pip 包，内置无 TUI 的 `mink-core` 二进制，支持沙箱控制和全参数配置
- **沙箱防护** — Linux nsjail/bubblewrap（完整文件系统隔离）、macOS sandbox-exec（写入隔离）
- **CPython WASI 沙箱** — `PythonSandbox` 工具，在 wasmtime + CPython WASI 中执行 Python 代码，WASI 级进程隔离
- **运行时约束** — `--disable-bash` / `--disable-sub-agent` / `--disable-web` / `--disable-python` 按场景禁用工具；`enabled_tools` 同时限制模型可见工具和真实执行边界
- **机器协议** — `--print` 输出 ndjson 事件流并以 `final` 结束；`--agent-jsonl` 提供 single-shot Agent JSONL 协议

---

## 快速开始

```bash
# 前置：Rust 1.85+，设置 DEEPSEEK_API_KEY 或通过配置指定 OpenAI-compatible 端点

# 编译
cargo build --release
# 或
make build

# REPL 交互模式
./target/release/mink -m flash -i

# TUI 全屏模式
./target/release/mink -m flash --tui

# 单次查询
./target/release/mink -m flash "explain this project"

# 继续上次会话
./target/release/mink -m flash --continue -i

# 使用自定义系统提示词
./target/release/mink --mission ./my-task.mission.md -i

```

---

## Rust Library

`mink-core` 是 Rust 发布包名，库 crate 名为 `mink`。发布库只包含可嵌入 runtime 和
`Display` 协议层；REPL/TUI、二进制入口和终端依赖归属 `mink-cli` workspace 包。
Rust 服务通常只启用嵌入式 runtime：

```toml
[dependencies]
mink = { package = "mink-core", version = "0.1.11", default-features = false, features = ["runtime"] }
```

然后在代码中通过 `mink::runtime` 或 `mink::prelude` 嵌入：

```rust
use mink::prelude::{AgentEvent, AgentOptions, AgentRuntime};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let rt = AgentRuntime::start_with_options(
        AgentOptions::new("/tmp/mink-home", ".")
            .with_api_key(std::env::var("DEEPSEEK_API_KEY")?)
            .with_model("flash"),
    ).await?;

    // 阻塞式 turn — 直接拿到 text/thinking
    let outcome = rt.run_turn("hello").await?;
    println!("{}", outcome.text);

    // 流式 turn — 实时事件
    let mut stream = rt.try_stream_turn("explain")?;
    while let Some(ev) = stream.recv().await {
        match ev {
            AgentEvent::Text { content } => print!("{content}"),
            AgentEvent::Final { .. } => break,
            _ => {}
        }
    }
    let outcome = stream.outcome().await?;

    rt.shutdown().await?;
    Ok(())
}
```

同进程 `AgentRuntime` 不会自动 sandbox 当前进程。需要完整进程级沙箱时，推荐参考 `examples/web_api.rs` 的 hidden worker 模式：业务服务 spawn 自身 worker 子进程，worker 先 re-exec 进沙箱，再调用 `mink::runtime`。

Rust 嵌入方可以继续使用默认 OpenAI-compatible backend，也可以实现 `mink::runtime::LlmBackend`
并通过 `AgentOptions::with_llm_backend()` 注入。模型名解析仍由 mink 统一处理：
`flash` / `pro` 是默认别名，`model_aliases` 可覆盖别名；未命中别名的模型名会原样传给 backend。
完整示例见 [custom_llm_backend.rs](crates/mink-core/examples/custom_llm_backend.rs)。

库使用方应优先从 `mink::prelude`、`mink::runtime`、`mink::config`、`mink::sandbox`
和 `mink::sdk_protocol` 导入类型。其他公开模块目前主要服务于内部 runtime 复用和过渡期测试，不建议作为稳定 API 依赖。

---

## Workspace Packages

| 路径 | 职责 |
|------|------|
| [crates/mink-core](crates/mink-core/README.md) | Rust 发布包 `mink-core`，库 crate 名 `mink`，包含可嵌入 runtime、工具核心、session、sandbox 和 SDK 协议 |
| [crates/mink-cli](crates/mink-cli/README.md) | workspace 内部二进制包，生成 `mink` 终端二进制和 `mink-core` SDK 精简二进制，持有 REPL/TUI 实现 |
| [mink_agent](mink_agent/README.md) | Python SDK，wheel 内置无 TUI 的 `mink-core` 二进制 |

---

## Python SDK

通过 pip 安装使用：

```bash
pip install mink-agent
```

```python
from mink_agent import AgentSession, SandboxConfig

session = AgentSession(SandboxConfig(
    api_key="sk-...",
    read_dirs=["src"],
    write_dirs=["src"],
    mission_file="./my-task.mission.md",
    signal_mode="full",
))
result = session.run("处理文档")
print(result["status"], result["events_path"])
```

详见 [mink_agent/README.md](mink_agent/README.md)。

---

## 文档索引

| 文档 | 说明 |
|------|------|
| [使用手册](docs/USAGE.md) | 面向用户：CLI/SDK/Rust 嵌入、配置、沙箱、session、技能和常见工作流 |
| [工具参考](docs/tools.md) | 面向工具协议：内置工具参数、结果通道、资源 URL、审批和构建裁剪 |
| [架构说明](docs/ARCHITECTURE.md) | 运行时分层、模块职责、资源/能力系统、核心数据流 |
| [设计文档](docs/DESIGN.md) | 设计哲学、关键不变式、注册式资源、能力快照、运行时和库化边界 |
| [信号系统设计](docs/设计哲学-信号系统.md) | 控制论 + 贝叶斯、冷却机制、信念度展示 |
| [Agent 开发指南](AGENTS.md) | 面向 AI agent：项目结构、模块索引、开发惯例 |

---

## 许可

MIT
