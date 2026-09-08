# 内置工具

> 更新日期：2026-09-08

本文是 Mink 内置工具的协议参考，面向需要理解工具参数、执行模型、结果通道、资源 URL、
审批和构建裁剪的使用者与开发者。终端使用与配置见 [使用手册](USAGE.md)；Rust/Python
嵌入见 [EMBEDDING.md](EMBEDDING.md)；模块分层和内部数据流见
[ARCHITECTURE.md](ARCHITECTURE.md)；工具 surface、语义能力和跨工具 workflow 的设计见
[工具能力与提示词解耦设计文档](设计哲学-工具能力与提示词解耦.md)。

[TOC]

## 执行模型

所有工具通过 `tools/runner.rs` 中的 `ToolExec` trait 注册到 `TOOL_REGISTRY`，由 `ToolRunner::execute_all()` 调度。只读工具会按连续批次并发执行；写入、执行、控制和 SubAgent 类工具按模型调用顺序串行执行，避免同批工具之间出现读写竞态。每个工具同时声明 `ToolMetadata`，包含 approval tier、结果类型、副作用、storm 例外和 `spawns_sub_agent` 标记。

统一流程：

```text
ToolCallEvent
  -> resolved ModelToolSurface 执行门禁
  -> StormBreaker 检查
  -> repair_tool_input() 非 fallback 输入守卫
  -> ToolExec::execute()
  -> format_dispatched_result() 生成 ToolExecution
     -> 普通结果执行大小保护、Bash noise filter、Read-Write summary、Edit conv_content
     -> Plan/SubAgent 结果标记为待定稿
  -> PlanActionHandler 生成 Plan effect / 压缩请求；SubAgentCoordinator 完成延迟工作
  -> finalize_deferred_results() 对延迟结果执行大小保护
  -> SignalCollector 只观察最终 ToolExecution.status；Command 正文只用于诊断 regex
```

OpenAI SSE parser 在生成 `ToolCallEvent` 前合并碎片化 arguments，并要求输入是 JSON object。
首次解析失败时调用 `repair_truncated_json()`；只有修复结果能够重新解析且
`fallback=false` 才继续，无法可靠修复的输入直接返回解析错误。Scavenge 回收的候选调用也
通过同一个 `build_tool_call_event()` 结构化校验。Runner 接收结构化 `input_json`，每个工具
通过 serde 从 `input_json` 反序列化参数；参数不匹配时返回 `Error:` 前缀的结构化错误，
不 panic。

工具结果有两个内容通道：

- `content`：工具层截断/过滤后的展示内容，会进入 UI 和默认 LLM tool result。
- `conversation_content` / `conv_content`：工具自定义给 LLM 的精简内容。非空时优先写入 conversation。

默认 `tool_result_max_bytes` 为 `100000`，可通过环境变量 `TOOL_RESULT_MAX_BYTES` 调整。

当工具输出超过上限时，完整输出会保存到当前 session 的 `artifacts/` 目录，工具结果中追加 `artifact://<id>`。可用 `Read` 按需读取，例如 `artifact://bash-0001:1-120`。ArtifactManager 从已有 index 的最大序号继续分配，并以独占创建写入正文；恢复 session 或 fork 后不会覆盖旧 artifact。

`Read` 当前是轻量资源的内置调用 provider。具体协议由 `ResourceRouter` 和各 scheme
handler 拥有，不能把 `skill://`、`rule://` 等协议视为 `Read` 工具自身合同：

- `artifact://<id>`：读取被截断工具输出。
- `skill://list` / `skill://list/all` / `skill://<name>` / `skill://<name>/<relative-path>`：通过当前 `Read` provider 列出、诊断或读取可用 skill；列表、正文和子资源读取来自同一 capability snapshot。`skill://all` 是 `skill://list/all` 的兼容别名；只有 filesystem-backed skill 支持 `<relative-path>` 子资源。
- `rule://list` / `rule://<name>`：列出或读取可用 rule。
- `session://current`：读取当前 session 摘要。
- `session://current/stats`：读取当前 session stats JSON。
- `session://current/messages`：读取最近 40 条 conversation 摘要。
- `session://current/messages/all`：读取全部 conversation 摘要。
- `session://current/history`：读取从完整 `conversation.jsonl` 生成的有损 transcript；省略 thinking 和完整工具结果正文。
- `session://current/artifacts`：列出当前 session artifacts。
- `session://current/todo`：读取当前 session Todo 快照（与 TodoRead 同源）。
- `session://current/plan`：读取当前 session 计划状态与内容（草稿/已确认/无，与 Plan 工具同源）。

这些资源都支持同样的行 selector，例如 `session://current/messages:1-20`。

### 嵌入式只读 VFS

嵌入式 runtime 可通过 `AgentOptions::with_read_only_file_system()` 注入同步
`ReadOnlyFileSystem`，替换普通路径上的 `Read`、`Glob`、`Grep` 后端。该机制不注册新工具，也不修改三个工具的 schema。

- 未注入时严格执行原有本地文件代码路径，包括本地路径解析、`ignore` 遍历和 Read snapshot。
- `artifact://`、`skill://`、`rule://`、`session://` 不进入 VFS，继续使用已有资源实现。
- 每次调用收到 `VfsScope { resource_session_id, agent_session_id }`。前者用于数据库分区，后者标识当前主代理或子代理。
- 未指定 `resource_session_id` 时默认使用 runtime session id；子代理继承父代理的 resource scope，但拥有自己的 agent session id。
- 虚拟路径按 POSIX 规则规范化，拒绝 `..` 越过虚拟根目录和 NUL 字节。
- 虚拟 Read 不生成 editable snapshot，因此 VFS runtime 不向模型暴露 `Edit`；`enabled_tools` 显式包含
  `Edit` 时启动失败。`Write` 仍操作本地文件，不修改 VFS。
- Glob/Grep 后端返回结构化结果。glob/regex 校验、文本格式和 100KB 搜索输出保护由 `mink-core` 保持。
- 后端必须自行实现 `glob` / `grep` 并遵守请求中的 `max_files` / `max_results`；`mink-core` 不提供第二套 VFS 搜索实现。

虚拟非 raw Read 输出示例：

```text
[read-only virtual file: knowledge/refunds.md]
1:# Refunds
2:Refunds are reviewed within two business days.
```

数据库适配由宿主应用实现。`mink-core` 不依赖具体数据库；
[`crates/mink-core/examples/redb_vfs.rs`](../crates/mink-core/examples/redb_vfs.rs)
提供按 `resource_session_id` 隔离并惰性扫描的 redb 完整示例。

工具审批模式由 `--config $'[tools]\napproval_mode="write"'` 或 `.minkrc` 的 `[tools]` 配置控制：

| 模式 | 自动允许 | 阻止/等待审批 |
|------|----------|---------------|
| `yolo` | Read / Write / Exec | 无，默认 |
| `write` | Read / Write | Exec |
| `always-ask` | Read | Write / Exec |

当前版本还没有交互式审批 prompt；必然需要 prompt 的工具不会进入模型可见 surface，历史或
异常调用仍会在 runner 中 fail closed。可用 `[tools.approval]` 为单个工具设置 `allow`、
`deny` 或 `prompt`。

### 工具选择

`enabled_tools` 是唯一工具启用入口，可由 CLI `--enabled-tools`、`.minkrc`、`--config`、
Rust `AgentOptions` 或 SDK `options.enabled_tools` 设置。显式列表精确选择工具，空列表禁用
全部工具，未设置时使用 catalog 默认集合。`PythonSandbox` 属于 explicit-only 工具，不在
默认集合中，必须显式列出。未知、重复或当前构建 feature 不可用的名称会在创建 session 前
报错。

最终 surface 还会考虑 approval、主/子代理 role、filesystem backend 和硬依赖；schema、
语义能力、workflow、运行时引导和真实执行门禁都消费同一个解析结果。工具启用合同不包含
disable flag 或 sandbox `allow_*` 策略。

### 按需编译（PythonSandbox）

默认的完整 `mink` 终端构建包含 `PythonSandbox` 工具。SDK 精简二进制和最小构建默认不包含
wasmtime，可在构建时按需加入：

```bash
# 最小 mink 二进制（不含 TUI/REPL/PythonSandbox）
cargo build -p mink-cli --release --no-default-features --bin mink

# SDK 精简二进制 mink-core（不含 TUI/REPL/PythonSandbox）
cargo build -p mink-cli --release --no-default-features --features sdk-bin --bin mink-core

# SDK 精简二进制，手动加入 PythonSandbox
cargo build -p mink-cli --release --no-default-features --features "sdk-bin python-sandbox" --bin mink-core

# 完整终端构建（默认含 PythonSandbox）
cargo build --release
```

`mink-core` Rust 发布包只保留 runtime 和工具核心；REPL/TUI 相关依赖位于 `mink-cli`。
`--no-default-features` 可减少二进制体积约 30-40MB。`python-sandbox` feature 可与
`mink-cli` 的 `runtime` 或 `sdk-bin` 组合使用。


## `Read`

读取文件内容或轻量资源。

| 参数 | 类型 | 说明 |
|---|---|---|
| `path` | string | 文件路径或资源 URL |

- `path` 支持 selector：`file:10-20`、`file:10+5`、`file:raw`、`file:raw:10-20`。
- `hashline` 模式下，本地非 raw 输出包含基于完整文件内容的 snapshot header 和行号：

```text
[src/foo.rs#0A3B]
41:fn target() {
42:    ...
```

- `:raw` 禁用 header 和行号，且不生成 snapshot、不推进 seen-lines；Hashline Edit 仍要求先有一次非 raw 可编辑 Read。
- `replace` 模式保持普通行号输出，不生成 Hashline tag。resource/VFS Read 始终只读且不生成 tag。
- 注入 VFS 后，普通路径读取虚拟文件并显示只读标记，不生成 snapshot；selector 和 `:raw` 语义保持一致。
- `artifact://<id>` 可读取被截断工具输出，支持同样的行 selector；恢复和 fork 后引用保持稳定。
- `skill://list` / `skill://list/all` / `skill://<name>` / `skill://<name>/<relative-path>` 可通过当前 `Read` provider 读取可用 skills，列表、诊断视图、正文和 filesystem-backed skill 子资源来自同一 capability snapshot。`skill://all` 是 `skill://list/all` 的兼容别名；built-in/runtime skill 只支持读取正文，不支持子资源。selected skill 正文直接进入 prompt，不依赖此 provider。
- `rule://list` / `rule://<name>` 可读取可用 rules。
- `session://current`、`session://current/stats`、`session://current/messages`、`session://current/history`、`session://current/artifacts` 可读取当前 session 状态。
- 默认可读整文件，但大文件超过工具结果上限时返回头尾预览 + 行数 + selector 示例（不再纯报错）。
- 会话内重复读取同一未变更文件返回 "unchanged, no edits since. Reuse that content." 短响应（Read memo，本地文件）；Write/Edit 成功后失效，压缩后强制重读。
- 参数只接受 `path`（行范围用路径选择器）；未知字段被拒绝。
- 搜索具体内容时优先用 `Grep`，定位后再用 `Read` path selector 读取目标范围。
- UI 展示会额外加 `Read(path) [lines, bytes]` 摘要。
- **多模态读图**（会话开启图片能力时）：`Read` 本地图片（PNG/JPEG/GIF/WebP，magic
  嗅探）或 `image://sha256:<hex64>` 引用会捕获图片并附加到下一次 LLM 请求；结果展示
  文本摘要 `Image: WxH MIME (bytes) — path` 与 `[The image will be attached to the
  next model request.]`。图片路径不支持行 selector / `:raw`；默认限额 600 张/批、
  16MB 单批原始字节、16MB 单图、16384px 边长、1600 万像素（`[provider.image]` 可
  覆盖）。
- 已消费引用在下一次请求后投影为文本提示（`[Previously attached image: ...]`）；
  需要重新看图用 `Read image://sha256:<hex64>`（幂等）。能力关闭时图片走普通文本路径，
  `image://` 保持未知 scheme fail-closed。图片不写入 `artifacts/`。
- TUI 的 `Ctrl+V` 粘贴（见 [USAGE.md](USAGE.md#粘贴图片ctrlfvmacos)）不改变这里的捕获语义：
  它只把剪贴板 PNG 暂存为绝对路径并写进用户消息，图片仍必须经 `Read` 捕获。

## `Write`

覆盖写入文件。

| 参数 | 类型 | 说明 |
|---|---|---|
| `path` | string | 文件路径 |
| `content` | string | 完整文件内容 |

- 自动创建父目录。
- 写入上限默认 `FILE_WRITE_MAX_BYTES=1048576`。
- 这是覆盖写入，不是追加。
- UI 展示会加 `Write(path) [lines, bytes]` 摘要。

## `Edit`

编辑已有文件。runtime 启动时由 `edit_mode` 固定为一种协议；同一次模型请求只会看到该
模式的 schema 和提示词。旧 `path + patch`、`@PATH#TAG` 和
`old_string/new_string` 输入均不兼容。

### Hashline（默认）

唯一参数是 `input`：

```json
{"input":"[src/foo.rs#0A3B]\nPUT 41:\n+    return new_value;\nPUT >55:\n+println!(\"done\");"}
```

- section header 为 `[PATH#TAG]`，支持 quoted path 和单次调用中的多个文件。
- 支持 `PUT N.=M:`（以及 `PUT N:`/`PUT N-M:` 别名）、`PUT <N:`、`PUT >N:`、
  `PUT <1:`、`PUT >$:`、`CUT N.=M [@register]`、`PUT <N|>N|<1|>$ [@register]`、
  `PUT N.=M @register`、`REM` 和 `MV DEST`。
- 范围端点可改用**行文本锚点**：`PUT 'start line text'..'end line text':` /
  `CUT 'start'..'end':`（单/双引号）。锚点按 trim 后精确匹配文件行且必须唯一；
  范围边界由行文本匹配决定（消除行号 ±1 行静默错误），0 匹配与多匹配均为可诊断
  错误并保持 fail-closed。锚点文本内的 `*` 等符号按普通字符解析。
- body 的每一行以 `+` 开头；坐标始终引用 snapshot 的原始行，不因前序操作位移。
- 匿名 `CUT` 只在当前 Edit 调用内可用；命名寄存器（例如 `@saved`）保存在 session runtime，
  可跨 section、跨文件、跨 Edit 调用重复读取。
- snapshot 保存完整规范化文本和 seen-lines；同内容复用版本。上限为 30 个路径、每路径
  4 个版本、全局 64 MiB。
- 当前文件发生无关漂移时，所有锚点只有在能唯一映射且共享同一偏移时才恢复；目标已改、
  删除、拆分、上下文重复或偏移不一致均 fail closed。只有 HEAD/TAIL 操作可在 stale 内容上
  直接应用并返回 warning。
- `edit_enforce_seen_lines=true` 时，只允许锚定 Read/Grep 实际完整展示的行。有限错误回显会
  授权同 tag 直接重试；超长/超宽回显不会更新授权。
- 相同输入连续三次产生 no-op 时第三次升级为失败。多文件先全部预检，再顺序提交；中途失败
  会列出已提交、失败和未提交文件，但不宣称跨文件原子性。
- 明确不支持上游的 `N*` Block locator（非合法语法，按行号解析自然拒绝）；
  Mink 不引入 tree-sitter block resolver。

### Replace

```json
{
  "path": "src/foo.rs",
  "edits": [{"old_text": "old()", "new_text": "new()", "all": false}]
}
```

- `old_text` 不得为空；默认要求唯一匹配，歧义时返回候选行和预览；`all: true` 替换全部。
- edits 按顺序执行，后一个基于前一个结果；后续失败不回滚已经成功提交的 edit。
- 先执行 exact；没有 exact 时，对与 `old_text` 行数相同的候选窗口做空白/兼容标点归一化、
  相对缩进建模和字符相似度评分，并按上游 dominant-candidate 规则选取唯一候选。新文本会
  依照实际命中窗口转换缩进。
- `edit_fuzzy_match` 控制模糊阶段，`edit_fuzzy_threshold` 默认为 `0.95`。多个高置信度候选
  始终拒绝，不任意选择第一个；失败诊断包含最近相似度、差异行和有限预览。
- 不创建不存在的文件，只允许唯一 workspace suffix 路径恢复；保留 BOM、CRLF/LF 和末尾换行。

## `Bash`

执行 shell 命令。

| 参数 | 类型 | 说明 |
|---|---|---|
| `command` | string | shell 命令 |
| `timeout` | integer | 单条命令超时秒数，可选 |

- 命令在当前会话 `cwd` 下通过 `bash -lc` 执行。
- 空命令和危险命令会被安全策略拒绝。
- 用于读文件、搜索内容或发现路径的 Bash 命令会被拦截，提示改用 `Read`、`Grep` 或 `Glob`。
- 当 `Write` 或 `Edit` 同时位于模型工具 surface 时，系统提示词要求文件创建、完整覆盖和锚定修改优先使用专用 provider，不使用 Bash 重定向、heredoc、sed 或 awk 代替。
- 恢复首步守卫生效后，首个 Bash 调用还要单独满足 `FocusedVerificationExec`。这只是
  恢复首步资格，不改变普通 Bash 的误用拦截；
- 显式 `timeout` 为 `1..=tool_timeout_max` 时按原值执行；超过 `tool_timeout_max` 直接报错（fail closed），`0` 回退到全局 `tool_timeout`；未设置 `tool_timeout` 时默认值稳定夹在 5 到 `tool_timeout_max` 秒之间。`tool_timeout_max` 默认 600 秒，可在 `[tools]` 中配置，最低 5 秒。
- Ctrl+C / interrupt 会尝试中断子进程，返回 exit code 130 语义。
- stdout 和 stderr 合并返回，非零退出码会追加提示。

## `Python`

执行 Python 脚本（宿主环境）。可使用完整 Python 生态：网络、子进程、C 扩展均可用。
如需沙箱隔离环境，使用 `PythonSandbox` 工具。

| 参数 | 类型 | 说明 |
|---|---|---|
| `script` | string | 内联 Python 代码 |
| `script_file` | string | Python 文件路径 |
| `timeout` | integer | 超时秒数，可选 |

- `script` 和 `script_file` 必须二选一，不能同时提供。
- 在当前会话 `cwd` 下使用 `python3 -B -W ignore -c` 执行。
- 显式 `timeout` 为 `1..=tool_timeout_max` 时按原值执行；超过 `tool_timeout_max` 直接报错（fail closed），`0` 回退到全局 `tool_timeout`；未设置 `tool_timeout` 时默认值稳定夹在 5 到 `tool_timeout_max` 秒之间。`tool_timeout_max` 默认 600 秒，可在 `[tools]` 中配置，最低 5 秒。
- Ctrl+C / interrupt 会杀掉脚本并返回 interrupted 提示。

## `PythonSandbox`

在 CPython WASI 沙箱中执行 Python 代码。基于 wasmtime + CPython WASI（Brett Cannon 的 cpython-wasi-build），提供 WASI 级进程隔离。

仅配置的目录有读写权限，无子进程、无网络、无 C 扩展，完整 CPython 标准库可用。
默认不进入工具 surface；在 `enabled_tools` 中显式列出 `PythonSandbox` 后启用。

### python.wasm 说明

沙箱使用的 `python.wasm` 是 CPython 3.13+ 编译为 WASI 的二进制，需单独下载：

```bash
curl -sL "https://github.com/brettcannon/cpython-wasi-build/releases/download/v3.13.13/python-3.13.13-wasi_sdk-24.zip" -o python-wasi.zip
unzip python-wasi.zip -d cpython-wasi
```

项目结构：
```
cpython-wasi/
├── python.wasm          # ~29MB
├── lib/python3.13/      # 标准库
└── LICENSE
```

### 参数

| 参数 | 类型 | 说明 |
|---|---|---|
| `script` | string | 内联 Python 代码 |
| `script_file` | string | Python 文件路径 |
| `timeout` | integer | 超时秒数，可选，默认 30，范围 5-300 |

- 显式 `timeout` 为 `1..=300` 时按原值执行；超过 300 秒直接报错（fail closed），`0` 回退到 `[sandbox_python].timeout` 配置（默认 30，并钳制到 5-300）。

### 限制
- 完整 CPython 标准库（json/csv/re/math/datetime/xml 等）
- 无 C 扩展（numpy/pandas/lxml 不可用）
- 无子进程、无网络
- 仅配置的目录可读写

### 路径规则

通过 `os.chdir` 注入使相对路径自然工作：
```python
open("./output/f.txt", "w")                  # 相对路径 ✅
open("/absolute/path/to/project/output/f.txt", "w")  # 绝对路径 ✅
```

## `Glob`

文件路径匹配。

| 参数 | 类型 | 说明 |
|---|---|---|
| `pattern` | string | glob 模式 |
| `path` | string | 搜索目录，可选，默认当前目录 |

- 本地后端基于 ripgrep 的 `ignore::WalkBuilder` 和 `OverrideBuilder`，语义对齐 `rg --files -g <pattern>`，不依赖外部 `rg` 二进制；注入 VFS 后由后端枚举虚拟路径。
- 用于快速发现文件，不读取文件内容。
- `path` 为空或相对路径时基于当前会话 `cwd` 解析。
- VFS 模式下 `path` 是虚拟根路径，不与宿主 `cwd` 拼接。
- pattern 使用 `rg -g` override glob 语义；VFS 调用前仍由工具层校验。
- 裸文件名 glob 会递归匹配，例如 `*.rs` 匹配任意目录下的 Rust 文件；带路径分隔符的模式按相对路径匹配，例如 `src/*.rs` 只匹配 `src` 当前层，`src/**/*.rs` 匹配所有子层。
- `!pattern` 表示排除匹配项，和 `rg -g '!pattern'` 一致。
- 没有匹配时返回空输出，和 `rg --files -g` 一致。
- 遍历达到上限或跳过不可读路径时，会在结果末尾追加诊断行，提示结果可能不完整。
限制：
- 最多遍历 `max_search_files`（默认 5000）个文件后截断，可通过 `.minkrc` 的 `[tools] max_search_files` 或环境变量 `MAX_SEARCH_FILES` 调整。
- 输出超过 100KB 时搜索工具会先截断；最终工具结果还会受 `tool_result_max_bytes` 保护，超长内容可能落到 `artifact://<id>`。
- 遍历跳过不可读路径时追加诊断行。

## `Grep`

内容搜索。

| 参数 | 类型 | 说明 |
|---|---|---|
| `pattern` | string | 正则表达式 |
| `path` | string | 文件、目录或 registered resource URL，可选 |
| `glob` | string | 本地/VFS 文件过滤，可选 |
| `context` | integer | 匹配前后上下文行数，可选 |

- 本地后端基于 ripgrep 的 `ignore::WalkBuilder`、`OverrideBuilder`、`grep-regex`、`grep-searcher` 和 `grep-printer`，语义和输出对齐 `rg -n/-C -g`，不依赖外部 `rg` 二进制；注入 VFS 后由后端搜索虚拟内容。
- registered resource URL 由 `ResourceRouter` 解析后直接搜索返回文本，例如 `session://current/history`；resource path 不接受 selector 或 glob。
- 优先用于定位编辑目标。
- `path` 为空或相对路径时基于当前会话 `cwd` 解析；`glob` 过滤使用 `rg -g` override glob 语义。
- VFS 模式下 `path` 是虚拟根路径，不与宿主 `cwd` 拼接；regex 和文件 glob 在调用后端前仍由工具层校验。
- `context` 用于定位目标；Hashline 模式下可直接使用每个本地文件结果的 `[PATH#TAG]` 和实际展示行编辑。
- 输出采用 rg 标准格式：匹配行为 `path:line:content`，上下文行为 `path-line-content`，上下文块之间为 `--`。
- 未匹配内容时返回空输出；遍历达到上限或跳过不可读路径时，会追加诊断行。
限制：
- 最多遍历 `max_search_files`（默认 5000）个文件后截断。
- 最多返回 `max_search_results`（默认 1000）行匹配结果后截断。
- 可通过 `.minkrc` 的 `[tools] max_search_files`/`max_search_results` 或环境变量 `MAX_SEARCH_FILES`/`MAX_SEARCH_RESULTS` 调整。
- 输出超过 100KB 时搜索工具会先截断；最终工具结果还会受 `tool_result_max_bytes` 保护，超长内容可能落到 `artifact://<id>`。

截断提示含义：

- `scanned first N files`：触发 `max_search_files` 文件遍历上限。
- `truncated at N results`：触发 `max_search_results` 匹配结果数上限。
- `output > 100000 bytes`：触发搜索工具内部输出字节上限，和文件数/结果数上限无关。
- `[Full output: artifact://...]`：触发统一工具结果保护，完整输出已写入 session artifact。


## `TodoRead`

读取当前 session 的持久化 todo 状态。结果包含 revision、状态计数、稳定 ID 和条目正文；
默认省略已完成条目。

| 参数 | 类型 | 说明 |
|---|---|---|
| `include_completed` | boolean | 是否返回已完成条目，默认 `false` |

## `TodoWrite`

基于 `TodoRead` 或最新成功 todo 事件返回的 revision 和稳定 ID，原子修改列表结构。

| 参数 | 类型 | 说明 |
|---|---|---|
| `base_revision` | integer | 当前 todo revision，必填 |
| `add` | array | 新增 `pending` 条目；每项只含 `content` |
| `update` | array | 替换未完成条目正文；每项必须包含 `id` 和 `content` |
| `remove` | string[] | 删除非 active 条目的稳定 ID |

新条目始终从 `pending` 开始；进入 `in_progress` 必须使用 TodoAdvance。已完成条目必须先
reopen 才能修改正文；active 条目必须先 pause 或 complete 才能删除。TodoWrite 不修改任何
条目的状态。至少提供一个结构变更，同一批中的所有变更要么一起提交，要么全部不生效。
Todo 列表最多包含 256 项，单项正文最多 1024 字节，单次调用最多提交 128 个结构变更。

## `TodoAdvance`

基于最新 revision 原子转换一个或多个条目的进度状态。

| 参数 | 类型 | 说明 |
|---|---|---|
| `base_revision` | integer | 当前 todo revision，必填 |
| `complete` | string[] | `in_progress → completed` |
| `activate` | string[] | `pending → in_progress` |
| `pause` | string[] | `in_progress → pending` |
| `reopen` | string[] | `completed → pending` |

同一 ID 不能在一个调用中出现多次，也不能跳过合法来源状态。多个相关条目可以同时
`in_progress`，工具不会自动选择下一项或强制逐项执行。revision 过期、ID 不存在、非法转换、
重复 ID 或空更新都会失败，调用方需要重新 TodoRead 后重算。
单次调用最多提交 128 个状态转换。

TodoWrite / TodoAdvance 的成功结果都在 conversation 尾部追加两部分：本次增量
`<todo-event>`，以及包含 revision、状态计数和当前 active batch 的 `<current-todos>` 紧凑
物化投影。它们不会在每次请求前重新插入状态，因此不会因 todo 更新改写已有消息前缀。
恢复、fork 或压缩后若文件 revision 领先活跃历史，runtime 追加一次 TodoSync；历史 revision
领先文件时 fail closed。

状态保存在 session 的 `todos.json`，包含格式版本、revision、下一个 ID 序号和条目数组。
文件通过同目录临时文件和 rename 原子替换；缺失文件表示空列表，损坏或不支持的版本会在
runtime 启动时 fail closed。

`TodoStore` 由单个 runtime 持有并在进程内串行化更新，不提供跨进程文件锁；不要让多个 runtime
并发写同一个 session。runtime 运行期间也不支持通过外部编辑 `todos.json` 热更新状态。

## `PlanDraft`

创建、替换或取消当前计划草稿。

| 参数 | 类型 | 说明 |
|---|---|---|
| `content` | string | 完整 Markdown 草稿；空字符串表示取消未确认草稿 |

- 草稿写入由 `PlanStore` 在同目录原子替换。
- 已确认计划存在时拒绝创建或替换草稿。
- 每次修改都传完整草稿，不使用 `Write` / `Edit` 操作内部计划文件。
- 写入失败会返回错误结果，不会产生空成功。

## `PlanConfirm`

确认并锁定当前 `plan.draft`。

无参数。

- 仅在用户明确确认计划后调用。
- 通过原子 rename 触发 `plan.draft -> plan.md`，草稿文件随之消失。
- 成功工具结果写入后追加 confirmed 内部 user transition，不强制上下文压缩。
- 文件变更通过 session 内的 Plan transaction journal 与 conversation 追加协调；崩溃恢复会
  回滚尚未绑定成功结果的操作，或幂等补齐已经绑定的结果和 transition。
- 历史已压缩时，从 `plan.md` 投影 `<active-plan-checkpoint>`，不改变 immutable prefix。
- 状态转换由类型化 `PlanCommand` 和 `PlanStore` 处理，失败会原样进入工具结果。

## `PlanClear`

清空当前计划。

无参数。

- 在计划完成后调用。
- 删除 `plan.md` 并清理可能遗留的 `plan.draft`；确认计划不存在或为空时 fail closed。
- 成功工具结果写入后追加 cleared 内部 user transition，不强制上下文压缩。
- 文件变更与 conversation 追加使用同一可恢复 Plan transaction journal。
- 下一次 LLM 请求不再注入动态计划，immutable prefix 保持不变。
- 状态转换由类型化 `PlanCommand` 和 `PlanStore` 处理，失败会原样进入工具结果。

## `SubAgent`

启动子代理执行任务。

| 参数 | 类型 | 说明 |
|---|---|---|
| `prompt` | string | 子代理任务描述，必需 |
| `description` | string | 简短描述，可选 |
| `fork` | boolean | 是否继承父会话上下文，默认 false |

模式：

| 模式 | 上下文 | 适用 |
|------|--------|------|
| 独立模式 | 新 session | 独立文件调查、搜索、隔离验证 |
| Fork 模式 | 继承父会话完整 session 状态 | 需要父上下文的延续任务 |

行为：

- 同一批 SubAgent 最多 8 个并发。
- 子代理不能递归启动子代理。
- Fork 在 child runtime 初始化前克隆完整 session 目录（跳过已有 `subagents/`）；child 身份与遥测重新初始化。
- Fork 后 artifact 从克隆 index 的最大序号继续写入，不覆盖父历史正文。
- 子代理完成时会通过 parent display 发送完整 thinking/text。
- tool result 会注入父会话，格式为 `[sub-agent <id>] <status> (in=<n>, out=<n>) ...`。
- 超时后未完成项返回 `Sub-agent timed out after <n>s.`，并取消对应子代理。
