# 机器协议

> 更新日期：2026-07-30

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
{"type":"tool_result","tool_use_id":"...","name":"Read","content":"..."}
{"type":"usage","input_tokens":100,"output_tokens":50}
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
最后以 `final` 结束。协议版本为 **2**；v2 对未知字段直接拒绝（`deny_unknown_fields`），
不提供 v1 双格式兼容。

```bash
# 最小请求
echo '{"version":2,"prompt":"scan this repo"}' | mink-core --agent-jsonl
```

### Request

| 字段 | 类型 | 说明 |
|------|------|------|
| `version` | `u32` | 协议版本（当前 `2`） |
| `prompt` | `string` | 用户输入（**必需**） |
| `session_id` | `string?` | session 引用（alias / id / 前缀 / title） |
| `mission` | `string?` | MISSION.md 内联内容（避免临时文件 I/O） |
| `options` | object | 见下 |

### Options

| 字段 | 类型 | 说明 |
|------|------|------|
| `model` | `string?` | 模型名（别名或真实名） |
| `max_tokens` / `max_turns` | `int?` | 输出上限与最大轮次 |
| `max_context` | `int?` | 上下文窗口；`0` 禁用自动压缩 |
| `context_compact_pct` | `int?` | 压缩百分比（1-100） |
| `context_reserve_tokens` | `int?` | 响应预留 |
| `context_compact_tail_tokens` | `int?` | 压缩后热尾部 |
| `context_compact_max_output_tokens` | `int?` | 摘要输出上限 |
| `context_compact_input_reduction` | `bool?` | 摘要输入降噪 |
| `tool_timeout` / `sub_agent_timeout` | `int?` | 工具/子代理超时（秒） |
| `llm_first_event_timeout` / `llm_idle_timeout` / `llm_wait_heartbeat` | `int?` | LLM 流超时 |
| `verbose` | `bool?` | 详细日志 |
| `stream_events` | `bool?` | `false` 时只输出最终 `final`，不输出过程事件 |
| `enabled_tools` | `string[]?` | 精确工具选择（唯一工具入口） |
| `skills` | `string[]?` | 选中技能 |
| `inline_skills` | `object[]?` | 内联技能（`name`/`description`/`content`/`exposure`/`revision`） |
| `skill_discovery_policy` | `string?` | `defaults` / `runtime_only` / `explicit_only` |
| `session_id` | `string?` | 覆盖外层 session_id |
| `session_layout` | `string?` | `project` / `home` / `direct` / `isolated` |

示例：

```json
{
  "version": 2,
  "prompt": "scan this repo and summarize",
  "session_id": "work-001",
  "options": {
    "model": "flash",
    "max_tokens": 8192,
    "max_turns": 20,
    "max_context": 64000,
    "context_compact_pct": 65,
    "context_reserve_tokens": 12000,
    "enabled_tools": ["Read", "Write", "Edit", "Grep", "Glob", "Bash"],
    "stream_events": false
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
  "version": 2,
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
  "usage": {"request_count": 1, "tokens": {...}, "cost_nano_cny": 140800}
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
