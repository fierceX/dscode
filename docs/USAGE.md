# 使用手册

## 快速开始

```bash
# 编译
make build

# 设置 API Key
export DEEPSEEK_API_KEY="sk-xxx"

# 单次任务
./target/release/dscode -m deepseek-v4-flash "scan this project"

# 交互式 REPL
./target/release/dscode -m deepseek-v4-flash -i

# stdin 管道输入
echo "list the files" | ./target/release/dscode -m deepseek-v4-flash

# 继续上次会话
./target/release/dscode -m deepseek-v4-flash --continue -i
```
## 配置文件

`~/.dscoderc`（用户级）和 `<project>/.dscoderc`（项目级）可选配置。
优先级：CLI 参数 > 项目配置 > 用户配置 > 环境变量 > 默认值。

```toml
# ~/.dscoderc 示例
api_key = "sk-xxx"                    # API 密钥
base_url = "https://api.deepseek.com/v1"  # API 端点
model = "deepseek-v4-flash"            # 默认模型
max_tokens = 81920                     # 最大输出 token
max_turns = 40                         # 最大轮次
max_context = "1M"                     # 最大上下文（支持 K/M 后缀）
tool_timeout = 600                     # 工具超时（秒）
auto_model = false                     # 自动升级
secondary_model = "deepseek-v4-pro"    # 升级目标模型
auto_upgrade_threshold = 4             # 升级阈值
auto_self_report = false               # 自报告升级
context_compact_pct = 85               # 压缩触发百分比
log_events = true                      # 事件日志
```

项目级 `.dscoderc` 覆盖用户级，CLI 参数覆盖所有文件设置。
所有字段可选，未设置的字段使用默认值或环境变量。

完整示例见 `.dscoderc.example`。

---



## CLI 参数

| 参数 | 默认值 | 说明 |
|------|--------|------|
| `-m` / `--model` | `deepseek-v4-flash` | 模型名 |
| `--max-tokens` | `81920` | 最大输出 token 数 |
| `--tool-timeout` | `600` | 工具执行超时（秒） |
| `--skill NAME` | — | 加载 skill（可重复） |
| `--max-turns` | `40` | 最大 agent 轮次 |
| `--max-context` | `1000000` | 上下文 token 上限。支持 `k`/`m` 后缀 |
| `--api-key KEY` | env | 覆盖 API Key |
| `--base-url URL` | `https://api.deepseek.com/v1` | 覆盖 API 端点 |
| `--output-format FMT` | `human` | 输出格式：`human` / `stream-json` |
| `--print` | — | `--output-format stream-json` 别名 |
| `--session [NAME]` | 自动生成 | 命名会话。提供名称可恢复 |
| `--continue` | — | 恢复最近的 session |
| `--list-sessions` | — | 列出所有 session |
| `--list-skills` | — | 列出内置 skill |
| `-v` / `--verbose` | `false` | 详细日志 |
| `-i` / `--interactive` | auto | 交互式 REPL |
| `-h` / `--help` | — | 显示帮助 |

不提供 prompt 参数且 stdin 是终端时自动进入交互模式。
非终端 stdin 时读取 stdin 作为 prompt。


---

## 环境变量

| 变量 | 默认值 | 说明 |
|------|--------|------|
| `DEEPSEEK_API_KEY` | — | **必需。** API 密钥 |
| `DEEPSEEK_BASE_URL` | `https://api.deepseek.com/v1` | 自定义 API 端点 |
| `JINA_API_KEY` | — | WebSearch/WebFetch 工具需要的 API 密钥 |
| `CONTEXT_COMPACT_PCT` | `85` | 上下文压缩触发百分比（1-99） |
| `AUTO_MODEL` | `false` | 设为 `true` 启用自动模型升级 |
| `AUTO_UPGRADE_THRESHOLD` | `4` | 升级触发分数 |
| `SECONDARY_MODEL` | — | 升级后切换的模型名 |
| `AUTO_SELF_REPORT` | `false` | 启用 `<<<NEEDS_PRO>>>` 自报告升级 |
| `TOOL_RESULT_MAX_BYTES` | `100000` | 单条工具结果截断上限 |
| `FILE_WRITE_MAX_BYTES` | `1048576` | Write/Edit 工具写入上限 |
| `LOG_EVENTS` | `true` | 设为 `0`/`false`/`no` 关闭 events.jsonl 记录 |
| `DSCODE_HOME` | `$HOME` | session 存储目录覆盖 |

---

## 会话管理

### 目录结构

```
~/.dscode/
├── history                    ← 交互式 REPL 历史
└── projects/<project_key>/
    └── <session_id>/
        ├── conversation.jsonl ← 对话消息（JSONL）
        ├── events.jsonl       ← 事件日志
        ├── summary.txt        ← 压缩后的上下文快照
        ├── plan.md            ← 确认后的计划
        ├── plan.draft         ← 草稿计划
        └── stats.json         ← Token 用量统计
```

### 操作

```bash
# 命名会话
dscode -m deepseek-v4-flash --session my-fix "fix the bug"

# 恢复命名会话（保持上下文）
dscode -m deepseek-v4-flash --session my-fix -i

# 恢复最近会话
dscode -m deepseek-v4-flash --continue -i

# 列出所有
dscode --list-sessions
```

`--continue` 自动选择最近修改的 session，replay 最近 10 轮 LLM 响应。

---

## 交互式 REPL

```bash
dscode -m deepseek-v4-flash -i
```

prompt 为空且 stdin 是终端时自动进入交互模式。

### 内置命令

| 命令 | 说明 |
|------|------|
| `/flash` | 切换到 flash 模型 |
| `/pro` | 切换到 pro 模型 |
| `/compact` | 强制上下文压缩 |
| `/skills` | 列出所有可用 skill |
| `/help` | 显示可用的命令列表 |
| `exit` / `quit` | 退出 REPL |
| Ctrl+C | 取消当前正在执行的 turn |
| Ctrl+D | 退出 REPL |

`/flash` 和 `/pro` 命令立即生效，不会发送给 LLM。切换后下一轮 LLM 调用使用新模型。所有其他输入作为普通消息发送给 LLM。

### 标题栏

当前活动模型和实时统计信息显示在终端标题栏（macOS 终端顶部标签页名称或 iTerm2 标题栏）：

```
deepseek-v4-flash T:5 R:12 I:890K(85%) O:12K C:742K(74%) ¥0.42
│                  │  │  │              │        │           │
│                  │  │  │              │        │            └─ 估计成本（基于累计 token × DeepSeek 定价）
│                  │  │  │              │        └── 当前上下文 + 使用率（当前/max）
│                  │  │  │              └─────────── 输出总 token（K 为单位）
│                  │  │  └──────────────────────── 输入总 token + 缓存命中率（K 为单位）
│                  │  └─────────────────────────── API 请求数（agent + compact + sub-agent）
│                  └────────────────────────────── 当前用户轮次
└───────────────────────────────────────────────── 当前模型名（/flash 或 /pro 切换后自动更新）
```

手动切换模型后标题栏立即更新。

### 特性

- `> ` 提示符，rustyline 编辑（Ctrl+W/Del/方向键）
- 历史持久化到 `~/.dscode/history`
- 会话结束时打印恢复命令

---

## 计划系统（Plan）

计划系统通过两个内置工具管理，不需要 CLI 参数启用——LLM 在需要时会自动使用。

### 生命周期

```
┌─ LLM 提议制定计划
│  → LLM 使用 TodoWrite 创建 checklist
│  → LLM 使用 Edit 编辑 plan.draft 文件
├─ 用户确认计划
│  → LLM 调用 PlanConfirm 工具
│    → plan.draft → plan.md（锁定）
│    → 触发上下文压缩
│    → system prompt 中出现 <current-plan> 段
├─ 执行阶段
│  → LLM 逐步完成任务，更新 TodoWrite
│  → 每一步完成后可调用 PlanClear
└─ 计划完成
   → LLM 调用 PlanClear 工具
     → plan.md 清空
     → 触发上下文压缩
     → system prompt 中不再包含 <current-plan> 段
```

### PlanConfirm

**触发条件**：用户明确确认 plan 后。**非**规划阶段或用户要求修改时。

```
参数：无
行为：plan.draft → plan.md + 触发压缩 + 重建 system prompt
返回："Plan confirmed and locked in."
```

如果 plan.draft 为空（用户未创建草稿就要求确认），返回错误信息。

### PlanClear

**触发条件**：计划所有任务完成后。

```
参数：无
行为：清空 plan.md + 触发压缩 + 重建 system prompt
返回："Plan cleared."
```

### 系统提示词中的引导

`plan-lifecycle-guidance` 段嵌入在 system prompt 中，指导 LLM 正确使用计划系统：

- 草稿阶段写入 `plan.draft`，确认后才写入 `plan.md`
- `PlanConfirm` 在用户确认后才调用
- `PlanClear` 在所有任务完成后调用
- 计划执行完毕后 system prompt 不再包含 plan section

---

## 上下文压缩

### 三级阈值

| 上下文使用率 | 压缩等级 | 保留比例 | 触发方式 |
|-------------|---------|:--------:|---------|
| <85% | — | 100% | 不压缩 |
| 85-95% | ForceSummary | 5% | 默认压缩点 |
| ≥95% | Emergency | 1-5 行 | preflight 紧急保护 |

压缩触发后：
1. 按 user 消息边界保留末尾 5% 会话
2. 截断部分送入 LLM 生成摘要（含 fold marker 标记）
3. 摘要写入 `summary.txt`，纳入 system prompt
4. conversation.jsonl 截断保留末尾

### 防护

- **同轮只压缩一次**：同一用户输入内多次 LLM 调用不重复压缩
- **最小收益检查**：节省不足 10% token 时跳过
- **preflight 预判**：发送前估算 token 量，>95% 时提前压缩

### 调优

```bash
CONTEXT_COMPACT_PCT=70 dscode -m deepseek-v4-flash -i    # 70% 触发（更频繁）
CONTEXT_COMPACT_PCT=90 dscode -m deepseek-v4-flash -i    # 90% 触发（更激进）
dscode -m deepseek-v4-flash --max-context 1M -i          # 1M 上下文窗口
```

---

## 维修流水线

每次工具执行前自动运行三段修复。

### Scavenge — 回收

从 LLM 的 `reasoning_content` 和文本回复中回收遗漏的工具调用。支持格式：

| 格式 | 示例 |
|------|------|
| DSML invoke（DeepSeek 原生） | `<\|DSML\|invoke name="Read">...<\|DSML\|parameter name="path" string="true">/x<\|DSML\|parameter><\|DSML\|invoke>` |
| XML 包装 | `<tool_call>{"name":"Bash","arguments":{"command":"ls"}}</tool_call>` |
| Bracket 包装 | `[TOOL_CALL]{"name":"Read"...}[/TOOL_CALL]` |
| 裸 JSON | `{"name":"Grep","arguments":{"pattern":"foo"}}` |
| OpenAI style | `{"type":"function","function":{"name":"Read","arguments":"..."}}` |
| R1 free-form | `{"tool_name":"Bash","tool_args":{"command":"ls"}}` |

### Truncation — 截断修复

修复被截断的 JSON 参数：闭合引号、补全括号、去掉尾逗号、填 null 到悬挂 key。

### StormBreaker — 重复抑制

滑动窗口（size=6）检测 `(工具名, 参数)` 重复。同一对出现 3 次时抑制该调用并返回抑制原因。mutating 工具（Bash/Write/Edit）执行时清空只读条目。

---

## 工具

| 工具 | 用途 | 关键参数 |
|------|------|---------|
| `Read` | 读文件 | `path`, `offset`, `limit` |
| `Write` | 写文件 | `path`, `content` |
| `Edit` | 精确替换 | `path`, `old_string`, `new_string` |
| `Bash` | 执行命令 | `command`, `timeout` |
| `Glob` | 文件匹配 | `pattern`, `path` |
| `Grep` | 内容搜索 | `pattern`, `path`, `glob`, `context` |
| `TodoWrite` | 维护 checklist | `todos[{content, status}]` |
| `PlanConfirm` | 确认计划 | 无参数 |
| `PlanClear` | 清空计划 | 无参数 |
| `SubAgent` | 启动子代理 | `prompt`, `description`, `fork` |
| `Skill` | 按需加载 skill | `name` |
| `WebSearch` | 网络搜索 | `query` |
| `WebFetch` | 网页获取 | `url` |

详见 [tools.md](tools.md)。

---

## Skills（技能）

### 启用方式

```bash
# CLI 加载
dscode -m deepseek-v4-flash --skill debugging

# 加载多个
dscode -m deepseek-v4-flash --skill debugging --skill tdd

# 按需加载（由 LLM 的 Skill 工具触发）
# LLM 从 system prompt 的 skill-index 段中选择 → Skill(name) → 加载 SKILL.md
```

### 内置技能（编译时嵌入）

所有 `skills/<name>/SKILL.md` 文件在编译时自动嵌入到二进制中。
添加新技能只需创建文件，不需要修改 Rust 代码。

| 技能名 | 描述 | 适用场景 |
|--------|------|---------|
| `debugging` | 四阶段系统调试：根因调查 → 模式分析 → 假设验证 → 修复实现 | 遇到 bug、测试失败、非预期行为时 |
| `verification` | 验证门控：禁止在未运行验证的情况下声称任务完成 | 完成任务、声称修复成功、commit 前 |
| `tdd` | 测试驱动开发：红绿重构循环 | 实现新功能或修 bug 前 |
| `pre-code-check` | 编码前置检查：先搜索调用点、读上下文、验证假设 | 编辑文件前 |

### 搜索路径（优先级）

1. **内置（编译时嵌入）** — 直接读取内存，零文件 I/O
2. `<project>/.claude/skills/<name>/SKILL.md` — 项目级覆盖
3. `<project>/skills/<name>/SKILL.md` — 项目开发目录
4. `~/.claude/skills/<name>/SKILL.md` — 用户全局

同名 skill 会被覆盖（优先级高的替代内置的）。

### 加载机制

- `--skill NAME` 在 system prompt 的 `<selected-skills>` 段嵌入 SKILL.md 全文
- `Skill` 工具在运行时按需加载，不修改后续轮次的 system prompt
- 内置技能即使在离线环境也可用（编译时已嵌入）

---

## SubAgent（子代理）

### 调用方式

LLM 自动调用 SubAgent 工具。

### 参数

| 参数 | 说明 |
|------|------|
| `prompt` | 子代理执行的任务描述（**必需**） |
| `description` | 日志标记（可选） |
| `fork` | `true` 继承父会话上下文（可选，默认独立） |

### 模式

| 模式 | 上下文 | 适用 |
|------|--------|------|
| 独立（默认，不带 `fork`） | 全新空会话 | 文件调查、搜索、隔离的假设验证 |
| Fork（`fork=true`） | 继承父会话对话/计划/技能 | 需要父上下文的延续性任务 |

### 行为

- 同一轮中多个 SubAgent 调用**并发**执行
- 结果**异步逐个返回**：收到一个结果时其他仍在运行，只需等待，不要重复启动
- 返回格式：`[sub-agent <id>] <status> (in=<n>, out=<n>)\nThinking: ...\nText: ...`
- 失败时（`status=failed`）结果可能为空，不要自动重试
- Token 用量计入父会话统计
- 最多 8 个并发

---

## Auto-Model 自动升级

### 启用

```bash
AUTO_MODEL=1 ./target/release/dscode -m deepseek-v4-flash -i
```

### 升级机制

基于贝叶斯 P(stall) + flash 质量监控，无需权重配置：

| 信号 | 来源 | 作用 |
|------|------|------|
| Controller 停滞 | 连续失败轮次 | P(stall) > 0.80 → 强制 Pro |
| flash 长期质量 | Beta(α,β) 后验 | Q<0.50, N≥8 → 证明不够好 → Pro |
| 传感器 | error.sh (70+ 模式) | 聚合后喂入 Controller |

**降级仅手动**：`/flash` 将 flash 重置为 Beta(3,3)。

### 标题栏实时指标

```
flash Q:0.68/33 T:12 R:45 I:200K(50%) O:20K C:400K(40%) ¥0.12
      ^^^^^^^^
      Q = flash 成功率 α/(α+β), /33 = 观测数
```

Q < 0.50 且 N ≥ 16 (工具级观测) 时自动升级。在 Pro 上不显示 Q。

### 配置

```bash
# 启用自动模型切换
AUTO_MODEL=1 ./target/release/dscode

# 传感器目录搜索（可选）
# .dscode/sensors/*.sh 或 ~/.dscode/sensors/*.sh 可覆盖内置 error.sh
```

---

## Stream-JSON 输出

```bash
dscode -m deepseek-v4-flash --print "explain this"
```

每行一个 JSON：

```json
{"type":"thinking","content":"Let me analyze..."}
{"type":"text","content":"Here is the explanation..."}
{"type":"tool_call","name":"Read","id":"...","input":{"path":"/x"}}
{"type":"tool_result","tool_use_id":"...","name":"Read","content":"..."}
{"type":"usage","input_tokens":100,"output_tokens":50,"cache_read_input_tokens":40}
{"type":"stop","reason":"end_turn"}
```

JQ 下游处理：

```bash
dscode -m deepseek-v4-flash --print "fix the bug" | jq 'select(.type=="text") | .content'
```

---

## 环境变量完整参考（按来源分类）

### API（`apply_provider_defaults`）

```
DEEPSEEK_API_KEY      ← 首选
DEEPSEEK_BASE_URL     ← 首选
```

### 大小限制（`apply_provider_defaults`）

```
TOOL_RESULT_MAX_BYTES  ← 默认 100000
FILE_WRITE_MAX_BYTES   ← 默认 1048576
```

### 压缩（`compaction.rs`）

```
CONTEXT_COMPACT_PCT    ← 默认 85
```

### 升级（`orchestrator.rs`）

```
AUTO_MODEL             ← 默认 false
AUTO_UPGRADE_THRESHOLD ← 默认 4
SECONDARY_MODEL        ← 无默认
AUTO_SELF_REPORT       ← 默认 false
```

### 调试（`main.rs` + `context.rs`）

```
LOG_EVENTS             ← 默认 true
DSCODE_HOME        ← 默认 $HOME
```

### Web 搜索（`tools/web.rs`）

```
JINA_API_KEY           ← 无默认（WebSearch/WebFetch 必需）
```

---

## 故障排查

```bash
# 检查 API key
curl -H "Authorization: Bearer $DEEPSEEK_API_KEY" https://api.deepseek.com/v1/models

# verbose 模式
dscode -m deepseek-v4-flash -v "hello"

# 扩大上下文窗口避免溢出
dscode -m deepseek-v4-flash --max-context 1M -i

# 查看 session 列表
dscode --list-sessions
```
