# Auto-Mode 双模型切换：工程控制论设计方案

## 背景

DeepSeek Flash（快/经济）vs DeepSeek Pro（强/贵）。简单任务 flash 足够，复杂任务 flash 可能失败循环。需要一个 auto 模式：默认 flash，检测到能力不足时自动升级 pro。

## 控制模型

```
OrchActor (Supervisory)
  ├─ active_model: "deepseek-chat"    ← 当前活跃
  ├─ failure_count: 0                 ← 信号累加器
  ├─ model_locked: false              ← Hysteresis lock
  ├─ switch_threshold: 2              ← Bang-bang threshold
  │
  │  resolve_active():
  │    if model_locked → pro          ← Hysteresis: 不降级
  │    if failures ≥ threshold → pro  ← Bang-bang: 触发
  │    else → flash
  │
  ▼
TurnDecision arrives
  ├─ success → reset counter (if not locked)
  ├─ failure → +1 counter → if ≥ threshold → switch + lock
```

## 控制论映射

| 概念 | 实现 |
|------|------|
| Bang-bang control | Flash ↔ Pro 二态切换，基于阈值 |
| Supervisory control | OrchActor 选择下层模型 |
| Hysteresis | 升级后锁定，不自动降级 |
| Cost optimization | Flash 低价稳态，Pro 仅在需要时触发 |
| Signal reliability | 只用"连续 TurnDecision::Failed"一个信号 |

## 信号设计

触发升级的信号：**连续 TurnDecision::Failed（且不是 "interrupted"）**。

TurnDecision::Failed 的来源（turn.rs）：
1. LLM stream 发送失败 (line 87)
2. Stream chunk 解析错误 (line 102)
3. LLM 返回 error 事件 (line 146)
4. max_tokens / length 截断 (line 272)

阈值 = 2（1 次可能偶发，2 次确认不是偶发）。

降级：不自动降级（Hysteresis 上沿是升级锁）。

## 代码改动总览

| 文件 | 改动 | 行数 |
|------|------|------|
| config.rs | 5 新字段 + 5 CLI + 5 env + 默认值填充 | ~50 |
| orchestrator.rs | 7 新字段 + resolve_active / update_after_turn / switch_to_secondary | ~45 |
| llm/client.rs | model/api_url 字段 + 修改 new/stream 签名 | ~25 |
| provider.rs | build_claude_request_with_model(model) | ~15 |
| llm/transport.rs | new_transport_for(provider) | ~8 |
| config.rs | api_url_for_provider(provider, base_url) | ~12 |
| stats.rs | model_switches / secondary_turns 字段 + record 方法 | ~15 |
| turn.rs | active_model 字段 + 标题适配 | ~5 |
| sub_executor.rs | AsyncLlClient::new 调用适配 | ~3 |
| compaction.rs | run_summary_call 适配 | ~3 |
| **总计** | | **~181 行** |

## 关键实现细节

### config.rs — 新增字段

```rust
pub auto_model: bool,              // --auto-model
pub secondary_provider: String,    // --secondary-provider  
pub secondary_model: String,       // --secondary-model
pub secondary_base_url: String,    // --secondary-base-url
pub auto_switch_threshold: u32,    // --auto-threshold (默认2)
```

默认值：auto_model=false, secondary_model=""(空=同 provider default), threshold=2。

### orchestrator.rs — 核心状态机

OrchActor 维护 7 个新字段。关键方法：
- resolve_active() → 返回当前应使用的 (model, provider, transport, api_url)
- update_after_turn(decision) → 成功则 reset counter，失败则 ++counter + check threshold
- switch_to_secondary() → 更新 active_* 字段 + model_locked = true + 记录 log_event + 通知用户

### llm/client.rs — 脱离 ctx.config 依赖

AsyncLlClient 新增 model/api_url 字段。stream() 用 self.model 替代 ctx.config.model 构建请求体，用 self.api_url 替代 ctx.api_url 发请求。

### provider.rs — model 覆盖函数

新增 build_claude_request_with_model(..., model: &str)，json body 中 "model" 字段用传入值。

### 降级与不降级策略

不自动降级（Pro → Flash）。如果未来需要：连续 N 次 TurnDecision::Stop + 且上下文压力 < 50% 后解锁 model_locked。

## 使用示例

```bash
# 默认: flash 失败 2 次自动切 pro
dscode -p openai -m deepseek-chat --auto-model "complex task"

# 环境变量
AUTO_MODEL=true SECONDARY_MODEL=deepseek-chat dscode -p openai -m deepseek-chat "task"

# 跨 provider
dscode -p openai -m deepseek-chat --auto-model \
  --secondary-provider claude --secondary-model claude-sonnet-4-20250514 "task"
```

## 测试场景

| 场景 | 期望 |
|------|------|
| flash 连续成功 | 不切换 |
| flash 第1次失败 | failure_count=1 |
| flash 第2次失败 | → pro, model_locked=true |
| pro 又失败 | counter 累加但不再次切换 |
| auto_model=false | 完全不变 |
| 子 agent flash 失败 | 子 agent 独立切换 |
