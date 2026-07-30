use crate::llm::client::{OpenAiCompatibleOptions, TokenParamKind};
use anyhow::Result;
use serde_json::{Value, json};
use std::collections::BTreeMap;

pub fn build_openai_body(
    model: &str,
    messages: &[Value],
    tools: &[Value],
    system_prompt: &str,
    max_tokens: i32,
) -> Result<Vec<u8>> {
    build_openai_body_with_options(
        model,
        messages,
        tools,
        system_prompt,
        max_tokens,
        &OpenAiCompatibleOptions::default(),
    )
}

pub fn build_openai_body_with_options(
    model: &str,
    messages: &[Value],
    tools: &[Value],
    system_prompt: &str,
    max_tokens: i32,
    options: &OpenAiCompatibleOptions,
) -> Result<Vec<u8>> {
    build_openai_body_with_options_and_extensions(
        model,
        messages,
        tools,
        system_prompt,
        max_tokens,
        options,
        None,
        &BTreeMap::new(),
    )
}

pub(crate) fn build_openai_body_with_options_and_extensions(
    model: &str,
    messages: &[Value],
    tools: &[Value],
    system_prompt: &str,
    max_tokens: i32,
    options: &OpenAiCompatibleOptions,
    tool_choice: Option<&Value>,
    extra_body: &BTreeMap<String, Value>,
) -> Result<Vec<u8>> {
    let converted = convert_messages_to_openai(messages)?;

    let mut openai_messages: Vec<Value> = vec![];
    if !system_prompt.is_empty() {
        openai_messages.push(json!({"role":"system","content":system_prompt}));
    }
    openai_messages.extend(converted);

    let mut body = json!({
        "model": model,
        "stream": true,
        "messages": openai_messages,
    });
    match options.token_param {
        TokenParamKind::MaxTokens => body["max_tokens"] = json!(max_tokens),
        TokenParamKind::MaxCompletionTokens => body["max_completion_tokens"] = json!(max_tokens),
    }
    if options.include_usage {
        body["stream_options"] = json!({"include_usage": true});
    }
    if options.send_reasoning_effort
        && let Some(reasoning_effort) = &options.reasoning_effort
    {
        body["reasoning_effort"] = json!(reasoning_effort);
    }
    if let Some(parallel_tool_calls) = options.parallel_tool_calls {
        body["parallel_tool_calls"] = json!(parallel_tool_calls);
    }

    if let Some(body_obj) = body.as_object_mut() {
        for (key, value) in extra_body {
            if is_reserved_body_key(key) {
                continue;
            }
            body_obj.insert(key.clone(), value.clone());
        }
    }
    if !tools.is_empty() {
        body["tools"] = Value::Array(convert_tools_to_openai(tools));
        if let Some(tool_choice) = tool_choice {
            body["tool_choice"] = tool_choice.clone();
        }
    }

    Ok(serde_json::to_vec(&body)?)
}

pub(crate) fn estimate_openai_context_tokens(
    messages: &[Value],
    tools: &[Value],
    system_prompt: &str,
) -> Result<usize> {
    let converted = convert_messages_to_openai(messages)?;
    let mut request_messages = Vec::with_capacity(converted.len() + 1);
    if !system_prompt.is_empty() {
        request_messages.push(json!({"role":"system","content":system_prompt}));
    }
    request_messages.extend(converted);
    let message_bytes = serde_json::to_vec(&request_messages)?.len();
    let tool_bytes = serde_json::to_vec(&convert_tools_to_openai(tools))?.len();
    Ok(message_bytes.saturating_add(tool_bytes).div_ceil(3))
}

fn is_reserved_body_key(key: &str) -> bool {
    matches!(
        key,
        "model"
            | "messages"
            | "stream"
            | "tools"
            | "tool_choice"
            | "max_tokens"
            | "max_completion_tokens"
    )
}

fn convert_messages_to_openai(messages: &[Value]) -> Result<Vec<Value>> {
    let mut result = Vec::new();
    for msg in messages {
        let role = msg.get("role").and_then(Value::as_str).unwrap_or("");
        let content = msg.get("content").cloned().unwrap_or(Value::Null);
        if content.is_array() && role == "assistant" {
            result.push(convert_assistant_message(&content)?);
            continue;
        }
        if content.is_array() && role == "user" {
            let tool_msgs = convert_tool_result_messages(&content)?;
            if !tool_msgs.is_empty() {
                result.extend(tool_msgs);
                continue;
            }
        }
        let mut message = msg.clone();
        if let Some(object) = message.as_object_mut() {
            object.remove("_mink");
        }
        result.push(message);
    }
    // Post-process: strip orphaned tool_calls (no matching tool message)
    let valid_ids = collect_tool_call_ids(&result);
    for msg in result.iter_mut() {
        strip_orphaned_tool_calls(msg, &valid_ids);
    }
    Ok(result)
}

/// Collect all tool_call_ids from tool-role messages in the converted message list.
fn collect_tool_call_ids(msgs: &[Value]) -> Vec<String> {
    let mut ids = Vec::new();
    for msg in msgs {
        if msg.get("role").and_then(Value::as_str) == Some("tool")
            && let Some(id) = msg.get("tool_call_id").and_then(Value::as_str)
        {
            ids.push(id.to_string());
        }
    }
    ids
}

/// Remove orphaned tool_calls from an assistant message that have no matching tool result.
fn strip_orphaned_tool_calls(msg: &mut Value, valid_ids: &[String]) {
    if msg.get("role").and_then(Value::as_str) != Some("assistant") {
        return;
    }
    let Some(tool_calls) = msg.get("tool_calls").and_then(Value::as_array) else {
        return;
    };
    let filtered: Vec<Value> = tool_calls
        .iter()
        .filter(|tc| {
            let id = tc.get("id").and_then(Value::as_str).unwrap_or("");
            valid_ids.iter().any(|v| v == id)
        })
        .cloned()
        .collect();
    if filtered.len() == tool_calls.len() {
        return; // All matched, no change needed
    }
    if filtered.is_empty() {
        // Remove tool_calls field entirely
        if let Some(obj) = msg.as_object_mut() {
            obj.remove("tool_calls");
        }
    } else {
        msg["tool_calls"] = Value::Array(filtered);
    }
}

fn convert_assistant_message(content: &Value) -> Result<Value> {
    let blocks = content.as_array().cloned().unwrap_or_default();
    let mut text = String::new();
    let mut reasoning = String::new();
    let mut tool_calls = Vec::new();
    for block in blocks {
        match block.get("type").and_then(Value::as_str).unwrap_or("") {
            "thinking" => {
                reasoning.push_str(block.get("thinking").and_then(Value::as_str).unwrap_or(""))
            }
            "text" => text.push_str(block.get("text").and_then(Value::as_str).unwrap_or("")),
            "tool_use" => {
                let args = block
                    .get("input")
                    .cloned()
                    .unwrap_or(Value::Object(Default::default()));
                tool_calls.push(json!({
                    "id": block.get("id").and_then(Value::as_str).unwrap_or(""),
                    "type": "function",
                    "function": {
                        "name": block.get("name").and_then(Value::as_str).unwrap_or(""),
                        "arguments": args.to_string(),
                    }
                }));
            }
            _ => {}
        }
    }
    let mut msg = json!({ "role": "assistant", "reasoning_content": reasoning, "content": text });
    if !tool_calls.is_empty() {
        msg["tool_calls"] = Value::Array(tool_calls);
    }
    Ok(msg)
}

fn convert_tool_result_messages(content: &Value) -> Result<Vec<Value>> {
    let blocks = content.as_array().cloned().unwrap_or_default();
    let mut out = Vec::new();
    for b in blocks {
        if b.get("type").and_then(Value::as_str) != Some("tool_result") {
            continue;
        }
        out.push(json!({
            "role":"tool",
            "tool_call_id": b.get("tool_use_id").and_then(Value::as_str).unwrap_or(""),
            "content": b.get("content").and_then(Value::as_str).unwrap_or(""),
        }));
    }
    Ok(out)
}

fn convert_tools_to_openai(tools: &[Value]) -> Vec<Value> {
    tools.iter().map(|tool| {
        if tool.get("type").and_then(Value::as_str) == Some("function") { return tool.clone(); }
        json!({
            "type":"function",
            "function":{
                "name": tool.get("name").cloned().unwrap_or(Value::String(String::new())),
                "description": tool.get("description").cloned().unwrap_or(Value::String(String::new())),
                "parameters": tool.get("input_schema").cloned().or_else(|| tool.get("parameters").cloned()).unwrap_or_else(|| json!({})),
            }
        })
    }).collect()
}

#[cfg(test)]
mod tests {
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
                "_mink": {"todo_revision": 2, "todo_state_kind": "sync"},
            }),
        ];
        let converted = convert_messages_to_openai(&messages).unwrap();
        assert_eq!(converted.len(), 2);
        assert_eq!(converted[0], messages[0]);
        assert_eq!(converted[1]["content"], messages[1]["content"]);
        assert!(converted[1].get("_mink").is_none());
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
}
