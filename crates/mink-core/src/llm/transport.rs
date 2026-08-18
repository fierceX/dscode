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

#[allow(clippy::too_many_arguments)]
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

pub(crate) fn convert_messages_to_openai(messages: &[Value]) -> Result<Vec<Value>> {
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
#[path = "transport_tests.rs"]
mod tests;
