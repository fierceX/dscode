# 使用手册

> 更新日期：2026-08-18

本文面向终端用户，覆盖 CLI 交互模式、配置参数、沙箱、session、计划、压缩、工具、技能和
常见工作流。Rust 库 / Python SDK 嵌入见 [嵌入与 SDK 使用](EMBEDDING.md)；机器协议
（`--print` / `--agent-jsonl`）见 [机器协议](PROTOCOL.md)；内置工具的完整参数、结果格式和
边界行为见 [工具参考](tools.md)；架构和内部模块职责见 [ARCHITECTURE.md](ARCHITECTURE.md)。

---

[TOC]

> 嵌入与 SDK：[EMBEDDING.md](EMBEDDING.md) · 机器协议：[PROTOCOL.md](PROTOCOL.md) ·
> Token 用量与费用：见 [EMBEDDING.md](EMBEDDING.md#token-用量与费用)

---

## 终端模式与操作

项目提供 REPL、Full TUI、Inline TUI 和非交互 CLI 四种使用模式。

### REPL 模式（`-i`）

基于 rustyline 的行编辑器，适合日常编码交互：

```
mink interactive mode (type 'exit' or Ctrl+D to quit)
> scan this project for Rust errors
[tool] Bash(command="cargo check")
...
```

- 输入：rustyline 行编辑（历史、Tab 补全、Ctrl+W/Del）
- 输出：stderr 渲染（灰色 thinking、黄色 tool call、普通 text）
- 标题栏：ANSI escape 更新终端窗口标题
- 历史记录：持久化到 `~/.mink/history`

### Full TUI（`--tui` / `--tui=full`）

使用 alternate screen 和应用内 transcript，支持鼠标滚动、工具卡片点击、自动折叠和展开，适合需要完整结构化操作的编码会话。

### Inline TUI（`--tui=inline`）

将完成的结构化内容渐进写入终端原生 scrollback，底部保留流式尾部、状态栏和输入区。
通过 terminal scrolling region 写入稳定内容，避免宽字符占位空格和 viewport 整体重绘，适合 SSH 和长日志：

```
flash B:0.73 T:12 R:45 I:200K(50%) O:20K C:400K(40%) ¥0.12 [idle]
────────────────────────────────────────────────────────────────
 原生 terminal scrollback（已完成对话和结构化工具卡片）
────────────────────────────────────────────────────────────────
 > 输入区域（多行输入）
```

**状态栏字段含义：**

| 字段 | 示例 | 含义 |
|------|------|------|
| `flash` | 模型名 | 当前模型 |
| `B:0.73` | 信念度 | 工具执行可靠性评分（0.0~1.0） |
| `T:12` | 对话轮次 | 当前对话的用户输入轮数 |
| `R:45` | API 请求 | 累计 LLM API 请求次数 |
| `I:200K` | 输入 tokens | 总输入 tokens，括号内为缓存命中率 |
| `O:20K` | 输出 tokens | 总输出 tokens |
| `C:400K` | 上下文 | 当前上下文 tokens，括号内为使用率 |
| `¥0.12` | 费用 | 累计费用（按模型单价实时计算） |
| `[idle]` | 工作状态 | idle / waiting / thinking / generating / tool / sub-agent / compacting / error |

**B（信念度）值含义：**

| B 值 | 含义 |
|------|------|
| 0.75 | 初始（信任先验） |
| > 0.7 | 顺利 |
| 0.5~0.7 | 偶有小错 |
| 0.3~0.5 | 频繁出错 |
| < 0.3 | 严重 |

两种 TUI 共用：
- 多行输入和 UTF-8 安全编辑，Ctrl+C 中断当前 turn
- 工具调用与结果按 ID 合并，显示退出状态、Plan/Todo 状态和 Artifact 元数据
- 语义工具着色、自动折叠和同一套 Markdown renderer（标题、列表、引用、代码块、表格、diff）
- Plan/Todo 详情从 session 状态文件加载；超长工具结果折叠后显示 `artifact://ID`
- `/artifact ID` 查看有界预览，Plan/Todo/Artifact 详情按内容宽度折行

模式差异：
- Full：鼠标捕获，应用内滚动，卡片可展开/折叠
- Inline：鼠标由终端处理，自动折叠后不可展开；详情临时使用 alternate screen

**TUI / REPL 操作：**（REPL 支持同样的 slash 命令）
| 操作 | 行为 |
|------|------|
| `Ctrl+C` | 工作中中断当前 turn；空闲时退出 |
| `/flash` / `/pro` | 切换模型 |
| `/compact` | 手动触发上下文压缩 |
| `/help` / `/skills` | 显示帮助或 skill 列表 |
| `/plan` / `/todos` | 打开 Plan/Todo 详情 |
| `/artifact ID` | 打开最多 256 KiB 的 Artifact 预览 |
| `/sub-agent ID` | 打开子代理详情 |
| `/exit` / `/quit` / `/q` | 退出 |
| 未知 `/xxx` | 本地提示，不发送给 LLM |
| 行首空格 + `/xxx` | 作为普通文本发送 |
| `Ctrl+D` | REPL 中退出 |

`/flash` / `/pro` 立即生效，不会发送给 LLM。

### 标题栏（REPL/CLI 模式）

终端窗口标题显示相同统计：`flash B:0.73 T:12 R:45 I:200K(50%) O:20K C:400K(40%) ¥0.12`
信念度在每次工具调用后实时更新，低于阈值时在同轮的下次 LLM 调用前注入提示或中止。

### 非交互 CLI 模式

```bash
# 单次查询
./target/release/mink -m flash "explain this"

# 管道输入
cat main.rs | ./target/release/mink -m flash "review"

# ndjson 结构化输出
./target/release/mink --print "list files"
```

prompt 为空且 stdin 是终端时自动进入交互模式。非终端 stdin 时读取 stdin 作为 prompt。

### Prefab 预置会话（`--prefab[=TEMPLATE]`）

`--prefab[=TEMPLATE]` 在 session 初始化后启用 Prefab 重组：检查 `events.jsonl` 是否已有 Prefab 特殊 `prefix_snapshot` 事件，没有则写入模板会话，并从该事件重建系统提示词。`TEMPLATE` 可以是内置名称（`pro` / `default` / `anchored-standard`、`flash` / `router-flash-weak`）或一个模板目录路径：

> 注意：Prefab 属于临时功能，后续 DeepSeek 更新模型后可能撤销该功能。

```bash
# 使用内置默认轨迹（pro / anchored-standard）
mink --prefab "review this repo"

# 使用 flash 模板
mink --prefab=flash "review this repo"

# 使用本地模板目录
mink --prefab=./my-prefab-template "review this repo"
```

- 需要构建时启用 `prefab` feature：`full-cli` 默认已包含；精简二进制需同时启用，例如 `--no-default-features --features "sdk-bin prefab"`。
- 新 session 会在初始化后由 Prefab 模块写入模板会话；已有 prefab 会话直接恢复，不会重复重组；对已有普通会话使用 `--prefab` 只补写标准 `prefix_snapshot` 事件，不修改 conversation。
- 生成的 `prefix_snapshot` 事件会让 prefab runtime 用它重建 system prompt + tools schema，而不是编译期 prompt builder。

### Flash 路由（`--router`）

`--router` 启用 `mink-router` 的 Flash 推理模式路由。对于 Flash 模型，**建议直接使用 `--router`，不需要再叠加 `--prefab=router-flash-weak`**：

```bash
# 仅路由（推荐）
mink --router "修复这个 bug"

# TUI 中使用
mink --tui --router
```

> 注意：`--router --prefab=router-flash-weak` 组合仅用于实验/兼容验证，不作为推荐用法。
> Prefab 预热轨迹会额外占用上下文和 TUI transcript，而 Router 已自带 Flash persona、近场引导和工具面渐进暴露。

- 需要构建时启用 `router` feature；`full-cli` 默认已包含。
- 非 Flash 模型自动透传，不干预。

---

## 配置与参数

### CLI 参数

| 参数 | 默认值 | 说明 |
|------|--------|------|
| `PROMPT` | — | 用户输入（位置参数） |
| `-m` / `--model` | `flash` | 模型名。`flash` / `pro` 是默认别名，也可直接指定任意 OpenAI-compatible 模型名 |
| `--mission PATH` | — | 加载 MISSION.md |
| `--prefab[=TEMPLATE]` | `default` | 启用 Prefab：session 初始化后重组 session，并从 `events.jsonl` 的 `prefix_snapshot` 事件重建前缀；`TEMPLATE` 为内置模板名或模板目录路径（需要 `prefab` feature） |
| `--router[=flash]` | — | 启用 Flash 推理模式路由（需要 `router` feature；`full-cli` 默认包含） |
| `--session NAME` | 自动生成 | 命名会话 |
| `--continue` | — | 恢复最近的 session |
| `--list-sessions` | — | 列出所有 session |
| `--list-skills` | — | 列出可用 skill |
| `--skill NAME` | — | 选择要加载的 skill；可重复传入，按传入顺序去重 |
| `-i` / `--interactive` | auto | REPL 交互模式 |
| `--tui` / `--tui=full` | — | Full TUI |
| `--tui=inline` | — | Inline TUI |
| `--print` | — | ndjson 结构化输出，事件格式见 [机器协议](PROTOCOL.md) |
| `--agent-jsonl` | — | Agent JSONL 协议（stdin request，stdout 事件流 + final），详见 [机器协议](PROTOCOL.md) |
| `--api-key KEY` | env | 覆盖 API Key |
| `--base-url URL` | 默认端点 | 覆盖 API 端点 |
| `--enabled-tools <list>` | 默认集合 | 逗号分隔的精确工具列表；`none` 禁用全部 |
| `--edit-mode <mode>` | `hashline` | `hashline` / `replace`；runtime 启动后固定 |
| `--edit-fuzzy-match <bool>` | `true` | Replace 行窗口模糊匹配开关 |
| `--edit-fuzzy-threshold <n>` | `0.95` | Replace 阈值，有限数且在 `0.0..=1.0` |
| `--edit-enforce-seen-lines <bool>` | `false` | Hashline 是否强制锚点已由 Read/Grep 展示 |
| `--config <toml>` | — | TOML 字符串设置配置 |
| `-v` / `--verbose` | `false` | 详细日志 |

### `--config` TOML 格式

中低频参数通过 `--config` 传递：

```toml
[provider]
model = "flash"
api_key = "sk-xxx"
base_url = "https://api.deepseek.com/v1"
openai_reasoning_effort = "max"
openai_include_usage = true
openai_token_param = "max_tokens"
openai_tool_choice = "auto"

[provider.model_aliases]
flash = "deepseek-v4-flash"
pro = "deepseek-v4-pro"

[provider.openai_extra_body]
custom_boolean = true
custom_budget = 8192

[generation]
max_tokens = 4096
max_turns = 20
llm_first_event_timeout = 60
llm_idle_timeout = 90
llm_wait_heartbeat = 30
output_format = "stream-json"

[context]
max_context = "500K"
context_compact_pct = 94
context_reserve_tokens = 64000
context_compact_tail_tokens = 256000
context_compact_max_output_tokens = 8192
context_compact_input_reduction = false

[tools]
tool_timeout = 300
tool_timeout_max = 600
sub_agent_timeout = 120
max_search_files = 5000
max_search_results = 1000
enabled_tools = ["Read", "Write", "Edit", "Grep", "Glob", "Bash"]
approval_mode = "write"
skills = ["python", "debugging"]

[tools.edit]
mode = "hashline"
fuzzy_match = true
fuzzy_threshold = 0.95
enforce_seen_lines = false

[signal]
policy = "full" # off / evidence / state_ops / restart / full

[sandbox_python]
wasm_path = "/path/to/python.wasm"
read_dirs = ["./data"]
```

旧的顶层扁平字段不再接受；解析器会报告 unknown field。算法阈值、先验、衰减、证据长度、冷却和恢复限制是内部策略，不属于配置协议。
`--agent-jsonl` 模式不会读取 `.minkrc`，但仍应用命令行 `--config`。

#### 信号系统（分层响应模型）

信号系统的响应按信念度分层（设计依据见 `docs/设计哲学-信号系统.md`）：

- **记录不干预**：软信号（regex 嗅探类）单独出现且信念尚可时，只记录不改行为；
- **证据注入**：信念进入提醒区后注入 `[trajectory]`/`[detector]` 轨迹事实
  （重复调用、失败聚类、预算消耗），不注入命令；
- **状态操作**：警告区把循环窗口内编辑过的文件回滚到最近快照，并启用恢复首步守卫
  （拦截会喂回信念，连续拦截达 `guard_max_blocks` 后绕过并强制证据注入）；
- **策略重启**：同一输入内连续第 2 次警告，或非交互环境下信念跌破 abort 阈值时，
  以 fresh 子代理（不继承父对话）重新规划后继续；
- **用户接管**：交互环境下信念跌破 abort 阈值时，输出结构化接管报告
  （证据/编辑路径/选项）并返回失败，等待用户重锚定。

`MINK_SIGNAL_POLICY=off` 关闭全部信号采集、证据注入、回滚、接管与守卫。

### 配置文件

`~/.minkrc`（用户级）和 `<project>/.minkrc`（项目级）可选配置。
优先级：CLI 参数 > `--config` TOML > 项目 `.minkrc` > 用户 `~/.minkrc` > 环境变量 > 默认值。
例外：`MINK_LIMITS`（JSON sandbox 限制）仍是 CLI 之后最高优先级，高于所有配置文件；
4 个 `MINK_EDIT_*` 变量则低于全部文件层（CLI > `--config` > 项目 `.minkrc` > 用户
`~/.minkrc` > env > 默认）。
环境变量在文件层之前应用，因此 `[tools]` / `[generation]` 等文件配置会覆盖同名环境变量。

```toml
# ~/.minkrc
[provider]
api_key = "sk-xxx"
base_url = "https://api.deepseek.com/v1"
model = "flash"
openai_reasoning_effort = "max"
openai_include_usage = true
openai_token_param = "max_tokens"
openai_tool_choice = "auto"

[provider.model_aliases]
flash = "deepseek-v4-flash"
pro = "deepseek-v4-pro"

[generation]
max_tokens = 81920
max_turns = 40
llm_first_event_timeout = 60
llm_idle_timeout = 90
llm_wait_heartbeat = 30
log_events = true

[context]
max_context = "1M"
context_compact_pct = 94
context_reserve_tokens = 64000
context_compact_tail_tokens = 256000
context_compact_max_output_tokens = 8192
context_compact_input_reduction = false
plan_projection_tail = true

[tools]
tool_timeout = 600
tool_timeout_max = 600
sub_agent_timeout = 120
max_search_files = 5000
max_search_results = 1000
enabled_tools = ["Read", "Write", "Edit", "Grep", "Glob", "Bash"]
approval_mode = "yolo"               # yolo | write | always-ask

[tools.edit]
mode = "hashline"
fuzzy_match = true
fuzzy_threshold = 0.95
enforce_seen_lines = false

[tools.approval]
Bash = "prompt"                      # allow | deny | prompt
Read = "allow"

[signal]
policy = "full"                      # off | evidence | state_ops | restart | full
```

`tool_timeout` 是 Bash/Python/自定义工具未显式指定 `timeout` 时的默认值；`tool_timeout_max`
是单次工具调用的硬上限（默认 600 秒）。显式 `timeout` 或默认值超过该上限时 fail closed /
钳制到上限，`tool_timeout_max` 最低可设为 5 秒。

`openai_extra_body` 会合并到 `/chat/completions` 请求体中。`model`、`messages`、`stream`、`tools`、`tool_choice`、`max_tokens`、`max_completion_tokens` 不会被 extra body 覆盖。

**`enabled_tools` 是模型工具 surface 的唯一输入。** 未设置时使用 catalog 默认集合；未知名称、重复名称、缺少硬依赖或 feature 未编译的工具会在创建 session 前报错。`PythonSandbox` 必须显式列出。最终 schemas、按需 tool/workflow prompt、Bash 路由、Signal Recovery 和真实执行门禁同时依据该 surface 收缩，不存在额外 disable flag 或 sandbox 工具策略。
当前版本尚未实现交互式审批 prompt。默认 `yolo` 允许所有 approval tiers。

### 环境变量

| 变量 | 默认值 | 说明 |
|------|--------|------|
| `DEEPSEEK_API_KEY` | — | **必需。** API 密钥 |
| `DEEPSEEK_BASE_URL` | `https://api.deepseek.com/v1` | 自定义 API 端点 |
| `TOOL_RESULT_MAX_BYTES` | `100000` | 单条工具结果截断上限 |
| `FILE_WRITE_MAX_BYTES` | `1048576` | Write/Edit 工具写入上限 |
| `MAX_SEARCH_FILES` | `5000` | Glob/Grep 最大遍历文件数 |
| `MAX_SEARCH_RESULTS` | `1000` | Grep 最大匹配结果行数 |
| `LOG_EVENTS` | `true` | 设为 `0`/`false` 关闭 events.jsonl |
| `MINK_SIGNAL_POLICY` | `full` | `off` / `evidence` / `state_ops` / `restart` / `full` |
| `MINK_EDIT_MODE` | `hashline` | Edit 协议：`hashline` / `replace` |
| `MINK_EDIT_FUZZY_MATCH` | `true` | Replace 模糊匹配开关 |
| `MINK_EDIT_FUZZY_THRESHOLD` | `0.95` | Replace 模糊阈值，`0.0..=1.0` |
| `MINK_EDIT_ENFORCE_SEEN_LINES` | `false` | Hashline seen-line 守卫 |
| `MINK_HOME` | `$HOME` | session 存储目录覆盖 |
| `MINK_LIMITS` | — | JSON sandbox 限制配置 |

搜索上限多层保护：`MAX_SEARCH_FILES`（文件遍历）、`MAX_SEARCH_RESULTS`（匹配行数）、
工具自身 100KB 输出保护、`TOOL_RESULT_MAX_BYTES` 最终截断。
`scanned first N files` = 文件数上限触发，`truncated at N results` = 匹配数上限触发，
`output > 100000 bytes` 或 artifact 提示 = 字节数保护触发。

`MINK_SIGNAL_POLICY=full` 时，低 belief 注入会要求 Recovery 首步先检查状态。Recovery 首步资格是独立的参数级能力判断，不等同于普通 Bash 安全策略。

---

## mink-server：Web 工作区服务器

单二进制 Web 服务（REST + SSE + 嵌入前端），与 TUI/CLI 共享同一 `~/.mink/projects` 会话目录，终端与浏览器可无缝交接。

```bash
cargo build -p mink-server        # build.rs 自动构建并嵌入前端
./target/debug/mink-server        # 默认 8765 端口，读取 ~/.minkrc
MINK_SERVER_PORT=9000 ./target/debug/mink-server
MINK_SERVER_DEV_WEB=1 ./target/debug/mink-server   # 开发模式：服务磁盘 web/dist
```

- 配置优先级：环境变量 > `mink-server.toml` > `~/.minkrc` > 默认
- 关键环境变量：`MINK_SERVER_HOST/PORT`、`MINK_HOME`、`MODEL`、`MINK_SERVER_MAX_RUNNING`、`MINK_SERVER_TURN_TIMEOUT`、`MINK_SERVER_DEV_WEB`
- REST/SSE API 与事件格式详见 [server.md](server.md)

## 沙箱与安全

### 进程级沙箱

通过 OS 原生工具（Linux nsjail/bubblewrap、macOS sandbox-exec）包裹 Mink 进程。

`.minkrc` 的 `[sandbox]` 段：

```toml
[sandbox]
enabled = true
backend = "auto"                 # nsjail | bwrap | sandbox-exec | off
read_dirs = ["src", "tests"]
write_dirs = ["src"]
allow_network = true

# 仅 Linux nsjail cgroup：
max_memory_mb = 1024
max_pids = 64
timeout_secs = 600
```

### 平台差异

| 功能 | Linux nsjail | Linux bwrap | macOS sandbox-exec |
|------|-------------|-------------|---------------------|
| 写入限制 | ✅ 内核强制 | ✅ 内核强制 | ✅ 内核强制 |
| 读取限制 | ✅ 内核强制 | ✅ 内核强制 | ❌ 不生效 |
| 网络隔离 | ✅ namespace | ✅ namespace | ❌ 不生效 |
| memory/pids 资源限制 | ✅ cgroup | ❌ 不执行 | ❌ 不执行 |
| 后台自动启用 | ✅ | ✅ | ✅（写入限制） |

### 启动机制

Mink 检测 `[sandbox] enabled = true` 后自动通过 `exec()` 装入沙箱，设置 `MINK_SANDBOXED=1` 防无限递归。
不可用的后端会 fatal 退出，不会静默降级。

### PythonSandbox（CPython WASI 沙箱）

在 wasmtime + CPython WASI 中执行 Python，WASI 级进程隔离，无网络、无子进程、无 C 扩展。

**准备工作：** 下载 [cpython-wasi-build](https://github.com/brettcannon/cpython-wasi-build) 发布的 Python 3.13+ WASI 包：

```bash
curl -sL "https://github.com/brettcannon/cpython-wasi-build/releases/download/v3.13.13/python-3.13.13-wasi_sdk-24.zip" -o python-wasi.zip
unzip python-wasi.zip -d cpython-wasi
```

项目结构：
```
cpython-wasi/
├── python.wasm          # ~29MB
├── lib/python3.13/
└── LICENSE
```

配置：

```toml
[tools]
enabled_tools = ["Read", "Write", "Bash", "PythonSandbox", ...]

[sandbox_python]
wasm_path = "cpython-wasi/python.wasm"
stdlib_dir = "cpython-wasi"
timeout = 30
read_dirs = ["./data"]
write_dirs = ["./output"]
package_dirs = ["./packages"]
```

`enabled_tools` 是精确列表；`PythonSandbox` 必须显式列出。

路径权限规则：
- 仅在 `read_dirs` / `write_dirs` 中声明的目录可访问
- `write_dirs` 优先于 CWD 只读
- 路径穿越（`../`）无法逃逸 preopen 范围

---

## 会话管理

### Session layout

`MINK_HOME`（默认 `$HOME`）是 session 持久化根目录。不同入口使用不同 layout：

| Layout | 最终 session 目录 | 默认入口 |
|--------|-------------------|----------|
| `project` | `HOME/.mink/projects/<project_key>/<session_id>/` | CLI、裸 `mink-core` |
| `home` | `HOME/.mink/sessions/<session_id>/` | Python SDK |
| `direct` | `HOME/<session_id>/` | 显式配置 |
| `isolated` | `HOME/` | Rust `AgentOptions` |

选择建议：终端用户用 `project`（按项目隔离）；Python SDK 用 `home`；Rust API 按任务建独立目录用 `isolated`；共享 Mink 根目录用 `direct`。

### 目录结构

```
~/.mink/
├── history
└── projects/<project_key>/
    └── <session_id>/
        ├── conversation.jsonl ← 对话消息（JSONL 追加）
        ├── events.jsonl       ← 事件日志
        ├── session.json       ← 元数据：alias、title、时间戳
        ├── summary.txt        ← 压缩上下文快照
        ├── stats.json         ← Token 统计
        ├── context-state.json ← 首次压缩后生成
        ├── plan.md            ← 确认计划
        ├── plan.draft         ← 未确认草稿
        ├── todos.json         ← 首次 Todo 变更后生成
        ├── usage.jsonl        ← 首次 LLM 请求后生成
        └── artifacts/
            ├── index.jsonl
            └── <tool>-0001.txt
```

### 操作

```bash
mink -m flash --session my-fix "fix the bug"   # 命名会话
mink -m flash --session my-fix -i               # 恢复命名会话
mink -m flash --continue -i                     # 恢复最近会话
mink --list-sessions                            # 列出所有 session
```

`--session my-fix` 按 alias、完整 id、id 前缀和 title 匹配已有 session；匹配不到时创建新 session 并写入 alias。
值收紧：`--session` 后必须跟非空且不以 `-` 开头的参数（`--session` 裸用或 `--session -x`
直接报 missing value，避免与后续选项混淆）。
`--continue` 选择最近修改的 session，恢复时 replay 最近 10 轮 LLM 响应。

---

## 计划系统（Plan）

三个内置工具管理计划生命周期；三者都进入 resolved tool surface 时才加载计划工作流。

### 生命周期

```
LLM 提议 → PlanDraft(草稿) → 用户确认 → PlanConfirm → <current-plan> 动态注入
  → TodoRead/TodoWrite/TodoAdvance 执行 → PlanClear → 移除动态计划
```

- `PlanDraft(content)`：保存或取消草稿（空 content = 取消）。已确认计划存在时拒绝创建。
- `PlanConfirm()`：原子 rename `plan.draft → plan.md` + 请求压缩。下一轮 LLM 请求注入 `<current-plan>`。
- `PlanClear()`：删除 `plan.md` + 清理残留 `plan.draft` + 请求压缩。下一轮移除动态计划。

PlanConfirm/PlanClear 的压缩请求服从 TurnCompactor 同轮一次守卫，失败会返回当前 turn。
执行阶段的 Todo 工具协议见 [工具系统 · Todo 协议](#todo-协议)。

---

## 上下文压缩

### 显式策略

所有参数显式配置，不根据窗口大小推断档位。统一使用 LLM 滚动摘要：

| 参数 | 默认值 | 作用 |
|------|-------:|------|
| `context_compact_pct` | 94 | 自动压缩百分比（1-100） |
| `context_reserve_tokens` | 64000 | 主请求响应预留，同时限制 max_tokens |
| `context_compact_tail_tokens` | 256000 | 压缩后保留的热尾部目标 |
| `context_compact_max_output_tokens` | 8192 | 摘要输出上限 |
| `context_compact_input_reduction` | false | 压缩 think 和工具噪声 |
| `plan_projection_tail` | true | 已确认计划尾置投影（最后一条消息），计划修订不失效前缀缓存；`false` 回退前置投影 |

触发点取百分比阈值和 `max_context - context_reserve_tokens` 中较早者。
`max_context_tokens=0` 禁用 auto/preflight 压缩，保留 `/compact`。

### 流程

1. 从活跃窗口选择不破坏 tool call/result 配对的边界
2. 可选降噪：删除 thinking，压缩工具参数和结果
3. 使用最小摘要 prompt，LLM 合并新旧历史
4. 原子提交 `context-state.json`（临时文件 + rename）
5. 裁剪运行时缓存到新活跃边界
6. `conversation.jsonl` 保持完整且只追加

### 防护

- 同轮只压缩一次；最小收益检查（节省 < 10% 跳过）
- Preflight 预判：发送前按实际形态估算，超预算先压缩
- 摘要使用独立输出预算，发送前校验能否装入窗口
- 配置组合校验：reserve < 窗口，摘要输出 < 窗口，热尾部 < 主请求预算
- 降噪只作用于摘要请求，不修改完整历史
- Provider overflow 恢复：无可见输出时最多一次压缩 + 一次重试

### 调优

```toml
# 1M DeepSeek
[context]
max_context = "1M"
context_compact_pct = 94
context_reserve_tokens = 64000
context_compact_tail_tokens = 256000

# 64k 私有模型（必须同时缩小所有预算）
# max_context = "64K"
# context_compact_pct = 65
# context_reserve_tokens = 12000
# context_compact_tail_tokens = 16000
# context_compact_max_output_tokens = 4096
```

仅修改 `max_context` 不调整 reserve/tail 会在启动时因参数冲突失败。

---

## 维修流水线

每次工具执行前自动运行三段修复，无需人工干预：

- **Scavenge**：从 `reasoning_content` 和文本回复中回收遗漏的工具调用（支持 DSML/XML/
  Bracket/裸 JSON/OpenAI/R1 等容器格式）
- **Truncation**：修复截断的 JSON 参数（闭合引号、补全括号、去尾逗号）
- **StormBreaker**：滑动窗口检测 `(工具名, 参数)` 重复，同一对出现 3 次时抑制调用；
  mutating 工具执行时清空只读条目，允许 edit→re-read 正常模式

完整设计见 [DESIGN.md](DESIGN.md#主题四维修流水线)。

---

## 工具系统

### 工具列表

| 工具 | 用途 | 关键参数 |
|------|------|---------|
| `Read` | 读文件或轻量资源，支持 selector | `path` |
| `Write` | 写文件 | `path`, `content` |
| `Edit` | runtime 固定的 Hashline 或 Replace 编辑 | Hashline: `input`；Replace: `path`, `edits` |
| `Bash` | 执行命令 | `command`, `timeout` |
| `Python` | 运行 Python（宿主环境，完整生态） | `script` / `script_file`, `timeout` |
| `PythonSandbox` | WASI 沙箱 Python（受限，需显式列出） | `script` / `script_file`, `timeout` |
| `Glob` | 文件匹配 | `pattern`, `path` |
| `Grep` | 内容搜索 | `pattern`, `path`, `glob`, `context` |
| `TodoRead` | 读取 checklist 快照 | `include_completed` |
| `TodoWrite` | 原子修改 checklist 结构 | `base_revision`, `add`, `update`, `remove` |
| `TodoAdvance` | 原子转换进度 | `base_revision`, `complete`, `activate`, `pause`, `reopen` |
| `PlanDraft` | 保存或取消计划草稿 | `content` |
| `PlanConfirm` | 确认计划 | 无参数 |
| `PlanClear` | 清空计划 | 无参数 |
| `SubAgent` | 启动子代理 | `prompt`, `description`, `fork` |
完整工具参数、结果格式和边界行为见 [工具参考](tools.md)。

### 工具选择与模型工具面

工具可见性由 **`enabled_tools`** 唯一决定：它是 CLI、TOML、Rust API、Agent JSONL 与
Python SDK 共用的唯一工具选择入口。未设置时使用 catalog 默认集合，空列表禁用全部；
`PythonSandbox` 必须显式列出。surface 解析、语义能力和工作流组合的设计见
[工具能力与提示词解耦设计文档](设计哲学-工具能力与提示词解耦.md)。

### Read selector 与资源 URL

`Read.path` 支持行 selector（`src/main.rs:40-80`、`:40+20`、`:raw` 等），并可读取轻量
资源 URL（`artifact://`、`skill://`、`rule://`、`session://`）。完整协议见
[工具参考](tools.md)。

### Edit 双模式

`edit_mode` 在 runtime 创建时固定，恢复 session 时可重新选择，但不会翻译历史工具调用。
默认 Hashline 模式让本地 Read/Grep 输出 `[PATH#TAG]`，支持跨文件 section、调用内匿名剪贴板、session 命名寄存器、
历史 snapshot 和安全 stale 恢复；Replace 模式保持普通 Read/Grep 输出，以唯一 `old_text`
执行 exact/行窗口 fuzzy 匹配，并在歧义时拒绝。schema、system prompt 和 executor 总是来自同一 resolved
mode；切换模式会改变 immutable prefix fingerprint。完整协议见 [工具参考](tools.md)。

### Todo 协议

权威快照保存在 session `todos.json`，使用 revision + 稳定 ID 防 stale write：
- `TodoRead`：读取完整快照、revision 和稳定 ID
- `TodoWrite`：原子新增 pending 条目、删除条目或替换正文
- `TodoAdvance`：原子转换 active batch 进度（activate / complete / pause / reopen）

成功变更后，在 conversation 尾部追加增量事件和当前 active batch 的紧凑物化投影。
session 恢复或压缩丢失最新 revision 时追加一次 TodoSync。一个 session 的 TodoStore 由单个 runtime 持有，不支持并发写。

### SubAgent 工具

LLM 自动调用，支持最多 8 个并发。模式、参数和输出格式详见
[SubAgent（子代理）](#subagent子代理)。

---

## Skills（技能）

### 启用

```bash
# CLI 加载（可重复 --skill；与 .minkrc 的 [tools].skills 等价）
mink -m flash --skill debugging --skill tdd -i

# 查看可用
mink --list-skills
```

### 内置技能（编译时嵌入）

所有 `skills/<name>/SKILL.md` 在编译时嵌入二进制，零文件 I/O：

| 技能名 | 描述 | 适用场景 |
|--------|------|---------|
| `debugging` | 四阶段系统调试 | 遇到 bug、测试失败、非预期行为 |
| `verification` | 验证门控：禁止未验证就声称完成 | 完成任务、commit 前 |
| `tdd` | 红绿重构循环 | 新功能或修 bug |
| `pre-code-check` | 先搜索调用点、读上下文、验证假设 | 编辑文件前 |

### 搜索路径（优先级）

1. `<project>/.claude/skills/<name>/SKILL.md` — 项目级覆盖
2. `<project>/skills/<name>/SKILL.md` — 项目开发目录
3. `~/.claude/skills/<name>/SKILL.md` — 用户全局
4. **内置** — 编译时嵌入，兜底

### 能力视图

每次 runtime 启动构建 `CapabilitySnapshot`。system prompt 的 skill index、selected skills、instruction files、rules 以及 `Read skill://` / `Read rule://` 都读取这份统一视图。CLI、Rust runtime、Python SDK 和子代理不各自重新扫描。

---

## MISSION（自定义系统提示词）

通过 `--mission PATH` 加载。MISSION 可覆盖少量稳定的 core section，可追加自定义 section；
**不能**覆盖工具、workflow 或 runtime-owned 内容。

### Section 分类

MISSION.md 使用行首一级标题（`# section-id`）分段。section 分三类：

| 类型 | section ID | 行为 |
|------|------------|------|
| 可覆盖 core | `system-conventions`、`agent-identity`、`environment`、`execution-codes`、`belief-awareness`、`output-language` | 替换同名 core |
| runtime-reserved | 工具 prompt、workflow、`runtime-capabilities`、`tool-inventory`、`rules`、`instruction-files`、`rule-index`、`skill-index`、`selected-skills`、`current-plan` 等 | 启动时 fail fast |
| 普通自定义 | 不属于以上两类的唯一 ID | 作为 `mission:<section-id>` 原样追加 |

```markdown
# agent-identity
你是文档处理助手，负责根据素材文件生成结构化文档。

# mission-rules
- 严格遵循素材内容，不得额外杜撰

# process-flow
## Phase 1: 素材分析
...
```

### 用法

```bash
mink --mission ./my-task.mission.md -i
mink --mission ./my-task.mission.md --config $'[tools]\nskills=["debugging"]' -i
```

Python SDK 使用 `SandboxConfig(mission_file=...)` 或内联 `mission_content`，见
[嵌入与 SDK 使用](EMBEDDING.md)。

### 迁移规则

- 旧 `# rules` → 改为 `# mission-rules` 或其他业务 ID
- 不再支持 `using-your-tools`、`anchored-edit-protocol`、`rationalization-table` 等旧 alias
- section ID 必须唯一；重复 heading 或占用 runtime-reserved ID 导致启动失败
- MISSION 只影响 prompt 文本，不改变工具 surface（工具选择仍用 `enabled_tools`）
- `MINK_SIGNAL_POLICY=off` 时 prompt 不存在 `belief-awareness`，MISSION 也不能创建它

---

## SubAgent（子代理）

### 参数

| 参数 | 说明 |
|------|------|
| `prompt` | 子代理任务描述（**必需**） |
| `description` | 日志标记（可选） |
| `fork` | 是否继承父会话上下文（可选，默认独立） |

### 模式

- **独立（默认）**：全新空会话，适合文件调查、搜索、隔离验证
- **Fork（`fork=true`）**：继承完整 session 状态（对话、压缩边界、摘要、计划、artifact），适合延续性任务

Fork 在子 runtime 初始化前复制父 session 目录，清除 child 的身份/事件/统计文件。
子代理从克隆的 `context-state.json` 恢复活跃投影；artifact 序号从克隆 index 继续。

### 输出

```
[sub-agent <id>] <status> (in=<n>, out=<n>)
Thinking: ...
Text: ...
```

Token 用量计入父会话统计。默认超时 300 秒（`sub_agent_timeout` 可调）。超时后标记为 `failed`。

---

## 故障排查

```bash
# 检查 API key
curl -H "Authorization: Bearer $DEEPSEEK_API_KEY" https://api.deepseek.com/v1/models

# verbose 模式
mink -m flash -v "hello"

# 扩大上下文窗口避免溢出
mink -m flash --config $'[context]\nmax_context="1M"' -i

# 查看 session 列表
mink --list-sessions

# 查看事件日志中的信念变化
grep '"belief"' events.jsonl | jq '{type, belief}'

# 查看前缀快照（system prompt + tools 指纹，用于离线重建请求前缀）
grep '"prefix_snapshot"' events.jsonl | jq '{version, fingerprint, dependency_fingerprint}'

# 查看请求级缓存明细（缓存命中率可算：兼容拼写路径已通，原生拼写为兜底）
cat usage.jsonl | jq '{input_tokens, cache_read_tokens, cache_creation_tokens}'

# 查看注入历史（轨迹证据注入落在 conversation.jsonl）
grep '\[trajectory\]' conversation.jsonl
```

### 缓存指标观测

DeepSeek 的 context caching 用量通过 OpenAI 兼容字段回传：
`prompt_tokens_details.cached_tokens`（DeepSeek 返回的就是这个兼容拼写，
不是原生 `prompt_cache_hit_tokens`）。mink 的解析链以兼容拼写优先、原生拼写
兜底（`prompt_cache_hit_tokens`），未命中部分隐含在
`input_tokens = prompt_tokens - cache_read_tokens` 的减法中。

**缓存命中率可正常计算**：命中率 = 累计 `cache_read_tokens` / 累计
`prompt_tokens`；标题栏 `I:` 字段括号内即该比率。前缀命中要求 system prompt、
tools 与历史消息前缀字节级稳定——计划修订会改变其后的全部前缀，因此
`plan_projection_tail=true`（默认）把计划放在最后一条消息，修订只影响自身位置；
`prefix_snapshot` 事件（events.jsonl）可用于离线重建请求前缀并归因
"这次为什么 cache miss"。
