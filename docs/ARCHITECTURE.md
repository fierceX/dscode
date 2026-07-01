# 架构说明

更新日期：2026-07-01

本文描述 mink 当前代码结构、模块职责和运行时数据流。面向用户的命令、配置和工作流见
[USAGE.md](USAGE.md)；完整工具协议见 [tools.md](tools.md)；设计取舍和不变式见
[DESIGN.md](DESIGN.md)。

## 项目定位

mink 是一个 Rust 实现的轻量 AI coding agent，默认面向 DeepSeek / OpenAI-compatible API，优先服务终端中的编码工作流；作为 Rust 库嵌入时也可注入自定义 LLM backend。

核心目标：

- 单二进制分发，终端优先，REPL / TUI / stream-json 三种使用形态
- 可作为 Rust 库嵌入：Rust 发布包名为 `mink-core`，库 crate 名为 `mink`，`mink::runtime` / `mink::prelude` 提供同进程调用
- LLM backend 可注入：默认 OpenAI-compatible streaming backend，宿主可替换为私有模型、内网网关或厂商 SDK
- Session 是一等公民，使用 JSONL 追加持久化，支持恢复、重放和压缩
- LLM 流式输出、工具执行、信号检测、决策恢复构成闭环
- 工具边界明确：超时、输出大小、写入大小、副作用和禁用开关都可控
- 工具有统一 metadata 和 approval tier，支持基础审批策略
- 超长工具输出落 session artifact，可通过 `Read artifact://<id>` 恢复
- `Read` 是轻量资源入口，支持本地文件、artifact、skill、rule 和 session introspection
- registered resource 与 capability snapshot 分离：资源读取走 `ResourceRouter`，prompt/skill/rule/context 能力视图走 `CapabilitySnapshot`
- `Edit` 支持 snapshot anchored patch，降低精确字符串替换失败率
- 上下文预算是硬约束，通过摘要压缩和 immutable prefix 尽量保留 prefix cache 命中

---

## 核心原则

- **单进程主循环**：`OrchActor` 接收命令并为每个用户输入创建 `TurnExecutor`。
- **机器协议优先**：`--print` 输出 stream-json 事件并以 `final` 收尾，`--agent-jsonl` 输出 single-shot Agent JSONL 事件和 `final`；request 可用 `options.stream_events=false` 关闭过程事件，仅保留最终 `final`，便于上层同时支持流式和非流式任务。
- **Session 追加友好**：conversation、events、stats、summary、plan 都落在 session 目录。
- **长输出可恢复**：工具输出超过上限时写入 artifact，conversation 只保留摘要和引用。
- **读取入口统一**：`Read.path` 可读取文件和轻量 internal URL，并支持行 selector。
- **工具结果双通道**：LLM conversation 使用工具结果或工具自定义 `conv_content`，UI 通过 `ToolResultDisplay` 展示。
- **信号驱动干预**：工具失败、错误模式、编辑循环会降低 belief，并触发注入或中止。

---

## 运行时分层

```text
crates/mink-cli/src/main.rs         ← mink binary thin wrapper
crates/mink-cli/src/bin/mink-core.rs← mink-core binary thin wrapper
crates/mink-cli/src/cli.rs          ← mink / mink-core 共用 CLI adapter
  │
  │  CLI 参数解析 -> 配置合并 -> sandbox re-exec
  │  根据模式启动 one-shot / REPL / TUI / stream-json / Agent JSONL
  │  调用 mink::runtime 构造 AgentRuntime
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
│ llm/client.rs         │ LlmBackend 注入、OpenAI-compatible 流式客户端、重试、usage 采集、模型名解析
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
│ tools/vfs.rs          │ Read / Glob / Grep 的同步只读 VFS hook、请求/结果协议和格式化
│ tools/bash.rs         │ Bash 执行、超时、ANSI 过滤、安全检查、误用拦截
│ tools/python.rs       │ 宿主 Python 执行
│ tools/sandbox_python.rs│ WASI CPython 沙箱执行（python-sandbox feature）
│ tools/web.rs          │ WebSearch / WebFetch
└───────────────────────┘
         │
┌─────── 资源与能力层 ───┐
│ resources/router.rs   │ ResourceRouter、ResourceHandler、scheme 注册和分发
│ resources/{artifact,skill,rule,session}.rs │ Read 轻量资源 handler
│ capabilities/mod.rs   │ CapabilitySnapshot，汇总 skills/context files/rules
│ capabilities/skills.rs│ SkillProvider、SkillSnapshot、runtime/filesystem/built-in skills
│ capabilities/{context_files,rules}.rs │ instruction files / rules snapshot
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
│ session/usage.rs      │ LLM 请求级 Token 与费用明细 JSONL（UsageJournal / MeteredStream）
│ session/compaction.rs │ 三级压缩、摘要生成、turn 对齐截断
│ session/prefix.rs     │ ImmutablePrefix
│ session/init.rs       │ session 目录和共享状态初始化
└───────────────────────┘
         │
┌─────── UI 层 ─────────┐
│ crates/mink-core/src/ui/mod.rs │ Display trait、ToolResultDisplay、StatsSnapshot
│ crates/mink-cli/src/ui/engine.rs │ REPL / human 输出
│ crates/mink-cli/src/ui/replay.rs │ REPL session 重放
│ crates/mink-cli/src/tui/         │ ratatui 全屏 UI
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
           ├── LLM stream -> Event （MeteredStream 采集 usage → usage.jsonl）
           ├── scavenge thinking/text 中遗漏的工具调用
           ├── store.add_assistant()
           ├── ToolRunner::execute_all()
           ├── ToolSignalProcessor 更新 belief
           ├── PlanActionHandler 处理 PlanConfirm / PlanClear
           ├── SubAgentCoordinator 启动/收集子代理
           ├── store.add_tool_results()
           ├── Display.render_tool_result_detail()
           ├── SignalCollector → BeliefTracker → DecisionEngine
           └── 循环结束 → OrchActor::finish_usage() 汇总 billing_turn_id → TurnOutcome
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
| `crates/mink-cli/src/cli.rs` | **mink / mink-core 共用 CLI adapter**，参数解析、配置合并、sandbox re-exec、模式分发；调用 `mink::runtime`，但 REPL/TUI 实现归属 CLI crate |
| `crates/mink-cli/src/main.rs` | `mink` binary thin wrapper → `mink_cli::cli::main_entry()` |
| `crates/mink-cli/src/bin/mink-core.rs` | `mink-core` binary thin wrapper → `mink_cli::cli::main_entry()` |
| `config.rs` | `Config`、CLI 解析、`.minkrc` 合并、环境变量默认值、sandbox 配置 |
| `context.rs` | `AgentSharedContext` 和工具层 `ToolContext` |
| `assets.rs` | 编译期嵌入的 `tools.json` 和 skill 索引 |
| `capabilities/` | model-visible 能力 snapshot：skills、instruction files、rules，以及 source/exposure 元数据 |
| `resources/` | `Read` 轻量资源 URL 的注册式 router 和内置 handler |
| `cancel.rs` | 父子 cancellation token |
| `safety.rs` | Bash 危险命令拦截 |
| `sandbox/` | Linux nsjail/bwrap 与 macOS sandbox-exec 自举 |
| `protocol.rs` | LLM 流式 `Event` 类型 |
| `events.rs` | typed event log 类型 |
| `errors.rs` | error 分类和用户提示 |
| `util.rs` | 截断等通用工具 |

### Rust 库门面

| 文件 | 职责 |
|------|------|
| `runtime/mod.rs` | `mink::runtime` 公共 API 导出，供 `mink::prelude` facade 复用 |
| `runtime/builder.rs` | `build_runtime()` — 从 `AgentRuntimeConfig` 构造完整 runtime |
| `runtime/config.rs` | `AgentRuntimeConfig` / `SessionPolicy` / `SessionInfo` |
| `runtime/handle.rs` | `AgentRuntime` — `start()`, `run_turn()`, `try_stream_turn()`, `stream_turn()`, `shutdown()` |
| `runtime/options.rs` | `AgentOptions` ergonomic builder，包括 LLM backend、只读 VFS 和 resource session scope 注入 |
| `runtime/events.rs` | `AgentEvent` 枚举 / `EventSink` trait / `EventDisplay` adapter |
| `runtime/sdk_adapter.rs` | SDK option 映射、status/exit code 映射、`SdkFinal` 组装 |

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
| `tools/runner.rs` | `ToolExec` trait、`TOOL_REGISTRY`、enabled/disabled gate、approval、并发调度、结果截断、artifact spill 和内置控制工具 |
| `tools/file.rs` | `ReadTool`、`WriteTool`、`EditTool`、selector、resource URL、anchored patch |
| `tools/snapshot.rs` | 文件 snapshot、tag、行 hash 校验 |
| `tools/search.rs` | `GlobTool`、`GrepTool` |
| `tools/vfs.rs` | `ReadOnlyFileSystem`、`VfsScope`、结构化请求/结果、虚拟路径规范化、请求校验和结果格式化 |
| `tools/bash.rs` | `BashTool`、危险命令检查、误用拦截 |
| `tools/python.rs` | `PythonTool` |
| `tools/web.rs` | `WebSearchTool`、`WebFetchTool` |
| `assets/tools.json` | 提供给模型的工具 schema |

新增工具时需要同时实现 `ToolExec::metadata()`、注册 `TOOL_REGISTRY`、更新 `assets/tools.json`，并在 metadata 中声明 approval tier、result kind、副作用、`storm_exempt`、`internal` 或 `spawns_sub_agent`。

### Registered Resources and Capabilities

`Read` 负责承载两类读取：普通路径和轻量资源 URL。普通路径保持本地文件/VFS 后端语义；轻量资源 URL 由 `ResourceRouter` 按 scheme 分发。`tools/file.rs` 只承担读取入口和 selector 处理，具体 resource 协议由 handler 表达。

```text
Read.path
  ├── registered scheme://... -> ResourceRouter -> ResourceHandler
  ├── http(s)://...           -> URL artifact cache
  ├── ordinary path + VFS     -> ReadOnlyFileSystem
  └── ordinary path           -> local filesystem + editable snapshot
```

内置 handler 当前覆盖：

| Scheme | 来源 | 说明 |
|--------|------|------|
| `artifact://` | session artifacts | 读取被截断工具输出或 URL cache 正文 |
| `skill://` | `CapabilitySnapshot.skills` | 列出/读取当前 capability snapshot 中的 skill |
| `rule://` | `CapabilitySnapshot.rules` | 列出/读取当前 capability snapshot 中的 rule |
| `session://` | current session files | 读取当前 session 摘要、stats、messages、artifacts |

`CapabilitySnapshot` 是 prompt、selected skills、`skill://`、`rule://` 和 prefix fingerprint 的共享能力视图。它在 runtime 构建时由 provider 生成，并挂到 `AgentSharedContext`；子代理继承父代理的 snapshot，避免主代理和子代理看到不同能力集合。能力 snapshot 只描述可被模型看见或按名读取的内容，不负责工具执行、文件编辑或 VFS 数据访问。

这套拆分的边界是：

- `resources/*` 只做 URL 到文本资源的适配，不重新发现 skill/rule。
- `capabilities/*` 只构建能力视图和依赖 fingerprint，不处理 `Read` selector。
- `prompt.rs` 只消费 snapshot，不扫描文件系统。
- `tools/file.rs` 只选择读取后端并应用 selector，不内联每个 resource 的业务逻辑。

### Read-only VFS hook

VFS 不是新增工具，也不改变模型可见的工具 schema。它只是嵌入式 runtime 对普通文件路径读取后端的可选替换：

```text
Read / Glob / Grep
  ├── read_only_fs == None
  │     └── 原有本地文件实现，执行路径和 snapshot 行为不变
  └── read_only_fs == Some(vfs)
        └── ReadOnlyFileSystem
              ├── VfsScope { resource_session_id, agent_session_id }
              ├── 同步 read / glob / grep
              └── 结构化结果 -> mink-core 统一格式化
```

注入链路为 `AgentOptions` / `AgentRuntimeConfig` → runtime builder → `AgentSharedContext` → `ToolContext`。未显式设置 `resource_session_id` 时使用当前 runtime session id。子代理复用同一个 `Arc<dyn ReadOnlyFileSystem>`，继承父代理的 `resource_session_id`，但使用自己的 `agent_session_id`，从而共享同一知识库作用域并保留调用方身份。

VFS 只接管普通路径。`artifact://`、`skill://`、`rule://`、`session://` 和 `http(s)://` 仍先走资源读取路径。虚拟路径使用 POSIX 分隔符和词法规范化，拒绝越过虚拟根目录。Glob/regex 请求由工具层先校验，后端返回结构化路径或匹配行，`mink-core` 统一输出格式和 100KB 搜索输出保护。请求中的 `max_files` / `max_results` 是后端契约，后端必须自行遵守；核心不提供第二套 VFS 搜索实现。

虚拟文件是只读资源，不创建 anchored Edit snapshot。具体数据库适配不进入核心依赖；`crates/mink-core/examples/redb_vfs.rs` 展示了按 `resource_session_id` 分区、惰性范围扫描的 redb 后端。

### UI

| 文件 | 职责 |
|------|------|
| `crates/mink-core/src/ui/mod.rs` | `Display`、`ToolResultDisplay`、`StatsSnapshot`，只保留协议层抽象 |
| `crates/mink-cli/src/ui/engine.rs` | REPL 同步渲染 |
| `crates/mink-cli/src/ui/replay.rs` | REPL replay |
| `crates/mink-cli/src/tui/mod.rs` | TUI 入口和事件循环 |
| `crates/mink-cli/src/tui/display.rs` | `Display` -> `TuiSignal` 适配 |
| `crates/mink-cli/src/tui/signal.rs` | `TuiSignal` reducer |
| `crates/mink-cli/src/tui/state.rs` | `TuiState`、消息、输入、视口、缓存、子代理状态 |
| `crates/mink-cli/src/tui/input.rs` | 键盘、鼠标、粘贴、历史和命令输入 |
| `crates/mink-cli/src/tui/command.rs` | slash command 解析 |
| `crates/mink-cli/src/tui/render.rs` | 渲染 facade 和布局 |
| `crates/mink-cli/src/tui/render/*` | content/detail/input/status 子渲染器 |
| `crates/mink-cli/src/tui/markdown.rs` | Markdown facade |
| `crates/mink-cli/src/tui/markdown/*` | normalize、block、inline、table、diff、types、util |
| `crates/mink-cli/src/tui/replay.rs` | TUI replay |

---

## Display 接口

`Display` 是 runtime 与具体输出实现之间的共享抽象。`mink-core` 只定义 trait 和展示数据结构；
REPL/TUI 的具体实现位于 `mink-cli`。工具结果通过 `ToolResultDisplay` 传递展示字段：

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

Session 文件固定包含 conversation、events、metadata、summary、plan、stats 和 artifacts；差异只在
session 根目录如何由 `home`、`cwd`、`session_id` 推导。当前有四种 layout：

| Layout | `home` 含义 | session 目录 |
|--------|-------------|--------------|
| `project` / `ProjectScoped` | 用户或服务根目录 | `home/.mink/projects/<project_key(cwd)>/<session_id>/` |
| `home` / `HomeScoped` | 用户或服务根目录 | `home/.mink/sessions/<session_id>/` |
| `direct` / `Direct` | mink session 集合根目录 | `home/<session_id>/` |
| `isolated` / `Isolated` | 当前 session 根目录 | `home/` |

默认入口：

- `mink` 和裸 `mink-core --agent-jsonl` 使用 `project`，保持历史 CLI 行为。
- Python SDK 默认使用 `home`，适合同一个 SDK home 下管理多个 session。
- Rust 嵌入式 `AgentOptions` 默认使用 `isolated`，适合外层服务已经按任务/session 创建独立目录。
- `direct` 适合服务持有一个共享 mink 根目录，但仍希望 mink 按 `session_id` 分目录。

以 `project` layout 为例：

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

`MINK_HOME` 可覆盖 CLI/SDK 的 home 根。`session_id` 是稳定内部 ID；除 `isolated` 外，它通常也是最终目录名。
`isolated` 中 `home` 自身就是 session 目录，`session_id` 仍写入 `session.json` 并用于事件、SDK final 和恢复引用。
`session.json` 保存用户可读的 alias、title、cwd 和时间戳。`--session NAME` 会按 alias、完整 id、id 前缀和 title解析已有 session，匹配不到时创建新的时间戳 session 并把 NAME 规范化为安全 alias。列表和解析路径对损坏的 `session.json` 采用 legacy fallback，不让单个坏 metadata 阻断恢复。`--continue` 会选择当前 layout 下最近修改的 session。

---

## 关键不变式

- 每个用户输入开始时重置 StormBreaker、belief、decision cooldown 和 interrupt。
- 同一用户输入的 tool_use 内循环最多压缩一次。
- `ImmutablePrefix` 变更必须通过 prefix manager / invalidate 路径。
- `ConversationStore` append 时保持内存缓存一致，不靠读盘恢复正常路径性能。
- `ToolRunner::format_tool_result()` 是工具输出进入 LLM/UI 前的最大字节保护。
- `ToolRunner::execute_all()` 在 StormBreaker 前执行 enabled/disabled 和 approval 检查；`enabled_tools` 既过滤工具 schema，也阻止真实执行。
- 超长工具输出必须保存为当前 session artifact，而不是丢失全文。
- `Read` 本地非 raw 输出记录 snapshot；raw 和 immutable resource 不生成可编辑 snapshot。
- registered resource 必须先于 VFS 处理；未知非 web scheme fail closed，不落入普通路径或 VFS。
- 未注入 VFS 时，`Read` / `Glob` / `Grep` 必须继续执行原有本地实现；VFS 分支不得改变本地路径语义或测试。
- prompt skill index、selected skills、`skill://` 和 `rule://` 必须来自同一 `CapabilitySnapshot`，其 dependency fingerprint 进入 `ImmutablePrefix`。
- 虚拟 `Read` 永远不生成可编辑 snapshot；`Edit` 和 `Write` 始终针对本地文件系统。
- VFS 后端必须使用 `resource_session_id` 隔离数据；`agent_session_id` 只标识具体调用代理。
- `Edit.patch` 必须校验 snapshot tag 和目标行 hash，stale 时 fail closed。
- `render_tool_result_detail()` 必须保留默认实现。
- TUI 输入 cursor 必须落在 UTF-8 char boundary。
- TUI 点击目标只对应当前可见 viewport。
- 子代理详情使用稳定 `session_id`，不要回退到裸 `line_idx` 作为视图主键。
