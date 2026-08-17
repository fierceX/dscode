use super::*;

#[test]
fn typed_event_serializes_with_version_and_legacy_type_name() {
    let event = EventLog::UserInput {
        version: Some(1),
        content: "hello".into(),
    };
    let value = serde_json::to_value(event).unwrap();
    assert_eq!(value["type"], "user_input");
    assert_eq!(value["version"], 1);
    assert_eq!(value["content"], "hello");
}

#[test]
fn prefix_snapshot_serializes_with_legacy_type_name() {
    let event = EventLog::PrefixSnapshot {
        version: Some(1),
        fingerprint: "fp".into(),
        dependency_fingerprint: "dep".into(),
        system_prompt: "prompt".into(),
        tools_json: vec![serde_json::json!({"name": "Bash"})],
    };
    let value = serde_json::to_value(event).unwrap();
    assert_eq!(value["type"], "prefix_snapshot");
    assert_eq!(value["version"], 1);
    assert_eq!(value["fingerprint"], "fp");
    assert_eq!(value["dependency_fingerprint"], "dep");
    assert_eq!(value["system_prompt"], "prompt");
    assert_eq!(value["tools_json"][0]["name"], "Bash");
}

#[test]
fn typed_events_match_legacy_jsonl_shapes() {
    use crate::tools::metadata::{ToolResultKind, ToolStatus};
    use serde_json::json;

    let cases: Vec<(EventLog, serde_json::Value)> = vec![
        (
            EventLog::UserInput {
                version: None,
                content: "u".into(),
            },
            json!({"type":"user_input","content":"u"}),
        ),
        (
            EventLog::Thinking {
                version: None,
                content: "t".into(),
            },
            json!({"type":"thinking","content":"t"}),
        ),
        (
            EventLog::ToolCall {
                version: None,
                name: "Read".into(),
                id: "c".into(),
                input: json!({"path":"a"}),
            },
            json!({"type":"tool_call","name":"Read","id":"c","input":{"path":"a"}}),
        ),
        (
            EventLog::Usage {
                version: None,
                input_tokens: 10,
                output_tokens: 2,
                cache_read_input_tokens: 3,
                cache_creation_input_tokens: 4,
                context_tokens: Some(5),
                max_context: Some(6),
                kind: "agent".into(),
            },
            json!({
                "type":"usage",
                "input_tokens":10,
                "output_tokens":2,
                "cache_read_input_tokens":3,
                "cache_creation_input_tokens":4,
                "context_tokens":5,
                "max_context":6,
                "kind":"agent"
            }),
        ),
        (
            EventLog::ToolResult {
                version: Some(2),
                tool_use_id: "c".into(),
                name: "Read".into(),
                content: "ok".into(),
                status: ToolStatus::Succeeded,
                exit_code: Some(0),
                result_kind: ToolResultKind::FileRead,
                presentation: None,
                artifacts: Vec::new(),
            },
            json!({
                "type":"tool_result",
                "version":2,
                "tool_use_id":"c",
                "name":"Read",
                "content":"ok",
                "status":{"state":"succeeded"},
                "exit_code":0,
                "result_kind":"file_read",
                "presentation":null,
                "artifacts":[]
            }),
        ),
        (
            EventLog::Stop {
                reason: "end_turn".into(),
            },
            json!({"type":"stop","reason":"end_turn"}),
        ),
        (
            EventLog::Error {
                version: None,
                message: "boom".into(),
            },
            json!({"type":"error","message":"boom"}),
        ),
        (EventLog::Retry, json!({"type":"retry"})),
    ];

    for (event, expected) in cases {
        assert_eq!(serde_json::to_value(event).unwrap(), expected);
    }
}

#[test]
fn typed_events_round_trip_through_jsonl_values() {
    use crate::tools::metadata::{ToolResultKind, ToolStatus};
    use serde_json::json;

    let events = vec![
        EventLog::SessionStart {
            session_id: "s".into(),
        },
        EventLog::TurnStart {
            model: "flash".into(),
            model_alias: Some("flash".into()),
            belief: 0.5,
            forced_model: None,
        },
        EventLog::TurnTracking {
            version: None,
            decision: "Stop".into(),
            tool_call_count: 1,
            tool_error_count: 0,
            belief: 0.5,
            model: "flash".into(),
        },
        EventLog::TurnError {
            error: "e".into(),
            category: "Network".into(),
            severity: Some("Fatal".into()),
            belief: Some(0.5),
            model: Some("flash".into()),
            elapsed_ms: Some(1),
            idle_ms: None,
        },
        EventLog::SubAgent {
            session_id: "sub".into(),
            status: "ok".into(),
            input_tokens: Some(1),
            output_tokens: Some(2),
        },
        EventLog::ToolResult {
            version: Some(2),
            tool_use_id: "c".into(),
            name: "Read".into(),
            content: "ok".into(),
            status: ToolStatus::Succeeded,
            exit_code: Some(0),
            result_kind: ToolResultKind::FileRead,
            presentation: None,
            artifacts: Vec::new(),
        },
        EventLog::SignalHandover {
            belief: 0.2,
            edited_paths: vec!["a".into()],
            evidence: "ev".into(),
            options: vec!["retry".into()],
        },
    ];

    for event in events {
        let value = serde_json::to_value(event).unwrap();
        let decoded: EventLog = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(serde_json::to_value(decoded).unwrap(), value);
    }

    // Old tool_result records without the enriched fields still deserialize.
    let legacy = json!({
        "type":"tool_result",
        "version":1,
        "tool_use_id":"c",
        "name":"Read",
        "content":"ok"
    });
    let event: EventLog = serde_json::from_value(legacy).unwrap();
    assert!(matches!(event, EventLog::ToolResult { .. }));
}
