# mink-cli

> 更新日期：2026-08-18

`mink-cli` 是 workspace 内部二进制包，不发布到 crates.io。它依赖
[`mink-core`](../mink-core/README.md)，并提供两个二进制入口：

| 二进制 | 用途 |
|--------|------|
| `mink` | 面向终端用户的完整 CLI，支持单次 prompt、REPL、TUI、stream-json 和 session 操作 |
| `mink-core` | 面向 Python SDK / 机器调用的精简二进制，主要用于 `--agent-jsonl` single-shot 协议 |

## 包内容

- `src/cli.rs`：`mink` / `mink-core` 共用 CLI adapter，负责参数解析、配置合并、sandbox re-exec、模式分发和 `--prefab[=TEMPLATE]` 启用。
- `src/ui`：REPL / 普通终端输出实现。
- `src/tui`：ratatui Full / Inline 双 TUI surface，共享结构化 transcript、工具卡片、
  Markdown、输入、详情、通知和 session replay。
- `src/main.rs`：`mink` thin wrapper。
- `src/bin/mink-core.rs`：SDK 精简二进制 thin wrapper。

## 构建

```bash
# 完整终端二进制，包含 REPL/TUI/PythonSandbox
cargo build -p mink-cli --release

# 显式启用 prefab 重组能力（full-cli 默认已包含）
cargo build -p mink-cli --release --features "prefab"

# 无默认 feature 的最小 mink 二进制
cargo build -p mink-cli --release --no-default-features --bin mink

# SDK 精简二进制 mink-core，不包含 TUI/REPL/PythonSandbox
cargo build -p mink-cli --release --no-default-features --features sdk-bin --bin mink-core

# SDK 精简二进制，手动加入 PythonSandbox
cargo build -p mink-cli --release --no-default-features --features "sdk-bin python-sandbox" --bin mink-core
```

## Feature

| Feature | 说明 |
|---------|------|
| `full-cli` | 默认 feature，等价于完整终端能力：CLI + REPL + TUI + PythonSandbox + Prefab |
| `cli` | 普通 CLI / REPL 基础能力 |
| `repl` | rustyline 交互输入 |
| `tui` | ratatui Full / Inline 双 TUI 界面 |
| `sdk-bin` | 构建 `mink-core` SDK 二进制所需的最小 runtime |
| `python-sandbox` | 透传启用 `mink-core/python-sandbox` |
| `prefab` | 透传启用 `mink-core/prefab`，提供 `--prefab[=TEMPLATE]` 会话重组能力（TEMPLATE 为内置名或模板目录） |
| `router` | 启用 `mink-router`，提供 `--router[=flash]` Flash 推理模式路由 |

## 验证

```bash
make feature-matrix
cargo test -p mink-cli --all-features
```

更多用户说明见根项目 [README](../../README.md) 和 [docs/USAGE.md](../../docs/USAGE.md)。
