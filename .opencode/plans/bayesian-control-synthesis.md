# 融合控制论、贝叶斯与因果推断的优化策略分析

> 对照：当前 dscode 代码库 + `.opencode/plans/feedback-control-system.md` 设计方案
> 参考：`融合控制论、贝叶斯与因果推断的轻量编码 Agent 设计.md`

---

## 一、三份设计的对照总览

| 维度 | 当前代码 (dscode) | 反馈控制系统 (feedback-control.md) | 贝叶斯融合设计 (新文档) |
|------|------------------|----------------------------------|----------------------|
| 决策基础 | 硬阈值计数 | 加权信号融合 | 概率信念 + 后验概率 |
| 模型选择 | 固定 flash→pro 单向升级 | 多信号→分数→升级 | Thompson Sampling (动态选择) |
| 停滞检测 | 连续失败计数 | consecutive_tool_errors | P(stall) 贝叶斯变点检测 |
| 重复检测 | StormBreaker 滑窗 | StormBreaker（复用） | 错误签名哈希 + 频次计数 |
| Token 效率 | 无 | 无 | ΔE/ΔT P 控制器 |
| 因果推断 | 无 | 无 | 反事实提示 + 干预分离 |
| 传感器 | Rust 层 hardcode | shell 脚本 error.sh | `agent_verify` 统一标量 |
| 实现阶段 | 已实现 | 设计完成未实现 | 理论阶段 |

---

## 二、贝叶斯文档的独特贡献

### 2.1 Thompson Sampling 用于模型选择（最有价值）

**文档提出**：

```python
# 每个模型维护 Beta(α, β)，代表"本步能减少误差"的信念
# 选择时从每个分布采样，选取采样值最高的模型
samples = {}
for name, b in beliefs.items():
    samples[name] = beta.rvs(b["alpha"], b["beta"])
selected = max(samples, key=samples.get)
```

**当前代码**（`orchestrator.rs:243-259`）：

```rust
fn resolve_active(&self) -> (String, String) {
    if let Some(ref forced) = self.forced_model { return forced; }
    if self.model_locked || self.auto_upgrade_score >= self.upgrade_threshold {
        return secondary_model;  // 永不降级
    }
    return cfg.model;  // 始终 flash
}
```

**差异**：

| 维度 | 当前 | Thompson Sampling |
|------|------|-------------------|
| 切换方向 | 单向（flash→pro，永不回来） | 双向（可降级回 flash） |
| 切换条件 | 分数 >= 4（硬阈值） | 采样概率（连续值） |
| 不确定性 | 无视 | 通过 Beta 分布量化 |
| 探索行为 | 无 | 自动平衡探索与利用 |
| 降级能力 | 无（除非手动 /flash） | 若 flash 表现恢复可自动回切 |

**适配性分析**：对 dscode 来说，Ben Thompson Sampling 的价值在于**降级能力**——当前模型升级后永不降级，即使后续任务很简单也继续用 pro，浪费 token。Thompson Sampling 允许在简单任务上自动回退到 flash。

**问题**：dscode 的模型切换是 per-user-input（每次 `handle_user_input` 重建 LLM client），不是 per-step。Thompson Sampling 的"每步切换"需要 per-step 模型选择，在 dscode 架构中意味着每个 tool_use 循环迭代都可能切换模型。这会导致频繁的 LLM client 重建（重连 HTTP、重新身份验证）。**不直接适用**。

### 2.2 贝叶斯停滞概率（价值中）

**文档提出**：

```python
prob = 1.0 - 0.5 ** self.no_progress_count
```

这是一个几何衰减的连续概率，而不是硬阈值。当连续无进展次数为：
- 0 → P=0
- 1 → P=0.5
- 2 → P=0.75
- 3 → P=0.875
- 4 → P=0.9375
- 5 → P=0.96875

**当前代码**（`failure_tracker.rs` + `orchestrator.rs`）：

```rust
if self.failure_tracker.note_and_crossed_threshold(kind) {
    // 首次跨过阈值时触发
}
if self.auto_upgrade_score >= self.upgrade_threshold {
    model_locked = true;
}
```

当前是计数 + 硬阈值。贝叶斯版本的 P(stall) 提供了更平滑的控制——可以在 P > 0.8 时注入反思提示，P > 0.95 时切换到 pro 模型，P > 0.99 时中止。比"连续 3 次→反射，5 次→中止"更细粒度。

**适配性**：可以直接替换当前的 `failure_count` 逻辑。代码量 ~10 行，零依赖（不需要 Beta 函数，只需要指数衰减计算）。

### 2.3 Token 效能比 P 控制器（价值中高）

**文档提出**：

```
instant_efficiency = ΔE / ΔT
if instant_efficiency < sliding_avg * threshold:
    inject_constraint("请用更简洁的方案")
```

**当前代码**：完全没有 token 效率的概念。标题栏显示 token 用量但不用于决策。

**适配性**：可以集成到 `TurnFailureTracker` 中或作为独立的 `EfficiencyTracker`。当效率持续偏低时，在 system prompt 中附加简洁性约束（通过 `invalidate_prefix()` → 重建 prompt）。

### 2.4 因果推断：干预与观测分离（价值中低）

**文档提出**：在系统提示词中将历史信息分为"观测记录"和"当前干预任务"两块。

**当前代码**：system prompt 没有做这种结构分离。上下文压缩后的 `context-snapshot` 段将历史作为背景信息，但并不明确指出"这是观测，你现在要做出干预"。

**适配性**：纯 prompt 级别变更。在 `plan-lifecycle-guidance` 或 `rules` 段中添加一句话："在修改前，先说明你期望的因果效应（这个改动会改变什么）。" +0 行 Rust 代码。

---

## 三、与 feedback-control-system.md 的融合

### 3.1 认知层 → 控制层的接口

两份设计都有"认知→控制"的接口。当前 feedback-control.md 的设计：

```
传感器信号 → 控制器状态 → 信号融合 → 控制动作
```

贝叶斯设计：

```
贝叶斯信念 (概率) → 控制层 (硬决策) → 执行器
```

**融合点**：将 `ControllerState` 中的数字计数器替换为贝叶斯概率。例如：

```rust
// 当前 (feedback-control.md):
struct ControllerState {
    consecutive_tool_errors: u32,      // 硬计数
}

// 融合后:
struct ControllerState {
    tool_error_belief: f64,            // P(stall) 概率
    model_beliefs: [(String, f64, f64)],   // (model_name, alpha, beta)
    stall_probability: f64,            // 变点后验概率
}
```

### 3.2 传感器层不变

贝叶斯文档的 `agent_verify` 单标量方案与 feedback-control.md 的多信号传感器方案不冲突。传感器层仍然可以输出多个信号（tool_error, perf_lag, output_large 等），控制器层根据信号类型更新不同的贝叶斯模型。

**建议**：保留多信号传感器（更丰富的信息），但在控制器层使用贝叶斯方法处理信号（更平滑的决策）。

### 3.3 Thompson Sampling vs 当前升级系统

当前升级系统有一个有用特性：**永不降级**（`model_locked`）。这个特性在简单任务上浪费了 pro 模型的 token 成本，但保证了稳定性——一旦切换到 pro 就不会因为单次幸运成功而回退到 flash。

Thompson Sampling 自动解决了这个问题：如果 flash 的表现恢复（连续成功），`α` 增长，采样值上升，自然回退。不需要显式的"永不降级"或"永不升级"。

**建议**：在 `feedback-controller.md` 的 Phase 2 中（信号融合控制器），使用 Thompson Sampling 替代当前的分数累加逻辑。

---

## 四、实施优先级

### Phase 1: 低挂果实（~30 行，零外部依赖）

| # | 改动 | 行数 | 来源 | 说明 |
|---|------|:----:|------|------|
| 1 | 停滞概率替代硬计数 | +10 | 贝叶斯 3.3.2 | `P(stall) = 1 - 0.5^k` 替换 `failure_count ≥ 3` |
| 2 | 因果干预提示 | +5 | 贝叶斯 3.4 | 在 `plan-lifecycle-guidance` 中添加"先说明期望因果效应" |
| 3 | Token 效率追踪 | +15 | 贝叶斯 3.2.3 | `ΔE/ΔT` 滑动窗，低于阈值时注入简洁性约束 |

### Phase 2: 贝叶斯模型选择器（~80 行）

| # | 改动 | 行数 | 来源 | 说明 |
|---|------|:----:|------|------|
| 4 | Beta-Bernoulli 信念结构体 | +20 | 贝叶斯 3.3.1 | `struct BetaBelief { alpha: f64, beta: f64 }` |
| 5 | Thompson Sampling 选择 | +15 | 贝叶斯 3.3.1 | 每个 `handle_user_input` 时采样选择模型 |
| 6 | 信念更新 | +15 | 贝叶斯 3.3.1 | 每轮结束后根据结果更新 α/β |
| 7 | 允许模型回退 | +10 | — | 移除 `model_locked` 的"永不降级"语义，让 Thompson Sampling 自动管理 |
| 8 | Beta 采样实现 | +20 | — | 使用 Gamma 近似或预计算表，不引入外部统计库 |

### Phase 3: 传感器层（复用的 feedback-control.md 设计）

| # | 改动 | 来源 |
|---|------|------|
| 9 | error.sh 传感器脚本 | feedback-control.md |
| 10 | perf.sh 传感器脚本 | feedback-control.md |
| 11 | 传感器注册表 | feedback-control.md |

---

## 五、不可采纳的部分

| 文档方案 | 不采纳原因 |
|---------|-----------|
| `agent_verify` 单标量 | dscode 已有多信号传感器设计，单标量丢失信号粒度（无法区分"编译错误"和"测试失败"） |
| 每步模型切换 | dscode 的 LLM client 是 per-user-input 创建，每步切换会导致大量 HTTP 连接重建 |
| Beta 分布的严格实现 | `scipy.stats.beta.rvs` 在 Rust 中需要数值库依赖。可用 Greedy 或 Epsilon-Greedy 近似 |
| 循环振荡检测的错误签名哈希 | StormBreaker 的 (name, args) 滑窗已覆盖这个场景，且不需要解析错误文本 |

---

## 六、核心结论

贝叶斯文档的三个有价值贡献按价值排序：

```
1. Thompson Sampling 模型选择 ← 最具变革性
   - 让 flash/pro 切换变成双向概率决策
   - 自动平衡成本与效果
   - 移除"永不降级"的硬约束

2. 停滞概率 P(stall)          ← 最易落地
   - 5 行代码替换现有的硬计数
   - 从"3 次→反射,5 次→中止"变为"P>0.8→反射,P>0.99→中止"
   - 控制更平滑，无额外运行时开销

3. 因果推断提示模式           ← 零成本
   - 修改 prompt.rs 的 plan-lifecycle-guidance 段
   - 不涉及任何 Rust 代码或运行时逻辑
```

与 `feedback-control.md` 不是冲突关系，而是互补。贝叶斯提供了**更精密的决策内核**，feedback-control.md 提供了**更完整的外围系统**（传感器、执行器、审计）。两者的正确关系：

```
传感器层 (feedback-control.md Phase 1)
  → 多信号输入
  → 贝叶斯核 (Phase 2，替代当前硬阈值)
  → 不确定量化后的信念
  → 控制层 (feedback-control.md Phase 2)
  → 动作执行
```
