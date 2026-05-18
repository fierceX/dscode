# 代码全面分析报告

> 基于 `src/` 当前代码（44 文件，6745 行，102 测试）
> 分析维度：架构设计、功能完备度、过度设计、优化方向、缺失功能

---

## 一、整体架构评分

| 维度 | 评分 | 分析 |
|------|:----:|------|
| 模块化 | A- | 模块边界清晰，但部分模块职责重复 |
| 可测试性 | B+ | 102 测试覆盖核心路径，但工具层无 mock |
| 错误处理 | B | ErrorCategory 完整，但分类路径未统一 |
| 性能 | A | 无瓶颈，spawn_blocking 设计正确 |
| 可扩展性 | B | 添加工具需改 3-4 个文件，未用 trait |
| 配置 | B+ | 环境变量 + CLI 组合清晰 |
| 文档 | B+ | 设计/架构/使用三件套完整 |
| **综合** | **B+** | 核心功能扎实，边缘模块有打磨空间 |

---

## 二、过度设计识别

### 2.1 `src/repair/mod.rs` — ToolCallRepair 未集成（~90 行死代码）

```rust
pub struct ToolCallRepair {
    storm: StormBreaker,
}
```

`ToolCallRepair` 定义了完整的 scavenge→truncation→storm pipeline，但**没有任何运行时路径调用它**。其功能已分散到 `turn.rs`（scavenge）和 `runner.rs`（truncation + storm）。保留为"未来 sub-agent 使用"的预测性设计。

**建议**：要么删除，要么集成到 sub-agent 路径（`sub_executor.rs`）。当前状态是死代码。

### 2.2 `src/compact_dp.rs:1-36` — CompactionTier 的头部注释（~10 行无用）

```rust
/// compact_turn_keep: compute turn-aligned lines to keep using MinKeepRatio.
```

文件中保留了旧 DP 时代的注释风格。`tail_budget_ratio` 中 Conservative 返回 `0.20`、Aggressive 返回 `0.10`，但这俩 tier **在默认 compact_pct=85% 下永远不被触发**。虽然 `CONTEXT_COMPACT_PCT` 环境变量可以使其生效，但这是"为灵活性牺牲了正确性"——当用户设置 `COMPACT_PCT=60` 时，压缩触发于 60%，但 tier 在 60% 下是 Conservative（keep=20%），压缩后上下文从 60% 降到 12%，收益比可能不合理。

**建议**：移除 Conservative 和 Aggressive 的 keep_ratio（或至少添加注释说明它们仅在 compact_pct < 80 时生效）。

### 2.3 `src/agent/sub_executor.rs:136-167` — 结果提取逻辑（~30 行，需 Review）

```rust
for line in lines.iter().rev() {
    if line.get("role")... == "assistant" {
        // 提取第一个 thinking block 和第一个 text block
        break;
    }
}
```

只提取最后一条 assistant 消息的第一个 thinking 和第一个 text block。如果 sub-agent 有多次 LLM 调用（tool_use 循环），**只取最后一次的最终回复**，丢弃之前的 reasoning。对于需要 debug 追踪的场景，丢失了大量中间推理信息。

**这不是过度设计，而是设计选择的取舍**——sub-agent 的结果整合进父会话时保持简洁。但如果用户期望看到 sub-agent 的完整推理链，当前设计是不够的。

---

## 三、缺失功能

### 3.1 传感器层未实现

`.opencode/plans/feedback-control-system.md` 完整设计了传感器层架构，但**代码中没有任何实现**。当前错误检测全靠 Rust 层硬编码的 `classify_error_from_message`。

| 缺失 | 影响 |
|------|------|
| 项目级 error.sh 传感器 | 编译/测试失败无法触发升级 |
| 性能传感器 | 工具延迟/输出大小无可追溯反馈 |
| 上下文传感器 | 缓存退化无法自动检测 |
| 传感器发现路径 | 用户不能自定规则 |

### 3.2 `verify_fingerprint` 仅 verbose 模式执行

```rust
// turn.rs:63-68
if self.ctx.verbose() && !prefix.verify_fingerprint() {
    panic!("...");
}
```

fingerprint 验证在设计上是为了**防止缓存偏移 bug 在生产环境运行**，但 `verbose` 通常只在开发环境打开。生产环境（`--print` stream-json）不会触发验证。

**建议**：默认启用 fingerprint 验证，verbose 时 panic，非 verbose 时 log event。

### 3.3 `max_context_tokens` 未传递到 StatsSnapshot 之外的路径

```rust
// turn.rs:355
max_context_tokens: self.ctx.config.max_context_tokens as u64,
```

现在已传递到 `StatsSnapshot`，但在其他路径中，`max_context_tokens` 用于：
1. CompactionEngine 的 `should_compact`（✅ 使用）
2. Preflight 检查（✅ 使用）
3. 标题栏上下文百分比（✅ 使用，刚刚添加）
4. **传感器上下文压力判断**（❌ 缺失——传感器未实现）

### 3.4 无 Budget 上限

没有成本上限控制。用户无法设置 "session 成本超过 $X 就停止"。虽然 DeepSeek 价格极低导致紧迫性不高，但对于自动化脚本（CI 集成等）这是一个明显的缺失。

### 3.5 无 `--help` 输出 `dscode` 模式

当前 `--help` 输出 `Usage: rustagent [options] [prompt]`——已经不匹配了。需要改为 `dscode`。

### 3.6 内置 Skill 缺失

system prompt 的 `skill-index` 段（`prompt.rs:102`）会枚举 `~/.claude/skills/` 和 `<project>/.claude/skills/` 中的 skill，但**项目本身不提供任何内置 skill**。`skills/test-skill-repo/` 只是一个测试标记文件。

`.opencode/plans/superpowers-analysis.md` 建议的 `debugging`、`verification`、`tdd` 三个内置 skill 都未实现。

### 3.7 SubAgent 的结果透传限制

`SubAgentResult` 通过 user message 注入父会话：

```
[sub-agent abc123] ok (in=500, out=200)
Thinking: ...
Text: ...
```

但如果 sub-agent 产生了 `TurnEffect`（如再次启动了 sub-sub-agent），这些 effect **不传递到父会话**。`sub_executor.rs` 的 `run_impl` 中 `let (decision, _effects) = executor.execute(prompt).await?` 直接丢弃了 effects。

---

## 四、优化方向

### 4.1 按重要性排序

| 优先级 | 优化项 | 预估算力 | 价值 |
|:------:|--------|:--------:|:----:|
| P0 | 工具分发用 trait 替代 match | +30 行 | 降低新工具添加成本 |
| P0 | `--help` 输出去掉旧的 `rustagent` 引用 | +1 行 | 品牌一致 |
| P1 | 实现错误传感器（shell 脚本） | +60 行 | 编译/测试失败触发升级 |
| P1 | ToolCallRepair 集成到 sub-agent | +30 行 | 子代理也获得修复能力 |
| P2 | fingerprint 验证默认启用 | +5 行 | 生产环境缓存偏移检测 |
| P2 | 上下文压力阈值可配置 | +10 行 | 调优灵活性 |
| P3 | Budget 限制 | +40 行 | 成本控制 |
| P3 | 内置 skill | +150 行 | 立即可用的流程技能 |

### 4.2 用 trait 替代 match 分发

当前工具分发在 `execute_one_sync` 中使用巨大的 `match` 块，每个工具内部又有一个内联 struct 定义：

```rust
"Bash" => {
    #[derive(Deserialize)] struct Args { ... }
    let args: Args = serde_json::from_value(...)?;
    bash::execute(...)
}
```

**重构方案**：

```rust
trait Tool {
    type Args: DeserializeOwned;
    fn name() -> &'static str;
    fn execute(args: Self::Args, ctx: &Context) -> Result<String>;
}

struct BashTool;
impl Tool for BashTool {
    type Args = BashArgs;
    fn name() -> &'static str { "Bash" }
    fn execute(args: BashArgs, ctx: &Context) -> Result<String> {
        bash::execute(...)
    }
}
```

**优点**：
1. 添加新工具只需 impl Tool trait，不需要改 match 分支
2. 工具参数 struct 移到工具自身模块，不堆积在 runner.rs
3. 可为工具添加元数据（storm_exempt、mutating 等）

**代价**：~30 行 trait 定义 + 每个工具约 5 行 impl。对于当前 12 个工具的规模，可以把 runner.rs 从 381 行压缩到 ~200 行。

### 4.3 传感器错误检测

作为 `--skill debugging` 或 `--skill code-review` 的 SKILL.md 文本：

```markdown
# Debugging Process

## Phase 1: Read the error completely
Before ANY fixes, read the ENTIRE error output:
- Read the FIRST error (not the last)
- Check line numbers and file paths
- Run the failing command with --verbose

## Phase 2: Isolate the root cause
- What changed between "working" and "broken"?
- Grep the error message across the codebase
- Create a minimal reproduction

## Phase 3: Fix with validation
- Write a test that reproduces the bug
- Implement the fix
- Verify the original error is gone
- Run the full test suite
```

这是 `.opencode/plans/superpowers-analysis.md` 提出的"Phase 1 Prompt 级增强"的后续——通过 `--skill debugging` 按需加载。不需要修改 Rust 代码。

### 4.4 非功能性优化

| 优化项 | 当前 | 优化后 | 复杂度 |
|--------|------|--------|:------:|
| 二进制体积 | 3.3MB（strip） | ~2.5MB（移除 unused features） | 低 |
| 编译时间 | ~16s release | ~12s（减少泛型使用） | 中 |
| REPL 启动 | 需等 `TurnExecutor` 首次创建 | 可提前预热 | 低 |

---

## 五、功能完备度矩阵

### 5.1 作为 coding agent 的核心功能

| 功能 | 状态 | 说明 |
|------|:----:|------|
| 文件读写 | ✅ | Read/Write/Edit |
| Shell 执行 | ✅ | Bash 安全沙箱 |
| 文件搜索 | ✅ | Glob + Grep（依赖 ripgrep） |
| 网络搜索 | ✅ | WebSearch + WebFetch（依赖 Jina） |
| 子代理 | ✅ | 并发 SubAgent 独立上下文 |
| 计划系统 | ✅ | PlanConfirm/PlanClear + plan.draft/plan.md |
| 上下文压缩 | ✅ | 三级阈值 + 同轮保护 |
| 会话管理 | ✅ | 命名/继续/列出/replay |
| 技能系统 | ✅ | 按需加载 SKILL.md |
| **错误回收（Scavenge）** | ✅ | DSML/XML/Bracket/JSON/3-shape |
| **截断修复** | ✅ | 每工具执行前自动修复 |
| **重复抑制** | ✅ | StormBreaker 6/3 窗口 |
| **模型升级** | ✅ | 多信号 TurnFailureTracker |
| **手动模型切换** | ✅ | /flash /pro 命令 |

### 5.2 相对于同类工具的功能差距

| 功能 | bash-agent | opencode | claude code |
|------|:---------:|:--------:|:----------:|
| 集成终端 UI | ❌ 仅 terminal | ✅ | ✅ |
| 在编辑器中内嵌 | ❌ | ✅ | ❌ |
| 项目级配置 | ✅ env var | ✅ JSON | ✅ `.claude/` |
| 内置 skill | ❌ | ✅ | ✅ |
| MCP 协议 | ❌ | ✅ | ✅ |
| 成本限制 | ❌ | ✅ | ✅ |
| 传感器层 | ❌ 设计完成 | ❌ | ❌ |
| 跨 session 任务延续 | ❌ | ✅ | ❌ |
| 行内 diff 编辑 | ❌ | ✅ | ✅ |
| Git 集成 | ❌ | ✅ | ✅ |

---

## 六、风险点

### 6.1 技术债务

| 风险 | 位置 | 影响 | 修复代价 |
|------|------|------|:--------:|
| `classify_failure_message` 与 `classify_error_from_message` 存在重复 | `orchestrator.rs` + `errors.rs` | 已修复一次，但仍有语义重叠 | 低（已本质上修复） |
| `runner.rs` 工具分发 match 块越长越大 | 每个新工具都加一个分支 | 代码膨胀，难以测试 | 中（需 trait 重构） |
| `execute_one_sync` 中内联 struct 定义 | 每个工具分支 | 无法复用，参数来源不清晰 | 低（移到对应模块） |

### 6.2 外部依赖风险

| 依赖 | 用途 | 风险等级 | 替代方案 |
|------|------|:--------:|---------|
| `ripgrep` (rg) | Glob/Grep | 中 | 可选依赖，不安装时功能降级 |
| `reqwest` + `rustls-tls` | HTTP | 低 | 标准库已包含 |
| `rustyline` | REPL | 低 | 降级为 simple_stdin_loop |
| `Jina AI API` | WebSearch/WebFetch | 中 | 可选功能，不影响核心 |

---

## 七、结论

### 7.1 是否过度设计

**局部过度设计有 3 处**（~130 行），**全局设计基本合理**。

| 过度设计 | 行数 | 状态 |
|---------|:----:|------|
| ToolCallRepair 死代码 | ~90 | 可删除或集成 |
| CompactionTier 无用 tier | ~30 | 可简化 |
| fingerprint 仅 verbose 验证 | ~10 | 可默认开启 |
| **合计** | ~130 | 占总量 1.9% |

### 7.2 优化方向优先级

```
P0（立即）：
  1. --help 输出去掉旧名
  2. ToolCallRepair 删除或集成
  3. 实现 --help 输出二进制命名 dscode

P1（短期）：
  4. 内置 debugging skill（编译/测试失败处理流程）
  5. 传感器 error.sh（编译/测试失败触发升级）
  6. 工具分发 trait 化

P2（中期）：
  7. fingerprint 验证默认开启
  8. SubAgent effect 透传
  9. Budget 上限

P3（长期）：
  10. 内置 skill 套件（debugging + verification + tdd）
  11. 完整的传感器层（perf + context + progress）
  12. Git 集成（git diff 预览、commit 建议）
```

### 7.3 核心优势

尽管与 opencode/claude code 相比缺少 MCP、终端 UI 等平台级功能，bash-agent 在**以下方面设计得更好**：

1. **维修流水线**（Scavenge + Truncation + Storm）— 同类工具中没有这样系统化的错误恢复链路
2. **缓存对齐的压缩** — 三级阈值 + 同轮保护 + 最小收益检查，直接针对 DeepSeek 前缀缓存优化
3. **传感器层设计**（未实现但已设计）— 脚本级可定制错误检测，同类工具没有类似的结构
4. **单二进制结构** — 3.3MB、零运行时依赖，比 opencode 的 Node.js 依赖树轻量两个数量级
