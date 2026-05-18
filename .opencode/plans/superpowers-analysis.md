# Superpowers Skill 分析：可借鉴设计模式

> 源项目：superpowers（14 个 skill，3000+ 行）
> 定位：为 opencode / claude code 设计的 LLM 过程控制 skill 集
> 目标：分析哪些模式可内化到 dscode

## 一、Superpowers 的设计哲学

### 1.1 三个核心原则

1. **过程即约束** — 不是给 LLM "建议"，而是给"强制流程"。每一步有明确的进入条件和退出条件。

2. **门控执行** — `<HARD-GATE>` 标记阻止 LLM 跳过关键阶段。示例：
   ```
   <HARD-GATE>
   Do NOT invoke any implementation skill, write any code, scaffold any project
   until you have presented a design and the user has approved it.
   </HARD-GATE>
   ```

3. **模式识别优于关键词匹配** — 用结构化流程（Phase 1→2→3→4）代替启发式规则（"if contains error then fix"）。

### 1.2 核心机制：四阶段过程

Superpowers 的 7 个核心 skill 都使用四阶段 gated 过程：

```
Phase 1: 调查（收集证据、理解问题）
  → Gate: 必须完成调查才能提方案
Phase 2: 分析（找模式、对比参照）
  → Gate: 必须找到根因才能修复
Phase 3: 假设（形成单一假设、最小化测试）
  → Gate: 必须通过测试验证才能修复
Phase 4: 实现（创建测试、单步修复、验证结果）
  → Gate: 必须验证通过才能 claim "done"
```

每个 Phase 有明确的进入门控（"你完成了上一步吗？"）和退出条件（"你能否证明这个结论？"）。

---

## 二、可内化的模式

### 2.1 `<HARD-GATE>` 门控模式（最高优先级）

**概念**：在 system prompt 的特定位置嵌入不可跳过的门控规则。

**Superpowers 示例**：

```
<HARD-GATE>
NO FIXES WITHOUT ROOT CAUSE INVESTIGATION FIRST.
If you haven't completed Phase 1, you cannot propose fixes.
</HARD-GATE>
```

**dscode 可内化**：

当前 dscode 的 system prompt 已有 `plan-lifecycle-guidance` 段（指导 LLM 何时调用 PlanConfirm/PlanClear），但没有门控模式。可以添加：

```
<verification-gate>
BEFORE claiming any work is complete, fixed, or passing:
1. IDENTIFY the verification command
2. RUN it fresh and capture full output
3. READ the output, check exit code
4. STATE the result with evidence
If you skip any step, you are LYING, not verifying.
</verification-gate>
```

**实施方式**：在 `prompt.rs` 的 `build_system_prompt()` 中添加一个 `<verification-gate>` 段，类似于现有的 `<plan-lifecycle-guidance>`。

**代价**：~30 行 prompt 文本。不需要任何 Rust 代码改动。

### 2.2 Red-Flags / STOP 模式（高优先级）

**概念**：列出 LLM 常犯的反模式，要求 LLM 遇到这些情况时立即停止。

**Superpowers 示例**（systematic-debugging）：

```
Red Flags - STOP and Follow Process:

If you catch yourself thinking:
- "Quick fix for now, investigate later"
- "Just try changing X and see if it works"
- "Add multiple changes, run tests"
- "Skip the test, I'll manually verify"
...
ALL of these mean: STOP. Return to Phase 1.
```

**dscode 可内化**：

LLM 在 dscode 中也有常见的反模式。可以在 `rules` 段中添加：

| 反模式 | 后果 | STOP 指令 |
|--------|------|----------|
| "这个 bug 很简单，不需要全部测试" | 引入新 bug | STOP。运行全部测试。 |
| "我改了三处，一起编译" | 无法隔离哪个改动有效 | STOP。一次只改一处。 |
| "exit code 不为 0，但只是 warning" | 忽略真正的错误 | STOP。查看完整输出。 |
| "已修好"（没运行验证） | 谎报完成 | STOP。运行验证命令。 |
| 连续 3 次工具调用得到 Error: | 盲目重试 | STOP。分析根因。 |

**实施方式**：在 `prompt.rs` 的 `rules` 段末尾添加 5-8 条 STOP 规则。纯 prompt 文本，零代码改动。

### 2.3 反模式与合理化表格（中优先级）

**概念**：不直接告诉 LLM"不要做 X"，而是列出"LLM 可能会想 X，但实际上 Y"。

**Superpowers 示例**：

```
| Excuse | Reality |
|--------|---------|
| "Issue is simple, don't need process" | Simple issues have root causes too. |
| "Emergency, no time for process" | Systematic debugging is FASTER than thrashing. |
| "I'll write test after confirming fix works" | Untested fixes don't stick. Test first proves it. |
```

**dscode 可内化**：

```
| 合理化 | 真实情况 |
|--------|---------|
| "这个文件改一处，不需要 Grep 先搜索" | 不搜索就无法保证不遗漏其他调用点。用 Grep。 |
| "只改了两行，直接 commit" | 两行改动能破坏整个构建链。运行 cargo check。 |
| "不用 TodoWrite，任务很简单" | 简单任务也有步骤顺序。写出 checklist 证明。 |
| "我可以同时改三个文件" | 每次只改一个文件，编译验证后再改下一个。 |
```

**实施方式**：添加到 `rules` 段。纯 prompt 文本，零代码改动。

### 2.4 "Iron Law" 铁律模式（中优先级）

**概念**：skill 头部的单行铁律，所有后续规则由此派生。

**Superpowers 示例**：

```
The Iron Law:
NO FIXES WITHOUT ROOT CAUSE INVESTIGATION FIRST

If you haven't completed Phase 1, you cannot propose fixes.
```

```
The Iron Law:
NO COMPLETION CLAIMS WITHOUT FRESH VERIFICATION EVIDENCE

If you haven't run the verification command in this message, you cannot claim it passes.
```

**dscode 可内化**：

```
The Iron Law:
NO CODE CHANGES WITHOUT PRIOR SEARCH AND READ

If you haven't Grep'd for all call sites and Read the surrounding context,
you cannot use Edit.
```

```
The Iron Law:
NO "DONE" WITHOUT PASSING TESTS

If you haven't run the project's test command and seen 0 failures,
you cannot claim work is complete.
```

### 2.5 过程流程图（低优先级）

**概念**：用 Graphviz Dot 格式的状态机图嵌入 skill 文本中，帮助 LLM 理解流程分支。

**Superpowers 示例**：brainstorming skill 包含完整的 `digraph` 流程图。

**dscode 的情况**：当前 system prompt 已有 `plan-lifecycle-guidance` 的文字描述。Dot 流程图增加 token 消耗但对阅读理解有帮助。

**实施方式**：可考虑在 `plan-lifecycle-guidance` 中添加简化的 ASCII 流程图代替当前的纯文本描述。不需要 Dot 格式——LLM 对 ASCII art 理解力相同但 token 消耗更低。

---

## 三、可作为 Skill 内化的完整工作流

### 3.1 系统调试技能（`systematic-debugging`）

**可内化程度**：高。四阶段过程完全通用，不需要任何外部工具。

**dscode 内化方式**：作为内置的 `debugging` skill。

```
skills/debugging/SKILL.md

Phase 1: 收集证据
  - Read 错误输出完整（不跳行）
  - Bash 相同的命令重现问题
  - Glob 查找相似的工作代码
  - Grep 搜索错误消息的历史

Phase 2: 对比分析
  - 找出工作的代码和不工作的代码的差异
  - 列出每条差异

Phase 3: 假设形成
  - 写一句话描述根因假设
  - 最小化修改测试假设

Phase 4: 修复与验证
  - 创建最小复现测试
  - 单步修改
  - 运行原问题和测试
```

**关键差异度**：Superpowers 版本 ~300 行，dscode 版本可压缩到 ~80 行（去掉 git 工作区、外部审查等 dscode 不具备的上下文）。

### 3.2 验证门控技能（`verification-before-completion`）

**可内化程度**：高。单文件、纯文本、无外部依赖。

**dscode 内化方式**：作为内置 skill 或直接嵌入 system prompt `<verification-gate>` 段。

**核心逻辑**：在任一步骤声称"完成"之前强制要求运行验证命令。

### 3.3 TDD 技能（`test-driven-development`）

**可内化程度**：中。对于有测试框架的项目适用，但对于 shell 脚本项目或一次性任务不适用。

**dscode 内化方式**：作为可选 skill（`--skill tdd`）。

**关键差异度**：Superpowers 版本假设项目有完整的测试框架（`pytest`/`jest`/`cargo test`）。dscode 的版本需要更通用——包括"如果没有测试框架怎么验证"。例如：

```
如果项目有 cargo test:
  → 先写最小测试 → 看到它失败 → 实现 → 看到它通过

如果项目是 shell 脚本:
  → 先写验证脚本 → 在干净环境运行 → 看到失败 → 修复 → 验证
```

### 3.4 头脑风暴技能（`brainstorming`）

**可内化程度**：低。适合 opencode/claude code 的持续对话模式。

**不适用的原因**：
- "one question at a time" 需要持续对话——dscode 是单次 prompt 执行
- 设计文档保存到 `docs/superpowers/specs/` — dscode 没有这个约定
- 视觉伴侣（browser-based mockups）— dscode 不具备

**可部分内化部分**：前置检查——"在执行任何代码之前，理解项目上下文、明确需求、检验假设"。可以压缩为 ~15 行的前置规则，添加到 plan-lifecycle-guidance。

---

## 四、Superpowers 不可内化的部分

### 4.1 外部工具依赖

| Skill | 依赖的工具 | dscode 有吗 |
|-------|-----------|:------------:|
| brainstorming | 浏览器视觉伴侣 | ❌ |
| executing-plans | git worktree 创建 | ❌ |
| finishing-a-development-branch | 分支操作、PR 工作流 | ❌ |
| requesting-code-review | PR 系统 | ❌ |
| receiving-code-review | PR 评论解析 | ❌ |

### 4.2 Prompt 模板分发

Superpowers 有 `implementer-prompt.md`、`spec-reviewer-prompt.md` 等作为 subagent 的模板提示词。这些模板包含 Superpowers 特定的角色定义和审查标准。

dscode 的 SubAgent 使用通用的 `prompt` 参数（用户或 LLM 自定义），不需要预设的角色模板。

### 4.3 进度跟踪假设

Superpowers 假设所有任务都有清晰的完成状态（"tests pass"、"PR created"）。dscode 处理的更多样化——包括一次性脚本、探索性搜索、代码理解任务——这些任务没有"通过"或"失败"。

---

## 五、实施路径

### Phase 1: Prompt 级增强（零代码改动）

修改 `prompt.rs` 的 `build_system_prompt()`，添加三个新段：

1. **`<verification-gate>`** — "没有验证就没有完成"铁律
2. **`<stop-triggers>`** — 5-8 条 Red-Flags/STOP 规则
3. **`<rationalization-table>`** — 3-5 对"反模式 vs 真实情况"

代价：~40 行 prompt 文本。编译后立即生效。

### Phase 2: 内置 Skill（创建 SKILL.md）

在 `skills/` 目录下创建两个内置 skill：

1. **`skills/debugging/SKILL.md`** — 压缩版四阶段系统调试流程（~80 行）
2. **`skills/verification/SKILL.md`** — 独立版验证门控（~40 行）

LLM 通过 `Skill(debugging)` 或 `Skill(verification)` 按需加载。不修改 system prompt。

### Phase 3: 可选 Skill

1. **`skills/tdd/SKILL.md`** — 通用 TDD（~60 行，适配有无测试框架）
2. **`skills/pre-code-check/SKILL.md`** — 来自 brainstorming 的前置检查（~20 行）

用户通过 `--skill tdd` 或 `--skill pre-code-check` 选择启用。

### 不做

1. ❌ 不做 prompt 模板分发（实施者/审查者角色模板）
2. ❌ 不做 git 工作区管理（dscode 不管理 git 状态）
3. ❌ 不做浏览器/视觉伴侣集成
4. ❌ 不做外部审查流程（需要 PR 系统）
5. ❌ 不做多 session workflow（brainstorming→writing-plans→subagent-driven-development 跨 session 链）
