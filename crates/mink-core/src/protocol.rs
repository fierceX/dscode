use serde_json::Value;
use std::collections::BTreeMap;

#[derive(Debug, Clone)]
pub enum Event {
    Text(TextEvent),
    Thinking(ThinkingEvent),
    ToolCall(ToolCallEvent),
    Usage(UsageEvent),
    UsageUnavailable,
    Stop(StopEvent),
    Error(ErrorEvent),
    Retry(RetryEvent),
}

#[derive(Debug, Clone)]
pub struct TextEvent {
    pub content: String,
}

#[derive(Debug, Clone)]
pub struct ThinkingEvent {
    pub content: String,
}

#[derive(Debug, Clone)]
pub struct StopEvent {
    pub reason: String,
}

#[derive(Debug, Clone)]
pub struct ErrorEvent {
    pub message: String,
}

#[derive(Debug, Clone)]
pub struct RetryEvent {}

#[derive(Debug, Clone)]
pub struct UsageEvent {
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cache_read_input_tokens: i64,
    pub cache_creation_input_tokens: i64,
}

#[derive(Debug, Clone)]
pub struct ToolCallEvent {
    pub name: String,
    pub id: String,
    pub input_json: Value,
    pub fields: BTreeMap<String, String>,
    /// Model-generated tool arguments that could not be parsed. The SSE layer
    /// fills this instead of failing the turn; the runner converts the call
    /// into a failed tool result so the model can retry.
    pub parse_error: Option<String>,
}

#[cfg(test)]
#[path = "protocol_tests.rs"]
mod tests;
