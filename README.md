# mink

**简体中文**

极简 AI coding agent。Rust 原生实现，专为 DeepSeek 优化。

单二进制，零运行时依赖，可在终端独立运行或被其他程序嵌入编排。

---

## 特性

- **DeepSeek 原生优化** — 针对 DeepSeek V4 系列设计，最大化 prefix-cache 命中率
- **两种终端模式** — REPL（`-i`，rustyline 行编辑）和 TUI（`--tui`，ratatui 全屏界面）
- **信号驱动的信念系统** — 自动检测工具执行错误，低信念时注入修正提示并约束恢复首步；可用 `MINK_SIGNAL_MODE=off` 关闭
- **自适应上下文压缩** — 三级压缩，自动摘要，保持上下文在窗口内
- **维修流水线** — Scavenge → Truncation → Storm Breaker，三段自动修复
- **Session 持久化** — JSONL 格式，`--continue` 无缝恢复
- **子代理（SubAgent）** — 隔离或 fork 上下文，并发执行
- **技能系统** — 按需加载 skill 文件，不污染后续 prompt
- **自定义提示词** — `--mission` 加载 MISSION.md 文件，替换默认系统提示词，自由定义 agent 目标和行为
- **Python SDK** — `mink-agent` pip 包，内置无 TUI 的 `mink-core` 二进制，支持沙箱控制和全参数配置
- **沙箱防护** — Linux nsjail/bubblewrap（完整文件系统隔离）、macOS sandbox-exec（写入隔离）
- **CPython WASI 沙箱** — `PythonSandbox` 工具，在 wasmtime + CPython WASI 中执行 Python 代码，WASI 级进程隔离
- **运行时约束** — `--disable-bash` / `--disable-sub-agent` / `--disable-web` / `--disable-python` 按场景禁用工具；`--enable-python-sandbox` 启用沙箱 Python 工具
- **机器协议** — `--print` 输出 ndjson 事件流并以 `final` 结束；`--agent-jsonl` 提供 single-shot Agent JSONL 协议

---

## 快速开始

```bash
# 前置：Rust 1.85+，设置 DEEPSEEK_API_KEY

# 编译
cargo build --release
# 或
make build

# REPL 交互模式
./target/release/mink -m flash -i

# TUI 全屏模式
./target/release/mink -m flash --tui

# 单次查询
./target/release/mink -m flash "explain this project"

# 继续上次会话
./target/release/mink -m flash --continue -i

# 使用自定义系统提示词
./target/release/mink --mission ./my-task.mission.md -i

```

---

## Python SDK

通过 pip 安装使用：

```bash
pip install mink-agent
```

```python
from mink_agent import AgentSession, SandboxConfig

session = AgentSession(SandboxConfig(
    api_key="sk-...",
    read_dirs=["src"],
    write_dirs=["src"],
    mission_file="./my-task.mission.md",
    signal_mode="full",
))
result = session.run("处理文档")
print(result["status"], result["events_path"])
```

详见 [mink_agent/README.md](mink_agent/README.md)。

---

## 文档索引

| 文档 | 说明 |
|------|------|
| [使用手册](docs/USAGE.md) | 完整 CLI 参数、配置、环境变量、会话管理、工具、技能 |
| [架构说明](docs/ARCHITECTURE.md) | 运行时分层、模块职责、核心数据流 |
| [设计文档](docs/DESIGN.md) | 14 个主题的设计哲学与实现取舍 |
| [信号系统设计](docs/设计哲学-信号系统.md) | 控制论 + 贝叶斯、冷却机制、信念度展示 |
| [Agent 开发指南](AGENTS.md) | 面向 AI agent：项目结构、模块索引、开发惯例 |
| [工具参考](docs/tools.md) | 内置工具参数与行为 |

---

## 许可

MIT
