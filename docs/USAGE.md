# 使用手册

## 快速开始

```bash
# 编译
make build
# 或
cargo build --release

# 设置 API Key
export DEEPSEEK_API_KEY="sk-xxx"

# 单次任务
./target/release/dscode -m flash "scan this project"

# REPL 交互模式
./target/release/dscode -m flash -i

# TUI 全屏模式
./target/release/dscode -m flash --tui

# 继续上次会话
./target/release/dscode -m flash --continue -i

# stdin 管道输入
echo "list the files" | ./target/release/dscode -m flash
```

---

## 两种终端模式

项目提供两种交互式终端模式，一种非交互 CLI 模式。

### REPL 模式（`-i`）

基于 rustyline 的行编辑器。适合日常编码交互：

```
dscode interactive mode (type 'exit' or Ctrl+D to quit)
> scan this project for Rust errors
[tool] Bash(command="cargo check")
...
```

- 输入：rustyline 行编辑（历史、Tab 补全、Ctrl+W/Del）
- 输出：stderr 渲染（灰色 thinking、黄色 tool call、普通 text）
- 标题栏：ANSI escape 更新终端窗口标题
- 历史记录：持久化到 `~/.dscode/history`

### TUI 模式（`--tui`）

基于 ratatui 的全屏终端界面。适合长时间编码会话：

```
flash B:0.73 T:12 R:45 I:200K(50%) O:20K C:400K(40%) ¥0.12 [idle]
────────────────────────────────────────────────────────────────
 消息列表（历史对话、工具调用、工具结果）
────────────────────────────────────────────────────────────────
 > 输入区域（多行输入）
```

**状态栏字段含义**：

| 字段 | 示例 | 含义 |
|------|------|------|
| `flash` | 模型名 | 当前模型 |
| `B:0.73` | 信念度 | 工具执行可靠性评分（0.0~1.0，0.0 表示未追踪） |
| `T:12` | 对话轮次 | 当前对话的用户输入轮数 |
| `R:45` | API 请求 | 累计 LLM API 请求次数 |
| `I:200K` | 输入 tokens | 总输入 tokens（含缓存读取），括号内为缓存命中率 |
| `O:20K` | 输出 tokens | 总输出 tokens |
| `C:400K` | 上下文 | 当前对话上下文 tokens，括号内为上下文使用率 |
| `¥0.12` | 费用 | 累计费用（按模型单价实时计算） |
| `[idle]` | 工作状态 | idle / thinking / generating / working |

**B 值含义**：

| B 值 | 含义 |
|------|------|
| 0.75 | 初始（信任先验） |
| > 0.7 | 🟢 顺利 |
| 0.5~0.7 | 🟡 偶有小错 |
| 0.3~0.5 | 🟠 频繁出错 |
| < 0.3 | 🔴 严重 |

### 标题栏（REPL/CLI 模式）

非 TUI 模式下，终端窗口标题显示相同统计信息，通过 ANSI escape `\x1b]0;...\x07` 设置：

```
flash B:0.73 T:12 R:45 I:200K(50%) O:20K C:400K(40%) ¥0.12
```

信念度在每次工具调用后实时更新。低于阈值时，系统会在同一次任务循环内的下一轮 LLM 调用前注入提示词或中止任务。

### 非交互 CLI 模式

```bash
# 单次查询
./target/release/dscode -m flash "explain this"

# 管道输入
cat main.rs | ./target/release/dscode -m flash "review"

# ndjson 结构化输出
./target/release/dscode --print "list files"
```

prompt 为空且 stdin 是终端时自动进入交互模式。非终端 stdin 时读取 stdin 作为 prompt。

---

## REPL 内置命令

| 命令 | 说明 |
|------|------|
| `/flash` | 切换到 flash 模型 |
| `/pro` | 切换到 pro 模型 |
| `/compact` | 强制上下文压缩 |
| `/skills` | 列出所有可用 skill |
| `/help` | 显示可用的命令列表 |
| `exit` / `quit` | 退出 |
| Ctrl+C | 取消当前正在执行的 turn |
| Ctrl+D | 退出 |

`/flash` 和 `/pro` 命令立即生效，不会发送给 LLM。切换后下一轮 LLM 调用使用新模型。所有其他输入作为普通消息发送给 LLM。

---

## CLI 参数

| 参数 | 默认值 | 说明 |
|------|--------|------|
| `PROMPT` | — | 用户输入（位置参数） |
| `-m` / `--model` | `flash` | 模型名：`flash` / `pro` / `deepseek-v4-flash` / `deepseek-v4-pro` |
| `--max-tokens` | `81920` | 每次 LLM 响应的最大 token 数 |
| `--max-turns` | `40` | 每用户输入的最大 LLM 调用轮数 |
| `--max-context` | `1000000` | 上下文 token 上限。支持 `k`/`m` 后缀（如 `500K` / `1M`） |
| `--tool-timeout` | `600` | 工具执行超时（秒） |
| `--sub-agent-timeout` | `300` | 子代理执行超时（秒） |
| `--skill NAME` | — | 加载 skill（可重复使用） |
| `--mission PATH` | — | 加载 MISSION.md 文件替换默认系统提示词 |
| `--session [NAME]` | 自动生成 | 命名会话。提供名称可恢复 |
| `--continue` | — | 恢复最近的 session |
| `--list-sessions` | — | 列出所有 session |
| `--list-skills` | — | 列出内置 skill |
| `-i` / `--interactive` | auto | REPL 交互模式 |
| `--tui` | — | TUI 全屏模式 |
| `--print` | — | ndjson 结构化输出（`--output-format stream-json` 别名） |
| `--output-format FMT` | `human` | 输出格式：`human` / `stream-json` |
| `--json-rpc` | — | JSON-RPC 模式（stdin 读请求，stdout 输出事件流，隐式启用 stream-json） |
| `--disable-bash` | `false` | 禁用 Bash 工具 |
| `--disable-python` | `false` | 禁用 Python 工具 |
| `--disable-sub-agent` | `false` | 禁用 SubAgent 工具 |
| `--disable-web` | `false` | 禁用 WebSearch / WebFetch 工具 |
| `--api-key KEY` | env | 覆盖 API Key |
| `--base-url URL` | 默认端点 | 覆盖 API 端点 |
| `-v` / `--verbose` | `false` | 详细日志 |
| `-h` / `--help` | — | 显示帮助 |

---

## 配置文件

`~/.dscoderc`（用户级）和 `<project>/.dscoderc`（项目级）可选配置。
优先级：CLI 参数 > 项目配置 > 用户配置 > 环境变量 > 默认值。

```toml
# ~/.dscoderc 示例
api_key = "sk-xxx"                        # API 密钥
base_url = "https://api.deepseek.com/v1"  # API 端点
model = "flash"                           # 默认模型
max_tokens = 81920                        # 最大输出 token
max_turns = 40                            # 最大轮次
max_context = "1M"                        # 最大上下文（支持 K/M 后缀）
tool_timeout = 600                        # 工具超时（秒）
sub_agent_timeout = 120                   # 子代理超时（秒）
context_compact_pct = 85                  # 压缩触发百分比
log_events = true                         # 事件日志
```

项目级 `.dscoderc` 覆盖用户级，CLI 参数覆盖所有文件设置。
所有字段可选，未设置的字段使用默认值或环境变量。

---

## 沙箱配置

沙箱通过 OS 原生工具（Linux nsjail/bubblewrap、macOS sandbox-exec）包裹 dscode 进程，
在文件系统层面强制执行访问控制。

### 配置方式

`.dscoderc` 的 `[sandbox]` 段控制沙箱开关和规则：

```toml
[sandbox]
enabled = true                          # 是否启用
backend = "auto"                        # nsjail | bwrap | sandbox-exec | off (macOS 忽略)

# 文件系统白名单（相对路径基于项目根目录）
read_dirs = ["src", "tests", "docs"]    # 允许读取的目录（macOS 忽略）
write_dirs = ["src"]                    # 允许写入的目录

# 工具限制
allow_bash = true
bash_allow_commands = ["ls", "cat", "cargo", "python", "rg"]
allow_python = true
allow_network = true
allow_sub_agent = true

# 资源配额（仅 Linux nsjail cgroup）
max_memory_mb = 1024
max_pids = 64
timeout_secs = 600
```

### 平台差异

| 功能 | Linux (nsjail/bwrap) | macOS (sandbox-exec) |
|------|---------------------|---------------------|
| **写入限制** `write_dirs` | ✅ 内核强制 | ✅ 内核强制 |
| **读取限制** `read_dirs` | ✅ 内核强制 | ❌ 不生效（sandbox 无法阻断 TUI 初始化的系统路径） |
| **网络隔离** `allow_network` | ✅ namespace | ❌ 不生效 |
| **资源限制** `max_memory_mb` | ✅ cgroup | ❌ 不生效 |
| **后台自动启用** | ✅ | ✅（写入限制） |

macOS 上的读取限制应该在应用层通过路径规范化解引用 + 白名单检查实现（计划后续版本添加）。

### 启动机制

dscode 启动时检测 `[sandbox] enabled = true`，自动通过 `exec()` 将自身重新装入沙箱：

```
dscode --tui
  → 读取 .dscoderc
  → exec("nsjail --bindmount_ro src /dscode --json-rpc")  // Linux
  → exec("sandbox-exec -p '<profile>' dscode --tui")        // macOS (写入限制)
  → 设置 DSCODE_SANDBOXED=1 防无限递归
  → 原进程被替换，进程完全在沙箱中运行
```

沙箱工具不可用时打印警告并正常运行（不阻塞）。

---

## 环境变量

| 变量 | 默认值 | 说明 |
|------|--------|------|
| `DEEPSEEK_API_KEY` | — | **必需。** API 密钥 |
| `DEEPSEEK_BASE_URL` | `https://api.deepseek.com/v1` | 自定义 API 端点 |
| `JINA_API_KEY` | — | WebSearch/WebFetch 工具需要的 API 密钥 |
| `CONTEXT_COMPACT_PCT` | `85` | 上下文压缩触发百分比（1-99） |
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
        ├── conversation.jsonl ← 对话消息（JSONL 逐行追加）
        ├── events.jsonl       ← 事件日志
        ├── summary.txt        ← 压缩后的上下文快照
        ├── plan.md            ← 确认后的计划
        ├── plan.draft         ← 草稿计划
        └── stats.json         ← Token 用量统计
```

### 操作

```bash
# 命名会话
dscode -m flash --session my-fix "fix the bug"

# 恢复命名会话（保持上下文）
dscode -m flash --session my-fix -i

# 恢复最近会话
dscode -m flash --continue -i

# 列出所有
dscode --list-sessions
```

`--continue` 自动选择最近修改的 session。恢复时会 replay 最近 10 轮 LLM 响应事件，在交互式终端重新渲染历史对话。

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
└─ 计划完成
   → LLM 调用 PlanClear 工具
     → plan.md 清空
     → 触发上下文压缩
     → system prompt 中不再包含 <current-plan> 段
```

### PlanConfirm

**触发条件**：用户明确确认 plan 后。**非**规划阶段或用户要求修改时。

- 参数：无
- 行为：plan.draft → plan.md + 触发压缩 + 重建 system prompt
- 返回："Plan confirmed and locked in."
- 如果 plan.draft 为空（用户未创建草稿就要求确认），返回错误信息

### PlanClear

**触发条件**：计划所有任务完成后。

- 参数：无
- 行为：清空 plan.md + 触发压缩 + 重建 system prompt
- 返回："Plan cleared."

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
CONTEXT_COMPACT_PCT=70 dscode -m flash -i          # 70% 触发（更频繁）
CONTEXT_COMPACT_PCT=90 dscode -m flash -i          # 90% 触发（更激进）
dscode -m flash --max-context 1M -i                # 1M 上下文窗口
```

---

## 维修流水线

每次工具执行前自动运行三段修复。

### Scavenge — 回收

从 LLM 的 `reasoning_content`（思考过程）和文本回复中回收遗漏的工具调用。支持 6 种格式：

| 格式 | 示例 |
|------|------|
| DSML invoke（DeepSeek 原生） | `<\|DSML\|invoke name="Read">...<\|DSML\|invoke>` |
| XML 包装 | `<tool_call>{"name":"Bash","arguments":{"command":"ls"}}</tool_call>` |
| Bracket 包装 | `[TOOL_CALL]{"name":"Read"...}[/TOOL_CALL]` |
| 裸 JSON | `{"name":"Grep","arguments":{"pattern":"foo"}}` |
| OpenAI style | `{"type":"function","function":{"name":"Read","arguments":"..."}}` |
| R1 free-form | `{"tool_name":"Bash","tool_args":{"command":"ls"}}` |

### Truncation — 截断修复

修复被截断的 JSON 参数：闭合引号、补全括号、去掉尾逗号、填 null 到悬挂 key。

### StormBreaker — 重复抑制

滑动窗口（size=6）检测 `(工具名, 参数)` 重复。同一对出现 3 次时抑制该调用。mutating 工具（Bash/Write/Edit）执行时清空只读条目，允许 edit→re-read 正常模式。

---

## 工具

| 工具 | 用途 | 关键参数 |
|------|------|---------|
| `Read` | 读文件 | `path`, `offset`, `limit` |
| `Write` | 写文件 | `path`, `content` |
| `Edit` | 精确替换 | `path`, `old_string`, `new_string` |
| `Bash` | 执行命令 | `command`, `timeout` |
| `Python` | 运行 Python 脚本（安全受限，禁用 subprocess/os.system/eval） | `script` / `script_file`, `timeout` |
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
dscode -m flash --skill debugging -i

# 加载多个
dscode -m flash --skill debugging --skill tdd -i

# 查看可用技能
dscode --list-skills
```

### 内置技能（编译时嵌入）

所有 `skills/<name>/SKILL.md` 文件在编译时自动嵌入到二进制中。添加新技能只需创建文件，不需要修改 Rust 代码。

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

## MISSION（自定义系统提示词）

通过 `--mission PATH` 加载一个 MISSION.md 文件，替换默认系统提示词的对应段。

### 机制

MISSION.md 使用一级标题（`# heading-name`）映射到系统提示词的段名。加载后自动替换同名的默认段，未在文件中定义的段保持默认内容。

```markdown
# agent-identity
你是文档处理助手，负责根据素材文件生成结构化文档。

# rules
- 严格遵循素材内容，不得额外杜撰
- 输出格式必须符合要求

# process-flow
## Phase 1: 素材分析
...
```

### 用法

```bash
# 加载自定义提示词
dscode --mission ./my-task.mission.md -i

# 结合技能使用
dscode --mission ./my-task.mission.md --skill debugging -i
```

### Python SDK

```python
# 文件方式
SandboxConfig(mission_file="./my-task.mission.md")

# 字符串方式
SandboxConfig(mission_content="# agent-identity\n...")
```

### 注意事项

- MISSION.md 替换的是 prompt 文本，不影响工具定义。禁用工具仍需 `--disable-bash` 等参数。
- 未在 MISSION.md 中定义的段（如 `verification-gate`、`belief-awareness` 等）保持默认内容。
- 建议将 MISSION.md 置于项目目录下，纳入版本管理。

---

## SubAgent（子代理）

### 调用方式

LLM 自动调用 SubAgent 工具。支持并发执行（最多 8 个并发）。

### 参数

| 参数 | 说明 |
|------|------|
| `prompt` | 子代理执行的任务描述（**必需**） |
| `description` | 日志标记（可选） |
| `fork` | `true` 继承父会话上下文（可选，默认独立） |

### 模式

| 模式 | 上下文 | 适用 |
|------|--------|------|
| 独立（默认） | 全新空会话 | 文件调查、搜索、隔离的假设验证 |
| Fork（`fork=true`） | 继承父会话对话/计划/技能 | 需要父上下文的延续性任务 |

### 结果格式

```
[sub-agent <id>] <status> (in=<n>, out=<n>)
Thinking: ...
Text: ...
```

失败时（`status=failed`）结果可能为空，不要自动重试。Token 用量计入父会话统计。

### 超时

子代理默认超时 300 秒（5 分钟），可通过 `--sub-agent-timeout` 参数或配置文件的 `sub_agent_timeout` 字段调整。超时后子代理被标记为 `failed`，父会话继续执行。

---

## Stream-JSON 输出

```bash
dscode -m flash --print "explain this"
```

每行一个 JSON 事件：

```json
{"type":"thinking","content":"Let me analyze..."}
{"type":"text","content":"Here is the explanation..."}
{"type":"tool_call","name":"Read","id":"...","input":{"path":"/x"}}
{"type":"tool_result","tool_use_id":"...","name":"Read","content":"..."}
{"type":"usage","input_tokens":100,"output_tokens":50}
{"type":"stop","reason":"end_turn"}
```

JQ 下游处理：

```bash
dscode -m flash --print "fix the bug" | jq 'select(.type=="text") | .content'
```

---

## 故障排查

```bash
# 检查 API key
curl -H "Authorization: Bearer $DEEPSEEK_API_KEY" https://api.deepseek.com/v1/models

# verbose 模式
dscode -m flash -v "hello"

# 扩大上下文窗口避免溢出
dscode -m flash --max-context 1M -i

# 查看 session 列表
dscode --list-sessions

# 查看事件日志中的信念变化
grep '"belief"' events.jsonl | jq '{type, belief}'

# 查看注入历史
grep '"Injecting hint"' events.jsonl
```
