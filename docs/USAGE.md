# 使用手册

本文面向 mink 使用者，覆盖 CLI、Rust 嵌入、Python SDK 相关的运行方式、配置、沙箱、
session、技能和常见工作流。内置工具的完整参数、结果格式和边界行为见
[工具参考](tools.md)；架构和内部模块职责见 [ARCHITECTURE.md](ARCHITECTURE.md)。

## 快速开始

```bash
# 前置：Rust 1.94+，设置 DEEPSEEK_API_KEY 或通过配置指定 OpenAI-compatible 端点

# 编译
make build
# 或
cargo build --release

# 设置 API Key
export DEEPSEEK_API_KEY="sk-xxx"

# 单次任务
./target/release/mink -m flash "scan this project"

# REPL 交互模式
./target/release/mink -m flash -i

# TUI 全屏模式
./target/release/mink -m flash --tui

# 继续上次会话
./target/release/mink -m flash --continue -i

# stdin 管道输入
echo "list the files" | ./target/release/mink -m flash
```

---

## 两种终端模式

项目提供两种交互式终端模式，一种非交互 CLI 模式。

### REPL 模式（`-i`）

基于 rustyline 的行编辑器。适合日常编码交互：

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
| `[idle]` | 工作状态 | idle / waiting / thinking / generating / tool / sub-agent / compacting / error |

**B 值含义**：

| B 值 | 含义 |
|------|------|
| 0.75 | 初始（信任先验） |
| > 0.7 | 顺利 |
| 0.5~0.7 | 偶有小错 |
| 0.3~0.5 | 频繁出错 |
| < 0.3 | 严重 |

TUI 支持：

- 多行输入和 UTF-8 安全编辑。
- Ctrl+C 在任务运行时中断当前 turn。
- 长工具结果自动折叠。
- 子代理消息可点击进入详情页，查看 thinking/text。
- Markdown 由内置 renderer 渲染，支持标题、列表、引用、代码块、表格和 diff。

**TUI 特有操作**：

| 操作 | 行为 |
|------|------|
| `Ctrl+C` | 工作中中断当前 turn；空闲时按退出流程处理 |
| `/flash` / `/pro` | 切换模型，不发送给 LLM |
| `/compact` | 手动触发上下文压缩 |
| `/help` / `/skills` | 本地显示帮助或 skill 列表 |
| `/exit` / `/quit` / `/q` | 退出 TUI |
| 未知 `/xxx` | 本地提示，不发送给 LLM |
| 行首空格 + `/xxx` | 作为普通文本发送给 LLM |
| 鼠标点击折叠项 | 展开或收起长内容 |
| 鼠标点击子代理消息 | 打开子代理详情页 |

### 标题栏（REPL/CLI 模式）

REPL/CLI 模式下，终端窗口标题显示相同统计信息，通过 ANSI escape `\x1b]0;...\x07` 设置：

```
flash B:0.73 T:12 R:45 I:200K(50%) O:20K C:400K(40%) ¥0.12
```

信念度在每次工具调用后实时更新。低于阈值时，系统会在同一次任务循环内的下一轮 LLM 调用前注入提示词或中止任务。

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

---

## 交互命令（REPL / TUI）

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

`/flash` 和 `/pro` 命令立即生效，不会发送给 LLM。切换后下一轮 LLM 调用使用新模型。TUI 会拦截未知 `/xxx` 命令并提示；如果要把 slash 开头文本发给模型，在行首加一个空格。

---

## CLI 参数

| 参数 | 默认值 | 说明 |
|------|--------|------|
| `PROMPT` | — | 用户输入（位置参数） |
| `-m` / `--model` | `flash` | 模型名。`flash` / `pro` 是默认别名，也可以直接指定任意 OpenAI-compatible 模型名 |
| `--mission PATH` | — | 加载 MISSION.md 文件替换默认系统提示词 |
| `--session [NAME]` | 自动生成 | 命名会话。提供名称可恢复 |
| `--continue` | — | 恢复最近的 session |
| `--list-sessions` | — | 列出所有 session |
| `--list-skills` | — | 列出可用 skill |
| `-i` / `--interactive` | auto | REPL 交互模式 |
| `--tui` | — | TUI 全屏模式 |
| `--print` | — | ndjson 结构化输出，最后输出 `type=final` |
| `--agent-jsonl` | — | Agent JSONL 协议（stdin 读 versioned request，stdout 输出事件流和最终 `final`；request 可用 `options.stream_events=false` 关闭过程事件，仅保留 `final`） |
| `--api-key KEY` | env | 覆盖 API Key |
| `--base-url URL` | 默认端点 | 覆盖 API 端点 |
| `--disable-bash` | `false` | 禁用 Bash 工具 |
| `--disable-python` | `false` | 禁用 Python 工具（宿主） |
| `--enable-python-sandbox` | `false` | 启用 PythonSandbox 工具（默认禁用） |
| `--disable-sub-agent` | `false` | 禁用 SubAgent 工具 |
| `--disable-web` | `false` | 禁用 WebSearch / WebFetch 工具 |
| `--config <toml>` | — | 通过 TOML 字符串设置配置（见下文） |
| `-v` / `--verbose` | `false` | 详细日志 |
| `-h` / `--help` | — | 显示帮助 |

### `--config` TOML 格式

中低频参数通过 `--config` 传递，支持全部可选字段：

```toml
# 标量字段
max_tokens = 4096
max_turns = 20
max_context = "500K"
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
openai_reasoning_effort = "max"          # 设为 "off" 可不发送 reasoning_effort
openai_include_usage = true              # stream_options.include_usage
openai_token_param = "max_tokens"        # max_tokens / max_completion_tokens

[model_aliases]
flash = "deepseek-v4-flash"
pro = "deepseek-v4-pro"
local = "private-model-v1"

# [sandbox_python] 段
[sandbox_python]
wasm_path = "/path/to/python.wasm"
read_dirs = ["./data"]
```

等价于旧版独立的 CLI 参数。也支持设置 `model`、`api_key`、`base_url`，但推荐使用独立参数以便 SDK 控制。显式传入的 `--config <toml>` 解析失败会直接退出；用户级/项目级 `.minkrc` 解析失败只输出 warning 并继续。
`model_aliases` 可覆盖默认别名；没有命中别名的 `model` 会作为真实模型名原样发送。

`--agent-jsonl` 模式不会读取用户级/项目级 `.minkrc`，以避免 SDK 调用产生额外文件 I/O；但仍会应用同一命令行传入的 `--config <toml>`。因此 SDK 可以通过 `--config` 精确传入 `max_search_files`、`max_search_results`、`enabled_tools` 等 per-call 配置。
---

## 配置文件

`~/.minkrc`（用户级）和 `<project>/.minkrc`（项目级）可选配置。
优先级：CLI 参数 > 项目配置 > 用户配置 > 环境变量 > 默认值。

```toml
# ~/.minkrc 示例
api_key = "sk-xxx"                        # API 密钥
base_url = "https://api.deepseek.com/v1"  # API 端点
model = "flash"                           # 默认模型；可为别名或真实模型名
max_tokens = 81920                        # 最大输出 token
max_turns = 40                            # 最大轮次
max_context = "1M"                        # 最大上下文（支持 K/M 后缀）
tool_timeout = 600                        # 工具超时（秒）
sub_agent_timeout = 120                   # 子代理超时（秒）
llm_first_event_timeout = 60              # 等待首个模型 stream event 的秒数
llm_idle_timeout = 90                     # 模型 stream 空闲超时（秒）
llm_wait_heartbeat = 30                   # 等待模型响应的提示间隔（秒，0=关闭）
context_compact_pct = 85                  # 压缩触发百分比
log_events = true                         # 事件日志
max_search_files = 5000                     # Glob/Grep 最大遍历文件数
max_search_results = 1000                   # Grep 最大匹配结果行数
enabled_tools = ["Read", "Write", "Edit", "Grep", "Glob", "Bash"]  # 工具白名单
openai_reasoning_effort = "max"           # OpenAI-compatible reasoning_effort；"off" 表示不发送
openai_include_usage = true               # 是否请求 stream usage
openai_token_param = "max_tokens"         # max_tokens | max_completion_tokens

[model_aliases]
flash = "deepseek-v4-flash"
pro = "deepseek-v4-pro"
# local = "private-model-v1"

[tools]
approval_mode = "yolo"                    # yolo | write | always-ask

[tools.approval]
Bash = "prompt"                           # allow | deny | prompt
Read = "allow"
```

项目级 `.minkrc` 覆盖用户级，CLI 参数覆盖所有文件设置。
所有字段可选，未设置的字段使用默认值或环境变量。
当前版本尚未实现交互式审批 prompt；需要审批的工具调用会 fail closed。默认 `yolo` 保持旧行为。
`enabled_tools` 同时作用于工具 schema 和执行层：未在白名单内的工具不会暴露给模型；即使模型或历史上下文产生该工具调用，也会在执行前返回错误结果。

---

## 沙箱配置

沙箱通过 OS 原生工具（Linux nsjail/bubblewrap、macOS sandbox-exec）包裹 mink 进程，
在文件系统层面强制执行访问控制。

### 配置方式

`.minkrc` 的 `[sandbox]` 段控制沙箱开关和规则：

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

### PythonSandbox 沙箱配置

#### python.wasm 文件

`python.wasm` 是 **CPython 编译为 WASI 的二进制**，由 CPython 核心开发者 Brett Cannon 维护的 [cpython-wasi-build](https://github.com/brettcannon/cpython-wasi-build) 项目自动构建发布。

- **版本**: Python 3.13.13（稳定版），3.14.5，3.15.0 beta
- **大小**: ~29MB（python.wasm）+ ~9MB（标准库 .py 文件）
- **运行时**: wasmtime
- **C 扩展**: 仅包含 CPython 内置 C 模块（math/hashlib/binascii 等），不含第三方 C 扩展

下载方式：

```bash
# 手动下载
curl -sL "https://github.com/brettcannon/cpython-wasi-build/releases/download/v3.13.13/python-3.13.13-wasi_sdk-24.zip" -o python-wasi.zip
unzip python-wasi.zip -d cpython-wasi
```

下载后，项目目录结构应为：

```
cpython-wasi/
├── python.wasm          # CPython WASI 二进制（~29MB）
├── lib/python3.13/      # CPython 标准库
└── LICENSE
```

然后在 `.minkrc` 中配置路径：

```toml
[sandbox_python]
enable = true                            # 启用（默认禁用）
wasm_path = "cpython-wasi/python.wasm"   # python.wasm 路径
stdlib_dir = "cpython-wasi"              # 标准库目录
```

#### 路径与权限配置

`[sandbox_python]` 段还支持以下配置项：

```toml
[sandbox_python]
enable = true
wasm_path = "cpython-wasi/python.wasm"
stdlib_dir = "cpython-wasi"
timeout = 30                             # 超时秒数
read_dirs = ["./data"]                   # 允许读取的目录
write_dirs = ["./output"]               # 允许写入的目录
package_dirs = ["./packages"]            # Python 包目录（挂载到 /packages）
```

路径解析通过沙箱自动注入的 `os.chdir` 实现，三种路径写法均支持：
- `open("./output/f.txt", "w")` — 相对路径
- `open("output/f.txt", "w")` — 无前缀相对路径
- `open("/absolute/path/to/output/f.txt", "w")` — 绝对路径

权限规则：
- 仅在 `read_dirs` / `write_dirs` 中声明的目录可访问
- `write_dirs` 优先于 CWD 只读（显式声明的写入权限覆盖 CWD 默认只读）
- 路径穿越（`../`）无法逃逸 preopen 范围

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

mink 启动时检测 `[sandbox] enabled = true`，自动通过 `exec()` 将自身重新装入沙箱：

```
mink --tui
  → 读取 .minkrc
  → exec("nsjail --bindmount_ro src mink --tui")        // Linux
  → exec("sandbox-exec -p '<profile>' mink --tui")        // macOS (写入限制)
  → 设置 MINK_SANDBOXED=1 防无限递归
  → 原进程被替换，进程完全在沙箱中运行
```

启用沙箱后，如果指定或自动选择的沙箱后端不可用，mink 会打印 fatal 错误并退出，而不是静默降级到非沙箱运行。未启用 `[sandbox] enabled = true` 时不会触发 re-exec。

---

## 环境变量

| 变量 | 默认值 | 说明 |
|------|--------|------|
| `DEEPSEEK_API_KEY` | — | **必需。** API 密钥 |
| `DEEPSEEK_BASE_URL` | `https://api.deepseek.com/v1` | 自定义 API 端点 |
| `MINK_WEB_USER_AGENT` | Firefox-like UA | WebSearch/WebFetch 的 User-Agent 覆盖 |
| `TOOL_RESULT_MAX_BYTES` | `100000` | 单条工具结果截断上限 |
| `FILE_WRITE_MAX_BYTES` | `1048576` | Write/Edit 工具写入上限 |
| `MAX_SEARCH_FILES` | `5000` | Glob/Grep 最大遍历文件数 |
| `MAX_SEARCH_RESULTS` | `1000` | Grep 最大匹配结果行数 |
| `LOG_EVENTS` | `true` | 设为 `0`/`false`/`no` 关闭 events.jsonl 记录 |
| `MINK_SIGNAL_MODE` | `full` | 信号系统模式：`full` 启用信念跟踪、注入和恢复守卫；`off` 关闭信号提示词和运行时信号干预 |
| `MINK_HOME` | `$HOME` | session 存储目录覆盖 |
| `MINK_LIMITS` | — | JSON 格式 sandbox 限制配置，启用时覆盖 `[sandbox]` |

---

搜索相关上限分为多层：`MAX_SEARCH_FILES` 控制 Glob/Grep 最多遍历的文件数，`MAX_SEARCH_RESULTS` 控制 Grep 最多返回的匹配行数；搜索工具自身还有 100KB 输出保护，最终工具结果还会受 `TOOL_RESULT_MAX_BYTES` 保护。看到 `scanned first N files` 表示文件遍历上限触发，看到 `truncated at N results` 表示匹配结果数上限触发，看到 `output > 100000 bytes` 或 artifact 提示则表示输出字节数保护触发。

## Token 用量与费用

每轮 `run_turn()` 结束后，`TurnOutcome` 携带本轮所有 LLM 请求的 Token 消耗和人民币费用。

### 字段说明

| Rust `TurnOutcome` 字段 | Python `result` 字段 | 类型 | 说明 |
|------------------------|---------------------|------|------|
| `billing_turn_id` | `billing_turn_id` | `String` | 本轮稳定标识。Agent、自动压缩和子代理共用同一个 `billing_turn_id` |
| `usage_records` | `usage_records` | `Vec<UsageRecord>` | 本轮每笔 LLM 请求的明细（最多一条记录 per 请求） |
| `usage` | `usage` | `UsageSummary` | 纯函数汇总：请求数、attempt 数、Token 总计、纳元费用 |
| `session.usage_path` | `usage_path` | `PathBuf` | session `usage.jsonl` 路径，可自行读取完整历史 |

### `UsageSummary` 字段

| 字段 | 类型 | 说明 |
|------|------|------|
| `request_count` | `u64` | 本轮逻辑 LLM 请求数 |
| `reported_request_count` | `u64` | provider 返回了 `usage` 的请求数 |
| `unreported_request_count` | `u64` | provider 未返回 `usage` 的请求数（失败、超时等） |
| `attempt_count` | `u64` | HTTP 重试合计次数 |
| `tokens` | [TokenUsage](#tokenusage) | 各项 Token 总数 |
| `cost_nano_cny` | `u64` | 预估人民币费用（纳元），`1 元 = 10⁹ 纳元` |

### `TokenUsage`

| 字段 | 类型 | 说明 |
|------|------|------|
| `input_tokens` | `u64` | 输入 Token（已减去缓存命中部分） |
| `cache_read_tokens` | `u64` | 缓存命中 Token（按折扣价计费） |
| `cache_creation_tokens` | `u64` | 新增缓存写入 Token（按全价计费，多数 provider 不报告此值） |
| `output_tokens` | `u64` | 输出 Token |

### 采集路径

```text
Turn / Compaction / SubAgent 都通过同一 AsyncLlClient 入口
→ MeteredStream 包装 SSE 流
→ Event::Usage → UsageCapture::reported() → usage.jsonl 追加
→ 或 stream_error / 无 usage → UsageCapture::unreported() → usage.jsonl 追加
→ OrchActor::finish_usage() 按 billing_turn_id 收集 → TurnOutcome
```

三类请求共享 billing_turn_id：Agent 工具循环中的多次 LLM 调用、turn 内的自动压缩、子代理的 LLM 请求都汇聚到同一 `billing_turn_id` 下。手动压缩（无活跃 turn）使用独立的 `operation-*` 标识。

### 定价模型

当前使用 DeepSeek API 官方单价（纳元整数运算，无浮点累积误差）：

| 模型 | 输入（纳元/token） | 输出（纳元/token） | 缓存读取（纳元/token） |
|------|-------------------|-------------------|----------------------|
| Flash | 1,000 | 2,000 | 20 |
| Pro | 3,000 | 6,000 | 25 |

价格来源：`ModelTier::price_input_per_m() / price_output_per_m() / price_cache_read_per_m()`。
费用计算公式（`price_usage()`）：

```
cost = input_tokens × input_nano
     + cache_creation_tokens × input_nano
     + cache_read_tokens × cache_read_nano
     + output_tokens × output_nano
```

未报告 `usage` 的请求 `cost_nano_cny` 为 `None`，不会用零值伪装。
未知模型或自定义模型即使命中了 `usage`，也只记录 Token，`cost_nano_cny` 按 0 统计；
业务侧如需私有模型计价，应读取 `usage_records` 后按自己的价格表计算。

### usage.jsonl 记录格式

```json
{"version":1,"billing_turn_id":"turn-...","request_id":"request-...",
 "kind":"agent","origin_session_id":"session-...","model":"deepseek-v4-flash",
 "attempt_count":1,"status":"reported",
 "tokens":{"input_tokens":100,"cache_read_tokens":40,"cache_creation_tokens":0,"output_tokens":20},
 "cost_nano_cny":140800,"reason":null,
 "completed_at":"2026-06-18T00:00:00Z"}
```

未报告 `usage` 的记录 `tokens` 和 `cost_nano_cny` 为 `null`，`reason` 描述原因。一个逻辑请求最多写入一条终态记录。

### Rust 库中访问

```rust
use mink::prelude::{AgentOptions, AgentRuntime, UsageSummary};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let rt = AgentRuntime::start_with_options(
        AgentOptions::new("/tmp/mink-session", ".")
            .with_api_key(std::env::var("DEEPSEEK_API_KEY")?)
            .with_model("flash"),
    ).await?;

    let outcome = rt.run_turn("解释这段代码").await?;

    println!("billing_turn_id: {}", outcome.billing_turn_id);
    println!("input tokens: {}", outcome.usage.tokens.input_tokens);
    println!("cache read tokens: {}", outcome.usage.tokens.cache_read_tokens);
    println!("output tokens: {}", outcome.usage.tokens.output_tokens);
    println!("cost: {} 纳元 ≈ {:.4} 元", outcome.usage.cost_nano_cny,
             outcome.usage.cost_nano_cny as f64 / 1_000_000_000.0);
    println!("usage file: {}", outcome.session.usage_path.display());

    for record in &outcome.usage_records {
        println!("  request {}: kind={:?}, status={:?}, attempts={}",
                 record.request_id, record.kind, record.status, record.attempt_count);
    }

    // session 中全部历史记录可通过读取 usage_path 文件获取：
    // let data = std::fs::read_to_string(&outcome.session.usage_path)?;

    rt.shutdown().await?;
    Ok(())
}
```

### Rust 嵌入式只读 VFS

私有化服务可保持 `Read`、`Glob`、`Grep` 工具协议不变，把普通路径的读取和检索切换到数据库：

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

后端实现同步的 `mink::runtime::ReadOnlyFileSystem`。每个 `read` / `glob` / `grep`
调用都会收到 `VfsScope`：

- `resource_session_id`：知识库数据分区；未配置时默认使用 runtime session id。
- `agent_session_id`：实际发起调用的主代理或子代理 session id。

子代理继承父代理的 `resource_session_id`，但使用自己的 `agent_session_id`。虚拟 Read 是只读的，不产生 anchored Edit snapshot；`Write` 和 `Edit` 仍只操作本地文件。`artifact://`、`skill://`、`rule://`、`session://` 和 `http(s)://` 也继续走内置资源实现，不进入 VFS。

完整 redb 示例见
[`crates/mink-core/examples/redb_vfs.rs`](../crates/mink-core/examples/redb_vfs.rs)。
redb 只是示例依赖，业务可替换为其他同步嵌入式数据库。

### Rust 嵌入式自定义 LLM backend

默认 LLM backend 是 OpenAI-compatible streaming client。Rust 嵌入时可以由业务服务注入
自己的后端，支持私有化部署、内网网关、自定义鉴权、非 HTTP transport 或模型厂商 SDK：

```rust
use std::sync::Arc;
use mink::prelude::{AgentOptions, AgentRuntime};

let runtime = AgentRuntime::start_with_options(
    AgentOptions::new("/tmp/mink-session", ".")
        .with_model("local")
        .with_llm_backend(Arc::new(MyLlmBackend::new())),
).await?;
```

后端实现 `mink::runtime::LlmBackend`，从 `LlmRequest` 读取系统提示词、消息、工具 schema、
取消 token、真实模型名和别名，然后返回 `LlmEvent` 流。`LlmRequest.model` 是解析后的真实模型名；
`LlmRequest.model_alias` 保留用户请求的别名。失败时建议返回
`LlmRequestFailure { attempt_count, error }.into()`，这样 usage 日志能记录失败请求的 attempt 数。

完整示例见
[`crates/mink-core/examples/custom_llm_backend.rs`](../crates/mink-core/examples/custom_llm_backend.rs)：

```bash
cargo run -p mink-core --example custom_llm_backend
```

### Python SDK 中访问

```python
from mink_agent import AgentSession, SandboxConfig

session = AgentSession(SandboxConfig(api_key="sk-...", read_dirs=["."]))
result = session.run("解释这段代码")

print(f"billing_turn_id: {result['billing_turn_id']}")
print(f"usage tokens: {result['usage']['tokens']}")
print(f"cost: {result['usage']['cost_nano_cny']} nano-cny")

for record in result['usage_records']:
    print(f"  {record['request_id']}: kind={record['kind']}, status={record['status']}")

session.close()
```

### CLI 中查看

```bash
# stream-json 模式的 final 事件携带用量
mink -m flash --print "hello" | jq 'select(.type=="final") | {billing_turn_id, usage}'

# 直接读取 usage.jsonl
cat ~/.mink/projects/<project_key>/<session_id>/usage.jsonl | jq -c

# 筛选特定 billing_turn_id 的记录
cat usage.jsonl | jq 'select(.billing_turn_id == "turn-...")'
```

## Rust 库嵌入


Rust 发布包名为 `mink-core`，库 crate 名为 `mink`。`mink-core` 发布包只包含可嵌入 runtime
和 `Display` 协议层；终端 REPL/TUI 实现、`mink` / `mink-core` 二进制入口都在 `mink-cli`
workspace 包中。服务端嵌入时推荐只启用 runtime：

```toml
[dependencies]
mink = { package = "mink-core", version = "0.1.12", default-features = false, features = ["runtime"] }
```

稳定入口优先使用 `mink::prelude`、`mink::runtime`、`mink::config`、`mink::sandbox` 和
`mink::sdk_protocol`。完整终端二进制使用 `mink-cli` 默认 feature 构建；SDK 精简二进制使用
`cargo build -p mink-cli --no-default-features --features sdk-bin --bin mink-core` 构建。

## 会话管理

### Session layout

`MINK_HOME` 是 session 持久化的 home 根目录，默认是 `$HOME`。不同入口使用不同 layout 推导最终
session 目录：

| Layout | `home` 含义 | 最终 session 目录 | 默认入口 |
|--------|-------------|-------------------|----------|
| `project` | 用户/服务根目录 | `home/.mink/projects/<project_key(cwd)>/<session_id>/` | CLI、裸 `mink-core --agent-jsonl` |
| `home` | 用户/服务根目录 | `home/.mink/sessions/<session_id>/` | Python SDK |
| `direct` | mink session 集合根目录 | `home/<session_id>/` | 显式配置 |
| `isolated` | 当前 session 根目录 | `home/` | Rust `AgentOptions` |

选择建议：

- 终端用户和 CLI 自动使用 `project`，同一个 `MINK_HOME` 下按项目隔离。
- Python SDK 默认 `home`，适合一个 SDK home 管理多个独立 session。
- Rust API 服务如果已经为每个任务创建了独立目录，例如 `default/<task_id>/.mink_home/`，用 `isolated`。
- 如果服务有一个共享 mink 根目录，例如 `/var/lib/my-service/mink/`，并希望 mink 自己按 session 分目录，用 `direct`。

### 目录结构

CLI 的历史目录结构是 `project` layout：

```
~/.mink/
├── history                    ← 交互式 REPL 历史
└── projects/<project_key>/
    └── <session_id>/
        ├── conversation.jsonl ← 对话消息（JSONL 逐行追加）
        ├── events.jsonl       ← 事件日志
        ├── session.json       ← session 元数据：alias、title、created_at、updated_at
        ├── summary.txt        ← 压缩后的上下文快照
        ├── plan.md            ← 确认后的计划
        ├── plan.draft         ← 草稿计划
        ├── stats.json         ← Token 用量统计
        └── usage.jsonl        ← LLM 请求级 Token 与费用明细
```

### 操作

```bash
# 命名会话
mink -m flash --session my-fix "fix the bug"

# 恢复命名会话（保持上下文）
mink -m flash --session my-fix -i

# 恢复最近会话
mink -m flash --continue -i

# 列出所有
mink --list-sessions
```

`session_id` 是稳定内部 ID，默认使用时间戳和随机后缀。除 `isolated` 外，它通常也是最终目录名；
`isolated` 中 `home` 自身就是 session 目录，`session_id` 仍写入 `session.json` 并出现在事件/SDK final 中。
`--session my-fix` 会先按 alias、完整 id、id 前缀和 title 匹配已有 session；匹配不到时创建新的时间戳 session，并把 `my-fix` 写入 `session.json` 的 alias。带空格或特殊字符的名称会规范化为安全 alias，例如 `feature x` 会保存并解析为 `feature-x`。如果某个 `session.json` 损坏，列表和解析会回退到目录名与 `summary.txt`，不会阻断其他 session。`--list-sessions` 优先展示 alias/title，同时保留内部 id。

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

```toml
# .minkrc
context_compact_pct = 70   # 70% 触发（更频繁）
max_context = "1M"         # 1M 上下文窗口
```

```bash
mink -m flash --config 'max_context="1M"' -i
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

本节只列出用户需要理解的工具能力和常用参数。工具调度模型、审批策略、资源 URL、
artifact、输出截断和每个工具的完整协议以 [工具参考](tools.md) 为准。

| 工具 | 用途 | 关键参数 |
|------|------|---------|
| `Read` | 读文件或轻量资源，支持 selector 和 `artifact://` / `skill://` / `rule://` / `session://` | `path` |
| `Write` | 写文件 | `path`, `content` |
| `Edit` | anchored patch 编辑 | `path`, `patch` |
| `Bash` | 执行命令 | `command`, `timeout` |
| `Python` | 运行 Python 脚本（宿主环境，完整生态） | `script` / `script_file`, `timeout` |
| `PythonSandbox` | WASI 沙箱中执行 Python（受限，默认禁用） | `script` / `script_file`, `timeout` |
| `Glob` | 文件匹配 | `pattern`, `path` |
| `Grep` | 内容搜索 | `pattern`, `path`, `glob`, `context` |
| `TodoWrite` | 维护 checklist | `todos[{content, status}]` |
| `PlanConfirm` | 确认计划 | 无参数 |
| `PlanClear` | 清空计划 | 无参数 |
| `SubAgent` | 启动子代理 | `prompt`, `description`, `fork` |
| `WebSearch` | 网络搜索 | `query` |
| `WebFetch` | 网页获取 | `url` |

完整工具说明见 [tools.md](tools.md)。

嵌入式 Rust runtime 注入只读 VFS 后，表中的 `Read`、`Glob`、`Grep` 对普通路径使用虚拟后端；未注入时本地工具行为不变。

### Read selector 与资源 URL

`Read.path` 可追加 selector：

- `src/main.rs:40-80`
- `src/main.rs:40+20`
- `src/main.rs:raw`
- `src/main.rs:raw:40-80`

可读取的轻量资源：

- `http(s)://...`：读取公开 URL，首次 fetch 后缓存到当前 session artifact，后续 selector 从缓存分页；cache 正文缺失时会重新 fetch
- `artifact://<id>`：读取被截断工具输出
- `skill://list` / `skill://<name>`：列出或读取可用 skill；列表与读取来自同一 capability snapshot
- `rule://list` / `rule://<name>`：列出或读取可用 rule
- `session://current`：当前 session 摘要
- `session://current/stats`：stats JSON
- `session://current/messages` / `session://current/messages/all`：conversation 摘要
- `session://current/artifacts`：artifact 列表

示例：

```jsonl
{"path":"https://example.com:20-60"}
{"path":"artifact://bash-0001:1-120"}
{"path":"skill://debugging"}
{"path":"session://current/messages:1-40"}
```

### Anchored Edit

本地文件非 raw `Read` 会输出 snapshot header：

```text
@src/foo.rs#0A3B
41:fn target() {
42:    old()
```

推荐用 `Edit.patch` 修改已有文件。`Edit` 不支持 `old_string/new_string`；新建或完整覆盖文件使用 `Write`。

```json
{
  "path": "src/foo.rs",
  "patch": "@src/foo.rs#0A3B\nreplace 41..42:\n+fn target() {\n+    new()\n+}"
}
```

同一文件多处修改时，优先在一次 `Edit.patch` 中合并多个 hunk。任何成功的 `Edit` 或 `Write`
都会让该文件之前的 snapshot tag 和行号过期。成功 `Edit` 会返回新的 `@PATH#TAG` 和修改区域附近的行号；
后续同一区域编辑可直接使用该新 header，其他区域应重新 `Read` 目标范围。patch 行号始终指向
snapshot 中的原始行号，同一次 patch 内不会因为前面的 hunk 而位移。

如果 snapshot 过期、tag 未知、目标行未覆盖或 patch 无实际变化，`Edit` 会拒绝修改，并给出建议
`Read path:N-M` 范围和当前相关行上下文。此时不要继续猜测或扩大 hunk，应先重新 `Read`，再用新 header 重试。

patch body 行只写最终内容，每行以 `+` 开头；不要写 `-old` 行、原始上下文行或 unified diff 的 `@@` header。
不要用 `Edit` 做机械格式化、import 排序、空白清理或纯缩进调整；语义修改后运行项目 formatter。

VFS 普通路径读取会输出行号和 `[read-only virtual file: ...]` 标记，但不会输出
`@PATH#TAG`，因此不能作为 `Edit.patch` 的输入。

---

## Skills（技能）

### 启用方式

```bash
# CLI 加载（通过 --config）
mink -m flash --config 'skills=["debugging"]' -i

# 加载多个
mink -m flash --config 'skills=["debugging","tdd"]' -i

# 查看可用技能
mink --list-skills
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

1. `<project>/.claude/skills/<name>/SKILL.md` — 项目级覆盖
2. `<project>/skills/<name>/SKILL.md` — 项目开发目录
3. `~/.claude/skills/<name>/SKILL.md` — 用户全局
4. **内置（编译时嵌入）** — 兜底读取内存，零文件 I/O

同名 skill 会被覆盖（优先级高的替代内置的）。

### 加载机制

- Skill 通过 `.minkrc` 的 `skills` 字段或 `--config "skills=[\"name\"]"` 加载，加载后在 system prompt 的 `<selected-skills>` 段嵌入 SKILL.md 全文
- `Read skill://<name>` 在运行时按需读取当前 capability snapshot 中的同一份 skill 视图，不修改后续轮次的 system prompt
- 内置技能即使在离线环境也可用（编译时已嵌入）

### 能力视图

每次 runtime 启动会构建一份 capability snapshot。system prompt 的 skill index、selected skills、instruction files、rules，以及 `Read skill://...` / `Read rule://...` 都读取这份视图。这样 CLI、Rust runtime、Python SDK 和子代理不会各自重新扫描一套能力来源。

Rust runtime 和 Agent JSONL / Python SDK 可注入 inline skills，并可通过 discovery policy 控制是否加载默认文件系统和内置 skills。默认 CLI 行为仍保留本地 skill 覆盖内置 skill 的优先级。

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
mink --mission ./my-task.mission.md -i
```

```bash
# 结合技能使用（通过 --config）
mink --mission ./my-task.mission.md --config 'skills=["debugging"]' -i
```

### Python SDK

```python
# 文件方式
SandboxConfig(mission_file="./my-task.mission.md")

# 内联方式（通过 SDK JSONL 直接传递，无临时文件开销）
SandboxConfig(mission_content="# agent-identity\n...")

# 关闭信号系统
SandboxConfig(signal_mode="off")
```

`signal_mode=None` 时继承进程环境中的 `MINK_SIGNAL_MODE`；如果环境变量也未设置，mink 默认使用 `full`。

### 注意事项

- MISSION.md 替换的是 prompt 文本，不影响工具定义。禁用工具仍需 `--disable-bash` 等参数。
- 未在 MISSION.md 中定义的段保持默认内容；当 `MINK_SIGNAL_MODE=off` 时，默认 prompt 不包含 `belief-awareness` 信号协议段。
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

子代理默认超时 300 秒（5 分钟），可通过 `--config` 或 `.minkrc` 的 `sub_agent_timeout` 字段调整。超时后子代理被标记为 `failed`，父会话继续执行。

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
