# dscode-sdk 使用文档

## 简介

dscode-sdk 是 [dscode](https://github.com/xialuyu/dscode) 的 Python 封装。`dscode` 二进制内置在 pip 包中，无需额外安装。

```bash
pip install dscode-sdk
```

安装时会自动选择匹配当前系统平台（macOS arm64/x86_64、Linux x86_64/aarch64）的 wheel 包。

## 快速开始

```python
from dscode_sdk import AgentSession, SandboxConfig

config = SandboxConfig(
    api_key="sk-...",                            # 或设置 DEEPSEEK_API_KEY 环境变量
    read_dirs=["/path/to/project/src"],           # agent 可读取的目录
    write_dirs=["/path/to/project/src"],          # agent 可写入的目录
)

session = AgentSession(config)
result = session.run("把 src/handler.rs 重构成使用 Result 类型")
print(result["text"])
session.close()
```

### 单次快捷调用

```python
from dscode_sdk import quick_run

result = quick_run(
    "解释这段代码",
    read_dirs=["/path/to/project"],
    api_key="sk-...",
)
print(result["text"])
```

## AgentSession API

### `run(prompt, *, extra_options=None) -> dict`

执行一个提示词并返回结果。

| 返回字段 | 类型 | 说明 |
|---------|------|------|
| `text` | `str` | agent 的文本回复 |
| `thinking` | `str` | agent 的推理过程（如有） |
| `tool_calls` | `list[dict]` | agent 执行的工具调用记录 |
| `exit_code` | `int` | 进程退出码（0 表示成功） |
| `error` | `str or None` | 错误信息（如有） |
| `stderr` | `str` | 进程原始错误输出（调试用） |

### `close()`

清理资源。会话目录保留在磁盘上，可供查阅。

## SandboxConfig 配置项

### 会话与路径

| 参数 | 默认值 | 说明 |
|------|--------|------|
| `dscode_home` | `~/.dscode/` | 会话存储目录。也可通过 `DSCODE_HOME` 环境变量设置 |
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
| `allow_bash` | `True` | 是否启用 Bash 工具 |
| `allow_python` | `True` | 是否启用 Python 工具 |
| `allow_network` | `True` | 是否启用 WebSearch/WebFetch 工具 |
| `allow_sub_agent` | `True` | 是否启用 SubAgent 工具 |
| `bash_allow_commands` | `[]` | 命令白名单（空 = 使用内置黑名单） |

### 资源限制

| 参数 | 默认值 | 说明 |
|------|--------|------|
| `timeout_secs` | `600` | agent 运行总超时（秒） |
| `tool_timeout` | `600` | 单次工具调用超时（秒） |
| `sub_agent_timeout` | `300` | 子代理执行超时（秒） |
| `max_tokens` | `81920` | 输出 token 上限 |
| `max_turns` | `40` | 最大循环轮数 |
| `max_memory_mb` | `1024` | 内存限制（仅 nsjail cgroup） |
| `max_pids` | `64` | 进程数限制（仅 nsjail cgroup） |

### 调试

| 参数 | 默认值 | 说明 |
|------|--------|------|
| `verbose` | `False` | 启用详细日志输出 |

### 沙箱后端

| 参数 | 默认值 | 说明 |
|------|--------|------|
| `sandbox_backend` | `"auto"` | 沙箱机制：`"auto"` / `"nsjail"` / `"bwrap"` / `"sandbox-exec"` / `"off"` |

- **macOS**：`"auto"` 使用 `sandbox-exec`（仅限制写入，读取不限制）。如需关闭沙箱，设 `sandbox_backend="off"`。
- **Linux**：`"auto"` 依次尝试 `nsjail` → `bubblewrap` → 无沙箱。

### API 配置

| 参数 | 默认值 | 说明 |
|------|--------|------|
| `api_key` | `""` | DeepSeek API 密钥。也可通过 `DEEPSEEK_API_KEY` 环境变量设置 |
| `api_url` | `""` | DeepSeek API 地址。也可通过 `DEEPSEEK_BASE_URL` 环境变量设置 |
| `model` | `""` | 模型名（如 `"deepseek-chat"`） |

## 沙箱行为说明

### macOS（sandbox-exec）

采用 `(allow default)` + 写入限制策略：

```
(version 1)
(allow default)
(deny file-write* (subpath "/"))
(allow file-write* (subpath "/path/to/write_dirs"))
(allow file-write* (subpath "/tmp"))
(allow file-write* (subpath "/private/tmp"))
(allow file-write* (subpath "~/.dscode"))
```

特点：
- 所有读取操作不受限
- 写入仅限 `write_dirs`、临时目录和会话目录
- 进程执行、网络、系统设施默认放行

### Linux（nsjail / bubblewrap）

agent 运行在容器中，仅挂载 `read_dirs` 和 `write_dirs`。`allow_network=False` 时切断网络。

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
            "input": {"path": "src/handler.rs", "old_string": "...", "new_string": "..."},
            "type": "tool_call"
        }
    ],
    "exit_code": 0,
    "error": None,
    "stderr": ""
}
```

## 常见问题

| 现象 | 原因 | 解决方法 |
|------|------|---------|
| `SandboxError: sandbox-exec not found` | macOS 缺少 sandbox-exec | 设 `sandbox_backend="off"` |
| `exit_code: 71` | sandbox-exec OS 错误 | 设 `sandbox_backend="off"`，或检查沙箱配置 |
| `exit_code: 1` | agent 运行出错（查看 stderr） | 确认 `api_key` 已正确设置 |
| 返回 `text` 为空且无 `error` | API 密钥缺失 | 设置 `api_key` 或 `DEEPSEEK_API_KEY` 环境变量 |

## 支持平台

| 平台 | 架构 | 沙箱 |
|------|------|------|
| macOS | arm64（Apple Silicon） | sandbox-exec（仅限写） |
| macOS | x86_64（Intel） | sandbox-exec（仅限写） |
| Linux | x86_64 | nsjail / bubblewrap |
| Linux | aarch64 | nsjail / bubblewrap |
