# 模型选择器重设计方案：Thompson Sampling + ε-探索

## 一、设计目标

| 目标 | 说明 |
|------|------|
| 贝叶斯一致 | 不做硬阈值决策，所有判断基于概率信念 |
| 小样本保护 | 方差大时自动保守，不因 1-2 次随机失败误判 |
| 防冻结 | 被 unfair 惩罚的模型仍有机会重新证明自己 |
| 缓存友好 | 极少自动降级，避免频繁重建 prefix-cache |
| 用户可控 | 手动 /flash 可降级，修正机制防震荡 |
| 参数极简 | 1 个可调参数（ε），且默认值适配绝大多数场景 |

---

## 二、核心机制

### 2.1 Thompson Sampling（替换 Greedy）

```
当前 Greedy：
  select_greedy() = argmax( α/(α+β) )
  问题：只看点估计，忽略方差

替换为 Thompson Sampling：
  select()：
    s_flash ~ Beta(α_flash, β_flash)
    s_pro   ~ Beta(α_pro,   β_pro)
    return argmax(s_flash, s_pro)
```

**为什么 TS 优于 Greedy**：

| 场景 | Greedy | TS |
|------|:------:|:--:|
| 无数据 (α=1,β=1) | 选 pro（max_by 最后一个等值） | ~50% 选 flash, ~50% 选 pro，**自然探索** |
| flash 1 失败, pro 1 成功 | 选 pro | ~70% 选 pro, ~30% 选 flash（方差大，探索概率高） |
| flash 5 失败, pro 20 成功 | 选 pro | >99% 选 pro（方差小，坚定利用） |
| flash 5 失败, pro 10 成功 | 选 pro | ~95% 选 pro, ~5% 选 flash（仍有小概率探索 flash） |

TS 不需要"最小样本保护"阈值——方差自然提供这个保护。样本越少，分布越宽，越容易在采样时产生"意外"（探索）。样本越多，分布越窄，越坚定。

### 2.2 ε-探索（防止冻结）

```
select():
  if random() < ε:
      return 随机(flash, pro)     // 强制探索，每 20 轮约 1 次
  else:
      return TS 采样结果           // 正常 Thompson Sampling
```

**为什么 TS 还不够**：一旦 flash 积累了大量失败证据，`Beta(1, 20)` 峰值靠近 0，极窄，`s_flash > s_pro` 的概率趋近于 0。此时 TS 自己已经不会再探索 flash。

ε = 0.05 给这个冻结状态一个硬出口。代价是每 20 轮约 1 次强制随机选择——对于 pro 成本 3x flash，这个开销相当于 3/60 ≈ 5% 的成本增加，可忽略。

**ε 探索的逻辑**：
- 如果 flash 确实不行 → 探索轮失败，flash α 不变、β+1 → 下次探索更不可能成功
- 如果 flash 变好了（修了代码/换了任务）→ 探索轮成功 → α 增长 → TS 自己开始偏好 flash → 自然回归

### 2.3 不自动降级（缓存保护）

**当前逻辑不变**——在 `resolve_active()` 中，`controller.is_locked()`（P_stall > 0.80）覆盖 TS 选择，强制 Pro。当 P_stall 降到 0.80 以下后：

```
当前：回到 Greedy 路径，可能立刻切回 flash（缓存重建）
方案：回到 TS 路径，但 TS 大概率仍选 pro（因为 pro 信念更强），
      只有在 ε 探索轮或 flash 积累足够证据时才会自然切回。
```

**效果**：不自动降级不是硬规则，而是 TS 的自然结果——pro 积累了大量成功 → `Beta(pro)` 很窄很高 → TS 几乎总选 pro。不需要额外的"禁止降级"逻辑。

### 2.4 手动 /flash 降级 + 修正

```
用户输入 /flash →
  1. forced_model = None
  2. controller.reset_stall()
  3. 修正 flash 信念：
     flash.α += N     (默认 N=2，追加 2 次幻影成功)
  4. 提示："切回 flash，已给 2 轮观察缓冲"
```

**修正的目的**：flash 可能积累了 N 次失败才触发升级。如果直接切回来，TS 采样 `s_flash ~ Beta(1, N+1)`，几乎必然 < `s_pro`，下一轮又选回 pro。修正 α 后：

```
    修正前: Beta(1, 6)  → mean=0.14  → 几乎不可能被选
    修正后: Beta(3, 6)  → mean=0.33  → 有机会在 TS 中胜出
```

**不是完全重置**：保留 β（失败次数），只修正 α。flash 还是"有劣迹"的，只是给了一个公平的再试机会。如果 flash 真的不行，很快又会积累失败 → β 上升 → 再次被 TS 忽略。

---

## 三、行为推演

### 3.1 新 session，无数据

```
flash: Beta(1,1), pro: Beta(1,1)
TS 采样: ~50%/50%
ε=0.05: 每 20 轮 1 次强制随机
→ 前 20 轮自然积累数据，不预设偏好
```

### 3.2 flash 遭遇随机网络波动

```
第 1 轮: flash 超时 (网络问题) → flash: Beta(1,2), mean=0.33
第 2 轮: TS 采样 s_flash ~ Beta(1,2), s_pro ~ Beta(1,1)
          Beta(1,2) 峰值在 0, 但尾巴能到 0.6
          Beta(1,1) 均匀分布 [0,1]
          → 仍有 ~25% 概率选 flash → 收集更多数据
```

### 3.3 flash 确实不合适

```
10 轮后: flash Beta(1,11), mean=0.08 → TS 几乎不选
Controller: P(stall) 靠传感器信号上升 → is_locked()=true → 强制 Pro
之后: 即使 P(stall) 回落，TS 也锁定 pro（pro 积累了大量成功）
ε 探索偶尔选 flash → 大概率失败 → flash β 继续增加 → 无影响
```

### 3.4 用户修好代码，/flash 切回

```
flash: Beta(1, 11), mean=0.08
/flash → α += 2 → Beta(3, 11), mean=0.21

接下来：
  TS 采样 s_flash ~ Beta(3,11): 峰值 ~0.15, 小概率到 0.4
  s_pro ~ Beta(20, 2): 峰值 ~0.92, 极窄
  → ~90% 仍选 pro（pro 确实好太多）
  → ε 探索或 flash 偶然胜出时 → 如果成功 → α 增长
```

### 3.5 项目后期，flash 完全够用

```
（通过 ε 探索或偶然采样，flash 开始获得成功）
flash: Beta(15, 11), mean=0.58
pro:   Beta(20, 2),  mean=0.91

TS 采样：s_flash 有 ~10% 概率 > s_pro
→ flash 获得更多轮次 → α 继续增长 → TS 概率继续上升
→ 自然切回 flash，无需触发任何阈值
```

---

## 四、参数

| 参数 | 默认值 | 含义 | 配置方式 |
|------|:------:|------|---------|
| ε | 0.05 | 强制探索概率 | env `EXPLORE_EPSILON` 或 config file |
| N_correction | 2 | /flash 后追加幻影成功数 | 硬编码，一般不需要改 |

**为什么是 0.05**：每 20 轮浪费 1 次 = 5% 探索代价。对于 3x 成本差的模型，这个开销完全在可接受范围。

---

## 五、代码变更范围

| 文件 | 变更 | 行数 |
|------|------|:----:|
| `model_selector.rs` | `select_greedy()` → `select_thompson()` + ε 逻辑 | +30 |
| `model_selector.rs` | 新增 `apply_correction(model, n)` 方法 | +5 |
| `orchestrator.rs` | `/flash` 处理中调用 `apply_correction("flash", 2)` | +2 |
| `Cargo.toml` | 无需新依赖（用 `rand` 或简单的 `fastrand`） | +1 |

---

## 六、和 Controller 的关系

```
resolve_active():
  if forced_model = Some(t)  → t              // 手动指定
  if controller.is_locked()  → Pro            // 紧急升级（P_stall > 0.80）
  else                       → TS 选择         // 正常流程

两者不冲突：
  Controller 管"是否卡住了"（短期停滞）
  TS 管"哪个模型更好"（长期偏好）

Controller 的 P_stall 是在单个 session 内累积的，
TS 的信念可以跨 session 持久化（已有 model_beliefs.json）。
```

---

## 七、不需要的机制（显式排除）

| 排除项 | 原因 |
|--------|------|
| 硬阈值 `flash_mean < 0.30 → 升级` | 违背贝叶斯，二元决策 |
| 成本权重 `mean / price` | TS 的采样本质已经在考虑"不确定性下的选择"，加价格扭曲会破坏概率语义 |
| 滞回带 `上去难下来容易` | TS 自己就有滞回——pro 窄分布就是自然滞回 |
| 选项 B 的 `P(pro > flash) > 0.95` | TS 直接比较采样值，比解析 CDF 更简单直观 |
| 最小样本保护 | TS 的方差自然保护小样本，不需要硬计数 |
| "pro 基线"冻结 | 不需要——TS 的 pro 信念会随使用自然增长，也会因失败而衰减 |
