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

## 控制系统（新增）

基于控制论 + 贝叶斯 + 因果推断的三层架构：

```
Phase 0  因果推断提示    → 每次代码修改前引导因果推理
Phase 1  传感器层        → error.sh 自动检测工具错误（70+ 模式）
Phase 2  控制器          → P(stall) = 1 - 0.5^k 贝叶斯停滞检测
Phase 3  模型选择器       → Thompson Sampling (Greedy) 自动切换 flash/pro
```

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

### 会话持久化

| 文件 | 内容 |
|------|------|
| `stats.json` | 累积统计（tokens, costs, turns） |
| `model_beliefs.json` | ModelSelector 信念（α/β 值），session 续接时自动恢复 |

### 手动模型切换

- `/flash` — 手动切到 flash
- `/pro` — 手动切到 pro

### 启用自动模型切换

```bash
AUTO_MODEL=1 ./target/release/dscode
```
