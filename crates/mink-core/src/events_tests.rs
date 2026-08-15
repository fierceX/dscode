use super::*;

#[test]
fn typed_event_serializes_with_version_and_legacy_type_name() {
    let event = EventLog::UserInput {
        version: 1,
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
        version: 1,
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
