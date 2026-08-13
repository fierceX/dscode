<p align="center">
  <img src="docs/assets/mink-wordmark.svg" alt="Mink" width="128">
</p>

[![Crates.io](https://img.shields.io/crates/v/mink-core.svg)](https://crates.io/crates/mink-core)
[![MIT licensed](https://img.shields.io/badge/license-MIT-blue.svg)](./LICENSE)
[![Rust 1.94+](https://img.shields.io/badge/rust-1.94%2B-blue)](https://blog.rust-lang.org/2025/06/05/Rust-1.94.0.html)
[![Python SDK](https://img.shields.io/badge/pypi-mink--agent-blue)](https://pypi.org/project/mink-agent)

**Rust 原生 · 终端优先 · 可嵌入**

Mink 是一个 Rust 实现的 **AI agent runtime**：面向终端，也面向系统。既适合在终端中直接
工作（REPL / Full TUI / Inline TUI），也适合嵌入到服务端、桌面端或内部工具中 —— CLI、
Python SDK 和 Rust 嵌入共享同一个运行时内核，不依赖子进程，语义完全一致。

---

[TOC]

---

## 快速开始

### 终端使用

```bash
# 前置：Rust 1.94+，设置 DEEPSEEK_API_KEY 或通过配置指定 OpenAI-compatible 端点

# 编译
cargo build --release        # 或 make build

# REPL 交互模式
./target/release/mink -m flash -i

# Full TUI 全屏模式
./target/release/mink -m flash --tui

# Inline TUI 原生 scrollback 模式
./target/release/mink -m flash --tui=inline

# 单次查询 / 恢复最近会话
./target/release/mink -m flash "explain this project"
./target/release/mink -m flash --continue -i

# 使用自定义系统提示词
./target/release/mink --mission ./my-task.mission.md -i
```

### Python SDK

```bash
pip install mink-agent
```

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

### Rust 嵌入

```toml
[dependencies]
mink = { package = "mink-core", version = "0.4.0", default-features = false, features = ["runtime"] }
```

```rust
use mink::prelude::{AgentOptions, AgentRuntime};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let rt = AgentRuntime::start(
        AgentOptions::new("/tmp/mink-home", ".")
            .with_api_key(std::env::var("DEEPSEEK_API_KEY")?)
            .with_model("flash"),
    ).await?;

    let outcome = rt.run_turn("hello").await?;
    println!("{}", outcome.text);

    rt.shutdown().await?;
    Ok(())
}
```

---

## 核心特点

- **可嵌入的运行时内核** — `AgentRuntime::start() → run_turn() / stream_turn() → shutdown()` 完整生命周期。CLI、REPL、TUI、Python SDK 和 Rust 嵌入共享同一套运行时语义，不需要维护多套 agent 内核。
- **长上下文与长任务可控** — 显式压缩参数 + LLM 摘要非破坏式投影 + 持久化 session 共同工作，上下文不无限膨胀，长任务可持续推进；`enabled_tools` 统一工具边界。
- **编辑与状态管理更可靠** — Anchored Edit（`Read` snapshot + `Edit.patch` 行锚定）、artifact 超长输出回读、Plan/Todo revision 原子提交和 session 恢复机制，不把正确性交给运气。

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
- **Server 与 Web** — `mink-server` 单二进制 Web 工作区服务器：REST + SSE 实时流，前端构建产物嵌入二进制，与 TUI 共享会话，浏览器里继续终端里的工作

---

## Workspace Packages
| 路径 | 职责 |
|------|------|
| [crates/mink-core](crates/mink-core/README.md) | Rust 发布包 `mink-core`，库 crate 名 `mink`，包含可嵌入 runtime、工具核心、session、sandbox 和 SDK 协议 |
| [crates/mink-cli](crates/mink-cli/README.md) | workspace 内部二进制包，生成 `mink` 终端二进制和 `mink-core` SDK 精简二进制，持有 REPL/TUI 实现 |
| [mink_agent](mink_agent/README.md) | Python SDK，wheel 内置无 TUI 的 `mink-core` 二进制 |
| [crates/mink-server](crates/mink-server/README.md) | Web 工作区服务器：REST + SSE + 嵌入前端，`build.rs` 自动构建并嵌入 web 产物 |

---

## 参考项目

| 项目 | 说明 |
|------|------|
| [oh-my-pi](https://github.com/can1357/oh-my-pi) | 开源 CLI agent（Bun/TypeScript），Edit 工具的行号锚定与快照协议参考实现 |
| [bash-agent](https://github.com/lloydzhou/bash-agent) | 终端 Agent（Bash 优先），交互与工具执行参考 |

---

## 文档索引

| 文档 | 说明 |
|------|------|
| [使用手册](docs/USAGE.md) | 面向终端用户：CLI 交互、配置、沙箱、session、工具和常见工作流 |
| [嵌入与 SDK](docs/EMBEDDING.md) | Rust 库 / Python SDK 嵌入、Token 用量与费用 |
| [机器协议](docs/PROTOCOL.md) | `--print` stream-json 与 `--agent-jsonl` 协议 |
| [工具参考](docs/tools.md) | 面向工具协议：内置工具参数、结果通道、资源 URL、审批和构建裁剪 |
| [架构说明](docs/ARCHITECTURE.md) | 运行时分层、模块职责、资源/能力系统、核心数据流 |
| [设计文档](docs/DESIGN.md) | 设计总纲与关键不变式；信号与工具能力细节见对应设计哲学文档 |
| [变更日志](CHANGELOG.md) | 版本变更记录 |
| [Server 与 Web](docs/server.md) | mink-server：REST/SSE API、嵌入构建、配置与部署 |
| [工具能力与提示词解耦](docs/设计哲学-工具能力与提示词解耦.md) | 工具 surface、语义能力、自由组合和前向求值算法 |
| [信号系统设计](docs/设计哲学-信号系统.md) | 控制论 + 贝叶斯、冷却机制、信念度展示 |
| [Agent 开发指南](AGENTS.md) | 面向 AI agent：项目结构、模块索引、开发惯例 |

---

## 许可

[MIT License](./LICENSE)
