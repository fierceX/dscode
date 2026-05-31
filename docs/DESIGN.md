# 设计文档

## 主题一：Agent 主循环

### 单轮执行契约

`TurnExecutor::execute()` 是 agent 的核心循环，接收一个用户输入，执行零到多轮 LLM 调用，返回最终决策。一轮定义为一个 LLM 请求→响应→工具执行→继续/停止判断的完整周期。

```rust
// src/agent/turn.rs
pub async fn execute(&mut self, user_input: &str)
    -> Result<(TurnDecision, Vec<TurnEffect>)>
```

**TurnDecision** 有四类：

| 变体 | 含义 | 后续 |
|------|------|------|
| `Stop` | 正常结束（end_turn/stop） | 等待下个用户输入 |
| `Continue` | 有更多 LLM 调用 | 循环继续 |
| `Interrupted` | 被取消令牌中断 | 退出 |
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
  ├── ToolSignalProcessor 采集信号并更新 belief
  ├── PlanActionHandler 处理 PlanConfirm / PlanClear
  ├── SubAgentCoordinator 启动并收集子代理
  ├── ConversationStore::add_tool_results()
  └── Display::render_tool_result_detail()
步骤 7: DecisionEngine 决策继续、注入、中止或停止
```

各个阶段之间有严格的依赖关系：
- 步骤 2 使用 `TurnCompactor` 内部标记互锁，同一用户输入最多压缩一次
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
            messages = store.lines().await?;  // 刷新
            continue;  // 继续下一轮 LLM 调用
        }
        "end_turn" | "stop" => return Stop,
        "error" | "max_tokens" | "length" => return Failed,
        _ => return Stop,
    }
}
```

`messages` 在 tool_use 路径末尾通过 `store.lines()` 刷新，确保下一轮 LLM 调用看到最新工具结果、信号注入消息、计划变更和子代理结果。

---

## 主题二：内存模型

### 三层分离

内存模型分为三个独立的部分，各自有不同的生命周期和变更路径：

#### 1. ImmutablePrefix（不变前缀）

`src/session/prefix.rs`

承载 system prompt 和工具定义。一旦构建，在 session 期间应保持不变。变更会触发 fingerprint 失效，导致下一次 LLM 调用丢失前缀缓存。

```rust
pub struct ImmutablePrefix {
    system_prompt: String,
    tools_json: Vec<Value>,
    fingerprint: String,
}
```

**变更路径**（只有三条，每条都主动清除 fingerprint 缓存）：
- 上下文压缩后（新摘要 → 新的 system prompt）
- PlanClear/PlanConfirm 后（计划内容变更）
- SubAgent fork 模式继承

**fingerprint 校验**（`verify_fingerprint()`）：重新计算指纹并与缓存值比对。如果 verbose 模式下发现不一致，直接 panic，防止缓存偏移 bug 潜入生产。

#### 2. ConversationStore（追加日志）

`src/session/store.rs`

JSONL 格式的持久化消息存储。两类操作：

```rust
// 追加（正常路径——O(1)，不读盘）
Append: OpenOptions::append → write → flush → 更新内存缓存

// 截断（压缩路径——O(n)，重写文件）
Trim: 读取全量 → 保留末尾 N 条 → 写回 → 重建缓存
```

**内存缓存**：`cache: RwLock<Option<Vec<Value>>>`，延迟加载，append 时增量更新，trim 时重建。缓存失效策略：写操作从不直接将缓存设为 None，而是保持一致性。

**消息格式**：

```json
{"role":"user","content":"..."}
{"role":"assistant","content":[{"type":"thinking","thinking":"..."},{"type":"text","text":"..."},{"type":"tool_use","id":"...","name":"...","input":{}}]}
{"role":"user","content":[{"type":"tool_result","tool_use_id":"...","content":"..."}]}
```

assistant 消息使用 content 数组承载多种内容类型（thinking/text/tool_use），而不是扁平字段。这是因为 Anthropic/DeepSeek 的 content block 格式是结构化的。

#### 3. VolatileScratch（每轮清除）

每轮 LLM 调用开始时重置的状态：
- StormBreaker 窗口（`tools.reset_storm()`）
- `compacted_this_turn` 标记
- `thinking` 和 `text` 缓冲区（在每轮流式响应中累积）

这些状态不在 LLM 调用之间传递，保证每轮的独立性。

---

## 主题三：上下文压缩

### 触发条件

```rust
// src/session/compaction.rs
fn should_compact(&self, trigger: &str, context_tokens: usize) -> bool {
    if trigger == "plan_clear" || trigger == "plan_confirm" {
        return true;  // 计划操作强制压缩
    }
    let pct = (context_tokens * 100) / self.config.max_context_tokens;
    pct >= self.compact_pct as usize  // 默认 85%
}
```

`compact_pct` 通过 `.dscoderc` 的 `context_compact_pct` 配置，默认 85%。

### 三级 Tier

`CompactionTier::from_ratio()` 根据当前上下文使用率选择压缩等级：

| 使用率 | Tier | Keep 比例 | 行数限制 |
|-------|------|:---------:|:--------:|
| <70% | Conservative（不触发） | 20% | — |
| 70-80% | Aggressive（不触发） | 10% | — |
| 80-95% | ForceSummary | 5% | — |
| ≥95% | Emergency | 5% | 1-5 行 |

注意：默认 compact_pct=85%，所以首次压缩落在 ForceSummary 区间（80-95%）。Conservative 和 Aggressive 仅在 `context_compact_pct` 设为更低值时可达。

### turn 对齐截断

`compact_turn_keep()` 按 user 消息边界截断，保留末尾符合 keep_ratio 比例的用户轮次。截断后必须从 user 消息开始：

```rust
for i in (0..lines.len()).rev() {
    if found >= target { break; }
    keep += 1;
    if is_user[i] { found += 1; }
}
```

### 摘要生成

截断掉的会话被送入 LLM 生成摘要。摘要指令包含 fold marker：

```
[CONVERSATION HISTORY SUMMARY — earlier turns compacted for context efficiency]

Update the existing summary snapshot using the messages above.
Use exactly these fields:
Task focus:
Latest request:
Progress:
Tool evidence:
Reflections:
```

生成的摘要写入 `summary.txt`，在下一次 `build_system_prompt()` 时被读取为 `<context-snapshot>` 段。

### 防护措施

**同轮防护**（`turn.rs:compacted_this_turn`）：同一用户输入中的多次 LLM 调用（tool_use 循环）只压缩一次。

**最小收益检查**（`compaction.rs:96-105`）：如果压缩节省的 token 不足当前总量的 10%，跳过压缩。防止小上下文场景下的无意义压缩。

**Preflight 紧急压缩**（`turn.rs:128-148`）：基于 messages 的实际字节估算，如果 >95% max_context，在发送 LLM 请求前触发 Emergency 压缩。

---

## 主题四：维修流水线

三段流水线位于工具执行之前，按序处理：

### 步骤 1：Scavenge（回收）

`src/agent/turn.rs:228-253`

LLM 流式响应结束后，从 `thinking`（reasoning_content）和 `text`（普通文本）两个渠道中回收工具调用，补充到标准 `tool_calls` 字段解析出的 calls 列表中。

```rust
// turn.rs
let (scavenged, scavenge_notes) = scavenge_combined(
    Some(&thinking).filter(|s| !s.is_empty()),
    Some(&text).filter(|s| !s.is_empty()),
    4,  // max_calls
);
for sc in &scavenged {
    if !calls.iter().any(|c| c.name == sc.name) {
        calls.push(ToolCallEvent {
            name: sc.name,
            id: format!("scavenged_{}", calls.len()),
            input_json: serde_json::from_str(&sc.arguments).unwrap_or_default(),
            fields: BTreeMap::new(),
            order: Vec::new(),
        });
    }
}
```

回收尝试顺序（`scavenge_tool_calls`）：

1. **DSML invoke** — `<|DSML|invoke name="Read">` DeepSeek 专用标记语言，不经过标准 tool_calls 字段
2. **XML 包装** — `<tool_call>{...}</tool_call>`
3. **Bracket 包装** — `[TOOL_CALL]{...}[/TOOL_CALL]`
4. **裸 JSON** — 扫描自由文本中的 `{name, arguments}` 形状
5. **OpenAI style** — `{"type":"function","function":{"name","arguments"}}`
6. **R1 variant** — `{"tool_name":"Bash","tool_args":{...}}`

### 步骤 2：Truncation（截断修复）

`src/tools/runner.rs:82-96`

每个工具调用执行前，检查其 `input_json` 参数是否被截断。如果 JSON 不完整，尝试修复：

修复规则序列（`repair_truncated_json`）：

```
1. 快速路径：JSON.parse 成功 → 不变
2. 删除尾逗号：,} → }
3. 填补悬挂 key："key": → "key": null
4. 闭合未完成的字符串：末尾 " → 补 "
5. 逆序闭合：{ → }，[ → ]，" → "
6. 验证：修复后 JSON.parse → 成功取修复版，失败退化为 {}
```

### 步骤 3：StormBreaker（重复抑制）

`src/tools/runner.rs:53-75`

在工具执行前检查重复调用。每个工具调用的 `(name, args_json)` 进入滑动窗口。检测到同一对 `(name, args)` 连续出现 ≥3 次（窗口 6），则抑制该调用，返回抑制说明：

```rust
StormDecision::Suppress(reason) => {
    results.push(ToolRunResult {
        content: format!("Error: {reason}"),
        ...
    });
}
```

**Mutating 清空规则**：当 mutating 工具（Bash/Write/Edit）被调用时，清空窗口中的 read-only 条目。这允许 edit→re-read 模式正常执行。

**StormExempt**：WebSearch/WebFetch/Skill/SubAgent/PlanClear/PlanConfirm/TodoWrite 跳过风暴检测。

---

## 主题五：信号驱动的信念系统

### 设计思想

信号系统的设计根植于两个学科。从工程控制论出发，它构建了一个负反馈回路：信念度是系统的测量值，注入/中止是控制动作，LLM 行为是被控对象。信念度偏低时施加修正，回升后撤销干预——修正力始终与偏差反向。冷却机制对应控制论中的抗积分饱和（anti-windup），防止执行器因重复提示而过早饱和。

从贝叶斯统计出发，信念度不是拍脑袋的评分，而是 Beta-Binomial 模型的均值。α=3, β=1 的信任先验编码了"模型大概率能正确使用工具"的初始假设。每次工具调用是一次伯努利试验，多条错误信号取 max 不叠加——因为一次调用只有成功或失败两种真实状态，重复计数违反试验独立性。

两者交汇在置信度加权反馈：贝叶斯推断将离散的间接信号（退出码、错误文本）合成为一个稳定的统计量 B，控制论的回路再用这个 B 做决策。

### 信号采集流程

每次工具执行后，`SignalCollector` 根据工具输出和调用历史采集三类信号：

| 信号 | 检测方式 | 严重度 | 可信度 |
|------|---------|--------|--------|
| `ToolFailed` | 退出码检测 / `"Error:"` 前缀 | 1.0 | 最高（命令自报告失败） |
| `ToolError` | 输出内容 regex 匹配（Rust 编译错、测试失败等） | 0.3~0.9 | 中等（启发式） |
| `EditLoop` | 滑动窗口 W=6 检测编辑-检查循环 | 0.4~0.9 | 高（序列模式） |

**EditLoop 触发条件**：
- Edit 调用 > 4 次（窗口内），按次数分级 0.6/0.8/0.9
- Edit↔Diff 交替且无 Bash/Grep/Read，按交替数分级 0.4/0.7/0.9

`SignalCollector` 自维护调用历史（`VecDeque<String>`），跨多次工具调用追踪序列。

### BeliefTracker

信号通过拉普拉斯平滑合并为单一信念度：

```rust
// 一次工具调用的多个信号 → 取 max(severity)，不叠加
success_weight = 1.0 - max_severity
failure_weight = max_severity

// 滑动窗口（默认 16 次工具调用）
α = 1 + Σ success_weight_i
β = 1 + Σ failure_weight_i

B = α / (α + β) ∈ [0, 1]
```

**关键特性**：
- 信任先验 α=3, β=1：无观测时 B=0.75（模型调用工具大概率成功）
- max 合并：ToolFailed(0.9) + ToolError(0.8) → failure=1.0（不重复计数）
- 滑动窗口：旧错误自然退出，信念自动恢复
- 每轮用户输入重置窗口

### 信号来源

三类信号，两个确性定一条启发式：

| 信号 | 来源 | 检测方式 | 确定性 | 说明 |
|------|------|---------|--------|------|
| `ToolFailed` | 工具执行结果 | exit_code ≠ 0 或 `"Error:"` 前缀 | ✅ | 命令真失败了，权重统一 1.0 |
| `ToolError` | 输出文本 | regex 匹配 | ❌ | 输出中有错误关键词，权重 0.3~0.9 |
| `EditLoop` | 工具序列 (W=6) | Edit > 4 或 Edit↔Diff 交替 | ✅ | 盲写循环，权重分级 0.4~0.9 |

`ToolFailed` 和 `ToolError` 是两条独立链路——exit_code 通过 `child.status.code()` 从 bash 执行层获取，不经过输出文本 regex。

### DecisionEngine

`DecisionEngine` 由 `TurnExecutor` 持久持有，内部管理冷却计数器：

```rust
pub fn decide(&mut self, belief: f64, errors: &[String]) -> Decision {
    if belief < 0.30    → Abort（绕过冷却）
    if cooldown > 0     → cooldown -= 1 → None（跳过注入）
    if belief < 0.50    → Inject(warning + 最近 5 条错误) + 激活冷却
    if belief < 0.70    → Inject(reminder + 最近 3 条错误) + 激活冷却
    else                → None
}

// 新增方法
pub fn reset(&mut self)              // 新用户输入时清零冷却
pub fn cooldown_remaining(&self)     // 查询剩余冷却轮数
```

`decide()` 改为 `&mut self`，冷却计数器由引擎自主管理，调用方 turn.rs 不传任何冷却相关参数。

**注入位置：任务循环内部**。注入发生在 `turn.rs::execute()` 的循环内，工具执行完成后、下一轮 LLM 调用之前：

```
Phase 3: 工具执行 → 信号 → BeliefTracker.observe()
Phase 4: stop = "tool_use"
  ├─ DecisionEngine.decide(belief, errors)
  │   ├─ 引擎内部检查冷却 → 跳过注入
  │   ├─ Inject → store.add_user(...) + 激活冷却 + 恢复首步守卫
  │   └─ Abort  → 返回 Failed，中断本轮（绕过冷却）
  └─ continue → 下一轮 LLM: messages = store.lines()（包含注入消息）
```

不在系统 prompt 中注入（保护前缀缓存），也不追加到用户输入末尾。而是作为一条独立的 User 消息（含 `[System note: ...]`）写入对话存储，LLM 在下一轮调用时自然看到。

**注入内容包含具体可靠性信号**：LLM 收到进入 `SIGNAL_RECOVERY` 的短控制消息和最近信号：

```
[System note: belief 0.37 indicates repeated tool failure. Enter SIGNAL_RECOVERY mode as defined in the system instructions before any further repair momentum. Your next tool call must inspect current state with Read, Grep, Glob, or a focused Bash verification/state command; do not start with Edit or Write.
Recent reliability signals:
- Bash(cargo build): process exited with code 1
- Bash(cargo build): Rust compilation error (error[E0308])
- Grep(pattern="xxx"): No such file]
```

**信念度感知**：默认情况下系统提示词包含 `<belief-awareness>` 区块，提前告知模型存在信念度机制、注入触发条件和 `SIGNAL_RECOVERY` 协议。模型在被注入时能理解上下文，按指引先读后写，而不是继续盲目操作。区块位于 `verification-gate` 之后、`stop-triggers` 之前，纯英文。设置 `DSCODE_SIGNAL_MODE=off` 时，该区块不会出现在系统提示词中，信号采集、注入、Abort 和恢复守卫也不会运行。

**恢复首步守卫**：注入后，下一轮首个工具调用如果是 `Edit` 或 `Write`，`TurnExecutor` 会拒绝执行并返回 `SignalRecoveryGuard` 结果，要求先使用 `Read`、`Grep`、`Glob` 或聚焦的 `Bash` 检查当前状态。该守卫只约束注入后的第一步，避免模型忽略信号后继续盲改。

### 错误分类

`src/errors.rs`

```rust
pub enum ErrorCategory {
    Network,    // 连接/超时/5xx
    Auth,       // 401/403/API key 无效
    RateLimit,  // 429
    Parse,      // JSON/SSE 解析
    Tool,       // 工具执行失败
    Internal,   // 其他
}
```

分类仅用于日志，不驱动任何决策。

### 信念度展示

信念度实时显示在终端界面上：

- **TUI 模式**（`--tui`）：状态栏 `flash B:0.73 T:12 R:45 I:200K(50%)...`
- **REPL/CLI 模式**（`-i` / 单次）：ANSI 标题栏 `\x1b]0;...\x07` 相同格式

两种模式共享同一套统计数据结构（`StatsSnapshot`），每轮工具执行后由 `render_title_update()` 刷新。

## 主题六：工具执行模型

### 分发架构

`ToolRunner` 持有工具执行上下文、StormBreaker 和全局工具注册表：

```rust
pub struct ToolRunner {
    ctx: Arc<ToolContext>,
    storm: Mutex<StormBreaker>,
    tools: &'static [Box<dyn ToolExec>],
}
```

每个工具实现 `ToolExec`：

```rust
pub trait ToolExec: Send + Sync {
    fn name(&self) -> &'static str;
    fn mutating(&self) -> bool { false }
    fn storm_exempt(&self) -> bool { false }
    fn internal(&self) -> bool { false }
    fn spawns_sub_agent(&self) -> bool { false }
    fn execute(&self, input: &Value, ctx: &ToolContext) -> Result<ToolOutcome>;
}
```

内置工具通过 `TOOL_REGISTRY: LazyLock<Vec<Box<dyn ToolExec>>>` 注册。新增工具需要实现 `ToolExec`、加入 registry，并同步更新 `assets/tools.json`。

工具调用在 `execute_all()` 中批量处理，每个调用在一个独立 `spawn_blocking` 任务中执行。这允许同步工具（Read/Write/Bash/Python 等）不阻塞 async agent 循环。

```rust
pub async fn execute_all(&self, calls: Vec<ToolCallEvent>) -> Result<Vec<ToolRunResult>> {
    for call in calls {
        // Storm check
        // Truncation repair
        // ToolExec lookup
        handles.push(spawn_blocking(move || execute_one_sync(&ctx, &call, tool)));
    }
    // 等待所有 handles
}
```

### 单工具执行

`execute_one_sync()` 根据 `call.name` 从 registry 找到对应 `ToolExec`。每个工具在自己的实现中通过 serde 从 `call.input_json` 反序列化参数：

```rust
impl ToolExec for ReadTool {
    fn name(&self) -> &'static str { "Read" }
    fn execute(&self, input: &Value, ctx: &ToolContext) -> Result<ToolOutcome> {
        let args: Args = serde_json::from_value(input.clone())?;
        read_with_context(&args.path, args.offset, args.limit, ctx).map(ToolOutcome::text)
    }
}
```

如果 serde 反序列化失败（参数不匹配），返回 `Error:` 前缀的错误消息，而不是 panic。这确保 LLM 收到结构化的错误反馈而非原始异常。

### 结果格式化

工具结果统一经过 `format_tool_result()` 处理：

- 超过 `tool_result_max_bytes`（默认 100KB）时截断，保留头尾
- Bash 输出经过 `filter_bash_noise()` 处理（ANSI 转义剥离 + 重复行压缩）
- Read/Write 结果加上行数/字节数统计前缀
- Edit 默认将首行作为 `conv_content`，减少 conversation 噪声

工具结果有两个通道：

| 字段 | 用途 |
|------|------|
| `content` | UI 展示和默认 tool_result 内容，已过最大字节保护 |
| `conv_content` | 非空时优先进入 LLM conversation，适合给模型更短的结果 |

`TurnExecutor` 渲染工具结果时会构造 `ToolResultDisplay`：

```rust
ToolResultDisplay {
    tool_name,
    content_preview,
    content,
    tool_use_id,
    exit_code,
}
```

`content_preview` 用于简短展示，`content` 是工具层截断/过滤后的展示内容。LLM 读取的是 `ConversationStore` 写入的 tool result。

---

## 主题七：SSE 流式解析

### 解析器状态机

`OpenAIParser` 是一个增量状态机，处理 `data: {...}\n\n` 格式的 SSE 帧：

```rust
pub struct OpenAIParser {
    stop_reason: String,
    input_tokens: i64,
    output_tokens: i64,
    cache_read_input_tokens: i64,
    saw_text: bool,
    pending_calls: BTreeMap<i64, PendingCall>,
    pending_usage: Option<UsageEvent>,
    marker_buf: String,
    pub needs_pro_detected: bool,
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

OpenAI API 只在最后一个 chunk（标记为 `[DONE]`）中提供完整的 usage 信息。Parser 在收到 `[DONE]` 时设置 `pending_usage`，然后在 `flush()` 中发出。

---

## 主题八：Session 与持久化

### 目录布局

每个 session 有独立的目录：

```
~/.dscode/projects/<project_key>/<session_id>/
├── conversation.jsonl   ← 对话消息（逐行追加 JSON）
├── events.jsonl          ← 事件日志（每行一个事件）
├── summary.txt           ← 压缩后的上下文快照
├── plan.md               ← 确认后的执行计划
├── plan.draft            ← 草稿计划
└── stats.json            ← Token 用量统计
```

`project_key` 是当前工作目录路径经过安全转义后的字符串，确保不同项目间的 session 隔离。

### JSONL 约束

**追加**：`append_line()` 使用 `OpenOptions::append` 写入，原子级单行追加。同时更新内存缓存。

**读取**：`lines()` 延迟加载缓存，首次读盘后缓存到 `RwLock<Option<Vec<Value>>>`。

**截断**：`trim_keep_last(k)` 是唯一修改历史文件的路径。读入全量 → 截取末尾 k 条 → 覆写 → 重建缓存。

### Session 恢复

`--continue` 模式通过读取最新 session 目录的时间戳来选择最近的 session。`--session NAME` 直接使用指定名称。

恢复时会 replay 最近 10 轮 LLM 响应事件（从 events.jsonl 读取），在交互式终端重新渲染历史对话。

---

## 主题九：SubAgent（子代理）

### 隔离执行

`SubAgentExecutor` 为每个子代理创建一个完全独立的 `AgentSharedContext`：

```rust
pub async fn new(parent_ctx, session_id, fork) -> Result<Self> {
    // 1. 创建子 session 目录 + 文件
    // 2. fork 模式：复制父会话 conversation/summary/plan
    // 3. 创建独立的 ConversationStore + StatsTracker
    // 4. 创建独立的 CompactionEngine
    // 5. 继承 cancel token（父取消→子取消）
    // 6. 独立的 immutable_prefix
}
```

### 两种模式

**独立模式（默认）**：子 session 从空白上下文开始。继承父 session 的模型、API URL、工具集，但不继承对话历史。

**Fork 模式**：复制父 session 的 conversation.jsonl、summary.txt、plan.md 到子 session 目录。子代理从父代理离开的地方继续执行。用于需要父会话上下文的延续性任务。

### 结果收集

子代理完成时，从子 session 的对话历史中读取最后一条 assistant 消息的 thinking 和 text，作为结果返回。不包含工具调用细节——只返回 LLM 的最终输出。

```rust
for line in child_store.lines().rev() {
    if line.role == "assistant" {
        // 提取第一个 thinking block 和第一个 text block
        break;
    }
}
```

### 并发控制

`SubAgentPool` 使用 `tokio::sync::Semaphore` 限制最大并发数（默认 8）。每个子代理占用一个 permit，完成后释放。

结果通过 `mpsc::UnboundedSender` 发送回 orchestrator，由 `handle_sub_agent_result()` 注入父会话。

---

## 主题十：配置系统

### 合并优先级

```
CLI 参数 > 环境变量 > 代码默认值
```

`config.rs` 中，`parse_args()` 优先解析 CLI 参数，`apply_provider_defaults()` 再补充环境变量和默认值：

```rust
pub fn apply_provider_defaults(cfg: &mut Config) -> Result<()> {
    // 1. 环境变量覆盖特定字段
    if let Ok(v) = std::env::var("TOOL_RESULT_MAX_BYTES") { ... }
    if let Ok(v) = std::env::var("FILE_WRITE_MAX_BYTES") { ... }
    // 2. API Key: DEEPSEEK_API_KEY > OPENAI_API_KEY > CLI 参数
    // 3. Base URL: DEEPSEEK_BASE_URL > OPENAI_BASE_URL > CLI 参数 > 默认
    // 4. 模型默认: deepseek-v4-flash
    // 5. 验证: API Key 或 Base URL 至少一个存在
```

### size 解析

`parse_size_bytes()` 支持 `k`/`m`/`g` 后缀：

```rust
"100"   → 100
"1k"    → 1000
"500K"  → 500000
"1m"    → 1000000
"2M"    → 2000000
```

用于 `--max-context`、`--max-tokens` 等参数。

### 环境变量分类

| 类别 | 变量 | 用途 |
|------|------|------|
| API | `DEEPSEEK_API_KEY`, `DEEPSEEK_BASE_URL` | 认证和端点 |
| 大小 | `TOOL_RESULT_MAX_BYTES`, `FILE_WRITE_MAX_BYTES` | 输出限制 |
| Web | `JINA_API_KEY` | WebSearch / WebFetch 认证 |
| 信号 | `DSCODE_SIGNAL_MODE` | `full` 启用信号系统，`off` 关闭信号提示词和运行时干预 |
| 沙箱 | `DSCODE_LIMITS` | JSON 格式 sandbox 限制配置 |
| 调试 | `LOG_EVENTS`, `DSCODE_HOME` | 日志和 session 路径 |

`context_compact_pct` 通过 `.dscoderc` 配置。

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
│  spawn_blocking()  spawn_blocking()  ...        │
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
4. 子代理收集循环停止等待

---

## 主题十二：系统提示词构建

`prompt.rs` 的 `Builder::build_system_prompt()` 按固定顺序组装系统提示词段：

```
<agent-identity>          ← 你是谁（中/英根据 locale）
<environment>             ← 当前工作目录、shell、平台
<rules>                   ← 行为规则
<tool-selection>          ← 工具选择原则
<safety>                  ← 安全边界
<verification-gate>       ← 验证门控
<belief-awareness>        ← 信号系统协议（DSCODE_SIGNAL_MODE=full 时）
<stop-triggers>           ← 停止条件
<output-discipline>       ← 输出纪律
<using-your-tools>        ← 工具使用说明
<sub-agent-guidance>      ← 子代理使用指引
<todo-guidance>           ← Todo 操作指引
<plan-lifecycle-guidance> ← Plan 生命周期
<instruction-files>       ← AGENTS.md / CLAUDE.md
<skill-index>             ← 可用的 skill 列表
<selected-skills>         ← 加载的 skill 内容
<current-plan>            ← plan.md（如果有）
<context-snapshot>        ← summary.txt（如果有，压缩后写入）
<output-language>         ← 输出语言要求
```

每个段都是可选的（空内容跳过）。这种设计使得压缩只影响 `<context-snapshot>` 段，其他段保持不变。

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
| compact 后 messages 必须刷新 | `turn.rs:96` | LLM 发送过期数据 |
| compact 后 system_prompt 必须重建 | `turn.rs:97` | 摘要信息丢失 |
| StormBreaker 窗口每用户输入重置 | `turn.rs:87` | 跨意图抑制误判 |
| TurnFailureTracker 每轮重置 | `orchestrator.rs:90` | 跨轮信号累积误升级 |
| 同轮最多压缩一次 | `turn.rs:106,128` | 多次无用压缩 |
| ImmutablePrefix 只能通过 `invalidate_prefix` 变更 | `turn.rs:68-69` | 缓存偏移不可检测 |
| store 写操作不将缓存设为 None | `store.rs` | 读盘性能下降 |
