use crate::protocol::ToolCallEvent;
use anyhow::{Result, anyhow};
use serde_json::Value;
use std::collections::BTreeMap;

pub fn build_tool_call_event(name: &str, id: &str, input: &str) -> Result<ToolCallEvent> {
    let trimmed = if input.trim().is_empty() {
        "{}"
    } else {
        input.trim()
    };
    let obj: Value = serde_json::from_str(trimmed).map_err(|e| anyhow!("parse tool input: {e}"))?;
    let mut event = ToolCallEvent {
        name: name.to_string(),
        id: id.to_string(),
        input_json: obj.clone(),
        fields: BTreeMap::new(),
        order: Vec::new(),
    };
    let map = obj
        .as_object()
        .ok_or_else(|| anyhow!("tool input must be object"))?;
    for (k, v) in map {
        event.order.push(k.clone());
        event.fields.insert(k.clone(), json_scalar_string(v));
    }
    Ok(event)
}

fn json_scalar_string(v: &Value) -> String {
    match v {
        Value::Null => "null".to_string(),
        Value::String(s) => s.clone(),
        Value::Bool(b) => b.to_string(),
        _ => v.to_string(),
    }
}

#[cfg(test)]
#[path = "toolcall_tests.rs"]
mod tests;
