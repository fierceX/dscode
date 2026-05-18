# 跨项目设计模式迁移分析

基于 DeepSeek-Reasonix (TypeScript) 和 DeepSeek-TUI (Rust) 的深度分析，
评估哪些设计模式可迁移至 dscode Rust 项目，聚焦 auto-mode 与工程控制论主题。

---

## 一、Auto-Model 切换模式对比

### 1.1 Reasonix 的失败信号驱动升级（高优先级迁移）

**文件**: `DeepSeek-Reasonix/src/loop/turn-failure-tracker.ts` (42行)

**机制**: TurnFailureTracker — 单信号闭环控制：
```
每轮累计: repair.fire 次数 + SEARCH-mismatch 次数
阈值 = 3 (默认) → 触发升级
每轮开始 → 重置计数器
```

**与我们的方案对比**:
| 维度 | Reasonix | 我们的设计 |
|------|----------|-----------|
| 信号 | repair.fire + search-mismatch | TurnDecision::Failed (连续) |
| 阈值 | 3 (可配置 escalate_after) | 2 |
| 升级方向 | flash→pro | flash→pro |
| 降级 | 无（每轮重置=允许降级） | Hysteresis lock（永不降级） |
| 自报告 | `<<<NEEDS_PRO>>>` 标记 | (暂无) |

**可迁移的点**:
1. **多信号融合**: 不应只用 `TurnDecision::Failed`，还应包含 tool parse error、repair 触发器。Reasonix 的 `repair.fire` 信号值得借鉴——dscode 的 `SseParser` 出错也应算入。
2. **可配置阈值**: Reasonix 的 `escalate_after` 通过 ENV 可配置，我们应保留此灵活性。
3. **自报告升级**: `<<<NEEDS_PRO>>>` 模式——LLM 在回复中自我标记需要更强模型，是一个巧妙的"模型即传感器"模式。这可以作为额外的升级信号源（在 SSE 流中检测此标记）。

### 1.2 TUI 的启发式 + Flash 路由 (中等优先级迁移)

**文件**: `DeepSeek-TUI/crates/tui/src/commands/config.rs` (lines 720-814)

**双层决策**:
- **Tier 1 (本地启发式)**: 消息包含 "refactor"/"architecture"/"debug" → Pro；短消息<100字 → Flash；长消息>500字 → Pro
- **Tier 2 (Flash 路由)**: 先让 Flash 跑一次分类 → 复杂则→Pro。带 source 标记

**评价**:
- 启发式规则简单但脆弱（关键词匹配不如信号驱动可靠）
- Flash 路由额外消耗一次 API 调用——成本第一回合就加倍，性价比待验证
- **建议**: 不采纳 Flash 路由（成本太高）。启发式可作为 `auto_model=false` 时的静态默认选择（无需运行时切换）。

### 1.3 Reasonix 的每轮模型决策

**文件**: `DeepSeek-Reasonix/src/loop.ts` (line 429-431)

```typescript
modelForCurrentCall(): string {
  if (this._escalateThisTurn || this._proArmed || ...) return proModel
  return flashModel
}
```

**关键细节**: 模型选择在每个 LLM 调用前动态决议，不是 per-session 固定。dscode 的 `resolve_active()` 也应 per-turn 决议。

---

## 二、过程控制 & 工程控制论模式

### 2.1 三级压缩/折叠阈值（高优先级迁移）

**文件**: `DeepSeek-Reasonix/src/context-manager.ts` (lines 18-30, 251行)

**比例控制带**:
```
50% → fold (正常折叠，保留20% tail budget)
70% → aggressive fold (激进折叠，仅10% tail budget)
80% → force-summary (强制总结)
95% → preflight emergency (预判紧急折叠)
```

**当前 dscode 状态**: 我们的 `DP_BETA=0.03` 只有单阈值压缩决策，缺乏多级渐进式响应。

**迁移建议**:
1. 引入多级 `CompactionTier` 枚举 (Conservative / Aggressive / ForceSummary / Emergency)
2. 在不同 tier 使用不同 `DP_BETA` 值（越高越激进）
3. 保留技能/工具定义内容跨越折叠（像 Reasonix 的 skill-pin body preservation）

**代码改动预估**: compaction.rs +30行

### 2.2 Storm Breaker 抑制模式（中等优先级迁移）

**文件**: `DeepSeek-Reasonix/src/repair/storm.ts` (66行)

**机制**: 滑动窗口追踪 (name, args) 对，连续相同调用 >= 阈值 → bang-bang 抑制。

```
窗口大小 = 6, 阈值 = 3
同类(read-only)调用累计 → 抑制
出现写入调用 → 清空之前的只读计数（状态变化=重置）
全部被抑制时 → 允许模型重试一次原始 tool_calls
```

**迁移建议**:
- dscode 在 `dispatch.rs` 或 `orchestrator.rs` 中添加 `StormBreaker` 结构
- 追踪最近 N 次工具调用，检测重复模式循环
- 触发抑制时发送 Event::ToolCallSuppressed + 注入 "you're stuck in a loop" 提示

**代码改动预估**: 新建 `src/guard/storm.rs` ~60行

### 2.3 预判门 (Feedforward Guard) (中等优先级迁移)

**文件**: `DeepSeek-Reasonix/src/context-manager.ts` (lines 130-143)

**机制**: 每次 API 调用前本地估算 token 数，>95% 上限时触发 emergency fold。

**当前 dscode 状态**: 我们只在 compaction 决策中使用 token 估计，不在每次 API 调用前做预判。

**迁移建议**:
- 在 `turn.rs` 中每次 `build_messages()` 后添加 preflight check
- token 估算用简单的 char/4 近似（或用 Regex 分词近似）
- >95% 时触发 compact 后再发请求

**代码改动预估**: turn.rs +15行

### 2.4 连接健康跟踪（低优先级迁移）

**文件**: `DeepSeek-TUI/crates/tui/src/client.rs` (line 131)

**ConnectionHealth 枚举**: Healthy → Degraded → Recovering
- 2次连续失败 → Degraded
- Degraded 后 15s 探测一次恢复

**当前 dscode 状态**: 我们有 `send_with_retry()` 但没有连接健康跟踪。可以增强：
- 健康态时普通重试
- 降级态时增加探测间隔、减少并发、缩短超时

**代码改动预估**: llm/client.rs +20行

---

## 三、记忆/上下文架构

### 3.1 三部曲内存模型 (关键迁移)

**文件**: `DeepSeek-Reasonix/src/memory/runtime.ts`

```
ImmutablePrefix  → system prompt + tool specs + few-shots (SHA-256 指纹)
AppendOnlyLog    → 消息序列 (严格 append-only, 仅 compact 时改写)
VolatileScratch  → 每轮临时状态 (thinking content, plan state, notes)
```

**当前 dscode 状态**: 我们只有 `ConversationStore` (JSONL) + `AgentSharedContext`。没有 ImmutablePrefix 的概念，也没有每轮清除的 VolatileScratch。

**迁移建议**:
1. 提取不可变部分到 `ImmutablePrefix`:
   - system prompt text
   - tool definitions (JSON schemas)
   - 所有一旦确定就不变的内容
   - SHA-256 指纹化 → 可在请求对比时验证缓存稳定性
2. 每轮清理临时状态:
   - thinking content 不跨轮传递
   - 临时工具结果已在 summary 后移除
3. `AppendOnlyLog` 本质是当前 `ConversationStore`，但需要更严格的 append-only 约束

**价值**: 这是 Reasonix 实现 99.82% cache hit rate 的核心。Prefix-cache 对齐需要 immutable prefix + append-only log 的分离。

**代码改动预估**: 新建 `src/session/prefix.rs` ~80行 + memory/runtime.rs ~60行

### 3.2 周期管理器 (Cycle Manager) 替代压缩

**文件**: `DeepSeek-TUI/crates/tui/src/cycle_manager.rs` (1074行)

**机制**: 在 768K tokens (~75%) 时不压缩历史，而是：
1. 归档当前 cycle 到 JSONL
2. 保留 system prompt + structured state (todos, plan, working set)
3. 模型生成 ~3K token briefing
4. 以干净上下文重启

**对比我们的 compaction**: 我们是 DP_BETA 决策 → summary call → in-place 替换。Cycle manager 是更彻底的重启，但需要更复杂的结构化状态提取。

**评价**: 对 dscode 来说太重了（1074行 vs 我们的 217行 compaction.rs）。但 "briefing 而非 summary" 的思路可借鉴：summary 调用时告知模型"为新对话生成 briefing 而非替换消息列表"，让模型自主选择最有价值的信息。

---

## 四、修复流水线 (Repair Pipeline)

### 4.1 四层修复 (高优先级迁移)

**文件**: `DeepSeek-Reasonix/src/repair/` (5个文件)

```
scavenge  → 从文本响应中提取 tool_calls（修复 JSON 格式错误）
truncation → token 级截断修复（过长参数截断）
flatten   → 扁平化修复（deep schema 自动 flatten）
storm     → 重复调用抑制
```

**当前 dscode 的状态**: 我们的 repair 非常简单——SSE parser 做基本解析，失败了就报错。没有多层修复 pipeline。

**迁移建议**: 采纳 scavenge + flatten：
1. **Scavenger**: 当 JSON 解析失败时，用 Regex 尝试从文本中提取 tool_call（处理 LLM 输出不在 JSON 格式中的情况）
2. **Flattener**: 自动展开嵌套 dot-notation 参数（处理 LLM 输出 `tool.name.sub` 而非 `tool: {name: "name.sub"}`）

**代码改动预估**: 新建 `src/repair/` (~100行)

---

## 五、错误处理 & 退化策略

### 5.1 错误分类学 (Error Taxonomy) (高优先级迁移)

**文件**: `DeepSeek-TUI/crates/tui/src/error_taxonomy.rs` (734行)

```
ErrorCategory: Network | Authentication | Authorization | RateLimit | Timeout | InvalidInput | Parse | Tool | State | Internal
ErrorSeverity: Info | Warning | Error | Critical
ErrorEnvelope { category, severity, recoverable, code, message }
```

**当前 dscode 的状态**: 我们用 `anyhow::Result` + 字符串 error，没有结构化分类。

**迁移建议**: 引入轻量版错误分类：
```rust
enum ErrorCategory { Network, Auth, RateLimit, Parse, Tool, Internal }
enum ErrorSeverity { Warning, Error, Fatal }
struct ErrorInfo { category, severity, recoverable: bool }
```
不采用完整 Envelope（太重），但至少做分类 + 可恢复判断。

**价值**: 这是 auto-mode 信号系统的基础——不同类别的失败对升级决策权重不同（Auth 错误不应触发模型升级，RateLimit 应等待而非升级）。

**代码改动预估**: 新建 `src/errors.rs` ~60行

### 5.2 流重试分层 (中等优先级迁移)

**文件**: `DeepSeek-TUI/crates/tui/src/core/engine/turn_loop.rs` (lines 694-727) + `streaming.rs` (lines 80-87)

**三层重试**:
1. **Intra-stream**: 连续 decode error < 5 → 继续
2. **Transparent retry**: 流中未收到任何内容 → 静默重发请求 (最多 2 次)
3. **Post-stream retry**: 流完全死亡 → 重新发送整轮请求 (最多 3 次)

**当前 dscode 的状态**: 只有 `send_with_retry()` HTTP 重试，没有流级重试。

**迁移建议**: 采纳 intra-stream 和 transparent retry。Post-stream 太激进（bill 可能翻倍），我们的 context 较小，不需要。

**代码改动预估**: llm/client.rs +30行

### 5.3 RAII 交互终端守卫 (低优先级迁移)

**文件**: `DeepSeek-TUI/crates/tui/src/core/engine/tool_execution.rs` (lines 35-120)

`InteractiveTerminalGuard`: Drop 时自动 ResumeEvents，即使用户 Ctrl+C 也不丢失 TUI 状态。

**迁移建议**: 我们的 sync TerminalDisplay 不需要（没有 Pause/Resume 概念）。但未来 TUI 时需要。

---

## 六、测试架构

### 6.1 Mock LLM Client 模式 (高优先级迁移)

**文件**: `DeepSeek-TUI/crates/tui/src/llm_client/mock.rs` + `tests/integration_mock_llm.rs` (617行)

**坑**: 引擎持有 `Option<DeepSeekClient>` 具体类型，无法注入 mock。测试标记 `#[ignore]`。

**dscode 的对应**: 我们的 `AsyncLlClient` 也是具体类型。如果要做 mock 测试，需要提取 `LlmClient` trait + 泛型/Arc<dyn>。

**迁移建议**: 现在就做！提取 `trait LlmClient` → `AsyncLlClient: LlmClient` → 编写 `MockLlmClient`。
这不是特性开发，是架构债务——让 turn loop 和 sub executor 可测试。

**代码改动预估**: llm/client.rs → 提取 trait + 实现 ~30行，新建 `src/llm/mock.rs` ~80行

### 6.2 架构不变量测试 (低优先级迁移)

**文件**: `DeepSeek-Reasonix/tests/architecture-invariants.test.ts`

测试内容:
- 指纹确定性（相同输入 → 相同 SHA-256 前缀）
- Reducer 确定性（相同输入 → 字节相同输出）
- Append-only 跨轮边界

**迁移建议**: 等我们实现 ImmutablePrefix 后再加。目前 conversation store 没有需要验证的不变量。

### 6.3 FakeFetch 模式 (低优先级迁移)

**文件**: `DeepSeek-Reasonix/tests/loop.test.ts` (lines 19-49)

`fakeFetch(responses): Response[]` → returns from array in order → captures calls via `push`.

**迁移建议**: 这是 mock HTTP 客户端的 JS 惯用写法。Rust 中我们通过 `LlmClient` trait mock 实现。

---

## 七、DevOps/运维模式

### 7.1 Panic Hook 崩溃快照 (`DeepSeek-TUI`)

**文件**: `DeepSeek-TUI/crates/tui/src/main.rs` (lines 587-637)

```rust
std::panic::set_hook(Box::new(|info| {
  // 恢复终端 → 写入 ~/.deepseek/crashes/ → 打印恢复建议
}))
```

**迁移建议**: dscode 运行 `agent.sh`/`dscode` 无需复杂崩溃恢复（单进程，无 TUI 状态）。
但可以加一个简单的 `panic::set_hook` 打印 `RUST_BACKTRACE` 提示。

### 7.2 会话检查点 (`DeepSeek-TUI`)

**文件**: `DeepSeek-TUI/crates/tui/src/session_manager.rs` (1770行)

- checkpoint.json 保存最后状态
- `--resume` 恢复之前的 checkpoint
- `--continue` 从中断点恢复

**当前 dscode 的状态**: 我们有 JSONL events.jsonl + conversation.jsonl，已经有完整日志。但没有显式 checkpoint 概念。

**迁移建议**: 在每次 turn 开始前保存一个 `~/.dscode/checkpoints/latest.json` 快照，包含:
- messages slice/len
- turn count
- stats summary
崩溃后可用 `--resume` 或 `--continue 日志文件` 恢复。

---

## 八、汇总：迁移优先级矩阵

| # | 模式 | 来源 | 价值 | 复杂度 | 行数 | 优先级 |
|---|------|------|------|--------|------|--------|
| 1 | 多信号融合升级 (repair.fire + parse error) | Reasonix | 高 | 低 | +20 | **P0** |
| 2 | 自报告升级 `<<<NEEDS_PRO>>>` | Reasonix | 中 | 低 | +25 | P1 |
| 3 | 三级压缩阈值 (50/70/80/95%) | Reasonix | 高 | 低 | +30 | **P0** |
| 4 | Storm Breaker 重复调用抑制 | Reasonix | 中 | 中 | +60 | P1 |
| 5 | Preflight 预判门 | Reasonix | 中 | 低 | +15 | P1 |
| 6 | 三部曲内存模型 (ImmutablePrefix) | Reasonix | 高 | 中 | +140 | **P0** |
| 7 | Scavenger + Flattener 修复流水线 | Reasonix | 中 | 中 | +100 | P1 |
| 8 | 错误分类学 (ErrorCategory/Severity) | TUI | 高 | 低 | +60 | **P0** |
| 9 | 流内重试 (intra-stream + transparent) | TUI | 中 | 低 | +30 | P1 |
| 10 | LlmClient trait 提取 + Mock | TUI | 高 | 中 | +110 | **P0** |
| 11 | 周期管理器替代压缩 | TUI | 低 | 高 | +500+ | Dropped |
| 12 | 启发式 + Flash 路由 | TUI | 低 | 低 | +30 | Dropped |
| 13 | 连接健康跟踪 | TUI | 低 | 低 | +20 | P2 |
| 14 | Panic hook 崩溃快照 | TUI | 低 | 低 | +15 | P2 |
| 15 | 会话检查点 | TUI | 中 | 中 | +80 | P1 |
| 16 | RAII 交互终端守卫 | TUI | 低 | 低 | +30 | P2 |
| 17 | 架构不变量测试 | Reasonix | 低 | 低 | +40 | P2 |
| 18 | Feedforward 控制信号注入 | Reasonix | 中 | 低 | +20 | P1 |

**P0 合并 (5项, ~360行)**: 多信号融合 / 三级压缩 / 三部曲内存 / 错误分类学 / LlmClient trait
**P1 合并 (7项, ~330行)**: 自报告升级 / Storm Breaker / Preflight / Repair / 流重试 / Checkpoint / Feedforward
**总合计**: ~690行 (远小于 TUI 单体 50000+行，符合 dscode 轻量定位)

---

## 九、关键洞察

### 设计哲学对比

| 维度 | Reasonix | TUI | dscode |
|------|----------|-----|------------|
| 语言 | TypeScript 38模块 | Rust 14 crates | Rust 36文件 |
| 行数 | ~20000 | ~50000+ | ~5650 |
| 模型 | DeepSeek only | DeepSeek only | Multi-provider |
| 哲学 | "减" — flash-first, subagents=cost tool, removed branch/harvest | "全" — 13 providers, 50+ features, cycle/compact/capacity 三套上下文管理 | "精" — minimal, correct, testable |
| 测试 | 201 Vitest tests | in-file unit tests + 617行 integration (ignored) | 74 unit tests, 0 failures |

### 关键对抗: "减" vs "全"

Reasonix 主动移除不必要功能（branch/harvest, tools system prompt section, multi-provider）。TUI 则呈现功能加法倾向（13 providers, cycle manager + compaction + capacity controller 三套上下文管理共存）。

dscode 应学 Reasonix 的**减原则**：
- 不重复造轮子（单压缩方案，不加 cycle manager）
- 信号驱动优于启发式（auto-model 基于错误信号而非关键词匹配）
- 缓存对齐优先（三部曲内存模型的核心价值是 prefix-cache hit rate，不是模块化）

### 关键采纳: 三层架构的缺失

dscode 缺少 Reasonix 的三部曲内存分层。当前 `ConversationStore` 把所有消息混在一起。引入 `ImmutablePrefix` 是我们能做的**最大单次 cache-hit 优化**——直接对标 Reasonix 的 99.82% 命中率。

### 关键放弃: TUI 的过度工程化

TUI 的 cycle manager (1074行) 和 capacity controller (976行) 是为巨型上下文窗口（1M tokens）设计的。dscode 的典型上下文小得多（~50K tokens），不需要这么复杂的容量管理。简单三级压缩阈值就足够了。
