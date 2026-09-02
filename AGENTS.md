# Agents Guide

> 更新日期：2026-08-27

[TOC]

---

## 项目概览

Mink 是一个 Rust 实现的轻量 AI coding agent，专为 DeepSeek/OpenAI-compatible API 优化。项目目标是单二进制、低运行时依赖、终端优先，同时可作为 Rust 库嵌入任何 Rust 项目（`mink::runtime`）。

核心能力：

- LLM 流式请求 -> 工具执行 -> 决策的内循环
- **Rust 库 API**：`AgentRuntime::start() → run_turn() / stream_turn() → shutdown()`，无需子进程
- 信号驱动的信念系统：自动错误检测、轨迹证据注入（`[trajectory]`）、编辑循环快照回滚与恢复首步守卫；响应档位由 `SignalPolicy` 门控，阈值/超参为内部 `Config.signal` 常量，可用 `MINK_SIGNAL_POLICY=off` 关闭
- 上下文自适应压缩：显式阈值、响应预留、热尾部和摘要预算；可选摘要输入降噪
- 维修流水线：Scavenge 回收、Truncation 修复、StormBreaker 重复调用抑制
- Session 持久化：JSONL 追加写入，支持恢复和重放
- Todo 持久化：session `todos.json` 保存权威完整快照、revision、稳定 ID 和状态；`TodoRead` 按需读取快照，`TodoWrite` 修改结构，`TodoAdvance` 转换进度
- Artifact 持久化：超长工具输出落到 session `artifacts/`，序号可恢复且禁止覆盖已有正文，可通过 `Read artifact://<id>` 读取
- 工具元数据与审批策略：每个工具声明 approval tier、结果类型、副作用和 storm 豁免状态
- 轻量资源读取：`Read` 支持本地文件、artifact、skill、rule 和 session introspection URL，并通过 `ResourceRouter` 分发 registered scheme
- Edit 双模式：默认 Hashline snapshot/stale 恢复，或 Replace exact/行窗口 fuzzy 内容匹配；runtime 启动后固定
- 两种终端交互模式：REPL + TUI
- 子代理：统一使用父 session 下的 isolated home；可从空目录启动或目录级 fork 当前 session 状态
- Prefab 启动注入：可选集成层在 session 初始化后重组 session（写入模板会话/标准 `prefix_snapshot` 事件），并让系统提示词从 session 重建缓存前缀；CLI 通过 `--prefab[=TEMPLATE]`（经 `mink-prefab` 的 `adapter` 模块 + core 的 `PrefixSource` / `PostInitHook` 扩展点），子代理继承前缀源（临时功能，后续 DeepSeek 更新模型后可能撤销）

---

## 终端交互模式

### REPL 模式（`-i`）

基于 `rustyline` 的行编辑和 `TerminalDisplay`（`crates/mink-cli/src/ui/engine.rs`）的同步渲染。

工作方式：读取用户输入 -> 发送到编排器 -> `TerminalDisplay` 直接写 stdout/stderr。

- 推理内容：灰色输出
- 文本回复：写 stdout
- 工具调用：黄色 `[tool] 摘要`
- 工具结果：显示 `ToolResultDisplay.content_preview`
- 提示符：绿色 `> `
- 标题栏：通过 ANSI escape 更新模型、tokens、费用和信念度

输入处理位于 `run_interactive()`，由 `rustyline::Editor` 提供历史、行编辑和 Tab 补全。

### TUI 模式（`--tui` / `--tui=inline`）

基于 `ratatui` 的两种事件驱动界面。`--tui`（等价于 `--tui=full`）使用全屏应用内
transcript；`--tui=inline` 使用原生 terminal scrollback。入口统一由
`run_tui()`（`crates/mink-cli/src/tui/mod.rs`）按 `TuiMode` 分发。

工作方式：编排器通过 `Display` trait 输出事件，`TuiDisplay`（`crates/mink-cli/src/tui/display.rs`）转为 `TuiSignal`，TUI 主循环消费 mpsc channel 并渲染。

两种模式共用结构化 transcript reducer、工具卡片、Markdown、自动折叠、多行输入、状态栏、
slash command，以及 Plan、Todo、Artifact 和子代理详情。

TUI 特有操作和行为：

- 输入区支持多行编辑，光标和删除逻辑按 UTF-8 char boundary 处理。
- Ctrl+C 在 `waiting/thinking/generating/tool/sub-agent/compacting` 等工作状态中断当前 turn；空闲状态按退出流程处理。
- `/flash`、`/pro`、`/compact`、`/help`、`/skills`、`/exit`、`/quit`、`/q` 在本地处理。未知 `/xxx` 不发送给模型；需要发送 slash 文本时在行首加空格。
- 工具结果可自动折叠。TUI 展示的是 `ToolResultDisplay.content`，仍受工具层 `tool_result_max_bytes` 保护。
- Full 模式使用 alternate screen 和 mouse capture，主视图支持应用内滚动、工具卡片点击和可逆折叠。
- Inline 模式把连续 sealed transcript 和稳定 Markdown 块通过 `insert_before` 写入原生
  scrollback；使用 terminal scrolling region 避免提交时重绘动态 viewport，并把一轮结束时的
  最后一个 item 保留在 viewport，直到新工作开始。自动折叠后的内容不再提供展开操作，主视图
  不启用 mouse capture。
- 工具卡片根据 `ToolResultKind`、执行状态和 presentation 使用同一套语义着色与紧凑/展开投影。
- TUI 初始化从当前 session 的 `plan.draft` / `plan.md` 和 `todos.json` 恢复 Plan/Todo 详情状态。
- 超长工具结果折叠后仍显示首个 `artifact://ID`，完整内容由 Artifact 详情按需读取。
- `/plan`、`/todos`、`/artifact ID` 和 `/sub-agent ID` 打开结构化详情。
- Plan、Todo 和 Artifact 详情按扣除水平 padding 后的内容宽度折行，滚动边界按折行后的可视行计算。
- Inline 详情页临时切换 alternate screen 时保存原 inline terminal；返回主视图必须恢复同一个
  terminal 对象并清理重绘，不能创建第二个 inline viewport。

---

## Display 抽象

两种终端模式实现同一个 `Display` trait（`crates/mink-core/src/ui/mod.rs`）。

```rust
pub struct ToolCallDisplay<'a> {
    pub tool_use_id: &'a str,
    pub tool_name: &'a str,
    pub summary: &'a str,
    pub input: Option<&'a serde_json::Value>,
}

pub struct ToolResultDisplay<'a> {
    pub tool_name: &'a str,
    pub content_preview: &'a str,
    pub content: &'a str,
    pub tool_use_id: Option<&'a str>,
    pub exit_code: Option<i32>,
}

pub struct PresentedToolResultDisplay<'a> {
    pub base: ToolResultDisplay<'a>,
    pub status: ToolStatus,
    pub result_kind: ToolResultKind,
    pub presentation: Option<&'a ToolPresentation>,
    pub artifacts: &'a [ArtifactDisplay],
}

pub trait Display: Send + Sync {
    fn render_thinking(&self, content: &str);
    fn render_text(&self, content: &str);
    fn render_tool_call(&self, call: &ToolCallDisplay<'_>);
    fn render_tool_result(&self, result: &PresentedToolResultDisplay<'_>);
    fn render_stop(&self, reason: &str);
    fn render_signal(&self, signal_kind: &str, severity: f64, message: &str);
    fn render_error(&self, message: &str);
    fn render_retry(&self);
    fn render_info(&self, msg: &str);
    fn render_title_update(&self, model: &str, stats: &StatsSnapshot);
    fn render_sub_agent_status(&self, session_id: &str, status: &str, in_tokens: u64, out_tokens: u64);
    fn render_sub_agent_output(
        &self,
        session_id: &str,
        status: &str,
        thinking: &str,
        text: &str,
        in_tokens: u64,
        out_tokens: u64,
    );
    fn render_prompt(&self);
    fn render_clear_line(&self);
}
```

工具结果显示使用 `PresentedToolResultDisplay` 包装 `ToolResultDisplay`。`content` 是工具层
截断/过滤后的展示内容，受 `tool_result_max_bytes` 保护；`content_preview` 用于简短终端展示，
presentation 携带 Plan/Todo 结构化状态。

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
│ tools/runner.rs       │ ToolRegistry + resolved surface gate + StormBreaker + artifact spill
│ tools/todo.rs         │ TodoRead / TodoWrite / TodoAdvance 与追加式状态事件
│ tools/metadata.rs     │ approval tier、结果类型、副作用等工具元数据
│ tools/file.rs         │ Read/Write/双模式 Edit + selector/resource/prepare/commit
│ tools/hashline.rs     │ 非 Block Hashline grammar、行号/文本锚点坐标 apply
│ tools/replace.rs      │ Replace exact/行窗口 fuzzy 匹配、歧义诊断和缩进转换
│ tools/snapshot.rs     │ Hashline 版本历史、seen-lines、tag 和淘汰
│ tools/search.rs       │ Glob/Grep
│ tools/vfs.rs          │ Read/Glob/Grep 同步只读 VFS hook、请求/结果协议和数据库 helper
│ tools/bash.rs         │ Bash 执行 + 误用拦截
│ tools/python.rs       │ Python 执行
│ tools/plan.rs         │ PlanDraft/PlanConfirm/PlanClear 类型化命令
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
┌─────── 信号层 ────────┐
│ guard/collector.rs    │ ToolFailed/ToolError/EditLoop 信号采集（soft/hard 分级）
│ guard/evidence.rs     │ 轨迹证据跟踪：重复调用/失败聚类/预算消耗（证据注入、回滚定位）
│ agent/belief.rs       │ 信念度计算（内部先验/窗口/衰减常量）
│ agent/decision.rs     │ 分层响应决策（证据注入/回滚/接管）
 │ config.rs             │ MINK_SIGNAL_POLICY 覆盖 / SignalPolicy 枚举 / 内部 SignalConfig 常量
└───────────────────────┘
         │
┌─────── 持久化层 ──────┐
│ session/store.rs      │ append-only JSONL、活跃后缀缓存和尾部修复
│ session/artifacts.rs  │ artifact 索引、持久序号恢复和防覆盖写入
│ session/stats.rs      │ Token/费用统计
│ session/usage.rs      │ LLM 请求级 Token 与费用明细
│ session/compaction.rs │ 显式压缩策略、非破坏式历史投影和 LLM 摘要
│ session/compaction_input.rs │ 可选摘要输入降噪
│ session/prefix.rs     │ ImmutablePrefix 缓存
│ session/plan.rs       │ PlanStore 与 append-only 计划状态转换
│ session/todo.rs       │ TodoStore、稳定 ID、原子持久化和追加式物化投影
│ session/atomic_file.rs│ Plan/Todo 共用的同目录原子替换
│ session/init.rs       │ Session 初始化
└───────────────────────┘
         │
┌─────── UI 层 ─────────┐
│ crates/mink-core/src/ui/mod.rs │ Display trait + StatsSnapshot
│ crates/mink-cli/src/ui/engine.rs │ REPL 同步渲染
│ crates/mink-cli/src/tui/         │ TUI 事件、状态、输入、渲染
│ crates/mink-cli/src/ui/replay.rs │ REPL session 重放
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
  ├── belief.decay(Config.signal.decay_per_input)   # 跨轮衰减替代硬重置
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
│    ├── resolved ModelToolSurface gate                 │
│    ├── StormBreaker                                  │
│    ├── ToolExec dispatch                             │
│    └── format_tool_result / noise filter / artifact  │
│ 6. 持久化 tool results 到 ConversationStore           │
│ 7. Display 输出工具结果                              │
│ 8. 信号采集 -> belief -> decision                    │
└──────────────────────────────────────────────────────┘
```

### 信号系统（分层响应模型）

```
工具执行完毕
       │
SignalCollector.collect()
       ├── ToolFailed：exit_code != 0 / Error 前缀 / safety blocked
       ├── ToolError：regex 匹配编译错、测试失败等
       └── EditLoop：编辑-检查循环窗口检测
       │
BeliefTracker.observe()      # 参数来自内部 SignalConfig 常量（阈值/先验/窗口/衰减）
       │  滑动窗口 + 拉普拉斯平滑
       ▼
DecisionEngine.decide_with_signals()
       ├── 单次软失败且 B >= warn_threshold -> None（记录不干预；累计 >= 2 次参与决策）
       ├── B 在提醒区 -> 轨迹证据注入（[trajectory]/[detector] 事实，无命令）
       ├── B 在警告区 -> 证据注入 + 快照回滚 + 恢复首步守卫（拦截喂回信念）
       ├── 守卫连续拦截 >= guard_max_blocks -> 绕过守卫并强制证据注入
       └── B < abort_threshold -> 用户接管（HandOver 事件 + 证据报告）
```

响应档位由 `MINK_SIGNAL_POLICY` / `SignalPolicy` 配置；阈值与超参是内部策略常量
（`Config.signal`：`remind/warn/abort_threshold`、`alpha/beta_prior`、
`window_size`、`decay_per_input`、`evidence_max_chars`、`guard_max_blocks`、
`rollback_enabled` 等），不通过 `.minkrc` / `--config` 暴露。设计依据见
`docs/设计哲学-信号系统.md`。

`MINK_SIGNAL_POLICY=off` 时，不生成 `<belief-awareness>` prompt 段，也不执行信号采集、信念更新、证据注入、回滚、接管和恢复守卫。

---

## 模块索引

### 入口与配置

| 文件 | 职责 |
|------|------|
| `crates/mink-cli/src/cli.rs` | **Mink / mink-core 共用 CLI adapter**，参数解析、配置合并、sandbox re-exec、模式分发 |
| `crates/mink-cli/src/main.rs` | `mink` binary thin wrapper → `mink_cli::cli::main_entry()` |
| `crates/mink-cli/src/bin/mink-core.rs` | `mink-core` SDK binary thin wrapper → `mink_cli::cli::main_entry()` |
| `crates/mink-prefab/` | 独立 prefab seeder crate：模板加载、校验、会话/事件/prefix 写入 |
| `crates/mink-router/` | 独立 Flash 路由 crate：pi-deepseek-route 策略 Rust 移植，`RouterLlmBackend` LlmBackend 装饰器（`--router`，`router` feature） |
| `config.rs` | Config 结构体、CLI/env/配置文件合并、API key 和 sandbox 配置 |
| `context.rs` | AgentSharedContext + ToolContext |
| `assets.rs` | 嵌入 tools.json、内置 skills |
| `capabilities/mod.rs` | CapabilitySnapshot 汇总 skills、context files、rules 和 dependency fingerprint |
| `capabilities/skills.rs` | 统一 skill provider / snapshot：本地目录优先、内置 skill 兜底、runtime skill 注入 |
| `capabilities/context_files.rs` | AGENTS/CLAUDE instruction files 的 snapshot |
| `capabilities/rules.rs` | rule provider / snapshot |
| `resources/router.rs` | ResourceRouter 和 ResourceHandler |
| `resources/skill.rs` | `skill://list` / `skill://<name>` 资源读取 handler |
| `resources/rule.rs` | `rule://list` / `rule://<name>` 资源读取 handler |
| `cancel.rs` | CancellationToken 父子传播 |
| `safety.rs` | 危险命令过滤 |
| `sandbox/` | 沙箱自举和平台实现 |

### Rust 库门面 (`mink::runtime`)

| 文件 | 职责 |
|------|------|
| `crates/mink-core/src/runtime/mod.rs` | 公共 API 导出：`AgentRuntime`、`AgentRuntimeHandle`、`AgentEventStream`、`AgentEvent`/`AgentEventKind`、`AgentOptions` 等 |
| `crates/mink-core/src/runtime/builder.rs` | crate-private `build_runtime()` — 从 `AgentOptions` 内部 resolved 配置构造 runtime |
| `crates/mink-core/src/runtime/config.rs` | 私有 resolved 配置 / `SessionPolicy` / `SessionInfo` |
| `crates/mink-core/src/runtime/handle.rs` | `AgentRuntime`（唯一 shutdown owner）/ 可克隆 `AgentRuntimeHandle` — `start()`, `handle()`, `run_turn()`, `stream_turn()`, `compact()`, `set_model()`, `interrupt_current_turn()`, `shutdown()` |
| `crates/mink-core/src/runtime/options.rs` | `AgentOptions` ergonomic builder，含 `with_tool()` 自定义工具注册与 `with_prefix_source()` / `with_post_init_hook()` 扩展点注入 |
| `crates/mink-core/src/runtime/extensions.rs` | 中立扩展点：`PrefixSource`（替代编译前缀）与 `PostInitHook`（初始化后钩子，带只读视图与事件写入） |
| `crates/mink-core/src/runtime/events.rs` | turn-scoped `AgentEvent` envelope / 异步 `EventSink` + dispatcher / `EventDisplay` adapter |
| `crates/mink-core/src/runtime/tools.rs` | 稳定异步 `AgentTool` 自定义工具 API：`ToolDefinition` / `ToolExecutionContext` / `ToolOutput` / `ToolError` |
| `crates/mink-core/src/runtime/sdk_adapter.rs` | SDK option/status/exit code 映射，CLI/SDK 去重 |
| `crates/mink-core/examples/web_api.rs` | Hidden-worker web API demo：axum + 进程沙箱 + 异步任务队列 |
### Agent 核心

| 文件 | 职责 |
|------|------|
| `agent/orchestrator.rs` | 命令循环、模型切换、手动 compact、turn 后处理 |
| `agent/turn.rs` | 单轮执行器、工具循环、Display 输出 |
| `agent/compactor.rs` | turn 内压缩封装和同轮压缩防护 |
| `agent/plan_actions.rs` | 将已完成的 Plan 类型化命令转换为 turn effect 和压缩请求 |
| `agent/belief.rs` | 信念度追踪 |
| `agent/decision.rs` | 结构化注入/中止决策 |
| `agent/recovery_policy.rs` | 从 resolved semantic capabilities 校验恢复首个调用 |
| `config.rs` | `SignalPolicy` / 信号模式开关 |
| `agent/tool_signals.rs` | 工具信号处理 + 轨迹证据记录（EvidenceTracker） |
| `guard/evidence.rs` | 轨迹证据构造与渲染：重复调用/失败聚类/预算截断/新鲜度去重 |
| `agent/sub_coordinator.rs` | 子代理启动、并发限制、结果收集 |
| `agent/sub_executor.rs` | 子代理执行 |
| `agent/prefix.rs` | agent 层 prefix manager；prefab 模式下从 session `events.jsonl` 的 `prefix_snapshot` 事件重建完整 prefix |

### 工具系统

| 文件 | 职责 |
|------|------|
| `tools/metadata.rs` | ToolMetadata、ApprovalTier、ToolResultKind、ToolErrorKind（结构化错误码） |
| `tools/catalog.rs` | schema、executor registry 和 build availability 的统一目录 |
| `tools/surface.rs` | 按工具选择、approval、角色、后端、feature 和硬依赖解析模型可见工具面 |
| `tools/approval.rs` | 构建模型工具面时使用的非交互审批判定 |
| `tools/semantic_capabilities.rs` | 语义能力 offer、provider binding 和参数级 scope classifier |
| `tools/runtime_guidance.rs` | 带结构化工具引用的运行时引导消息 |
| `tools/runner.rs` | ToolExec registry、resolved surface gate、批量分发、结果格式化、artifact spill、SubAgent tool |
| `tools/todo.rs` | `TodoRead` / `TodoWrite` / `TodoAdvance`，revision 校验、增量事件和物化投影 |
| `tools/file.rs` | Read/Write/双模式 Edit、path selector、resource URL、prepare/commit |
| `tools/hashline.rs` | 非 Block parser、行号/文本锚点坐标操作和剪贴板 apply |
| `tools/replace.rs` | exact/行窗口 fuzzy 匹配、歧义诊断和缩进转换 |
| `tools/snapshot.rs` | FileSnapshotStore、版本历史、seen-lines、tag 和路径迁移 |
| `tools/search.rs` | Glob/Grep |
| `tools/vfs.rs` | `ReadOnlyFileSystem`、session scope、结构化 VFS 请求/结果和虚拟路径 helper |
| `tools/bash.rs` | Bash、危险命令检查、误用拦截 |
| `tools/python.rs` | Python |
| `tools/plan.rs` | PlanDraft / PlanConfirm / PlanClear |

### Read / Edit 协议

`Read.path` 支持行 selector：

- `path:raw`
- `path:N`
- `path:N-M`
- `path:N+K`
- `path:N-M:raw`
- `path:raw:N-M`

`Read.path` 支持轻量资源 URL：

- `artifact://<id>`：读取被截断工具输出
- `skill://list` / `skill://<name>`：列出或读取当前 capability snapshot 中的 skill
- `rule://list` / `rule://<name>`：列出或读取当前 capability snapshot 中的 rule
- `session://current`：当前 session 摘要
- `session://current/stats`：stats JSON
- `session://current/messages` / `session://current/messages/all`：conversation 摘要
- `session://current/history`：从完整 `conversation.jsonl` 生成的有损检索 transcript，可由 Grep 直接搜索；不包含 thinking 和完整工具结果正文
- `session://current/artifacts`：artifact 列表
- `session://current/todo`：Todo 快照（与 TodoRead 同源）；`session://current/plan`：计划状态与内容（与 Plan 工具同源）

Hashline 模式下，本地文件非 raw `Read` 输出带 snapshot header：

```text
[src/foo.rs#0A3B]
41:fn target() {
42:    old()
```

`Edit` 在 runtime 启动时固定为 `hashline` 或 `replace`。Hashline 只接受带
`[PATH#TAG]` section 的 `input`；Replace 只接受 `path + edits[{old_text,new_text,all}]`。
旧 `path + patch` / `@PATH#TAG` 协议不兼容。`N*` Block locator 不支持（非合法语法，
按行号解析自然拒绝），Mink 不提供 tree-sitter block resolver；范围端点可用行文本
锚点 `PUT 'start'..'end':` / `CUT 'start'..'end':`（trim 后精确匹配、必须唯一）。

Hashline 保留 session 内历史版本并只在全部锚点能唯一映射、共享一致偏移时恢复 stale
内容；Replace 默认要求唯一候选。歧义、目标冲突、越权路径和无法解释的 no-op 都必须
fail closed。

### UI

| 文件 | 职责 |
|------|------|
| `crates/mink-core/src/ui/mod.rs` | Display trait、结构化工具展示协议、ToolResultDisplay、StatsSnapshot |
| `crates/mink-cli/src/ui/engine.rs` | REPL 同步渲染 |
| `crates/mink-cli/src/ui/replay.rs` | REPL session 重放 |
| `crates/mink-cli/src/tui/mod.rs` | TUI 入口和事件循环 |
| `crates/mink-cli/src/tui/display.rs` | Display 到 TuiSignal 的适配 |
| `crates/mink-cli/src/tui/state.rs` | 结构化 transcript、提交边界、输入、Plan/Todo/Artifact 和子代理状态 |
| `crates/mink-cli/src/tui/input.rs` | TUI 输入、快捷键、鼠标、slash command |
| `crates/mink-cli/src/tui/render/*` | TUI 内容区、详情页、输入区、状态栏渲染 |
| `crates/mink-cli/src/tui/markdown/*` | TUI Markdown 子集渲染 |

### Session 与协议

| 文件 | 职责 |
|------|------|
| `session/store.rs` | ConversationStore append-only JSONL、活跃后缀缓存、流式后缀读取和尾部修复 |
| `session/artifacts.rs` | ArtifactManager、artifact index、持久序号恢复和完整工具输出防覆盖落盘 |
| `session/metadata.rs` | session identity、alias、title 和时间戳元数据 |
| `session/stats.rs` | token 和费用统计 |
| `session/usage.rs` | LLM 请求级 Token 与费用明细 journal |
| `session/compaction.rs` | 显式压缩策略、非破坏式历史投影和 LLM 摘要 |
| `session/compaction_input.rs` | 可选摘要输入降噪：删除 thinking、压缩工具参数和结果 |
| `session/prefix.rs` | ImmutablePrefix |
| `session/plan.rs` | PlanStore、原子计划状态转换和 append-only transition |
| `session/todo.rs` | session `todos.json` 的原子存储、revision 对账和追加式物化投影 |
| `session/atomic_file.rs` | Plan/Todo 状态文件共用的同目录临时文件和原子替换 |
| `session/paths.rs` | session 路径 |
| `session/init.rs` | session 初始化 |
| `protocol.rs` | LLM stream Event 类型 |
| `events.rs` | 结构化事件日志 |
| `repair/scavenge.rs` | 工具调用回收和 JSON 修复 |
| `prompt.rs` | system prompt 构建 |
| `errors.rs` | 错误分类 |

---

## 关键不变式

- `TurnCompactor`：同一用户输入的内循环最多压缩一次上下文；auto、preflight、manual 和 overflow 统一经过该守卫并传播失败
- `ImmutablePrefix`：system prompt/tools 变更必须 invalidate prefix
- 前缀构建/失效重建时必须向 events.jsonl 写一条 `prefix_snapshot` 事件（fingerprint/dependency_fingerprint/system_prompt/tools_json），使任意请求的模型可见前缀可离线重建；缓存命中不得重复写（invariant 测试钉住）
- `ConversationStore` 内存缓存只保留当前活跃后缀；append 增量更新该缓存，完整历史读取作为一次性读盘操作
- `conversation.jsonl` 完整保留且只追加；压缩只推进 `context-state.json` 中的活跃投影边界
- `ConversationStore` 续写前修复未换行尾记录，并以包含换行的单缓冲区追加
- `context-state.json` 必须通过同目录临时文件 + rename 原子替换，成功后再更新内存状态
- 压缩状态提交成功后必须按新的 `active_start` 裁剪 ConversationStore 缓存；模型请求只能通过 `active_messages()` 读取活跃投影
- 投影边界必须位于完整历史内，且不能拆开 tool call/result 协议
- 所有压缩统一调用 LLM 摘要；摘要以唯一 internal user `<compacted-summary>` checkpoint 投影，不修改 immutable system/tools prefix
- auto 压力优先使用同模型、同 system/tools 指纹、同 projection generation 的最近 provider prompt usage 校准；preflight 始终使用保守本地估算，usage 基线只保存在 runtime 内存
- 支持 cache projection 的 backend 必须让 compaction 复用上一 Agent 请求的实际 system/tools 与 dropped 历史公共前缀；无法证明边界或超预算时按 reduction 配置降级
- todo 权威完整快照保存在 session `todos.json`；TodoWrite / TodoAdvance 成功后在 conversation 尾部追加增量事件和 `<current-todos>` 物化投影，不做逐请求前置投影
- `TodoWrite` 和 `TodoAdvance` 各自依赖 `TodoRead`；调用使用最高可见 revision 和稳定 ID，stale revision 必须失败后重读
- `TodoWrite` 只新增 pending 条目、删除条目或替换正文，`TodoAdvance` 只执行 pending / in_progress / completed 合法转换；两者都必须原子提交
- session 恢复或压缩后若 `todos.json` revision 领先活跃历史，只追加一次 TodoSync；历史 revision 领先文件时 fail closed
- 同一 active batch 可有多个 `in_progress` 项；结束前提醒最多注入一次，不强制逐项串行执行
- 一个 session 的 `TodoStore` 由单个 runtime 持有；不支持跨进程并发写或外部热编辑 `todos.json`
- 压缩请求使用当前 turn/编排器传入的活动真实模型名和别名，并复用 runtime 的共享 `LlmBackend`
- `TurnExecutor` 启动子代理时传入包含当前活动模型的 child config，子代理复用父 runtime 的 `LlmBackend`
- `context_compact_input_reduction=true` 时只精简摘要请求，不能改写完整历史或热尾部
- Agent JSONL `SdkOptions`、Python `SandboxConfig` 和 Rust `AgentOptions` 必须覆盖同一组 `max_context`/压缩参数，并映射到唯一 `Config`
- runtime 必须在创建 session 前调用 `validate_runtime_config()`；有限窗口的 reserve 和摘要输出必须小于窗口，热尾部必须小于主请求输入预算
- 压缩百分比、响应预留、热尾部和摘要输出预算必须来自显式配置，不根据上下文窗口推断隐式档位
- `max_context_tokens=0` 禁用 auto/preflight 和本地输入预算上限，但保留手动压缩
- provider context overflow 只允许在无部分输出且本轮尚未压缩时触发一次 LLM 压缩和一次重试
- 子代理 fork 在 runtime 初始化前以目录级克隆继承父 session 状态
- Prefab 模式使用标准 `prefix_snapshot` 事件记录特殊 system prompt/tools，不创建额外 `prefab-*.json` 文件；普通 runtime 忽略该事件中的 Prefab 前缀
- Prefab 重组只允许写入全新 conversation；已有 conversation 的 session 不重新写入模板，缺少 `prefix_snapshot` 事件时只补写标准前缀事件，不得修改 conversation
- `mink-prefab` 提供独立 seeder（`seed`/`template`）与可选 `mink-integration` 适配层（`adapter::PrefixPrefixSource` / `PrefabRestructureHook` / `install_template`）；CLI `--prefab[=TEMPLATE]` 经适配层接线，core 通过 `PrefixSource` / `PostInitHook` 扩展点接入，子代理继承父前缀源
- `ArtifactManager` 初始化必须从已有 index 的最大序号继续，正文文件必须使用独占创建，禁止覆盖恢复或 fork 继承的 artifact
- `ConversationStore` append 写入通过内部写锁串行化；读盘只容忍文件末尾未换行的半截 JSONL
- `StormBreaker` 每个新用户输入重置；同轮内所有调用（包括 mutating 调用）统一计数，相同调用连续 3 次触发抑制，不同调用不误伤
- `BeliefTracker` 初始信念 0.75；每用户输入按 `Config.signal.decay_per_input`（默认 0.6）衰减替代硬重置——跨轮重复失败累积升级、偶然失败自然消退；`DecisionEngine` 冷却与 `StormBreaker` 仍每输入 reset
- 软信号（ToolError/EditLoop/ArgumentError）单独出现（累计 <= 1 次）且信念 >= warn_threshold 时不产生任何响应（记录不干预）；累计 >= 2 次软失败或出现硬信号（ToolFailed/SafetyBlocked/CompileError/TestFailure）与结构化错误码（Timeout/ProcessFailed/SafetyBlocked/Aborted）即参与决策
- 信号响应注入的是轨迹事实（`[trajectory]`/`[detector]` 帧），禁止祈使句与"进入恢复模式"类命令；证据窗口由 `signal.seq_window` 注入，去重哈希只覆盖证据事实文本（`evidence_dedup_window` 控制窗口），不含 belief 数值，同一证据批不重复注入；响应事件必须携带证据文本（可回溯到 conversation.jsonl）
- 快照回滚只作用于循环窗口（最近 ROLLBACK_WINDOW_STEPS 步，与 collector seq_window 对齐）内被编辑过的路径；回滚目标是该路径**最后一次 Read/Write 完整内容基线**（record_edit 的编辑后内容不得作为回滚目标，否则恒等 no-op）；Replace 模式的 Read 同样记录基线；写回必须经 atomic_replace 且磁盘内容与基线不一致时才写；写回后 bump memo mutation；回滚事件以 `signal_rollback` 落 events.jsonl
- 恢复守卫拦截必须生成真实信号喂回信念；连续拦截达到 `guard_max_blocks` 必须绕过守卫并强制证据注入，禁止无限拦截
- B < abort_threshold 时进入用户接管：结构化 `signal_handover` 事件（证据/编辑路径/选项）落 events.jsonl 后返回 Failed；禁止静默丢弃证据
- 策略重启子代理初始化失败必须降级（记录 `signal_replan_error` 事件后返回 None），禁止把信号响应路径的初始化错误升级为整轮 Err
- Recovery 首步资格来自 resolved semantic capabilities；Bash 的 `FocusedVerificationExec` classifier 与普通 Bash 安全/误用策略相互独立
- approval 在构建 `ModelToolSurface` 时解析；`ToolRunner::execute_all()` 在 StormBreaker 前校验调用属于同一个 resolved surface
- `ToolRunner::execute_all()` 只并发连续只读工具；写入、执行、控制和 SubAgent 工具必须按调用顺序串行执行
- `enabled_tools` 是唯一工具启用输入；`None` 使用 catalog 默认集合，空列表禁用全部，显式列表精确选择
- `PythonSandbox` 是 catalog explicit-only 工具，只有 `enabled_tools` 显式列出时进入 surface
- 工具真实执行只接受 `ModelToolSurface` 中的工具；disable flag 和 sandbox 工具策略不属于运行时合同
- PlanDraft/PlanConfirm/PlanClear 必须通过类型化 `PlanCommand` 和 `PlanStore` 完成；文件错误必须返回模型，禁止空成功
- 已确认计划存在时禁止创建新草稿；PlanClear 必须同时清理可能遗留的陈旧草稿
- PlanConfirm/PlanClear 成功工具结果写入后必须追加 confirmed/cleared 内部 user transition，不触发强制压缩；未压缩历史依赖 PlanDraft + transition，压缩后若 `plan.md` 仍存在则在摘要后投影唯一的 `<active-plan-checkpoint>`，PlanClear 后移除
- Plan 与 SubAgent 结果必须在延迟工作完成并经过统一大小保护后再进入信号采集
- 默认 approval mode 是 `yolo`；`prompt` 目前没有交互式 UI，会 fail closed
- `ToolRunner::format_tool_result()` 是工具输出进入 LLM/UI 前的统一最大字节保护，超长输出写入 `artifact://<id>`
- `Bash` / `Python` 必须在 `ToolContext.cwd` 下执行；Bash 未显式设置 `timeout` 时使用稳定的全局 tool timeout
- `TurnExecutor` 写入 LLM conversation 使用 `conv_content`，为空时使用 `content`
- `Read` 本地非 raw 输出会记录 snapshot；raw 或 immutable resource 不生成可编辑 snapshot
- registered resource URL 先于 VFS 处理；未知 URL-like scheme 必须 fail closed
- Grep 可搜索 registered resource 文本；resource path 不接受 selector/glob，返回行号用于后续 Read selector
- `Read` 模型可见参数只含 `path`（行范围用路径选择器）；全工具 schema 声明字段必须与 serde 接受字段一致且 `additionalProperties:false`（catalog 一致性测试强制）
- 行选择器 `N+K` 计算必须饱和；offset 超过总行数时报错而非回读幻影行号；`split_content_lines("")` 返回空集（空文件 0 行），Read/Write/Edit 与 hashline 解析都遵循该语义
- Read memo 命中必须同时满足 len/mtime 一致、epoch 一致、mutation_epoch 一致与范围覆盖；任何压缩提交成功后必须 bump epoch，任何 Write/Edit 成功后必须 bump mutation；子代理 memo 相互独立，仅本地文件
- `tool-inventory` section 内容必须与当前 `ModelToolSurface` 名称集一致；空 surface 才使用 `runtime-capabilities`
- prompt 资产写作纪律：所有适用的 system 指令均必须遵守；RFC2119 只精确定义 system prompt 内全大写关键字的强度，不重解释用户/rule/skill 普通措辞或输出标记；每个 `<critical>` 必须有 3-6 条战术 bullet，每条 ≤12 英文词且只表达一个主张；示例/规范形态置尾、禁 token/budget 措辞、不写引擎内部机制；由 `tests/prompt_discipline.rs` 机械执行（bullet 数/词数/禁词/占位符白名单/示例置尾）
- 压缩 cut point 必须保留最近真实 user 消息（≥2 条，优先于纯 token 预算）
- Edit no-change 幂等成功仅当"位置精确且最终状态可验证一致"（`hashline::already_applied`）；任何歧义必须退回 soft no-op → 3 次硬错误的 fail-closed 路径
- prompt skill index、selected skills、`skill://` 和 `rule://` 必须来自同一 `CapabilitySnapshot`
- selected skill 正文不依赖资源读取 provider；skill index 和子资源访问由已解析的 `ResourceRead` provider 与 `ResourceRouter` handler 共同提供
- MISSION 只能覆盖 allowlisted core；runtime-owned section fail fast，普通自定义规则不得使用 reserved 的 `# rules`
- 嵌入式 runtime 可为普通路径注入同步只读 VFS，仅替换 Read/Glob/Grep 后端；未注入时必须严格保持原有本地执行路径
- VFS 调用同时携带继承的 `resource_session_id` 和当前 `agent_session_id`；虚拟 Read 不生成 snapshot，Edit/Write 始终操作本地文件
- Hashline stale 恢复仅允许所有锚点唯一映射且共享一致偏移；Replace 多候选必须拒绝
- Display 实现必须完整转发 `ToolCallDisplay` / `PresentedToolResultDisplay` 的结构化字段；不得退化丢失 `tool_use_id`、presentation 或 artifact 元数据。
- TUI 光标必须始终落在 UTF-8 char boundary。
- Inline TUI 只提交连续且 sealed 的 transcript 前缀；committed 项不得再修改或重复写入原生 scrollback。
- Inline TUI 空闲时保留最后一个 sealed item；新工作开始后才能推进该 item 的 committed 边界。
- Inline 详情视图不得丢弃或重建主视图 terminal，否则 alternate screen 恢复内容会与新
  viewport 叠加形成残影。
- Full TUI 必须保留主视图鼠标命中、可逆折叠和应用内 viewport；不得受 Inline committed 边界影响。
- TUI 实时 signal 与 replay 必须经过同一个 reducer；结构化工具状态不得从展示文本反向解析。
- TUI Todo 增量 presentation 必须合并到当前完整状态，不能覆盖未出现在本次变化中的条目。
- TUI Plan/Todo/Artifact 详情必须先按实际内容宽度折行，再计算垂直滚动范围。

---

## 新增工具步骤

1. 在 `crates/mink-core/src/tools/*.rs` 中实现 `ToolExec` 或辅助函数。
2. 在 `crates/mink-core/src/tools/runner.rs` 的 `TOOL_REGISTRY` 中注册工具。
3. 在 `crates/mink-core/src/assets/tools.json` 中添加 schema。
4. 在 `metadata()` 中声明 `ApprovalTier`、`ToolResultKind`、副作用（mutating）、spawns_sub_agent、storm_exempt 等属性。
5. 如果工具需要压缩给 LLM 的内容，设置 `ToolOutcome.conversation_content`。
6. 添加单元测试，包括 schema/registry 一致性、approval、错误路径、截断、artifact、信号和安全边界。

---

## 开发提示

```bash
cargo build
cargo build --release
cargo test              # 日常测试（跳过重型测试，~5 秒）
cargo test -p mink-cli --all-features tui  # 仅 TUI 模块测试
cargo test -p mink-prefab                # 仅 prefab seeder 测试
cargo test -p mink-router                # 仅 router 测试
cargo test -p mink-prefab --features mink-integration  # prefab runtime 适配层测试
cargo test -p mink-cli --features prefab prefab   # prefab CLI/TUI 测试
cargo build -p mink-cli --features prefab         # 构建带 prefab 的终端二进制
cargo clippy --all-targets
make build
make check
make test

# 全量测试（含 WASM 沙箱测试，~120 秒）
cargo test --features slow-tests -- --include-ignored

# 仅运行重型测试（CPython WASI 沙箱）
cargo test --features slow-tests -- --include-ignored tools::sandbox_python
make feature-matrix     # workspace 拆分和 SDK 精简构建矩阵
```

`crates/mink-core/src/tools/sandbox_python.rs` 的 25 个测试默认跳过。它们通过 wasmtime JIT 执行 CPython WASM，
CPU 密集度高。CI 环境应使用 `--features slow-tests -- --include-ignored` 确保全量覆盖。

调试：

```bash
# 查看信念变化
grep '"belief"' events.jsonl | jq '{type, belief}'

# 查看注入历史
grep '"Injecting trajectory evidence"' events.jsonl

# 查看回滚与接管事件
grep '"signal_rollback"\|"signal_handover"' events.jsonl

# 查看前缀快照（system prompt + tools 指纹，离线重建请求前缀 / cache miss 归因）
grep '"prefix_snapshot"' events.jsonl | jq '{version, fingerprint, dependency_fingerprint}'

# stream-json 模式
./target/release/mink --print "..."

# TUI 模式
./target/release/mink --tui
```

---

## 文档索引

| 文档 | 说明 |
|------|------|
| `docs/ARCHITECTURE.md` | 运行时分层、模块职责、核心数据流 |
| `docs/DESIGN.md` | 设计总纲与关键不变式；信号/工具能力细节见对应设计哲学文档 |
| `docs/USAGE.md` | CLI 参数、环境变量、会话管理、工具参考 |
| `docs/EMBEDDING.md` | Rust 库 / Python SDK 嵌入、Token 用量与费用 |
| `docs/PROTOCOL.md` | `--print` stream-json 与 `--agent-jsonl` 协议 |
| `docs/server.md` | mink-server REST/SSE API、生命周期与并发语义 |
| `docs/tools.md` | 内置工具参数与行为 |
| `docs/设计哲学-工具能力与提示词解耦.md` | 工具 surface、语义能力、自由组合、前向求值和 prompt 所有权 |
| `docs/设计哲学-信号系统.md` | 信号系统完整设计 |
| `docs/TUI_OPTIMIZATION_ROADMAP.md` | TUI 当前实现和维护建议 |
| `docs/设计哲学-多模态读图.md` | 多模态读图协议设计（能力冻结、缓存布局、`image://` 协议、单次消费、配额与降级矩阵） |
| `docs/设计哲学-远程图片URL.md` | 后续远程图片 URL 直通设计（尚未实现） |

---

*本文件面向 AI code agent，帮助快速理解当前项目结构、运行时不变式和开发惯例。*
