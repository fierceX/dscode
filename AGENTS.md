# Agents Guide

## 项目概览

dscode 是一个 Rust 实现的轻量 AI coding agent，专为 DeepSeek 优化。单二进制，零运行时依赖。

核心能力：
- LLM 流式请求 → 工具执行 → 决策的内循环
- 信号驱动的信念系统（自动错误检测 + 注入修正 + 恢复首步守卫，可用 `DSCODE_SIGNAL_MODE=off` 关闭）
- 上下文自适应压缩（三级 Tier，最大化 prefix-cache 命中率）
- 维修流水线（Scavenge 回收 → Truncation 修复 → Storm Breaker 抑制）
- Session 持久化（JSONL，天然追加友好）
- 两种终端交互模式（REPL + TUI）
- 子代理（SubAgent，隔离或 fork 上下文并发执行）

---

## 两种终端交互模式

### REPL 模式（`-i`）

基于 `rustyline` 的行编辑 + `TerminalDisplay`（`src/ui/engine.rs`）的同步渲染。

**工作方式**：读取用户输入 → 发送到编排器 → `TerminalDisplay` 直接将输出写入 stderr。
- 推理内容：灰色（`\x1b[90m`）
- 文本回复：直接输出到 stdout
- 工具调用：黄色 `[tool] 摘要` 到 stderr
- 提示符：绿色 `> ` 到 stderr（`render_prompt()`）
- 标题栏：通过 ANSI escape `\x1b]0;...\x07` 更新终端窗口标题

**输入处理**（`run_interactive()` in `main.rs`）：`rustyline::Editor` 提供历史、行编辑、Tab 补全。

### TUI 模式（`--tui`）

基于 `ratatui` + `TuiDisplay`（`src/tui/mod.rs`）的事件驱动全屏界面。

**工作方式**：编排器通过 mpsc channel 发送信号 → `TuiDisplay` 转发 → TUI 事件循环渲染。
- 完整的终端界面（alternate screen）
- 状态栏（模型名、信念度、tokens、费用、工作状态）
- 消息列表（thinking/text/tool_call/tool_result）
- 输入区域
- 实时流式内容渲染

**TUI 入口**：`run_tui()` 消费 `TuiSignal` 事件流，驱动 ratatui `Frame` 渲染。

### 共用抽象

两种模式实现同一个 `Display` trait（`src/ui/mod.rs`）：

```rust
pub trait Display: Send + Sync {
    fn render_thinking(&self, content: &str);
    fn render_text(&self, content: &str);
    fn render_tool_call(&self, name: &str, summary: &str);
    fn render_tool_result(&self, tool_name: &str, content_preview: &str);
    fn render_stop(&self);
    fn render_error(&self, message: &str);
    fn render_retry(&self);
    fn render_info(&self, msg: &str);
    fn render_title_update(&self, model: &str, stats: &StatsSnapshot);
    fn render_sub_agent_status(&self, session_id: &str, status: &str, in_tokens: u64, out_tokens: u64);
    fn render_prompt(&self);
    fn render_clear_line(&self);
}
```

选择逻辑在 `main.rs`：`--tui` 标志创建 `TuiDisplay`，否则创建 `TerminalDisplay`。

---

## 运行时分层

```
main.rs
  │  CLI 参数解析 → 配置合并 → Session 初始化
  ▼
OrchActor (agent/orchestrator.rs)
  │  主循环，持有全局上下文和核心组件
  │  每用户输入创建 TurnExecutor 执行
  ▼
TurnExecutor (agent/turn.rs)
  │  单轮执行器：LLM 流 → 工具 → 决策
  │  同一输入可循环多轮（tool_use 循环）
  ▼
┌─────── LLM 层 ────────┐
│ llm/client.rs         │  HTTP 流式客户端 + 重试
│ llm/transport.rs      │  OpenAI API 请求构造
│ sse/openai.rs         │  增量 SSE 解析 → Event 流
└───────────────────────┘
         │
┌─────── 工具层 ─────────┐
│ tools/runner.rs        │  批量分发 + StormBreaker
│ tools/file.rs          │  Read/Write/Edit/Glob/Grep
│ tools/bash.rs          │  Bash 执行（超时 + 安全过滤）
│ tools/web.rs           │  WebSearch/WebFetch
└────────────────────────┘
         │
┌─────── 信号层 ─────────┐
│ guard/collector.rs     │  信号采集（ToolFailed/Error/EditLoop）
│ agent/belief.rs        │  信念度计算（拉普拉斯平滑）
│ agent/decision.rs      │  决策（注入/中止，含冷却）
│ agent/signal_mode.rs   │  信号系统开关
└────────────────────────┘
         │
┌─────── 持久化层 ───────┐
│ session/store.rs       │  ConversationStore（JSONL）
│ session/stats.rs       │  Token 用量统计
│ session/compaction.rs  │  三级上下文压缩
└────────────────────────┘
         │
┌─────── UI 层 ─────────┐
│ ui/engine.rs           │  TerminalDisplay（REPL 同步渲染）
│ tui/mod.rs             │  TuiDisplay + TUI 框架（ratatui）
│ ui/mod.rs              │  Display trait + StatsSnapshot
│ ui/replay.rs           │  Session 重放
└────────────────────────┘
```

---

## 核心执行流程

### 单轮执行（TurnExecutor）

```
用户输入
  │
  ▼
OrchActor.handle_user_input()
  ├── belief.reset()            ← 新轮开始，信念窗口重置
  ├── TurnExecutor::execute()
  │
  ▼
┌─── while turn < max_turns ──────────────────────────┐
│  1. 上下文压缩检查（同轮最多一次）                    │
│  2. LLM 流式请求（SSE → Event 流）                  │
│     ├── Thinking（推理内容）                         │
│     ├── Text（文本回复）                             │
│     ├── ToolCall（工具调用声明）                     │
│     └── Stop（end_turn / tool_use）                  │
│                                                      │
│  3. Scavenge 回收（从 thinking/text 提取遗漏的调用）  │
│  4. 持久化 assistant 消息                            │
│                                                      │
│  5. 工具执行（ToolRunner::execute_all）              │
│     ├── StormBreaker 检查（重复调用抑制）             │
│     ├── Truncation 修复（截断 JSON 补全）            │
│     └── 每工具调用: signal → belief                  │
│                                                      │
│  6. 决策（DecisionEngine）                           │
│     ├── B ≥ 0.70 → 继续                              │
│     ├── B < 0.70 → 注入 System note + 激活冷却 + 恢复首步守卫 │
│     ├── B < 0.30 → Abort（中止本轮）                 │
│     └── stop == "tool_use" → 继续循环                │
└──────────────────────────────────────────────────────┘
  │
  ▼
返回 TurnDecision（Stop/Interrupted/Failed）
```

### 信号系统流程

```
工具执行完毕
       │
SignalCollector.collect()
       ├── ToolFailed  — exit_code ≠ 0 / "Error:" 前缀
       ├── ToolError   — regex 匹配（编译错/测试失败等）
       └── EditLoop    — W=6 窗口检测编辑-检查循环
       │
       ▼
BeliefTracker.observe()
       │  max(severity) 合并多信号
       │  拉普拉斯平滑 α=3+Σs, β=1+Σf
       │  滑动窗口 W=16
       ▼
DecisionEngine.decide()
       ├── B ≥ 0.70 → None
       ├── B < 0.70 → Inject（冷却 3 轮后恢复）+ 恢复首步守卫
       └── B < 0.30 → Abort
```

设置 `DSCODE_SIGNAL_MODE=off` 时，不生成 `<belief-awareness>` 系统提示词段，也不执行信号采集、信念更新、注入、中止和恢复守卫。

详见 [`docs/设计哲学-信号系统.md`](docs/设计哲学-信号系统.md)。

---

## 模块索引

### 入口与配置

| 文件 | 职责 |
|------|------|
| `main.rs` | CLI 参数解析 → 配置合并 → Session 创建 → 启动 Orchestrator |
| `config.rs` | Config 结构体、CLI/env/配置文件三级合并、API key 解析 |
| `context.rs` | AgentSharedContext（全局共享状态） + ToolContext（工具层上下文） |
| `assets.rs` | 嵌入的 tools.json 定义、内置 skill 列表 |
| `cancel.rs` | CancellationToken 父子传播与取消协作 |
| `safety.rs` | 危险命令黑名单过滤（rm -rf /、sudo、shutdown 等） |
| `util.rs` | 通用工具函数（truncate_str 等） |

### Agent 核心

| 文件 | 职责 |
|------|------|
| `agent/orchestrator.rs` | 主循环：新用户输入 → 创建 TurnExecutor → 收集效果（子代理/计划变更） |
| `agent/turn.rs` | 单轮执行器：LLM 流 → 持久化 → 工具执行 → 决策，含 tool_use 内循环 |
| `agent/belief.rs` | BeliefTracker：信号合并 → 拉普拉斯平滑 → B ∈ [0,1] |
| `agent/decision.rs` | DecisionEngine：阈值判断 + 注入内容格式化 + 冷却计数器管理 |
| `agent/signal_mode.rs` | SignalMode：读取 `DSCODE_SIGNAL_MODE`，控制信号系统开关 |
| `agent/sub_pool.rs` | 子代理并发池（tokio::sync::Semaphore 限流） |
| `agent/sub_executor.rs` | 子代理独立 session 创建、fork 模式、结果收集 |

### 信号与防护

| 文件 | 职责 |
|------|------|
| `guard/collector.rs` | SignalCollector：退出码检测 + regex 错误匹配 + EditLoop 序列检测 |
| `guard/storm.rs` | StormBreaker：滑动窗口检测重复 (tool, args) 调用并抑制 |

### LLM 通信

| 文件 | 职责 |
|------|------|
| `llm/client.rs` | HTTP 流式客户端 + 指数退避重试 + 模型名解析 |
| `llm/transport.rs` | OpenAI chat/completions 请求体构造（含缓存控制） |
| `sse/openai.rs` | SSE 增量解析器：跨 chunk 合并 tool_call、提取 thinking/usage |
| `sse/toolcall.rs` | SSE tool_call 字段提取 |

### 工具系统

| 文件 | 职责 |
|------|------|
| `tools/runner.rs` | 工具批量分发器 + StormBreaker 嵌入 + truncation 修复 |
| `tools/file.rs` | Read（offset/limit）、Write、Edit（diff）、Glob、Grep |
| `tools/bash.rs` | Bash 命令执行（timeout + output truncation + ANSI 过滤） |
| `tools/web.rs` | WebSearch（Tavily） + WebFetch（HTTP GET） |

### Session 与持久化

| 文件 | 职责 |
|------|------|
| `session/store.rs` | ConversationStore：JSONL 追加写入 + 延迟加载缓存 + trim 截断 |
| `session/stats.rs` | Token 用量统计 + 费用估算 + JSON 持久化 |
| `session/compaction.rs` | 三级压缩引擎（Conservative/Aggressive/Emergency）+ turn 对齐截断 + 摘要生成 |
| `session/prefix.rs` | ImmutablePrefix：system prompt + tools 缓存 + fingerprint 校验 |
| `session/paths.rs` | Session 目录路径计算（project_key 转义） |
| `session/init.rs` | 共享 Session 初始化（主进程 + 子代理共用） |

### UI（两种交互模式）

| 文件 | 职责 |
|------|------|
| `ui/mod.rs` | `Display` trait 定义 + `StatsSnapshot`（信念度/tokens/费用等统计结构） |
| `ui/engine.rs` | `TerminalDisplay`：REPL 模式的同步渲染器（stderr 写 thinking/text/tool 调用 + ANSI 标题栏） |
| `tui/mod.rs` | `TuiDisplay` + TUI 事件循环：ratatui 全屏终端界面（状态栏、消息列表、输入区） |
| `ui/replay.rs` | Session 历史事件重放（恢复时渲染最近对话） |

### 维修与协议

| 文件 | 职责 |
|------|------|
| `repair/scavenge.rs` | DSML/XML/JSON/bracket 五种格式的工具调用回收 + JSON 截断修复 |
| `protocol.rs` | Event enum（Text/Thinking/ToolCall/Usage/Stop/Error/Retry/SelfReport） |
| `prompt.rs` | System prompt 按序构建器（11+ 个 `<section>` 段，信号开启时含 belief-awareness） |
| `errors.rs` | ErrorCategory 分类（Network/Auth/RateLimit/Parse/Tool/Internal） |

---

## 开发提示

### 编译与测试

```bash
cargo build            # Debug 编译
cargo build --release  # Release 编译
cargo test             # 全部单元测试（188+）
cargo test <name>      # 单个测试
make build             # Release 编译
make check             # Type check
make test              # 测试
```

### 关键不变式

- `compacted_this_turn`：同一用户输入的同轮循环中最多压缩一次上下文
- `ImmutablePrefix`：system prompt 变更必须通过 `invalidate_prefix()`，禁止直接修改
- `store` 写操作不将内存缓存设为 None（读盘性能）
- `StormBreaker` 窗口每新用户输入重置（跨轮不携带旧窗口）
- `BeliefTracker` 每新用户输入 reset（信念从 0.75 重新开始）
- `DecisionEngine` 每新用户输入 reset 冷却（新轮新开始）

### 新增工具的步骤

1. 在 `tools/*.rs` 中实现工具函数（同步函数，返回 `Result<String>`）
2. 在 `tools/mod.rs` 的 `execute_one_sync()` 中添加分发分支
3. 在 `assets.rs` 的 `TOOLS_JSON` 中添加工具定义（name/description/parameters）
4. 如果有副作用，考虑 StormBreaker 豁免（修改 `StormExempt` 集合）
5. 添加测试，包括错误路径和截断场景

### 调试手段

```bash
# 查看信念变化
grep '"belief"' events.jsonl | jq '{type, belief}'

# 查看注入历史
grep '"Injecting hint"' events.jsonl

# stream-json 模式（观察结构化事件）
./target/release/dscode --print "..."

# TUI 模式
./target/release/dscode --tui
```

---

## 文档索引

| 文档 | 说明 |
|------|------|
| [`docs/设计哲学-信号系统.md`](docs/设计哲学-信号系统.md) | 信号系统完整设计：控制论 + 贝叶斯、冷却机制、信念度展示 |
| [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md) | 运行时分层、模块职责、核心数据流 |
| [`docs/DESIGN.md`](docs/DESIGN.md) | 14 个主题的设计哲学：执行循环、内存模型、压缩、维修、信号、工具等 |
| [`docs/USAGE.md`](docs/USAGE.md) | CLI 参数、环境变量、会话管理、工具参考 |
| [`docs/tools.md`](docs/tools.md) | 内置工具参数与行为 |

---

*本文件面向 AI code agent，帮助快速理解项目结构、代码模块和开发惯例。*
