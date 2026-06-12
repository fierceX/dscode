# Changelog

## v0.1.8 (2026-06-12)

### Features

- **搜索参数可配置** — Glob/Grep 最大文件/结果数通过 `max_search_files`/`max_search_results` 参数控制
- **Agent 启动路径优化** — `--agent-jsonl` 跳过 `.minkrc` 文件 I/O；Mission 内容通过 stdin JSONL 直接传递，消除临时文件
- **按需裁剪** — `wasmtime`/`wasmtime-wasi` 通过 `python-sandbox` feature-gate 按需编译，`--no-default-features` 可缩小二进制体积
- **工具白名单 `enabled_tools`** — 按任务类型裁剪 system prompt 中暴露的工具列表

### SDK

- **SDK 二进制拆分** — Python SDK 打包 no-TUI `mink-core` 替代完整 `mink` 二进制，降低分发体积
- **SDK streaming 控制** — 新增 `stream_events`/`verbose` 参数，`AgentStreamEvent` 归一化事件协议，`raw_stream()` 公开为公共 API
- `max_search_files`/`max_search_results` 通过 `SandboxConfig` 暴露

### Refactor

- **工具过滤统一到配置层** — 合并 `filter_disabled_tools` + `filter_enabled_tools` 为 `ToolConfig::filter_tools_json` 单一路径
- **`TOOL_DISABLE_MAP` 从 `prefix.rs` 移到 `config.rs`**
- **PythonSandbox 重构** — CPython WASI 沙箱逻辑重构（`src/tools/sandbox_python.rs`）

### Config

- 新增 `max_search_files`（默认 5000）、`max_search_results`（默认 1000）配置项，支持环境变量覆盖
- `.minkrc.example` 重构，分组对齐 Python SDK 配置风格

### Fixes

- **PythonSandbox** — 修复路径权限、`os.chdir` 注入、WASI 文件系统隔离等问题
- **SDK** — 修复 wheel 构建中二进制路径和包名不一致问题

## v0.1.7 (2026-06-09)

### Features

- **TUI 文件选择器** — 新增 Tab 路径补全、父目录入口和沙箱感知过滤 (`src/tui/file_picker.rs`)
- **TUI 任务完成通知** — 新增任务完成/失败通知链路，兼容 macOS 系统通知，接入用户输入与 compact 流程

### Refactor

- **精简 CLI 参数** — 移除 11 个中低频 CLI 参数，改为 `--config <toml>` 统一传递
  - 涉及参数：max-tokens、max-turns、max-context、tool-timeout 等
  - 对齐 Python SDK 的配置构建方式，统一走 TOML 通道

### CI & Build

- **新增 FreeBSD CI 构建目标** (`x86_64-unknown-freebsd`)
- **修复 FreeBSD CI** 包名和 release 版本

### Tests

- **测试加速** — 标记 25 个 PythonSandbox 重型测试为 `slow-tests` feature gate
  - 日常 `cargo test` 从 ~120 秒降至 ~5 秒
  - CI 环境通过 `--features slow-tests -- --include-ignored` 全量覆盖

## v0.1.6 (2026-06-09)

### Features

- **PythonSandbox 工具** — 新增基于 wasmtime + CPython WASI 的沙箱 Python 执行环境
  - WASI 级进程隔离：无子进程、无网络、无 C 扩展
  - 完整 CPython 标准库（json/csv/re/math/datetime/xml 等）
  - 通过 `--enable-python-sandbox` CLI 参数或 `.minkrc` 的 `[sandbox_python]` 段启用
  - 默认禁用，避免与宿主 Python 工具混用
  - 支持相对路径和绝对路径（自动注入 `os.chdir`）
  - 通过 `read_dirs` / `write_dirs` 精细化控制文件访问权限
  - 25 个边界测试覆盖

- **hashline patch 解析器** — 用 Tokenizer + Executor 两阶段状态机替换旧手写解析器
  - 修复 markdown 表格行（`|`）导致 Edit 崩溃的问题
  - 非 `+` 前缀 body 行接受并给出警告，而非直接报错
  - 空白行在 hunk body 中正确跳过，不终止收集

### Refactor

- **移除 Python 工具字符串过滤** — 删除 `BLOCKED_PATTERNS`
  - 安全策略不再由工具层承担，交给 OS 进程沙箱处理
  - 宿主 Python 现拥有完整生态访问能力（网络、子进程、C 扩展）
- **Edit 工具内部重构** — 用 hashline 解析器替换旧 `parse_anchored_patch`
- **系统提示词结构优化** — 简化 Read 工具 schema 描述
- **Skill 系统重构** — 统一 skill 发现与读取协议

### Config

- 新增 `[sandbox_python]` 配置段（CPython WASI 沙箱路径和权限）
- 新增 `--enable-python-sandbox` CLI 参数
- 新增 `.minkrc.example` 完整配置示例
- 更新 `.minkrc` 统一为 mink 格式

### SDK & Protocol

- Agent JSONL 协议优化，支持 single-shot 模式
- SubAgent 调用协议优化
- Session 命名与 Read 资源协议优化

### Fixes

- Agent 停止与超时边界问题
- 工具执行稳定性问题
- Web 工具替换为 DuckDuckGo 实现（无需 API Key）

### Tests

- 470 测试通过（0 failed）
- 新增 25 个 PythonSandbox 边界测试（路径权限、读写隔离、路径穿越、Unicode 路径、stdout/stderr 捕获等）

### Dependencies

- 新增 `wasmtime` 28、`wasmtime-wasi` 28

## v0.1.5 (2026-04-22)

### Features

- **工具协议与资源读取优化** — Session 命名、Read 资源协议走 URL 模式
- 离线的 TUI 操作模式
- TUI 支持 UTF-8 光标和软换行
- `print` 模式输出 ndjson 事件流

### CI & Build

- 完善的 Linux wheel 构建流水线
  - manylinux_2_35 glibc 原生 wheel
  - musllinux_1_2 静态编译 wheel
  - Apple Silicon 原生 wheel
  - 修复 musl 构建 segfault（cargo-zigbuild 替代 musl-gcc）
  - 正确设置 wheel tag（PEP 656）
- Ubuntu 22.04 迁移（glibc 2.35 兼容性）

### Fixes

- **沙箱 work_dir 写入权限修复** — sandbox 内 work_dir 写入权限问题
- **bwrap 缺少 --chdir** — 修复沙箱内 cwd 不正确问题
- TUI 退出流程修复（Ctrl+C 行为）
- 会话恢复逻辑修复

### Config

- 新增 `.minkrc` 配置文件和 `[sandbox]` 配置段
- 新增 `--disable-bash`、`--disable-python`、`--disable-sub-agent`、`--disable-web` CLI 参数
- 沙箱后端支持：nsjail（Linux）、bubblewrap（Linux）、sandbox-exec（macOS）
- 沙箱自举 re-exec 到 sandbox-exec / nsjail / bwrap



## v0.1.2 (2026-03-15)

### Features

- **信号驱动的信念系统** — 自动检测工具执行错误（ToolFailed/ToolError/EditLoop）
  - 滑动窗口信念度计算（拉普拉斯平滑）
  - 低信念注入修正提示 + 恢复首步守卫
  - 低于阈值自动中止执行
  - 可通过 `MINK_SIGNAL_MODE=off` 关闭
- **自适应上下文压缩** — 三级 Tier 压缩，自动摘要，保持上下文在窗口内
- **维修流水线** — Scavenge 回收遗漏工具调用 → Truncation 修复 → StormBreaker 重复调用抑制
- **Session 持久化** — JSONL 格式，`--continue` 无缝恢复
- **SubAgent 子代理** — 隔离或 fork 上下文，并发执行
- **Skill 系统** — 按需加载 skill 文件，不污染后续 prompt
- **自定义提示词** — `--mission` 加载 MISSION.md 文件

### Tools

- Read / Write / Edit（anchored patch）
- Bash / Python（受限）
- Glob / Grep / WebSearch / WebFetch
- TodoWrite / PlanConfirm / PlanClear
- 工具元数据与审批策略（ApprovalTier / ToolResultKind）
- Artifact 持久化（超长工具输出落盘）

### TUI

- 基于 ratatui 的全屏界面
- 消息列表 + 输入区 + 状态栏
- Markdown 子集渲染
- 工具结果折叠 / 子代理详情 / 鼠标点击

### Fixes

- **sandbox reexec 移到 JSON-RPC 解析之前** — 修复 session_id 丢失问题
- **PyPI 发布修复** — 分离 wheel 目录与二进制目录
- 修复 CI Python 包发布流程

## v0.1.1 (2026-03-01)

### Features

- **REPL 模式**（rustyline 行编辑 + TerminalDisplay 同步渲染）
- DeepSeek V4 流式请求（SSE → Event → ToolCall）
- 工具调用执行循环（LLM → Tool → Decision）
- Artifact 持久化（session artifacts 读写）
- CLI 参数解析 + 配置合并（.minkrc / 环境变量 / CLI）
- 危险命令过滤（safety.rs）

### SDK

- Python pip 包发布流水线（GitHub Actions CI）
- 跨平台构建支持（Linux x86 + musl + ARM，macOS ARM）
- Python SDK 基础接口

### TUI

- 初始 TUI 原型
- 多行输入 / Ctrl+C 中断
- Slash command（`/flash`、`/pro`、`/compact`、`/help`、`/exit`）

### Fixes

- 修复自适应超时测试竞态条件
- 修复 ARM64 交叉编译 CI

## v0.1.0 (2026-02-15)

### Features

- 初始发布
- 基本 CLI 参数解析 + 配置加载
- LLM 流式请求基础框架
- Read 工具（本地文件 + URL）
- Bash 工具执行
- Session 管理基础
