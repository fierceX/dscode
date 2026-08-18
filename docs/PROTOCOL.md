# 机器协议

> 更新日期：2026-08-18

本文面向机器消费方：通过 `--print`（stream-json）或 `--agent-jsonl`（Agent JSONL
single-shot 协议）与 Mink 集成。终端交互与配置见 [使用手册](USAGE.md)；Rust/Python
嵌入见 [嵌入与 SDK 使用](EMBEDDING.md)。

---

[TOC]

---

## Stream-JSON（`--print`）

```bash
mink -m flash --print "explain this"
```

每行一个 JSON 事件：

```json
{"type":"thinking","content":"Let me analyze..."}
{"type":"text","content":"Here is the explanation..."}
{"type":"tool_call","name":"Read","id":"...","input":{"path":"/x"}}
{"type":"tool_result","tool_use_id":"...","name":"Read","content":"...","status":{"state":"succeeded"},"result_kind":"file_read","artifacts":[]}
{"type":"usage","input_tokens":100,"output_tokens":50,"cache_read_input_tokens":20,"cache_creation_input_tokens":0,"kind":"agent"}
{"type":"stop","reason":"end_turn"}
```

事件流以 `final` 事件结束，`final` 携带用量信息：

```bash
mink -m flash --print "hello" | jq 'select(.type=="final") | {billing_turn_id, usage}'
```

JQ 下游处理：

```bash
mink -m flash --print "fix the bug" | jq 'select(.type=="text") | .content'
```

---

## Agent JSONL（`--agent-jsonl`）

SDK 专用 single-shot 协议：stdin 读入一个 versioned JSON request，stdout 输出事件流，
最后以 `final` 结束。协议版本为 **3**；v3 的 `options` 各分组对未知字段直接拒绝
（`deny_unknown_fields`），不兼容 v2 的扁平 options。

```bash
# 最小请求
echo '{"version":3,"prompt":"scan this repo"}' | mink-core --agent-jsonl
```

### Request

| 字段 | 类型 | 说明 |
|------|------|------|
| `version` | `u32` | 协议版本（当前 `3`） |
| `prompt` | `string` | 用户输入（**必需**） |
| `session_id` | `string?` | session 引用（alias / id / 前缀 / title） |
| `mission` | `string?` | MISSION.md 内联内容（避免临时文件 I/O） |
| `options` | object | 见下 |

### Grouped options

| 分组 | 主要字段 | 说明 |
|------|------|------|
| `provider` | `model` | 模型名（别名或真实名） |
| `generation` | `max_tokens`, `max_turns`, `llm_*_timeout` | 生成和流超时 |
| `context` | `max_context`, `context_compact_*`, `context_reserve_tokens` | 上下文与压缩；`max_context=0` 禁用自动压缩 |
| `tools` | `enabled_tools`, `tool_timeout`, `tool_timeout_max`, `sub_agent_timeout`, edit 和 skill 字段 | 工具 surface 与执行策略 |
| `session` | `session_id`, `session_layout` | session 引用与布局 |
| `output` | `verbose`, `stream_events` | 输出策略 |
| `signal` | `policy` | `off` / `evidence` / `state_ops` / `restart` / `full` |

示例：

```json
{
  "version": 3,
  "prompt": "scan this repo and summarize",
  "session_id": "work-001",
  "options": {
    "provider": {"model": "flash"},
    "generation": {"max_tokens": 8192, "max_turns": 20},
    "context": {
      "max_context": 64000,
      "context_compact_pct": 65,
      "context_reserve_tokens": 12000
    },
    "tools": {
      "tool_timeout": 300,
      "tool_timeout_max": 600,
      "edit_mode": "hashline",
      "edit_enforce_seen_lines": false,
      "enabled_tools": ["Read", "Write", "Edit", "Grep", "Glob", "Bash"]
    },
    "output": {"stream_events": false}
  }
}
```

### Events

过程事件（`stream_events=true` 时输出）：`thinking` / `text` / `tool_call` /
`tool_result` / `usage` / `stop` 等，格式与 [Stream-JSON](#stream-jsonprint) 一致。

### Final

```json
{
  "type": "final",
  "version": 3,
  "status": "ok",
  "billing_turn_id": "turn-...",
  "session_id": "session-...",
  "session_ref": "work-001",
  "home": "/app/mink-home",
  "cwd": "/app/work",
  "events_path": ".../events.jsonl",
  "conversation_path": ".../conversation.jsonl",
  "artifacts_dir": ".../artifacts",
  "summary_path": ".../summary.txt",
  "usage_path": ".../usage.jsonl",
  "tool_call_count": 3,
  "tool_error_count": 0,
  "error": null,
  "usage_records": [],
  "usage": {"request_count": 1, "tokens": {...}, "cost": {"known_nano_cny": 140800, "unpriced_requests": 0}}
}
```

`status` 取值：`ok` / `failed` / `interrupted` / `max_turns_exceeded`。
`request.options.stream_events=false` 时只输出此 `final`；SDK 侧从
`conversation.jsonl` 回读最后一条 assistant 消息补齐 `text` / `thinking`。

`--agent-jsonl` 模式不会读取用户级/项目级 `.minkrc`，但仍应用同一命令行传入的
`--config <toml>`，避免 SDK 调用产生额外文件 I/O。

---

## 相关文档

- 终端用户手册：[USAGE.md](USAGE.md)
- 嵌入与 SDK 使用：[EMBEDDING.md](EMBEDDING.md)
- Python SDK 协议适配：[mink_agent/README.md](../mink_agent/README.md)
