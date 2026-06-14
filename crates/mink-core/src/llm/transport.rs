use anyhow::Result;
use serde_json::{Value, json};

pub fn build_openai_body(
    model: &str,
    messages: &[Value],
    tools: &[Value],
    system_prompt: &str,
    max_tokens: i32,
) -> Result<Vec<u8>> {
    let converted = convert_messages_to_openai(messages)?;

    let mut openai_messages: Vec<Value> = vec![];
    if !system_prompt.is_empty() {
        openai_messages.push(json!({"role":"system","content":system_prompt}));
    }
    openai_messages.extend(converted);

    let mut body = json!({
        "model": model,
        "max_tokens": max_tokens,
        "stream": true,
        "messages": openai_messages,
        "reasoning_effort": "max",
    });

    if !tools.is_empty() {
        body["tools"] = Value::Array(convert_tools_to_openai(tools));
    }

    Ok(serde_json::to_vec(&body)?)
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
        result.push(msg.clone());
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
}
