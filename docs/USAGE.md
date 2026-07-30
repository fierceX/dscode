# 使用手册

> 更新日期：2026-07-30

本文面向 mink 使用者，覆盖 CLI、Rust 嵌入、Python SDK 的运行方式、配置、沙箱、session、技能和常见工作流。
内置工具的完整参数、结果格式和边界行为见 [工具参考](tools.md)；架构和内部模块职责见 [ARCHITECTURE.md](ARCHITECTURE.md)；
工具与提示词解耦、自由组合见 [工具能力与提示词解耦设计文档](设计哲学-工具能力与提示词解耦.md)。

---

## 目录

- [终端模式与操作](#终端模式与操作)
- [配置与参数](#配置与参数)
- [沙箱与安全](#沙箱与安全)
- [Token 用量与费用](#token-用量与费用)
- [Rust 库嵌入](#rust-库嵌入)
- [会话管理](#会话管理)
- [计划系统](#计划系统)
- [上下文压缩](#上下文压缩)
- [维修流水线](#维修流水线)
- [工具系统](#工具系统)
- [Skills（技能）](#skills技能)
- [MISSION（自定义系统提示词）](#mission自定义系统提示词)
- [SubAgent（子代理）](#subagent子代理)
- [Stream-JSON 输出](#stream-json-输出)
- [故障排查](#故障排查)

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

**TUI 操作：**

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

### 交互命令（REPL / TUI）

| 命令 | 说明 |
|------|------|
| `/flash` | 切换到 flash 模型 |
| `/pro` | 切换到 pro 模型 |
| `/compact` | 强制上下文压缩 |
| `/skills` | 列出所有可用 skill |
| `/help` | 显示可用命令列表 |
| `exit` / `quit` | 退出 |
| `Ctrl+C` | 取消当前正在执行的 turn |
| `Ctrl+D` | 退出 |

`/flash` 和 `/pro` 立即生效，不会发送给 LLM。

---

## 配置与参数

### CLI 参数

| 参数 | 默认值 | 说明 |
|------|--------|------|
| `PROMPT` | — | 用户输入（位置参数） |
| `-m` / `--model` | `flash` | 模型名。`flash` / `pro` 是默认别名，也可直接指定任意 OpenAI-compatible 模型名 |
| `--mission PATH` | — | 加载 MISSION.md |
| `--session [NAME]` | 自动生成 | 命名会话 |
| `--continue` | — | 恢复最近的 session |
| `--list-sessions` | — | 列出所有 session |
| `--list-skills` | — | 列出可用 skill |
| `-i` / `--interactive` | auto | REPL 交互模式 |
| `--tui` / `--tui=full` | — | Full TUI |
| `--tui=inline` | — | Inline TUI |
| `--print` | — | ndjson 结构化输出 |
| `--agent-jsonl` | — | Agent JSONL 协议（stdin request，stdout 事件流 + final） |
| `--api-key KEY` | env | 覆盖 API Key |
| `--base-url URL` | 默认端点 | 覆盖 API 端点 |
| `--enabled-tools <list>` | 默认集合 | 逗号分隔的精确工具列表；`none` 禁用全部 |
| `--config <toml>` | — | TOML 字符串设置配置 |
| `-v` / `--verbose` | `false` | 详细日志 |

### `--config` TOML 格式

中低频参数通过 `--config` 传递：

```toml
# 标量字段
max_tokens = 4096
max_turns = 20
max_context = "500K"
context_compact_pct = 94
context_reserve_tokens = 64000
context_compact_tail_tokens = 256000
context_compact_max_output_tokens = 8192
context_compact_input_reduction = false
tool_timeout = 300
sub_agent_timeout = 120
llm_first_event_timeout = 60
llm_idle_timeout = 90
llm_wait_heartbeat = 30
max_search_files = 5000
max_search_results = 1000
output_format = "stream-json"
enabled_tools = ["Read", "Write", "Edit", "Grep", "Glob", "Bash"]
approval_mode = "write"
skills = ["python", "debugging"]
openai_reasoning_effort = "max"          # "off" 表示不发送
openai_include_usage = true
openai_token_param = "max_tokens"        # 或 max_completion_tokens
openai_tool_choice = "auto"              # auto / none / required，或 JSON 对象

[openai_extra_body]
custom_boolean = true
custom_budget = 8192

[model_aliases]
flash = "deepseek-v4-flash"
pro = "deepseek-v4-pro"
local = "private-model-v1"

[sandbox_python]
wasm_path = "/path/to/python.wasm"
read_dirs = ["./data"]
```

`model_aliases` 可覆盖默认别名；未命中别名的 `model` 作为真实模型名原样发送。
`--agent-jsonl` 模式不会读取 `.minkrc`，但仍应用命令行 `--config`。

### 配置文件

`~/.minkrc`（用户级）和 `<project>/.minkrc`（项目级）可选配置。
优先级：CLI 参数 > 项目配置 > 用户配置 > 环境变量 > 默认值。

```toml
# ~/.minkrc
api_key = "sk-xxx"
base_url = "https://api.deepseek.com/v1"
model = "flash"
max_tokens = 81920
max_turns = 40
max_context = "1M"
tool_timeout = 600
sub_agent_timeout = 120
llm_first_event_timeout = 60
llm_idle_timeout = 90
llm_wait_heartbeat = 30
context_compact_pct = 94
context_reserve_tokens = 64000
context_compact_tail_tokens = 256000
context_compact_max_output_tokens = 8192
context_compact_input_reduction = false
log_events = true
max_search_files = 5000
max_search_results = 1000
enabled_tools = ["Read", "Write", "Edit", "Grep", "Glob", "Bash"]
openai_reasoning_effort = "max"
openai_include_usage = true
openai_token_param = "max_tokens"
openai_tool_choice = "auto"

[model_aliases]
flash = "deepseek-v4-flash"
pro = "deepseek-v4-pro"

[tools]
approval_mode = "yolo"               # yolo | write | always-ask

[tools.approval]
Bash = "prompt"                      # allow | deny | prompt
Read = "allow"
```

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
| `MINK_SIGNAL_MODE` | `full` | 信号模式：`full` 启用信念跟踪和注入；`off` 关闭 |
| `MINK_HOME` | `$HOME` | session 存储目录覆盖 |
| `MINK_LIMITS` | — | JSON sandbox 限制配置 |

搜索上限多层保护：`MAX_SEARCH_FILES`（文件遍历）、`MAX_SEARCH_RESULTS`（匹配行数）、
工具自身 100KB 输出保护、`TOOL_RESULT_MAX_BYTES` 最终截断。
`scanned first N files` = 文件数上限触发，`truncated at N results` = 匹配数上限触发，
`output > 100000 bytes` 或 artifact 提示 = 字节数保护触发。

`MINK_SIGNAL_MODE=full` 时，低 belief 注入会要求 Recovery 首步先检查状态。Recovery 首步资格是独立的参数级能力判断，不等同于普通 Bash 安全策略。

---

## 沙箱与安全

### 进程级沙箱

通过 OS 原生工具（Linux nsjail/bubblewrap、macOS sandbox-exec）包裹 mink 进程。

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

| 功能 | Linux (nsjail/bwrap) | macOS (sandbox-exec) |
|------|---------------------|---------------------|
| 写入限制 | ✅ 内核强制 | ✅ 内核强制 |
| 读取限制 | ✅ 内核强制 | ❌ 不生效 |
| 网络隔离 | ✅ namespace | ❌ 不生效 |
| 资源限制 | ✅ cgroup | ❌ 不生效 |
| 后台自动启用 | ✅ | ✅（写入限制） |

### 启动机制

mink 检测 `[sandbox] enabled = true` 后自动通过 `exec()` 装入沙箱，设置 `MINK_SANDBOXED=1` 防无限递归。
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

## Token 用量与费用

每轮 `run_turn()` 结束后，`TurnOutcome` 携带本轮所有 LLM 请求的 Token 消耗和人民币费用。

### 字段说明

| Rust 字段 | Python 字段 | 说明 |
|-----------|------------|------|
| `billing_turn_id` | `billing_turn_id` | 本轮稳定标识；Agent、压缩、子代理共用 |
| `usage_records` | `usage_records` | 每笔 LLM 请求明细 |
| `usage` | `usage` | `UsageSummary` 汇总：请求数、attempt 数、Token、纳元费用 |
| `session.usage_path` | `usage_path` | `usage.jsonl` 路径 |

### UsageSummary

| 字段 | 说明 |
|------|------|
| `request_count` | 本轮逻辑请求数 |
| `reported_request_count` | 返回 usage 的请求数 |
| `unreported_request_count` | 未返回 usage 的请求数 |
| `attempt_count` | HTTP 重试合计 |
| `tokens` | [TokenUsage](#tokenusage) |
| `cost_nano_cny` | 预估费用（纳元，`1 元 = 10⁹ 纳元`） |

### TokenUsage

| 字段 | 说明 |
|------|------|
| `input_tokens` | 输入 Token（已减缓存命中） |
| `cache_read_tokens` | 缓存命中 Token（按折扣价） |
| `cache_creation_tokens` | 新增缓存写入 Token（按全价） |
| `output_tokens` | 输出 Token |

### 采集路径

```text
Turn / Compaction / SubAgent → MeteredStream → usage.jsonl
→ OrchActor::finish_usage() → TurnOutcome
```

Agent 工具循环、自动压缩、子代理共享同一 `billing_turn_id`。手动压缩使用 `operation-*`。

### 定价模型

DeepSeek API 官方单价（纳元整数运算）：

| 模型 | 输入（纳元/token） | 输出（纳元/token） | 缓存读取（纳元/token） |
|------|-------------------|-------------------|----------------------|
| Flash | 1,000 | 2,000 | 20 |
| Pro | 3,000 | 6,000 | 25 |

计算公式：`input × input_nano + cache_creation × input_nano + cache_read × cache_read_nano + output × output_nano`

未报告 usage 的请求 `cost_nano_cny` 为 `None`。未知模型只记录 Token，费用按 0 统计。

### usage.jsonl 格式

```json
{"version":2,"billing_turn_id":"turn-...","request_id":"request-...",
 "kind":"agent","model":"deepseek-v4-flash","attempt_count":1,"status":"reported",
 "tokens":{"input_tokens":100,"cache_read_tokens":40,...},
 "cost_nano_cny":140800,"completed_at":"2026-06-18T00:00:00Z"}
```

### Rust 库中访问

```rust
use mink::prelude::{AgentOptions, AgentRuntime, UsageSummary};

let outcome = AgentRuntime::start_with_options(
    AgentOptions::new("/tmp/mink-session", ".")
        .with_api_key(std::env::var("DEEPSEEK_API_KEY")?)
        .with_model("flash"),
).await?.run_turn("解释这段代码").await?;

println!("input: {}, cost: {} 纳元",
    outcome.usage.tokens.input_tokens, outcome.usage.cost_nano_cny);
for record in &outcome.usage_records {
    println!("  {}: kind={:?}, status={:?}", record.request_id, record.kind, record.status);
}
```

### Python SDK 中访问

```python
from mink_agent import AgentSession, SandboxConfig

session = AgentSession(SandboxConfig(api_key="sk-...", read_dirs=["."]))
result = session.run("解释这段代码")
print(f"cost: {result['usage']['cost_nano_cny']} nano-cny")
for record in result['usage_records']:
    print(f"  {record['request_id']}: kind={record['kind']}")
session.close()
```

### CLI 中查看

```bash
mink -m flash --print "hello" | jq 'select(.type=="final") | {billing_turn_id, usage}'
cat ~/.mink/projects/<project_key>/<session_id>/usage.jsonl | jq -c
```

---

## Rust 库嵌入

Rust 发布包为 `mink-core`，库 crate 名为 `mink`。发布包只包含可嵌入 runtime 和 `Display` 协议层；
终端 REPL/TUI 和二进制入口在 `mink-cli` workspace 包。

```toml
[dependencies]
mink = { package = "mink-core", version = "0.2.0", default-features = false, features = ["runtime"] }
```

稳定入口：`mink::prelude`、`mink::runtime`、`mink::config`、`mink::sandbox`、`mink::sdk_protocol`。

### Rust 嵌入式只读 VFS

私有化服务可替换 `Read`、`Glob`、`Grep` 的后端为数据库：

```rust
use std::sync::Arc;
use mink::prelude::{AgentOptions, AgentRuntime};

let vfs = Arc::new(MyReadOnlyFileSystem::open("knowledge.db")?);
let runtime = AgentRuntime::start_with_options(
    AgentOptions::new("/tmp/mink-session", ".")
        .with_resource_session_id("tenant-task-001")
        .with_read_only_file_system(vfs),
).await?;
```

实现同步的 `ReadOnlyFileSystem` trait。每个操作收到 `VfsScope`：
- `resource_session_id`：知识库数据分区
- `agent_session_id`：调用方 session id

子代理继承 `resource_session_id`。虚拟 Read 不产生 snapshot。`Write`/`Edit` 仍操作本地文件。
`artifact://`、`skill://`、`rule://`、`session://` 不进入 VFS。

### Rust 嵌入式自定义 LLM backend

默认 OpenAI-compatible backend 支持 `openai_extra_body` 和 `openai_tool_choice` 适配大多数兼容端点：

```rust
let runtime = AgentRuntime::start_with_options(
    AgentOptions::new("/tmp/mink-session", ".")
        .with_model("local")
        .with_openai_reasoning_effort("high")
        .with_openai_tool_choice("auto")
        .with_openai_extra_body(BTreeMap::from([
            ("custom_budget".to_string(), json!(8192)),
        ])),
).await?;
```

非兼容协议可使用自定义 backend：

```rust
let runtime = AgentRuntime::start_with_options(
    AgentOptions::new("/tmp/mink-session", ".")
        .with_model("local")
        .with_llm_backend(Arc::new(MyLlmBackend::new())),
).await?;
```

实现 `mink::runtime::LlmBackend`，从 `LlmRequest` 读取 system prompt、messages、tools、取消 token 和模型名。
`LlmRequest.model` 是解析后的真实模型名；`LlmRequest.model_alias` 保留用户请求的别名。
失败时返回 `LlmRequestFailure { attempt_count, error }`。

完整示例：
```bash
cargo run -p mink-core --example custom_llm_backend
```

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

选择建议：终端用户用 `project`（按项目隔离）；Python SDK 用 `home`；Rust API 按任务建独立目录用 `isolated`；共享 mink 根目录用 `direct`。

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

每次工具执行前自动运行三段修复。

### Scavenge — 回收

从 `reasoning_content` 和文本回复中回收遗漏工具调用，支持 6 种格式：

| 格式 | 示例 |
|------|------|
| DSML invoke | `<\|DSML\|invoke name="Read">...</\|DSML\|invoke>` |
| XML 包装 | `<tool_call>{"name":"Bash",...}</tool_call>` |
| Bracket | `[TOOL_CALL]{...}[/TOOL_CALL]` |
| 裸 JSON | `{"name":"Grep",...}` |
| OpenAI style | `{"type":"function","function":{"name":"Read",...}}` |
| R1 free-form | `{"tool_name":"Bash","tool_args":{...}}` |

### Truncation — 截断修复

修复截断 JSON：闭合引号、补全括号、去尾逗号、填 null 到悬挂 key。

### StormBreaker — 重复抑制

滑动窗口（size=6）检测 `(工具名, 参数)` 重复。同一对出现 3 次时抑制调用。
mutating 工具执行时清空只读条目，允许 edit→re-read 正常模式。

---

## 工具系统

### 工具列表

| 工具 | 用途 | 关键参数 |
|------|------|---------|
| `Read` | 读文件或轻量资源，支持 selector | `path` |
| `Write` | 写文件 | `path`, `content` |
| `Edit` | anchored patch 编辑 | `path`, `patch` |
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

### 模型工具面（ModelToolSurface）

工具可见性由 **`enabled_tools`** 唯一决定。每个 agent runtime 将此列表与 approval、角色、文件系统后端、编译 feature 和硬依赖结合，解析出 `ModelToolSurface` —— 即当前模型真正可见的工具集合。

- `PrefixManager` 从 `ModelToolSurface` 生成 tools schema 和能力工作流 prompt
- `ToolRunner::execute_all()` 在真实执行前校验同一 surface，形成纵深防线
- semantic capabilities 层按能力分类工具（如“搜索内容”由 Grep 或受限 Bash 提供），自动组合工作流提示

嵌入 Rust runtime 注入只读 VFS 后，`Read`、`Glob`、`Grep` 对普通路径使用虚拟后端；未注入时行为不变。

### Read selector 与资源 URL

`Read.path` 支持 selector：
- `src/main.rs:40-80`、`src/main.rs:40+20`、`src/main.rs:raw`、`src/main.rs:raw:40-80`

轻量资源 URL：
- `artifact://<id>`：读取被截断工具输出
- `skill://list` / `skill://<name>` / `skill://<name>/<relative-path>`：列出或读取 skill
- `rule://list` / `rule://<name>`：列出或读取 rule
- `session://current` / `session://current/stats` / `session://current/messages` / `session://current/history` / `session://current/artifacts`：session 内省

`Grep.path` 也可使用 registered resource URL。建议先搜索 `session://current/history`，再按行号用 `Read` selector 读取局部。

### Anchored Edit

本地文件非 raw `Read` 输出带有 `@PATH#TAG` 的 snapshot header：

```text
@src/foo.rs#0A3B
41:fn target() {
42:    old()
```

`Edit.patch` 只支持基于 snapshot header 的行操作：

```json
{"path": "src/foo.rs",
 "patch": "@src/foo.rs#0A3B\nreplace 41..42:\n+fn target() {\n+    new()\n+"}
```

同一文件多处修改时优先合并多个 hunk。成功的 Write/Edit 会让旧 snapshot 过期。
VFS 普通路径不输出 `@PATH#TAG`，不能作为 Edit 输入。

### Todo 协议

权威快照保存在 session `todos.json`，使用 revision + 稳定 ID 防 stale write：
- `TodoRead`：读取完整快照、revision 和稳定 ID
- `TodoWrite`：原子新增 pending 条目、删除条目或替换正文
- `TodoAdvance`：原子转换 active batch 进度（activate / complete / pause / reopen）

成功变更后，在 conversation 尾部追加增量事件和当前 active batch 的紧凑物化投影。
session 恢复或压缩丢失最新 revision 时追加一次 TodoSync。一个 session 的 TodoStore 由单个 runtime 持有，不支持并发写。

### SubAgent 工具

LLM 自动调用，支持最多 8 个并发。结果统一收集后进入信号采集。

| 模式 | 上下文 | 适用 |
|------|--------|------|
| 独立（默认） | 全新空会话 | 调查、搜索、隔离验证 |
| Fork（`fork=true`） | 继承完整 session 状态 | 需要上下文延续的任务 |

Fork 在 runtime 初始化前克隆父 session 目录。Artifact 序号从克隆 index 继续，旧 `artifact://` 引用保持有效。
技能和规则来自父 capability snapshot。

---

## Skills（技能）

### 启用

```bash
# CLI 加载
mink -m flash --config 'skills=["debugging","tdd"]' -i

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
| 可覆盖 core | `agent-identity`、`environment`、`execution-codes`、`belief-awareness`、`output-language` | 替换同名 core |
| runtime-reserved | 工具 prompt、workflow、`runtime-capabilities`、`rules`、`instruction-files`、`rule-index`、`skill-index`、`selected-skills`、`current-plan` 等 | 启动时 fail fast |
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
mink --mission ./my-task.mission.md --config 'skills=["debugging"]' -i
```

```python
# Python SDK
SandboxConfig(mission_file="./my-task.mission.md")
SandboxConfig(mission_content="# agent-identity\n...")  # 内联，无临时文件
SandboxConfig(signal_mode="off")
```

### 迁移规则

- 旧 `# rules` → 改为 `# mission-rules` 或其他业务 ID
- 不再支持 `using-your-tools`、`anchored-edit-protocol`、`rationalization-table` 等旧 alias
- section ID 必须唯一；重复 heading 或占用 runtime-reserved ID 导致启动失败
- MISSION 只影响 prompt 文本，不改变工具 surface（工具选择仍用 `enabled_tools`）
- `MINK_SIGNAL_MODE=off` 时 prompt 不存在 `belief-awareness`，MISSION 也不能创建它

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

## Stream-JSON 输出

```bash
mink -m flash --print "explain this"
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
mink -m flash --print "fix the bug" | jq 'select(.type=="text") | .content'
```

---

## 故障排查

```bash
# 检查 API key
curl -H "Authorization: Bearer $DEEPSEEK_API_KEY" https://api.deepseek.com/v1/models

# verbose 模式
mink -m flash -v "hello"

# 扩大上下文窗口避免溢出
mink -m flash --config 'max_context="1M"' -i

# 查看 session 列表
mink --list-sessions

# 查看事件日志中的信念变化
grep '"belief"' events.jsonl | jq '{type, belief}'

# 查看注入历史
grep '"Injecting hint"' events.jsonl
```
