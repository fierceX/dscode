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
    Ok(result)
}

fn convert_assistant_message(content: &Value) -> Result<Value> {
    let blocks = content.as_array().cloned().unwrap_or_default();
    let mut text = String::new();
    let mut reasoning = String::new();
    let mut tool_calls = Vec::new();
    for block in blocks {
        match block.get("type").and_then(Value::as_str).unwrap_or("") {
            "thinking" => reasoning.push_str(block.get("thinking").and_then(Value::as_str).unwrap_or("")),
            "text" => text.push_str(block.get("text").and_then(Value::as_str).unwrap_or("")),
            "tool_use" => {
                let args = block.get("input").cloned().unwrap_or(Value::Object(Default::default()));
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
        if b.get("type").and_then(Value::as_str) != Some("tool_result") { continue; }
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
