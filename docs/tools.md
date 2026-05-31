# 内置工具

更新日期：2026-05-31

## 执行模型

所有工具通过 `tools/runner.rs` 中的 `ToolExec` trait 注册到 `TOOL_REGISTRY`，由 `ToolRunner::execute_all()` 并发分发。

统一流程：

```text
ToolCallEvent
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

## `Read`

读取文件内容。

| 参数 | 类型 | 说明 |
|---|---|---|
| `path` | string | 文件路径 |
| `offset` | integer | 起始行号，1-indexed，可选 |
| `limit` | integer | 读取行数，可选 |

- 输出包含行号，适合编辑前定位。
- 默认可读整文件，但大文件会受到工具结果上限保护。
- 搜索具体内容时优先用 `Grep`，定位后再用 `Read offset/limit`。
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

精确字符串替换。

| 参数 | 类型 | 说明 |
|---|---|---|
| `path` | string | 文件路径 |
| `old_string` | string | 要替换的原文本 |
| `new_string` | string | 替换后的文本 |

- `old_string` 必须 byte-for-byte 精确匹配，包括缩进、空格和换行。
- 不支持正则。
- 适合小范围修改。
- conversation 中默认只保留结果首行，避免 diff 过度污染上下文；UI 仍可展示完整工具内容。

## `Bash`

执行 shell 命令。

| 参数 | 类型 | 说明 |
|---|---|---|
| `command` | string | shell 命令 |
| `timeout` | integer | 单条命令超时秒数，可选 |

- 命令通过 `bash -lc` 执行。
- 空命令和危险命令会被安全策略拒绝。
- 显式 `timeout` 优先；未设置时使用全局 `--tool-timeout` 和自适应超时，最大 600 秒。
- Ctrl+C / interrupt 会尝试中断子进程，返回 exit code 130 语义。
- stdout 和 stderr 合并返回，非零退出码会追加提示。

## `Python`

执行受限 Python 脚本。

| 参数 | 类型 | 说明 |
|---|---|---|
| `script` | string | 内联 Python 代码 |
| `script_file` | string | Python 文件路径 |
| `timeout` | integer | 超时秒数，可选，默认 30，范围 5-300 |

- `script` 和 `script_file` 必须二选一，不能同时提供。
- 使用 `python3 -B -W ignore -c` 执行。
- 黑名单拦截 `subprocess`、`os.system`、`os.popen`、`shutil`、`ctypes`、`socket`、`pty`、`__import__`、`compile(`、`exec(`、`eval(` 等模式。
- Ctrl+C / interrupt 会杀掉脚本并返回 interrupted 提示。
- Web、系统调用、任意 import 绕过不属于该工具的目标能力。

## `Glob`

文件路径匹配。

| 参数 | 类型 | 说明 |
|---|---|---|
| `pattern` | string | glob 模式 |
| `path` | string | 搜索目录，可选，默认当前目录 |

- 基于 ripgrep 文件列表能力。
- 用于快速发现文件，不读取文件内容。

## `Grep`

内容搜索。

| 参数 | 类型 | 说明 |
|---|---|---|
| `pattern` | string | 正则表达式 |
| `path` | string | 文件或目录，可选 |
| `glob` | string | 文件过滤，可选 |
| `context` | integer | 匹配前后上下文行数，可选 |

- 基于 `rg -n`。
- 优先用于定位编辑目标。
- `context` 往往足够直接构造 `Edit old_string`。

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

## `Skill`

按需加载 skill。

| 参数 | 类型 | 说明 |
|---|---|---|
| `name` | string | skill 名称 |

- 优先读取编译期嵌入 skill。
- 文件系统 fallback 路径由 `prompt::resolve_skill_file()` 决定。
- 返回内容只作为当前工具结果进入对话，不会永久修改后续 system prompt。

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

- 基于 Jina AI Search API。
- 依赖 `JINA_API_KEY`。
- 默认 HTTP 超时 30 秒。
- 回答中应带来源链接。

## `WebFetch`

获取网页内容。

| 参数 | 类型 | 说明 |
|---|---|---|
| `url` | string | 完整 URL |

- 基于 Jina AI Reader API。
- 依赖 `JINA_API_KEY`。
- 默认 HTTP 超时 60 秒。
- HTTP URL 会升级为 HTTPS。
- 不适合需要认证的私有 URL。
