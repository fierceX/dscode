# Agents Guide

## 编译与测试

### 快速命令

```bash
# 编译 Release 版本
make build

# 运行所有测试
make test

# 清理构建产物
make clean
```

### 分项命令

| 命令 | 说明 |
|------|------|
| `make build` | 编译 release 版本二进制到 `target/release/dscode` |
| `make check` | 运行 `cargo check` 类型检查 |
| `make test` | 运行 `cargo check` + `cargo test` |

### CI 推荐流程

```bash
make test
```

---

## 控制系统

基于控制论 + 贝叶斯 + 因果推断的三层架构：

```
Phase 0  因果推断提示    → 每次代码修改前引导因果推理
Phase 1  传感器层        → error.sh 自动检测工具错误（70+ 模式）
Phase 2  控制器          → P(stall) = 1 - 0.5^k 贝叶斯停滞检测
Phase 3  模型选择器       → 单边监控 flash 质量，证明不够好才切 pro
```

### 模型选择器设计（当前实现）

**核心思想**：flash 是默认模型，pro 是保底方案。不需要比较 flash vs pro——只需要证明 flash "不够好"。

```
resolve_active():
  ① forced_model  → 手动指定（/flash, /pro）
  ② !auto_model   → 配置文件中的模型
  ③ is_locked()   → Pro（短期停滞，P(stall) > 0.80）
  ④ flash Q < 0.50 && N ≥ 16 → Pro（长期质量低于阈值，工具级观测）
  ⑤ 默认 → Flash
```

**Q = α/(α+β)**：flash 的 Beta 后验均值
**N = α+β-2**：总观测次数
**/flash 复位**：flash 信念重置为 Beta(3,3)，给公平证明机会

### 传感器层

`assets/sensors/error.sh` 内置 70+ 种错误检测模式，覆盖：

| 类别 | 模式数 | 权重 |
|:----:|:------:|:----:|
| Rust | 12 | 1.0 |
| Python | 11 | 1.0 / 0.8 |
| Node.js | 7 | 1.0 / 0.8 |
| Go/Java/Docker/网络/文件系统/进程 | 30+ | 0.3-1.0 |
| 性能（慢执行/大输出） | 2 | 0.3-0.5 |

每次工具执行后自动调用。信号通过 `accumulated_signals` 聚合后喂入 Controller。

### 运行时日志

日志输出到 `<session>/events.jsonl`，关键事件类型：

| 事件 | 说明 |
|------|------|
| `turn_start` | 每轮开始：模型选择、controller 快照、selector 信念 |
| `turn_tracking` | 每轮结束：决策、工具调用数、信号详情、完整状态 |
| `sensor_signal_aggregated` | 传感器信号聚合（仅失败轮次） |
| `control_action` | 控制动作：InjectReflectionHint / UpgradeModel / Abort |
| `model_selector_update` | 模型信念更新：tier、成功/失败、α/β 均值 |

用 `grep` + `jq` 分析：
```bash
# 查看模型选择快照
grep '"type":"turn_tracking"' events.jsonl | jq '{decision, controller: {k: .controller.k, P: .controller.P_stall}, selector: .model_selector}'

# 查看控制动作触发历史
grep '"type":"control_action"' events.jsonl | jq '{action, P_stall, k}'
```

### 标题栏实时状态

```
flash Q:0.68/33 T:12 R:45 I:200K(50%) O:20K C:400K(40%) ¥0.12

≥1M 时自动切换为 M 单位:
pro  Q:—    T:40 R:150 I:1.23M(60%) O:50K C:800K(80%) ¥2.35
       ^^ 在 Pro 上不显示 Q
```

**字段说明**：

| 字段 | 含义 |
|------|------|
| Q:0.68/33 | flash 成功率 α/(α+β) / 观测数 |
| T | turn 数 |
| R | request 数 |
| I | input tokens（缓存命中率） |
| O | output tokens |
| C | context tokens（使用率） |
| ¥ | 总成本（美元） |

### 会话持久化

| 文件 | 内容 |
|------|------|
| `stats.json` | 累积统计（tokens, costs, turns） |
| `model_beliefs.json` | 模型 β 信念（α/β 值），session 续接时自动恢复 |

### 手动模型切换

```
/flash — 切回 flash，信念重置为 Beta(3,3)
/pro   — 强制 pro，直到用户切回
```

### 启用自动模型切换

```bash
AUTO_MODEL=1 ./target/release/dscode
```
