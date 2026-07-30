# mink

[![Crates.io](https://img.shields.io/crates/v/mink-core.svg)](https://crates.io/crates/mink-core)
[![MIT licensed](https://img.shields.io/badge/license-MIT-blue.svg)](./LICENSE)
[![Rust 1.94+](https://img.shields.io/badge/rust-1.94%2B-blue)](https://blog.rust-lang.org/2025/06/05/Rust-1.94.0.html)

**极简 AI coding agent — Rust 原生 · 终端优先 · 库级可嵌入**

默认面向 DeepSeek / OpenAI-compatible API 优化，交付为 `mink` 终端二进制，
也可作为 Rust 库（`mink::runtime`）嵌入任何服务，或通过 Python SDK
（`pip install mink-agent`）直接调用。

---

**三种使用方式：** `mink` 终端二进制 · `mink-core` SDK 精简二进制 · `mink::runtime` Rust 库

---

## 目录

- [谁适合用 mink](#谁适合用-mink)
- [特性](#特性)
- [快速开始](#快速开始)
- [Rust Library](#rust-library)
- [Python SDK](#python-sdk)
- [Workspace Packages](#workspace-packages)
- [文档索引](#文档索引)
- [许可](#许可)

---

## 谁适合用 mink

| 场景 | 推荐方式 |
|------|----------|
| 日常开发——在终端中手写 prompt 让 AI 读写文件、执行命令 | `mink -i`（REPL）或 `mink --tui`（全屏 TUI） |
| CI/CD 流水线——自动化代码审查、文档生成、批量重构 | `mink "task description"` 单次查询，或 `--agent-jsonl` 机器协议 |
| Python 项目——在 Python 中编排 AI agent 工作流 | `pip install mink-agent`，Python SDK 内置 `mink-core` 二进制 |
| Rust 服务——将 AI coding agent 嵌入你自己的应用 | `mink::runtime` 库，`AgentRuntime::run_turn()` / `stream_turn()` |
| 企业/内网环境——接入私有模型或非 OpenAI 协议 | 自定义 `LlmBackend` 注入，`model_aliases` 别名映射 |
| 多租户知识库——用隔离的知识库上下文驱动 agent | 只读 VFS 注入，按 `resource_session_id` 隔离 |

---

## 特性

### ⚙️ 核心引擎

- **OpenAI-compatible 默认后端** — 内置 DeepSeek / OpenAI 流式客户端，支持 reasoning、usage、工具调用和扩展参数（`openai_tool_choice`、`openai_extra_body`）
- **可注入 LLM backend** — 实现 `mink::runtime::LlmBackend` trait，接入私有模型、内网网关、厂商 SDK 或非 HTTP transport
- **信号驱动的信念系统** — 自动检测工具执行错误，低信念时注入修正提示并约束恢复首步；`MINK_SIGNAL_MODE=off` 可完全关闭
- **显式上下文压缩** — 百分比阈值、响应预留、热尾部和摘要输出预算全参数化；可选摘要输入降噪（过滤 thinking、压缩工具结果）
- **三段维修流水线** — Scavenge（回收遗漏调用）→ Truncation（修复残缺消息）→ StormBreaker（抑制重复调用），自动闭环修复

### 🖥️ 终端与界面

- **三种交互 surface** — REPL 行模式（`-i`）、Full TUI 全屏模式（`--tui`）、Inline TUI 原生 scrollback 模式（`--tui=inline`）
- **Anchored Edit 协议** — `Read` 生成带行 hash 的 snapshot header，`Edit.patch` 按行锚定修改，stale snapshot fail closed，防止并发漂移
- **结构化 transcript** — 统一的工具卡片渲染、Markdown 子集、自动折叠、实时信念 / token / 费用状态栏
- **机器协议** — `--print` 输出 ndjson 事件流；`--agent-jsonl` 提供 single-shot Agent JSONL 协议

### 🛠️ 工具系统

- **内置工具** — Read / Write / Edit / Bash / Python / Glob / Grep / PlanDraft / PlanConfirm / PlanClear / TodoRead / TodoWrite / TodoAdvance / SubAgent
- **统一工具选择** — `enabled_tools` 是唯一启用入口，同时决定模型可见 schema、能力工作流和真实执行边界；`PythonSandbox` 仅在显式列出时启用
- **语义能力模型** — 工具按语义能力分类，自动组合工作流提示；不可用工具不会出现在 schema、提示词或组合链路中
- **注册式轻量资源** — `Read` 通过 `ResourceRouter` 统一分发 `artifact://`、`skill://`、`rule://`、`session://` 等 scheme
- **技能系统** — 按需加载 skill 文件，不污染后续 prompt；`skill_discovery_policy` 控制发现策略

### 🔒 沙箱与安全

- **进程级沙箱** — Linux nsjail / bubblewrap（完整文件系统隔离）、macOS sandbox-exec（写入隔离）
- **CPython WASI 沙箱** — `PythonSandbox` 工具在 wasmtime + CPython WASI 中执行，WASI 级进程隔离，无网络、无 C 扩展
- **危险命令过滤** — Bash 误用拦截与安全约束，可选审批策略

### 🗃️ 持久化与状态

- **Session 持久化** — Append-only JSONL 完整历史，活跃后缀内存缓存，`--continue` 无缝恢复
- **非破坏式压缩** — 只更新 `context-state.json` 投影边界，不重写 `conversation.jsonl`；压缩统一使用 LLM 摘要
- **Plan & Todo 状态** — 确认计划按请求动态投影为 `<current-plan>`；Todo 使用稳定 ID、revision 和原子批量提交
- **Artifact 超长输出** — 工具结果超限自动落盘至 `artifacts/`，序号可恢复且禁止覆盖；`Read artifact://<id>` 读取
- **Token 用量与费用** — LLM 请求级 `usage.jsonl` journal，纳元级定价，覆盖主 Agent、自动压缩和子代理

### 🔌 集成与扩展

- **Rust 库 API** — `mink::runtime::{AgentRuntime, AgentOptions, LlmBackend, ReadOnlyFileSystem}`，完整同步/流式 turn 生命周期
- **Python SDK** — `pip install mink-agent`，内置无 TUI 的 `mink-core` 二进制，支持全参数配置
- **嵌入式只读 VFS** — 为 Read/Glob/Grep 注入数据库后端，按 `resource_session_id` 隔离多租户知识库
- **子代理（SubAgent）** — 隔离或目录级 fork 完整 session 状态，复用父 runtime 的 LLM backend，支持并发执行
- **自定义提示词** — `--mission` 加载 MISSION.md，允许覆盖白名单 core section，runtime 保留 section fail closed
- **模型别名系统** — `flash` / `pro` 内置 DeepSeek 别名，`model_aliases` 可覆盖；任意模型名未命中时原样传递

---

## 快速开始

```bash
# 前置：Rust 1.94+，设置 DEEPSEEK_API_KEY 或通过配置指定 OpenAI-compatible 端点

# 编译
cargo build --release
# 或
make build

# REPL 交互模式
./target/release/mink -m flash -i

# TUI 全屏模式
./target/release/mink -m flash --tui

# TUI 原生 scrollback 模式
./target/release/mink -m flash --tui=inline

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
mink = { package = "mink-core", version = "0.2.0", default-features = false, features = ["runtime"] }
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

同进程 `AgentRuntime` 不会自动 sandbox 当前进程。需要完整进程级沙箱时，推荐参考
`examples/web_api.rs` 的 hidden worker 模式：业务服务 spawn 自身 worker 子进程，
worker 先 re-exec 进沙箱，再调用 `mink::runtime`。

Rust 嵌入方可以继续使用默认 OpenAI-compatible backend，也可以实现
`mink::runtime::LlmBackend` 并通过 `AgentOptions::with_llm_backend()` 注入。
模型名解析仍由 mink 统一处理：`flash` / `pro` 是默认别名，`model_aliases` 可覆盖别名；
未命中别名的模型名会原样传给 backend。默认 OpenAI-compatible backend 支持
`openai_tool_choice` 和 `openai_extra_body`，可直接透传 Chat Completions 兼容端点的
扩展请求参数；`reasoning_effort`、usage 和 token 参数也有对应的 `AgentOptions` builder。
非标准协议再使用自定义 `LlmBackend`。
完整示例见 [custom_llm_backend.rs](crates/mink-core/examples/custom_llm_backend.rs)。

库使用方应只把 `mink::prelude`、`mink::runtime`、`mink::config`、`mink::sandbox`
和 `mink::sdk_protocol` 视为稳定入口；其他公开模块不承诺稳定 API。

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

## Workspace Packages

| 路径 | 职责 |
|------|------|
| [crates/mink-core](crates/mink-core/README.md) | Rust 发布包 `mink-core`，库 crate 名 `mink`，包含可嵌入 runtime、工具核心、session、sandbox 和 SDK 协议 |
| [crates/mink-cli](crates/mink-cli/README.md) | workspace 内部二进制包，生成 `mink` 终端二进制和 `mink-core` SDK 精简二进制，持有 REPL/TUI 实现 |
| [mink_agent](mink_agent/README.md) | Python SDK，wheel 内置无 TUI 的 `mink-core` 二进制 |

---

## 文档索引

| 文档 | 说明 |
|------|------|
| [使用手册](docs/USAGE.md) | 面向用户：CLI/SDK/Rust 嵌入、配置、沙箱、session、技能和常见工作流 |
| [工具参考](docs/tools.md) | 面向工具协议：内置工具参数、结果通道、资源 URL、审批和构建裁剪 |
| [架构说明](docs/ARCHITECTURE.md) | 运行时分层、模块职责、资源/能力系统、核心数据流 |
| [设计文档](docs/DESIGN.md) | 设计哲学、关键不变式、注册式资源、能力快照、运行时和库化边界 |
| [工具能力与提示词解耦](docs/设计哲学-工具能力与提示词解耦.md) | 工具 surface、语义能力、自由组合和前向求值算法 |
| [信号系统设计](docs/设计哲学-信号系统.md) | 控制论 + 贝叶斯、冷却机制、信念度展示 |
| [Agent 开发指南](AGENTS.md) | 面向 AI agent：项目结构、模块索引、开发惯例 |

---

## 许可

MIT
