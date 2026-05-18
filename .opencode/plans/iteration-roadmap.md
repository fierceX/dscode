# Bash-Agent 新一代迭代路线图

**目标**：单 DeepSeek 1M 上下文，简洁，模块化，高缓存命中率，轻量级，信号驱动的工程控制论 agent code 工具。

**基线**：36 文件, 5650 行, 74 测试, 0 失败。**目标**：~43 文件, ~6600 行, ~116 测试。

---

## 设计原则

1. **信号驱动优于启发式** — 基于可测量信号（实际 token 使用量、实际执行耗时、实际错误分类），不做文本关键词匹配推断状态
2. **减原则** — 每个功能追问"去掉会怎样"，不重复造轮子（单压缩方案，不加 cycle manager）
3. **缓存对齐优先** — ImmutablePrefix 分离是一切 cache-hit 优化的基础，对标 Reasonix 99.82%
4. **轻量级** — 目标 ~6600 行，远低于 TUI 的 ~50000 行
5. **模块独立** — 每个优化改 1-2 文件，不引入跨文件状态耦合

---

## 源文档追溯

本文档综合以下分析文档：

| 文档 | 路径 | 核心贡献 |
|------|------|---------|
| 控制论优化评判 | `.opencode/plans/cybernetic-verdict.md` | 16项评估 → 保留7项 (~145行) |
| 控制论优化补充篇 | `.opencode/plans/cybernetic-optimization-plan-2.md` | 8项新增优化详细方案 |
| Auto-Model 设计 | `.opencode/plans/auto-model-design.md` | 双模型切换方案 (~181行) |
| 跨项目分析 | `.opencode/plans/cross-project-analysis.md` | DeepSeek-Reasonix + DeepSeek-TUI → 12可迁移模式 |

---

## Phase 1: 基础设施 + 控制平面

**目标**：建立信号系统、可测试架构、内存模型、控制回路。

**代码增量**：~545 行。**测试增量**：+22 (96 total)。**新文件**：3。

### 1.1 错误分类学

- **文件**：`src/errors.rs` (新建, ~60行)
- **来源**：cybernetic #10 (故障隔离) + cross-project #8 (ErrorCategory/Severity)
- **设计**：

```rust
/// 错误类别 — 决定 auto-model 升级权重
pub enum ErrorCategory {
    Network,     // 连接/超时 → 升级权重 0，不触发模型切换
    Auth,        // 401/403 → 升级权重 0，不应升级
    RateLimit,   // 429 → 升级权重 0，应等待而非升级
    Parse,       // JSON/SSE 解析错误 → 升级权重 2，flash 能力不足信号
    Tool,        // 工具执行失败 → 升级权重 1
    Internal,    // 内部 bug → 升级权重 0，升级模型无济于事
}

/// 严重性 — 决定是否可恢复
pub enum ErrorSeverity {
    Warning,     // 可忽略，不影响 turn 结果
    Error,       // 需要处理但 turn 可继续
    Fatal,       // 终止当前 turn
}

/// 用 Category + Severity 标注任意错误
pub fn classify_error(err: &dyn std::error::Error) -> (ErrorCategory, ErrorSeverity) { ... }
pub fn is_upgrade_signal(cat: ErrorCategory) -> bool { ... }
```

- **测试场景**（5 个）：
  - Network 错误 → category=Network, upgrade_signal=false
  - 401 → category=Auth, upgrade_signal=false
  - JSON parse → category=Parse, upgrade_signal=true
  - bash 命令失败 → category=Tool, upgrade_signal=true
  - anyhow!("内部错误") → category=Internal

### 1.2 LlmClient trait 提取 + Mock

- **文件**：`src/llm/client.rs` (修改, +30行) + `src/llm/mock.rs` (新建, ~80行)
- **来源**：cross-project #10 (Mock LLM Client) — TUI 200 行 integration 测试全标记 `#[ignore]` 的教训
- **设计**：

```rust
#[async_trait]
pub trait LlmClient: Send + Sync {
    async fn stream(&self, ctx: &AgentSharedContext, msgs: &[Message],
                    events_tx: mpsc::Sender<Event>) -> Result<(), Error>;
    fn model(&self) -> &str;
}

// 现有实现
pub struct AsyncLlClient { ... }
impl LlmClient for AsyncLlClient { ... }

// Mock 实现（测试用）
pub struct MockLlmClient {
    canned_responses: Vec<String>,  // 预设 SSE 事件序列
    calls: Mutex<Vec<Vec<Message>>>, // 记录每次调用的 messages
}
```

- **关键**：让 auto-model / turn loop / sub executor 全部通过 trait bound 注入，无需 `Option<ConcreteClient>`。

- **测试场景**（4 个）：
  - MockCanned(empty) → stream returns with no tool calls
  - MockCanned(tool_call) → events_tx receives ToolCallStarted
  - MockCanned(error) → returns Err
  - MockCanned(multi_turn) → 3 tool call rounds

### 1.3 ImmutablePrefix 三部曲内存模型

- **文件**：`src/session/prefix.rs` (新建, ~80行) + `src/session/store.rs` (修改) + `src/agent/turn.rs` (修改)
- **来源**：cross-project #6 — Reasonix 99.82% cache hit 的核心秘密
- **设计**：

```
ImmutablePrefix
  ├─ system_prompt_text    (SHA-256 指纹化 → 可验证缓存稳定性)
  ├─ tool_definitions      (JSON schemas，构建消息时拼接)
  ├─ few_shot_examples     (可选)
  └─ fingerprint: String   (sha256(system_prompt + tools))

AppendOnlyLog              (当前 ConversationStore 改名)
  ├─ messages: Vec<Message>
  ├─ append(msg)           (唯一写操作，compact 时 replace_range)
  └─ clear_scratchable()   (每轮清除 volatile content)

VolatileScratch
  ├─ thinking_content      (每轮重置)
  ├─ plan_state            (每轮重置)
  └─ notes                 (每轮重置)
```

- **构建消息的流程变更**：
```
旧：context.messages — 把所有消息喂给 LLM
新：prefix.to_messages() + append_only.messages + scratch.as_context_hint()
```
LLM 实际收到的消息列表 = immutable 部分 + append-only 部分 + scratch hint（不跨轮传递）。

- **价值**：`ImmutablePrefix` 在 session 生命周期内永不变化 → system prompt + tool defs 的 prefix 完全固定 → DeepSeek 的 prefix-cache 次次命中。这是**最大单次 cache-hit 优化**。

- **测试场景**（4 个）：
  - SHA-256 确定性：相同 system_prompt + tools → 相同 fingerprint
  - fingerprint 变更：tools 变化 → fingerprint 变化
  - append-only 约束：append() 后 messages.len()++
  - scratch 清理：clear_scratchable() → 内容归空

### 1.4 Auto-Model 双模型切换（增强版）

- **文件**：`src/agent/orchestrator.rs` (修改, +60行) + `src/config.rs` (修改, +50行) + `src/agent/turn.rs` (修改, +20行) + `src/llm/client.rs` (修改, +25行) + `src/llm/provider.rs` (修改, +15行) + `src/session/stats.rs` (修改, +15行) + `src/sse/parser.rs` (修改, +15行)
- **来源**：auto-model-design.md (基础方案) + cross-project #1 (多信号融合) + cross-project #2 (自报告升级)
- **总行数**：~200行

#### 控制模型

```
OrchActor (Supervisory Control)
  ├─ active_model: "deepseek-chat"       ← 当前活跃模型
  ├─ active_provider: Provider::OpenAI   ← 当前活跃后端
  ├─ upgrade_score: 0                    ← 多信号加权累加器
  ├─ model_locked: false                 ← Hysteresis lock（升级后永不降级）
  ├─ upgrade_threshold: 4                ← Bang-bang 触发阈值（可配置）
  │
  │  resolve_active():
  │    if model_locked → pro
  │    if upgrade_score ≥ threshold → switch_to_pro() → lock
  │    else → flash
  │
  ▼
  TurnDecision arrives → update_after_turn(decision, errors)
    ├─ success + not locked → reset score
    ├─ error → add_error_signal(category) → upgrade_score += weight(category)
    ├─ <<<NEEDS_PRO>>> detected in stream → upgrade_score += 3（高优先级）
    └─ if score ≥ threshold → switch + lock
```

#### 信号加权系统

| 信号源 | 检测位置 | 权重 | 说明 |
|--------|---------|------|------|
| `TurnDecision::Failed` | `turn.rs` | 3 | 原方案，覆盖 stream 失败/chunk 错误/max_tokens |
| `ErrorCategory::Parse` | `sse/parser.rs` | 2 | SSE 解析失败 — flash 模型输出格式不稳定 |
| `ErrorCategory::Tool` | `agent/orchestrator.rs` | 1 | 工具执行失败 — 可能 flash 参数质量低 |
| `<<<NEEDS_PRO>>>` 标记 | `sse/parser.rs` (stream中检测) | 3 | 自报告升级 — "模型即传感器"（可选特性） |
| `ErrorCategory::Network` | `llm/client.rs` | 0 | 网络错误不触发升级（升级模型无济于事） |
| `ErrorCategory::Auth` | `llm/client.rs` | 0 | 认证错误不触发升级 |
| `ErrorCategory::RateLimit` | `llm/client.rs` | 0 | 限流不触发升级 |

#### 配置

```rust
// config.rs 新增字段
pub auto_model: bool,                 // --auto-model / AUTO_MODEL=true
pub secondary_model: String,          // --secondary-model (deepseek-reasoner)
pub auto_upgrade_threshold: u32,     // --auto-threshold (默认 4)
pub auto_self_report: bool,          // --auto-self-report (默认 false, 可选特性)
```

#### 降级策略

不自动降级 (Pro → Flash)。Hysteresis lock：一旦升级，`model_locked=true` 永不降级——Pro 失败的代价（钱）远低于 Flash 失败的代价（用户时间）。

#### 子 Agent 行为

子 agent 继承 `auto_model` + `upgrade_threshold`，但独立维护 `upgrade_score` + `model_locked`。子 agent Flash 升级不影响父 agent。

#### SSE 流中检测 `<<<NEEDS_PRO>>>`

`SseParser` 在处理 delta 事件时维护一个标记缓冲区（最后 50 个字符），匹配 `<<<NEEDS_PRO>>>` 模式时触发信号。该标记不传递给 display（在 parser 层消费）。

- **测试场景**（6 个）：
  - Flash 连续 2 次 Parse 错误 → score=4 → 升级
  - 单次 Network 错误 → score=0 → 不升级
  - `<<<NEEDS_PRO>>>` 在流中 → score+=3
  - 升级成功 → model_locked=true → 不再降级
  - auto_model=false → 完全不变
  - 子 agent 独立升级 → 父 agent 不受影响

### 1.5 监督控制退化

- **文件**：`src/agent/orchestrator.rs` (修改, ~25行)
- **来源**：cybernetic #14 (Consecutive Failure Degradation)

```
连续失败次数 → 响应:
  2 → 追加 guidance："Previous turns had errors. Double-check tool parameters before calling."
  3 → 削减 max_tokens 到 50%
  4 → 建议用户 "/pro" 或 Ctrl-C
```

与 auto-model 的区别：auto-model 升级模型，监督退化调整参数。两者互补——先升级模型（1.4），如果 pro 也失败再走参数退化（1.5）。

- **测试场景**（2 个）：
  - 2 次连续失败 → guidance 注入到 system prompt
  - 3 次连续失败 → max_tokens 削减到 50%

### 1.6 前馈控制

- **文件**：`src/prompt.rs` (修改, ~10行)
- **来源**：cybernetic #5 (Feedforward，追加模式)

根据用户任务关键词在 system prompt 尾部**追加** 1-2 行特化指导（不替换！保留通用 guidance）。

```rust
fn add_feedforward_hint(user_input: &str, system_prompt: &mut String) {
    if contains_any(user_input, &["bug", "fix", "debug", "调试", "修复"]) {
        push("For debugging: prefer Grep→Read→Edit workflow.");
    }
    if contains_any(user_input, &["refactor", "架构", "重构"]) {
        push("For refactoring: use PlanConfirm before making large-scale changes.");
    }
}
```

- **测试场景**（1 个）：
  - "fix bug" → 追加 debugging hint

---

### Phase 1 汇总

| 子项 | 新文件 | 修改文件 | 行数 | 测试 |
|------|--------|----------|------|------|
| 1.1 错误分类学 | `errors.rs` | — | 60 | 5 |
| 1.2 LlmClient trait | `llm/mock.rs` | `llm/client.rs`, `agent/turn.rs` | 110 | 4 |
| 1.3 ImmutablePrefix | `session/prefix.rs` | `session/store.rs`, `agent/turn.rs` | 140 | 4 |
| 1.4 Auto-Model | — | `orchestrator.rs`, `config.rs`, `turn.rs`, `llm/client.rs`, `provider.rs`, `stats.rs`, `sse/parser.rs` | 200 | 6 |
| 1.5 监督控制退化 | — | `orchestrator.rs` | 25 | 2 |
| 1.6 前馈控制 | — | `prompt.rs` | 10 | 1 |
| **合计** | **3 新文件** | **~10 修改文件** | **~545** | **+22 (96)** |

---

## Phase 2: 上下文架构 + 鲁棒性

**目标**：多层次上下文管理、工具调用修复、循环检测。

**代码增量**：~245 行。**测试增量**：+14 (110 total)。**新文件**：3。

### 2.1 三级压缩带 + 防颤

- **文件**：`src/compact_dp.rs` (修改, ~50行)
- **来源**：cross-project #3 (50/70/80/95% 比例控制带) + cybernetic #4 (防颤 Hysteresis)

```
CompactionTier:
  Conservative   (50%)  → DP_BETA=0.02, 保留 20% tail budget, min_interval=0
  Aggressive     (70%)  → DP_BETA=0.05, 保留 10% tail budget, min_interval=1
  ForceSummary   (80%)  → DP_BETA=0.10, 强制总结,             min_interval=0
  Emergency      (95%)  → DP_BETA=0.50, 阻断式紧急压缩,       min_interval=0

Hysteresis: 从 Aggressive→Conservative 降级需 ≥3 轮间隔（防止边界振荡）
```

- **关键**：Conservative tier 行为 = 当前单阈值行为。向后兼容——`dp_decision()` 增加 tier 参数（默认 Conservative）。

- **测试场景**（3 个）：
  - 50% → Conservative tier, DP_BETA=0.02
  - 75% → Aggressive tier, DP_BETA=0.05
  - 连续 2 轮 70% → 不清算 Conservative（间隔 < 3）

### 2.2 在线 DP 参数估计

- **文件**：`src/compact_dp.rs` (修改, ~20行)
- **来源**：cybernetic #13 (v/s EWMA 在线更新)

```rust
fn record_compact_result(&mut self, actual_tokens: u64) {
    // EWMA α=0.3
    self.s = 0.3 * (actual_tokens as f64) + 0.7 * self.s;  // ∑_i μ_i estimate
    self.v = 0.3 * (self.s / self.turns) + 0.7 * self.v;   // average per turn
}
```

`record_compact_result()` 在 `run_summary_call()` 成功后调用——使用 `new_tokens` 作为 `μ_i` 的实际值。零成本反馈改善 DP 决策精度。

- **测试场景**（2 个）：
  - 首次 compact 后 s/v 更新
  - EWMA 稳定收敛

### 2.3 Preflight 预判门

- **文件**：`src/agent/turn.rs` (修改, ~15行)
- **来源**：cross-project #5 (Reasonix preflight check)

```rust
// turn.rs build_messages() 后、stream() 前
let estimated_tokens = messages.iter().map(|m| m.content.len() / 4).sum::<usize>();
if estimated_tokens > (context_window * 95 / 100) {
    ctx.compact().await?;  // 触发 Emergency tier 压缩
    messages = ctx.build_messages().await?;  // 重建
}
```

- **测试场景**（1 个）：
  - >95% context → preflight compact 触发

### 2.4 Scavenger + Flattener 修复流水线

- **文件**：`src/repair/scavenge.rs` (新建, ~50行) + `src/repair/flatten.rs` (新建, ~50行)
- **来源**：cross-project #7 (Reasonix 四层修复流水线，取其二)

**Scavenger** — 从文本中抢救 tool_calls：
```
输入：LLM 输出的 block_text（JSON 解析失败时）
逻辑：
  1. 查找模式: <tool_call>{"name":..., "arguments":{...}}</tool_call>
  2. 查找模式: [TOOL_CALL] ... [/TOOL_CALL]
  3. 查找模式: {"name":..., "arguments":{...}}（裸 JSON）
输出：解析后的 ToolCall 或 None
```

**Flattener** — 展开 dot-notation 参数：
```
输入: {"tool.name.sub": {"key": "val"}}
输出: {"tool": {"name": "name.sub"}, "arguments": {"key": "val"}}
```

**集成点**：`SseParser` 的 `finish_block()` 中，当 `serde_json::from_str` 失败时调用修复流水线。

- **测试场景**（5 个）：
  - scavenge: `<tool_call>{"name":"bash","arguments":{"cmd":"ls"}}</tool_call>` → 成功解析
  - scavenge: 裸 JSON → 成功解析
  - scavenge: 无 tool_call → None
  - flatten: `{"file.write": {...}}` → `{"file": {"write": {...}}}`
  - flatten: 无 dot-notation → 无变化

### 2.5 Storm Breaker 重复调用抑制

- **文件**：`src/guard/storm.rs` (新建, ~60行)
- **来源**：cross-project #4 (Reasonix bang-bang 抑制)

```rust
pub struct StormBreaker {
    window: VecDeque<(String, String)>,  // (tool_name, args_json)
    max_window: usize,                    // 6
    threshold: usize,                     // 3
}

impl StormBreaker {
    pub fn check(&mut self, name: &str, args: &str) -> StormDecision {
        let key = (name.to_string(), args.to_string());
        self.window.push_back(key.clone());
        if self.window.len() > self.max_window {
            self.window.pop_front();
        }
        let count = self.window.iter().filter(|k| k == &key).count();
        if count >= self.threshold {
            StormDecision::Suppress  // 抑制，不执行
        } else {
            StormDecision::Allow
        }
    }
}

pub enum StormDecision { Allow, Suppress }
```

**触发抑制时**：不执行工具调用 → 返回 structured error → LLM 看到 "Tool call suppressed: detected repeated identical calls. Rephrase or try a different approach."

- **测试场景**（3 个）：
  - 3 次相同 (bash, "ls") → Suppress
  - 2 次相同 → Allow
  - 出现中间不同调用 → 窗口推进，旧调用过期

---

### Phase 2 汇总

| 子项 | 新文件 | 修改文件 | 行数 | 测试 |
|------|--------|----------|------|------|
| 2.1 三级压缩带 | — | `compact_dp.rs` | 50 | 3 |
| 2.2 在线 DP 参数 | — | `compact_dp.rs` | 20 | 2 |
| 2.3 Preflight 门 | — | `agent/turn.rs` | 15 | 1 |
| 2.4 修复流水线 | `repair/scavenge.rs`, `repair/flatten.rs` | — | 100 | 5 |
| 2.5 Storm Breaker | `guard/storm.rs` | — | 60 | 3 |
| **合计** | **3 新文件** | **2 修改文件** | **~245** | **+14 (110)** |

---

## Phase 3: 精细化 + 流可靠性

**目标**：减少上下文噪声、智能超时、流级容错。

**代码增量**：~75 行。**测试增量**：+6 (116 total)。**新文件**：0。

### 3.1 噪声滤波

- **文件**：`src/tools/runner.rs` (修改, ~15行)
- **来源**：cybernetic #7 (Low-pass Filter，仅 Bash 输出)

```rust
fn filter_bash_output(raw: &str) -> String {
    let no_ansi = strip_ansi_escapes::strip_str(raw);  // 去 ANSI 颜色码
    // 压缩连续完全相同的行
    let lines: Vec<&str> = no_ansi.lines().collect();
    let mut out = Vec::new();
    let mut repeat_count = 0;
    for i in 0..lines.len() {
        if i > 0 && lines[i] == lines[i-1] {
            repeat_count += 1;
        } else {
            if repeat_count > 0 {
                out.push(format!("  [previous line repeated {} times]", repeat_count));
                repeat_count = 0;
            }
            out.push(lines[i].to_string());
        }
    }
    out.join("\n")
}
```

- **价值**：编译输出的 `[ 1/100] [ 2/100]...` 重复 100 次对 LLM 无价值，压缩为 1 行 + `[repeated 99 times]`。

- **测试场景**（2 个）：
  - ANSI 颜色码 → 清除
  - 10 行相同输出 → 压缩为 1 行 + repeat note

### 3.2 自适应工具超时

- **文件**：`src/tools/bash.rs` (修改, ~30行)
- **来源**：cybernetic #16 (Adaptive Deadline)

```rust
pub struct AdaptiveTimeout {
    history: VecDeque<Duration>,  // 最近 10 次 Bash 实际耗时
    default: Duration,             // 30s fallback
}

impl AdaptiveTimeout {
    pub fn compute(&self) -> Duration {
        if self.history.len() < 5 {
            self.default
        } else {
            let mut sorted: Vec<_> = self.history.iter().copied().collect();
            sorted.sort();
            let median = sorted[sorted.len() / 2];
            median * 3  // 3倍中位数，覆盖长尾
        }
    }

    pub fn record(&mut self, elapsed: Duration) {
        self.history.push_front(elapsed);
        self.history.truncate(10);
    }
}
```

- **测试场景**（2 个）：
  - <5 条历史 → 返回 default 30s
  - 10 条历史 median=1s → timeout=3s

### 3.3 流内重试

- **文件**：`src/llm/client.rs` (修改, ~30行)
- **来源**：cross-project #9 (Intra-stream + Transparent retry)

```
Layer 1 (Intra-stream): 连续 decode error < 5 → 继续 stream
Layer 2 (Transparent):  流中未收到任何内容 → 静默重发请求 (最多 2 次)
```

```
MAX_STREAM_ERRORS = 5
MAX_TRANSPARENT_RETRIES = 2

如果 any_content_received = false（还未收到任何有意义内容）
  → retry_count < MAX_TRANSPARENT_RETRIES
  → 重新创建 stream（不通知用户，不修改 messages）

如果 any_content_received = true
  → decode error += 1
  → 如果 decode error < MAX_STREAM_ERRORS → 继续
  → 否则 → 失败
```

- **测试场景**（2 个）：
  - 前 2 次无内容的 stream 失败 → 第 3 次成功 → 返回内容
  - 收到内容后 decode error < 5 → 继续

---

### Phase 3 汇总

| 子项 | 新文件 | 修改文件 | 行数 | 测试 |
|------|--------|----------|------|------|
| 3.1 噪声滤波 | — | `tools/runner.rs` | 15 | 2 |
| 3.2 自适应超时 | — | `tools/bash.rs` | 30 | 2 |
| 3.3 流内重试 | — | `llm/client.rs` | 30 | 2 |
| **合计** | **0 新文件** | **3 修改文件** | **~75** | **+6 (116)** |

---

## Phase 4: 运维与连接韧性（延后）

> 确认延后到 Phase 4，Phase 1-3 完成后视情评估。

| 子项 | 来源 | 行数 | 说明 |
|------|------|------|------|
| 4.1 会话检查点 | cross-project #15 | ~80 | --resume/--continue，依赖 ImmutablePrefix 稳定 |
| 4.2 连接健康跟踪 | cross-project #13 | ~20 | Healthy/Degraded/Recovering，DeepSeek API 稳定性较高时收益低 |
| — | **合计** | **~100** | |

---

## 全局视图

### 路线图

```
Phase 1 ────────────────── Phase 2 ──────────── Phase 3 ─────── ─ Phase 4 (future)
│                             │                    │                   │
├─ 1.1 错误分类学 (errors.rs)  ├─ 2.1 三级压缩带    ├─ 3.1 噪声滤波     ├─ 4.1 会话检查点
├─ 1.2 LlmClient (mock.rs)    ├─ 2.2 在线DP参数      ├─ 3.2 自适应超时   ├─ 4.2 连接健康
├─ 1.3 ImmutablePrefix         ├─ 2.3 预判门          └─ 3.3 流内重试     └─ (tbd)
├─ 1.4 Auto-Model (增强版)     ├─ 2.4 修复流水线
├─ 1.5 监督控制退化            └─ 2.5 Storm Breaker
└─ 1.6 前馈控制
  ~545行                        ~245行                ~75行             ~100行
  +22 测试                     +14 测试             +6 测试
```

### 文件变更总览

| 操作 | Phase 1 | Phase 2 | Phase 3 | 合计 |
|------|---------|---------|---------|------|
| **新建** | `errors.rs`, `llm/mock.rs`, `session/prefix.rs` | `repair/scavenge.rs`, `repair/flatten.rs`, `guard/storm.rs` | — | 6 |
| **修改** | `llm/client.rs`, `agent/turn.rs`, `session/store.rs`, `agent/orchestrator.rs`, `config.rs`, `llm/provider.rs`, `session/stats.rs`, `sse/parser.rs`, `prompt.rs` | `compact_dp.rs`, `agent/turn.rs` | `tools/runner.rs`, `tools/bash.rs`, `llm/client.rs` | 14 |

### 与所有源文档的追溯矩阵

| # | 来源 | 子项 | 采纳位置 | 状态 |
|---|------|------|---------|------|
| — | cybernetic #4 | 防颤控制 | Phase 2.1 (三级压缩带内置) | ✅ |
| — | cybernetic #5 | 前馈控制 | Phase 1.6 | ✅ |
| — | cybernetic #7 | 噪声滤波 | Phase 3.1 | ✅ |
| — | cybernetic #10 | 故障隔离 | Phase 1.1 (错误分类学) | ✅ |
| — | cybernetic #13 | 在线DP参数 | Phase 2.2 | ✅ |
| — | cybernetic #14 | 监督控制 | Phase 1.5 | ✅ |
| — | cybernetic #16 | 自适应超时 | Phase 3.2 | ✅ |
| — | auto-model-design | 双模型切换 | Phase 1.4 (基础方案) | ✅ |
| — | cross-project #1 | 多信号融合 | Phase 1.4 (合入 auto-model) | ✅ |
| — | cross-project #2 | 自报告升级 | Phase 1.4 (可选特性) | ✅ |
| — | cross-project #3 | 三级压缩 | Phase 2.1 | ✅ |
| — | cross-project #4 | Storm Breaker | Phase 2.5 | ✅ |
| — | cross-project #5 | 预判门 | Phase 2.3 | ✅ |
| — | cross-project #6 | 三部曲内存 | Phase 1.3 | ✅ |
| — | cross-project #7 | 修复流水线 | Phase 2.4 | ✅ |
| — | cross-project #8 | ErrorTaxonomy | Phase 1.1 (合并) | ✅ |
| — | cross-project #9 | 流重试 | Phase 3.3 | ✅ |
| — | cross-project #10 | LlmClient trait | Phase 1.2 | ✅ |
| — | cybernetic #1 | 极限环检测 | ❌ 放弃 | — |
| — | cybernetic #2 | SPRT | ❌ 放弃 | — |
| — | cybernetic #3 | 串级控制 | ❌ 放弃 | — |
| — | cybernetic #6 | 系统辨识 | ❌ 放弃 | — |
| — | cybernetic #8 | 状态观测器 | ❌ 放弃 | — |
| — | cybernetic #9 | 增益调度 | ❌ 放弃 | — |
| — | cybernetic #11 | 消息前过滤 | ❌ 放弃 | — |
| — | cybernetic #12 | 相位检测 | ❌ 放弃 | — |
| — | cybernetic #15 | 单轮LLM重试 | ❌ 放弃 | — |
| — | cross-project #11 | 周期管理器 | ❌ 放弃 | — |
| — | cross-project #12 | Flash 路由 | ❌ 放弃 | — |
| — | cross-project #13 | 连接健康 | Phase 4 (延后) | ⏳ |
| — | cross-project #14 | Panic hook | ❌ 放弃 | — |
| — | cross-project #15 | 会话检查点 | Phase 4 (延后) | ⏳ |
| — | cross-project #16 | RAII 终端守卫 | ❌ 放弃 | — |
| — | cross-project #17 | 架构不变量测试 | ❌ 放弃 | — |
| — | cross-project #18 | Feedforward 信号 | Phase 1.6 (合并) | ✅ |

### 成功指标

| 指标 | 当前 | Phase 1 后 | Phase 2 后 | Phase 3 后 |
|------|------|-----------|-----------|-----------|
| 代码行数 | 5650 | ~6195 | ~6440 | ~6515 |
| 文件数 | 36 | 39 | 42 | 42 |
| 测试数 | 74 | 96 | 110 | 116 |
| Prefix-cache hit rate | ~60% | ~95% | ~98% | ~98% |
| Flash→Pro 切换延迟 | N/A | ≤2 turns | ≤2 turns | ≤2 turns |
| JSON parse 失败修复率 | 0% | 0% | ~70% | ~70% |
| 循环检测 (Storm) | 无 | 无 | 6-窗口 | 6-窗口 |
| 上下文管理粒度 | 单阈值 | 单阈值 | 4 级比例带 | 4 级比例带 |
| 流重试 | 仅 HTTP | 仅 HTTP | 仅 HTTP | 3 层 |

---

## 实施顺序与依赖

```
1.1 错误分类学 ───────┐
                       ├──→ 1.4 Auto-Model (需要 ErrorCategory)
1.2 LlmClient trait ───┤
                       ├──→ 1.3 ImmutablePrefix
1.6 前馈控制 ──────────┘
                       └──→ 1.5 监督控制退化

Phase 1 全部完成后:
  ├──→ 2.1 三级压缩带 ──→ 2.2 在线DP参数
  ├──→ 2.3 预判门
  ├──→ 2.4 修复流水线 ── (独立)
  └──→ 2.5 Storm Breaker ─ (独立)

Phase 2 全部完成后:
  ├──→ 3.1 噪声滤波
  ├──→ 3.2 自适应超时
  └──→ 3.3 流内重试

内部依赖:
  1.1 → 1.4 (error.rs 为 auto-model 提供信号分类)
  1.2 → 1.3, 1.4, 1.5 (LlmClient trait 让后续修改可测试)
  2.1 → 2.2 (在线参数依赖压缩带 tier 框架)
```

### 每个子项可并行

Phase 1 内：1.3 (ImmutablePrefix) / 1.5 (监督) / 1.6 (前馈) 可并行。
Phase 2 内：2.3 / 2.4 / 2.5 可并行。
Phase 3 内：全部可并行。

---

## 关键决策记录

| 决策 | 选择 | 理由 |
|------|------|------|
| ImmutablePrefix 优先级 | Phase 1 | cache hit 最大单次优化，越早做后续 alignment 成本越低 |
| LlmClient trait 优先级 | Phase 1 | 架构债，让 auto-model + turn loop + sub executor 全部可测试 |
| Repair Pipeline 范围 | Scavenger + Flattener (2/4) | Truncation 已有 token 限制，Storm 独立做 2.5 |
| 自报告升级 | 可选特性 (--auto-self-report) | `<<<NEEDS_PRO>>>` 依赖 LLM 配合，不成熟时关闭 |
| 降级策略 | 永不降级 (Hysteresis lock) | Pro 失败的金钱成本远低于 Flash 失败的时间成本 |
| 升级阈值 | 4 (多信号加权) | Parse(2)+Parse(2)=4 两次 parse 错误即升级 |
| 压缩带 Hysteresis | 仅 Aggressive→Conservative 降级需要间隔 | Conservative/ForceSummary/Emergency 已是高水位，不应延迟 |
| Phase 4 延后 | 检查点 + 连接健康 | 依赖 ImmutablePrefix 稳定 + DeepSeek API 已很可靠 |
