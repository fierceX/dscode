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
mod tests {
    use super::*;

    #[test]
    fn build_tool_call_empty_input_defaults_to_object() {
        let event = build_tool_call_event("Read", "call_1", " ").unwrap();
        assert_eq!(event.name, "Read");
        assert_eq!(event.id, "call_1");
        assert!(event.fields.is_empty());
        assert!(event.order.is_empty());
        assert_eq!(event.input_json, serde_json::json!({}));
    }

    #[test]
    fn build_tool_call_records_field_order_and_scalar_strings() {
        let event = build_tool_call_event(
            "Bash",
            "call_2",
            r#"{"command":"echo hi","timeout":3,"safe":true,"env":null}"#,
        )
        .unwrap();
        assert_eq!(event.order, ["command", "env", "safe", "timeout"]);
        assert_eq!(event.fields["command"], "echo hi");
        assert_eq!(event.fields["timeout"], "3");
        assert_eq!(event.fields["safe"], "true");
        assert_eq!(event.fields["env"], "null");
    }

    #[test]
    fn build_tool_call_rejects_non_object_input() {
        let err = build_tool_call_event("Read", "call_3", r#"[]"#)
            .unwrap_err()
            .to_string();
        assert!(err.contains("tool input must be object"), "{err}");
    }

    #[test]
    fn nested_tool_fields_are_preserved_as_json() {
        let event = build_tool_call_event(
            "TodoWrite",
            "call_todo",
            r#"{"base_revision":2,"update":[{"id":"T0001","content":"revised"}]}"#,
        )
        .unwrap();
        assert_eq!(event.order, ["base_revision", "update"]);
        assert_eq!(event.fields["base_revision"], "2");
        assert_eq!(
            event.fields["update"],
            r#"[{"content":"revised","id":"T0001"}]"#
        );
    }
}
