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
SignalCollector.collect(name, output)
     ├── NonZeroExit — 退出码 ≠ 0（可信度最高，权重 0.8~0.9）
     ├── ToolError   — regex 匹配错误模式（权重 0.3~1.0）
     └── EditLoop    — 编辑-检查循环（窗口 W=6，分级 0.4~0.9）
     │
BeliefTracker.observe(signals)
     ├── 拉普拉斯平滑: α = 1+Σsuccess, β = 1+Σfailure
     ├── 滑动窗口: 默认 W=16，旧错误自动退出
     └── B = α/(α+β) ∈ [0, 1]
     │
DecisionEngine.decide(B, errors)
     ├── B ≥ 0.7 → None
     ├── B < 0.7 → Inject(含具体错误)
     └── B < 0.3 → Abort
```

### 三组件

| 组件 | 文件 | 职责 |
|------|------|------|
| `SignalCollector` | `guard/collector.rs` | 采集信号，自维护调用历史 |
| `BeliefTracker` | `agent/belief.rs` | 纯计算：滑动窗口+拉普拉斯平滑 |
| `DecisionEngine` | `agent/decision.rs` | 纯决策：注入/中止 |

### 关键参数

| 参数 | 默认值 |
|------|--------|
| 滑动窗口大小 | 16 次工具调用 |
| EditLoop 检测窗口 | 6 次 |
| Abort 阈值 | B < 0.30 |
| 警告阈值 | B < 0.50 |
| 提醒阈值 | B < 0.70 |

### 标题栏信念度

```
flash B:0.73 T:12 R:45 I:200K(50%) O:20K C:400K(40%) ¥0.12
```

| B 值 | 含义 |
|------|------|
| 1.0~0.7 | 🟢 顺利 |
| 0.7~0.5 | 🟡 偶有小错 |
| 0.5~0.3 | 🟠 频繁出错 |
| < 0.3 | 🔴 严重 |

### 提示词注入

低信念时自动追加提示词到用户消息末尾（不修改 system prompt，保护前缀缓存）：

```ruby
# 用户输入
$ fix the bug

# 实际发送（belief < 0.5）
$ fix the bug

[System note: Multiple failures detected (belief 0.37). Adjust approach.
Recent errors:
- Bash(cargo build): Rust compilation error (error[E0308])
- Bash(cargo build): process exited with code 1]
```

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
