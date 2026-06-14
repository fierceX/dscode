# 内置工具

更新日期：2026-06-06

## 执行模型

所有工具通过 `tools/runner.rs` 中的 `ToolExec` trait 注册到 `TOOL_REGISTRY`，由 `ToolRunner::execute_all()` 调度。只读工具会按连续批次并发执行；写入、执行、控制和 SubAgent 类工具按模型调用顺序串行执行，避免同批工具之间出现读写竞态。每个工具同时声明 `ToolMetadata`，包含 approval tier、结果类型、副作用、storm 例外和 discoverable 标记。

统一流程：

```text
ToolCallEvent
  -> Approval 检查
  -> StormBreaker 检查
  -> repair_truncated_json()
  -> ToolExec::execute()
  -> format_tool_result(tool_result_max_bytes)
  -> Bash noise filter / Read-Write summary / Edit conv_content
  -> ToolRunResult
```

工具结果有两个内容通道：

- `content`：工具层截断/过滤后的展示内容，会进入 UI 和默认 LLM tool result。
- `conversation_content` / `conv_content`：工具自定义给 LLM 的精简内容。非空时优先写入 conversation。

默认 `tool_result_max_bytes` 为 `100000`，可通过环境变量 `TOOL_RESULT_MAX_BYTES` 调整。

当工具输出超过上限时，完整输出会保存到当前 session 的 `artifacts/` 目录，工具结果中追加 `artifact://<id>`。可用 `Read` 按需读取，例如 `artifact://bash-0001:1-120`。

`Read` 也承担轻量资源路由职责。当前只实现高收益、低耦合的内置协议分支，没有引入完整 `ResourceRouter` 框架：

- `http(s)://...`：读取公开 URL，首次 fetch 后写入当前 session artifact cache，后续同 URL 的 selector 从缓存分页；如果 cache index 命中但正文 artifact 丢失，会重新 fetch 并写入新缓存。
- `artifact://<id>`：读取被截断工具输出。
- `skill://list` / `skill://<name>`：列出或读取可用 skill；本地 skill 优先，内置 skill 兜底。
- `session://current`：读取当前 session 摘要。
- `session://current/stats`：读取当前 session stats JSON。
- `session://current/messages`：读取最近 40 条 conversation 摘要。
- `session://current/messages/all`：读取全部 conversation 摘要。
- `session://current/artifacts`：列出当前 session artifacts。

这些资源都支持同样的行 selector，例如 `session://current/messages:1-20` 或 `https://example.com:20-60`。

工具审批模式由 `--config 'approval_mode=\"write\"'` 或 `.minkrc` 的 `[tools]` 配置控制：

| 模式 | 自动允许 | 阻止/等待审批 |
|------|----------|---------------|
| `yolo` | Read / Write / Exec | 无，默认 |
| `write` | Read / Write | Exec |
| `always-ask` | Read | Write / Exec |

当前版本还没有交互式审批 prompt；需要审批的调用会 fail closed，并返回工具错误。可用 `[tools.approval]` 为单个工具设置 `allow`、`deny` 或 `prompt`。

### 工具白名单

工具列表可通过两种方式过滤，减少 LLM 可见的工具数量以节省 token 并提升遵循率：

1. **禁用开关**（CLI `--disable-bash` 等，或 SDK `options.disable_*`）：按类别禁用，如 Bash、Python、SubAgent、Web 工具。
2. **白名单 `enabled_tools`**（`.minkrc` 或 `--config` 或 SDK `options.enabled_tools`）：精确指定允许的工具名称列表。未在列表中的工具对 LLM 不可见。`None` 或未设置表示全部启用（受禁用开关约束）。

两种方式可同时使用：白名单先限定可见工具集，禁用开关进一步从中移除。

### 按需编译（PythonSandbox）

`PythonSandbox` 工具（wasmtime 沙箱）默认编译进二进制，但可在构建时按需裁剪：

```bash
# 最小 mink 二进制（不含 TUI/REPL/PythonSandbox）
cargo build -p mink-cli --release --no-default-features

# SDK 精简二进制 mink-core（不含 TUI/REPL/PythonSandbox）
cargo build -p mink-cli --release --no-default-features --features sdk-bin --bin mink-core

# 完整构建（含 PythonSandbox，默认）
cargo build --release
```

`--no-default-features` 可减少二进制体积约 30-40MB。`python-sandbox` feature 也可与
`runtime` 或 `sdk-bin` 组合使用。


## `Read`

读取文件内容或轻量资源。

| 参数 | 类型 | 说明 |
|---|---|---|
| `path` | string | 文件路径或资源 URL |

- `path` 支持 selector：`file:10-20`、`file:10+5`、`file:raw`、`file:raw:10-20`。
- 输出包含 snapshot header 和行号，适合 anchored edit：

```text
@src/foo.rs#0A3B
41:fn target() {
42:    ...
```

- `:raw` 禁用 snapshot header 和行号。
- `http(s)://...` 可读取公开 URL。URL 输出不生成 editable snapshot；首次读取会保存为 `ReadUrl` artifact cache，后续同 URL selector 从缓存分页，不重复 fetch。损坏的 URL cache index 行会被跳过；cache 正文缺失时会重新 fetch。
- `artifact://<id>` 可读取被截断工具输出，支持同样的行 selector。
- `skill://list` / `skill://<name>` 可读取可用 skills，搜索顺序与 `--config` 或 `.minkrc` 的 `skills` 字段一致。
- `session://current`、`session://current/stats`、`session://current/messages`、`session://current/artifacts` 可读取当前 session 状态。
- 默认可读整文件，但大文件会受到工具结果上限保护。
- 搜索具体内容时优先用 `Grep`，定位后再用 `Read` path selector 读取目标范围。
- UI 展示会额外加 `Read(path) [lines, bytes]` 摘要。

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

编辑文件。仅支持 anchored patch。

| 参数 | 类型 | 说明 |
|---|---|---|
| `path` | string | 文件路径 |
| `patch` | string | anchored line patch |

- `patch` 必须使用最近一次非 raw `Read` 输出中的 `@PATH#TAG` header。
- patch 支持 `replace N..M:`、`delete N..M`、`insert before N:`、`insert after N:`、`insert head:`、`insert tail:`。
- patch body 行必须以 `+` 开头。
- patch 只能修改 snapshot 覆盖且未漂移的行；文件变化时会拒绝并要求重新 `Read`。
- 同一文件如果要修改多个位置，优先在一次 `Edit.patch` 中合并多个 hunk。
- 同一文件成功 `Edit` 或 `Write` 后，之前的 snapshot tag 都视为过期；继续编辑前重新 `Read` 目标范围。
- snapshot 过期、未知 tag、未覆盖行或 no-op 错误会给出建议 `Read path:N-M` 范围。
- conversation 中默认只保留结果首行，避免 diff 过度污染上下文；UI 仍可展示完整工具内容。

示例：

```text
@src/foo.rs#0A3B
replace 41..43:
+fn target() {
+    new_value()
+}
insert after 55:
+println!("done");
```

## `Bash`

执行 shell 命令。

| 参数 | 类型 | 说明 |
|---|---|---|
| `command` | string | shell 命令 |
| `timeout` | integer | 单条命令超时秒数，可选 |

- 命令在当前会话 `cwd` 下通过 `bash -lc` 执行。
- 空命令和危险命令会被安全策略拒绝。
- 用于读文件、搜索内容或发现路径的 Bash 命令会被拦截，提示改用 `Read`、`Grep` 或 `Glob`。
- 显式 `timeout` 优先；未设置时使用全局 `tool_timeout`（`--config` 或 `.minkrc` 设置），默认超时会稳定夹在 5 到 600 秒之间，不再根据历史执行耗时自适应调整。
- Ctrl+C / interrupt 会尝试中断子进程，返回 exit code 130 语义。
- stdout 和 stderr 合并返回，非零退出码会追加提示。

## `Python`

执行 Python 脚本（宿主环境）。可使用完整 Python 生态：网络、子进程、C 扩展均可用。
如需沙箱隔离环境，使用 `PythonSandbox` 工具。

| 参数 | 类型 | 说明 |
|---|---|---|
| `script` | string | 内联 Python 代码 |
| `script_file` | string | Python 文件路径 |
| `timeout` | integer | 超时秒数，可选，默认 30，范围 5-300 |

- `script` 和 `script_file` 必须二选一，不能同时提供。
- 在当前会话 `cwd` 下使用 `python3 -B -W ignore -c` 执行。
- Ctrl+C / interrupt 会杀掉脚本并返回 interrupted 提示。

## `PythonSandbox`

在 CPython WASI 沙箱中执行 Python 代码。基于 wasmtime + CPython WASI（Brett Cannon 的 cpython-wasi-build），提供 WASI 级进程隔离。

仅配置的目录有读写权限，无子进程、无网络、无 C 扩展，完整 CPython 标准库可用。
默认禁用，需通过 `--enable-python-sandbox` 或 `.minkrc` 中 `[sandbox_python] enable = true` 启用。

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

- 基于 `globset` 匹配和 `ignore` 目录遍历，不依赖外部 `rg` 二进制。
- 用于快速发现文件，不读取文件内容。
- `path` 为空或相对路径时基于当前会话 `cwd` 解析。
- pattern 透传给 `globset`，工具层不做自定义 glob 解析。
- `*` 和 `?` 不跨路径分隔符；递归匹配使用 `**/`，例如 `**/*.rs`、`**/*.docx`、`**/*.*`。
- 带路径分隔符的模式按相对路径匹配，例如 `src/*.rs` 只匹配 `src` 当前层，`src/**/*.rs` 匹配所有子层。
- 没有匹配时返回明确的 no-match 提示，而不是静默空字符串。
- 遍历达到上限或跳过不可读路径时，会在结果末尾追加诊断行，提示结果可能不完整。
限制：
- 最多遍历 `max_search_files`（默认 5000）个文件后截断，可通过 `.minkrc` 的 `max_search_files` 或环境变量 `MAX_SEARCH_FILES` 调整。
- 输出超过 100KB 时搜索工具会先截断；最终工具结果还会受 `tool_result_max_bytes` 保护，超长内容可能落到 `artifact://<id>`。
- 遍历跳过不可读路径时追加诊断行。

## `Grep`

内容搜索。

| 参数 | 类型 | 说明 |
|---|---|---|
| `pattern` | string | 正则表达式 |
| `path` | string | 文件或目录，可选 |
| `glob` | string | 文件过滤，可选 |
| `context` | integer | 匹配前后上下文行数，可选 |

- 基于内置目录遍历和 Rust `regex` 搜索，不依赖外部 `rg` 二进制。
- 优先用于定位编辑目标。
- `path` 为空或相对路径时基于当前会话 `cwd` 解析；`glob` 过滤同样使用 `globset` 语义。
- `context` 往往足够定位目标；需要修改时优先 `Read` 目标范围拿到 `@PATH#TAG` 后使用 anchored `Edit.patch`。
- 未匹配内容时返回明确的 no-match 提示；遍历达到上限或跳过不可读路径时，会追加诊断行。
限制：
- 最多遍历 `max_search_files`（默认 5000）个文件后截断。
- 最多返回 `max_search_results`（默认 1000）行匹配结果后截断。
- 可通过 `.minkrc` 的 `max_search_files`/`max_search_results` 或环境变量 `MAX_SEARCH_FILES`/`MAX_SEARCH_RESULTS` 调整。
- 输出超过 100KB 时搜索工具会先截断；最终工具结果还会受 `tool_result_max_bytes` 保护，超长内容可能落到 `artifact://<id>`。

截断提示含义：

- `scanned first N files`：触发 `max_search_files` 文件遍历上限。
- `truncated at N results`：触发 `max_search_results` 匹配结果数上限。
- `output > 100000 bytes`：触发搜索工具内部输出字节上限，和文件数/结果数上限无关。
- `[Full output: artifact://...]`：触发统一工具结果保护，完整输出已写入 session artifact。


## `TodoWrite`

维护当前会话的 checklist。

| 参数 | 类型 | 说明 |
|---|---|---|
| `todos` | array | 完整 todo 列表，每项含 `content` 和 `status` |

`status` 只能是：

- `pending`
- `in_progress`
- `completed`

约束：

- 每次传完整列表，不传增量 diff。
- 最多一个 `in_progress`。
- 结果以 Markdown checklist 返回。

## `PlanConfirm`

确认并锁定当前 `plan.draft`。

无参数。

- 仅在用户明确确认计划后调用。
- 触发 `plan.draft -> plan.md`。
- 触发上下文压缩和 immutable prefix 失效。
- 工具本身是 internal tool，具体副作用由 `PlanActionHandler` 处理。

## `PlanClear`

清空当前计划。

无参数。

- 在计划完成后调用。
- 清空 `plan.md`。
- 触发上下文压缩和 immutable prefix 失效。
- 工具本身是 internal tool，具体副作用由 `PlanActionHandler` 处理。

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
| Fork 模式 | 继承父会话 conversation / plan / skills | 需要父上下文的延续任务 |

行为：

- 同一批 SubAgent 最多 8 个并发。
- 子代理不能递归启动子代理。
- 子代理完成时会通过 parent display 发送完整 thinking/text。
- tool result 会注入父会话，格式为 `[sub-agent <id>] <status> (in=<n>, out=<n>) ...`。
- 超时后未完成项返回 `Sub-agent did not complete.`。

## `WebSearch`

网络搜索。

| 参数 | 类型 | 说明 |
|---|---|---|
| `query` | string | 搜索关键词 |

- 基于 DuckDuckGo Lite GET，失败或空结果时回退到 DuckDuckGo HTML POST，不需要 API key。
- 会识别 DuckDuckGo `anomaly.js` / challenge 页面，并明确返回反爬挑战错误，而不是伪装成空结果。
- 默认使用 Firefox-like User-Agent 和基础浏览器导航请求头，可用 `MINK_WEB_USER_AGENT` 覆盖 UA。
- 默认 HTTP 超时 15 秒。
- 回答中应带来源链接。

## `WebFetch`

获取网页内容。

| 参数 | 类型 | 说明 |
|---|---|---|
| `url` | string | 完整 URL |

- 直接通过 HTTP GET 获取公开网页，不需要 API key。
- HTML 会做轻量文本抽取，非 HTML 内容直接返回正文。
- 默认使用 Firefox-like User-Agent 和基础浏览器导航请求头，可用 `MINK_WEB_USER_AGENT` 覆盖 UA。
- 默认 HTTP 超时 60 秒。
- HTTP URL 会升级为 HTTPS。
- 不适合需要认证的私有 URL。
