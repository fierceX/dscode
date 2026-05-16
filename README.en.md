# dscode

[中文](README.md)

A minimal AI coding agent runtime in Rust. Single binary, optimized for DeepSeek.

## Features

- **DeepSeek-native** — Single binary, no runtime dependencies
- **Cache-Aware Compression** — Adaptive 3-tier threshold compaction maximizing prefix-cache hit rate
- **Repair Pipeline** — Scavenge (DSML/JSON recovery) → Truncation → Storm Breaker (repeat suppression)
- **Session Persistence** — JSONL format, append-friendly, crash-safe
- **Machine-Friendly** — `stream-json` structured event output
- **Skill System** — On-demand skill loading
- **SubAgent** — Isolated or fork-mode parallel sub-tasks

## Quick Start

```bash
# Build
cargo build --release

# Run
export DEEPSEEK_API_KEY="sk-xxx"
./target/release/dscode -m deepseek-chat "scan this project"
./target/release/dscode -m deepseek-chat -i   # interactive mode
```

## Documentation

| Document | Description |
|----------|-------------|
| [Usage](docs/USAGE.md) | CLI flags, env vars, session management, tools |
| [Architecture](docs/ARCHITECTURE.md) | Runtime layers, module responsibilities, data flow |
| [Design](docs/DESIGN.md) | Design philosophy, key decisions, trade-offs |
| [Tools](docs/tools.md) | Built-in tool parameters and behavior |

## License

MIT
