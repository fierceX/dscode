# Agents Guide

## 编译与测试

```bash
make build    # Release 编译
make check    # Type check
make test     # 测试
```

---

## 信号机制

### 架构

```
工具执行完毕
     │
SignalCollector.collect(name, output, exit_code, content)
     ├── ToolFailed — 确定性工具失败（exit_code ≠ 0 / 输出 "Error:"，权重 0.9）
     ├── ToolError  — 启发式错误检测（regex 匹配，权重 0.3~1.0）
     └── EditLoop   — 编辑-检查循环（窗口 W=6，分级 0.4~0.9）
     │
BeliefTracker.observe(signals)
     ├── 拉普拉斯平滑: α = 3+Σsuccess, β = 1+Σfailure
     ├── 滑动窗口: 默认 W=16，旧错误自动退出
     └── B = α/(α+β) ∈ [0, 1]
     │
DecisionEngine.decide(B, errors)
     ├── B ≥ 0.7 → None
     ├── B < 0.7 → Inject(含具体错误)
     └── B < 0.3 → Abort（中断任务）
```

### 设计思想

**信号采集只有一个入口**。所有错误信号都在 `SignalCollector.collect()` 中产生，`turn.rs` 只负责调用它并传递参数。不出现在其他任何地方进行额外的内联检测。

**三条信号链路互不重叠**：

| 信号 | 来源 | 确定性 | 说明 |
|------|------|--------|------|
| `ToolFailed` | 工具执行结果 | ✅ 确定性 | exit_code ≠ 0（Bash）或输出 `"Error:"` 开头（其他工具）。命令真的失败了 |
| `ToolError` | 输出文本 regex | ❌ 启发式 | 输出中出现了错误关键词，但命令不一定真失败 |
| `EditLoop` | 工具调用序列 | ✅ 确定性 | 窗口 W=6 内 Edit > 4 或 Edit↔Diff 交替，无读操作 |

`ToolFailed` 和 `ToolError` 是两条完全独立的链路。`ToolFailed` 不通过 regex 检测——它来自工具执行层直接报告的 `exit_code` 或 `Result::Err`。退出码信息由 `bash::execute()` 捕获（`child.status.code()`），通过 `ToolExec` trait 的返回值一路透传到 `SignalCollector.collect()`，不走输出文本匹配。

### 三组件

| 组件 | 文件 | 职责 | 外部依赖 |
|------|------|------|---------|
| `SignalCollector` | `guard/collector.rs` | 采集三种信号，自维护调用历史 | 0 |
| `BeliefTracker` | `agent/belief.rs` | 拉普拉斯平滑+滑动窗口，纯计算 | 0 |
| `DecisionEngine` | `agent/decision.rs` | 根据 B 和错误列表，输出注入/中止 | 1 |

### 信任先验

信念度使用拉普拉斯平滑（贝叶斯 Beta 推断），并非"无偏"：因为使用模型本身意味着信任它大概率能正确使用工具，所以先验设置为 α=3, β=1，初始 B=0.75：

| 场景 | B | 含义 |
|------|:-:|:----:|
| 初始（无观测） | 0.750 | **信任先验**—默认相信工具正常工作 |
| 1 次清洁调用 | 0.800 | ✅ 不触发 |
| 1 次严重错误 | 0.600 | 🟡 提醒区 |
| 2 次严重错误 | 0.533 | 🟠 警告区 |
| 5 次严重错误 | 0.333 | 🔴 Abort 区 |

初始 0.75 > 0.70，所以新任务开始时不会立即触发提醒。先验效应会随着观测累积被自然淹没（长窗口下先验不重要）。

### 滑动窗口

信念度基于最近 W=16 次工具调用（默认），旧错误随着窗口滑动自动退出，信念自然恢复。新用户输入时窗口清空，信念回到 0.75。

### 标题栏信念度

```
flash B:0.73 T:12 R:45 I:200K(50%) O:20K C:400K(40%) ¥0.12
```

信念度在每次工具调用后实时更新（标题栏可见）。

| B 值 | 含义 |
|------|------|
| 0.75 | 初始（信任先验） |
| > 0.7 | 🟢 顺利 |
| 0.5~0.7 | 🟡 偶有小错 |
| 0.3~0.5 | 🟠 频繁出错 |
| < 0.3 | 🔴 严重 |

### 提示词注入（任务循环内）

注入发生在 **同一次任务循环内部的 `turn.rs::execute()` 循环中**，工具执行完成后、下一轮 LLM 调用之前：

```
Phase 3: 工具执行 → SignalCollector.collect() → BeliefTracker.observe()
Phase 4: stop = "tool_use"
  ├─ DecisionEngine.decide(B, errors)
  │   ├─ Inject → store.add_user("[System note: ...]")  ← 写入对话存储
  │   └─ Abort  → 返回 Failed，中断本轮
  └─ continue → 下一轮 LLM: messages = store.lines()（包含注入消息）
```

注入消息作为一条独立的 User 消息写入对话存储，**不在 system prompt 中注入**（保护前缀缓存），**也不追加到用户输入末尾**。LLM 在下一轮调用时自然看到。

注入内容包含具体错误信息：

```
[System note: Multiple failures detected (belief 0.37). Adjust approach.
Recent errors:
- Bash(cargo build): Rust compilation error (error[E0308])
- Bash(cargo build): process exited with code 1]
```

详细的信号系统设计哲学见 [`docs/设计哲学-信号系统.md`](docs/设计哲学-信号系统.md)。

---

## 模型切换

手动切换（无自动模型选择）：

```
/flash — 切回 flash（重置信念）
/pro   — 强制 pro
```

---

## 运行时日志

日志输出到 `<session>/events.jsonl`：

```bash
# 查看信念变化
grep '"belief"' events.jsonl | jq '{type, belief}'

# 查看注入历史
grep '"Injecting hint"' events.jsonl
```
