//! Shared event classification for REPL and TUI session replay.
//!
//! Both replay surfaces read persisted `events.jsonl` and project its raw
//! JSON events into their own rendering pipeline. The event vocabulary and
//! tool-summary extraction live here so the two implementations cannot drift.

use serde_json::Value;

pub(crate) const REPLAY_TURNS: usize = 10;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ReplayEventKind {
    UserInput,
    Thinking,
    Text,
    ToolCall,
    ToolResult,
    Error,
    AssistantMessage,
    PrefixSnapshot,
    Ignored,
    Unknown,
}

pub(crate) fn classify_event(evt: &Value) -> ReplayEventKind {
    match event_type(evt) {
        "user_input" | "user_message" => ReplayEventKind::UserInput,
        "thinking" => ReplayEventKind::Thinking,
        "text" => ReplayEventKind::Text,
        "tool_call" => ReplayEventKind::ToolCall,
        "tool_result" => ReplayEventKind::ToolResult,
        "error" => ReplayEventKind::Error,
        "assistant_message" => ReplayEventKind::AssistantMessage,
        "prefix_snapshot" => ReplayEventKind::PrefixSnapshot,
        "session_start" | "usage" | "stop" | "retry" => ReplayEventKind::Ignored,
        _ => ReplayEventKind::Unknown,
    }
}

pub(crate) fn event_type(evt: &Value) -> &str {
    evt.get("type").and_then(Value::as_str).unwrap_or("")
}

pub(crate) fn event_content(evt: &Value) -> &str {
    evt.get("content").and_then(Value::as_str).unwrap_or("")
}

pub(crate) fn event_name(evt: &Value) -> &str {
    evt.get("name").and_then(Value::as_str).unwrap_or("")
}

pub(crate) fn event_message(evt: &Value) -> &str {
    evt.get("message").and_then(Value::as_str).unwrap_or("")
}

pub(crate) fn build_tool_summary(name: &str, evt: &Value) -> String {
    crate::session::store::build_tool_summary_from_json(name, evt)
}
