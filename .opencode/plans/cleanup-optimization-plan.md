# 清理优化计划方案

> 基于四份分析报告（architecture / code-quality / feature-completeness / test-coverage）
> 合计 1428 行深度分析，提取可执行的分步骤行动计划。
> 日期：2025-06-18
> **更新：2025-06-18 — 大部分已完成，详见下方标记**

---

## 执行状态总览

| 阶段 | 任务数 | 已完成 | 进度 | 
|:----:|:------:|:------:|:----:|
| **第一阶段：紧急清理** | 6 | 6 | ✅ 100% |
| **第二阶段：架构修正** | 4 | 1 | 🟡 25% |
| **第三阶段：测试补强** | 6 | 3 | 🟡 50% |
| **第四阶段：性能与质量** | 5 | 3 | 🟡 60% |
| **第五阶段：远期迭代** | 4 | 0 | ⚪ 0% |
| **额外修复（计划外）** | 3 | 3 | ✅ 100% |

### 计划外修复

| # | 修复 | commit |
|:-:|------|--------|
| E1 | Stats 缺 `#[serde(default)]` 导致旧 session 统计归零 | `3851f35` |
| E2 | ModelSelector 幽灵模型导致 `/flash` 无法切回 flash | `3342f6f` |
| E3 | ModelSelector 信念持久化到 session 目录 | `10c096e` |

---

## 总览

| 阶段 | 任务数 | 预计影响 | 预计时间 |
|:----:|:------:|---------|:--------:|
| **第一阶段：紧急清理** | 6 | 删除死代码 + 修复 bug | 1-2h |
| **第二阶段：架构修正** | 5 | 修复接口缺口 + 重构过度设计 | 3-5h |
| **第三阶段：测试补强** | 6 | 补全核心路径集成测试 | 4-6h |
| **第四阶段：性能与质量** | 5 | 减依赖、去冗余、加文档 | 2-3h |
| **第五阶段：远期迭代** | 4 | 可选增强，不紧急 | 按需 |

---

## 第一阶段：紧急清理（P0）

### 1.1 删除 `failure_tracker.rs` 整个文件

**来源**：architecture-review（2.1）+ code-quality-review（1.1）+ feature-completeness（删除清单）

**当前状态**：
- `src/agent/failure_tracker.rs` 110 行（含 6 个测试）
- `src/agent/mod.rs` 中**没有**模块声明
- 全局 `grep -rn "failure_tracker" src/` 返回空
- 该文件的代码从不被编译，是纯垃圾代码

**步骤**：
1. 删除 `src/agent/failure_tracker.rs`
2. 验证编译：`cargo check`
3. 验证测试：`cargo test`

**影响**：-110 行

---

### 1.2 删除 Controller 中 2 个死字段

**来源**：architecture-review（2.2）+ code-quality-review（1.3）+ feature-completeness（4.2）

**当前状态**：
```rust
// src/agent/controller.rs 第 22-23 行
context_pressure_history: VecDeque<f32>,  // 有写入方法但从未被调用
cache_hit_history: VecDeque<u8>,          // 有写入方法但从未被调用
```

**步骤**：
1. 删除两个字段
2. 删除 `record_context_pressure()` 方法
3. 删除 `record_cache_hit()` 方法
4. 删除 `use std::collections::VecDeque`（不再需要）
5. 删除对应测试 `record_context_pressure_maintains_window`
6. 运行 `cargo test` 确认无回归

**影响**：-40 行

---

### 1.3 `note_end_turn()` 接入 orchestrator

**来源**：feature-completeness（Signal Chain 完整性）

**当前状态**：
- `Controller::note_end_turn()` 方法已定义但**从未被调用**
- `has_fix_loop()` 依赖 `had_end_turn` 标记，但由于该标记永远为 `false`，当 `tool_call_count > 15` 时**总是误触发**
- 修复循环检测的 `had_end_turn` 条件完全无效

**步骤**：
1. 在 `OrchActor::handle_user_input` 的 `Ok((decision, effects))` 分支中，`TurnDecision::Stop` 时调用 `self.controller.note_end_turn()`
2. 确认调用位置在所有工具调用计数之后、`update_after_turn` 之前
3. 运行现有 fix_loop 测试确认仍通过
4. 新增测试：Stop 后 `had_end_turn` 为 true

**影响**：+3 行，修复一个功能性 bug

---

### 1.4 删除 `append_feedforward_hints()` 废弃函数

**来源**：code-quality-review（1.2）

**当前状态**：
- `src/prompt.rs:405`，明确标注 `DEPRECATED`
- 禁用原因：破坏前缀缓存对齐
- 函数体 ~55 行

**步骤**：
1. 删除函数定义
2. 删除 `fn has_any()` 辅助函数（仅被 `append_feedforward_hints` 使用）
3. 运行 `cargo test` 确认无回归

**影响**：-65 行

---

### 1.5 删除 `turn.rs:69` 的生产环境 `panic!`

**来源**：code-quality-review（4.2）

**当前状态**：
```rust
// src/agent/turn.rs:69
if !prefix.verify_fingerprint() {
    panic!(
        "ImmutablePrefix fingerprint mismatch — prefix mutated without invalidation. \
         This will break DeepSeek's prefix-cache alignment."
    );
}
```

**风险**：如果 `invalidate_prefix()` 未在某条路径调用，**整个进程崩溃**。

**步骤**：
1. 将 `panic!` 改为 `log::error!` + 强制重建前缀：
```rust
if !prefix.verify_fingerprint() {
    log::error!("ImmutablePrefix fingerprint mismatch — forcing rebuild");
    *guard = None;
    return Err(anyhow::anyhow!("prefix fingerprint mismatch, forcing rebuild"));
}
```
2. 确认调用方 `ensure_prefix()` 能正确处理错误返回

**影响**：-1 行，消除生产崩溃风险

---

### 1.6 删除 `ToolResult` 和 `SensorOutput` 中的死字段

**来源**：code-quality-review（1.3）

**当前状态**：
- `sensor.rs` ：`SensorOutput::actions` 标注了 `dead_code` 但仅用于 serde 反序列化
- `sub_pool.rs:20` ：`max_concurrent` 字段赋值但从未读取
- `sub_executor.rs:20` ：`session_id` 字段赋值但从未读取

**步骤**：
1. 对 `SensorOutput::actions`：保留（是协议字段，未来传感器可能发出）
2. 删除 `sub_pool.rs` 的 `max_concurrent` 字段（当前使用 `Semaphore` 而不是此字段）
3. 删除 `sub_executor.rs` 的 `session_id` 字段（如果确实未被读取）
4. 确认编译通过

**影响**：-5 行

---

## 第二阶段：架构修正（P1）

### 2.1 消除 `truncate_str` 重复定义

**来源**：architecture-review（3.1）+ code-quality-review（5.2）

**当前状态**：
- `src/agent/orchestrator.rs:488` 定义了 `fn truncate_str()`
- `src/agent/turn.rs:410` 定义了**完全相同**的 `fn truncate_str()`

**步骤**：
1. 在 `src/` 下新建 `util.rs`（如不想新增文件，可放入现有模块如 `session/mod.rs`）
2. 将 `truncate_str` 移到公共位置，标记 `pub(crate)`
3. 在 orchestrator.rs 和 turn.rs 中删除本地定义，改为 `use crate::util::truncate_str`
4. 运行测试确认

**影响**：-20 行，消除重复

---

### 2.2 删除 `Controller::note_error()` 包装方法

**来源**：architecture-review（3.3）

**当前状态**：
```rust
pub fn note_error(&mut self, error_decreased: bool) {
    self.note_progress(error_decreased);
}
```

该方法是 `note_progress` 的直接转发，没有增加语义价值，且参数名 `error_decreased` 反直觉。

**步骤**：
1. 搜索 `controller.note_error` 调用点（`orchestrator.rs` 约 5 处 + `test_mock.rs` 约 30 处）
2. 将 `note_error(false)` 替换为 `note_progress(false)`
3. 将 `note_error(true)` 替换为 `note_progress(true)`
4. 删除 `note_error` 方法定义
5. 运行全部测试确认替换正确

**影响**：-5 行，语义更清晰

---

### 2.3 统一模型名字符串到 `ModelTier` 枚举

**来源**：architecture-review（3.4）

**当前状态**：
- `model_selector.rs` 中有 `unwrap_or("flash")` 硬编码
- `orchestrator.rs` 中有 `self.model_selector.ensure("flash")` / `ensure("pro")` 硬编码
- `test_mock.rs` 中有多处 `ensure("flash")` / `ensure("pro")` 硬编码

**步骤**：
1. 在 `ModelTier` 上添加 `const PRIMARY: &str = "flash"` 和 `const SECONDARY: &str = "pro"`
2. 将所有硬编码字符串替换为常量引用
3. 运行测试验证

**影响**：~10 行，消除魔法字符串

---

### 2.4 合并 `SubAgentReport` 和 `SubAgentResult`

**来源**：architecture-review（3.5）

**当前状态**：
- `sub_executor.rs` 定义 `SubAgentResult`（含 `status`, `thinking`, `text`, `usage`, `session_id`）
- `sub_pool.rs` 定义 `SubAgentReport`（含 `status`, `thinking`, `text`, `usage`, `session_id`）——字段高度重叠

**步骤**：
1. 检查两个结构体的字段差异
2. 选择保留 `SubAgentReport`（更常用），将其移动到公共位置
3. 删除 `SubAgentResult`
4. 更新引用点

**影响**：-15 行

---

### 2.5 拆分 `OrchActor::handle_user_input` 为子方法

**来源**：architecture-review（5.4）

**当前状态**：
`handle_user_input` 约 250 行，耦合了模型解析 → LLM 创建 → 执行 → 错误处理 → Controller 更新 → ModelSelector 更新 → 日志 → UI 渲染。

**步骤**：
1. 提取 `setup_turn()` → 模型选择 + Controller reset + LLM 创建
2. 提取 `process_effects()` → 子代理 / PlanClear / PlanConfirm / NeedsPro
3. 提取 `post_process_turn()` → 传感器聚合 + 控制动作 + 模型选择器更新 + 日志
4. 保留 `handle_user_input` 作为调度入口

**影响**：~250 行重组，提高可读性

---

## 第三阶段：测试补强（P0-P1）

### 3.1 新增 `OrchActor::resolve_active` 全分支测试

**来源**：test-coverage-review（G1, G6）

**当前状态**：
- `simulate_resolve_active` mock 函数缺少 `forced_model` 分支
- `auto_disabled` 时 mock 返回 "flash"，但真实逻辑是 `ModelTier::parse(&config.model)`
- 当 `controller.is_locked()` 和 `get_control_action()` 均为真时，有重复检查

**步骤**：
1. 补充 `forced_model = Some(Pro)` → 直接返回 "pro" 的测试
2. 补充 `forced_model = Some(Flash)` → 直接返回 "flash" 的测试
3. 对齐 `simulate_resolve_active` 与真实 `resolve_active` 的差异
4. 添加 `auto_disabled=false && config.model="pro"` → 返回 "pro" 的测试

**影响**：+15 行测试

---

### 3.2 新增 TurnExecutor 基本测试（含 Mock LLM）

**来源**：test-coverage-review（G2, G9）

**当前状态**：
- `TurnExecutor` 0 个直接测试
- `MockLlmClient` 存在但仅用于 2 个创建测试
- 工具调用计数、传感器信号累积均未通过真实 TurnExecutor 验证

**步骤**：
1. 创建最小 `AgentSharedContext` 测试夹具（使用 `tempfile::TempDir` 作为 store 和 events 路径）
2. 使用 `MockLlmClient` 创建 `TurnExecutor`
3. 测试简单场景：1 个 text + stop → 验证 tool_call_count=0、无信号
4. 测试工具调用场景：LLM 返回 tool_call 事件 → 验证计数正确
5. 测试传感器信号累积：工具输出含错误 → 验证 `accumulated_signals` 非空

**影响**：+80 行测试

---

### 3.3 新增信号链路全流程集成测试

**来源**：test-coverage-review（全链路缺失项 + G8）

**当前状态**：
- 各段独立测试覆盖完整
- 但端到端 "传感器→TurnExecutor→OrchActor→Controller" 全链路无测试

**步骤**：
1. 模拟完整 turn 流程（在 test_mock.rs 或新文件）：
   - 创建 Controller + ModelSelector
   - 模拟 3 轮失败（含传感器信号）
   - 验证 P(stall) 从 0 → 0.5 → 0.75 → 0.875（每轮 Failed + 传感器聚合）
   - 第 4 轮成功 → P(stall) 重置为 0
2. 模拟模型切换流程：
   - 设置 selector 偏好 flash（高均值）
   - 连续失败使 P(stall) > 0.95
   - 验证 `is_locked()` 为 true → 模型选择强制为 pro
3. 模拟修复循环检测：
   - 20 次工具调用，无 end_turn
   - 验证 `has_fix_loop()` 为 true
   - 下一轮调用 `note_end_turn()` 后验证 `has_fix_loop()` 为 false

**影响**：+100 行测试

---

### 3.4 新增 CompactionEngine 异步单元测试

**来源**：test-coverage-review（G5）

**当前状态**：
- `compaction.rs` 的单元测试仅覆盖 `compact_turn_keep` 内联逻辑
- `evaluate_and_compact` 异步方法 0 个测试

**步骤**：
1. 使用 `tempfile::TempDir` 创建临时 summary 文件
2. 构造 `CompactionEngine` 实例
3. 测试：context tokens < 阈值 → 不触发压缩
4. 测试：context tokens > 阈值 → 触发压缩
5. 测试：手动触发（manual / plan_confirm）→ 总是压缩

**影响**：+50 行测试

---

### 3.5 新增 SubAgentPool 基本测试

**来源**：test-coverage-review（G3）

**当前状态**：0 个测试

**步骤**：
1. 创建 `SubAgentPool` 实例
2. 测试 `active_count()` 初始为 0
3. 测试并发限制（semaphore）——启动超过 `max_concurrent` 个子 agent，验证排队
4. 测试 `drain()` 等待全部完成

**影响**：+40 行测试

---

### 3.6 添加 Mutex 中毒恢复测试

**来源**：code-quality-review（4.1, 4.4）

**当前状态**：
- 5 处 `Mutex::lock().unwrap()` 存在中毒级联崩溃风险

**步骤**：
1. 对于所有长期持有的 Mutex（`SENSOR_INIT`、`immutable_prefix`、`storm`），将 `lock().unwrap()` 改为 `lock().unwrap_or_else(|e| e.into_inner())`
2. 不需要新增测试（Mutex 中毒在 Rust 标准库中已有确定性行为），但代码变更后运行全量测试确认

**影响**：~10 行变更

---

## 第四阶段：性能与质量（P2）

### 4.1 移除未使用的 Cargo 依赖

**来源**：code-quality-review（7.1, 7.2）

**当前状态**：
- `bytes = "1"` — 未使用
- `sha2 = "0.10"` — 未使用
- `tokio-stream = "0.1"` — 未使用
- `tokio-util = "0.7"` — 未使用
- `tokio = { features = ["full"] }` — 过度启用

**步骤**：
1. 从 `Cargo.toml` 移除 `bytes`、`sha2`、`tokio-stream`、`tokio-util`
2. 将 `tokio = "full"` 替换为 `tokio = { features = ["rt", "macros", "sync", "fs", "io-util", "process", "time"] }`
3. 运行 `cargo build --release` 确认编译
4. 运行 `cargo test` 确认无回归

**影响**：-4 个依赖，减少编译时间

---

### 4.2 统一 `snapshot` / `format` 模式

**来源**：architecture-review（3.2）

**当前状态**：
- `Controller` 有 `format_state()` + `snapshot()`（一对）
- `ModelSelector` 有 `format_beliefs()` + `snapshot_beliefs()`（一对）

**步骤**：
1. 保留两对方法（用途不同：console vs JSON）
2. 内部实现改为 `snapshot()` 生成 JSON → `format_*()` 从 JSON 提取字段格式化
   或者：让 `format_*()` 直接读取字段而非通过 JSON 中转
3. 当前架构可接受，此步骤**可选**

**影响**：~20 行

---

### 4.3 补充 pub 项的文档注释

**来源**：code-quality-review（6.1）

**当前状态**：以下 pub 项缺少文档注释：
- `Stats` struct
- `ToolResult` struct
- `first_line()` / `build_tool_call_summary()`
- `Paths` / `project_key()` / `paths_for()` / `chrono_session_id()`
- `compact_turn_keep()` / `CompactionTier`

**步骤**：
1. 依次为每个 pub 项添加 `///` 文档注释
2. 包含用途、参数说明、返回值说明

**影响**：+60 行文档，零逻辑变更

---

### 4.4 统一命名一致性

**来源**：code-quality-review（5.1）

| 当前 | 目标 |
|------|------|
| `chrono_now` (orchestrator.rs) | 提取到 `util.rs` 为 `timestamp_with_suffix()` |
| `chrono_now_rfc3339` (stats.rs) | 保留或统一为相同前缀 |
| 两个 `truncate_str` | 已在 2.1 处理 |

**步骤**：交由 2.1 处理 `truncate_str`；`chrono_now` 可后续统一

**影响**：~10 行

---

### 4.5 将传感器检测内联为 Rust 纯函数

**来源**：architecture-review（2.3）

**当前状态**：
- 传感器执行通过子进程 `bash error.sh` → stdin 管道 → stdout JSON 解析
- 每次工具调用 spawn 一个 shell 子进程
- 需要文件系统写入临时目录

**风险评估**：
- 当前性能可接受（~5-50ms/tool call）
- 但可维护性差、平台依赖强（`#[cfg(unix)]` 权限设置）
- 改为 Rust 纯函数（~50 行正则匹配）可以消除子进程开销、文件 IO、JSON 序列化

**步骤**：
1. 在 `sensor.rs` 中新增 `run_sensor_inline()` 纯函数版本
2. 保留 `run_sensor()` 作为 fallback（向后兼容）
3. 将 60+ 种错误模式从 shell 正则迁移到 Rust `regex` crate
4. 性能比较测试
5. 移除子进程版本（后续版本）

**影响**：~150 行重构，立即减少运行时开销

---

## 第五阶段：远期迭代（P3，按需）

### 5.1 添加用户传感器配置目录

**来源**：feature-completeness（用户扩展机制分析）

**当前状态**：
- `run_sensor()` 硬编码搜索 `/tmp/dscode-sensors-{PID}/`
- 计划承诺"用户可扩展"但实际不可用

**步骤**：
1. 在 `sensor.rs` 中修改 `sensor_resolve_path()`，按优先级搜索：
   - `<project>/.dscode/sensors/{name}.sh`
   - `<home>/.dscode/sensors/{name}.sh`
   - 内置回退
2. 参考设计文档：`.opencode/plans/error-sensor-skill-design.md`

---

### 5.2 实现 `perf` / `context` / `progress` 传感器

**来源**：feature-completeness（传感器缺失说明）

**当前状态**：
- `error.sh` 内嵌了 `perf_warning` 信号
- 没有独立的 `perf.sh`、`context.sh`、`progress.sh`

**步骤**：
1. `perf.sh`：检测工具延迟（elapsed > 阈值）、输出膨胀
2. `context.sh`：检测上下文压力
3. `progress.sh`：检测修复循环
4. 为每个新传感器定义 `SensorSignal` 类型（`perf_warning`、`context_high`、`progress_stalled`）

---

### 5.3 减少 `tokio = "full"` feature

**来源**：code-quality-review（7.2）

已在 4.1 中一并处理。

---

### 5.4 考虑消除 `once_cell` 依赖

**来源**：code-quality-review（7.3）

**当前状态**：`once_cell` 仅用于 `safety.rs` 的 `Lazy`。Rust 1.80+ 有 `std::sync::LazyLock`。

**步骤**：当 MSRV 升级到 1.80 时，迁移到标准库。

---

## 执行顺序依赖

```
1.1 (删除 failure_tracker)  ← 独立，无依赖
1.2 (删除死字段)            ← 独立，无依赖
1.3 (note_end_turn 接入)    ← 独立，无依赖
1.4 (删除废弃函数)          ← 独立，无依赖
1.5 (panic → error)         ← 独立，无依赖
1.6 (删除 sub_pool 死字段)  ← 独立，无依赖
    │
    ▼
2.1 (truncate_str 统一)     ← 需确认 utils 位置
2.2 (删除 note_error)       ← 依赖 1.3 后的代码状态
2.3 (ModelTier 常量)        ← 独立，无依赖
2.4 (合并 SubAgent)         ← 独立，无依赖
2.5 (拆分 handle_user_input) ← 独立，建议在所有修复后
    │
    ▼
3.1-3.6 (测试补强)          ← 可在第二阶段后独立并行
    │
    ▼
4.1-4.5 (性能与质量)        ← 独立，可在任意阶段执行
    │
    ▼
5.1-5.4 (远期迭代)          ← 按需
```

---

## 影响预估

| 阶段 | 删除行数 | 新增行数 | 净变更 | 风险 |
|:----:|:--------:|:--------:|:------:|:----:|
| 第一阶段 | -221 | +3 | **-218** | 低 |
| 第二阶段 | -40 | +260 | **+220** | 中 |
| 第三阶段 | 0 | +285 | **+285** | 低 |
| 第四阶段 | -70 | +70 | **0** | 低 |
| 第五阶段 | — | — | 按需 | — |
| **合计** | **-331** | **+618** | **+287** | — |

### 关键收益

- **删除死代码**：~330 行
- **消除生产崩溃风险**：2 处（panic + Mutex 中毒）
- **修复功能性 bug**：1 处（fix_loop 误触发）
- **补全测试覆盖**：+285 行，覆盖核心 orchestrator/turn 路径
- **减少编译依赖**：-4 个 Cargo crate
- **代码重复消除**：`truncate_str` × 2
