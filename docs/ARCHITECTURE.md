# 架构说明

## 项目定位

dscode 是一个 Rust 实现的轻量 agent 内核，专为 DeepSeek 优化。目标：
- 可在终端独立运行
- 可被其他程序嵌入和编排的单二进制
- Session 为一等公民，JSONL 持久化
- Cache-Aligned 上下文压缩，最大化 DeepSeek prefix-cache 命中率

## 核心原则

- **单 agent、单进程主循环**
- **机器协议优先** — stream-json 输出结构化事件
- **Session 为一等公民** — 持久化、恢复、重放
- **Context budget 是硬约束** — 自适应三级压缩保持上下文在窗口内
- **工具边界可预测** — 每个工具的执行时间、输出大小、副作用明确
- **信念驱动干预** — 工具执行质量通过拉普拉斯平滑计算信念度 B ∈ [0,1]，低信念时自动注入提示词或中止

## 运行时分层

```
                    main.rs
         parse_args → apply_provider_defaults
         → init_session → new_orchestrator
               │
       ┌───────┴────────┐
       ▼                ▼
  Interactive       Single/Stdin
  (rustyline REPL)  (prompt|pipe)
       │                │
       └───────┬────────┘
               ▼
        ┌──────────────────────┐
        │   OrchActor          │
        │  ┌──────────────────┐│
        │  │ SignalCollector  ││
        │  │ → ToolFailed    ││
        │  │ → ToolError      ││
        │  │ → EditLoop       ││
        │  └──────────────────┘│
        │  ┌──────────────────┐│
        │  │ BeliefTracker    ││
        │  │ Laplace + window ││
        │  └──────────────────┘│
        │  ┌──────────────────┐│
        │  │ DecisionEngine   ││
        │  │ → Inject/Abort   ││
        │  └──────────────────┘│
        │  dispatches via      │
        │  TurnExecutor        │
        └────────┬─────────────┘
                 │
        ┌────────┴─────────────┐
        │  TurnExecutor        │
        │                      │
        │  1. store.add_user   │
        │  2. ensure_prefix    │
        │     (Immutable)      │
        │  3. compact check    │
        │     (same-turn       │
        │      guard)          │
        │  4. LLM stream       │
        │  5. Scavenge         │
        │     (DSML/JSON       │
        │      recovery)       │
        │  6. Persist          │
        │  7. ToolRunner       │
        │     (Truncation +    │
        │      StormBreaker)   │
        │     → SignalCollect  │
        │     → BeliefTracker  │
        │  8. Decide           │
        └──────────────────────┘
```

## 模块职责

| 模块 | 文件 | 职责 |
|------|------|------|
| **main** | `main.rs` | CLI 参数解析 → Session → 启动 Orchestrator |
| **config** | `config.rs` | 配置结构体、CLI 参数、环境变量合并 |
| **context** | `context.rs` | AgentSharedContext（全局） + ToolContext（工具层） |
| **agent/orchestrator** | `agent/orchestrator.rs` | 主循环，持有 BeliefTracker + DecisionEngine |
| **agent/turn** | `agent/turn.rs` | 单轮执行器：LLM 流 → 工具 → 决策 |
| **agent/belief** | `agent/belief.rs` | BeliefTracker：拉普拉斯平滑 + 滑动窗口 |
| **agent/decision** | `agent/decision.rs` | DecisionEngine：阈值判断 → Inject/Abort |
| **agent/sub_pool** | `agent/sub_pool.rs` | 子代理并发池，Semaphore 限流 |
| **agent/sub_executor** | `agent/sub_executor.rs` | 子代理独立上下文执行 |
| **guard/collector** | `guard/collector.rs` | SignalCollector：ToolFailed/ToolError/EditLoop |
| **guard/storm** | `guard/storm.rs` | StormBreaker：重复调用抑制 |
| **session/store** | `session/store.rs` | ConversationStore：JSONL 持久化 |
| **session/stats** | `session/stats.rs` | Token 用量统计 |
| **session/compaction** | `session/compaction.rs` | 上下文压缩引擎 + CompactionTier |
| **session/prefix** | `session/prefix.rs` | ImmutablePrefix：缓存稳定性保障 |
| **session/paths** | `session/paths.rs` | Session 路径计算 |
| **session/init** | `session/init.rs` | 共享会话初始化（main + sub 共用） |
| **llm/client** | `llm/client.rs` | HTTP 客户端 + 重试 + SSE 流 |
| **llm/transport** | `llm/transport.rs` | OpenAI API 请求构造 |
| **sse/openai** | `sse/openai.rs` | SSE 流解析 |
| **tools/runner** | `tools/runner.rs` | 工具分发器 + StormBreaker + Truncation |
| **repair/scavenge** | `repair/scavenge.rs` | DSML/XML/JSON 回收 + 截断修复 |
| **errors** | `errors.rs` | 错误分类 |
| **protocol** | `protocol.rs` | Event enum |
| **prompt** | `prompt.rs` | System prompt 构建器 |
| **ui/engine** | `ui/engine.rs` | 终端渲染 + 标题栏 |
| **ui/replay** | `ui/replay.rs` | Session 重放 |

## 核心数据流 — 信号链路

```
工具执行完毕 (tools/runner.rs)
     │
SignalCollector.collect(name, output)
     ├── ToolFailed — 确定性失败 (exit_code≠0 / "Error:"前缀, 统一 1.0)
     ├── ToolError   — regex 匹配 (severity 0.3~0.9)
     └── EditLoop    — 序列检测 (W=6, severity 0.4~0.9)
     │
     ▼
TurnExecutor 每工具调用后调用 belief.observe(signals)
     ├── Observation.from_signals: 多信号取 max(severity)
     ├── 滑动窗口 (默认 16): 满则 pop 最旧
     └── α = 1 + Σ success, β = 1 + Σ failure
     │
     ▼
BeliefTracker.belief() = α / (α + β) ∈ [0, 1]
     │
     ▼
DecisionEngine.decide(B, errors)
     ├── B ≥ 0.7 → None
     ├── 0.3 ≤ B < 0.7 → Inject(含具体错误详情)
     └── B < 0.3 → Abort
```

### 信念度语义

| B 值 | 含义 |
|------|------|
| 0.75 | 初始状态（信任先验 α=3） |
| > 0.7 | 🟢 顺利 |
| < 0.5 | 🟡 偶有错误 |
| < 0.3 | 🔴 严重，需中止 |

### 提示词注入（任务循环内）

注入发生在 `turn.rs::execute()` 的循环内，工具执行完成后、下一轮 LLM 调用之前：

```
Phase 3: 工具执行 → 信号 → BeliefTracker.observe()
Phase 4: stop = "tool_use"
  ├─ DecisionEngine.decide(belief, errors)
  │   ├─ Inject → store.add_user("[System note: ...]")  ← 写入对话存储
  │   └─ Abort  → 返回 Failed，中断本轮
  └─ continue → 下一轮 LLM: messages = store.lines()（包含注入消息）
```

注入消息作为一条独立的 User 消息写入对话存储，LLM 在下一轮调用时自然看到。不修改 system prompt（保护前缀缓存），也不追加到用户输入末尾。

## 核心数据流 — 单轮执行

```
用户输入
  │
  ▼
OrchActor.handle_user_input()
  ├── maybe_inject()           ← 检查上一轮信念，决定注入/中止
  ├── belief.reset()           ← 新轮开始
  ├── prepare_turn()           ← 解析模型，创建 LLM 客户端
  │
  ▼
TurnExecutor::execute(belief)
  │
  ├── reset_storm()
  ├── store.add_user(input)
  ├── ensure_prefix()
  ├── while turn < max_turns:
  │   ├── compact check
  │   ├── preflight emergency
  │   ├── LLM stream
  │   │   ├── Thinking/Text
  │   │   ├── ToolCall
  │   │   └── Stop (tool_use/end_turn)
  │   ├── Scavenge
  │   ├── store.add_assistant()
  │   ├── ToolRunner::execute_all()
  │   │   ├── StormBreaker
  │   │   ├── Truncation repair
  │   │   └── execute_one
  │   ├── 每工具调用后:
  │   │   ├── SignalCollect.collect(name, output)
  │   │   └── belief.observe(signals)   ← 实时更新信念
  │   ├── store.add_tool_results()
  │   └── Decide (continue/stop)
  │
  └── 返回 TurnDecision + TurnEffect
```

## 上下文压缩

```
should_compact() 检查 context_tokens / max_context_tokens ≥ compact_pct
  │
  ├── CompactionTier::from_ratio()
  │   ├── Conservative  (<70%)   keep=20%
  │   ├── Aggressive    (70-80%)  keep=10%
  │   ├── ForceSummary  (80-95%)  keep=5%
  │   └── Emergency     (≥95%)    keep=1-5 lines
  │
  ├── compact_turn_keep()         ← turn 对齐截断
  ├── run_summary_call()          ← LLM 生成摘要
  ├── store.trim_keep_last()
  └── invalidate_prefix()
```

## Session 结构

```
~/.dscode/projects/<project_key>/<session_id>/
├── conversation.jsonl    ← 对话消息（JSONL 逐行追加）
├── events.jsonl          ← 事件日志
├── summary.txt           ← 压缩后的上下文快照
├── plan.md / plan.draft  ← 计划文件
└── stats.json            ← Token 用量统计
```

## 模型切换

手动切换（无自动模型选择）：

```
/flash — 切回 flash（重置信念）
/pro   — 强制 pro
```

## 配置优先级

```
CLI 参数 > 环境变量 > 代码默认值
```

关键环境变量：`DEEPSEEK_API_KEY`, `DEEPSEEK_BASE_URL`, `TOOL_RESULT_MAX_BYTES`, `FILE_WRITE_MAX_BYTES`, `CONTEXT_COMPACT_PCT`, `LOG_EVENTS`
