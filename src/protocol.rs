use serde_json::Value;
use std::collections::BTreeMap;

#[derive(Debug, Clone)]
pub enum Event {
    Text(TextEvent),
    Thinking(ThinkingEvent),
    ToolCall(ToolCallEvent),
    Usage(UsageEvent),
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
    pub order: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn make_usage(input: i64, output: i64, cache_read: i64, cache_creation: i64) -> UsageEvent {
        UsageEvent { input_tokens: input, output_tokens: output, cache_read_input_tokens: cache_read, cache_creation_input_tokens: cache_creation }
    }

    #[test]
    fn text_event_fields() {
        let e = TextEvent { content: "hello".into() };
        assert_eq!(e.content, "hello");
    }

    #[test]
    fn thinking_event_fields() {
        let e = ThinkingEvent { content: "thinking text".into() };
        assert_eq!(e.content, "thinking text");
    }

    #[test]
    fn usage_event_fields() {
        let u = make_usage(100, 50, 20, 0);
        assert_eq!(u.input_tokens, 100);
        assert_eq!(u.output_tokens, 50);
        assert_eq!(u.cache_read_input_tokens, 20);
    }

    #[test]
    fn stop_event_reason_field() {
        let e = StopEvent { reason: "end_turn".into() };
        assert_eq!(e.reason, "end_turn");
    }

    #[test]
    fn error_event_message_field() {
        let e = ErrorEvent { message: "rate limit".into() };
        assert_eq!(e.message, "rate limit");
    }

    #[test]
    fn tool_call_event_roundtrip_fields() {
        let input = json!({"path": "/tmp/file.txt"});
        let e = ToolCallEvent {
            name: "Read".into(),
            id: "call_1".into(),
            input_json: input.clone(),
            fields: [("path".to_string(), "/tmp/file.txt".to_string())].into(),
            order: vec!["path".to_string()],
        };
        assert_eq!(e.name, "Read");
        assert_eq!(e.id, "call_1");
        assert_eq!(e.input_json, input);
        assert_eq!(e.fields.get("path").unwrap(), "/tmp/file.txt");
    }

    #[test]
    fn usage_default_values_are_zero() {
        let u = UsageEvent { input_tokens: 0, output_tokens: 0, cache_read_input_tokens: 0, cache_creation_input_tokens: 0 };
        assert_eq!(u.input_tokens, 0);
        assert_eq!(u.cache_creation_input_tokens, 0);
    }
}
