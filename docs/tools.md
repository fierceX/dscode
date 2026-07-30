# 内置工具

更新日期：2026-07-30

本文是 mink 内置工具的协议参考，面向需要理解工具参数、执行模型、结果通道、资源 URL、
审批和构建裁剪的使用者与开发者。CLI 参数、session、沙箱、技能和常见工作流见
[使用手册](USAGE.md)；模块分层和内部数据流见 [ARCHITECTURE.md](ARCHITECTURE.md)；工具
surface、语义能力和跨工具 workflow 的设计见
[工具能力与提示词解耦设计文档](设计哲学-工具能力与提示词解耦.md)。

## 执行模型

所有工具通过 `tools/runner.rs` 中的 `ToolExec` trait 注册到 `TOOL_REGISTRY`，由 `ToolRunner::execute_all()` 调度。只读工具会按连续批次并发执行；写入、执行、控制和 SubAgent 类工具按模型调用顺序串行执行，避免同批工具之间出现读写竞态。每个工具同时声明 `ToolMetadata`，包含 approval tier、结果类型、副作用、storm 例外和 discoverable 标记。

统一流程：

```text
ToolCallEvent
  -> resolved ModelToolSurface 执行门禁
  -> StormBreaker 检查
  -> repair_truncated_json()
  -> ToolExec::execute()
  -> format_dispatched_result() 生成 ToolRunResult
     -> 普通结果执行大小保护、Bash noise filter、Read-Write summary、Edit conv_content
     -> Plan/SubAgent 结果标记为待定稿
  -> PlanActionHandler 生成 Plan effect / 压缩请求；SubAgentCoordinator 完成延迟工作
  -> finalize_deferred_results() 对延迟结果执行大小保护
  -> SignalCollector 只观察最终 ToolRunResult
```

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

工具审批模式由 `--config 'approval_mode=\"write\"'` 或 `.minkrc` 的 `[tools]` 配置控制：

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
语义能力、workflow、运行时引导和真实执行门禁都消费同一个解析结果，不再存在 disable flag
或 sandbox `allow_*` 工具策略。

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
- 本地非 raw 输出包含 snapshot header 和行号，适合 anchored edit：

```text
@src/foo.rs#0A3B
41:fn target() {
42:    ...
```

- `:raw` 禁用 snapshot header 和行号。
- 注入 VFS 后，普通路径读取虚拟文件并显示只读标记，不生成 snapshot；selector 和 `:raw` 语义保持一致。
- `artifact://<id>` 可读取被截断工具输出，支持同样的行 selector；恢复和 fork 后引用保持稳定。
- `skill://list` / `skill://list/all` / `skill://<name>` / `skill://<name>/<relative-path>` 可通过当前 `Read` provider 读取可用 skills，列表、诊断视图、正文和 filesystem-backed skill 子资源来自同一 capability snapshot。`skill://all` 是 `skill://list/all` 的兼容别名；built-in/runtime skill 只支持读取正文，不支持子资源。selected skill 正文直接进入 prompt，不依赖此 provider。
- `rule://list` / `rule://<name>` 可读取可用 rules。
- `session://current`、`session://current/stats`、`session://current/messages`、`session://current/history`、`session://current/artifacts` 可读取当前 session 状态。
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

编辑已有文件。仅支持 anchored patch；新建或完整覆盖文件使用 `Write`。

| 参数 | 类型 | 说明 |
|---|---|---|
| `path` | string | 文件路径 |
| `patch` | string | anchored line patch |

- `patch` 第一行必须是最近一次非 raw `Read` 输出中的 `@PATH#TAG` header，或上一次成功 `Edit` 结果返回的新 header。
- patch 支持 `replace N..M:`、`replace N:`、`delete N..M`、`delete N`、`insert before N:`、`insert after N:`、`insert head:`、`insert tail:`。
- patch 中的行号来自 snapshot 的原始文件行号；同一次 patch 内多个 hunk 的行号不会因为前面的 hunk 而位移。
- patch body 行只出现在 `replace` / `insert` header 后，必须以 `+` 开头，且只能写最终新内容；不要写 `-old` 行、原始行或上下文行。
- patch range 应保持 tight，只覆盖实际变化的行。要修改不连续行时，使用多个 hunk。
- snapshot 对应一个文件状态；同一文件成功 `Edit` 或 `Write` 后，之前的 snapshot tag 和行号都视为过期。
- 同一文件如果要修改多个位置，优先在一次 `Edit.patch` 中合并多个 hunk。
- 成功 `Edit` 会返回新的 `@PATH#TAG` 和修改区域附近的行号，可用于紧接着在该可见区域继续编辑；其他区域应重新 `Read`。
- snapshot 过期、未知 tag、未覆盖行、no-op 或任何无法完全解释的结果，都应先按工具提示重新 `Read path:N-M`，再用新 header 重试。
- 不要用 `Edit` 做机械格式化、import 排序、空白清理或纯缩进调整；语义修改后运行项目 formatter。

示例：

```text
@src/foo.rs#0A3B
replace 41..41:
+    return new_value;
insert after 55:
+println!("done");
```

反例：

```text
# 错误：body 不能包含 -old 或无前缀上下文行
replace 41..41:
-    old_value()
+    new_value()

# 错误：为了改第 2 和第 5 行而吞掉 3-4 行
replace 2..5:
+...
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
- 当 `Write` 或 `Edit` 同时位于模型工具 surface 时，系统提示词要求文件创建、完整覆盖和锚定修改优先使用专用 provider，不使用 Bash 重定向、heredoc、sed 或 awk 代替。
- `SIGNAL_RECOVERY` 注入后的首个 Bash 调用还要单独满足 `FocusedVerificationExec`。这只是
  Recovery 首步资格，不改变普通 Bash 是否可以执行；
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
- 最多遍历 `max_search_files`（默认 5000）个文件后截断，可通过 `.minkrc` 的 `max_search_files` 或环境变量 `MAX_SEARCH_FILES` 调整。
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
- `context` 用于定位目标；需要修改时必须 `Read` 目标范围拿到 `@PATH#TAG` 后使用 anchored `Edit.patch`。
- 输出采用 rg 标准格式：匹配行为 `path:line:content`，上下文行为 `path-line-content`，上下文块之间为 `--`。
- 未匹配内容时返回空输出；遍历达到上限或跳过不可读路径时，会追加诊断行。
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
- 请求上下文压缩；请求统一经过 `TurnCompactor` 的同轮一次守卫，失败会返回当前 turn。
- 下一次 LLM 请求将计划作为动态 `<current-plan>` system message 注入，不改变 immutable prefix。
- 状态转换由类型化 `PlanCommand` 和 `PlanStore` 处理，失败会原样进入工具结果。

## `PlanClear`

清空当前计划。

无参数。

- 在计划完成后调用。
- 删除 `plan.md` 并清理可能遗留的 `plan.draft`；确认计划不存在或为空时 fail closed。
- 请求上下文压缩；请求统一经过 `TurnCompactor` 的同轮一次守卫，失败会返回当前 turn。
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
