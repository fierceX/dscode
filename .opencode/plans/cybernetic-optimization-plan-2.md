# dscode 工程控制论优化 — 补充篇

本文档是前 8 项优化（极限环检测 / 最优停止 / 串级控制 / 防颤控制 / 前馈控制 / 系统辨识 / 噪声滤波 / 状态观测器）的补充。
聚焦于系统级的**自适应与鲁棒性**（adaptation & robustness）。

---

## 总览

前 8 项覆盖的是 agent loop 内部的**过程控制层**——loop 内部的稳定性、停止策略、信号过滤。
本 8 项覆盖的是**自适应层 + 架构容错层**——参数自我调优、故障分级响应、在线模型更新。

```
                    process control layer (前8项)
                   ┌──────────────────────────────┐
    user input ──→ │  turn.rs (loop body)         │
                   │  #1极限环  #2SPRT  #8观测器  │
                   │  #3串级   (#5前馈 built into  │
                   │            prompt.rs)        │
                   │  (#7噪声 built into          │
                   │            runner.rs)        │
                   │  (#4防颤 built into           │
                   │            compaction.rs)    │
                   └──────────────┬───────────────┘
                                  │
    adaptation + robustness layer (本8项补充)
    ┌─────────────────────────────┼─────────────────────────────┐
    │                             │                             │
    ▼                             ▼                             ▼
┌──────────┐              ┌──────────────┐            ┌────────────────┐
│ turn.rs  │              │ orchestrator │            │ compaction.rs  │
│ #9 增益  │              │   .rs        │            │ +compact_dp.rs │
│   调度   │              │ #14 监督控制 │            │ #13 在线DP     │
│ #15 ARQ │              │              │            │   参数估计     │
└────┬─────┘              └──────┬───────┘            └───────┬────────┘
     │                          │                            │
     │    ┌─────────────────────┤                            │
     │    │                     │                            │
     ▼    ▼                     ▼                            ▼
┌──────────────┐     ┌──────────────────┐         ┌──────────────────┐
│ tools/       │     │ session/stats.rs │         │ session/         │
│ runner.rs    │     │                  │         │ relevance.rs     │
│ +bash.rs     │     │ #12 任务相位检测 │         │                  │
│              │     │                  │         │ #11 消息前过滤   │
│ #10 错误分级 │     │ (为#9/#13/#14    │         │                  │
│ #16 自适应   │     │  提供相位信号)   │         │                  │
│   超时       │     └──────────────────┘         └──────────────────┘
└──────────────┘
```

---

## 优化 9：自适应增益调度 — thinking_budget 动态调整

### 文件

`agent/turn.rs:84`（stream 调用传参）、`provider.rs:21-25`（thinking 写入 request body）、`config.rs:75`（默认 2048）

### 现状

`thinking_budget` 固定 2048，无论任务复杂度、是否出错、是否在规划阶段，全程不变。

### 控制论原理

**Adaptive Gain Scheduling** — 控制器根据运行状态（平稳 vs 扰动 vs 饱和）调整增益参数。在工业中，PID 控制器的 Kp/Ki/Kd 在不同运行点不同；在这里，thinking budget 就是 agent 的"认知增益"。

### 实现

TurnExecutor 新增方法 `compute_adaptive_thinking_budget`，在每次 LLM 调用前根据信号调整：

| 信号 | 调整量 | 解释 |
|------|--------|------|
| 上轮有 tool 失败（error 分类为 transient） | +1024 | 需要更多思考来纠正 |
| 连续 3 轮无工具调用 | -512 | 任务接近完成，减少冗余思考 |
| 上轮有 PlanConfirm | +1024 | 刚确认计划，需要仔细规划执行 |
| 上轮发生 compaction | +512 | 上下文变化大，需要重新理解 |
| current_turn / max_turns > 0.8 | +512 | 接近轮次上限，思考应更谨慎 |
| #12 检测到 Explore 阶段 | +1024 | 建立认知阶段需要更多思考 |
| #12 检测到 Review 阶段 | -1024 | 验证阶段不需要深度推理 |

边界约束：floor = 512, ceiling = 8192。

### 预期收益

复杂任务因更多思考而减少来回试探，简单任务因更少思考而加快速度。

### 新增代码

~30 行（`turn.rs`）。

---

## 优化 10：故障检测与隔离 — 工具错误分级

### 文件

`tools/runner.rs:131-134`（错误处理）

### 现状

所有工具错误折叠成 `"Error: tool execution failed: {e}"` 一条消息。LLM 无法区分"文件不存在"（永久错误，不重试）和"网络超时"（临时错误，可重试），只能盲猜是否重试。

### 控制论原理

**Fault Detection and Isolation (FDI)** — 工业控制系统区分传感器故障、执行器故障、过程扰动，每类触发不同响应。FDI 系统有三个层次：检测（有故障）、隔离（哪个组件）、识别（什么类型）。这里只需轻量的检测+分类。

### 实现

在 `ToolRunResult` 中新增枚举：

```rust
enum ToolError {
    TransientTimeout,     // 超时 → 重试，加大 timeout
    TransientNetwork,     // 网络 → 重试，等一会
    NotFound,             // 文件/路径不存在 → 不重试，请求用户确认
    PermissionDenied,     // 权限 → 不重试，建议替代路径
    InvalidArgs,          // 参数错误 → 不重试，建议修正
    SafetyBlocked,        // 安全策略拦截 → 不重试，解释原因
    Unknown(String),      // 未知 → 不重试，报告详情
}
```

每个工具实现内部分类：

| 工具 | 错误来源 | 分类 |
|------|---------|------|
| Bash | 超时（timeout kill） | TransientTimeout |
| Bash | 安全策略拦截 | SafetyBlocked |
| Bash | 进程启动失败 | Unknown |
| Read | 文件不存在 | NotFound |
| Read | 权限拒绝 | PermissionDenied |
| Read | offset 超限 | InvalidArgs |
| Write | 权限拒绝 | PermissionDenied |
| Write | 内容超限 | InvalidArgs |
| Edit | 文件不存在 | NotFound |
| Edit | old_string 找不到 | InvalidArgs |
| Edit | 文件太大 | InvalidArgs |
| Glob/Grep | rg 未安装 | Unknown |
| WebSearch/Fetch | 网络错误/超时 | TransientNetwork |
| Skill | 技能未找到 | NotFound |

`runner.rs` 统一返回格式化后的错误消息：`[TRANSIENT_TIMEOUT] command timed out after 10s — retry with longer timeout`。

同时在 system prompt 的 `using-your-tools` section 追加故障分类的含义和推荐 LLM 响应。

### 预期收益

减少 LLM 对错误的误判，避免因"文件不存在"反复调用 Read 或用不同参数盲目重试。

### 新增代码

~40 行（`runner.rs` + 各 `tools/*.rs` 错误返回处修改）。

---

## 优化 11：消息前过滤 — 信息密度评分

### 文件

`agent/turn.rs:66`（`messages = self.ctx.store.lines()`）、`agent/turn.rs:84`（`messages.clone()` 传入 stream）

### 现状

每次 LLM 调用前，messages 是 conversation store 的完整快照。历史中的错误信息、冗余工具结果、无进展的对话轮次全部消耗上下文 token。

### 控制论原理

**Pre-Filtering / Sensor Selection** — Kalman 滤波器只融合信息量最大的测量值，忽略噪声。agent 应在发往 LLM 前对消息做轻量评分，低价值消息可截断或降权。这是 independent innovation 而非 Kalman 滤波的直接套用——核心思想是 information density gating。

### 实现

新增 `session/relevance.rs`，对每条 conversation line 打分：

| 信号 | 权重 | 解释 |
|------|------|------|
| Recency（指数衰减，越近越高） | 0.35 | 近因效应 |
| Content length（normalized，适中最好） | 0.15 | 太短=废话，太长=噪声 |
| Is user message（role == user 且 content 是 string） | 0.15 | 用户消息是任务锚点 |
| Is tool_result with non-error content | 0.20 | 有实际产出的工具结果 |
| Is tool_result with only error | -0.25 | 纯错误消息拖累上下文 |
| Is text-only assistant response | 0.10 | 最终回答 |

每条消息得到一个 [0, 1] 的分数。`threshold` 随 context pressure 自适应：
```
threshold = 0.2 + 0.3 * (current_context_tokens / max_context_tokens)
```
context 越满，阈值越高，过滤越激进。

低于阈值的消息不删除，而是在其位置插入一条轻量 inline summary：
```
[summary of earlier turn: user asked to grep for "main", found in 3 files]
```
原消息不进入当前 LLM 调用的 messages 数组，但仍保留在 store 中供后续 compact 处理。

### 预期收益

上下文利用效率提升 15-25%，等价于扩容而不增加 API 成本。

### 新增代码

~65 行（新文件 `session/relevance.rs`，`turn.rs` 集成 5 行）。

---

## 优化 12：会话阶段检测 — 任务相位估计

### 文件

`session/stats.rs:9-21`（Stats 结构体）

### 现状

Stats 跟踪 turn count、token count，但没有"任务在哪个阶段"的概念。这使得 phase-aware 优化无法实现。

### 控制论原理

**Mode Detection / Hybrid Estimation** — 混合系统在连续动态之间切换模式，需要 mode observer 检测当前模式。agent 会话有明显的 phase 转换：探索→执行→审查→完成。

### 实现

在 Stats 中新增 `TaskPhase` 枚举：

```rust
enum TaskPhase {
    Explore,     // 大量 Glob/Grep/Read，建立认知
    Execute,     // 大量 Bash/Edit/Write，执行更改
    Review,      // 以 Read/验证为主，确认结果
    Complete,    // 无工具调用多轮，任务完成
}
```

每轮结束后，在 `turn.rs` Phase 4 根据以下信号更新 phase：

| 信号 | Explore | Execute | Review | Complete |
|------|---------|---------|--------|----------|
| Tool 密度（tools/turn） | ≥2 | 1-2 | 0-1 | 0 |
| Glob/Grep/Read 占比 | >50% | <30% | <20% | 0 |
| Bash/Edit/Write 占比 | <20% | >50% | <30% | 0 |
| 连续无工具调用 | <2 | <2 | ≥2 | ≥4 |
| turn / baseline_e | <0.3 | 0.3-0.7 | 0.7-1.0 | >1.0 |

Phase 信息存入 Stats（`stats.set_task_phase`），供以下优化读取：
- #9：Explore +1024 thinking；Review -1024
- #14：Review 阶段连续失败直接建议用户介入（更保守）
- compaction：Explore 阶段 bete 偏高（保护初建认知），Execute 阶段 beta 偏低（积极压缩）

### 预期收益

本身不直接产生收益，但作为基础信号让 3 个其他优化成为可能。

### 新增代码

~45 行（`stats.rs` 新增枚举和方法，`turn.rs` Phase 4 调用）。

---

## 优化 13：在线 DP 参数估计

### 文件

`session/compaction.rs:176-177`（compact_usage 捕获）、`compact_dp.rs:6-18`（DPCompactConfig）、`compact_dp.rs:104-105`（token 估计）

### 现状

compact 调用后，实际 token 使用量已捕获（`compact_usage`），但从未用于更新 DP 公式的参数：

| 参数 | 默认值 | 含义 | 是否应自适应 |
|------|--------|------|-------------|
| `v` | 5000 | summary 输出 token 估计量 | ✅ 实际值从 usage 可知 |
| `s` | 500 | summary 开销 token 估计量 | ✅ 同上 |
| `p_input` | 3.0 | input 每百万 token 价格 | ⚠️ 仅 provider 变更时需更新 |
| `p_cache` | 0.30 | cache hit 每百万 token 价格 | ⚠️ 同上 |
| `p_out` | 15.0 | output 每百万 token 价格 | ⚠️ 同上 |
| `r` | 0.8 | 重复压缩的收益衰减 | ❌ 固定经验值 |
| `beta` | 0.03 | 信息损失惩罚系数 | ✅ 可根据下轮 tool 失败率调整 |

### 控制论原理

**Recursive Least Squares / Online Parameter Estimation** — 自适应控制器用每次新的 I/O 数据更新内部模型。DP 公式包含 8 个参数，其中 `v`、`s`、`beta` 可从实测数据在线更新。

### 实现

compact 调用完成后，在 `compaction.rs:176-177`（compact_usage 记录后）追加：

```rust
// 用实测数据更新 DP 参数
if let Some(ref usage) = compact_usage {
    // v 跟踪 summary 的实际输出 token 数
    let new_v = (0.7 * cfg.dp_v as f64 + 0.3 * usage.output_tokens as f64) as usize;
    cfg.dp_v = new_v.max(100);

    // s 跟踪 summary 的输入开销（去掉 prefix 后的实际增量）
    let overhead = usage.input_tokens.saturating_sub(cfg.dp_v as i64);
    let new_s = (0.7 * cfg.dp_s as f64 + 0.3 * overhead as f64) as usize;
    cfg.dp_s = new_s.max(100);
}
```

**beta 自适应**（在 `turn.rs` Phase 4，compact 调用后的下一轮）：
```rust
// 如果 compact 后的下一轮 tool 有失败，说明压缩可能丢了关键信息
let failure_occurred = processed_results.iter().any(|r| r.tool_name == "Error: tool execution failed");
if failure_occurred {
    self.ctx.config.dp_beta *= 1.2;  // 更加保守
} else {
    self.ctx.config.dp_beta *= 0.95; // 逐渐放松
    self.ctx.config.dp_beta = self.ctx.config.dp_beta.max(0.01);
}
```

**token 估计校准**（`compact_dp.rs:104-105`）：
```rust
// 当前：sizes.push((s.len() + 3) / 4);  // 硬编码 bytes→tokens 比例

// 改为使用校准因子：
let estimated_tokens = (s.len() as f64 / self.bytes_per_token_ratio) as usize;
sizes.push(estimated_tokens);

// bytes_per_token_ratio 从 usage 事件中更新：
// ratio = total_message_bytes / total_input_tokens (from LLM usage)
```

### 预期收益

DP 决策精度随时间自适应提升，不再依赖手工估计的成本数字。

### 新增代码

~45 行（`compaction.rs` ~25 行 + `compact_dp.rs` ~10 行 + `turn.rs` ~10 行）。

---

## 优化 14：监督控制 — 连续失败参数降级

### 文件

`agent/orchestrator.rs:75-117`（`handle_user_input`）

### 现状

`TurnDecision::Failed` 发生时，错误被展示给用户，orchestrator 不做任何调整，下轮以相同参数重试。

### 控制论原理

**Supervisory Control** — 高层控制器在子系统异常时调整设定点（setpoint）。工业中，当某段工艺出现连续次品时，监督层降低产速或调整温度。在这里，连续 turn 失败应触发参数降级。

### 实现

OrchActor 新增字段：

```rust
struct OrchActor {
    // ... 现有字段 ...
    consecutive_failures: usize,
    last_failure_type: Option<FailureType>,
}

enum FailureType {
    HttpError(String),
    MaxTokens,
    ToolFailure(String),
    StreamError(String),
}
```

每轮结束后的降级策略：

| 连续失败次数 | 动作 |
|-------------|------|
| 1 | 记录错误类型，不干预 |
| 2 | 将 `cfg.max_tokens` 降至当前值 × 0.7；往 conversation 追加 guidance：`"[SYSTEM] The previous two attempts failed. Please try a simpler approach. Focus on the minimal viable step."` |
| 3 | 将 `cfg.thinking_budget` 翻倍（可能需要更多思考来诊断问题）；提示用户考虑拆分任务 |
| 4+ | 放弃，`render_error("Multiple consecutive failures. Manual intervention required.")`；不再次尝试，返回给用户 |

成功后 `consecutive_failures = 0`。

按失败类型细化：
- HttpError + StreamError → 偏向重试（网络问题）
- ToolFailure → 偏向调整参数或更换工具
- MaxTokens → 偏向提示用户减少输入

### 预期收益

避免 agent 反复用相同参数尝试必然失败的操作。

### 新增代码

~25 行（`orchestrator.rs`）。

---

## 优化 15：消息级重试 — 单轮 LLM 调用 ARQ

### 文件

`agent/turn.rs:84-88`（`self.llm.stream()` 调用）

### 现状

`self.llm.stream()` 失败直接 return `TurnDecision::Failed`。`llm/client.rs` 的 `send_with_retry` 在 HTTP 层已做重试，但如果重试后仍失败（如 502 持续或短暂的连接断开），turn 直接死亡。

### 控制论原理

**Automatic Repeat reQuest (ARQ)** — 通信系统在传输失败时重发同样的数据包。一次 HTTP 失败不等于 turn 失败——同一组 messages 可以重试。

### 实现

在 `turn.rs:84` 调用外加轻量重试：

```rust
let mut stream = None;
for attempt in 0..=2 {
    match self.llm.stream(&self.ctx, messages.clone(), &tools_json, &system_prompt).await {
        Ok(s) => { stream = Some(s); break; }
        Err(e) if attempt < 2 && is_transient_error(&e) => {
            self.ctx.display.render_info(&format!("Stream attempt {} failed, retrying...", attempt + 1));
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        }
        Err(e) => return Ok((TurnDecision::Failed(e.to_string()), effects)),
    }
}
let mut stream = stream.unwrap();
```

仅重试 transient 错误（5xx, timeout, connection reset），不重试 4xx 错误。

### 预期收益

消除因偶发网络波动导致的单轮失败。

### 新增代码

~15 行（`turn.rs`）。

---

## 优化 16：自适应工具超时 — 基于观察的动态 deadline

### 文件

`tools/bash.rs:7-18`（execute 函数）、`tools/runner.rs:86`（dispatch 传 default_timeout）

### 现状

`tool_timeout_secs` 固定 600 秒。快命令（如 `echo hello`）不需要等这么久才判超时；慢命令（如 `cargo build --release`）可能在 600 秒不够。

### 控制论原理

**Deadline Propagation / Adaptive Timeout** — 实时系统根据历史执行时间动态调整 deadline。如果上 5 个 Bash 命令平均耗时 2 秒，下一个命令的 timeout 可以设为 6 秒而不是 600 秒。

### 实现

在 Stats 中跟踪最近 N 次 Bash 成功执行的耗时：

```rust
// session/stats.rs
pub struct BashTimingStats {
    recent_durations_ms: VecDeque<u64>,  // 最近 10 次的耗时
    median_ms: u64,                       // 滑动中位数
}
```

`bash.rs` 的 `execute` 函数：
```rust
// 用中位数计算自适应超时
let adaptive_timeout = if median_ms > 0 {
    let secs = (median_ms * 3 / 1000) as u64; // 3倍中位数
    secs.clamp(10, default_timeout)            // 边界 [10s, 600s]
} else {
    default_timeout
};
```

如果命令实际超时，下次中位数上浮 50%：
```rust
if timed_out {
    timings.median_ms = timings.median_ms * 3 / 2;
}
```

### 预期收益

快的命令不会被多余的超时窗口拖累（提前感知异常），慢的命令不会因固定上限被误杀。

### 新增代码

~30 行（`bash.rs` ~20 行 + `stats.rs` ~10 行）。

---

## 总结表

| # | 优化 | 控制论原理 | 主要文件 | 新增代码 |
|---|------|-----------|---------|---------|
| 9 | thinking_budget 自适应 | Gain Scheduling | `turn.rs` | ~30行 |
| 10 | 工具错误分级 | Fault Isolation | `runner.rs`, `tools/` | ~40行 |
| 11 | 消息前过滤 | Pre-Filtering | 新 `relevance.rs`, `turn.rs` | ~65行 |
| 12 | 任务相位检测 | Mode Detection | `stats.rs` | ~45行 |
| 13 | 在线 DP 参数估计 | Online Estimation | `compaction.rs`, `compact_dp.rs` | ~45行 |
| 14 | 连续失败降级 | Supervisory Control | `orchestrator.rs` | ~25行 |
| 15 | 单轮 LLM 重试 | ARQ | `turn.rs` | ~15行 |
| 16 | 自适应工具超时 | Deadline Propagation | `bash.rs`, `stats.rs` | ~30行 |

**总计**：~295 行新代码，分布在 8 个文件。

---

## 最大杠杆点

- **#12（相位检测）** — 为 #9、#14、compaction 自适应提供基础信号
- **#10（错误分级）** — 让 LLM 能做出明智的重试/放弃/替代决策

---

## 与前 8 项的关系

| 维度 | 前 8 项 | 本 8 项 |
|------|---------|---------|
| 焦点 | loop 内部的过程控制 | 系统级的自适应与鲁棒 |
| 控制论类别 | 相位补偿、SPRT、串级、迟滞、前馈、辨识、滤波、观测 | 增益调度、故障隔离、前过滤、相位检测、在线估计、监督控制、ARQ、截止期传播 |
| 作用域 | 单轮 loop 迭代内 | 跨轮次、跨会话的自我调优 |
| 依赖关系 | 独立 | #12 是 #9/#14 的前提，#10 是 #15 的前提 |

---

## 分批实施建议

| 批次 | 内容 | 理由 |
|------|------|------|
| **基础信号层** | #4 防颤 + #5 前馈 + #7 噪声 + #12 相位检测 | 先建好信号基础设施 |
| **过程控制层** | #1 极限环 + #2 SPRT + #8 观测器 + #9 增益调度 | loop 内优化 |
| **自适应层** | #10 错误分级 + #11 前过滤 + #13 在线估计 + #16 自适应超时 | 自我调优 |
| **架构容错层** | #3 串级 + #6 辨识 + #14 监督控制 + #15 ARQ | 架构级增强 |

每批独立实施，互不阻塞。推荐从基础信号层开始。
