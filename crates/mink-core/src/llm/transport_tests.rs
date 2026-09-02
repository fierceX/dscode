use super::*;

#[test]
fn strip_orphaned_tool_calls_removes_unmatched() {
    let mut msg = json!({
        "role": "assistant",
        "content": "text",
        "tool_calls": [
            {"id": "call_1", "type": "function", "function": {"name": "Read", "arguments": "{}"}},
            {"id": "call_2", "type": "function", "function": {"name": "Bash", "arguments": "{}"}}
        ]
    });
    // Only call_2 has a matching tool result
    let valid_ids = vec!["call_2".to_string()];
    strip_orphaned_tool_calls(&mut msg, &valid_ids);
    let tcs = msg["tool_calls"].as_array().unwrap();
    assert_eq!(tcs.len(), 1);
    assert_eq!(tcs[0]["id"], "call_2");
    assert!(msg.get("content").is_some());
}

#[test]
fn strip_orphaned_removes_tool_calls_field_when_all_orphaned() {
    let mut msg = json!({
        "role": "assistant",
        "content": "text",
        "tool_calls": [
            {"id": "call_1", "type": "function", "function": {"name": "Read", "arguments": "{}"}}
        ]
    });
    let valid_ids: Vec<String> = vec![];
    strip_orphaned_tool_calls(&mut msg, &valid_ids);
    assert!(msg.get("tool_calls").is_none());
    assert_eq!(msg["content"], "text");
}

#[test]
fn strip_orphaned_keeps_all_when_matched() {
    let mut msg = json!({
        "role": "assistant",
        "content": "text",
        "tool_calls": [
            {"id": "call_1", "type": "function", "function": {"name": "Read", "arguments": "{}"}},
            {"id": "call_2", "type": "function", "function": {"name": "Bash", "arguments": "{}"}}
        ]
    });
    let valid_ids = vec!["call_1".to_string(), "call_2".to_string()];
    strip_orphaned_tool_calls(&mut msg, &valid_ids);
    let tcs = msg["tool_calls"].as_array().unwrap();
    assert_eq!(tcs.len(), 2);
}

#[test]
fn strip_orphaned_ignores_non_assistant_messages() {
    let mut msg = json!({"role": "user", "content": "hello"});
    let original = msg.clone();
    strip_orphaned_tool_calls(&mut msg, &["x".to_string()]);
    assert_eq!(msg, original);
}

#[test]
fn convert_messages_strips_orphaned_end_to_end() {
    // Simulate: assistant with tool_calls but no tool result
    let msgs = vec![
        json!({"role": "user", "content": "do it"}),
        json!({"role": "assistant", "content": [
            {"type": "text", "text": "ok"},
            {"type": "tool_use", "id": "orphan_1", "name": "Read", "input": {"path": "/x"}}
        ]}),
        // No tool result for orphan_1 → next message is another user message
        json!({"role": "user", "content": "next"}),
    ];
    let converted = convert_messages_to_openai(&msgs).unwrap();
    // The assistant message should have tool_calls stripped (no matching tool msg)
    let assistant = converted.iter().find(|m| m["role"] == "assistant").unwrap();
    assert!(assistant.get("tool_calls").is_none());
}

#[test]
fn convert_messages_keeps_matched_tool_calls() {
    let msgs = vec![
        json!({"role": "user", "content": "do it"}),
        json!({"role": "assistant", "content": [
            {"type": "text", "text": "ok"},
            {"type": "tool_use", "id": "call_1", "name": "Read", "input": {"path": "/x"}}
        ]}),
        json!({"role": "user", "content": [
            {"type": "tool_result", "tool_use_id": "call_1", "content": "result"}
        ]}),
    ];
    let converted = convert_messages_to_openai(&msgs).unwrap();
    // Should have tool-role message for call_1
    assert!(
        converted
            .iter()
            .any(|m| m["role"] == "tool" && m["tool_call_id"] == "call_1")
    );
    // Assistant should still have tool_calls (call_1 is matched)
    let assistant = converted.iter().find(|m| m["role"] == "assistant").unwrap();
    assert!(assistant.get("tool_calls").is_some());
}

#[test]
fn internal_todo_metadata_is_not_sent_to_the_provider() {
    let messages = vec![
        json!({"role": "user", "content": "start"}),
        json!({
            "role": "user",
            "content": "<todo-sync revision=\"2\">state</todo-sync>",
            "internal": true,
            "_mink": {"todo_revision": 2, "todo_state_kind": "sync"},
        }),
    ];
    let converted = convert_messages_to_openai(&messages).unwrap();
    assert_eq!(converted.len(), 2);
    assert_eq!(converted[0], messages[0]);
    assert_eq!(converted[1]["content"], messages[1]["content"]);
    assert!(converted[1].get("_mink").is_none());
    assert!(converted[1].get("internal").is_none());
}

#[test]
fn openai_body_respects_compatible_options() {
    let body = build_openai_body_with_options(
        "custom-model",
        &[json!({"role":"user","content":"hello"})],
        &[],
        "",
        123,
        &OpenAiCompatibleOptions {
            send_reasoning_effort: false,
            reasoning_effort: None,
            include_usage: false,
            token_param: TokenParamKind::MaxCompletionTokens,
            parallel_tool_calls: Some(false),
        },
    )
    .unwrap();
    let value: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(value["model"], "custom-model");
    assert_eq!(value["max_completion_tokens"], 123);
    assert!(value.get("max_tokens").is_none());
    assert!(value.get("reasoning_effort").is_none());
    assert!(value.get("stream_options").is_none());
    assert_eq!(value["parallel_tool_calls"], false);
}

#[test]
fn openai_body_merges_extra_body_and_tool_choice_without_overriding_core_fields() {
    let mut extra_body = std::collections::BTreeMap::new();
    extra_body.insert("temperature".to_string(), json!(0.2));
    extra_body.insert("enable_thinking".to_string(), json!(true));
    extra_body.insert("model".to_string(), json!("ignored-model"));
    extra_body.insert("messages".to_string(), json!([]));
    extra_body.insert("stream".to_string(), json!(false));
    extra_body.insert("max_tokens".to_string(), json!(999));
    extra_body.insert("tool_choice".to_string(), json!("none"));

    let options = OpenAiCompatibleOptions {
        send_reasoning_effort: false,
        reasoning_effort: None,
        include_usage: false,
        token_param: TokenParamKind::MaxTokens,
        parallel_tool_calls: None,
    };
    let tool_choice = json!("auto");
    let tools = vec![json!({
        "name": "Read",
        "description": "read file",
        "input_schema": {
            "type": "object",
            "properties": {
                "path": {"type": "string"}
            },
            "required": ["path"]
        }
    })];
    let body = build_openai_body_with_options_and_extensions(
        "custom-model",
        &[json!({"role":"user","content":"hello"})],
        &tools,
        "",
        123,
        &options,
        Some(&tool_choice),
        &extra_body,
    )
    .unwrap();

    let value: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(value["model"], "custom-model");
    assert_eq!(value["stream"], true);
    assert_eq!(value["max_tokens"], 123);
    assert_eq!(value["temperature"], 0.2);
    assert_eq!(value["enable_thinking"], true);
    assert_eq!(value["tool_choice"], "auto");
    assert!(
        value["tools"]
            .as_array()
            .is_some_and(|tools| tools.len() == 1)
    );
    assert_eq!(value["messages"].as_array().unwrap().len(), 1);
}

#[test]
fn openai_body_omits_tool_choice_when_no_tools_are_sent() {
    let options = OpenAiCompatibleOptions {
        send_reasoning_effort: false,
        reasoning_effort: None,
        include_usage: false,
        token_param: TokenParamKind::MaxTokens,
        parallel_tool_calls: None,
    };
    let tool_choice = json!("auto");

    let body = build_openai_body_with_options_and_extensions(
        "custom-model",
        &[json!({"role":"user","content":"hello"})],
        &[],
        "",
        123,
        &options,
        Some(&tool_choice),
        &Default::default(),
    )
    .unwrap();

    let value: Value = serde_json::from_slice(&body).unwrap();
    assert!(value.get("tools").is_none());
    assert!(value.get("tool_choice").is_none());
}

#[test]
fn estimate_counts_tool_attachment_pixel_payload() {
    use serde_json::json;
    // A 1024x768 attachment: 85 + 170*4 = 765 tokens on top of JSON bytes.
    let messages = vec![json!({"role": "user", "content": [
        json!({"type": "tool_attachment", "tool_use_id": "c", "url": "image://sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", "format": "png", "width": 1024, "height": 768, "bytes": 1024}),
    ]})];
    let plain = estimate_openai_context_tokens(&messages, &[], "").unwrap();
    let without_attachment = estimate_openai_context_tokens(
        &[json!({"role": "user", "content": [json!({"type": "text", "text": "x"})]})],
        &[],
        "",
    )
    .unwrap();
    // Attachment must add the tile estimate (>= 765 tokens).
    assert!(
        plain >= without_attachment + 765,
        "{plain} vs {without_attachment}"
    );
}

#[test]
fn estimate_counts_only_unconsumed_after_projection() {
    use serde_json::json;
    // Single-consumption: project consumed references into text citations
    // first, then only the unconsumed batch contributes image tokens.
    let messages = vec![
        json!({"role": "user", "content": [
            json!({"type": "tool_attachment", "tool_use_id": "a", "url": "image://sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", "format": "png", "width": 1024, "height": 768, "bytes": 1000}),
        ]}),
        json!({"role": "assistant", "content": [json!({"type": "text", "text": "seen"})]}),
        json!({"role": "user", "content": [
            json!({"type": "tool_attachment", "tool_use_id": "b", "url": "image://sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb", "format": "png", "width": 1024, "height": 768, "bytes": 1000}),
        ]}),
    ];
    let projected = crate::llm::image_projection::project_consumed_attachments(&messages);
    let counted = estimate_openai_context_tokens(&projected, &[], "").unwrap();
    let fresh_only = estimate_openai_context_tokens(
        &[json!({"role": "user", "content": [
            json!({"type": "tool_attachment", "tool_use_id": "b", "url": "image://sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb", "format": "png", "width": 1024, "height": 768, "bytes": 1000}),
        ]})],
        &[],
        "",
    )
    .unwrap();
    assert!(counted >= fresh_only, "{counted} vs {fresh_only}");
}
