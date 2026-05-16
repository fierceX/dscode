# dscode

**简体中文 | [English](README.en.md)**

极简 AI coding agent。Rust 原生实现，专为 DeepSeek 优化。

## 特点

- **DeepSeek 原生优化** — 单二进制，零运行时依赖
- **缓存感知压缩** — 基于三级阈值的自适应上下文管理，最大化 prefix-cache 命中率
- **维修流水线** — Scavenge（DSML/JSON 回收）→ Truncation（截断修复）→ Storm Breaker（重复调用抑制）
- **Session 持久化** — JSONL 格式，天然追加友好，崩溃安全
- **机器友好** — `stream-json` 输出结构化事件
- **技能系统** — 按需加载 skill，不污染后续 prompt
- **子代理（SubAgent）** — 隔离或继承上下文的并行子任务执行

## 快速开始

```bash
# 编译
cargo build --release
./target/release/dscode "scan this repo"

# 或使用 Makefile
make build
./target/release/dscode -i               # 交互模式
./target/release/dscode --print "hello"  # stream-json 输出
```

```bash
# 设置 API Key
export DEEPSEEK_API_KEY="sk-xxx"

# 交互模式
./target/release/dscode -m deepseek-chat -i
```

## 安装

```bash
# 编译
make build

# 运行
./target/release/dscode -m deepseek-chat "hello"

# 别名
alias agent='./target/release/dscode -m deepseek-chat'
agent -i
```

## 使用示例

```bash
# 单次查询
dscode -m deepseek-v4-flash "explain the architecture"

# 交互式 REPL
dscode -m deepseek-v4-flash -i

# 回到上次会话继续
dscode --continue -i

# 指定上下文窗口和轮次上限
dscode -m deepseek-v4-flash --max-context 1M --max-turns 1000 -i

# 加载技能
dscode -m deepseek-v4-flash --skill debugging -i
```

## 文档

| 文档 | 说明 |
|------|------|
| [使用手册](docs/USAGE.md) | CLI 参数、环境变量、会话管理、工具参考 |
| [架构说明](docs/ARCHITECTURE.md) | 运行时分层、模块职责、数据流 |
| [设计文档](docs/DESIGN.md) | 设计哲学、关键决策、实现取舍 |
| [工具参考](docs/tools.md) | 内置工具参数与行为 |

## 许可

MIT
