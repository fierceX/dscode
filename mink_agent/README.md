# mink-agent 使用文档

## 简介

mink-agent 是 [mink](https://github.com/xialuyu/mink) 的 Python 封装。SDK 专用的 `mink-core` 二进制内置在 pip 包中，无需额外安装。

```bash
pip install mink-agent
```

安装时会自动选择匹配当前系统平台（macOS arm64/x86_64、Linux x86_64/aarch64）的 wheel 包。

## 快速开始

```python
from mink_agent import AgentSession, SandboxConfig

config = SandboxConfig(
    api_key="sk-...",                            # 或设置 DEEPSEEK_API_KEY 环境变量
    read_dirs=["/path/to/project/src"],           # agent 可读取的目录
    write_dirs=["/path/to/project/src"],          # agent 可写入的目录
    signal_mode="full",                           # 可选："full" 启用信号系统，"off" 关闭
    stream_events=True,                            # 可选：是否输出过程事件
)

session = AgentSession(config)
result = session.run("把 src/handler.rs 重构成使用 Result 类型")
print(result["text"])
session.close()
```

### 单次快捷调用

```python
from mink_agent import quick_run

result = quick_run(
    "解释这段代码",
    read_dirs=["/path/to/project"],
    api_key="sk-...",
)
print(result["text"])
```

## AgentSession API

### `run(prompt, *, extra_options=None, on_event=None) -> dict`

执行一个提示词并返回聚合结果。每次调用都会启动一个新的 `mink-core --agent-jsonl` 进程；持续交互通过相同的 `mink_home + session_id` 复用磁盘 session，而不是复用同一个进程。同一个 `AgentSession` 实例不支持并发调用；并发任务应创建多个实例或由外层应用排队。

Rust core 会在 `conversation.jsonl` 中完整保留历史，并通过 `context-state.json` 只恢复当前活跃后缀。
压缩不会删除旧消息；长 session 的冷历史留在磁盘，不会在每次调用中全部常驻内存。
`session://current/history` 提供从完整 `conversation.jsonl` 生成的有损检索视图；原始 thinking
和完整工具输出仍需读取 `conversation.jsonl` 或相关 artifact。

默认情况下 `run()` 会消费 Rust 侧输出的过程事件并聚合 `text`、`thinking`、工具调用和最终状态。传入 `on_event` 时，每个归一化后的 `AgentStreamEvent` 会同步回调给调用方，适合在不直接迭代 stream 的场景里做 UI 增量更新。

如果 `SandboxConfig.stream_events=False`，SDK 会在 Agent JSONL request 的 `options` 中传入 `stream_events=false`。Rust 侧不会向 stdout 输出 `thinking`、`text`、`tool_call`、`tool_result` 等过程事件，只输出最终 `final`；`run()` 会从 session `conversation.jsonl` 回读最后一条 assistant 消息，尽量补齐最终 `text` / `thinking`。这个模式用于不需要流式展示的长程任务，可减少 stdout 事件处理开销。

| 返回字段 | 类型 | 说明 |
|---------|------|------|
| `text` | `str` | agent 的文本回复 |
| `thinking` | `str` | agent 的推理过程（如有） |
| `tool_calls` | `list[dict]` | agent 执行的工具调用记录 |
| `tool_results` | `list[dict]` | 工具结果事件 |
| `events` | `list[dict]` | 本次调用的完整 JSONL 事件 |
| `status` | `str or None` | `final.status`，如 `ok`、`failed`、`interrupted` |
| `session_id` | `str or None` | Rust 侧解析后的真实 session id |
| `session_ref` | `str or None` | 调用方传入的 session 引用或真实 id |
| `home` | `str or None` | 实际 `MINK_HOME` |
| `events_path` | `str or None` | session `events.jsonl` 路径 |
| `conversation_path` | `str or None` | session `conversation.jsonl` 路径 |
| `artifacts_dir` | `str or None` | session artifacts 目录 |
| `summary_path` | `str or None` | session summary 路径 |
| `usage_path` | `str or None` | session `usage.jsonl` 路径 |
| `billing_turn_id` | `str or None` | 本次根用户 Turn 的计量归属标识 |
| `usage_records` | `list[dict]` | 本次 Turn 的 Agent、压缩和子代理 LLM 请求明细 |
| `usage` | `dict` | 本次 Turn 的 Token、请求次数和人民币费用汇总 |
| `tool_call_count` | `int` | 本次调用执行的工具调用数量 |
| `tool_error_count` | `int` | 本次调用检测到的工具错误数量 |
| `exit_code` | `int` | 进程退出码（0 表示成功） |
| `error` | `str or None` | 错误信息（如有） |
| `stderr` | `str` | 进程原始错误输出（调试用） |

### `stream(prompt, *, extra_options=None) -> Iterator[dict]`

兼容旧版本的流式接口，逐条产出原始 dict 事件。新代码优先使用 `stream_events()`。

### `raw_stream(prompt, *, extra_options=None) -> Iterator[dict]`

执行一个提示词并逐条产出 Rust Agent JSONL 协议的原始 dict 事件。普通事件会即时返回；最终事件为 `{"type": "final", ...}`，其中包含 `status`、session 路径和 stderr。

### `stream_events(prompt, *, extra_options=None) -> Iterator[AgentStreamEvent]`

执行一个提示词并逐条产出归一化事件对象。`AgentStreamEvent.type` 常见值：

| 类型 | 说明 |
|------|------|
| `thinking_delta` | 中间思考增量 |
| `answer_delta` | 最终回答文本增量 |
| `tool_call` | 工具调用开始 |
| `tool_result` | 工具调用结果 |
| `final` | 本次调用完成，包含状态和 session 路径 |

事件对象可通过 `event.to_dict()` 转回 dict。QA/聊天类前端建议使用 `thinking_delta` 和 `answer_delta` 分离展示中间思考和最终回答。

### `close()`

清理资源。会话目录保留在磁盘上，可供查阅。

## SandboxConfig 配置项

### 会话与路径

| 参数 | 默认值 | 说明 |
|------|--------|------|
| `mink_home` | 用户 home 目录 | 传给 Rust 的 `MINK_HOME` 根目录。也可通过 `MINK_HOME` 环境变量设置 |
| `session_layout` | `"home"` | session 路径布局。Python SDK 默认写入 `mink_home/.mink/sessions/<session_id>`；可设为 `"project"` 使用 CLI 兼容布局，`"direct"` 写入 `mink_home/<session_id>`，或 `"isolated"` 直接使用 `mink_home` 作为当前 session 目录 |
| `mission_file` | `None` | 自定义系统提示词文件路径（MISSION.md） |
| `mission_content` | `None` | 自定义系统提示词内容（字符串），与 `mission_file` 二选一 |
| `cwd` | 当前目录 | agent 的工作目录 |

### 文件系统访问

| 参数 | 默认值 | 说明 |
|------|--------|------|
| `read_dirs` | `[]` | agent 可读取的目录（相对路径基于 cwd 解析） |
| `write_dirs` | `[]` | agent 可写入的目录 |

### 工具控制

| 参数 | 默认值 | 说明 |
|------|--------|------|
| `enabled_tools` | `None` | 精确工具选择；`None` 使用默认集合，`[]` 禁用全部，显式列出 `PythonSandbox` 才启用它 |
| `allow_network` | `True` | 是否允许沙箱进程访问网络（LLM API 需要） |

### 资源限制

| 参数 | 默认值 | 说明 |
|------|--------|------|
| `timeout_secs` | `600` | agent 运行总超时（秒），超时会终止 agent 进程组 |
| `tool_timeout` | `600` | 单次工具调用超时（秒） |
| `sub_agent_timeout` | `300` | 子代理执行超时（秒） |
| `llm_first_event_timeout` | `60` | 等待首个模型 stream event 的秒数 |
| `llm_idle_timeout` | `90` | 模型 stream 空闲超时（秒） |
| `llm_wait_heartbeat` | `30` | 等待模型响应的提示间隔；设为 `0` 关闭提示 |
| `max_tokens` | `81920` | 输出 token 上限 |
| `max_turns` | `40` | 最大循环轮数 |
| `max_context` | `1000000` | 模型上下文 token 上限；设为 `0` 时禁用自动压缩和本地输入预算限制 |
| `context_compact_pct` | `94` | 自动压缩触发百分比，范围 1-100 |
| `context_reserve_tokens` | `64000` | 主请求响应预留，同时限制主请求输出预算 |
| `context_compact_tail_tokens` | `256000` | 压缩后原样保留的热历史目标 |
| `context_compact_max_output_tokens` | `8192` | 摘要请求输出上限 |
| `context_compact_input_reduction` | `False` | 摘要前是否删除 thinking 并压缩工具噪声 |
| `max_search_files` | `5000` | Glob/Grep 最大遍历文件数 |
| `max_search_results` | `1000` | Grep 最大匹配结果行数 |
| `max_memory_mb` | `1024` | 内存限制（仅 nsjail cgroup） |
| `max_pids` | `64` | 进程数限制（仅 nsjail cgroup） |

### 调试

| 参数 | 默认值 | 说明 |
|------|--------|------|
| `verbose` | `False` | 启用详细日志输出 |
| `signal_mode` | `None`（实际默认 `full`） | 信号系统模式覆盖：`"full"` 启用信念跟踪、注入和恢复守卫；`"off"` 关闭信号提示词和运行时信号干预；`None` 继承 `MINK_SIGNAL_MODE` |
| `stream_events` | `True` | 是否让 Rust 侧输出过程事件；设为 `False` 时仅输出最终 `final`，适合非流式长任务 |

本地调试可以通过 `MINK_BINARY=/path/to/mink-core` 覆盖 SDK 使用的二进制。未设置时优先使用 wheel 内置二进制，然后查找 `PATH`。

默认发布的 SDK wheel 使用精简 `mink-core`，不包含 `PythonSandbox` 工具。需要该工具时可手动构建：

```bash
cargo build -p mink-cli --release --no-default-features --features "sdk-bin python-sandbox" --bin mink-core
MINK_BINARY=./target/release/mink-core python your_script.py
```

或构建带 `PythonSandbox` 的 wheel：

```bash
MINK_SDK_FEATURES="sdk-bin python-sandbox" python scripts/build_wheel.py
```

### 沙箱后端

| 参数 | 默认值 | 说明 |
|------|--------|------|
| `sandbox_backend` | `"auto"` | 传递给 Rust 内部 (`MINK_LIMITS`)：`"auto"` / `"nsjail"` / `"bwrap"` / `"sandbox-exec"` / `"off"` |

沙箱由 Rust ``mink-core`` 二进制内部处理。Python SDK 不构造任何沙箱命令。

### API 配置

| 参数 | 默认值 | 说明 |
|------|--------|------|
| `api_key` | `""` | DeepSeek API 密钥。也可通过 `DEEPSEEK_API_KEY` 环境变量设置 |
| `api_url` | `""` | DeepSeek API 地址。也可通过 `DEEPSEEK_BASE_URL` 环境变量设置 |
| `model` | `""` | 模型档位：`"flash"` / `"pro"`，也接受内部名 `"deepseek-v4-flash"` / `"deepseek-v4-pro"` |

## 沙箱行为说明

沙箱由 Rust ``mink-core`` 二进制内部通过 ``reexec_in_sandbox()`` 处理。

- **macOS**：使用 ``sandbox-exec``，写入限制 + 应用层读取限制。
- **Linux**：自动检测 ``nsjail`` → ``bubblewrap``，文件系统隔离 + 命名空间隔离。

详细策略见 Rust 代码 ``crates/mink-core/src/sandbox/``。

## 返回结果示例

```python
{
    "text": "已完成文件重构，改用 Result 类型。",
    "thinking": "查看代码后发现该函数返回...",
    "tool_calls": [
        {
            "name": "Read",
            "input": {"path": "src/handler.rs"},
            "type": "tool_call"
        },
        {
            "name": "Edit",
            "input": {
                "path": "src/handler.rs",
                "patch": "@src/handler.rs#A1B2\nreplace 42:\n+    Ok(value)"
            },
            "type": "tool_call"
        }
    ],
    "status": "ok",
    "session_id": "20260606-101500-abcd",
    "events_path": "/Users/me/.mink/sessions/20260606-101500-abcd/events.jsonl",
    "exit_code": 0,
    "error": None,
    "stderr": ""
}
```

## 常见问题

| 现象 | 原因 | 解决方法 |
|------|------|---------|
| `exit_code: 1` | agent 运行出错（查看 stderr） | 确认 `api_key` 已正确设置 |
| 返回 `text` 为空且无 `error` | API 密钥缺失 | 设置 `api_key` 或 `DEEPSEEK_API_KEY` 环境变量 |

## 支持平台

| 平台 | 架构 | 沙箱（由 Rust 内部处理） |
|------|------|------|
| macOS | arm64（Apple Silicon） | sandbox-exec |
| macOS | x86_64（Intel） | sandbox-exec |
| Linux | x86_64 | nsjail / bubblewrap |
| Linux | aarch64 | nsjail / bubblewrap |
