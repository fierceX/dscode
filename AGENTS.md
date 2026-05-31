# Agents Guide

## 项目概览

dscode 是一个 Rust 实现的轻量 AI coding agent，专为 DeepSeek/OpenAI-compatible API 优化。项目目标是单二进制、低运行时依赖、终端优先。

核心能力：

- LLM 流式请求 -> 工具执行 -> 决策的内循环
- 信号驱动的信念系统：自动错误检测、注入修正、恢复首步守卫，可用 `DSCODE_SIGNAL_MODE=off` 关闭
- 上下文自适应压缩：三级 Tier，尽量保持 prefix-cache 命中
- 维修流水线：Scavenge 回收、Truncation 修复、StormBreaker 重复调用抑制
- Session 持久化：JSONL 追加写入，支持恢复和重放
- 两种终端交互模式：REPL + TUI
- 子代理：SubAgent 支持隔离上下文或 fork 当前上下文并发执行

---

## 终端交互模式

### REPL 模式（`-i`）

基于 `rustyline` 的行编辑和 `TerminalDisplay`（`src/ui/engine.rs`）的同步渲染。

工作方式：读取用户输入 -> 发送到编排器 -> `TerminalDisplay` 直接写 stdout/stderr。

- 推理内容：灰色输出
- 文本回复：写 stdout
- 工具调用：黄色 `[tool] 摘要`
- 工具结果：显示 `ToolResultDisplay.content_preview`
- 提示符：绿色 `> `
- 标题栏：通过 ANSI escape 更新模型、tokens、费用和信念度

输入处理位于 `run_interactive()`，由 `rustyline::Editor` 提供历史、行编辑和 Tab 补全。

### TUI 模式（`--tui`）

基于 `ratatui` 的事件驱动全屏界面。入口是 `run_tui()`（`src/tui/mod.rs`）。

工作方式：编排器通过 `Display` trait 输出事件，`TuiDisplay`（`src/tui/display.rs`）转为 `TuiSignal`，TUI 主循环消费 mpsc channel 并渲染。

核心能力：状态栏、消息列表、多行输入、Ctrl+C 中断、slash command、Markdown 子集渲染、长工具结果折叠、子代理详情和鼠标点击。

TUI 特有操作和行为：

- 输入区支持多行编辑，光标和删除逻辑按 UTF-8 char boundary 处理。
- Ctrl+C 在 `waiting/thinking/generating/tool/sub-agent/compacting` 等工作状态中断当前 turn；空闲状态按退出流程处理。
- `/flash`、`/pro`、`/compact`、`/help`、`/skills`、`/exit`、`/quit`、`/q` 在本地处理。未知 `/xxx` 不发送给模型；需要发送 slash 文本时在行首加空格。
- 工具结果可自动折叠。TUI 展示的是 `ToolResultDisplay.content`，仍受工具层 `tool_result_max_bytes` 保护。
- 子代理消息可通过点击进入详情页，详情以 `session_id` 查找。
- 鼠标点击只命中当前可见 viewport 中的折叠项或详情入口。

---

## Display 抽象

两种终端模式实现同一个 `Display` trait（`src/ui/mod.rs`）。

```rust
pub struct ToolResultDisplay<'a> {
    pub tool_name: &'a str,
    pub content_preview: &'a str,
    pub content: &'a str,
    pub tool_use_id: Option<&'a str>,
    pub exit_code: Option<i32>,
}

pub trait Display: Send + Sync {
    fn render_thinking(&self, content: &str);
    fn render_text(&self, content: &str);
    fn render_tool_call(&self, name: &str, summary: &str);
    fn render_tool_result(&self, tool_name: &str, content_preview: &str);
    fn render_tool_result_detail(&self, result: &ToolResultDisplay<'_>) {
        self.render_tool_result(result.tool_name, result.content_preview);
    }
    fn render_stop(&self);
    fn render_error(&self, message: &str);
    fn render_retry(&self);
    fn render_info(&self, msg: &str);
    fn render_title_update(&self, model: &str, stats: &StatsSnapshot);
    fn render_sub_agent_status(&self, session_id: &str, status: &str, in_tokens: u64, out_tokens: u64);
    fn render_sub_agent_output(
        &self,
        _session_id: &str,
        _status: &str,
        _thinking: &str,
        _text: &str,
        _in_tokens: u64,
        _out_tokens: u64,
    ) {
    }
    fn render_prompt(&self);
    fn render_clear_line(&self);
}
```

工具结果显示使用 `ToolResultDisplay`。`content` 是工具层截断/过滤后的展示内容，受 `tool_result_max_bytes` 保护；`content_preview` 用于简短终端展示。

---

## 运行时分层

```
main.rs
  │  CLI 参数解析 -> 配置合并 -> Session 初始化
  ▼
OrchActor (agent/orchestrator.rs)
  │  主循环，持有全局上下文和核心组件
  │  每个用户输入创建 TurnExecutor
  ▼
TurnExecutor (agent/turn.rs)
  │  单轮执行器：LLM 流 -> 工具 -> 决策
  │  同一输入可循环多轮 tool_use
  ▼
┌─────── LLM 层 ────────┐
│ llm/client.rs         │ HTTP 流式客户端 + 重试
│ llm/transport.rs      │ OpenAI chat/completions 请求构造
│ sse/openai.rs         │ SSE 增量解析
│ sse/toolcall.rs       │ tool_call 字段提取
└───────────────────────┘
         │
┌─────── 工具层 ────────┐
│ tools/runner.rs       │ ToolRegistry + 批量分发 + StormBreaker
│ tools/file.rs         │ Read/Write/Edit
│ tools/search.rs       │ Glob/Grep
│ tools/bash.rs         │ Bash 执行
│ tools/python.rs       │ Python 执行
│ tools/web.rs          │ WebSearch/WebFetch
└───────────────────────┘
         │
┌─────── 信号层 ────────┐
│ guard/collector.rs    │ ToolFailed/ToolError/EditLoop 信号采集
│ agent/belief.rs       │ 信念度计算
│ agent/decision.rs     │ 注入/中止决策
│ agent/signal_mode.rs  │ 信号系统开关
└───────────────────────┘
         │
┌─────── 持久化层 ──────┐
│ session/store.rs      │ JSONL ConversationStore
│ session/stats.rs      │ Token/费用统计
│ session/compaction.rs │ 三级上下文压缩
│ session/prefix.rs     │ ImmutablePrefix 缓存
│ session/init.rs       │ Session 初始化
└───────────────────────┘
         │
┌─────── UI 层 ─────────┐
│ ui/mod.rs             │ Display trait + StatsSnapshot
│ ui/engine.rs          │ REPL 同步渲染
│ tui/                  │ TUI 事件、状态、输入、渲染
│ ui/replay.rs          │ REPL session 重放
└───────────────────────┘
```

---

## 核心执行流程

### 单轮执行（TurnExecutor）

```
用户输入
  │
  ▼
OrchActor.handle_user_input()
  ├── belief.reset()
  ├── DecisionEngine reset cooldown
  ├── StormBreaker reset
  └── TurnExecutor::execute()
       │
       ▼
┌── while turn < max_turns ───────────────────────────┐
│ 1. 上下文压缩检查（同一用户输入最多一次）             │
│ 2. LLM 流式请求（SSE -> Event）                     │
│    ├── Thinking                                     │
│    ├── Text                                         │
│    ├── ToolCall                                     │
│    └── Stop                                         │
│ 3. Scavenge 回收遗漏工具调用                         │
│ 4. 持久化 assistant 消息                             │
│ 5. ToolRunner::execute_all                           │
│    ├── StormBreaker                                  │
│    ├── Truncation repair                             │
│    ├── ToolExec dispatch                             │
│    └── format_tool_result / noise filter             │
│ 6. 持久化 tool results 到 ConversationStore           │
│ 7. Display 输出工具结果                              │
│ 8. 信号采集 -> belief -> decision                    │
└──────────────────────────────────────────────────────┘
```

### 信号系统

```
工具执行完毕
       │
SignalCollector.collect()
       ├── ToolFailed：exit_code != 0 / Error 前缀 / safety blocked
       ├── ToolError：regex 匹配编译错、测试失败等
       └── EditLoop：编辑-检查循环窗口检测
       │
       ▼
BeliefTracker.observe()
       │  滑动窗口 + 拉普拉斯平滑
       ▼
DecisionEngine.decide()
       ├── B >= 0.70 -> None
       ├── B < 0.70 -> Inject + 冷却 + 恢复首步守卫
       └── B < 0.30 -> Abort
```

`DSCODE_SIGNAL_MODE=off` 时，不生成 `<belief-awareness>` prompt 段，也不执行信号采集、信念更新、注入、中止和恢复守卫。

---

## 模块索引

### 入口与配置

| 文件 | 职责 |
|------|------|
| `main.rs` | CLI 参数解析、配置合并、Session 创建、启动 REPL/TUI/print 模式 |
| `config.rs` | Config 结构体、CLI/env/配置文件合并、API key 和 sandbox 配置 |
| `context.rs` | AgentSharedContext + ToolContext |
| `assets.rs` | 嵌入 tools.json、内置 skills |
| `cancel.rs` | CancellationToken 父子传播 |
| `safety.rs` | 危险命令过滤 |
| `sandbox/` | 沙箱自举和平台实现 |
| `util.rs` | 通用工具函数 |

### Agent 核心

| 文件 | 职责 |
|------|------|
| `agent/orchestrator.rs` | 命令循环、模型切换、手动 compact、turn 后处理 |
| `agent/turn.rs` | 单轮执行器、工具循环、Display 输出 |
| `agent/compactor.rs` | turn 内压缩封装和同轮压缩防护 |
| `agent/plan_actions.rs` | PlanConfirm / PlanClear 副作用处理 |
| `agent/belief.rs` | 信念度追踪 |
| `agent/decision.rs` | 注入/中止决策 |
| `agent/signal_mode.rs` | 信号模式开关 |
| `agent/tool_signals.rs` | 工具信号处理 |
| `agent/sub_coordinator.rs` | 子代理启动、并发限制、结果收集 |
| `agent/sub_executor.rs` | 子代理执行 |
| `agent/prefix.rs` | agent 层 prefix manager |

### 工具系统

| 文件 | 职责 |
|------|------|
| `tools/runner.rs` | ToolExec registry、批量分发、结果格式化、TodoWrite/Skill/Plan/SubAgent tools |
| `tools/file.rs` | Read/Write/Edit |
| `tools/search.rs` | Glob/Grep |
| `tools/bash.rs` | Bash |
| `tools/python.rs` | Python |
| `tools/web.rs` | WebSearch/WebFetch |

### UI

| 文件 | 职责 |
|------|------|
| `ui/mod.rs` | Display trait、ToolResultDisplay、StatsSnapshot |
| `ui/engine.rs` | REPL 同步渲染 |
| `ui/replay.rs` | REPL session 重放 |
| `tui/mod.rs` | TUI 入口和事件循环 |
| `tui/display.rs` | Display 到 TuiSignal 的适配 |
| `tui/state.rs` | TUI 消息、输入、视口、缓存、子代理状态 |
| `tui/input.rs` | TUI 输入、快捷键、鼠标、slash command |
| `tui/render/*` | TUI 内容区、详情页、输入区、状态栏渲染 |
| `tui/markdown/*` | TUI Markdown 子集渲染 |

### Session 与协议

| 文件 | 职责 |
|------|------|
| `session/store.rs` | ConversationStore JSONL |
| `session/stats.rs` | token 和费用统计 |
| `session/compaction.rs` | 三级压缩 |
| `session/prefix.rs` | ImmutablePrefix |
| `session/paths.rs` | session 路径 |
| `session/init.rs` | session 初始化 |
| `protocol.rs` | LLM stream Event 类型 |
| `events.rs` | 结构化事件日志 |
| `repair/scavenge.rs` | 工具调用回收和 JSON 修复 |
| `prompt.rs` | system prompt 构建 |
| `errors.rs` | 错误分类 |

---

## 关键不变式

- `TurnCompactor`：同一用户输入的内循环最多压缩一次上下文
- `ImmutablePrefix`：system prompt/tools 变更必须 invalidate prefix
- `ConversationStore` 写操作不把内存缓存设为 None
- `StormBreaker` 每个新用户输入重置
- `BeliefTracker` 每个新用户输入 reset，初始信念为 0.75
- `DecisionEngine` 每个新用户输入 reset cooldown
- `ToolRunner::format_tool_result()` 是工具输出进入 LLM/UI 前的统一最大字节保护
- `TurnExecutor` 写入 LLM conversation 使用 `conv_content`，为空时使用 `content`
- `Display::render_tool_result_detail()` 必须保持默认实现。
- TUI 光标必须始终落在 UTF-8 char boundary。

---

## 新增工具步骤

1. 在 `tools/*.rs` 中实现 `ToolExec` 或辅助函数。
2. 在 `tools/runner.rs` 的 `TOOL_REGISTRY` 中注册工具。
3. 在 `src/assets/tools.json` 中添加 schema。
4. 如果工具有副作用，实现 `mutating()`；如果应跳过风暴检测，实现 `storm_exempt()`。
5. 若工具需要压缩给 LLM 的内容，设置 `ToolOutcome.conversation_content`。
6. 添加单元测试，包括错误路径、截断、信号和安全边界。

---

## 开发提示

```bash
cargo build
cargo build --release
cargo test
cargo test tui
cargo clippy --all-targets
make build
make check
make test
```

调试：

```bash
# 查看信念变化
grep '"belief"' events.jsonl | jq '{type, belief}'

# 查看注入历史
grep '"Injecting hint"' events.jsonl

# stream-json 模式
./target/release/dscode --print "..."

# TUI 模式
./target/release/dscode --tui
```

---

## 文档索引

| 文档 | 说明 |
|------|------|
| `docs/ARCHITECTURE.md` | 运行时分层、模块职责、核心数据流 |
| `docs/DESIGN.md` | 设计哲学：执行循环、内存、压缩、维修、信号、工具 |
| `docs/USAGE.md` | CLI 参数、环境变量、会话管理、工具参考 |
| `docs/tools.md` | 内置工具参数与行为 |
| `docs/设计哲学-信号系统.md` | 信号系统完整设计 |
| `docs/TUI_OPTIMIZATION_ROADMAP.md` | TUI 当前实现和维护建议 |
| `docs/TUI_MARKDOWN_RENDERING_DESIGN.md` | TUI Markdown 渲染说明 |
| `docs/TUI_CURRENT_STAGE_REVIEW_AND_NEXT_OPTIMIZATION_PLAN.md` | TUI 质量说明和后续建议 |

---

*本文件面向 AI code agent，帮助快速理解当前项目结构、运行时不变式和开发惯例。*
