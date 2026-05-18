# 模型选择器重设计：证明 flash 足够好

> 版本: v2.0
> 日期: 2025-06-18
> 替代: 原 Thompson Sampling 双边比较方案

---

## 一、设计哲学

```
核心思想: flash 是默认选择，pro 是保底方案。
        不需要证明 pro "更好"——需要证明的是 flash "不够好"。

双边比较 (v1):              单边监控 (v2):
  flash vs pro               flash 自己的质量
  需要 pro 的数据 → 死循环    每轮都在产生 flash 数据
  需要探索机制                不需要探索
  需要成本因子                不需要成本因子
```

---

## 二、决策规则

### resolve_active()

```
1. forced_model = Some(t)     → t                    // 手动控制
2. !auto_model_enabled        → config.model          // 自动关闭
3. controller.is_locked()     → Pro                   // 短期停滞
4. flash_quality_triggers()   → Pro                   // 长期质量
5. else                       → Flash                 // 默认
```

### flash_quality_triggers()

```
Q = flash 的 Beta(α, β) 后验均值 = α/(α+β)
N = 总观测次数 = α + β - 2

触发: Q < 0.50  AND  N ≥ 8
```

**为什么不是纯贝叶斯**：方案有两层——底层是贝叶斯（Beta 后验追踪 flash 成功率分布），上层是实用主义决策（硬阈值 Q<0.50, N≥8）。这不是纯粹贝叶斯，但它是正确的工程设计：模型选择本质是成本决策问题，不是推断问题。

### 参数选择理由

```
Q < 0.50: 成功率的"及格线"。flash 必须至少有一半的轮次成功，
         否则不值得用它——即使便宜。

N ≥ 8:   最小样本量。防止 /flash 后第 1 次失败就立刻又升级。
         8 轮足够让 flash 证明自己是否能达到 >50% 成功率。
```

---

## 三、手动降级：/flash

```
用户输入 /flash →
  1. forced_model = None
  2. flash 信念重置为 Beta(3, 3)
     → mean = 0.50（温和先验）
     → N = 4（从 8 轮观察期还剩 4 轮）
  3. controller.reset_stall()
  4. 提示："切回 flash，重置观察记录 (Beta(3,3))。"
```

**Beta(3,3) 而不 Beta(1,1) 的原因**：

```
Beta(1,1): weak prior, 1 次失败就 mean=0.33
Beta(3,3): 等价于已观察 4 轮 (2 成功 2 失败), 需要再失败 2 次才 Q<0.50
          给 flash 缓冲空间, 防止立刻又被证明不够好
```

---

## 四、行为推演

### 4.1 新项目，flash 够好

```
轮次  结果    α   β   Q     N   升级?
  1    ✓      2   1   0.67  1   no
  2    ✓      3   1   0.75  2   no
  3    ✗      3   2   0.60  3   no
  ...  ...
 40    ...   25   10  0.71  33  no  → flash 全程运行
```

### 4.2 新项目，flash 不够好

```
轮次  结果    α   β   Q     N   升级?
  1    ✓      2   1   0.67  1   no
  2    ✗      2   2   0.50  2   no
  3    ✗      2   3   0.40  3   no (N<8)
  4    ✗      2   4   0.33  4   no
  5    ✓      3   4   0.43  5   no
  6    ✗      3   5   0.38  6   no
  7    ✗      3   6   0.33  7   no (N<8)
  8    ✗      3   7   0.30  8   → Pro!
```

第 8 轮触发升级。Controller 的 P(stall) 可能在更早的轮次就触发了（短期停滞），两者互补。

### 4.3 /flash 后再次失败

```
/flash → flash: Beta(3,3), Q=0.50, N=4
  1    ✗     3   4   0.43  5   no (N<8)
  2    ✗     3   5   0.38  6   no
  3    ✓     4   5   0.44  7   no
  4    ✗     4   6   0.40  8   no (Q<0.50 但才刚到 N=8)
  5    ✓     5   6   0.45  9   no (Q 回升了)
  ...

如果 flash 真的不行:
  4    ✗     3   8   0.27  8   → Pro!
```

---

## 五、代码变更

| 文件 | 变更 |
|------|------|
| `model_selector.rs` | 新增 `observations()`, `reset_belief()` |
| `orchestrator.rs` | `resolve_active()` 改为默认 flash + `flash_quality_triggers()` |
| `orchestrator.rs` | `/flash` 调用 `reset_belief("flash", 3, 3)` |
| `test_mock.rs` | 更新模拟逻辑，新增质量触发测试 |

---

## 六、与 Controller 的关系

```
Controller (短期停滞)          ModelSelector (长期质量)

P(stall) = 1 - 0.5^k           Q = α/(α+β)
仅在连续无进展时上升           追踪所有轮的成败
成功即重置                     跨 session 持久化
管"卡住了"                     管"根本行不行"

两者互补：
  Controller: 偶尔网络超时 → P 上升 → 锁 pro → 然后恢复
  Selector:   连续 8 轮 60% 失败 → Q 下降 → 证明 flash 不够好 → 升 pro
```

## 七、对比 v1 (Thompson Sampling)

| 维度 | v1 (TS + ε) | v2 (证明 flash) |
|------|:--:|:--:|
| 默认模型 | TS 采样结果 | flash |
| pro 数据依赖 | 需要 → ε 探索 | 不需要 |
| 参数数量 | 1 (ε) | 0 |
| 探索机制 | ε = 0.05 | 不需要 |
| 降级 | 手动 + 修正 | 手动 + 重置 |
| 缓存友好 | 中（ε 探索可能切换） | 高（极少自动降级） |
| 代码复杂度 | +80 行 | +40 行 |
