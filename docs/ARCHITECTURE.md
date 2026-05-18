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
- **减原则** — 每个功能追问"去掉会怎样"

## 运行时分层

```
                    main.rs
         parse_args → apply_provider_defaults
         → create_session → new_orchestrator
               │
       ┌───────┴────────┐
       ▼                ▼
  Interactive       Single/Stdin
  (rustyline REPL)  (prompt|pipe)
       │                │
       └───────┬────────┘
               ▼
        ┌──────────────────┐
        │   OrchActor           │
        │  ┌──────────────────┐│
        │  │ Controller       ││
        │  │ P(stall) Bayesian││
        │  │ stall detection  ││
        │  └──────────────────┘│
        │  ┌──────────────────┐│
        │  │ ModelSelector    ││
        │  │ Beta-Bernoulli   ││
        │  │ flash quality    ││
        │  └──────────────────┘│
        │  dispatches via      │
        │  TurnExecutor     │
        └────────┬─────────┘
                 │
        ┌────────┴─────────┐
        │  TurnExecutor     │
        │                   │
        │  1. store.add_user│
        │  2. ensure_prefix │
        │     (Immutable)   │
        │  3. compact check │
        │     (same-turn    │
        │      guard)       │
        │  4. LLM stream    │
        │  5. Scavenge      │
        │     (DSML/JSON    │
        │      recovery)    │
        │  6. Persist       │
        │  7. ToolRunner    │
        │     (Truncation + │
        │      StormBreaker)│
        │  8. Decide        │
        └──────────────────┘
```

## 模块职责

| 模块 | 文件 | 职责 |
|------|------|------|
| **main** | `main.rs` | CLI 参数解析 → 创建 Session → 启动 Orchestrator |
| **config** | `config.rs` | 配置结构体、CLI 参数解析、环境变量合并 |
| **context** | `context.rs` | AgentSharedContext：全局共享状态聚合 |
| **agent/orchestrator** | `agent/orchestrator.rs` | 主循环：接收用户输入/子代理结果，管理状态 |
| **agent/turn** | `agent/turn.rs` | 单轮执行器：流式 LLM → 工具执行 → 决策 |
| **agent/sub_pool** | `agent/sub_pool.rs` | 子代理并发池，Semaphore 限流 |
| **agent/sub_executor** | `agent/sub_executor.rs` | 子代理独立上下文执行 |
| **agent/failure_tracker** | `agent/failure_tracker.rs` | 多信号升级跟踪器 |
| **session/store** | `session/store.rs` | ConversationStore：JSONL 持久化 |
| **session/stats** | `session/stats.rs` | Token 用量统计，持久化 |
| **session/compaction** | `session/compaction.rs` | 上下文压缩引擎 |
| **session/prefix** | `session/prefix.rs` | ImmutablePrefix：缓存稳定性保障 |
| **session/paths** | `session/paths.rs` | Session 路径计算 |
| **compact_dp** | `compact_dp.rs` | CompactionTier 枚举 + turn 对齐截断 |
| **llm/client** | `llm/client.rs` | Async HTTP client + 重试 + SSE 流传输 |
| **llm/transport** | `llm/transport.rs` | OpenAI-compatible API 请求构造 |
| **sse/openai** | `sse/openai.rs` | SSE 流解析：增量 text/thinking/tool_calls |
| **tools/runner** | `tools/runner.rs` | 工具分发器 + StormBreaker + Truncation 修复 |
| **guard/storm** | `guard/storm.rs` | StormBreaker：重复调用滑动窗口检测 |
| **repair/scavenge** | `repair/scavenge.rs` | DSML/XML/JSON 回收 + 3-shape coerce + 截断修复 |
| **repair/flatten** | `repair/flatten.rs` | Dot-notation 参数扁平化 |
| **errors** | `errors.rs` | 错误分类学：Parse/Tool/Network/Auth/RateLimit |
| **protocol** | `protocol.rs` | Event enum：Text/Thinking/ToolCall/Usage/Stop/Error |
| **prompt** | `prompt.rs` | System prompt 构建器 |

## 核心数据流

### 单轮执行

```
用户输入
  │
  ▼
TurnExecutor::execute()
  │
  ├── reset_storm()                 ← 新意图，清窗口
  ├── store.add_user(input)
  ├── ensure_prefix()               ← 构建/复用 ImmutablePrefix
  ├── while turn < max_turns:
  │   ├── compact check             ← 自适应三级压缩
  │   │   └── compacted_this_turn guard
  │   ├── preflight check           ← >95% 紧急压缩
  │   ├── LLM stream (SSE parse)
  │   │   ├── Event::Thinking       ← reasoning_content
  │   │   ├── Event::Text           ← 最终回复
  │   │   ├── Event::ToolCall       ← 工具调用
  │   │   ├── Event::Usage          ← token 用量
  │   │   └── Event::Stop           ← stop_reason
  │   ├── Scavenge                  ← 从 thinking+text 回收工具调用
  │   ├── store.add_assistant()     ← 持久化
  │   ├── ToolRunner::execute_all()
  │   │   ├── StormBreaker.check()  ← 重复调用抑制
  │   │   ├── Truncation repair    ← 截断 JSON 修复
  │   │   └── execute_one_sync()   ← 实际执行
  │   ├── store.add_tool_results()
  │   └── Decide (continue/stop)
  │
  └── 返回 TurnDecision + TurnEffect
```

### 上下文压缩

```
should_compact() 检查 context_tokens / max_context_tokens ≥ compact_pct
  │
  ├── CompactionTier::from_ratio()
  │   ├── Conservative  (<70%)   keep=20%
  │   ├── Aggressive    (70-80%)  keep=10%
  │   ├── ForceSummary  (80-95%)  keep=5%   ← 默认首次触发点
  │   └── Emergency     (≥95%)    keep=1-5 lines
  │
  ├── compact_turn_keep()         ← turn 对齐截断
  ├── 最小收益检查 (≥10%)
  ├── run_summary_call()          ← LLM 生成摘要
  ├── store.trim_keep_last()
  └── invalidate_prefix()         ← 下次重建
```

### 维修流水线

```
LLM 流式响应
  │
  ├── Phase 1: Scavenge (turn.rs)
  │   ├── DSML 格式解析: <|DSML|invoke name="...">
  │   ├── XML 包装: <tool_call>{...}</tool_call>
  │   ├── Bracket 包装: [TOOL_CALL]{...}[/TOOL_CALL]
  │   ├── 裸 JSON: {name:..., arguments:...}
  │   └── 3-shape coerce: OpenAI / tool_name / 标准
  │
  ├── Phase 2: Truncation Repair (runner.rs)
  │   ├── 补全未闭合的字符串/括号
  │   ├── 去掉尾逗号
  │   └── 填 null 到悬挂 key
  │
  └── Phase 3: Storm Breaker (runner.rs)
      ├── 滑动窗口 (size=6, threshold=3)
      ├── Mutating 调用清空只读窗口
      └── StormExempt 工具跳过检查
```

## Session 结构

```
~/.dscode/
└── projects/<project_key>/
    └── <session_id>/
        ├── conversation.jsonl    ← 对话消息（JSONL 逐行追加）
        ├── events.jsonl          ← 事件日志
        ├── summary.txt           ← 压缩后的上下文快照
        ├── plan.md               ← 确认后的计划
        ├── plan.draft            ← 草稿计划
        └── stats.json            ← Token 用量统计
```

### 持久化策略

- 写入使用 `OpenOptions::append`，逐行追加
- `trim_keep_last()` 重写文件（仅压缩路径）
- 内存缓存避免重复读盘
- Append 更新缓存，trim 重建缓存

## 配置优先级

```
CLI 参数 > 环境变量 > 代码默认值
```

所有配置合并发生在 `config.rs` 的 `apply_provider_defaults()` 函数中。

## 模型选择决策

reslove_active() 决定当前轮次使用 flash 还是 pro：

```
① forced_model?      → 手动指定 (/flash, /pro)
② !auto_model_enabled → config.model 中的值
③ is_locked()?        → Pro (短期停滞, P(stall)>0.80)
④ flash Q<0.50, N≥8?  → Pro (长期质量不足)
⑤ 默认                → Flash
```

### 短期停滞 (Controller)

贝叶斯 `P(stall) = 1 - 0.5^k`：

- k = 连续无进展轮数
- 成功（Stop）时 k=0，失败时 k+=1
- P > 0.80 自动锁定 Pro，P < 0.80 解锁

### 长期质量 (ModelSelector)

Beta-Bernoulli 追踪 flash 的成功率：

- Q = α/(α+β)
- N = α+β-2 (观测次数)
- Q < 0.50 且 N ≥ 8 → flash 被证明不够好 → 升级 Pro
- /flash 手动降级时重置为 Beta(3,3)

### 信号源

| 信号 | 来源 | 权重 |
|------|------|:----:|
| Tool 执行失败 | error.sh 传感器 (70+ 模式) | 1.0/0.8/0.5 |
| NEEDS_PRO 自报告 | `<<<NEEDS_PRO>>>` 标记 | — |

所有错误信号聚合后喂入 Controller 的 `note_error()`。
