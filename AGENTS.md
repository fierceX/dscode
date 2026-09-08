# Agents Guide

> 更新日期：2026-09-08

## 项目概览

Mink 是一个 Rust 实现的轻量 AI coding agent，专为 DeepSeek/OpenAI-compatible API 优化。目标是单二进制、终端优先，同时可作为 Rust 库嵌入任何项目（`mink::runtime`）。

核心能力：

- LLM 流式请求 → 工具执行 → 决策的内循环；REPL / TUI 两种终端模式
- **Rust 库 API**：`AgentRuntime::start() → run_turn() / stream_turn() → shutdown()`，无需子进程
- 信号驱动的信念系统：自动错误检测、轨迹证据注入（`[trajectory]`）、编辑循环快照回滚与恢复首步守卫；档位由 `SignalPolicy` 门控，阈值/超参为内部 `Config.signal` 常量，`MINK_SIGNAL_POLICY=off` 关闭
- 上下文自适应压缩：显式阈值、响应预留、热尾部、摘要预算；摘要以 internal user `<compacted-summary>` checkpoint 投影
- Todo（`todos.json` 权威快照 + revision/稳定 ID）、Artifact（超长工具输出落盘）、轻量资源读取（artifact/skill/rule/session URL）
- 工具元数据与审批策略（approval tier / 结果类型 / 副作用 / storm 豁免）；Edit 双模式（Hashline snapshot 或 Replace exact/fuzzy），runtime 启动后固定
- 子代理（父 session 下 isolated home，可目录级 fork）；Prefab 启动注入（可选，`--prefab`，临时功能）

---

## Agent 工作规范

本文件每个任务都会注入；下面是本仓库对 agent 的**行为约束**，与「关键不变式」（系统架构契约）互补。违反任一条都属于需要修正的工作方式。

### 改动纪律

- **先搜后改**：路径不确定时先 `Grep`/`Glob` 定位，再 `Read` 精确目标范围；禁止凭印象猜路径、行号、字段名或 API。
- **因果自检**：每次代码改动前回答：改什么行为（cause）、预期观察（effect）、如何验证（verify）。三问答不出就不改。
- **独立小步**：因果独立的改动分开提交/验证；一次只做一件事，避免捆绑无关修改。
- **不动不知道的**：不理解的不改；先读调用链与既有测试，再决定。
- **走公开协议**：改计划用 `PlanDraft`/`PlanConfirm`/`PlanClear`；改 Todo 用 `TodoRead`/`TodoWrite`/`TodoAdvance`；编辑文件用 `Read` 后的 snapshot tag（Hashline）或先读后写（Replace）；**禁止直接写 `plan.md`/`todos.json`/`context-state.json` 等运行时状态文件**。
- **不伪造成功**：失败必须如实报告；不得绕过输出大小保护（`format_tool_result`/artifact）或任何 fail-closed 边界。

### 验证纪律

- **改完必验**：跑相关测试并读完整输出（`cargo test -p mink-core` 起手；改动面涉及 CLI 则加 `-p mink-cli`；涉及构建矩阵改动跑 `make feature-matrix`）。
- **提交前**：`cargo fmt --all -- --check` + `cargo clippy --workspace --all-targets --all-features -- -D warnings` 必须干净（CI 同款命令）。
- **报告证据**：声称"完成/修复/通过"前必须有命令输出佐证；无法验证就说明无法验证。
- **回归意识**：改动核心路径（压缩/持久化/编辑/信号）注意 `crates/mink-core/tests/invariants.rs` 与 regression 套件；删除或弱化被测试钉住的行为前先确认该行为已废弃。

### 失败纪律

- **停止-重析**：同一命令连续失败或输出异常时停下，重读相关代码与错误信息，换一种方法；禁止无修改重复相同命令。
- **假定不可信**：编译通过≠行为正确；测试全绿≠没有回归；用新证据（针对性测试/读取日志）确认因果。
- **如实暴露**：发现与自己之前结论矛盾的新证据，按新证据行事并明说修正。

### 架构变动纪律

- **契约必守**：「关键不变式」是不可逾越的红线，不得以"这次简单/换个方式也行"为由绕过；若确需变更协议，先更新本文件与 `docs/` 对应章节，并保持全仓一致。
- **同步文档**：改了架构/行为，同步 `docs/ARCHITECTURE.md`、`docs/DESIGN.md`、`docs/USAGE.md`、`docs/tools.md` 中对应描述及 `AGENTS.md` 不变式与更新日期；用户可见功能变更记入 `CHANGELOG.md` Unreleased。
- **测试锚定**：新增行为必有测试；守护不变式优先落在 `tests/invariants.rs` 或模块级回归测试。

### 提交纪律

- **提交卫生**：不提交敏感数据（模型轨迹、密钥、用户目录、内网地址）与实验性未跟踪产物；拿不准先问。
- **注释全面**：commit message 按主题完整概括改动（what + why + 影响面），不写"update/fix 小改"这种无法追溯的标题。
- **粒度**：一个主题一个提交；「版本号 + 文档同步」这类准备性改动可单独成提交。

---

## 运行时分层（概览）

```
main.rs → OrchActor (agent/orchestrator.rs) → TurnExecutor (agent/turn.rs)
  TurnExecutor 内循环（同一用户输入可多轮 tool_use）：
  1. 压缩检查（同输入最多一次，TurnCompactor 守卫）
  2. LLM 流式请求（SSE → Event；工具层解析）
  3. Scavenge 回收遗漏工具调用 → 持久化 assistant 消息
  4. ToolRunner::execute_all（surface gate → StormBreaker → dispatch → 格式化/artifact）
  5. 持久化 tool results → Display 输出 → 信号采集 → belief → decision
```

分层：LLM 层（`llm/client.rs` 流式客户端与重试、`llm/transport.rs` 请求构造、`sse/*` 解析）、
工具层（`tools/runner.rs` 注册与分发、`tools/file.rs` Read/Write/Edit、`tools/{bash,python,todo,plan}.rs`）、
资源与能力层（`resources/router.rs`、`capabilities/*`）、信号层（`guard/*`、`agent/{belief,decision}.rs`）、
持久化层（`session/{store,compaction,plan,todo,prefix,artifacts}.rs`）、UI 层（`crates/mink-cli/{ui,tui}/*`）。
完整模块职责表见 `docs/ARCHITECTURE.md`；REPL/TUI 行为细节见 `docs/DESIGN.md` 与 `docs/TUI_OPTIMIZATION_ROADMAP.md`。

---

## 关键不变式

以下契约均由代码或测试钉住；改动相关代码时必须保持。动机/设计依据见 `docs/DESIGN.md` 与对应 `docs/设计哲学-*.md`。

### 压缩与持久化

- `TurnCompactor`：同一用户输入的内循环最多压缩一次；auto、preflight、manual、overflow 统一经过该守卫并传播失败。
- `ImmutablePrefix`：system prompt/tools 变更必须 invalidate prefix；前缀构建/失效重建必须向 events.jsonl 写一条 `prefix_snapshot` 事件（fingerprint/dependency_fingerprint/system_prompt/tools_json），缓存命中不得重复写（invariant 测试钉住）。
- `conversation.jsonl` 完整保留且只追加；压缩只推进 `context-state.json` 的活跃投影边界；`ConversationStore` 内存缓存只保留活跃后缀，append 增量更新。
- `context-state.json` 必须同目录临时文件 + rename 原子替换，成功后更新内存并按新 `active_start` 裁剪缓存；模型请求只能经 `active_messages()` 读取活跃投影。
- 续写前修复未换行尾记录，追加用含换行的单缓冲区；append 经内部写锁串行化；读盘只容忍文件末尾未换行的半截 JSONL。
- 投影边界必须位于完整历史内，且不能拆开 tool call/result 协议；cut point 必须保留最近 ≥2 条真实 user 消息（优先于纯 token 预算）。
- 所有压缩统一调用 LLM 摘要；摘要以唯一 internal user `<compacted-summary>` checkpoint 投影，不修改 immutable system/tools prefix；`context_compact_input_reduction=true` 只精简摘要请求，不改写完整历史或热尾部。
- auto 压力优先使用同模型、同 system/tools 指纹、同 projection generation 的最近 provider prompt usage 校准（基线须为当前投影严格前缀）；preflight 始终保守本地估算，基线只存 runtime 内存；`prompt_usage_calibration_safe=false` 的后端禁用校准。
- 支持 cache projection 的 backend 必须让摘要复用上一 Agent 请求的实际 system/tools 与历史公共缓存前缀；无法证明边界或超预算时按 reduction 配置降级。
- 压缩/子代理请求必须使用当前活动真实模型名和别名，并复用 runtime 共享 `LlmBackend`。
- provider context overflow 只允许在无部分输出且本轮尚未压缩时触发一次 LLM 压缩和一次重试。
- `max_context_tokens=0` 禁用 auto/preflight 和本地输入预算上限，保留手动压缩；压缩百分比、响应预留、热尾部、摘要输出预算来自显式配置（不推断隐式档位）；有限窗口下 reserve 与摘要输出必须小于窗口、热尾部小于主请求预算。
- Agent JSONL `SdkOptions`、Python `SandboxConfig`、Rust `AgentOptions` 覆盖同一组 `max_context`/压缩参数并映射到唯一 `Config`；runtime 创建 session 前调用 `validate_runtime_config()`。

### Plan 与 Todo

- PlanDraft/PlanConfirm/PlanClear 必须通过类型化 `PlanCommand` 与 `PlanStore` 完成；文件错误必须返回模型，禁止空成功；已确认计划存在时禁止创建新草稿；PlanClear 同时清理陈旧草稿。
- PlanConfirm/PlanClear 成功工具结果写入后追加 confirmed/cleared 内部 user transition，不触发强制压缩；未压缩历史依赖 PlanDraft + transition，压缩后若 `plan.md` 存在则在摘要后投影唯一 `<active-plan-checkpoint>`，PlanClear 后移除。
- Plan 文件变更与 conversation 追加由 `plan-transaction.json` 可重放 journal 协调：未绑定操作回滚、已绑定操作幂等补齐（此文件任一时刻不可与 `plan.md`/对话历史存在分叉）。
- todo 权威完整快照在 `todos.json`；TodoWrite/TodoAdvance 成功后追加增量事件与 `<current-todos>` 物化投影，不做逐请求前置投影；两者依赖 TodoRead，使用最高可见 revision 与稳定 ID，stale 失败后重读。
- TodoWrite 只新增 pending/删除/替换正文，TodoAdvance 只做合法转换，均原子提交；session 恢复或压缩后若文件 revision 领先活跃历史只追加一次 TodoSync，历史领先文件时 fail closed。
- 同一 active batch 可有多个 in_progress；结束前提醒最多注入一次；一个 session 的 TodoStore 由单个 runtime 持有，不支持跨进程并发写或外部热编辑。

### 信号系统

- `BeliefTracker` 初始 0.75，每输入按 `decay_per_input`（默认 0.6）衰减替代硬重置；`DecisionEngine` 冷却与 `StormBreaker` 每输入 reset。
- 软信号（ToolError/EditLoop/ArgumentError）单次且信念 ≥ warn 不干预；累计 ≥2 次软失败或出现硬信号（ToolFailed/SafetyBlocked/CompileError/TestFailure）与结构化错误码（Timeout/ProcessFailed/SafetyBlocked/Aborted）才参与决策。
- 响应注入的是轨迹事实帧（`[trajectory]`/`[detector]`），禁止祈使句与"进入恢复模式"命令；去重哈希只覆盖证据事实文本，同一证据批不重复注入；响应事件携带证据文本（可回溯 conversation.jsonl）。
- 回滚只作用于循环窗口内被编辑路径，目标为最后一次 Read/Write 完整内容基线（编辑后内容不得作为回滚目标）；Replace 的 Read 同样记录基线；写回经 `atomic_replace` 且仅当磁盘与基线不一致；写回后 bump memo mutation；以 `signal_rollback` 落事件。
- 恢复守卫拦截必须生成真实信号喂回信念；连续拦截达 `guard_max_blocks` 必须绕过守卫并强制注入，禁止无限拦截；B < abort 进入用户接管（`signal_handover` 事件落盘后 Failed），禁止静默丢弃证据；策略重启子代理初始化失败必须降级（`signal_replan_error` 后返回 None），不得升级为整轮 Err。
- Recovery 首步资格来自 resolved semantic capabilities；Bash 的 `FocusedVerificationExec` classifier 与普通 Bash 安全/误用策略相互独立。

### 工具面与执行

- approval 在构建 `ModelToolSurface` 时解析；`ToolRunner::execute_all()` 在 StormBreaker 前校验调用属于同一个 resolved surface；真实执行只接受 surface 内工具（disable flag 与沙箱策略不属于运行时合同）。
- `execute_all()` 只并发连续只读工具；写入、执行、控制、SubAgent 工具按调用顺序串行执行。
- `enabled_tools` 是唯一工具启用输入；`None` 用 catalog 默认集，空列表禁用全部，显式列表精确选择；`PythonSandbox` 是 explicit-only，仅显式列出时进入 surface。
- 默认 approval mode 是 `yolo`；`prompt` 无交互式 UI，fail closed。
- `format_tool_result()` 是工具输出进入 LLM/UI 前的统一最大字节保护，超长写 `artifact://<id>`；`TurnExecutor` 写入 conversation 用 `conv_content`，为空用 `content`。
- `Bash`/`Python` 必须在 `ToolContext.cwd` 下执行；Bash 未显式设 timeout 时用稳定全局 tool timeout。
- `StormBreaker` 每新输入重置；同轮所有调用（含 mutating）统一计数，相同调用连续 3 次触发抑制，不同调用不误伤。
- Plan 与 SubAgent 结果必须在延迟工作完成并经过统一大小保护后再进入信号采集。
- 子代理 fork 在 runtime 初始化前以目录级克隆继承父 session 状态；子代理复用父 `LlmBackend` 与当前活动模型。
- Prefab 模式用标准 `prefix_snapshot` 事件记录特殊前缀，不创建 `prefab-*.json`；重组只允许写入全新 conversation，已有会话不重写模板、不得修改 conversation；普通 runtime 忽略该事件中的 Prefab 前缀；`mink-prefab` 提供独立 seeder 与可选 `mink-integration` 适配层。
- `ArtifactManager` 从已有 index 最大序号继续，正文独占创建，禁止覆盖恢复或 fork 继承的 artifact。

### Read / Edit 协议

- `Read` 模型可见参数只含 `path`（行范围用 `path:N`/`N-M`/`N+K`/`:raw` 选择器）；全工具 schema 字段必须与 serde 接受字段一致且 `additionalProperties:false`（catalog 一致性测试强制）。
- 行选择器 `N+K` 必须饱和；offset 超总行数报错而非回读幻影行号；`split_content_lines("")` 返回空集（空文件 0 行），Read/Write/Edit 与 hashline 解析共用该语义。
- `Read` 本地非 raw 输出记录 snapshot；raw 或 immutable resource 不生成可编辑 snapshot；Read memo 命中需 len/mtime/epoch/mutation_epoch 一致且范围覆盖；压缩提交成功后 bump epoch，Write/Edit 成功后 bump mutation；子代理 memo 独立，仅本地文件。
- Hashline stale 恢复仅当所有锚点唯一映射且共享一致偏移；Replace 多候选必须拒绝；Edit no-change 幂等成功仅当位置精确且最终状态可验证一致（`hashline::already_applied`），任何歧义退回 soft no-op → 3 次硬错误的 fail-closed 路径。
- registered resource URL 先于 VFS 处理，未知 URL-like scheme fail closed；Grep 可搜索 registered resource 文本，resource path 不接受 selector/glob（返回行号供后续 Read selector）。
- 嵌入式 runtime 注入同步只读 VFS 仅替换 Read/Glob/Grep 后端；未注入时严格保持本地执行路径；VFS 调用同时携带继承的 `resource_session_id` 与当前 `agent_session_id`；虚拟 Read 不生成 snapshot，Edit/Write 始终本地。
- prompt 资产纪律：每个 `<critical>` 3-6 条战术 bullet、每条 ≤12 英文词且单一主张；示例置尾；禁 token/budget 措辞；不写引擎内部机制；由 `tests/prompt_discipline.rs` 机械执行。
- `tool-inventory` 内容必须与当前 `ModelToolSurface` 名称集一致，空 surface 才用 `runtime-capabilities`；MISSION 只能覆盖 allowlisted core，runtime-owned section fail fast；普通自定义规则不得使用 reserved 的 `# rules`。
- skill index、selected skills、`skill://`/`rule://` 必须来自同一 `CapabilitySnapshot`；selected skill 正文不依赖资源读取 provider。

### Display 与 TUI

- Display 实现必须完整转发 `ToolCallDisplay` / `PresentedToolResultDisplay` 结构化字段，不得丢失 `tool_use_id`、presentation 或 artifact 元数据。
- TUI 光标必须落在 UTF-8 char boundary；输入/删除按 char boundary 处理。
- Inline TUI 只提交连续且 sealed 的 transcript 前缀；committed 项不得修改或重复写入原生 scrollback；空闲保留最后一个 sealed item，新工作开始后才能推进 committed 边界。
- Inline 详情视图不得丢弃或重建主视图 terminal（否则 alternate screen 内容叠加残影）；Full TUI 保留主视图鼠标命中、可逆折叠与应用内 viewport，不受 Inline committed 边界影响。
- TUI 实时 signal 与 replay 必须经同一个 reducer，结构化工具状态不得从展示文本反向解析；Todo 增量 presentation 合并到当前完整状态；Plan/Todo/Artifact 详情先按内容宽度折行再算滚动范围。

---

## 新增工具步骤

1. 在 `crates/mink-core/src/tools/*.rs` 实现 `ToolExec` 或辅助函数。
2. 在 `tools/runner.rs` 的 `TOOL_REGISTRY` 注册。
3. 在 `crates/mink-core/src/assets/tools.json` 添加 schema（字段与 serde 一致）。
4. 在 `metadata()` 声明 approval tier、结果类型、副作用（mutating）、spawns_sub_agent、storm_exempt。
5. 需要压缩给 LLM 的内容时设置 `ToolOutcome.conversation_content`。
6. 添加单元测试：schema/registry 一致性、approval、错误路径、截断、artifact、信号与安全边界；涉及协议变更同步 `docs/tools.md`。

---

## 开发提示

```bash
cargo test              # 日常测试（跳过重型测试，~5 秒）
cargo test -p mink-core --lib           # 仅 core 单元测试
cargo test --workspace                  # 全 workspace（含 server/CLI/router）
cargo test --features slow-tests -- --include-ignored   # 全量（含 WASM 沙箱，~120 秒）
cargo test --features slow-tests -- --include-ignored tools::sandbox_python   # 重型沙箱
make build / make check / make test / make feature-matrix / make regression-all
cargo fmt --all -- --check && cargo clippy --workspace --all-targets --all-features -- -D warnings

# 调试（session 目录内）
grep '"belief"' events.jsonl | jq '{type, belief}'
grep '"signal_rollback"\|"signal_handover"' events.jsonl
grep '"prefix_snapshot"' events.jsonl | jq '{version, fingerprint, dependency_fingerprint}'
./target/release/mink --print "..."   # stream-json 模式
./target/release/mink --tui
```

`crates/mink-core/src/tools/sandbox_python.rs` 的 25 个测试默认跳过（wasmtime JIT 执行 CPython WASM，CPU 密集），CI 用 `--features slow-tests -- --include-ignored` 全量覆盖。

---

## 文档索引

| 文档 | 说明 |
|------|------|
| `docs/ARCHITECTURE.md` | 运行时分层、模块职责、核心数据流（模块查找首选） |
| `docs/DESIGN.md` | 设计取舍与不变式详述；信号/工具能力细节见设计哲学文档 |
| `docs/USAGE.md` | CLI 参数、配置、会话管理、工具参考 |
| `docs/EMBEDDING.md` | Rust 库 / Python SDK 嵌入、Token 用量 |
| `docs/PROTOCOL.md` | `--print` stream-json 与 `--agent-jsonl` 协议 |
| `docs/server.md` | mink-server REST/SSE API、生命周期与并发语义 |
| `docs/tools.md` | 内置工具参数与行为（Read/Edit 协议细节） |
| `docs/设计哲学-工具能力与提示词解耦.md` | 工具 surface、语义能力、prompt 所有权 |
| `docs/设计哲学-信号系统.md` | 信号系统完整设计 |
| `docs/设计哲学-多模态读图能力.md` | 多模态读图能力设计说明（能力冻结、单次消费、配额与降级矩阵） |
| `docs/TUI_OPTIMIZATION_ROADMAP.md` | TUI 当前实现和维护建议 |
