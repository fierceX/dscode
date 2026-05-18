# DeepSeek-Reasonix 设计迁移实施计划

**目标**: 将 Reasonix 中已验证的设计模式逐步移植到 dscode（DeepSeek-only），
提升缓存命中率可达性、修复鲁棒性和升级决策准确性。

**现状基线**:
- StormBreaker（存在但未集成）—— `src/guard/storm.rs`
- Scavenge（基础版，缺 DSML + 3-shape + truncation）—— `src/repair/scavenge.rs`
- Flatten（已完成）—— `src/repair/flatten.rs`
- ErrorCategory/ErrorSeverity（已完成）—— `src/errors.rs`
- 压缩后 messages 未刷新（已修复）—— `src/agent/turn.rs`
- 死字段清理（已完成）—— `config.rs`, `compact_dp.rs`, `compaction.rs`
- 当前无 repair pipeline、无 TurnFailureTracker、无 fingerprint 验证

---

## Phase 1 — StormBreaker 集成（预计：+60 行）

### 现状

`src/guard/storm.rs` 有完整的 `StormBreaker` 实现和 4 个测试，
但**没有任何代码 import 或调用它**。StormBreaker 不参与工具执行流程。

### 实施步骤

**Step 1.1: 确定 mutating 判断逻辑**

Reasonix 的 `isMutating` 基于工具定义中的 `readOnly` 标记 + 动态 `readOnlyCheck`。
dscode 当前没有 `readOnly` 概念，需要从工具行为推断：

| 工具 | 是否 mutating | 依据 |
|------|:------------:|------|
| Read | ❌ | 纯查询 |
| Glob | ❌ | 纯查询 |
| Grep | ❌ | 纯查询 |
| WebSearch | ❌ | 纯查询 |
| WebFetch | ❌ | 纯查询 |
| Skill | ❌ | 纯查询 |
| Bash | ✅ | 可写文件系统 |
| Write | ✅ | 写文件 |
| Edit | ✅ | 写文件 |
| SubAgent | ❌ | 独立子会话 |
| TodoWrite | ❌ | 会话状态变更但不触发风暴 |

在 `src/tools/runner.rs` 或新建 `src/tools/mod.rs` 中添加 `is_tool_mutating(name: &str) -> bool`。

**Step 1.2: 在工具执行前插入 StormBreaker.check()**

`src/tools/runner.rs` 中 `execute_all()` 或 `execute_one()` 处插入：

```rust
// 伪代码
match storm.check(&call.name, &call.args_json, is_tool_mutating(&call.name)) {
    StormDecision::Suppress(reason) => {
        // 返回一条特殊的结果，内容包含抑制原因
        // 让 LLM 在下一次调用中看到这个信息
    }
    StormDecision::Allow => {
        // 正常执行
    }
}
```

**Step 1.3: 抑制后的反馈循环**

被抑制的工具结果应注入到下一轮 LLM 上下文（通过 tool_result 消息），内容包含：
```
[Tool call suppressed: Bash repeated 3 times with identical args. 
 Rephrase or try a different approach.]
```

**Step 1.4: StormExempt 支持**

添加 `is_storm_exempt(name: &str) -> bool`，让 `WebSearch`、`WebFetch`、`Skill`、`SubAgent` 等状态无关调用跳过风暴检测。

**Step 1.5: turn 重置**

在 `TurnExecutor.execute()` 的 while 循环开始处重置 StormBreaker 窗口。
每次新的 LLM 调用代表新的意图，旧窗口应清空。

**涉及文件**:
- `src/guard/storm.rs` — 追加 `reset()` 方法（当前没有）
- `src/tools/runner.rs` — 插入 storm.check() 调用
- `src/tools/mod.rs` — 添加 `is_tool_mutating()` + `is_storm_exempt()`
- `src/agent/turn.rs` — 每轮 LLM 调用前 reset storm

**测试验证**:
- 同一个 Repetitive Read 调用连续 3 次 → 第 3 次被 suppressed
- Read(path="/x") → Edit(path="/x") → Read(path="/x") → mutating 清窗口 → Read 正常
- 注入抑制结果后，下一条 LLM 消息中可见抑制原因

---

## Phase 2 — Scavenge + Truncation 增强（预计：+140 行）

### 现状

`src/repair/scavenge.rs` 支持：
- `<tool_call>{...}</tool_call>` XML 格式
- `[TOOL_CALL]{...}[/TOOL_CALL]` 括号格式
- 裸 JSON 对象提取（`{name, arguments}` 形状）

缺少：
- DSML 标记解析（`<|DSML invoke name="Read">...`）
- OpenAI 风格 `{type: "function", function: {name, arguments}}`
- 自由变体 `{tool_name, tool_args}`
- JSON 截断修复（括号不闭合、字符串不闭合、末尾逗号）

### 实施步骤

**Step 2.1: DSML 格式解析（+40 行）**

DeepSeek R1 有时在 `reasoning_content` 中通过 DSML（DeepSeek Markup Language）
标记输出工具调用，而不是通过标准 `tool_calls` 字段。

格式：
```
<|DSML invoke name="Read">
<|DSML parameter name="path" string="true">/tmp/x.txt<|DSML parameter>
<|DSML invoke>

或变体（全角竖线）：
<｜DSML｜invoke name="Grep">
```

在 `src/repair/scavenge.rs` 中添加 `scavenge_dsml()`：

```rust
// 格式: <|DSML invoke name="NAME">
//           <|DSML parameter name="KEY" string="true">VALUE<|DSML parameter>
//           ...
//       <|DSML invoke>
fn scavenge_dsml(text: &str) -> Vec<(String, serde_json::Value)> {
    // 双模式：半角 | 和全角 ｜
    // 提取 name + 所有 parameter → 组装成 JSON 参数对象
    // 支持 string="false" 时 JSON 解析，否则文字保留
}
```

**Step 2.2: 多形状 JSON 回收（+30 行）**

Reasonix 的 `coerceToToolCall` 识别 3 种形状，而当前 `scavenge_tool_calls`
只识别 `{name, arguments}` 一种。

在 `src/repair/scavenge.rs` 中添加 `coerce_to_tool_call()`：

```rust
fn coerce_to_tool_call(json: &str, allowed_names: &[&str]) -> Option<ToolCallInfo> {
    // 形状 1: { "name": "...", "arguments": {...} }
    // 形状 2: { "type": "function", "function": { "name": "...", "arguments": "..." } }
    // 形状 3: { "tool_name": "...", "tool_args": {...} }
}
```

**Step 2.3: JSON 截断修复（+50 行）**

从 Reasonix 的 `repair/truncation.ts` 移植：

```rust
/// 修复截断的 JSON 字符串：补括号、闭引号、填 null、去尾逗号
fn repair_truncated_json(input: &str) -> RepairResult {
    // 1. 快速路径：JSON.parse 直接成功 → 原样返回
    // 2. 修复路径：
    //    a. 去掉末尾逗号
    //    b. 如果停在 key 上: "foo": → "foo": null
    //    c. 补全未闭合的字符串
    //    d. 逆序补全未闭合的 {} [] ""
    // 3. 验证修复后的 JSON，失败则 fallback 到 {}
}
```

**Step 2.4: 组装 repair pipeline**

在 `src/repair/mod.rs` 中新建 `ToolCallRepair`（参考 Reasonix `repair/index.ts`）：

```rust
pub struct ToolCallRepair {
    storm: StormBreaker,
    allowed_names: Vec<String>,
}

impl ToolCallRepair {
    pub fn process(&mut self, text: &str, reasoning: Option<&str>, content: Option<&str>)
        -> RepairOutput
    {
        // 1. Scavenge: 从 reasoning + content 中回收 tool_call
        // 2. Truncation: 修复截断的 arguments JSON
        // 3. Storm: 去重
        // 4. 合并到原有的 declared_calls 中
    }
}
```

**涉及文件**:
- `src/repair/scavenge.rs` — 新增 DSML + coerce + truncation
- `src/repair/mod.rs` — 新增 ToolCallRepair + pipeline
- `src/sse/openai.rs` — 解析失败后调用 Scavenge 兜底

**测试验证**:
- DSML invoke 格式解析正确
- `{"type":"function","function":{"name":"Read","arguments":"{\"path\":\"/x\"}"}}` → 识别
- `{"tool_name":"Bash","tool_args":{"command":"ls"}}` → 识别
- `{"name":"Bash","arguments":"`（截断）→ 修复为完整 JSON
- 截断 key: `{"name":"Bash","arguments":{"command":"ls"}` → 补 `}`

---

## Phase 3 — TurnFailureTracker（预计：+50 行）

### 现状

`src/orchestrator.rs` 的 `update_after_turn` 只检查 `TurnDecision::Failed`，
且只通过 `failure_count` 做连续失败计数（累计不清零直到 Stop）。

`src/errors.rs` 已有 ErrorCategory/ErrorSeverity/is_upgrade_signal/upgrade_weight，
但**不被 orchestrator 使用**。

### 实施步骤

**Step 3.1: 新建 TurnFailureTracker 结构（+30 行）**

参考 `Reasonix/src/loop/turn-failure-tracker.ts`（42行）直接移植：

```rust
pub struct TurnFailureTracker {
    count: u32,
    types: Vec<(String, u32)>,
    threshold: u32,
}

impl TurnFailureTracker {
    pub fn new(threshold: u32) -> Self;
    pub fn reset(&mut self);  // 每轮开始调用
    pub fn note_and_crossed_threshold(&mut self, kind: &str, weight: u32) -> bool;
    pub fn format_breakdown(&self) -> String;
}
```

**Step 3.2: 确定信号来源与权重**

| 信号 | kind | weight | 来源 |
|------|------|--------|------|
| tool 执行失败 | "tool_error" | 1 | ToolRunner 返回 error |
| JSON parse 失败 | "parse_error" | 2 | SSE parser 错误 |
| Storm 抑制命中 | "repeat_loop" | 1 | StormBreaker 返回 Suppress |
| Scavenge 回收 | "scavenged" | 1 | Scavenge 成功回收 tool_call |
| Truncation 修复 | "truncated" | 1 | Truncation 修复成功 |

阈值可配置（默认 3），通过 `AUTO_UPGRADE_THRESHOLD` env var 覆盖。

**Step 3.3: 集成到 Orchestrator（+20 行）**

替换 `orchestrator.rs` 当前的简单 `failure_count` 逻辑：

```rust
// handle_user_input() 每轮开头
self.failure_tracker.reset();

// update_after_turn() 中
if let TurnDecision::Failed(ref msg) = decision {
    let info = errors::classify_anyhow(&anyhow::anyhow!("{}", msg));
    let kind = category_to_signal_kind(info.category);
    if self.failure_tracker.note_and_crossed_threshold(kind, info.weight) {
        // 触发升级
        self.auto_upgrade_score += info.weight;
    }
}
```

同时从工具执行和 SSE parser 处注入信号（通过 context 回调）。

**涉及文件**:
- 新建 `src/agent/failure_tracker.rs` — TurnFailureTracker 实现
- `src/agent/orchestrator.rs` — 集成替换
- `src/tools/runner.rs` — 注入工具执行错误信号
- `src/sse/openai.rs` — 注入 parse error 信号

**测试验证**:
- 连续 3 个 tool error → 触发升级
- Parse error 权重 2 → 2 次即触发
- 每轮 reset → 新的一轮从零开始

---

## Phase 4 — ImmutablePrefix Fingerprint 验证（预计：+15 行）

### 现状

`src/session/prefix.rs` 已有 `ImmutablePrefix` 和 `fingerprint` 计算（SHA-256 based），
但缺乏 `verifyFingerprint()` 调用。已修复的 messages 未刷新 bug 本应被此机制捕获。

### 实施步骤

**Step 4.1: 在 ensure_prefix 返回前验证指纹**

```rust
fn ensure_prefix(&self) -> Result<(String, Vec<serde_json::Value>)> {
    let mut guard = self.ctx.immutable_prefix.lock().unwrap();
    if let Some(ref prefix) = *guard {
        let result = (prefix.system_prompt().to_string(), prefix.tools_json().to_vec());
        // debug 模式/verbose 模式: 验证指纹
        if self.ctx.verbose() {
            prefix.verify_fingerprint();  // 不一致时 panic
        }
        return Ok(result);
    }
    // 重建...
}
```

**Step 4.2: verifyFingerprint 实现（prefix.rs 中已有 but check）**

检查 `prefix.rs` 中是否已有 `verify_fingerprint`：

```rust
pub fn verify_fingerprint(&self) -> String {
    let fresh = self.compute_fingerprint();
    if let Some(cached) = &self.fingerprint_cache {
        if cached != &fresh {
            panic!(
                "ImmutablePrefix fingerprint mismatch! cached={}, fresh={}. \
                 This means the prefix was mutated through a non-invalidation path, \
                 which breaks DeepSeek's prefix-cache alignment.",
                cached, fresh
            );
        }
    }
    self.fingerprint_cache = Some(fresh.clone());
    fresh
}
```

**涉及文件**:
- `src/session/prefix.rs` — 已有 fingerprint，添加 verify 调用
- `src/agent/turn.rs` — ensure_prefix 返回前验证（verbose only）

**测试验证**:
- 模拟指纹漂移 → 触发 panic
- 正常流程 → 不 panic

---

## Phase 5 — 压缩防护与标记（预计：+25 行）

### 现状

`turn.rs` 中每次 LLM 调用前都检查 compact，可在同一轮用户输入中触发多次压缩。
`compaction.rs` 没有"最小收益检查"。

### 实施步骤

**Step 5.1: 同轮多次 compact 防护（+10 行）**

在 `TurnExecutor` 中添加 `compacted_this_turn: bool` 字段：

```rust
// turn.rs: execute() 方法中
if did_compact {
    self.invalidate_prefix();
    messages = self.ctx.store.lines().await?;
    (system_prompt, tools_json) = self.ensure_prefix()?;
    self.compacted_this_turn = true;  // ← 标记
}

// evaluate_and_compact 调用处：
if self.compacted_this_turn {
    // 跳过，同轮已压缩过
    continue;
}
```

**Step 5.2: 最小收益检查（+10 行）**

在 `compaction.rs` 的 `evaluate_and_compact()` 中：

```rust
// 估算压缩能省多少 token
let remaining_tokens = // 压缩后 context token 估算
let total_tokens = // 当前 context token 估算
let savings_fraction = (total_tokens - remaining_tokens) as f64 / total_tokens as f64;
const MIN_SAVINGS_FRACTION: f64 = 0.10;  // 至少省 10%
if savings_fraction < MIN_SAVINGS_FRACTION {
    return Ok((false, "savings too small".into()));
}
```

**Step 5.3: fold marker 提示文本（+5 行）**

在 summary 前加上标记文本：

```rust
// compaction.rs: run_summary_call 中
let summary_instruction = 
    "[CONVERSATION HISTORY SUMMARY — earlier turns compacted for context efficiency]\n\n"
    + "The conversation context above needs to be compacted...";
```

**涉及文件**:
- `src/agent/turn.rs` — compacted_this_turn 防护
- `src/session/compaction.rs` — 最小收益检查 + fold marker

---

## Phase 6 — 完整 repair pipeline 集成（预计：+50 行 ORM）

将 Phase 1-2 的组件整合为一条完整的修复流水线。

**pipeline 执行顺序**（参考 Reasonix `repair/index.ts`）：

```
SSE 解析完成 → declared_calls 提取
  ↓
1. Scavenge（从 reasoning + content 回收额外调用）
  ↓ 合并到 declared_calls
2. Truncation repair（修复每个 call.arguments 的截断 JSON）
  ↓
3. Storm breaker（去重，抑制重复循环）
  ↓
4. 执行最终 calls
  ↓
5. 收集 repair report
  ↓
6. report → TurnFailureTracker 注入信号
```

**集成入口**: `src/tools/runner.rs` 的 `execute_all()` 或
`src/agent/turn.rs` 中收到 tool_calls 后、执行前。

**涉及文件**:
- `src/repair/mod.rs` — pipeline orchestrator
- `src/tools/runner.rs` — 调用入口
- `src/agent/turn.rs` — 传递 reasoning content 给 pipeline

---

## 文件变更汇总

| 文件 | Phase | 新增/修改 | 行数 |
|------|-------|----------|:----:|
| `src/guard/storm.rs` | 1 | 追加 reset() | +5 |
| `src/tools/mod.rs` | 1 | is_tool_mutating + is_storm_exempt | +15 |
| `src/tools/runner.rs` | 1,6 | storm.check + pipeline 入口 | +25 |
| `src/agent/turn.rs` | 1,5 | storm reset + compacted_this_turn | +20 |
| `src/repair/scavenge.rs` | 2 | DSML + coerce + truncation | +120 |
| `src/repair/mod.rs` | 2,6 | ToolCallRepair + pipeline | +40 |
| `src/sse/openai.rs` | 2 | parse error → scavenge | +10 |
| `src/agent/failure_tracker.rs` | 3 | 新建 TurnFailureTracker | +40 |
| `src/agent/orchestrator.rs` | 3 | 集成替换 | +15 |
| `src/session/prefix.rs` | 4 | verify_fingerprint 调用 | +10 |
| `src/session/compaction.rs` | 5 | 最小收益检查 + fold marker | +15 |
| **合计** | | | **+315** |

---

## 实施建议顺序

```
Phase 1 (StormBreaker 集成)     → 最低风险，直接提升鲁棒性
Phase 5 (压缩防护)               → 2 行改动，防同轮多次压缩
Phase 3 (TurnFailureTracker)    → 解耦 orchestrator，信号更精细
Phase 2 (Scavenge 增强)         → 最大改动量，需要充分测试
Phase 4 (Fingerprint 验证)      → 安全网，可在任意阶段加入
Phase 6 (Pipeline 集成)          → 收尾，将前 4 个 phase 串联
```

每个 Phase 独立可部署、独立可测试、独立可回滚。
Phase 6 是可选的"锦上添花"，前 5 个 Phase 各自独立提供价值。
