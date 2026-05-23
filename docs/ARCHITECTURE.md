# 架构说明

## 项目定位

dscode 是一个 Rust 实现的轻量 AI coding agent，专为 DeepSeek 优化。单二进制，零运行时依赖。

目标：
- 可在终端独立运行，也可嵌入其他程序
- Session 为一等公民，JSONL 持久化，支持恢复和重放
- Cache-Aligned 上下文压缩，最大化 DeepSeek prefix-cache 命中率

---

## 核心原则

- **单 agent、单进程主循环** — 无分布式依赖，无守护进程
- **机器协议优先** — `--print` 模式输出 ndjson 事件流，方便嵌入和编排
- **Session 持久化** — JSONL 格式，天然追加友好，崩溃安全
- **Context budget 是硬约束** — 自适应三级压缩，持续保持上下文在窗口内
- **工具边界可预测** — 每个工具的执行时间、输出大小、副作用明确
- **信念驱动干预** — 工具执行质量通过贝叶斯推断计算信念度，低信念时自动干预

---

## 运行时分层

```
main.rs
  │  CLI 参数解析 → 配置合并（CLI > .dscoderc > 环境变量 > 默认值）
  │  → Session 初始化 → 启动 Orchestrator
  ▼
┌────── Orchestrator 层 ──────────────────┐
│ agent/orchestrator.rs                   │
│  主循环：持有全局上下文、信念追踪器、   │
│  决策引擎，调度 TurnExecutor 执行每轮   │
└─────────────────────────────────────────┘
         │
┌────── Turn 执行层 ──────────────────────┐
│ agent/turn.rs                           │
│  1. 上下文压缩检查（同轮最多一次）      │
│  2. LLM 流式请求（通过 LLM 层）         │
│  3. Scavenge 回收（修复遗漏调用）       │
│  4. 持久化 assistant 消息               │
│  5. 工具执行（通过工具层）              │
│  6. 决策（继续/停止/中止）              │
└─────────────────────────────────────────┘
         │
┌────── LLM 通信层 ───────────────────────┐
│ llm/client.rs       HTTP 客户端 + 重试  │
│ llm/transport.rs    API 请求体构造      │
│ sse/openai.rs       SSE 增量解析        │
└─────────────────────────────────────────┘
         │
┌────── 工具执行层 ───────────────────────┐
│ tools/runner.rs     分发 + 截断修复     │
│ tools/file.rs       Read/Write/Edit     │
│ tools/bash.rs       Bash 执行           │
│ tools/web.rs        WebSearch/Fetch     │
│ tools/search.rs     Grep/Glob           │
└─────────────────────────────────────────┘
         │
┌────── 信号与防护层 ─────────────────────┐
│ guard/collector.rs  信号采集            │
│ guard/storm.rs      重复调用抑制        │
│ agent/belief.rs     信念度计算          │
│ agent/decision.rs   决策引擎            │
│ safety.rs           命令安全过滤        │
└─────────────────────────────────────────┘
         │
┌────── 持久化层 ─────────────────────────┐
│ session/store.rs    JSONL 存储          │
│ session/stats.rs    Token 统计          │
│ session/compaction  三级上下文压缩      │
│ session/prefix      缓存管理            │
└─────────────────────────────────────────┘
         │
┌────── 终端 UI 层 ───────────────────────┐
│ ui/mod.rs     Display trait + 统计结构  │
│ ui/engine.rs  TerminalDisplay（REPL）   │
│ tui/mod.rs    TuiDisplay + TUI 框架     │
│ ui/replay.rs  Session 重放渲染          │
└─────────────────────────────────────────┘
```

---

## 模块职责

### 入口与基础

| 文件 | 职责 |
|------|------|
| `main.rs` | CLI 入口：参数解析 → 配置合并 → Session 初始化 → 启动 Orchestrator 或 TUI |
| `config.rs` | Config 结构体、parse_args()、apply_config_file()、apply_provider_defaults()、size 解析 |
| `context.rs` | AgentSharedContext（全局共享状态）+ ToolContext（工具执行上下文） |
| `assets.rs` | 嵌入的 tools.json 定义、内置 skill 列表 |
| `cancel.rs` | CancellationToken 父子传播 |
| `safety.rs` | 危险命令黑名单（rm -rf /、sudo、shutdown 等） |
| `util.rs` | 通用工具函数（truncate_str 等） |
| `errors.rs` | ErrorCategory 分类（Network/Auth/RateLimit/Parse/Tool/Internal） |
| `protocol.rs` | Event enum 定义 |
| `sandbox/` | 沙箱自举模块：平台分发（Linux nsjail/bwrap + macOS sandbox-exec）

### Agent 核心

| 文件 | 职责 |
|------|------|
| `agent/orchestrator.rs` | 主循环：接收用户输入 → 创建 TurnExecutor → 处理 TurnEffect（子代理/计划变更） |
| `agent/turn.rs` | 单轮执行器：LLM 流 → 工具 → 决策，内循环（tool_use 循环） |
| `agent/belief.rs` | BeliefTracker：信号合并、拉普拉斯平滑、滑动窗口 |
| `agent/decision.rs` | DecisionEngine：阈值判断、注入格式化、冷却计数器管理 |
| `agent/sub_pool.rs` | 子代理并发池（Semaphore 限流） |
| `agent/sub_executor.rs` | 子代理独立上下文创建、fork 模式、结果收集 |

### LLM 通信

| 文件 | 职责 |
|------|------|
| `llm/client.rs` | HTTP 流式客户端 + 指数退避重试 + 模型名解析 |
| `llm/transport.rs` | OpenAI chat/completions 请求体构造（含缓存控制标记） |
| `llm/mock.rs` | Mock LLM 客户端（测试用） |
| `sse/openai.rs` | SSE 增量解析器：跨 chunk 合并 tool_call、提取 thinking/usage/stop_reason |
| `sse/toolcall.rs` | SSE 中 tool_call 字段提取 |

### 工具系统

| 文件 | 职责 |
|------|------|
| `tools/runner.rs` | 批量分发：StormBreaker 检查 → Truncation 修复 → execute_one_sync |
| `tools/file.rs` | Read（offset/limit）、Write、Edit（diff 格式）、Glob、Grep |
| `tools/bash.rs` | Bash 命令执行：超时控制、输出截断、ANSI 过滤、安全校验 |
| `tools/web.rs` | WebSearch（Tavily API）+ WebFetch（HTTP GET） |
| `tools/search.rs` | 搜索工具辅助函数 |

### 信号与防护

| 文件 | 职责 |
|------|------|
| `guard/collector.rs` | SignalCollector：exit_code 检测 + regex 错误匹配 + EditLoop 序列检测 |
| `guard/storm.rs` | StormBreaker：滑动窗口 (tool, args) 重复检测与抑制 |

### Session 与持久化

| 文件 | 职责 |
|------|------|
| `session/store.rs` | ConversationStore：JSONL 追加、延迟加载缓存、trim 截断 |
| `session/stats.rs` | Token 用量统计 + 费用估算 + JSON 持久化 |
| `session/compaction.rs` | 三级压缩引擎 + turn 对齐截断 + 摘要生成 |
| `session/prefix.rs` | ImmutablePrefix：system prompt + tools 缓存 + fingerprint 校验 |
| `session/paths.rs` | Session 目录路径计算（project_key 安全转义） |
| `session/init.rs` | 共享 Session 初始化（主进程 + 子代理共用） |

### 终端 UI

| 文件 | 职责 |
|------|------|
| `ui/mod.rs` | Display trait 定义 + StatsSnapshot（信念度/tokens/费用统计结构） |
| `ui/engine.rs` | TerminalDisplay：REPL 模式同步渲染器（stderr 输出 + ANSI 标题栏） |
| `tui/mod.rs` | TuiDisplay + TUI 事件循环：ratatui 全屏界面（状态栏、消息列表、输入区） |
| `ui/replay.rs` | Session 历史事件重放渲染 |

### 维修

| 文件 | 职责 |
|------|------|
| `repair/scavenge.rs` | DSML/XML/JSON/bracket 五种格式工具调用回收 + JSON 截断修复 |
| `prompt.rs` | System prompt 按序构建器（多个 `<section>` 段） |

---

## 核心数据流

### 单轮执行流程

```
用户输入
  │
  ▼
OrchActor.handle_user_input()
  ├── maybe_inject()         ← 检查上一轮信念
  ├── belief.reset()         ← 新轮开始
  ├── prepare_turn()         ← 创建 LLM 客户端
  │
  ▼
TurnExecutor::execute(belief)
  │
  ├── reset_storm()          ← 重置重复检测窗口
  ├── store.add_user(input)  ← 持久化用户消息
  ├── ensure_prefix()        ← 构建/复用 system prompt 缓存
  │
  ├── while turn < max_turns:
  │   ├── 上下文压缩检查（compact_pct ≥ 85%）
  │   ├── Preflight 紧急压缩（>95% 时触发）
  │   ├── LLM 流式请求（SSE → Event）
  │   ├── Scavenge 回收（修复遗漏调用）
  │   ├── store.add_assistant()
  │   ├── ToolRunner::execute_all()
  │   │   ├── StormBreaker 检查
  │   │   ├── Truncation 修复
  │   │   └── 每工具调用: signal → belief
  │   ├── store.add_tool_results()
  │   └── DecisionEngine.decide()
  │       ├── B ≥ 0.70 → continue
  │       ├── B < 0.70 → Inject + 冷却
  │       ├── B < 0.30 → Abort
  │       └── stop == "tool_use" → 继续循环
  │
  └── 返回 TurnDecision
```

### 信号系统流程

```
工具执行完毕 → SignalCollector → BeliefTracker → DecisionEngine
                    │                  │               │
              ToolFailed         拉普拉斯平滑      阈值判断
              ToolError          滑动窗口 W=16    Inject/Abort
              EditLoop           B ∈ [0, 1]      内部冷却 3 轮
```

信号系统完整设计见 [`设计哲学-信号系统.md`](设计哲学-信号系统.md)。

---

## 上下文压缩

```
should_compact() 检查 context_tokens / max_context_tokens ≥ compact_pct (默认 85%)
  │
  ├── CompactionTier::from_ratio()
  │   ├── Conservative  (<70%)   keep=20%   ← 仅通过更低 compact_pct 可达
  │   ├── Aggressive    (70-80%)  keep=10%   ← 仅通过更低 compact_pct 可达
  │   ├── ForceSummary  (80-95%)  keep=5%    ← 默认首次触发区间
  │   └── Emergency     (≥95%)    keep=1-5   ← 紧急压缩
  │
  ├── compact_turn_keep()    ← 按 user 消息边界 turn 对齐截断
  ├── run_summary_call()     ← LLM 生成摘要（写入 summary.txt）
  ├── store.trim_keep_last() ← 保留末尾轮次
  └── invalidate_prefix()    ← 使 system prompt 缓存失效
```

**防护**：同轮最多压缩一次（`compacted_this_turn` 标记）；压缩收益 <10% 时跳过；Preflight 在发送 LLM 请求前做紧急压缩。

---

## Session 结构

```
~/.dscode/projects/<project_key>/<session_id>/
├── conversation.jsonl    ← 对话消息（JSONL 逐行追加）
├── events.jsonl          ← 结构化事件日志
├── summary.txt           ← 压缩后的上下文摘要
├── plan.md / plan.draft  ← 计划文件
└── stats.json            ← Token 用量统计
```

`project_key` 由工作目录路径安全转义生成，确保项目隔离。`--continue` 模式自动选择最近 session。

---

## 配置系统

### 优先级

```
CLI 参数 > 项目 .dscoderc > 用户 ~/.dscoderc > 环境变量 > 代码默认值
```

### 关键环境变量

| 变量 | 说明 |
|------|------|
| `DEEPSEEK_API_KEY` | API 密钥 |
| `DEEPSEEK_BASE_URL` | API 端点（默认 `https://api.deepseek.com/v1`） |
| `TOOL_RESULT_MAX_BYTES` | 工具结果截断阈值（默认 100000） |
| `FILE_WRITE_MAX_BYTES` | 文件写入上限（默认 1048576） |
| `CONTEXT_COMPACT_PCT` | 压缩触发百分比（默认 85） |
| `LOG_EVENTS` | 事件日志开关 |
| `DSCODE_HOME` | 数据目录（默认 `~/.dscode`） |
| `DSCODE_SANDBOXED` | 内部标记，防止沙箱自举无限递归 |
| `DSCODE_LIMITS` | JSON 格式的 `SandboxConfig` 覆盖（最高优先级） |
