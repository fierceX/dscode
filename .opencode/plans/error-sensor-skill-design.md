# error-sensor Skill 设计稿（深度扩展版）

> 目标：将内置的 `error.sh` 检测脚本可扩展化——启用该 skill 后，LLM 自动分析当前项目，生成贴合项目的自定义 `error.sh`，并替换内置的通用检测。

---

## 目录

1. [现存问题与设计目标](#一现存问题与设计目标)
2. [总体架构](#二总体架构)
3. [SKILL.md 深度设计](#三skillmd-深度设计)
4. [传感器框架扩展规格](#四传感器框架扩展规格)
5. [项目分析引擎详解](#五项目分析引擎详解)
6. [生成模板系统](#六生成模板系统)
7. [LLM 行为规范与提示词设计](#七llm-行为规范与提示词设计)
8. [用户交互模型](#八用户交互模型)
9. [安全模型](#九安全模型)
10. [测试策略](#十测试策略)
11. [与现有系统集成](#十一与现有系统集成)
12. [暂不实现的功能（远期规划）](#十二暂不实现的功能远期规划)
13. [实施步骤与依赖关系](#十三实施步骤与依赖关系)
14. [附录：完整生成示例](#十四附录完整生成示例)

---

## 一、现存问题与设计目标

### 1.1 当前架构的局限

```
当前链路：

include_str!("assets/sensors/error.sh")
  → 写入临时目录 /tmp/dscode-sensors-<PID>/error.sh
  → 每次工具调用执行
  → 丢弃结果（或喂入 Controller）

问题：
  • 脚本在二进制中，用户不可修改
  • 只认识 3 种模式：Rust 编译错误、pytest 失败、非零退出码
  • 没有项目上下文：不知道项目是 Go/JS/Python，不知道用哪个测试框架
  • 每次启动写入 temp dir，$TMPDIR 可能被清理
```

### 1.2 设计目标

| 目标 | 优先级 | 说明 |
|------|:------:|------|
| 用户可扩展错误检测模式 | P0 | 项目级配置文件覆盖内置 |
| 项目感知 | P0 | 自动检测语言/框架/构建系统，生成针对性规则 |
| 内置回退 | P0 | 任何情况下都不比当前更差 |
| 零配置启用 | P1 | `--skill error-sensor` 即可工作 |
| 向前兼容 | P0 | 不启用 skill 时行为完全不变 |
| 可调试 | P1 | 用户可查看当前生效的 error.sh 内容 |

---

## 二、总体架构

### 2.1 分层查找架构

```
                          run_sensor("error", ...)
                                   │
                                   ▼
                    ┌──────────────────────────────┐
                    │      Sensor 查找解析器         │
                    │  (sensor_resolve_path)        │
                    └──────────────┬───────────────┘
                                   │
              ┌────────────────────┼────────────────────┐
              ▼                    ▼                    ▼
    ┌─────────────────┐  ┌─────────────────┐  ┌─────────────────┐
    │  ① Project 级   │  │  ② User 级     │  │  ③ 内置回退    │
    │  .dscode/sensors │  │  ~/.dscode/     │  │  include_str!   │
    │  /error.sh       │  │  sensors/       │  │  → temp dir     │
    │                  │  │  error.sh       │  │                 │
    │  由 Skill 生成   │  │  用户手动放置   │  │  编译时固化     │
    └─────────────────┘  └─────────────────┘  └─────────────────┘
              │                    │                    │
              └────────────┬───────┘────────────────────┘
                           ▼
              成功 → 执行并返回信号
              失败 → 降级到下一优先级
```

### 2.2 Skill 激活上下文

```
启用 --skill error-sensor 后，LLM 系统提示词中追加本 skill。
Skill 的执行时机：

  首次工具调用前（ensure_prefix 构建时）
       │
       ▼
  ensure_sensor_dir() 被调用
       │
       ▼
  检查 .dscode/sensors/error.sh 是否存在
       │
       ├── 存在且内容与内置不同 → 跳过（用户/之前已生成）
       │
       └── 不存在或内容与内置相同 → 触发分析→生成流程
             │
             ▼
        LLM 执行 skill 指示的分析流程
```

### 2.3 核心变更文件清单

| 文件 | 变更类型 | 说明 |
|------|:--------:|------|
| `skills/error-sensor/SKILL.md` | 新建 | 定义 LLM 的分析和生成行为 |
| `src/guard/sensor.rs` | 修改 | 添加多路径查找逻辑 |
| `src/guard/mod.rs` | 不变 | 无需新增模块声明 |
| `assets/sensors/error.sh` | 不变 | 作为内置回退保留 |
| `src/agent/orchestrator.rs` | 不变 | 信号链路无需改动 |
| `src/lib.rs` | 不变 | build.rs 自动嵌入新 skill |
| `build.rs` | 不变 | 已支持自动嵌入 `skills/` 下的所有 skill |

---

## 三、SKILL.md 深度设计

### 3.1 完整结构

```markdown
---
name: error-sensor
description: >-
  Analyzes project structure and generates a project-specific error.sh
  sensor script for more accurate tool-error detection. Replaces the
  built-in generic sensor with project-tailored patterns.
---

# error-sensor Skill

## 激活条件

- 用户通过 `--skill error-sensor` 启用本 skill
- 在每次新 session 的首次工具调用前自动触发
- 如果 `error.sh` 已存在且不是内置版本的拷贝，**跳过**生成（保留用户自定义）

## 工作流程总览

```
[项目分析] → [生成模板选择] → [error.sh 生成] → [语法验证] → [写入] → [确认]
     │               │               │               │          │
     ▼               ▼               ▼               ▼          ▼
  Glob+Read    规则决策树       填充模板        bash -n     .dscode/
  收集证据     选择检测模式     生成完整脚本    语法检查    sensors/
```

---

### Step 1：项目分析

#### 1.1 语言检测

按优先级依次探测（使用 Glob），一旦命中即停止：

```
优先级 语言      探测文件
──────────────────────────────────────
 1     Rust      **/Cargo.toml          (排除 target/ 目录)
 2     Python    **/setup.py, **/pyproject.toml, **/requirements.txt
 3     Go        **/go.mod
 4     Java      **/pom.xml, **/build.gradle, **/build.gradle.kts
 5     Node.js   **/package.json
 6     Ruby      **/Gemfile
 7     PHP       **/composer.json
 8     C/C++     **/CMakeLists.txt, **/Makefile
 9     Shell     回退：无以上任何文件，但有 .sh 文件
10     未知      回退：使用通用检测模式
```

**冲突解决规则**：
- 如果多个语言的文件同时存在（如 monorepo），**选择文件最多的语言**作为主语言，其他语言作为辅语言各生成额外规则
- monorepo 判断：根目录 + 子目录各有独立的构建文件 → 多个语言规则合并

#### 1.2 构建系统检测

```
探测对象              判定结果
────────────────────────────────
Cargo.toml            cargo build/cargo test
package.json +        查看 scripts.test 字段
  "jest" in devDeps   → jest
  "vitest"            → vitest
  "mocha"             → mocha
Makefile              make (通用)
justfile              just (通用)
```

#### 1.3 测试框架检测

对每个检测到的语言，进一步探测测试框架：

```
语言     探测方法                    框架
──────  ─────────────────────────  ───────────
Rust    Cargo.toml 中 dev-deps     cargo test (内置)
        或 [[test]] sections       
Python  glob **/conftest.py       pytest
        **/pytest.ini             pytest
        tox.ini + [testenv]       tox+pytest
Go      **/*_test.go 存在         go test
Node    package.json scripts     jest/vitest/mocha
        **/jest.config.*          jest
        **/vitest.config.*        vitest
```

#### 1.4 错误格式预采样

通过 Glob 查找项目中的历史错误输出或 CI 日志（可选优化）：

```
查找路径：
  **/error.log
  **/ci/*.log
  .dscode/error-samples/

读取前 5 个文件，提取常见的错误前缀模式。
```

> **注意**：此步骤是性能优化而非必需。如果未找到样本文件，跳过即可。
> 大项目中此步可能耗时，应在执行时设定合理的时间预算。

---

### Step 2：模板选择

根据分析结果选择以下维度的组合：

```
语言层模板（多选）：
  □ rust (cargo)
  □ python (pytest / unittest)
  □ node (jest / vitest / mocha)
  □ go (go test)
  □ java (maven / gradle)
  □ generic（始终选中）

框架层模板（多选）：
  □ cargo-test      □ pytest      □ jest
  □ vitest          □ go-test     □ maven
  □ gradle          □ unittest

增强层模板（多选）：
  □ docker     → 检测容器相关错误
  □ ci-env     → 检测 CI 环境变量缺失错误
  □ network    → 检测网络超时/DNS 错误
  □ filesystem → 检测权限/磁盘空间错误
```

#### 决策矩阵

```
主语言    测试框架    选中的模板
──────────────────────────────────────
Rust      cargo-test  rust + cargo-test + generic
Python    pytest      python + pytest + generic
Python    unittest    python + unittest + generic
Node      jest        node + jest + generic
Node      vitest      node + vitest + generic
Go        go-test     go + go-test + generic
Java      maven       java + maven + generic
Java      gradle      java + gradle + generic
未知      未知        generic（仅基础检测）

增强层判断：
  Dockerfile 存在       → +docker
  .github/ 或 .gitlab-ci.yml → +ci-env
  项目有外部 API 调用   → +network
```

---

### Step 3：生成 error.sh

#### 3.1 输出文件结构

```bash
#!/bin/bash
# ================================================================
# Project-specific error sensor
# Generated by error-sensor skill
# Project: <project-name>
# Language: <detected-language>
# Framework: <detected-framework>
# Generated at: <timestamp>
# Project fingerprint: <git-root-hash>
# ================================================================

tool="$1"
elapsed_ms="$2"
output_len="$3"
output=$(cat)
signals=""

# ================================================================
# <GENERATED: language-specific patterns>
# ================================================================

# --- Rust compilation errors ---
echo "$output" | grep -qi "error\[E" && \
  signals+="{\"kind\":\"tool_error\",\"weight\":1.0,\"detail\":\"Rust compilation error\"},"

# ... 更多规则 ...

# ================================================================
# <GENERATED: framework-specific patterns>
# ================================================================

# --- cargo test failures ---
echo "$output" | grep -qE "^test .+ FAILED" && \
  signals+="{\"kind\":\"tool_error\",\"weight\":1.0,\"detail\":\"Cargo test failure\"},"

# ================================================================
# <GENERATED: generic fallback patterns>
# ================================================================

# --- Non-zero exit ---
echo "$output" | grep -qE "exit code [1-9]|^Error:" && \
  signals+="{\"kind\":\"tool_error\",\"weight\":0.5,\"detail\":\"Non-zero exit\"},"

# ================================================================
# Output
# ================================================================
if [ -n "$signals" ]; then
  echo "{\"signals\":[${signals%,}]}"
else
  echo "{}"
fi
```

#### 3.2 每条规则的元契约

LLM 在生成每一条 `grep` 规则时必须遵循以下元规则：

```markdown
每一条检测规则 = { pattern, weight, detail, rationale }

  pattern  必须是 bash 正则（grep -E 兼容）
  weight   取 {0.3, 0.5, 0.8, 1.0} 四档
           1.0 = 确定性错误（编译失败、断言失败）
           0.8 = 高可能性错误（异常堆栈、panic）
           0.5 = 可疑错误（非零退出码）
           0.3 = 低置信度警告（超时、资源不足）
  detail   必须是可读的简短描述（用于日志和调试）
  rationale 仅在生成时内部使用，不出现在脚本中

规则去重要求：
  如果两条规则的 pattern 匹配同一类错误，保留 weight 更高的一条。
  例如 "error[E" 和 "error: aborting" 都匹配 Rust 编译错误，但
  "error[E" 更精确，保留它。
```

---

### Step 4：语法验证

```markdown
写入前必须执行以下验证：

1. bash -n <file>        # 检查 shell 语法
   → 如果失败，修复语法错误后重试，最多 3 次
   → 3 次后仍失败，回退到内置 error.sh 并报告用户

2. 模拟执行（可选）:
   echo "test" | bash <file> "mock_tool" "100" "4"
   → 确认输出是合法 JSON
   → 确认 exit code = 0
```

### Step 5：写入文件

```markdown
写入路径：<project>/.dscode/sensors/error.sh

步骤：
1. 确保 <project>/.dscode/sensors/ 目录存在
   → 不存在则创建（mkdir -p）
2. 写入 error.sh
3. 设置可执行权限（chmod +x）
4. 验证可被 bash 执行（bash -n 再次确认）
5. 通知用户：已生成项目专属 error.sh
```

---

### 重新生成触发器

LLM 应在以下时机判断是否需要重新生成：

| 时机 | LLM 判断逻辑 | 动作 |
|------|-------------|------|
| 用户显式要求 | "重新生成 error.sh"、"更新传感器" 等关键词 | 无条件重新分析生成 |
| 项目新增构建文件 | 用户创建了 `package.json` / `Cargo.toml` 等 | 提示用户是否重新生成 |
| 新增测试框架 | 用户添加了 `jest.config.js` / `pytest.ini` | 提示用户是否重新生成 |
| 用户手动修改 error.sh | 校验注释中的 fingerprint 与当前项目不匹配 | 保留用户版本（不覆盖） |
```

### 3.2 SKILL.md 的约束规则（反事实引导）

SKILL.md 末尾应包含以下约束，防止 LLM 做出不安全行为：

```markdown
## 约束规则

IMPORTANT - 生成 error.sh 时的安全边界：

1. 不要修改或删除 `output=$(cat)` 行 — 这是 stdin 输入的唯一入口
2. 不要修改 JSON 输出格式 — 必须保持 `{"signals":[...]}` 或 `{}`
3. 不要写入除 `<project>/.dscode/sensors/` 以外的路径
4. 不要包含 `rm`, `kill`, `exec`, `eval` 等危险命令
5. 不要读取除 stdin 以外的文件
6. 不要执行除 grep/awk/sed 以外的外部命令
7. 所有 grep 必须转义特殊字符，防止注入
8. 生成后执行两次验证：bash -n + 模拟运行
```

---

## 四、传感器框架扩展规格

### 4.1 `sensor_resolve_path` 函数设计

```rust
/// 解析传感器脚本的完整查找路径。
///
/// 查找顺序（高优先级优先）：
///   1. <project>/.dscode/sensors/<name>.sh
///   2. <home>/.dscode/sensors/<name>.sh
///   3. 内置回退（临时目录）
///
/// 返回 (路径, 来源标签) 二元组。
/// 来源标签用于日志和调试。
pub fn sensor_resolve_path(
    ctx: &AgentSharedContext,
    name: &str,
) -> (PathBuf, SensorSource)
```

```rust
/// 传感器来源标签
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SensorSource {
    /// <project>/.dscode/sensors/<name>.sh
    Project,
    /// <home>/.dscode/sensors/<name>.sh
    User,
    /// 内置（编译时嵌入）
    Builtin,
}
```

#### 路径解析实现细节

```
sensor_resolve_path(ctx, "error")
  │
  ├─ 1. project_path = ctx.cwd.join(".dscode/sensors/error.sh")
  │    → 存在且可执行 → 返回 (project_path, Project)
  │
  ├─ 2. user_path = ctx.home.join(".dscode/sensors/error.sh")
  │    → 存在且可执行 → 返回 (user_path, User)
  │
  └─ 3. 回退到内置
       → ensure_builtin_sensor_dir() 确保内置脚本解压到 temp dir
       → 返回 (temp_dir/error.sh, Builtin)
```

### 4.2 `run_sensor` 修改点

```rust
/// 修改前：仅从 temp dir 查找
pub fn run_sensor(name, tool_name, elapsed_ms, output_len, output) -> Result<Vec<SensorSignal>> {
    let dir = ensure_sensor_dir()?;  // 仅 temp dir
    let script_path = dir.join(format!("{name}.sh"));
    // ... 执行 ...
}

/// 修改后：按优先级查找
pub fn run_sensor(name, tool_name, elapsed_ms, output_len, output) -> Result<Vec<SensorSignal>> {
    let (script_path, source) = sensor_resolve_path(ctx, name);
    // 记录日志：使用哪个来源的传感器
    log::debug!("run_sensor: {name} resolved from {source:?} → {}", script_path.display());
    // ... 执行 ...
}
```

#### 需要新增的参数

`run_sensor()` 需要接收 `ctx` 或 `cwd`/`home` 参数来构建查找路径。当前 `run_sensor` 不接收 context，需要新增：

```rust
// 当前签名（无 context，仅从 temp dir）
pub fn run_sensor(
    name: &str,
    tool_name: &str,
    elapsed_ms: u64,
    output_len: usize,
    output: &str,
) -> anyhow::Result<Vec<SensorSignal>>

// 修改后签名（新增 ctx 参数）
pub fn run_sensor(
    ctx: &AgentSharedContext,   // ← 新增，用于路径查找
    name: &str,
    tool_name: &str,
    elapsed_ms: u64,
    output_len: usize,
    output: &str,
) -> anyhow::Result<Vec<SensorSignal>>
```

### 4.3 热重载策略（暂不实现，记录设计）

```
远期目标：运行时监测文件变化，自动切换。

方案：
  在每个 tool call 前 stat 文件 mtime。
  如果 mtime 变化 → 重新加载脚本内容。
  如果文件消失 → 降级到下一优先级。

暂缓原因：
  stat 增加每次 tool call 的延迟（约 0.1ms），
  且用户极少在 session 中修改 error.sh。
  改为：每次 session 启动时检测一次，之后不重载。
```

### 4.4 缓存与性能

```
─ session 启动时：
   └─ sensor_resolve_path() 搜索一次，结果缓存到 run_sensor 闭包中

─ 每次 tool call：
   └─ 使用缓存的路径，不重复搜索

─ 路径不变时：
   └─ 零额外开销

─ 内存占用：
   └─ 缓存 ~200 字节（路径字符串 + 来源标签 + 脚本内容）
```

---

## 五、项目分析引擎详解

### 5.1 探测算法

#### Glob 规则

```json
{
  "language_probes": {
    "rust": {
      "files": ["**/Cargo.toml"],
      "exclude": ["**/target/**"],
      "priority": 1,
      "min_match": 1
    },
    "python": {
      "files": ["**/setup.py", "**/pyproject.toml", "**/requirements.txt"],
      "exclude": ["**/node_modules/**", "**/.venv/**", "**/venv/**"],
      "priority": 2,
      "min_match": 1
    },
    "go": {
      "files": ["**/go.mod"],
      "exclude": [],
      "priority": 3,
      "min_match": 1
    },
    "node": {
      "files": ["**/package.json"],
      "exclude": ["**/node_modules/**"],
      "priority": 4,
      "min_match": 1
    },
    "java": {
      "files": ["**/pom.xml", "**/build.gradle", "**/build.gradle.kts"],
      "exclude": [],
      "priority": 5,
      "min_match": 1
    }
  },
  "max_probe_depth": 3,
  "max_probe_files": 50
}
```

#### 单语言 vs 多语言冲突处理

```python
# 伪代码展示冲突解决逻辑
detected_languages = []

for probe in language_probes:
    matches = glob(probe.files, exclude=probe.exclude)
    if len(matches) >= probe.min_match:
        detected_languages.append({
            "language": probe,
            "match_count": len(matches),
            "locations": [dirname(m) for m in matches]
        })

if len(detected_languages) == 1:
    primary = detected_languages[0]
    secondary = []
elif len(detected_languages) > 1:
    # 按 match_count 降序
    detected_languages.sort(key=lambda x: x.match_count, reverse=True)
    primary = detected_languages[0]
    secondary = detected_languages[1:]

    # 如果 match_count 差异 < 20%，视为 monorepo，全部作为辅语言
    if primary.match_count - secondary[0].match_count < primary.match_count * 0.2:
        secondary = detected_languages[1:]  # 全部保留
else:
    primary = {"language": "generic"}
    secondary = []
```

### 5.2 测试框架探测

```python
# 伪代码
def detect_framework(primary_lang, locations):
    if primary_lang == "rust":
        # 检查 Cargo.toml 中是否定义了 test 配置
        for loc in locations:
            cargo = read_file(loc / "Cargo.toml")
            if "[[test]]" in cargo or "[dev-dependencies]" in cargo:
                return ["cargo-test"]
        return ["cargo-test"]  # 即使无配置，cargo test 仍可用

    elif primary_lang == "python":
        frameworks = []
        for loc in locations:
            if exists(loc / "pytest.ini") or exists(loc / "conftest.py"):
                frameworks.append("pytest")
            if exists(loc / "tox.ini"):
                frameworks.append("tox")
        if not frameworks:
            frameworks.append("unittest")  # Python 内置
        return frameworks

    elif primary_lang == "node":
        frameworks = []
        for loc in locations:
            pkg = read_json(loc / "package.json")
            if pkg:
                deps = {**pkg.get("dependencies", {}), **pkg.get("devDependencies", {})}
                if "jest" in deps:
                    frameworks.append("jest")
                if "vitest" in deps:
                    frameworks.append("vitest")
                if "mocha" in deps:
                    frameworks.append("mocha")
            if exists(loc / "jest.config.*"):
                frameworks.append("jest")
            if exists(loc / "vitest.config.*"):
                frameworks.append("vitest")
        return frameworks

    # ... 其他语言类似
```

### 5.3 分析结果缓存

```markdown
项目分析是 Glob 密集操作，应避免重复执行。

缓存策略：
  • 分析结果写入 <project>/.dscode/sensors/.analysis-cache.json
  • 缓存包含：项目指纹（git HEAD hash）、语言、框架、构建系统、时间戳
  • 下次激活时比较指纹，相同则跳过分析
  • 指纹变更（git commit / checkout）时重新分析

    .dscode/sensors/
    ├── error.sh                ← 生成的传感器脚本
    ├── .analysis-cache.json    ← 分析结果缓存
    └── .gitignore
```

---

## 六、生成模板系统

### 6.1 完整检测模式库

#### Rust 专属（权重 1.0）

```bash
# Rust compilation errors
echo "$output" | grep -qi "error\[E" && \
  signals+="{\"kind\":\"tool_error\",\"weight\":1.0,\"detail\":\"Rust compilation error\"},"
echo "$output" | grep -qi "error: aborting" && \
  signals+="{\"kind\":\"tool_error\",\"weight\":1.0,\"detail\":\"Rust build aborted\"},"
echo "$output" | grep -qi "could not compile" && \
  signals+="{\"kind\":\"tool_error\",\"weight\":1.0,\"detail\":\"Cargo build failure\"},"
echo "$output" | grep -qiE "^error\[E\d+\]" && \
  signals+="{\"kind\":\"tool_error\",\"weight\":1.0,\"detail\":\"Rust error code\"},"

# Rust warnings (lower weight)
echo "$output" | grep -qi "warning\[W" && \
  signals+="{\"kind\":\"tool_error\",\"weight\":0.3,\"detail\":\"Rust warning\"},"
```

#### Python 专属（权重 1.0/0.8）

```bash
# Python exceptions
echo "$output" | grep -qi "Traceback (most recent call last)" && \
  signals+="{\"kind\":\"tool_error\",\"weight\":1.0,\"detail\":\"Python exception\"},"
echo "$output" | grep -qi "SyntaxError:" && \
  signals+="{\"kind\":\"tool_error\",\"weight\":1.0,\"detail\":\"Python syntax error\"},"
echo "$output" | grep -qi "ModuleNotFoundError:" && \
  signals+="{\"kind\":\"tool_error\",\"weight\":0.8,\"detail\":\"Python module not found\"},"
echo "$output" | grep -qi "ImportError:" && \
  signals+="{\"kind\":\"tool_error\",\"weight\":0.8,\"detail\":\"Python import error\"},"
echo "$output" | grep -qi "IndentationError:" && \
  signals+="{\"kind\":\"tool_error\",\"weight\":1.0,\"detail\":\"Python indentation error\"},"

# pytest framework
echo "$output" | grep -qE "FAILED [a-zA-Z0-9_/]+\.py::" && \
  signals+="{\"kind\":\"tool_error\",\"weight\":1.0,\"detail\":\"Pytest failure\"},"
echo "$output" | grep -qE "ERROR collecting" && \
  signals+="{\"kind\":\"tool_error\",\"weight\":1.0,\"detail\":\"Pytest collection error\"},"
```

#### Node.js 专属（权重 1.0/0.8）

```bash
# JavaScript runtime errors
echo "$output" | grep -qi "ReferenceError:" && \
  signals+="{\"kind\":\"tool_error\",\"weight\":1.0,\"detail\":\"JS ReferenceError\"},"
echo "$output" | grep -qi "TypeError:" && \
  signals+="{\"kind\":\"tool_error\",\"weight\":1.0,\"detail\":\"JS TypeError\"},"
echo "$output" | grep -qi "SyntaxError:" && \
  signals+="{\"kind\":\"tool_error\",\"weight\":1.0,\"detail\":\"JS SyntaxError\"},"
echo "$output" | grep -qi "Cannot find module" && \
  signals+="{\"kind\":\"tool_error\",\"weight\":0.8,\"detail\":\"JS module not found\"},"

# jest framework
echo "$output" | grep -qE "FAIL .+\.test\.(js|ts|jsx|tsx)" && \
  signals+="{\"kind\":\"tool_error\",\"weight\":1.0,\"detail\":\"Jest test failure\"},"
echo "$output" | grep -qi "expect(received).toBe(expected)" && \
  signals+="{\"kind\":\"tool_error\",\"weight\":1.0,\"detail\":\"Jest assertion failed\"},"

# vitest framework
echo "$output" | grep -qE "FAIL .+\.spec\.(js|ts)" && \
  signals+="{\"kind\":\"tool_error\",\"weight\":1.0,\"detail\":\"Vitest test failure\"},"
```

#### Go 专属（权重 1.0）

```bash
# Go compilation errors
echo "$output" | grep -qi "undefined:" && \
  signals+="{\"kind\":\"tool_error\",\"weight\":1.0,\"detail\":\"Go undefined reference\"},"
echo "$output" | grep -qi "cannot use" && \
  signals+="{\"kind\":\"tool_error\",\"weight\":1.0,\"detail\":\"Go type mismatch\"},"
echo "$output" | grep -qi "syntax error" && \
  signals+="{\"kind\":\"tool_error\",\"weight\":1.0,\"detail\":\"Go syntax error\"},"

# go test framework
echo "$output" | grep -qE "--- FAIL:" && \
  signals+="{\"kind\":\"tool_error\",\"weight\":1.0,\"detail\":\"Go test failure\"},"
echo "$output" | grep -qi "FAIL" && \
  signals+="{\"kind\":\"tool_error\",\"weight\":0.5,\"detail\":\"Go test suite failed\"},"
```

#### Java 专属（权重 1.0）

```bash
# Java compilation errors
echo "$output" | grep -qi "error: cannot find symbol" && \
  signals+="{\"kind\":\"tool_error\",\"weight\":1.0,\"detail\":\"Java symbol not found\"},"
echo "$output" | grep -qi "Exception in thread" && \
  signals+="{\"kind\":\"tool_error\",\"weight\":1.0,\"detail\":\"Java exception\"},"
echo "$output" | grep -qi "BUILD FAILED" && \
  signals+="{\"kind\":\"tool_error\",\"weight\":1.0,\"detail\":\"Java build failure\"},"

# Maven / Gradle
echo "$output" | grep -qi "BUILD FAILURE" && \
  signals+="{\"kind\":\"tool_error\",\"weight\":1.0,\"detail\":\"Maven build failure\"},"
echo "$output" | grep -qi "Task .+ failed" && \
  signals+="{\"kind\":\"tool_error\",\"weight\":1.0,\"detail\":\"Gradle task failure\"},"
```

#### 增强层——Docker

```bash
echo "$output" | grep -qi "container.*exited" && \
  signals+="{\"kind\":\"tool_error\",\"weight\":0.8,\"detail\":\"Docker container exited\"},"
echo "$output" | grep -qi "Cannot connect to the Docker daemon" && \
  signals+="{\"kind\":\"tool_error\",\"weight\":1.0,\"detail\":\"Docker daemon not running\"},"
echo "$output" | grep -qi "image.*not found" && \
  signals+="{\"kind\":\"tool_error\",\"weight\":0.8,\"detail\":\"Docker image not found\"},"
```

#### 增强层——网络

```bash
echo "$output" | grep -qi "Connection timed out" && \
  signals+="{\"kind\":\"tool_error\",\"weight\":0.8,\"detail\":\"Network timeout\"},"
echo "$output" | grep -qi "Connection refused" && \
  signals+="{\"kind\":\"tool_error\",\"weight\":0.8,\"detail\":\"Connection refused\"},"
echo "$output" | grep -qi "Could not resolve host" && \
  signals+="{\"kind\":\"tool_error\",\"weight\":0.8,\"detail\":\"DNS resolution failed\"},"
echo "$output" | grep -qi "TLS handshake" && \
  signals+="{\"kind\":\"tool_error\",\"weight\":0.8,\"detail\":\"TLS error\"},"
```

#### 增强层——文件系统

```bash
echo "$output" | grep -qi "Permission denied" && \
  signals+="{\"kind\":\"tool_error\",\"weight\":0.8,\"detail\":\"Permission denied\"},"
echo "$output" | grep -qi "No space left on device" && \
  signals+="{\"kind\":\"tool_error\",\"weight\":1.0,\"detail\":\"Disk full\"},"
echo "$output" | grep -qi "No such file or directory" && \
  signals+="{\"kind\":\"tool_error\",\"weight\":0.8,\"detail\":\"File not found\"},"
echo "$output" | grep -qi "Is a directory" && \
  signals+="{\"kind\":\"tool_error\",\"weight\":0.5,\"detail\":\"Is a directory\"},"
```

#### 通用回退（始终生成）

```bash
# Non-zero exit codes
echo "$output" | grep -qE "exit code [1-9]|^Error:" && \
  signals+="{\"kind\":\"tool_error\",\"weight\":0.5,\"detail\":\"Non-zero exit\"},"

# Timeout / signal
echo "$output" | grep -qi "killed\|SIGTERM\|SIGKILL\|timed out" && \
  signals+="{\"kind\":\"tool_error\",\"weight\":0.8,\"detail\":\"Process killed or timed out\"},"

# Memory
echo "$output" | grep -qi "out of memory\|OOM\|Allocation failure" && \
  signals+="{\"kind\":\"tool_error\",\"weight\":1.0,\"detail\":\"Out of memory\"},"
```

### 6.2 规则去重算法

```
相同 detail 前缀的规则不能同时出现在生成的脚本中。

去重规则：
  1. 按 detail 分组（取 ":" 和 "(" 之前的部分作为 group key）
  2. 每组保留 weight 最高的规则
  3. 如果 weight 相同，保留 pattern 更长的（更精确）

示例：
  detail="Rust compilation error"   group key="Rust compilation error"
  detail="Rust build aborted"       group key="Rust build aborted"
  detail="Rust error code"          group key="Rust error code"
  → 三条独立，无需去重

  pattern匹配 "error[E" 和 "error\\[E\\d+\\]"
  都对应 Rust 编译错误
  → 保留 pattern 更长的 "error\\[E\\d+\\]"
```

---

## 七、LLM 行为规范与提示词设计

### 7.1 Skill 在系统提示词中的定位

```
在系统提示词中，error-sensor skill 被插入在 <selected-skills> 段。
与其他 skill（debugging、tdd、verification）同级。

当激活时：
  1. 从 skill-index 中可查到 error-sensor 技能
  2. LLM 首次工具调用前会扫描选中的 skills
  3. 根据 SKILL.md 的指示，在执行第一阶段前先完成项目分析
```

### 7.2 LLM 决策树

LLM 在执行 error-sensor skill 时的内部决策流程：

```
IF (当前 session 尚未生成 error.sh)
  OR (用户显式要求重新生成)
  OR (检测到项目变更 → git diff --name-only 显示新增构建文件):
  
  THEN:
    Step 1: 分析项目（Glob + Read）
    Step 2: 根据分析结果选择模板
    Step 3: 生成 error.sh 内容
    Step 4: bash -n 验证语法
    Step 5: 写入 .dscode/sensors/error.sh
    Step 6: 通知用户生成完成

ELSE:
  → 跳过，使用已有的 error.sh
```

### 7.3 对话引导示例

用户首次启用 skill 时的 LLM 输出预期：

```
[info] error-sensor: 正在分析项目...
  → 检测到 Rust 项目 (Cargo.toml)
  → 检测到测试框架: cargo test
  → 生成项目专属 error.sh...
  → bash -n 验证通过
  → 已写入 .dscode/sensors/error.sh
[info] error-sensor: 完成。检测模式包含:
   • Rust 编译错误
   • Cargo 测试失败
   • Python 异常 (辅助检测)
   • 通用非零退出码
```

用户修改项目结构后重新生成的预期：

```
[info] error-sensor: 检测到项目变更 (新增 jest.config.js)
  → 重新分析项目...
  → 更新 error.sh，新增 jest 检测模式
  → 已写入 .dscode/sensors/error.sh
```

---

## 八、用户交互模型

### 8.1 交互方式

| 方式 | 示例 | 说明 |
|------|------|------|
| CLI 启用 | `dscode --skill error-sensor` | 启动时加载 skill |
| 对话触发 | "重新生成 error.sh" | LLM 识别意图后执行 |
| 状态查询 | "查看当前 error.sh" | LLM 读取并显示内容 |
| / 命令 | `/sensor-status` | 暂不实现，远期规划 |

### 8.2 状态查询

LLM 应能回答以下用户问题：

```
Q: "当前 error.sh 是什么状态？"
A: "项目专属 error.sh 已就绪。
    来源: .dscode/sensors/error.sh (project-level)
    生成时间: 2025-06-18 12:34
    检测模式: 8 条规则
    上次触发: 3 次 tool_error 信号 (Bash 工具)"

Q: "error.sh 检测到了什么？"
A: "在当前 session 中，error.sh 共触发了 5 次：
     • Rust compilation error × 2 (Bash 工具)
     • Cargo test failure × 2 (Bash 工具)
     • Non-zero exit × 1 (Bash 工具)
     这些信号已传递到 Controller，影响模型切换决策。"

Q: "我想添加自定义规则"
A: "请编辑 .dscode/sensors/error.sh，添加新的 grep 规则。
    注意保持 JSON 输出格式不变。
    编辑后运行 bash -n .dscode/sensors/error.sh 验证语法。"
```

### 8.3 错误处理与用户反馈

| 场景 | LLM 行为 |
|------|---------|
| 分析阶段 Glob 失败 | 降级到纯通用模板，通知用户 |
| bash -n 验证失败 | 重试最多 3 次，失败则保留内置脚本 |
| 写入权限不足 | 尝试 `~/.dscode/sensors/` 作为回退 |
| 用户手动编辑后请求"重新生成" | 先询问是否保留用户修改，选择"是"则跳过生成 |
| 项目路径包含空格 | shell 脚本中正确引用路径 |
| 大项目（>1000 文件） | 限制 Glob 深度为 3 层，文件数上限 50 |

---

## 九、安全模型

### 9.1 生成的脚本安全约束

LLM 生成 error.sh 时受到 SKILL.md 中的约束：

```
✓ grep - 允许（只读模式匹配）
✓ awk - 允许（文本处理，仅模式匹配模式）
✓ sed - 限制为 's/pattern/replacement/' 格式，不执行脚本
✗ rm - 禁止
✗ kill - 禁止
✗ exec/eval - 禁止  
✗ 写入非传感器目录 - 禁止
✗ 网络请求 - 禁止
✗ 读取 stdin 以外的文件 - 禁止
✗ 包含未转义的用户输入 - 禁止（所有 grep pattern 必须静态）
```

### 9.2 运行时安全

```
当前 error.sh 已在 shell 中执行，与 Bash 工具共享相同的风险模型。
传感器脚本没有额外的沙箱。

缓解：
  • error.sh 只匹配文本，不修改任何状态
  • 输出仅限于 stdout 上的单行 JSON
  • 脚本权重低，即使被恶意修改，也不会导致数据丢失
```

### 9.3 文件权限

```bash
# 写入时设置
chmod 644 .dscode/sensors/error.sh
# 不设可执行位，由 run_sensor 通过 "bash script.sh" 方式执行
# 避免用户误双击执行
```

---

## 十、测试策略

### 10.1 测试覆盖矩阵

| 测试层 | 测试项 | 类型 | 涉及文件 |
|--------|-------|:----:|---------|
| 单元 | `sensor_resolve_path` 返回正确优先级 | 单元 | sensor.rs |
| 单元 | `sensor_resolve_path` 不存在时回退内置 | 单元 | sensor.rs |
| 单元 | `sensor_resolve_path` 项目级优先于用户级 | 单元 | sensor.rs |
| 单元 | 新增 `ctx` 参数向后兼容性 | 单元 | sensor.rs |
| 集成 | skill 启用后生成文件位置正确 | 集成 | SKILL.md + sensor.rs |
| 集成 | 生成脚本语法验证 (bash -n) | 集成 | SKILL.md |
| 集成 | 生成脚本输出合法 JSON | 集成 | SKILL.md |
| 回归 | 不启用 skill 时行为不变 (213 现有测试) | 回归 | 全部 |
| 回归 | 启用 skill + 无项目配置 → 使用内置回退 | 回归 | sensor.rs |
| 冒烟 | 完整链路：分析→生成→执行→信号→Controller | 冒烟 | 端到端 |

### 10.2 测试场景

#### 场景 1：纯 Rust 项目

```
项目结构：
  my-rust-app/
  ├── Cargo.toml
  └── src/
      └── main.rs

预期行为：
  • 检测语言: Rust (Cargo.toml)
  • 检测框架: cargo test
  • 生成的 error.sh 包含 Rust + cargo test + generic 规则
  • 不包含 Python/Node/Go 规则
```

#### 场景 2：Python + pytest 项目

```
项目结构：
  my-py-app/
  ├── pyproject.toml
  ├── pytest.ini
  ├── src/
  └── tests/
      └── conftest.py

预期行为：
  • 检测语言: Python (pyproject.toml)
  • 检测框架: pytest (pytest.ini + conftest.py)
  • 生成的 error.sh 包含 Python + pytest + generic 规则
  • 不包含 Rust/Node 规则
```

#### 场景 3：Monorepo (Rust + Python)

```
项目结构：
  monorepo/
  ├── Cargo.toml          (根目录)
  ├── packages/
  │   ├── rust-service/
  │   │   └── Cargo.toml
  │   └── py-service/
  │       ├── setup.py
  │       └── pytest.ini

预期行为：
  • 检测语言: Rust (主) + Python (辅)
  • 检测框架: cargo test + pytest
  • 生成的 error.sh 包含 Rust + Python + cargo test + pytest + generic 规则
  • 规则合并，按 language-priority 排序
```

#### 场景 4：未知项目（仅 Makefile）

```
项目结构：
  legacy/
  ├── Makefile
  └── src/
      └── main.c

预期行为：
  • 检测语言: C/C++ (Makefile)
  • 检测框架: 无
  • 生成的 error.sh 仅包含 generic + C/C++ 规则
```

#### 场景 5：无构建文件

```
项目结构：
  scripts/
  └── deploy.sh

预期行为：
  • 检测语言: Shell (回退)
  • 检测框架: 无
  • 生成的 error.sh 仅包含 generic 规则（等同于内置）
```

### 10.3 测试辅助函数（需要实现）

```rust
// 在测试中创建临时项目目录，模拟文件结构
fn setup_project_fixture(files: &[(&str, &str)]) -> tempfile::TempDir

// 模拟 AgentSharedContext 指向 fixture
fn mock_context_for_project(project_dir: &Path) -> AgentSharedContext

// 验证生成的 error.sh 包含指定的检测模式
fn assert_error_sh_contains(error_sh: &Path, pattern: &str)

// 验证生成的 error.sh 可通过 bash -n
fn assert_error_sh_syntax_ok(error_sh: &Path)

// 模拟工具输出，检查传感器返回的信号
fn assert_sensor_detects(script: &Path, tool_output: &str, expected_kind: &str)
```

---

## 十一、与现有系统集成

### 11.1 与其他 skill 的关系

| Skill | 关系 | 说明 |
|-------|------|------|
| debugging | 互补 | debugging skill 指导 LLM 如何修复错误； error-sensor 帮助检测错误。两者同时启用时，error-sensor 先运行（在工具调用层），debugging 在 LLM 决策层 |
| verification | 互补 | verification skill 要求 LLM 运行测试验证；error-sensor 在测试失败时提供结构化信号 |
| pre-code-check | 无冲突 | pre-code-check 在修改代码前触发，error-sensor 在每次工具调用后触发，时间点不同 |
| tdd | 无冲突 | tdd 指导开发流程，error-sensor 在流程中的工具执行层工作 |

### 11.2 与 config 系统的集成

```toml
# .dscoderc 配置项（远期规划）
[sensors]
# 传感器搜索路径，按优先级排列
search_paths = [
  ".dscode/sensors",
  "~/.dscode/sensors",
]

# 全局禁用传感器（调试用）
enabled = true

# 每个传感器的独立开关
[sensors.error]
enabled = true
# 自定义传感器路径（覆盖搜索）
script = "/path/to/custom/error.sh"

# 传感器执行超时（毫秒）
timeout_ms = 5000
```

### 11.3 与现有 `--list-skills` 的集成

```
$ dscode --list-skills
  debugging: Use when encountering any bug...
  error-sensor: Analyzes project structure and generates...
  pre-code-check: Use BEFORE touching any code...
  tdd: Use when implementing any feature...
  verification: Use when about to claim work is complete...
```

---

## 十二、暂不实现的功能（远期规划）

### 12.1 多传感器编排

```
远期目标：
  perf.sh    — 检测工具执行延迟过高、输出膨胀
  context.sh — 检测上下文压力、缓存退化
  progress.sh— 检测修复循环、任务停滞

每个传感器独立命名，通过 run_sensor(name, ...) 调用。
查找路径与 error.sh 相同机制。

暂缓原因：
  这些传感器的信号类型（perf_warning, context_high 等）在 Controller
  中还没有对应的处理逻辑。需先定义信号→控制动作的映射。
```

### 12.2 传感器信号权重自适应

```
远期设计：
  Controller 跟踪每个传感器信号的历史准确率。
  如果某条规则频繁触发但 LLM 总能忽略它并成功（Stop），
  自动降低该规则的 weight。

  实现方式：
  run_sensor 返回的信号包含 signal_id（规则内容的 hash）。
  Controller 为每个 signal_id 维护 Beta(α,β) 分布。
  当 LLM 成功完成轮次（Stop）但该信号存在时 → 降低 weight。

暂缓原因：
  需要 Controller 跟踪 signal_id 级别的统计，增加复杂度。
  当前 weight 固定已经能工作。
```

### 12.3 热重载

```
参见 4.3 节。核心问题：session 中文件变更检测的成本 vs 收益。
暂缓。
```

### 12.4 `/sensor-status` 命令

```
CLI 命令显示当前传感器状态：

$ /sensor-status
error.sh → .dscode/sensors/error.sh (project)
  大小: 2.4KB
  规则数: 12
  生成时间: 2025-06-18 12:34
  来源: error-sensor skill
  最近触发: Bash 工具, 3 次

perf.sh → ~/.dscode/sensors/perf.sh (user)
  大小: 1.1KB
  规则数: 5

context.sh → <built-in>
  规则数: 3
```

### 12.5 分析缓存失效

```
当前设计：每次 session 启动时检查 fingerprint。
远期设计：使用 inotify/FSEvents 监听项目文件变更，自动重新分析。

暂缓原因：跨平台文件监听实现复杂（Linux: inotify, macOS: FSEvents,
Windows: ReadDirectoryChangesW），且增加额外依赖。
```

### 12.6 项目级别的传感器配置

```yaml
# .dscode/sensors/config.yaml（远期）
sensors:
  error:
    enabled: true
    custom_rules:
      - pattern: "my-custom-error"
        weight: 1.0
        detail: "Project-specific error"
    exclude_rules:
      - "Rust compilation error"  # 项目不是 Rust
  perf:
    enabled: false
```

### 12.7 传感器市场 / 共享规则

```
远期设计：用户可分享 error.sh 规则片段。
.dscode/sensors/rules.d/ 目录中的 .sh 文件自动包含。

暂缓原因：需要设计规则片段标准和合并逻辑。
```

---

## 十三、实施步骤与依赖关系

### 13.1 依赖图

```
Phase A: 传感器框架扩展
  └── A1: sensor.rs 添加 ctx 参数
  └── A2: 实现 sensor_resolve_path()
  └── A3: 更新 run_sensor() 和 find_sensor()
  └── A4: run_sensor 调用方更新（runner.rs）
  └── A5: 单元测试（查找优先级）
  └── A6: 回归测试（不启用时行为不变）
         │
         ▼
Phase B: error-sensor SKILL.md
  └── B1: 编写 SKILL.md（含所有模板）
  └── B2: build.rs 自动嵌入验证
  └── B3: 集成测试（项目分析→生成→验证）
         │
         ▼
Phase C: 端到端验证
  └── C1: 冒烟测试（完整链路）
  └── C2: 文档更新（--list-skills, README）
  └── C3: 清理测试辅助函数
```

### 13.2 实施顺序建议

```
实施顺序（按依赖）:
  1. Phase A1-A4  → 传感器框架扩展（核心基础设施）
  2. Phase B1-B2  → SKILL.md 编写（LLM 行为定义）
  3. Phase A5-A6  → 单元测试+回归测试（验证基础设施）
  4. Phase B3     → 集成测试（验证 skill 行为）
  5. Phase C1-C3  → 端到端验证+文档

其中 Phase B 和 Phase A 可以部分并行：
  • A1-A4 完成后即可开始 B1
  • A5-A6 与 B3 可并行执行
```

---

## 十四、附录：完整生成示例

### 14.1 示例：Rust + cargo test 项目生成的 error.sh

```bash
#!/bin/bash
# ================================================================
# Project-specific error sensor
# Generated by error-sensor skill
# Project: my-rust-app
# Language: Rust
# Framework: cargo test
# Generated at: 20250618-123456-abc0
# Project fingerprint: a1b2c3d4e5f6
# ================================================================

tool="$1"
elapsed_ms="$2"
output_len="$3"
output=$(cat)
signals=""

# ================================================================
# Rust compilation errors (12 rules)
# ================================================================

echo "$output" | grep -qi "error\[E" && \
  signals+="{\"kind\":\"tool_error\",\"weight\":1.0,\"detail\":\"Rust compilation error\"},"
echo "$output" | grep -qi "error: aborting" && \
  signals+="{\"kind\":\"tool_error\",\"weight\":1.0,\"detail\":\"Rust build aborted\"},"
echo "$output" | grep -qi "could not compile" && \
  signals+="{\"kind\":\"tool_error\",\"weight\":1.0,\"detail\":\"Cargo build failure\"},"
echo "$output" | grep -qi "mismatched types" && \
  signals+="{\"kind\":\"tool_error\",\"weight\":1.0,\"detail\":\"Rust type mismatch\"},"
echo "$output" | grep -qi "cannot borrow" && \
  signals+="{\"kind\":\"tool_error\",\"weight\":1.0,\"detail\":\"Rust borrow checker\"},"
echo "$output" | grep -qi "unused variable" && \
  signals+="{\"kind\":\"tool_error\",\"weight\":0.3,\"detail\":\"Rust unused variable\"},"

# ================================================================
# Cargo test failures (3 rules)
# ================================================================

echo "$output" | grep -qE "^test .+ FAILED" && \
  signals+="{\"kind\":\"tool_error\",\"weight\":1.0,\"detail\":\"Cargo test failure\"},"
echo "$output" | grep -qE "test result: FAILED" && \
  signals+="{\"kind\":\"tool_error\",\"weight\":1.0,\"detail\":\"Cargo test suite failed\"},"
echo "$output" | grep -qi "panicked at" && \
  signals+="{\"kind\":\"tool_error\",\"weight\":1.0,\"detail\":\"Rust panic\"},"

# ================================================================
# Generic fallback patterns (5 rules)
# ================================================================

echo "$output" | grep -qE "exit code [1-9]|^Error:" && \
  signals+="{\"kind\":\"tool_error\",\"weight\":0.5,\"detail\":\"Non-zero exit\"},"
echo "$output" | grep -qi "killed\|SIGTERM\|SIGKILL\|timed out" && \
  signals+="{\"kind\":\"tool_error\",\"weight\":0.8,\"detail\":\"Process killed or timed out\"},"
echo "$output" | grep -qi "Permission denied" && \
  signals+="{\"kind\":\"tool_error\",\"weight\":0.8,\"detail\":\"Permission denied\"},"
echo "$output" | grep -qi "No such file or directory" && \
  signals+="{\"kind\":\"tool_error\",\"weight\":0.8,\"detail\":\"File not found\"},"
echo "$output" | grep -qi "out of memory\|OOM\|Allocation failure" && \
  signals+="{\"kind\":\"tool_error\",\"weight\":1.0,\"detail\":\"Out of memory\"},"

# ================================================================
# Output
# ================================================================
if [ -n "$signals" ]; then
  echo "{\"signals\":[${signals%,}]}"
else
  echo "{}"
fi
```

### 14.2 生成过程消耗估算

```
单次激活的资源消耗：

项目分析阶段：
  Glob 调用：     ~5-15 次（取决于项目大小）
  Read 调用：     ~2-5 次（读取构建文件内容）
  LLM 推理：      ~500-1500 tokens（分析+模板选择+生成）
  文件写入：      1 次

运行时每次工具调用的额外开销：
  路径查找：      ~0ms（缓存命中）
  脚本执行：      ~1-5ms（error.sh 执行时间）
  内存：          ~10KB（脚本内容 + 输出缓冲区）

结论：对正常使用的影响可忽略。
```
