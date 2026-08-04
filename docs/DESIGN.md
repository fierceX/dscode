# 设计文档

> 更新日期：2026-08-03

本文记录 Mink 的设计取舍和关键不变式，不作为用户手册或工具协议参考。终端用户入口、
配置和运行方式见 [USAGE.md](USAGE.md)；Rust/Python 嵌入见 [EMBEDDING.md](EMBEDDING.md)；
机器协议见 [PROTOCOL.md](PROTOCOL.md)；工具参数与边界见 [tools.md](tools.md)；
模块分层见 [ARCHITECTURE.md](ARCHITECTURE.md)。

> **分工说明**：信号系统的完整设计见 [设计哲学-信号系统.md](设计哲学-信号系统.md)；
> 工具 surface、语义能力与提示词解耦的完整设计见
> [设计哲学-工具能力与提示词解耦.md](设计哲学-工具能力与提示词解耦.md)。
> 本文主题五、主题六只保留关键不变式与实现模型摘要，细节以对应设计文档为准。

---

[TOC]

---

## 主题一：Agent 主循环

### 单轮执行契约

`TurnExecutor::execute()` 是 agent 的核心循环，接收一个用户输入，执行零到多轮 LLM 调用，返回最终决策。一轮定义为一个 LLM 请求→响应→工具执行→继续/停止判断的完整周期。

```rust
// crates/mink-core/src/agent/turn.rs
pub async fn execute(
    &mut self,
    user_input: &str,
    belief: Option<&mut BeliefTracker>,
)
    -> Result<(TurnDecision, Vec<TurnEffect>)>
```

**TurnDecision** 有五类：

| 变体 | 含义 | 后续 |
|------|------|------|
| `Stop` | 正常结束（end_turn/stop） | 等待下个用户输入 |
| `Continue` | 有更多 LLM 调用 | 循环继续 |
| `Interrupted` | 被取消令牌中断 | 退出 |
| `MaxTurnsExceeded` | 当前输入超过最大内部循环次数 | 报告限制并结束 |
| `Failed(String)` | 不可恢复的错误 | 报告错误 |

**TurnEffect** 是 turn 内部工具副作用的完成标记，供调用者（OrchActor）补充 UI 提示：

| 变体 | 触发条件 | 调用者处理 |
|------|---------|-----------|
| `PlanCleared` | PlanClear 工具副作用已完成 | 显示计划已清空 |
| `PlanConfirmed` | PlanConfirm 工具副作用已完成 | 显示计划已确认 |

SubAgent 由 `SubAgentCoordinator` 在 turn 内部启动、收集和注入结果。

### 执行阶段

每轮 LLM 调用按固定阶段顺序执行：

```
步骤 0: 新用户输入初始化
  ├── reset_storm()
  ├── TurnCompactor::reset()
  ├── ToolSignalProcessor::reset()
  ├── DecisionEngine::reset()
  └── signal_recovery_guard = false
步骤 1: store.add_user() + ensure_prefix()
步骤 2: 自动压缩检查 + Preflight 紧急压缩
步骤 3: LLM 流式请求（SSE 解析 → Event stream）
步骤 4: Scavenge 回收（从 thinking/text 复原工具调用）
步骤 5: 持久化 assistant 消息和 usage
步骤 6: 工具执行（ToolRunner::execute_all）
  ├── resolved ModelToolSurface 执行门禁
  ├── StormBreaker 重复抑制
  ├── PlanActionHandler 将已完成 PlanCommand 转换为 effect / 压缩请求
  ├── SubAgentCoordinator 启动并收集子代理
  ├── 延迟结果统一执行大小保护
  ├── ToolSignalProcessor 基于最终结果采集信号并更新 belief
  ├── ConversationStore::add_tool_results()
  └── Display::render_tool_result_presented()
步骤 6.1: Plan 压缩请求交给 TurnCompactor，失败则终止并返回错误
步骤 7: DecisionEngine 决策继续、注入、中止或停止
```

各个阶段之间有严格的依赖关系：
- 步骤 2 和步骤 6.1 共用 `TurnCompactor` 内部标记互锁，同一用户输入最多压缩一次
- 步骤 4 依赖步骤 3 收集的 thinking + text 内容
- 步骤 6 依赖步骤 4 补充后的 calls 列表
- 步骤 7 根据 stop_reason 决定是否循环

### LLM 调用循环

同一个用户输入可能触发多次 LLM 调用（工具调用→工具结果→再次调用 LLM）。循环受两个条件约束：

```rust
while turn < max_turns {
    // 每一轮 LLM 调用...
    
    match stop.as_str() {
        "tool_use" | "tool_calls" => {
            messages = compaction.active_messages().await?;  // 刷新活跃投影
            continue;  // 继续下一轮 LLM 调用
        }
        "end_turn" | "stop" => return Stop,
        "error" | "max_tokens" | "length" => return Failed,
        _ => return Stop,
    }
}
```

`messages` 在 tool_use 路径末尾通过 `compaction.active_messages()` 刷新，确保下一轮 LLM 调用
看到最新工具结果、信号注入消息、计划变更和子代理结果，同时不会把冷历史重新加载进模型上下文。

---

## Edit 协议绑定

Edit 是 runtime 配置变体，不是单一 schema 的运行时猜测。`ModelToolSurface::resolve()` 根据
`EditMode` 只物化 Hashline 或 Replace 中的一种 schema；相同配置重复构建保持字节稳定，
切换模式则改变 schema 和 immutable prefix fingerprint。提示词工作流通过互斥语义能力
`HashlineEdit` / `ContentReplaceEdit` 选择，并与 executor 使用同一份最终配置。

Hashline 维护 session 内完整文本历史、seen-lines 和跨调用剪贴板。历史边界是 30 个路径、
每路径 4 个版本、全局 64 MiB；stale 恢复只接受所有锚点唯一映射且共享一致偏移的情况。
Replace 不依赖 snapshot，以 exact 和归一化行窗口 fuzzy 匹配处理有限格式差异，但唯一性优先于相似度：高置信度候选
多于一个时仍拒绝。两种模式都保留现有 approval、路径安全、写入大小、artifact、信号系统和
串行 mutation 边界。Block Hashline 操作明确不支持，也不引入 tree-sitter。

## 主题二：内存模型

### 运行时状态分层

运行时状态分为五个部分，各自有独立的生命周期和变更路径：

#### 1. ImmutablePrefix（不变前缀）

`crates/mink-core/src/session/prefix.rs`

承载 system prompt 和工具定义。一旦构建，在 session 期间应保持不变。变更会触发 fingerprint 失效，导致下一次 LLM 调用丢失前缀缓存。

```rust
pub struct ImmutablePrefix {
    system_prompt: String,
    tools_json: Vec<Value>,
    dependency_fingerprint: String,
    fingerprint: String,
}
```

压缩摘要始终作为活跃消息投影中的动态 system message，不修改 immutable system/tools prefix。

**fingerprint 校验**（`verify_fingerprint()`）：重新计算 system prompt、tools schema 和依赖
fingerprint 的联合指纹并与缓存值比对。校验失败时 `PrefixManager` 丢弃旧前缀并重新构建，不使用
已经漂移的缓存内容。

#### 2. ConversationStore（追加日志）

`crates/mink-core/src/session/store.rs`

JSONL 格式的持久化消息存储。正常运行只有追加操作：

```rust
// 追加（正常路径——O(1)，只检查文件尾部）
Append: repair tail → single-buffer line write → flush → 更新内存缓存

```

**内存缓存**：`cache: RwLock<Option<CachedLines>>`，其中 `CachedLines` 保存全局消息起点
`start` 和对应 `lines`。首次正常请求通过 `lines_from(active_start)` 流式解析并校验 JSONL，
只保留和缓存活跃后缀；append 增量追加到该缓存。压缩状态提交成功后按新的 `active_start` 裁剪缓存，
因此冷历史只保留在磁盘，不随 session 生命周期持续占用运行时内存。

`lines()` 仍可显式读取完整历史，但当缓存已经裁剪时不会用完整结果替换活跃缓存。
`last_assistant_message()` 优先从活跃缓存查找，必要时流式扫描磁盘，只保留最后一条 assistant，
避免 SDK final 或子代理结果收集重新加载完整 session。

续写前只反向扫描最后一条未换行记录：若它是完整 JSON，则先补换行；若它是崩溃留下的半截 JSON，则截断到上一条完整记录。新 JSON 与换行在同一个缓冲区中写入，避免再次制造可被后续记录拼接的尾部。

**消息格式**：

```json
{"role":"user","content":"..."}
{"role":"assistant","content":[{"type":"thinking","thinking":"..."},{"type":"text","text":"..."},{"type":"tool_use","id":"...","name":"...","input":{}}]}
{"role":"user","content":[{"type":"tool_result","tool_use_id":"...","content":"..."}]}
```

assistant 消息使用 content 数组承载多种内容类型（thinking/text/tool_use），而不是扁平字段。这是因为 Anthropic/DeepSeek 的 content block 格式是结构化的。

#### 3. Mutable Session State（可变会话状态）

压缩投影、计划和 Todo 使用独立的 session 状态文件：

- `context-state.json` 保存 `active_start` 和滚动摘要，决定 conversation 的当前活跃投影；
- `plan.draft` 和 `plan.md` 由 `PlanStore` 管理，确认后的计划在每次请求时投影为唯一的动态
  `<current-plan>` system message；
- `todos.json` 保存 Todo 的权威完整快照，conversation 只追加成功变更事件、紧凑 active
  投影和必要的 TodoSync。

这些状态不进入 `ImmutablePrefix`，也不改写 `conversation.jsonl` 中的既有消息。Plan/Todo 状态文件
通过同目录临时文件、rename 或受控 unlink 提交；持久化成功后才更新进程内状态。

#### 4. VolatileScratch（每轮清除）

每个新用户输入开始时重置的状态：
- StormBreaker 窗口（`tools.reset_storm()`）
- `TurnCompactor` 的同轮压缩标记
- `ToolSignalProcessor`、`DecisionEngine` 和恢复首步守卫

`thinking` 和 `text` 缓冲区则在每次 LLM 流式请求中重新创建，不跨请求复用。

这些 turn 级状态会跨同一用户输入的多次 tool-use 请求保持，但不会泄漏到下一个用户输入。

#### 5. Session Artifacts（会话资源）

`crates/mink-core/src/session/artifacts.rs`

超长工具输出不直接丢弃。`ToolRunner` 在统一结果格式化阶段，如果 `ToolOutcome.content` 超过 `tool_result_max_bytes`：

1. 完整内容写入当前 session 的 `artifacts/<id>.txt`。
2. `artifacts/index.jsonl` 追加 `ArtifactRecord`。
3. 工具结果保留截断摘要，并追加 `artifact://<id>`。
4. 后续可通过 `Read artifact://<id>` 或 `Read artifact://<id>:N-M` 按需读取。

artifact 跟随 session 生命周期，不跨 session 共享。

`ArtifactManager` 初始化时扫描有效 index 记录的数字后缀，从最大序号加一继续分配。
正文文件使用 `create_new` 独占创建；即使存在未写入 index 的孤立文件也只会继续取下一个 ID，
不会覆盖恢复或 fork 继承的历史 artifact。

---

## 主题三：上下文压缩

### 显式策略参数

压缩行为由以下显式配置直接控制，不根据上下文窗口推断策略档位：

| 参数 | 默认值 | 作用 |
|------|-------:|------|
| `context_compact_pct` | 94 | 自动压缩百分比 |
| `context_reserve_tokens` | 64000 | 主请求响应预留，同时限制主请求 `max_tokens` |
| `context_compact_tail_tokens` | 256000 | 压缩后原样保留的热尾部目标 |
| `context_compact_max_output_tokens` | 8192 | 摘要请求输出上限 |
| `context_compact_input_reduction` | false | 是否精简摘要请求输入 |

自动触发点取“窗口百分比”和“窗口减去响应预留”中较早者。手动、preflight、overflow 和计划变更
会绕过百分比判断，但使用同一个热尾部和摘要实现。`max_context_tokens=0` 禁用 auto/preflight
压缩和本地请求预算上限，但保留手动压缩。

### 非破坏式投影

`conversation.jsonl` 始终保存完整消息。压缩在 `context-state.json` 中提交
`active_start + summary`，模型请求只投影动态摘要和边界后的热尾部。
边界只选择字符串 user 消息或 assistant 消息，禁止从 tool result 开始，从而保持
tool call/result 配对。`context-state.json` 是活跃投影状态的唯一来源；状态损坏或边界超过历史长度时直接报错。
`context-state.json` 使用同目录临时文件写完后 rename，替换成功后才更新进程内状态并裁剪
ConversationStore 的活跃缓存。主 turn、tool_use 循环刷新和压缩评估统一使用
`active_messages()` / `lines_from(active_start)` 构建模型上下文。

### 摘要生成与输入降噪

所有压缩都将被折叠的活跃消息送入当前 LLM，并与已有摘要合并：

“当前 LLM”由调用方的活动模型决定：turn 内压缩使用当前 `LlmClient` 的真实模型名和别名，
手动压缩使用 `OrchActor::resolve_active()` 的结果。摘要请求统一通过 runtime 注入的
`Arc<dyn LlmBackend>` 发送。

```
Merge the conversation turns above with the previous context snapshot.
Use exactly these fields:
Task focus:
Latest request:
Progress:
Tool evidence:
Reflections:
```

摘要请求使用独立的最小 system prompt，不加载主 agent 的 skills、rules、工具定义和 mission。
输出预算由 `context_compact_max_output_tokens` 独立控制。摘要结果统一作为动态 system message，
保持 immutable system/tools prefix；`summary.txt` 仅作为 session 元数据和人工检查缓存。

开启 `context_compact_input_reduction` 时，`compaction_input.rs` 先把被折叠消息转换为紧凑
transcript：用户和 assistant text 保留，thinking 删除，工具参数限制为 1000 字符，工具结果限制为
2000 字符并保留头尾、错误/失败/退出状态和 artifact 证据。该转换只影响摘要请求，不修改
`conversation.jsonl`、热尾部或历史资源。

摘要输出按不透明文本处理。提示词要求固定字段，运行时只校验请求成功、正常 stop 和清洗后非空，
不解析或强制校验字段标题。

### 防护措施

**同轮防护**（`TurnCompactor::compacted_this_turn`）：同一用户输入中的自动、Preflight、
overflow 和 PlanConfirm / PlanClear 强制压缩共用一个守卫，整个 tool_use 循环只压缩一次。

**最小收益检查**（`CompactionEngine::evaluate_and_compact`）：如果压缩节省的 token 不足当前总量的 10%，跳过压缩。防止小上下文场景下的无意义压缩。

**Preflight 预算检查**：按转换后的 OpenAI messages、system prompt 和 tools schema 估算输入。
超过 `max_context_tokens - effective_max_tokens` 时强制压缩；压缩后仍超预算则不发送请求。

**摘要预算检查**：摘要请求按最小 system prompt、原始或降噪后的输入和独立输出预算估算；超出
`max_context_tokens` 时在发送前失败，不依赖 provider 隐式截断。

**Provider overflow 恢复**：如果 provider 在尚未输出文本、thinking 或工具调用前明确返回 context
overflow，并且本轮尚未压缩，则执行一次相同的 LLM 压缩并重试一次；第二次失败直接返回重试请求的错误，
不循环恢复。

---

## 主题四：维修流水线

维修机制分布在流式协议归一化、turn 级回收和工具执行门禁三个边界：

```text
SSE tool-call 组装与非 fallback 截断修复
→ Turn 级 Scavenge 回收与结构化校验
→ resolved ModelToolSurface gate
→ StormBreaker
→ ToolExec dispatch
```

### 步骤 1：Tool-call 组装与 Truncation 修复

`crates/mink-core/src/sse/openai.rs` 在一个流式响应内合并 tool-call name、id 和 arguments。
准备生成 `ToolCallEvent` 时，`build_tool_call_event()` 要求 arguments 是 JSON object；首次解析失败
后调用 `repair_truncated_json()`，只接受能够重新解析且 `fallback=false` 的修复结果。无法可靠
修复的输入直接返回解析错误，不以空对象继续执行。

修复规则依次处理尾逗号、悬挂 key、未闭合字符串和未闭合容器，并在返回前重新执行 JSON
校验。`ToolRunner` 对已经结构化的 `input_json` 保留同样的非 fallback 输入守卫。

### 步骤 2：Scavenge（回收）

LLM 流式响应结束后，`crates/mink-core/src/agent/turn.rs` 从 `thinking`
（reasoning_content）和 `text`（普通文本）两个渠道回收遗漏的工具调用，补充到标准
tool-call 列表中。每个候选调用都通过 `build_tool_call_event()` 转为结构化事件；解析失败的
候选会被丢弃并记录诊断。去重使用 `(tool name, input_json)`，因此同名但参数不同的调用可以保留。

回收尝试顺序（`scavenge_tool_calls`）：

1. **DSML invoke** — `<|DSML|invoke name="Read">` DeepSeek 专用标记语言，不经过标准 tool_calls 字段
2. **XML 包装** — `<tool_call>{...}</tool_call>`
3. **Bracket 包装** — `[TOOL_CALL]{...}[/TOOL_CALL]`
4. **裸 JSON** — 扫描自由文本中的 `{name, arguments}` 形状
5. **OpenAI style** — `{"type":"function","function":{"name","arguments"}}`
6. **R1 variant** — `{"tool_name":"Bash","tool_args":{...}}`

这些格式是“容器协议”兼容层，不代表旧参数重新成为模型协议。回收层只把内容规整成
`{name, arguments}`，之后仍由当前 `tools.json` schema、`ToolExec` 参数反序列化和工具实现校验。
例如 XML/Bracket/R1 中可以恢复 `Read {"path":"src/lib.rs:40-80"}` 或
`Edit {"path":"src/lib.rs","patch":"@src/lib.rs#TAG\nreplace 40:\n+..."}`。`Read offset/limit`
仍可由执行层接受，但不会暴露在当前工具 schema 中；`Edit old_string/new_string` 会被拒绝。

### 步骤 3：Surface Gate 与 StormBreaker（重复抑制）

`enabled_tools` 是唯一工具启用输入：`None` 使用 catalog 默认集合，空列表禁用全部，显式列表精确选择；`PythonSandbox` 是 explicit-only 工具。`ModelToolSurface` 再结合 approval、角色、文件系统后端、编译 feature 和硬依赖完成一次解析。默认 `yolo` 允许全部 tier；`write` 自动允许 Read/Write、阻止 Exec；`always-ask` 自动允许 Read、阻止 Write/Exec。单工具 `allow/deny/prompt` 可覆盖模式。当前没有交互式 prompt，`prompt` 会 fail closed。

resolved `ModelToolSurface` 是唯一工具边界：`PrefixManager` 从它生成 tools schema 和能力工作流，`ToolRunner` 在真实执行前检查同一个 surface。这样即使恢复会话、模型异常输出或自定义 backend 产生未暴露的调用，也只会写入错误 tool result，不会执行。运行时直接消费 resolved surface；disable flag、sandbox `allow_*` 和独立的 runner approval 判定不属于执行合同。

`ToolRunner` 先校验调用属于 resolved `ModelToolSurface`，再把每个工具调用的
`(name, args_json)` 放入滑动窗口。检测到同一对 `(name, args)` 连续出现 ≥3 次（窗口 6），
则抑制该调用，返回抑制说明：

```rust
StormDecision::Suppress(reason) => {
    results.push(ToolRunResult {
        content: format!("Error: {reason}"),
        ...
    });
}
```

**Mutating 清空规则**：当 metadata 标记为 mutating 的工具被调用时，清空窗口中的 read-only
条目。这允许 edit→re-read 模式正常执行。

**StormExempt**：SubAgent、PlanDraft、PlanClear、PlanConfirm、TodoWrite、TodoAdvance
跳过风暴检测。TodoRead 是普通只读工具；TodoWrite 只负责结构更新，TodoAdvance 只负责
进度转换，两者都是 revision 驱动的 session 状态变更并通过 `TodoStore` 原子提交。

---

## 主题五：信号驱动的信念系统

信号系统是 Mink 的反馈回路：工具执行质量被采集为信号，合并为单一信念度 `B`，
低信念时向 LLM 注入修正提示（或中止），构成闭环。完整设计（设计思想、信号采集、
信念计算、决策干预、边界情况、组件接口、展示协议）见
[设计哲学-信号系统.md](设计哲学-信号系统.md)。

### 关键不变式

- 信号采集：`ToolFailed`（退出码/`Error:` 前缀）、`ToolError`（regex 启发式）、
  `EditLoop`（滑动窗口序列检测）三类；一次调用多条信号取 `max(severity)`，不叠加
- 信念计算：Beta-Binomial 拉普拉斯平滑，先验 `α=3, β=1`（无观测时 `B=0.75`），
  滑动窗口 W=16，每次用户输入重置
- 决策阈值：`B ≥ 0.70` 不干预；`B < 0.70` 注入（带冷却）；`B < 0.30` 中止（绕过冷却）；
  `DecisionEngine` 每个用户输入重置冷却
- 注入协议：以独立 User 消息（`[System note: ...]`）写入 conversation，不污染 system
  prefix；`RecoveryPolicy` 按 resolved capabilities 渲染恢复提示并校验恢复首步
- `MINK_SIGNAL_MODE=off`：不生成 `<belief-awareness>` prompt 段，不采集、不注入、
  不中止、不启用恢复守卫
- 错误分类（`errors.rs` 的 `ErrorCategory`：Network/Auth/RateLimit/Parse/Tool/Internal）
  仅用于日志与用户提示，不驱动任何决策

## 主题六：工具执行模型

工具执行模型分为三层：模型可见的 surface（`enabled_tools` → `ModelToolSurface` →
resolved capabilities）、执行门禁（surface 校验 → StormBreaker → ToolExec dispatch）、
结果格式化（大小保护 → artifact → 双通道）。surface 与语义能力的完整设计见
[设计哲学-工具能力与提示词解耦.md](设计哲学-工具能力与提示词解耦.md)；
工具参数、执行流程和结果协议见 [tools.md](tools.md)。

### 关键不变式

- `enabled_tools` 是唯一工具启用输入；`ModelToolSurface` 在 session/prefix 构造前
  一次解析，`ToolRunner::execute_all()` 在执行前校验同一 surface
- 只读工具按连续批次并发执行；写入、执行、控制和 SubAgent 工具按调用顺序串行执行
- 工具结果统一经过 `format_tool_result()`：`tool_result_max_bytes` 截断 + artifact 落盘、
  Bash noise filter、Read/Write 行数摘要、Edit 新 header + diff
- 结果双通道：`content`（UI/默认 tool_result）与 `conv_content`（非空时优先进 LLM）
- 同步只读 VFS hook（`ReadOnlyFileSystem`）只替换普通路径后端，不注册新工具；
  `artifact://`、`skill://`、`rule://`、`session://` 不进入 VFS
- 工具执行失败返回 `Error:` 前缀的结构化消息，不 panic

## 主题七：SSE 流式解析

### 解析器状态机

`OpenAIParser` 是一个增量状态机，处理 `data: {...}\n\n` 格式的 SSE 帧：

```rust
pub struct OpenAIParser {
    stop_reason: String,
    input_tokens: i64,
    output_tokens: i64,
    cache_read_input_tokens: i64,
    cache_creation_input_tokens: i64,
    saw_text: bool,
    pending_calls: BTreeMap<i64, PendingCall>,
    pending_usage: Option<UsageEvent>,
    pending_stop: Option<String>,
    saw_done: bool,
    saw_usage: bool,
}
```

每次 `process_line()` 调用处理一行 SSE 数据。多个 chunk 之间的工具调用按 `index` 合并到 `pending_calls` map 中。

### 关键边界处理

**工具调用跨 chunk 合并**：OpenAI 流式 API 将工具调用的 id、name、arguments 分多个 chunk 发送。Parser 用 `BTreeMap<i64, PendingCall>` 按 index 聚合，在 finish_reason="tool_calls" 时一次性 flush。

**推理内容拆分**：`reasoning_content` 和 `reasoning` 两个字段都被识别。DeepSeek R1 使用 `reasoning_content`，OpenAI o1 使用 `reasoning`。

**缓存 token 读取**：`prompt_tokens_details.cached_tokens` 在 usage 解析时通过多层 fallback 提取：

```rust
usage.get("cached_tokens")
    .or_else(|| usage.get("prompt_tokens_details")
        .and_then(|d| d.get("cached_tokens")))
```

### usage 事件延迟发出

OpenAI-compatible provider 可能在 finish chunk、独立 usage chunk 或 `[DONE]` 前后给出终态信息。
Parser 暂存 usage 和 stop reason，并通过 `saw_done` / `saw_usage` 保证终态事件按协议只发出一次。

### LLM backend 注入

默认 backend 是 `OpenAiCompatibleBackend`。`AgentRuntimeConfig` / `AgentOptions` 可以注入
`Arc<dyn LlmBackend>`，主代理、子代理和自动压缩共用同一个 backend trait，不复制 agent loop、
tool runner、session 写入或 usage 统计。

runtime 在构建阶段创建或接收共享 backend。主代理、子代理和压缩分别构造带有不同
`LlmPurpose` 的 `LlmRequest`，并提交到该 backend。
`TurnExecutor` 创建子代理协调器时，会从父配置克隆一份 child config，并把 `model` 设置为当前
活动 `LlmClient` 的别名或真实模型名；存在别名时同时写入该别名到真实模型的映射，保证配置自洽。
每个子代理使用这份显式配置构建 context，因此模型切换后的活动模型和父 runtime 注入的 backend
会同时传递到子代理。

`BackendLlmClient` 负责把当前上下文转换为 `LlmRequest`：

- `model`：经过 `ModelResolver` 解析后的真实 provider 模型名。
- `model_alias`：用户请求的别名，如 `flash`、`pro` 或 `model_aliases` 自定义别名。
- `messages` / `tools` / `system_prompt`：当前 prefix 和 conversation 状态。
- `cancel` / `display` / `purpose`：取消、状态输出和请求归属。

自定义 backend 只返回 `LlmEvent` 流。失败时可用 `LlmRequestFailure { attempt_count, error }`
保留重试次数，`MeteredStream` / `UsageCapture` 仍统一写入 `usage.jsonl`。

---

## 主题八：Session 与持久化

### 目录布局

每个 session 有独立的目录：

| Layout | `home` 含义 | session 目录 |
|--------|-------------|--------------|
| `project` | 用户或服务根目录 | `home/.mink/projects/<project_key(cwd)>/<session_id>/` |
| `home` | 用户或服务根目录 | `home/.mink/sessions/<session_id>/` |
| `direct` | Mink session 集合根目录 | `home/<session_id>/` |
| `isolated` | 当前 session 根目录 | `home/` |

CLI 默认使用 `project`，Python SDK 默认使用 `home`，Rust 嵌入式 `AgentOptions` 默认使用 `isolated`。
`direct` 用于一个共享 Mink 根目录下保存多个 session。`isolated` 用于外层服务已经按任务/session
创建独立目录的场景，此时不再追加 `session_id` 子目录。

以 `project` layout 为例：

```
~/.mink/projects/<project_key>/<session_id>/
├── conversation.jsonl   ← 对话消息（逐行追加 JSON）
├── events.jsonl          ← 事件日志（每行一个事件）
├── session.json          ← session 元数据：alias、title、cwd、时间戳
├── summary.txt           ← 压缩后的上下文快照
├── stats.json            ← Token 用量统计
├── context-state.json    ← 首次提交压缩状态后生成
├── plan.md               ← 确认计划存在时生成
├── plan.draft            ← 未确认草稿存在时生成
├── todos.json            ← 首次成功 Todo 变更后生成
├── usage.jsonl           ← 首次记录 LLM 请求后生成
└── artifacts/            ← 超长工具输出
    ├── index.jsonl
    └── bash-0001.txt
```

`project_key` 是当前工作目录路径经过安全转义后的字符串，确保不同项目间的 session 隔离。
`session_id` 是稳定内部 ID；除 `isolated` 外通常也是目录名。`isolated` 的目录名由外层服务决定，
但 `session_id` 仍写入 `session.json` 并用于事件、SDK final 和恢复引用。

### JSONL 约束

**追加**：`append_line()` 使用 `OpenOptions::append`，通过 store 内部写锁串行化。写入前修复
未换行尾记录，然后以包含换行的单缓冲区追加，并同步更新内存缓存。

**读取**：模型上下文使用 `lines_from(active_start)`。恢复已有 session 时流式解析并校验完整 JSONL，
但只保留活跃边界后的消息并缓存为 `CachedLines { start, lines }`；后续 turn 直接复用该后缀。
`lines()` 只用于显式完整历史读取，且不会扩大已经裁剪的活跃缓存。如果文件末尾存在没有换行的
半截 JSONL，会跳过这条不完整记录；已经以换行结束的坏 JSONL 仍按错误处理，避免静默吞掉真实损坏。

**压缩**：不重写 JSONL。`CompactionEngine` 从活跃缓存读取 `active_start` 后缀，选择新的安全边界；
状态文件通过临时文件和 rename 原子替换，成功后同时推进内存状态并裁剪 store 缓存。
session 恢复和重放仍可按需读取全部原始消息；`session://current/history` 提供有损检索视图。

### Session 恢复

`--continue` 模式通过读取最新 session 目录的时间戳来选择最近的 session。`--session NAME` 会先按 alias、完整 id、id 前缀和 title 解析已有 session；匹配不到时创建新的时间戳 session，并将 NAME 规范化后写入 `session.json` 的 alias。解析时也会尝试规范化后的 alias，因此 `feature x` 能恢复 alias 为 `feature-x` 的 session。坏 `session.json` 不阻断列表和解析，会回退到目录名与 `summary.txt`。

恢复时会 replay 最近 10 轮 LLM 响应事件（从 events.jsonl 读取），在交互式终端重新渲染历史对话。

---

## 主题九：SubAgent（子代理）

### 隔离执行

`SubAgentExecutor` 为每个子代理创建一个完全独立的 `AgentSharedContext`：

```rust
pub async fn new(parent_ctx, session_id, fork) -> Result<Self> {
    // 1. 在父 session 的 subagents/<id>/ 创建 isolated home
    // 2. fork 模式在 runtime 初始化前递归克隆父 session
    // 3. 重置 child session identity、events、stats 和 usage 文件
    // 4. 从 isolated home 正常初始化 store、compaction 和 artifacts
    // 5. 创建 linked child cancel token（父取消→子取消，子取消不影响父）
}
```

### 两种模式

**独立模式（默认）**：在父 session 的 `subagents/<session_id>/` 创建空的 isolated home。
继承父 session 的模型、API URL、工具集和 capability snapshot，但不继承 session 历史。

**Fork 模式**：在构建子 runtime 之前递归克隆父 session 目录，跳过父 session 已有的
`subagents/`。克隆后清除 `session.json`、`events.jsonl`、`stats.json` 和 `usage.jsonl`，
使子代理拥有新的身份与用量统计；conversation、context state、plan、artifacts 及同目录
session 状态会整体继承。压缩引擎在正常初始化时直接加载克隆后的状态。ArtifactManager 同样从
克隆后的 index 恢复下一个序号，并以独占创建方式写入，
因此子代理继续产生 artifact 时不会覆盖父历史中的正文或破坏已有 `artifact://` 引用。

如果父 runtime 注入了只读 VFS，子代理复用同一个 `Arc<dyn ReadOnlyFileSystem>`。两种模式都继承父代理的 `resource_session_id`，但 `agent_session_id` 使用子 session id；fork 只影响对话与 session 文件，不改变知识库分区。

### 结果收集

子代理完成时，通过 `last_assistant_message()` 从活跃缓存读取最后一条 assistant；缓存中不存在时
才流式扫描子 session，并且只保留最后一个匹配消息。返回 thinking 和 text，不包含工具调用细节。

```rust
if let Some(message) = child_store.last_assistant_message().await? {
    // 提取第一个 thinking block 和第一个 text block
}
```

### 并发控制

`SubAgentPool` 使用 `tokio::sync::Semaphore` 限制最大并发数（默认 8）。每个子代理占用一个 permit，完成后释放。
首次并发启动时，共享 `subagents/` 父目录按 `AlreadyExists` 幂等创建，并在创建竞争结束后重新校验为
实体目录；随机 session 子目录仍严格独占创建，冲突时直接失败。

结果通过 `mpsc::UnboundedSender` 发送回 orchestrator，由 `handle_sub_agent_result()` 注入父会话。

每个子代理有独立超时。超时后会取消子代理的 child token 并返回 failed 结果，父会话继续执行。

---

## 主题十：配置系统

### 合并优先级

```
CLI 参数 > 项目 .minkrc > 用户 ~/.minkrc > 环境变量 > 代码默认值
```

`config.rs` 中，配置加载先读取环境变量和默认值，再合并用户级 / 项目级 `.minkrc`，最后用 CLI 参数覆盖。

```rust
pub fn apply_provider_defaults(cfg: &mut Config) -> Result<()> {
    // 1. 环境变量覆盖特定字段
    if let Ok(v) = std::env::var("TOOL_RESULT_MAX_BYTES") { ... }
    if let Ok(v) = std::env::var("FILE_WRITE_MAX_BYTES") { ... }
    // 2. API Key: CLI 参数或配置文件 > DEEPSEEK_API_KEY
    // 3. Base URL: CLI 参数或配置文件 > DEEPSEEK_BASE_URL > 默认 DeepSeek base URL
    // 4. 模型默认: flash alias
    // 5. 验证: API Key 或 Base URL 至少一个存在
```

`flash` / `pro` 是默认模型别名，分别解析到 DeepSeek 默认模型。`.minkrc` 或 `--config` 的
`[model_aliases]` 可覆盖这些别名，也可新增自定义别名。未命中别名的 `model` 会作为真实模型名原样传给 LLM backend。

OpenAI-compatible 请求还可通过顶层字段控制：

```toml
openai_reasoning_effort = "max"       # off/none/false/disabled 表示不发送
openai_include_usage = true
openai_token_param = "max_tokens"     # max_tokens | max_completion_tokens
openai_tool_choice = "auto"           # auto | none | required，或 JSON 对象

[openai_extra_body]
custom_boolean = true
custom_budget = 8192
```

`openai_extra_body` 仅补充 provider 扩展字段，不覆盖 `model`、`messages`、`stream`、
`tools`、`tool_choice`、`max_tokens` 和 `max_completion_tokens`。非 OpenAI-compatible
协议由 `LlmBackend` 注入处理。

显式 CLI `--config <toml>` 解析失败必须 fail fast；用户级/项目级 `.minkrc` 解析失败只输出 warning 并继续。

### size 解析

`parse_size_bytes()` 支持 `k`/`m`/`g` 后缀：

```rust
"100"   → 100
"1k"    → 1000
"500K"  → 500000
"1m"    → 1000000
"2M"    → 2000000
```

用于 `max_context`、`max_tokens` 等配置字段。CLI 中低频配置通过 `--config <toml>` 或
`.minkrc` 传入。

### 环境变量分类

| 类别 | 变量 | 用途 |
|------|------|------|
| API | `DEEPSEEK_API_KEY`, `DEEPSEEK_BASE_URL` | 认证和端点 |
| 大小 | `TOOL_RESULT_MAX_BYTES`, `FILE_WRITE_MAX_BYTES` | 输出限制 |
| 信号 | `MINK_SIGNAL_MODE` | `full` 启用信号系统，`off` 关闭信号提示词和运行时干预 |
| 沙箱 | `MINK_LIMITS` | JSON 格式 sandbox 限制配置 |
| 调试 | `LOG_EVENTS`, `MINK_HOME` | 日志和 session 路径 |

`context_compact_pct`、`context_reserve_tokens`、`context_compact_tail_tokens`、
`context_compact_max_output_tokens`、`context_compact_input_reduction`、`enabled_tools`、
OpenAI-compatible 参数和 `[tools] approval_mode` 可通过 `.minkrc` 或 `--config` 配置。
Agent JSONL `SdkOptions` 和 Python `SandboxConfig` 同样直接暴露 `max_context` 及五个压缩参数，
由 `runtime::sdk_adapter` 映射到共享的 `Config`。

`validate_runtime_config()` 在 runtime 创建任何 session 文件前执行组合校验。有限窗口下要求
`context_reserve_tokens < max_context`、`context_compact_max_output_tokens < max_context`，并要求
`context_compact_tail_tokens` 小于 `max_context - min(max_tokens, context_reserve_tokens)` 得到的
主请求输入预算。`max_context=0` 保留禁用自动压缩和本地输入预算限制的语义。

### 工具审批配置

`ToolConfig` 从 `Config` 派生，随 `ToolContext` 进入工具层：

```toml
[tools]
approval_mode = "yolo" # yolo | write | always-ask

[tools.approval]
Bash = "prompt"        # allow | deny | prompt
Read = "allow"
```

approval 在构建 `ModelToolSurface` 时解析；`ToolRunner::execute_all()` 在 StormBreaker 和实际
执行前再次校验同一 resolved surface。当前 `prompt` 没有交互 UI，因此 prompt policy 会在
surface 解析阶段 fail closed。

---

## 主题十一：并发模型

### 异步边界

```
┌──────────────── Tokio Runtime ────────────────┐
│                                                 │
│  OrchActor  ←── mpsc channel ── SubAgent pool  │
│      │                                          │
│  TurnExecutor                                   │
│      │                                          │
│  ToolRunner::execute_all()                      │
│      │                                          │
│  read batch spawn_blocking() ...                │
│  sequential spawn_blocking()                    │
│      │              │                           │
│  file::read()    bash::execute()                │
│  (同步 I/O)     (子进程 + wait)                 │
└─────────────────────────────────────────────────┘
```

### 状态共享

所有共享状态通过 `Arc<AgentSharedContext>` 传递。内部可变性使用：
- `RwLock` — store cache（读多写少）
- `Mutex` — StormBreaker 窗口（写多）、immutable_prefix 缓存
- `AtomicBool` — dirty 标记
- `AtomicBool` — 当前 turn interrupt 标志
- `mpsc` — Orchestrator 命令、TUI signal、子代理结果收集

### 取消传播

`CancellationToken` 用于全局退出；`AgentSharedContext::interrupt` 用于当前 turn 中断。取消或中断时：
1. 主循环退出
2. SSE stream 在 25ms 检查窗口内停止
3. Bash / Python 工具检查 interrupt 并尝试杀掉子进程
4. 子代理使用 linked child token 接收父取消；子代理超时只取消自身，不取消父会话

---

## 主题十二：系统提示词构建

`prompt.rs` 协调 `PromptDocument`，但不再拥有具体工具清单。提示词按所有权分层：

```
Core sections                 ← 通用行为，不含具体工具名
MISSION core overrides        ← 仅允许覆盖 allowlist core，标记 ExternalOverride
Tool prompt fragments         ← 仅随对应 active tool 加载
Capability workflow packs     ← 由正向事实求值并从 provider bindings 渲染
Rules/instruction/skills      ← 外部内容，不纳入生成内容工具引用保证
Output language               ← core
```

`Current plan` 不参与上述 immutable prompt 装配。每次 agent 请求都从 `plan.md` 生成唯一的
`<current-plan>` 动态 system message，并插入压缩摘要之后、conversation 之前。

Todo 采用追加式事件而不是逐请求前置投影。TodoWrite / TodoAdvance 成功时，tool result
追加本次增量事件和 `<current-todos>` 物化投影；投影只包含 revision、状态计数与当前
`in_progress` 批次，完整列表通过 TodoRead 按需读取。恢复或压缩后，若 `todos.json`
revision 领先活跃历史，则追加一次 TodoSync；历史领先文件时 fail closed。Todo 状态变化
因此不修改 immutable prefix，也不改写旧消息，已有请求仍是后续请求的稳定前缀。

跨工具规则依赖 `ToolSemanticCapability`，不枚举工具 pair。专用 provider 按 tier、priority、
catalog order 和名称稳定决胜；fallback 只填补未绑定能力。workflow 只使用正向事实，并在互斥
winner 决定后提交上游 workflow fact。所有生成 section 都携带 `referenced_tools` 和
`consumed_facts`，装配时校验其属于当前 surface/事实闭包。

MISSION 不再支持旧 section alias。它只能覆盖 allowlist 中当前存在的 core：
`agent-identity`、`environment`、`execution-codes`、`belief-awareness` 和
`output-language`。tool/workflow、`runtime-capabilities`、`rules`、instruction files、
rule/skill index、selected skills 和 current plan 等 runtime-owned section 均不可覆盖；
冲突直接 fail fast。其他一级标题作为 `mission:<id>` 外部 section 追加，例如自定义规则使用
`# mission-rules`，不能使用 reserved 的 `# rules`。压缩摘要仍位于动态消息投影中，不进入
immutable system/tools prefix。

`belief-awareness` 仅在 signal mode 为 `full` 时存在；signal mode 为 `off` 时不能通过
MISSION 创建该 core section。依赖项目直接迁移到新 section 格式，不保留 alias 或双格式
兼容路径。

完整的 surface 解析、语义能力、工具自由组合和受约束前向求值算法见
[工具能力与提示词解耦设计文档](设计哲学-工具能力与提示词解耦.md)。

---

## 主题十三：Protocol 事件

`Event` enum 是流式响应的统一接口：

```rust
pub enum Event {
    Text(TextEvent),          // 文本内容
    Thinking(ThinkingEvent),  // 推理内容
    ToolCall(ToolCallEvent),  // 工具调用
    Usage(UsageEvent),        // Token 用量
    Stop(StopEvent),          // 停止原因
    Error(ErrorEvent),        // 错误
    Retry(RetryEvent),        // 重试信号
}
```

事件由 SSE parser 产生，由 `TurnExecutor` 消费。stream-json 模式下，事件也被序列化为 JSON 行输出到 stdout：

```json
{"type":"text","content":"Hello"}
{"type":"tool_call","name":"Read","id":"...","input":{"path":"/x"}}
{"type":"usage","input_tokens":100,"output_tokens":50,"cache_read_input_tokens":40}
{"type":"stop","reason":"end_turn"}
```

---

## 主题十四：不变式（Invariants）

代码中维护以下不变式：

| 不变式 | 位置 | 违反后果 |
|--------|------|---------|
| compact 后 messages 必须刷新 | `agent/turn.rs`、`agent/compactor.rs` | LLM 发送过期数据 |
| compact 后必须重建消息投影 | `agent/compactor.rs` | LLM 发送完整冷历史或过期边界 |
| 压缩摘要只能进入动态消息投影 | `session/compaction.rs` | prefix cache 因摘要变化失效 |
| Todo 文件原子提交后才能更新内存 revision | `session/todo.rs` | 文件与运行时状态分叉，stale write 失效 |
| Todo 状态只通过成功工具结果或 TodoSync 追加 | `session/todo.rs`、`tools/todo.rs`、`agent/turn.rs` | 前置投影破坏消息前缀，或文件与上下文 revision 分叉 |
| 输入降噪只能修改摘要请求 | `session/compaction_input.rs` | 完整历史或热尾部发生信息损失 |
| 压缩使用调用方活动模型和共享 backend | `turn.rs`、`orchestrator.rs`、`session/compaction.rs` | 模型切换后摘要发往启动模型或绕过宿主 backend |
| 子代理启动配置继承当前活动模型 | `turn.rs`、`sub_coordinator.rs`、`sub_executor.rs` | 模型切换后 child 请求退回父启动模型 |
| 所有嵌入入口映射完整压缩参数 | `sdk_protocol.rs`、`sdk_adapter.rs`、`mink_agent/__init__.py` | 私有小窗口模型只能依赖外部 TOML |
| runtime 在 session 创建前校验上下文预算组合 | `config.rs`、`runtime/builder.rs` | 首次请求才因零输入预算或不可压缩热尾部失败 |
| StormBreaker 窗口每用户输入重置 | `agent/turn.rs` | 跨意图抑制误判 |
| BeliefTracker、ToolSignalProcessor 和 DecisionEngine 每用户输入重置 | `agent/orchestrator.rs`、`agent/turn.rs` | 跨意图信号累积误升级 |
| 同一用户输入最多压缩一次 | `agent/turn.rs`、`agent/compactor.rs` | 多次无用压缩 |
| PrefixManager 校验完整依赖 fingerprint，漂移时重建 ImmutablePrefix | `agent/prefix.rs`、`session/prefix.rs` | 缓存偏移不可检测 |
| Plan/SubAgent 延迟结果完成并执行大小保护后才能采集信号 | `agent/turn.rs`、`tools/runner.rs` | 信号观察占位结果或未保护正文 |
| store 只缓存活跃后缀，append 增量更新 | `store.rs` | 长 session 内存持续增长或读盘性能下降 |
| compact 提交后按 active_start 裁剪 store 缓存 | `compaction.rs` | 冷历史继续常驻内存 |
| 压缩不删除 conversation 历史 | `compaction.rs` | session 无法完整恢复或重放 |
| artifact 序号恢复且正文独占创建 | `artifacts.rs` | fork/恢复后覆盖历史 artifact |

## 主题十五：Rust 库 API 设计

### 为什么是 Rust 库

`mink-core --agent-jsonl` 通过 stdin/stdout 子进程调用已经可用，但：

- 进程启动成本（~100ms cold start）
- JSON 序列化/反序列化开销
- 无法共享内存中的 session store
- 无法订阅实时 typed event

Rust 发布包名为 `mink-core`，库 crate 名为 `mink`。`mink-core` 发布包不包含 REPL/TUI
实现；终端二进制和 UI 实现由 workspace 中的 `mink-cli` 包持有。服务端依赖时推荐只启用嵌入式 runtime：

```toml
mink = { package = "mink-core", version = "0.3.1", default-features = false, features = ["runtime"] }
```

`mink::runtime` / `mink::prelude` 解决这些问题：**同一套 OrchActor / TurnExecutor / ToolRunner 核心，但无进程边界**。

### 三入口共用核心

```text
mink CLI ──────────┐
mink-core SDK ─────┤
Rust crate mink ───┘
         │
    mink-cli::cli::main_entry()
         │
    runtime::build_runtime()
         │
    OrchActor::run()
```

两个二进制入口通过 `crates/mink-cli/src/cli.rs` 调用 `mink::runtime`，Rust 库调用方直接使用
`mink::runtime` / `mink::prelude`。三者最终都进入同一 `runtime::builder` 和 orchestrator 核心，不允许分叉逻辑。

### API 分层

| 层 | 类型 | 定位 |
|----|------|------|
| **lossless** | `AgentRuntimeConfig` + 完整 `Config` | 不丢失任何配置项 |
| **ergonomic** | `AgentOptions` builder | 常用字段快捷方法，`config_mut()` 逃生口 |
| **stream** | `AgentEventStream` | per-turn 实时事件，`recv()` + `outcome()` |

嵌入式调用方可通过 `AgentOptions::with_read_only_file_system()` 或
`AgentRuntimeConfig.read_only_fs` 注入 VFS，并通过
`AgentOptions::with_resource_session_id()` / `AgentRuntimeConfig.resource_session_id`
指定业务知识库分区。VFS trait 和请求/结果类型从 `mink::runtime` 导出。

### 关键设计决策

| 决策 | 理由 |
|------|------|
| `EventSink` 为同步 trait | 避免 async sink 引入背压复杂度，下游自行 channel |
| `TurnOutcome` 聚合 text/thinking | 调用方不订阅事件也能拿到结果 |
| `shutdown()` 5s grace period | 防止 orchestrator 死锁时无限等待 |
| `try_stream_turn()` 返回并发错误，`stream_turn()` 保持 panic 兼容 | 服务端可优雅处理并发冲突，旧 API 行为不变 |
| llm_override 仅 `#[cfg(test)]` | 不暴露生产 mock 能力 |

### 隐藏 worker 模式

私有化业务服务可以通过自身隐藏 worker 分支 + `sandbox::reexec_in_sandbox()` 实现进程级沙箱。沙箱配置走 argv，任务数据走 stdin（re-exec 后读），和 Mink CLI 流程完全一致。该隐藏分支属于业务服务实现细节，不要求 `mink` / `mink-core` 暴露新的公开 CLI。
