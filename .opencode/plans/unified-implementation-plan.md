# 基于控制论 + 贝叶斯 + 因果推断的统一实施计划

> 来源融合：
> - `.opencode/plans/feedback-control-system.md` — 传感器层 + 控制器 + 执行器矩阵
> - `融合控制论、贝叶斯与因果推断的轻量编码 Agent 设计.md` — Thompson Sampling + P(stall) + 因果提示 + Token 效率
> - `.opencode/plans/bayesian-control-synthesis.md` — 综合分析与取舍

---

## 一、总体架构

```
┌─────────────────────────────────────────────┐
│           Phase 3. 认知层 (贝叶斯核)         │
│  Beta-Bernoulli 模型选择器 (Thompson Sample) │
│  停滞概率 P(stall) = 1 - 0.5^k              │
│  Token 效能追踪 (ΔE / ΔT)                    │
└──────────────┬──────────────────────────────┘
               │ 概率信念 + 连续分值
┌──────────────▼──────────────────────────────┐
│           Phase 2. 控制层 (信号融合)        │
│  ControllerState (跨轮累积 + 滑动窗)        │
│  信号融合 → ControlAction                   │
│  执行器冲突解决                              │
└──────────────┬──────────────────────────────┘
               │ 调制后的控制指令
┌──────────────▼──────────────────────────────┐
│           Phase 1. 传感器层 (信号采集)       │
│  error.sh (工具失败 / 编译 / 测试失败)       │
│  perf.sh (延迟 / 输出膨胀)                   │
│  context.sh (上下文压力 / 缓存退化)           │
│  progress.sh (修复循环 / 任务停滞)           │
└──────────────┬──────────────────────────────┘
               │ 原始信号 (JSON 行)
┌──────────────▼──────────────────────────────┐
│           Phase 0. 因果提示 (零代码)         │
│  干预与观测分离 — prompt.rs 文本修改          │
│  反事实反思提示 — plan-lifecycle-guidance    │
└─────────────────────────────────────────────┘
```

**实施顺序**：

```
Phase 0 → Phase 1 → Phase 2 → Phase 3
(最易)    (基础)    (核心)    (价值最高但需 Phase 1/2 先完成)
```

---

## 二、Phase 0：因果推断提示模式（零代码，+10 行）

### 目标

在 system prompt 中嵌入因果推理引导，促使 LLM 在每次修改前进行因果思考。与已有 Superpowers 迁移的 `<verification-gate>`、`<rationalization-table>` 形成完整流程。

### 实施细节

**文件**：`src/prompt.rs`，`build_system_prompt()`

**改动**：新增 `<causal-reasoning>` 段，追加在 `plan-lifecycle-guidance` 之后、`instruction-files` 之前。

```rust
sections.push(wrap_section(
    "causal-reasoning",
    concat!(
        "Before every code change, answer silently:\n",
        "1. What specific behavior will this change affect? (cause)\n",
        "2. What observable result do I expect? (effect)\n",
        "3. How will I verify the cause-effect link? (verify)\n",
        "\n",
        "If you cannot answer all three, DO NOT make the change.\n",
        "One change at a time — multiple changes confound causality.\n",
        "Verify immediately after each change to confirm cause-effect."
    ),
    None,
));
```

### 验证

编译后运行 `cargo test` 即可——新增的 prompt 段不会影响任何已有逻辑。可通过 `./target/debug/dscode --list-skills` 验证启动正常。

---

## 三、Phase 1：传感器层（~160 行 Rust + ~80 行 shell）

### 3.1 已存在的基础

- `guard/storm.rs`：StormBreaker 滑动窗口抑制 ✅
- `repair/scavenge.rs`：工具调用回收 + 截断修复 ✅
- `tools/runner.rs`：已集成 StormBreaker 和 truncation repair ✅

**缺失**：shell 脚本传感器 + Rust 调用框架。当前错误检测全靠 `classify_error_from_message` 的硬编码关键词，无法被用户扩展。

### 3.2 传感器合约设计

**输入**（通过 argv + stdin）：

| 参数 | 来源 | 说明 |
|------|------|------|
| `argv[1]` | `call.name` | 工具名（Read/Write/Bash/...） |
| `argv[2]` | `elapsed_ms` | 执行耗时（毫秒） |
| `argv[3]` | `output.len()` | 输出字节数 |
| `stdin` | `final_output` | 工具输出的完整文本 |

**输出**（stdout，单行 JSON）：

```json
// 正常：检测到信号
{"signals":[{"kind":"tool_error","weight":1.0,"detail":"Rust compilation error: 2"}],"actions":[]}
// 正常：无信号
{}
// 异常：exit code ≠ 0 → 控制器忽略此轮
```

**错误处理**：传感器 exit code ≠ 0 时，控制器降级到 Rust 层 `classify_error_from_message`。

### 3.3 文件清单

| 文件 | 操作 | 行数 | 说明 |
|------|------|:----:|------|
| `src/guard/sensor.rs` | 新建 | +60 | `SensorSignal`/`SensorOutput`/`run_sensor()`/`find_sensor()` |
| `src/guard/mod.rs` | 修改 | +1 | 声明 `mod sensor` |
| `assets/sensors/error.sh` | 新建 | +25 | 内置默认错误传感器（Rust 编译/Python 测试/通用） |
| `src/tools/runner.rs` | 修改 | +15 | 在 `execute_one_sync` 返回前调用传感器 |

### 3.4 集成路径

```
execute_one_sync(ctx, call)
  → 工具执行完成，得到 final_output
  → run_sensor("error", tool_name, elapsed, bytes, &final_output)
    → Ok(signals) → 传递到 orchestrator 的 Controller
    → Err/None → 降级到 Rust 层 classify_error_from_message
```

### 3.5 内置 error.sh 传感器

```bash
#!/bin/bash
tool="$1"
output=$(cat)
signals=""

echo "$output" | grep -qi "error\[E" && \
  signals+="{\"kind\":\"tool_error\",\"weight\":1.0,\"detail\":\"Rust compilation error\"},"
echo "$output" | grep -q "FAILED\|AssertionError\|Traceback" && \
  signals+="{\"kind\":\"tool_error\",\"weight\":1.0,\"detail\":\"Test failure\"},"
echo "$output" | grep -qE "exit code [1-9]|^Error:" && \
  signals+="{\"kind\":\"tool_error\",\"weight\":0.5,\"detail\":\"Non-zero exit\"},"

[ -n "$signals" ] && echo "{\"signals\":[${signals%,}]}" || echo "{}"
```

### 3.6 Phase 1 测试

| 测试 | 验证 |
|------|------|
| `run_sensor_returns_signals_for_rust_error` | 编译错误返回 tool_error |
| `run_sensor_returns_empty_for_success` | 正常返回 `{}` |
| `run_sensor_returns_none_when_script_not_found` | 传感器不存在返回 None |
| `error_sensor_detects_rust_error` | error.sh 检测 `error[E0425]` |
| `error_sensor_detects_python_test_failure` | error.sh 检测 `FAILED` |

---

## 四、Phase 2：控制器——信号融合 + 贝叶斯停滞（~120 行 Rust）

### 4.1 已存在的基础

- `TurnFailureTracker`（`src/agent/failure_tracker.rs`）：多信号计数 ✅
- `orchestrator.rs` 中的 `update_after_turn()` ✅
- `Record_compact()`、`record_usage_with_tier()` ✅
- 标题栏实时成本显示 ✅

**缺失**：
- P(stall) 概率替代硬计数 → 更平滑
- 修复循环检测（连续 >15 次工具调用无 end_turn）
- 控制动作输出（Abort/ReflectionHint/UpgradeModel）

### 4.2 控制器状态机

**文件**：`src/agent/controller.rs`（新建）

```rust
pub struct Controller {
    // 贝叶斯停滞概率
    no_progress_count: u32,
    stall_probability: f64,

    // 修复循环检测
    tool_call_count: u32,     // 本轮用户输入内工具调用数
    had_end_turn: bool,       // 本轮是否正常结束

    // 上下文历史 (滑动窗)
    context_pressure_history: VecDeque<f32>,
    cache_hit_history: VecDeque<u8>,

    // 升级阈值
    upgrade_threshold: u32,
}
```

**核心方法**：

```rust
impl Controller {
    /// 贝叶斯停滞概率更新。
    /// P(stall) = 1 - 0.5^k, 其中 k = 连续无进展次数
    /// k=0→P=0, k=1→P=0.5, k=3→P=0.875, k=5→P=0.969
    pub fn note_error(&mut self, error_decreased: bool) {
        if error_decreased {
            self.no_progress_count = 0;
        } else {
            self.no_progress_count += 1;
        }
        self.stall_probability = 1.0 - 0.5_f64.powi(self.no_progress_count as i32);
    }

    /// 产出控制动作。
    /// P(stall) 范围 VS 动作：
    ///   > 0.99 + k ≥ 10 → Abort（请求人工介入）
    ///   > 0.95          → UpgradeModel（建议升 pro）
    ///   > 0.80          → InjectReflectionHint（注入反思提示）
    ///   ≤ 0.80          → None（正常推进）
    pub fn get_control_action(&self) -> Option<ControlAction> {
        if self.stall_probability > 0.99 && self.no_progress_count >= 10 {
            return Some(ControlAction::Abort);
        }
        if self.stall_probability > 0.95 {
            return Some(ControlAction::UpgradeModel);
        }
        if self.stall_probability > 0.80 {
            return Some(ControlAction::InjectReflectionHint);
        }
        None
    }
}

pub enum ControlAction {
    InjectReflectionHint,   // 提示 LLM 改变策略
    UpgradeModel,           // 切换到 pro
    Abort,                  // 请求人工介入
}
```

### 4.3 与旧逻辑的关键差异

| 维度 | 当前（TurnFailureTracker） | Phase 2（Controller） |
|------|--------------------------|----------------------|
| 停滞检测 | 连续失败计数 | `P(stall) = 1 - 0.5^k` |
| 动作触发 | `count ≥ 4 → upgrade` | `P > 0.80 → reflect, > 0.95 → upgrade, > 0.99 → abort` |
| 降级能力 | 无（`model_locked` 永不降级） | 滞回降级（`P < 0.30 → unlock`） |
| 修复循环 | 无 | `tool_call_count > 15 && !had_end_turn` |

### 4.4 集成路径

```rust
// orchestrator.rs
pub struct OrchActor {
    controller: Controller,
    // ... 其余字段不变 ...
}

fn handle_user_input(&mut self, input: String) {
    self.controller.reset_per_turn();  // ← 新增

    match executor.execute(&input).await {
        Ok((decision, effects)) => {
            match decision {
                TurnDecision::Stop => self.controller.note_end_turn(),  // ← 新增
                TurnDecision::Failed(msg) => {
                    // 已有：classify → 升级分数
                    // 新增：controller.note_error(false)
                }
                _ => {}
            }
            // 新增：检查控制动作
            if let Some(action) = self.controller.get_control_action() {
                match action {
                    ControlAction::UpgradeModel => { /* 触发升级 */ }
                    ControlAction::InjectReflectionHint => { /* 注入提示词 */ }
                    ControlAction::Abort => { /* 中止 */ }
                }
            }
        }
        Err(e) => {
            self.controller.note_error(false);  // ← 新增
        }
    }
}
```

### 4.5 Phase 2 测试（7 个）

| 测试 | 断言 |
|------|------|
| `stall_probability_zero_when_progressing` | `note_error(true)` → P=0 |
| `stall_probability_k3` | `note_error(false)×3` → P=0.875 |
| `stall_probability_k5` | `note_error(false)×5` → P>0.968 |
| `get_control_action_at_p08` | P=0.875 → `InjectReflectionHint` |
| `get_control_action_at_p095` | P=0.969 → `UpgradeModel` |
| `get_control_action_abort_at_p099_k10` | P>0.99 + k≥10 → `Abort` |
| `fix_loop_detected` | tool_call=16, had_end_turn=false |

---

## 五、Phase 3：Thompson Sampling 模型选择器（~80 行 Rust）

### 5.1 设计背景

当前模型切换逻辑：

```rust
// orchestrator.rs
fn resolve_active(&self) -> (String, String) {
    let tier = if let Some(forced) = self.forced_model {
        forced
    } else if !self.auto_model_enabled {
        ModelTier::Parse(&config.model)...
    } else if self.model_locked || score ≥ upgrade_threshold {
        ModelTier::Pro
    } else {
        ModelTier::Flash
    };
    (tier.model_name().to_string(), self.api_url.clone())
}
```

这是硬阈值决策：要么 flash 要么 pro，中间没有平滑过渡。

**Thompson Sampling 改进**：每个模型维护 Beta(α, β) 分布，代表"本轮能减少错误的概率"。选择时从每个分布采样，选最高的。自动平衡探索与利用，允许双向切换。

### 5.2 Beta-Bernoulli 信念结构体

**文件**：`src/agent/model_selector.rs`（新建，~70 行）

```rust
pub struct ModelSelector {
    beliefs: Vec<ModelBelief>,
}

struct ModelBelief {
    name: String,
    alpha: f64,  // 成功 + 1（先验）
    beta: f64,   // 失败 + 1（先验）
}

impl ModelSelector {
    /// Greedy 选择：选期望成功率最高的模型。
    /// 不需要 Beta 采样库，直接使用均值 α/(α+β)。
    pub fn select_greedy(&self) -> &str {
        self.beliefs.iter()
            .max_by(|a, b| a.mean().partial_cmp(&b.mean()).unwrap())
            .map(|b| b.name.as_str())
            .unwrap_or("flash")
    }

    /// 更新信念。
    pub fn update(&mut self, model: &str, success: bool) {
        if let Some(b) = self.beliefs.iter_mut().find(|b| b.name == model) {
            if success { b.alpha += 1.0; } else { b.beta += 1.0; }
        }
    }
}
```

### 5.3 选择的降级路线

| 级别 | 方法 | 依赖 | 适用 |
|:----:|------|------|------|
| 最优 | Thompson Sampling（Beta 采样） | Gamma 随机数生成 | 需要完整探索-利用平衡 |
| 中等 | Greedy（选均值最高） | 无 | 大多数场景，简单可靠 |
| 保底 | 当前硬阈值逻辑 | 无 | 传感器失效时的熔断 |

**建议**：从 Greedy 开始（零依赖），如果发现探索不足再升级到真随机采样。

### 5.4 集成路径

```rust
// orchestrator.rs
pub struct OrchActor {
    controller: Controller,
    model_selector: ModelSelector,  // 新增
    // ...
}

fn handle_user_input(&mut self, input: String) {
    // 注册模型（首次使用时）
    self.model_selector.ensure("flash");
    self.model_selector.ensure("pro");

    // 选择模型
    let selected = self.model_selector.select_greedy();

    // 根据选择创建 LLM client
    let model_name = ModelTier::parse(selected).unwrap().model_name();
    let llm = AsyncLlClient::new(model_name, ...)?;

    // ... 执行 ...

    // 执行完毕后更新信念
    let success = matches!(decision, TurnDecision::Stop);
    self.model_selector.update(selected, success);
}
```

### 5.5 与 Controller 的交互

Thompson Sampling 和 Controller 不冲突，而是互补：

```
Controller
  ├── stall_probability → 决定是否升级
  ├── fix_loop          → 决定是否注入反思提示
  └── 这些是"此刻是否应该中断当前策略"

ModelSelector
  └── 决策"下一次尝试用 flash 还是 pro"
      └── 即使 Controller 没有检测到停滞，Selector 也可能
          因为 pro 的成功率信念更高而自动选择 pro
```

两者在 `resolve_active()` 中结合：

```rust
fn resolve_active(&self) -> (String, String) {
    let tier = if let Some(forced) = self.forced_model {
        forced
    } else if self.controller.stall_probability > 0.95 {
        ModelTier::Pro  // Controller 紧急升级
    } else {
        // Thompson Sampling 正常选择
        let selected = self.model_selector.select_greedy();
        ModelTier::parse(selected).unwrap_or(ModelTier::Flash)
    };
    (tier.model_name().to_string(), self.api_url.clone())
}
```

### 5.6 Phase 3 测试（5 个）

| 测试 | 断言 |
|------|------|
| `model_selector_initial_mean_is_05` | 新模型 α=1,β=1 → mean=0.5 |
| `update_increases_alpha_on_success` | 成功 → mean 上升 |
| `update_increases_beta_on_failure` | 失败 → mean 下降 |
| `greedy_picks_better_model` | α=10,β=1 的模型优先于 α=1,β=1 |
| `selector_does_not_crash_with_empty_registry` | 空注册表返回默认 |

---

## 六、Phase 0-3 的完整文件变更清单

| Phase | 文件 | 操作 | 行数 |
|:-----:|------|:----:|:----:|
| 0 | `src/prompt.rs` | 新增 `<causal-reasoning>` 段 | +10 |
| 1 | `src/guard/sensor.rs` | 新建：传感器合约 + 调用框架 | +60 |
| 1 | `src/guard/mod.rs` | 声明 `mod sensor` | +1 |
| 1 | `assets/sensors/error.sh` | 新建：内置默认传感器 | +25 |
| 1 | `src/tools/runner.rs` | 集成传感器调用 | +15 |
| 2 | `src/agent/controller.rs` | 新建：Controller + ControlAction | +90 |
| 2 | `src/agent/mod.rs` | 声明 `mod controller` | +1 |
| 2 | `src/agent/orchestrator.rs` | Controller 替代 TurnFailureTracker | ~±25 |
| 2 | `src/agent/turn.rs` | note_end_turn/note_tool_call 集成 | +5 |
| 3 | `src/agent/model_selector.rs` | 新建：Beta-Bernoulli 选择器 | +70 |
| 3 | `src/agent/orchestrator.rs` | 模型选择器集成 | +15 |
| — | **合计** | — | **~317 新行** |

---

## 七、删除清单

| 删除项 | 文件 | 替代物 |
|--------|------|--------|
| `TurnFailureTracker`（所有代码） | `src/agent/failure_tracker.rs` | `Controller`（Phase 2） |
| `record_usage()` 无 tier 版本 | `src/session/stats.rs` | 已移除 |
| `model_locked` 永不降级语义 | `src/agent/orchestrator.rs` | `Controller.get_control_action()` + Thompson Sampling |

---

## 八、收益预估

| 维度 | 当前 | Phase 2 后 | Phase 3 后 |
|------|------|-----------|-----------|
| 模型切换 | 单向 flash→pro，永不降级 | P(stall) 滞回 + 可降级 | Thompson Sampling 自动双向 |
| 停滞检测 | 硬计数（3次→反射，5次→中止） | P(stall) 连续概率 | 保留 |
| 错误信号 | 硬编码 Rust 关键词 | 可定制 shell 传感器 | 保留 |
| 因果推理 | 无引导 | prompt 级引导 | 保留 |
