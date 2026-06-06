# 架构说明

更新日期：2026-06-03

## 项目定位

mink 是一个 Rust 实现的轻量 AI coding agent，面向 DeepSeek / OpenAI-compatible API，优先服务终端中的编码工作流。

核心目标：

- 单二进制分发，终端优先，REPL / TUI / stream-json 三种使用形态
- Session 是一等公民，使用 JSONL 追加持久化，支持恢复、重放和压缩
- LLM 流式输出、工具执行、信号检测、决策恢复构成闭环
- 工具边界明确：超时、输出大小、写入大小、副作用和禁用开关都可控
- 工具有统一 metadata 和 approval tier，支持基础审批策略
- 超长工具输出落 session artifact，可通过 `Read artifact://<id>` 恢复
- `Read` 是轻量资源入口，支持本地文件、artifact、skill 和 session introspection
- `Edit` 支持 snapshot anchored patch，降低精确字符串替换失败率
- 上下文预算是硬约束，通过摘要压缩和 immutable prefix 尽量保留 prefix cache 命中

---

## 核心原则

- **单进程主循环**：`OrchActor` 接收命令并为每个用户输入创建 `TurnExecutor`。
- **机器协议优先**：`--print` 输出 stream-json 事件，`--agent-jsonl` 输出 single-shot Agent JSONL 事件和 `final`，便于上层编排。
- **Session 追加友好**：conversation、events、stats、summary、plan 都落在 session 目录。
- **长输出可恢复**：工具输出超过上限时写入 artifact，conversation 只保留摘要和引用。
- **读取入口统一**：`Read.path` 可读取文件和轻量 internal URL，并支持行 selector。
- **工具结果双通道**：LLM conversation 使用工具结果或工具自定义 `conv_content`，UI 通过 `ToolResultDisplay` 展示。
- **信号驱动干预**：工具失败、错误模式、编辑循环会降低 belief，并触发注入或中止。

---

## 运行时分层

```text
main.rs
  │  CLI 参数解析 -> 配置合并 -> sandbox re-exec -> Session 初始化
  │  根据模式启动 one-shot / REPL / TUI / stream-json / Agent JSONL
  ▼
OrchActor (agent/orchestrator.rs)
  │  接收用户输入、模型切换、手动 compact 命令
  │  维护 BeliefTracker 和当前强制模型
  ▼
TurnExecutor (agent/turn.rs)
  │  单轮执行器：压缩 -> LLM stream -> scavenge -> 工具 -> 信号 -> 决策
  │  组合 PrefixManager / TurnCompactor / ToolSignalProcessor /
  │  PlanActionHandler / SubAgentCoordinator
  ▼
┌─────── LLM 层 ────────┐
│ llm/client.rs         │ HTTP 流式客户端、重试、模型名解析
│ llm/transport.rs      │ OpenAI chat/completions 请求构造
│ sse/openai.rs         │ SSE 增量解析、usage、stop、tool call 合并
│ sse/toolcall.rs       │ tool_call 字段归一化
└───────────────────────┘
         │
┌─────── 工具层 ────────┐
│ tools/runner.rs       │ ToolExec registry、metadata、approval、StormBreaker、结果格式化
│ tools/metadata.rs     │ ApprovalTier、ToolResultKind、ToolMetadata
│ tools/file.rs         │ Read / Write / Edit、selector、resource、anchored patch
│ tools/snapshot.rs     │ FileSnapshotStore、行 hash 和 snapshot tag
│ tools/search.rs       │ Glob / Grep
│ tools/bash.rs         │ Bash 执行、超时、ANSI 过滤、安全检查、误用拦截
│ tools/python.rs       │ 受限 Python 执行
│ tools/web.rs          │ WebSearch / WebFetch
└───────────────────────┘
         │
┌─────── 信号与防护层 ──┐
│ guard/collector.rs    │ ToolFailed / ToolError / EditLoop 信号
│ guard/storm.rs        │ 重复工具调用抑制
│ agent/belief.rs       │ belief 滑动窗口和平滑
│ agent/decision.rs     │ Inject / Abort / cooldown / recovery guard
│ agent/signal_mode.rs  │ MINK_SIGNAL_MODE 开关
│ safety.rs             │ 危险 Bash 命令过滤
└───────────────────────┘
         │
┌─────── 持久化层 ──────┐
│ session/store.rs      │ ConversationStore JSONL、缓存、tool_result 写入
│ session/artifacts.rs  │ ArtifactManager、artifact index、完整工具输出
│ session/stats.rs      │ token、费用、请求数统计
│ session/compaction.rs │ 三级压缩、摘要生成、turn 对齐截断
│ session/prefix.rs     │ ImmutablePrefix
│ session/init.rs       │ session 目录和共享状态初始化
└───────────────────────┘
         │
┌─────── UI 层 ─────────┐
│ ui/mod.rs             │ Display trait、ToolResultDisplay、StatsSnapshot
│ ui/engine.rs          │ REPL / human 输出
│ ui/replay.rs          │ REPL session 重放
│ tui/                  │ ratatui 全屏 UI
└───────────────────────┘
```

---

## 核心数据流

### 单轮执行

```text
用户输入
  │
  ▼
OrchActor.handle_user_input()
  ├── belief.reset()
  ├── ctx.interrupt = false
  ├── resolve_active_model()
  └── TurnExecutor::execute()
       │
       ├── tools.reset_storm()
       ├── compactor.reset()
       ├── signal_processor.reset()
       ├── decision_engine.reset()
       ├── store.add_user(input)
       ├── ensure_prefix()
       │
       └── while turn < max_turns:
           ├── auto compact + preflight compact
           ├── LLM stream -> Event
           ├── scavenge thinking/text 中遗漏的工具调用
           ├── store.add_assistant()
           ├── ToolRunner::execute_all()
           ├── ToolSignalProcessor 更新 belief
           ├── PlanActionHandler 处理 PlanConfirm / PlanClear
           ├── SubAgentCoordinator 启动/收集子代理
           ├── store.add_tool_results()
           ├── Display.render_tool_result_detail()
           └── DecisionEngine.decide()
```

### 工具结果进入 LLM 与 UI

```text
ToolExec::execute()
  -> ToolOutcome { content, conversation_content, exit_code, ... }
  -> format_tool_result(tool_result_max_bytes)
       超限时写 artifacts/<id>.txt 并追加 artifact://<id>
  -> bash noise filter / Read/Write summary / Edit first-line conv content
  -> ToolRunResult
  -> ConversationStore::add_tool_results()
       使用 conv_content（若非空）否则使用 content
  -> Display::render_tool_result_detail()
```
`content` 受 `tool_result_max_bytes` 保护；`content_preview` 用于简短终端展示。LLM conversation 由 `ConversationStore::add_tool_results()` 写入，不依赖 UI preview。

### 信号系统

信号系统位于工具执行之后、下一轮 LLM 调用之前。`ToolSignalProcessor` 使用 `SignalCollector` 从工具结果中采集失败、错误模式和编辑循环信号，写入 `BeliefTracker`，再由 `DecisionEngine` 判断是否继续、注入恢复提示或中止当前 turn。

```text
ToolRunResult
  -> SignalCollector
  -> BeliefTracker
  -> DecisionEngine
  -> None / Inject / Abort
```

每个用户输入开始时会重置 belief、decision cooldown 和 StormBreaker 窗口。`MINK_SIGNAL_MODE=off` 时，信号采集、belief 更新、注入和中止逻辑都关闭。

---

## 模块职责

### 入口与配置

| 文件 | 职责 |
|------|------|
| `main.rs` | CLI 入口、sandbox re-exec、session 初始化、模式分发 |
| `config.rs` | `Config`、CLI 解析、`.minkrc` 合并、环境变量默认值、sandbox 配置 |
| `context.rs` | `AgentSharedContext` 和工具层 `ToolContext` |
| `assets.rs` | 编译期嵌入的 `tools.json` 和 skill 索引 |
| `cancel.rs` | 父子 cancellation token |
| `safety.rs` | Bash 危险命令拦截 |
| `sandbox/` | Linux nsjail/bwrap 与 macOS sandbox-exec 自举 |
| `protocol.rs` | LLM 流式 `Event` 类型 |
| `events.rs` | typed event log 类型 |
| `errors.rs` | error 分类和用户提示 |
| `util.rs` | 截断等通用工具 |

### Agent 核心

| 文件 | 职责 |
|------|------|
| `agent/orchestrator.rs` | 命令循环、模型切换、手动 compact、turn 后处理 |
| `agent/turn.rs` | 单轮执行主流程和 tool_use 内循环 |
| `agent/prefix.rs` | `PrefixManager`，构建/复用 immutable prefix |
| `agent/compactor.rs` | `TurnCompactor`，封装同轮压缩防护 |
| `agent/tool_signals.rs` | 工具信号采集和 belief 更新 |
| `agent/plan_actions.rs` | PlanConfirm / PlanClear 副作用 |
| `agent/sub_coordinator.rs` | SubAgent 工具调用的启动与结果注入 |
| `agent/sub_executor.rs` | 子代理独立 session / fork session 执行 |
| `agent/belief.rs` | `BeliefTracker` |
| `agent/decision.rs` | `DecisionEngine` |
| `agent/signal_mode.rs` | 信号系统开关 |

### 工具系统

| 文件 | 职责 |
|------|------|
| `tools/metadata.rs` | `ToolMetadata`、approval tier、结果类型、副作用标记 |
| `tools/runner.rs` | `ToolExec` trait、`TOOL_REGISTRY`、approval、并发调度、结果截断、artifact spill 和内置控制工具 |
| `tools/file.rs` | `ReadTool`、`WriteTool`、`EditTool`、selector、resource URL、anchored patch |
| `tools/snapshot.rs` | 文件 snapshot、tag、行 hash 校验 |
| `tools/search.rs` | `GlobTool`、`GrepTool` |
| `tools/bash.rs` | `BashTool`、危险命令检查、误用拦截 |
| `tools/python.rs` | `PythonTool` |
| `tools/web.rs` | `WebSearchTool`、`WebFetchTool` |
| `assets/tools.json` | 提供给模型的工具 schema |

新增工具时需要同时实现 `ToolExec::metadata()`、注册 `TOOL_REGISTRY`、更新 `assets/tools.json`，并在 metadata 中声明 approval tier、result kind、副作用、`storm_exempt`、`internal` 或 `spawns_sub_agent`。

### UI

| 文件 | 职责 |
|------|------|
| `ui/mod.rs` | `Display`、`ToolResultDisplay`、`StatsSnapshot` |
| `ui/engine.rs` | REPL 同步渲染 |
| `ui/replay.rs` | REPL replay |
| `tui/mod.rs` | TUI 入口和事件循环 |
| `tui/display.rs` | `Display` -> `TuiSignal` 适配 |
| `tui/signal.rs` | `TuiSignal` reducer |
| `tui/state.rs` | `TuiState`、消息、输入、视口、缓存、子代理状态 |
| `tui/input.rs` | 键盘、鼠标、粘贴、历史和命令输入 |
| `tui/command.rs` | slash command 解析 |
| `tui/render.rs` | 渲染 facade 和布局 |
| `tui/render/*` | content/detail/input/status 子渲染器 |
| `tui/markdown.rs` | Markdown facade |
| `tui/markdown/*` | normalize、block、inline、table、diff、types、util |
| `tui/replay.rs` | TUI replay |

---

## Display 接口

`Display` 是 REPL 与 TUI 的共享抽象。工具结果通过 `ToolResultDisplay` 传递展示字段：

```rust
pub struct ToolResultDisplay<'a> {
    pub tool_name: &'a str,
    pub content_preview: &'a str,
    pub content: &'a str,
    pub tool_use_id: Option<&'a str>,
    pub exit_code: Option<i32>,
}
```

- `tool_name`：工具名。
- `content_preview`：简短展示文本。
- `content`：工具层截断/过滤后的展示内容。
- `tool_use_id` / `exit_code`：工具结果元数据。

---

## Session 结构

```text
~/.mink/projects/<project_key>/<session_id>/
├── conversation.jsonl
├── events.jsonl
├── session.json
├── summary.txt
├── plan.md
├── plan.draft
├── stats.json
└── artifacts/
    ├── index.jsonl
    └── <tool>-0001.txt
```

`MINK_HOME` 可覆盖默认 home。`session_id` 是稳定内部目录名；`session.json` 保存用户可读的 alias、title、cwd 和时间戳。`--session NAME` 会按 alias、完整 id、id 前缀和 title 解析已有 session，匹配不到时创建新的时间戳 session 并把 NAME 规范化为安全 alias。列表和解析路径对损坏的 `session.json` 采用 legacy fallback，不让单个坏 metadata 阻断恢复。`--continue` 会选择最近修改的 session。

---

## 关键不变式

- 每个用户输入开始时重置 StormBreaker、belief、decision cooldown 和 interrupt。
- 同一用户输入的 tool_use 内循环最多压缩一次。
- `ImmutablePrefix` 变更必须通过 prefix manager / invalidate 路径。
- `ConversationStore` append 时保持内存缓存一致，不靠读盘恢复正常路径性能。
- `ToolRunner::format_tool_result()` 是工具输出进入 LLM/UI 前的最大字节保护。
- `ToolRunner::execute_all()` 在 StormBreaker 前执行 approval 检查。
- 超长工具输出必须保存为当前 session artifact，而不是丢失全文。
- `Read` 本地非 raw 输出记录 snapshot；raw 和 immutable resource 不生成可编辑 snapshot。
- `Edit.patch` 必须校验 snapshot tag 和目标行 hash，stale 时 fail closed。
- `render_tool_result_detail()` 必须保留默认实现。
- TUI 输入 cursor 必须落在 UTF-8 char boundary。
- TUI 点击目标只对应当前可见 viewport。
- 子代理详情使用稳定 `session_id`，不要回退到裸 `line_idx` 作为视图主键。
