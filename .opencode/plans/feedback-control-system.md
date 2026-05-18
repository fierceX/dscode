# 反馈控制回路设计 —— 基于工程控制论的 Agent 自调节系统

> 状态：设计提案
> 参考：DeepSeek-Reasonix `turn-failure-tracker.ts`、`storm.ts`、`context-manager.ts`

## 一、问题陈述

### 1.1 当前架构的局限

dscode 的 Auto-Model 升级和上下文压缩是两个**独立的开环决策**：

```
工具执行失败       →   TurnDecision::Failed →   升级分数
SSE 解析失败       →   同上                   →   升级分数
context > 85%     →   CompactionTier         →   压缩
```

每一个决策只依赖**单一信号源**，没有信号之间的相互验证，也没有基于组合信号的多级控制。

### 1.2 工程控制论的视角

经典反馈控制系统由三部分构成：

```
                     ┌──────────────────┐
                     │    控制器        │
                     │   Controller     │
        ┌────────────┤                  ├──────────────┐
        │ 参考信号   │  计算偏差        │  控制指令    │
        │ (目标值)   │  = 目标 - 实际    │              │
        └────────────┤                  ├──────────────┘
                     └──────────────────┘
                      ↑                ↓
                 ┌────┘                └────┐
                 ↓                          ↓
          ┌────────────┐            ┌────────────┐
          │   传感器    │            │   执行器    │
          │   Sensor   │◄───────────│  Actuator  │
          └────────────┘  系统状态  └────────────┘
                 ↑                          │
                 └────── 被控对象 ───────────┘
                       (Agent Loop)
```

在 dscode 中：
- **被控对象**：Agent 循环（LLM 调用 → 工具执行 → 持久化 → 决策）
- **传感器**：当前为隐式（错误分类、上下文比率），缺失显式的反馈信息采集层
- **控制器**：OrchActor 中的 `update_after_turn()` 和 CompactionEngine 中的 `should_compact()`
- **执行器**：模型切换、上下文压缩、工具抑制、输出截断

**核心问题**：传感器层是零散的，控制器之间没有协调，执行器之间可能冲突。

---

## 二、多层反馈回路设计

### 2.1 四层时间尺度

反馈回路的响应速度应与被控量的变化速度匹配。四个时间尺度的反馈回路：

```
采样频率    回路              周期      控制目标
─────────────────────────────────────────────────────
每次工具调用   快速回路 (Fast)     ~1-5s    抑制重复、修复错误、调整超时
每轮 LLM 调用  中速回路 (Medium)   ~10-60s  缓存优化、token 效率
每用户输入后   慢速回路 (Slow)     ~分钟级   模型升级、任务进度、成本控制
请求发送前     预测回路 (Predict)  每次前    预判压缩、超时调整
```

### 2.2 回路详细设计

#### 快速回路（每次工具执行后）

```
输入：工具名、执行耗时、输出大小、输出内容、是否有 Error: 前缀
控制器：ToolRunner + StormBreaker
执行器：抑制、截断、重试、注入提示

信号类型：
  - 重复检测：同(name, args) N次 → suppress
  - 失败检测：输出含 Error: → 累积计数
  - 延迟检测：elapsed > 5s → 标记 slow
  - 输出膨胀：output > 100KB → 标记 large
  - 噪声检测：重复行 > 50% → compress
```

#### 中速回路（每轮 LLM 调用后）

```
输入：token 用量、缓存命中率、上下文使用率、thinking 长度
控制器：OrchActor + CompactionEngine
执行器：提前压缩、模型升级、调整 system prompt

信号类型：
  - 上下文压力：(current/max) 连续上升 > 0.8 → 提前压缩
  - 缓存退化率：cache_pct 连续 N 轮下降 → 重建前缀
  - 思考质量：thinking 为空或过长 → 调整 reasoning_effort
  - token 效率：(output/total) 过低 → 考虑换策略
```

#### 慢速回路（每次用户输入后）

```
输入：任务进度（todo 完成率）、成本累计、修复循环次数
控制器：OrchActor + TurnFailureTracker
执行器：模型锁定、计划重新确认、用户提示

信号类型：
  - 修复循环：同工具调用 > 15 次 + 无 end_turn → 建议升级
  - 任务停滞：多轮无 todo 进展 → 提示取消
  - 成本逼近：累计成本 > 预算 × 0.8 → 警告
  - 升级决议：综合信号融合 → lock_model
```

#### 预测回路（每次 LLM 请求发送前）

```
输入：估算 token 数、历史响应时长、当前上下文
控制器：Preflight compressor
执行器：预判压缩、超时调整

信号类型：
  - token 超标：estimate > max × 0.95 → 紧急压缩
  - 低缓存率：上次压缩后 cache_pct 低于预期 → 提前压缩
  - 响应预测：历史平均时长 × 安全系数 → 设置超时
```

---

## 三、信号体系

### 3.1 信号层级与优先级

| 优先级 | 信号类别 | 信号来源 | 置信度 | 控制类型 |
|:------:|---------|---------|:-----:|---------|
| P0 | 工具异常（stderr 含编译/测试失败） | Bash/Test 输出 | 高 | 抑制 + 升级 |
| P0 | 工具执行失败（Rust 层 Err） | `execute_one_sync` | 高 | 升级 |
| P0 | 上下文溢出风险（>95%） | `estimateRequestTokens` | 高 | 紧急压缩 |
| P1 | 修复循环（连续 tool_use > 15 次） | turn 计数器 | 中高 | 升级警告 |
| P1 | 上下文压力（连续 > 0.8） | `current/max` | 中 | 提前压缩 |
| P2 | 思考质量（stuck、iteration 关键词） | `reasoning_content` | 中低 | 组合信号 |
| P2 | 性能退化（延迟上升、输出变大） | 工具耗时/输出大小 | 中 | 调优控制 |
| P3 | 成本逼近（>80% 预算） | UsageEvent | 高 | 用户警告 |
| P3 | 任务停滞（todo 速率降低） | TodoWrite | 中低 | 建议取消 |

### 3.2 信号融合规则

单信号置信度低时，通过组合提升决策置信度：

```python
def evaluate_upgrade(signals):
    score = 0
    if signals.tool_error >= 2:
        score += 2
    if signals.parse_error >= 1:
        score += 1
    if signals.stuck >= 3:
        score += 3
    if signals.fix_loop:
        score += 2
    if signals.stuck and signals.fix_loop:
        score += 1  # 组合加成
    return score >= 4

def evaluate_compact(signals):
    # 单一信号不允许压缩决策
    if signals.pressure > 0.95:
        return Emergency
    if signals.pressure > 0.8 and signals.degrading:
        return ForceSummary
    if signals.pressure > 0.8 and signals.large_output:
        return ForceSummary
    # 默认不压缩
    return None
```

### 3.3 信号组合示例

| 场景 | 信号组合 | 融合结论 | 控制动作 |
|------|---------|---------|---------|
| LLM 在修复 Rust 类型错误，2 次 Bash 编译都失败但不同错误 | tool_error=2 | 修复循环 | 建议升级 |
| 上下文从 200K 迅速涨到 600K，缓存命中从 95% 降到 40% | pressure=0.6 + degrading | 即将退化 | 提前压缩 |
| 大文件爬虫任务：连续 5 次 WebFetch 返回 > 200KB | large_output=5 | 流量过大 | 截断 + 降并发 |
| 凌晨 3 点，美国用户：会话成本达到 $18 | cost>$15 + no_todo=3 | 预算预警 | 提示节省 |
| 编译两次失败 + 思考内容含 "I keep making the same mistake" | tool_error=2 + stuck | 高置信度卡住 | 立即升级 |

---

## 四、传感器层设计

### 4.1 设计原则

1. **与 Agent Core 物理隔离**：传感器是独立脚本/进程，通过 stdin/stdout 与 Rust 控制器通信
2. **项目可定制**：每个项目可放置自己的传感器，覆盖内置默认
3. **失败安全**：传感器脚本出错时，退回到 Rust 层默认行为（不引入新故障）
4. **可组合**：多个传感器可以叠加，控制器负责融合
5. **零编译依赖**：修改传感器不需要重新编译 agent

### 4.2 传感器合约

```bash
#!/bin/bash
# ============================================================
# 传感器名称: error
# 描述: 检测工具输出中的错误信号
# 输入:
#   argv[1] = 工具名 (Read/Write/Edit/Bash/Glob/Grep/...)
#   argv[2] = 执行耗时 (毫秒)
#   argv[3] = 输出字节数
#   stdin   = 工具输出的文本内容
# 输出 (单行 JSON):
#   {
#     "signals": [
#       {"kind": "tool_error", "weight": 1.0, "detail": "compilation failed: 3 errors"}
#     ],
#     "actions": []         // 推荐动作（可选）
#   }
# 空信号:
#   {}
# 退出码: 0 (正常), 非 0 (传感器执行异常，控制器忽略此轮)
# ============================================================

tool="$1"
elapsed="$2"
output_bytes="$3"
output=$(cat)

errors=0
signals=""

# Rust 编译错误
errors=$(echo "$output" | grep -ci "error\[E" 2>/dev/null || echo 0)
if [ "$errors" -gt 0 ]; then
    signals+="{\"kind\":\"tool_error\",\"weight\":1.0,\"detail\":\"compilation failed: $errors errors\"},"
fi

# Python 测试失败
if echo "$output" | grep -q "FAILED\|AssertionError\|Traceback (most recent call last)" 2>/dev/null; then
    signals+="{\"kind\":\"tool_error\",\"weight\":1.0,\"detail\":\"test failure detected\"},"
fi

# 通用退出码
if echo "$output" | grep -qE "exit code [1-9]|Error:" 2>/dev/null; then
    signals+="{\"kind\":\"tool_error\",\"weight\":0.5,\"detail\":\"non-zero exit or Error: prefix\"},"
fi

# 输出信号
signals=$(echo "$signals" | sed 's/,$//')
if [ -n "$signals" ]; then
    echo "{\"signals\":[$signals]}"
else
    echo "{}"
fi
```

### 4.3 其他传感器示例

**性能传感器 (perf.sh)**

```bash
# 检测工具执行延迟和输出大小异常
elapsed="$2"
bytes="$3"
signals=""

[ "$elapsed" -gt 5000 ] && signals+="{\"kind\":\"tool_slow\",\"detail\":\"${elapsed}ms\"},"
[ "$bytes" -gt 102400 ] && signals+="{\"kind\":\"output_large\",\"detail\":\"${bytes}B\"},"

[ -n "$signals" ] && echo "{\"signals\":[${signals%,}]}" || echo "{}"
```

**上下文传感器 (context.sh)**

```bash
# 输入: 环境变量 $CURR_CTX $MAX_CTX $CACHE_HIT $LAST_COMPACT_TURN
pressure=$(echo "scale=2; $CURR_CTX / $MAX_CTX" | bc)
signals=""

# 压力分级
if [ "$(echo "$pressure > 0.95" | bc)" -eq 1 ]; then
    signals+="{\"kind\":\"ctx_critical\",\"weight\":1.0},"
elif [ "$(echo "$pressure > 0.80" | bc)" -eq 1 ]; then
    signals+="{\"kind\":\"ctx_pressure\",\"weight\":0.6},"
fi

# 缓存退化
if [ -n "$CACHE_HIT" ] && [ "$CACHE_HIT" -lt 40 ]; then
    signals+="{\"kind\":\"cache_degrading\",\"detail\":\"hit=${CACHE_HIT}%\"},"
fi

[ -n "$signals" ] && echo "{\"signals\":[${signals%,}]}" || echo "{}"
```

**修复循环传感器 (progress.sh)**

```bash
# 输入: 环境变量 $TURN_COUNT $LAST_END_TURN_TURN $TOOL_COUNT
# 检测同一用户输入内的修复循环
signals=""

# 超过 10 次工具调用且没有 end_turn → 可能在修复循环
if [ "$TOOL_COUNT" -gt 10 ] && [ -z "$LAST_END_TURN_TURN" ]; then
    signals+="{\"kind\":\"fix_loop\",\"detail\":\"${TOOL_COUNT} tool calls without end_turn\"},"
fi

# 进度停滞：多轮无 todo 变化
if [ -n "$TODO_STALE_ROUNDS" ] && [ "$TODO_STALE_ROUNDS" -gt 3 ]; then
    signals+="{\"kind\":\"progress_stalled\",\"detail\":\"no todo progress for ${TODO_STALE_ROUNDS} rounds\"},"
fi

[ -n "$signals" ] && echo "{\"signals\":[${signals%,}]}" || echo "{}"
```

### 4.4 传感器发现路径

```
传感器加载优先级（后加载覆盖前加载）:

1. <project>/.dscode/sensors/<name>.sh    ← 项目级（最高优先级）
2. ~/.dscode/sensors/<name>.sh            ← 用户级
3. <binary-embedded>/assets/sensors/<name>.sh ← 内置默认（最低优先级）

控制器根据优先级合并信号：同名传感器只有最高优先级的生效。
不同名传感器各自独立运行，信号在控制器层融合。
```

---

## 五、控制器架构

### 5.1 控制器状态机

```rust
// 控制器维护的累积状态（跨轮持久化）
struct ControllerState {
    // 快速回路状态
    consecutive_tool_errors: u32,      // 连续工具错误数
    consecutive_large_outputs: u32,    // 连续大输出数
    consecutive_slow_tools: u32,       // 连续慢工具数
    
    // 中速回路状态
    context_pressure_history: Vec<f32>,  // 最近 N 轮的压力记录
    cache_hit_history: Vec<u8>,         // 最近 N 轮的缓存命中率
    reasoning_quality_history: Vec<f32>, // 最近 N 轮的思考质量评分
    
    // 慢速回路状态
    fix_loop_detected: bool,           // 是否检测到修复循环
    upgrade_score: u32,                 // 升级累积分数
    cost_accumulated: f64,             // 累计成本（USD）
    budget: Option<f64>,                // 预算上限
    
    // 预测回路状态
    average_response_latency: f32,     // 平均 LLM 响应时长
    predicted_next_tokens: u64,        // 预测下次请求的 token 数
}

impl ControllerState {
    /// 每轮开始：重置瞬时状态
    fn reset_turn(&mut self) {
        self.consecutive_tool_errors = 0;
        self.consecutive_large_outputs = 0;
        self.consecutive_slow_tools = 0;
        self.fix_loop_detected = false;
        // 历史信号不清零
    }
    
    /// 每轮结束：滑动窗口更新
    fn update_history(&mut self, pressure: f32, cache_hit: u8, latency: f32) {
        const WINDOW_SIZE: usize = 10;
        self.context_pressure_history.push(pressure);
        self.cache_hit_history.push(cache_hit);
        self.reasoning_quality_history.push(latency);  // placeholder
        
        self.context_pressure_history.truncate(WINDOW_SIZE);
        self.cache_hit_history.truncate(WINDOW_SIZE);
        self.reasoning_quality_history.truncate(WINDOW_SIZE);
    }
}
```

### 5.2 信号融合算法

```rust
impl FeedbackController {
    /// 处理传感器返回的信号
    fn process_signals(&mut self, sensor_outputs: Vec<SensorOutput>) -> Vec<ControlAction> {
        let mut actions = Vec::new();
        let mut upgrade_temp = 0;
        
        for output in &sensor_outputs {
            for signal in &output.signals {
                match signal.kind.as_str() {
                    "tool_error" => {
                        self.state.consecutive_tool_errors += 1;
                        if self.state.consecutive_tool_errors >= 3 {
                            upgrade_temp += (signal.weight * 2.0) as u32;
                        }
                    }
                    "fix_loop" => {
                        upgrade_temp += 3;
                        actions.push(ControlAction::Notify("stuck, consider /pro"));
                    }
                    "ctx_pressure" => {
                        if signal.weight >= 0.8 {
                            actions.push(ControlAction::Compact(CompactionTier::ForceSummary));
                        }
                    }
                    "ctx_critical" => {
                        actions.push(ControlAction::Compact(CompactionTier::Emergency));
                    }
                    "cache_degrading" => {
                        // 检查是否连续退化
                        let recent: Vec<u8> = self.state.cache_hit_history.iter()
                            .rev().take(3).copied().collect();
                        if recent.len() >= 3 && recent.iter().all(|&h| h < 40) {
                            actions.push(ControlAction::Compact(CompactionTier::ForceSummary));
                        }
                    }
                    "tool_slow" => {
                        self.state.consecutive_slow_tools += 1;
                        if self.state.consecutive_slow_tools >= 3 {
                            actions.push(ControlAction::ReduceTimeout(signal.detail));
                        }
                    }
                    "output_large" => {
                        self.state.consecutive_large_outputs += 1;
                        if self.state.consecutive_large_outputs >= 2 {
                            actions.push(ControlAction::TruncateEarlier(signal.detail));
                        }
                    }
                    _ => {}
                }
            }
        }
        
        // 融合升级分数
        self.state.upgrade_score += upgrade_temp;
        if self.state.upgrade_score >= 4 && !self.model_locked {
            actions.push(ControlAction::LockModel);
        }
        
        actions
    }
}
```

---

## 六、控制执行器矩阵

### 6.1 执行器列表

| 执行器 | 控制变量 | 调节方式 | 可逆性 | 延迟 | 应用频率限制 |
|--------|---------|---------|:-----:|:---:|:----------:|
| **抑制工具调用** (StormBreaker) | 调用频率 | 开关 | ✅ 下次重置 | 立即 | 每次工具调用 |
| **截断输出** | 输出大小 | 提前截断 | ✅ 临时 | 立即 | 每次工具调用 |
| **缩短超时** | 超时秒数 | 渐变调节 | ✅ | 本轮生效 | 每 3 个 slow 信号 |
| **提前压缩** (ForceSummary) | 上下文窗口 | 一次性 | ❌ | 立即 | 每次用户输入最多 1 次 |
| **紧急压缩** (Emergency) | 上下文窗口 | 紧急 | ❌ | 立即 | 无限制 |
| **锁定模型** (flash→pro) | LLM 模型 | 开关 | ❌ | 下一轮用户输入 | 每会话最多 1 次 |
| **通知用户** | 用户注意力 | 消息 | ✅ | 本轮结束 | 每 3 次升级信号 |
| **建议中止** | 任务执行 | 建议 | ✅ | 用户手动 | 每 5 次 stuck 信号 |

### 6.2 执行器冲突解决

两个执行器可能在同一轮被触发：例如同时触发"提前压缩"和"紧急压缩"。

```rust
fn resolve_conflicts(actions: Vec<ControlAction>) -> Vec<ControlAction> {
    // 互斥规则：
    // - Emergency > ForceSummary（只取最激进）
    // - LockModel + Notify → LockModel 优先（用户已知）
    // - TruncateEarlier + ReduceTimeout → 可以共存
    
    let mut has_emergency = false;
    let mut has_force_summary = false;
    let mut filtered = Vec::new();
    
    for action in actions {
        match action {
            ControlAction::Compact(Emergency) => has_emergency = true,
            ControlAction::Compact(ForceSummary) => has_force_summary = true,
            _ => filtered.push(action),
        }
    }
    
    if has_emergency {
        filtered.push(ControlAction::Compact(Emergency));
    } else if has_force_summary {
        filtered.push(ControlAction::Compact(ForceSummary));
    }
    
    filtered
}
```

---

## 七、实施路线图

### Phase 1: 传感器基础框架（~100 行 Rust + ~60 行 shell）

- 定义传感器合约格式（stdin/stdout JSON）
- 实现传感器发现路径复用（复用 skills 路径解析）
- 在 ToolRunner 中集成传感器调用（`execute_one_sync` 后 pipe 输出到传感器脚本）
- 内置默认 `error.sh` 传感器（覆盖 Rust/Python/通用编译测试错误）
- 将传感器信号接入 `TurnFailureTracker`

### Phase 2: 信号融合控制器（~80 行 Rust）

- 实现 `FeedbackController` 和 `ControllerState`
- 信号融合算法（组合弱信号提升置信度）
- 执行器冲突解决逻辑
- 复用现有的 `TurnEffect::NeedsPro` 路径

### Phase 3: 扩展传感器与执行器（~120 行 shell + ~60 行 Rust）

- 新增 `perf.sh`（延迟/输出大小检测）
- 新增 `context.sh`（压力/缓存退化）
- 新增 `progress.sh`（修复循环/任务停滞）
- 新增执行器：`ReduceTimeout`、`TruncateEarlier`、`Notify`、`SuggestAbort`

### Phase 4: 生产化（~80 行）

- 传感器执行超时保护（>2s 杀进程）
- 传感器输出大小限制（>16KB 截断）
- 传感器故障降级（运行失败→回到 Rust 兜底）
- 传感器审计日志（每个信号 → 记录触发来源）
- 控制动作审计日志（每个动作 → 记录决策依据）

---

## 八、关键决策记录

### 为什么是脚本而不是 Rust 插件？

1. **零编译依赖**：项目组换语言只需要修改 shell 脚本，不需要 PR 审核 Rust 代码
2. **天然隔离**：进程级隔离，传感器崩溃不影响 agent core
3. **可测试性**：`echo "test output" | ./error.sh Bash` 极快反馈
4. **社区生态**：shell 是最通用的脚本语言，用户无需学新 DSL

### 为什么信号只在控制器层融合，不在传感器层？

1. 单个传感器只输出单一信号的置信度（"我看到的 failure 概率是 0.8"），不负责组合
2. 控制器拥有完整的状态视图（历史数据、跨传感器关联）
3. 分层决策：传感器层做信号提取，控制器层做信号融合

### 为什么传感器不负责执行控制？

1. 避免传感器之间产生副作用冲突（两个传感器都决定压缩→冲突）
2. 执行器需要全局状态感知（现有上下文、已有压缩次数、模型锁定状态）
3. 传感器只输出"建议"（`actions: ["compact"]`），由控制器统一决策

### 为什么故障时退回 Rust 层，而不是报错？

1. 传感器是可选的增强层，不是关键路径
2. 传感器故障不应该使 agent 无法工作
3. Rust 层兜底保证了最基本的功能（`classify_error_from_message` + `should_compact`）
