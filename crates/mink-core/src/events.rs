use crate::protocol::UsageEvent;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum EventLog {
    UserInput {
        version: u8,
        content: String,
    },
    Thinking {
        version: u8,
        content: String,
    },
    Text {
        version: u8,
        content: String,
    },
    ToolCall {
        version: u8,
        name: String,
        id: String,
        input: Value,
    },
    ToolResult {
        version: u8,
        tool_use_id: String,
        name: String,
        content: String,
    },
    Usage {
        version: u8,
        input_tokens: i64,
        output_tokens: i64,
        cache_read_input_tokens: i64,
        cache_creation_input_tokens: i64,
        kind: String,
    },
    Signal {
        version: u8,
        signal_kind: String,
        severity: f64,
        source_tool: String,
        exit_code: Option<i32>,
        matched_pattern: Option<String>,
        message: String,
    },
    Compact {
        version: u8,
        trigger: String,
        result: String,
    },
    TurnTracking {
        version: u8,
        tool_call_count: u32,
        tool_error_count: u32,
        belief: f64,
        model: String,
    },
    /// 前缀构建/失效重建时的完整快照：使任意请求的 system prompt 与 tools
    /// 可离线重建（"模型可见 = 日志可重建"）。仅在构建时写，不逐请求写。
    PrefixSnapshot {
        version: u8,
        fingerprint: String,
        dependency_fingerprint: String,
        system_prompt: String,
        tools_json: Vec<Value>,
    },
    Error {
        version: u8,
        message: String,
    },
}

impl EventLog {
    pub fn usage(version: u8, usage: &UsageEvent, kind: &str) -> Self {
        Self::Usage {
            version,
            input_tokens: usage.input_tokens,
            output_tokens: usage.output_tokens,
            cache_read_input_tokens: usage.cache_read_input_tokens,
            cache_creation_input_tokens: usage.cache_creation_input_tokens,
            kind: kind.to_string(),
        }
    }
}

#[cfg(test)]
#[path = "events_tests.rs"]
mod tests;
