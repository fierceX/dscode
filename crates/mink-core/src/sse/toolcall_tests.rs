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
