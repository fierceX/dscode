use super::*;
use crate::runtime::SessionInfo;

#[test]
fn turn_status_maps_to_sdk_status_and_exit_code() {
    assert_eq!(sdk_status_from_turn(TurnStatus::Ok), SdkStatus::Ok);
    assert_eq!(sdk_status_from_turn(TurnStatus::Failed), SdkStatus::Failed);
    assert_eq!(
        sdk_status_from_turn(TurnStatus::Interrupted),
        SdkStatus::Interrupted
    );
    assert_eq!(
        sdk_status_from_turn(TurnStatus::MaxTurnsExceeded),
        SdkStatus::MaxTurnsExceeded
    );
    assert_eq!(exit_code_from_turn(TurnStatus::Ok), 0);
    assert_eq!(exit_code_from_turn(TurnStatus::Failed), 1);
    assert_eq!(exit_code_from_turn(TurnStatus::Interrupted), 130);
    assert_eq!(exit_code_from_turn(TurnStatus::MaxTurnsExceeded), 2);
}

#[test]
fn final_from_outcome_contains_all_required_fields() {
    use std::path::PathBuf;
    let session = SessionInfo {
        session_id: "sid-1".into(),
        session_ref: "ref-1".into(),
        is_new: true,
        home: PathBuf::from("/tmp/home"),
        cwd: PathBuf::from("/tmp/cwd"),
        events_path: PathBuf::from("/tmp/home/sid-1/events.jsonl"),
        conversation_path: PathBuf::from("/tmp/home/sid-1/conversation.jsonl"),
        artifacts_dir: PathBuf::from("/tmp/home/sid-1/artifacts"),
        summary_path: PathBuf::from("/tmp/home/sid-1/summary.json"),
        usage_path: PathBuf::from("/tmp/home/sid-1/usage.jsonl"),
        plan_path: PathBuf::from("/tmp/home/sid-1/plan.md"),
        plan_draft_path: PathBuf::from("/tmp/home/sid-1/plan.draft"),
        todos_path: PathBuf::from("/tmp/home/sid-1/todos.json"),
    };
    let outcome = TurnOutcome {
        turn_id: crate::runtime::TurnId::new("sid-1:1"),
        billing_turn_id: "turn-1".into(),
        status: TurnStatus::Ok,
        session: session.clone(),
        text: "hello".into(),
        thinking: "hmm".into(),
        tool_call_count: 3,
        tool_error_count: 1,
        error: None,
        usage_records: Vec::new(),
        usage: Default::default(),
    };

    let final_json = serde_json::to_value(final_from_outcome(&outcome)).unwrap();
    assert_eq!(final_json["type"], "final");
    assert_eq!(final_json["version"], PROTOCOL_VERSION);
    assert_eq!(final_json["session_id"], "sid-1");
    assert_eq!(final_json["session_ref"], "ref-1");
    assert_eq!(final_json["home"], "/tmp/home");
    assert_eq!(final_json["cwd"], "/tmp/cwd");
    assert_eq!(final_json["tool_call_count"], 3);
    assert_eq!(final_json["tool_error_count"], 1);
    assert!(final_json["error"].is_null());
}

#[test]
fn final_fields_match_python_sdk_contract() {
    // Python SDK reads these fields from the final JSON line.
    // Every mink-core --agent-jsonl run emits these keys.
    let expected_keys = &[
        "type",
        "version",
        "status",
        "billing_turn_id",
        "session_id",
        "session_ref",
        "home",
        "cwd",
        "events_path",
        "conversation_path",
        "artifacts_dir",
        "summary_path",
        "usage_path",
        "usage_records",
        "usage",
        "tool_call_count",
        "tool_error_count",
        "error",
    ];
    let session = SessionInfo {
        session_id: "sid".into(),
        session_ref: "ref".into(),
        is_new: false,
        home: "/h".into(),
        cwd: "/c".into(),
        events_path: "/h/sid/events.jsonl".into(),
        conversation_path: "/h/sid/conversation.jsonl".into(),
        artifacts_dir: "/h/sid/artifacts".into(),
        summary_path: "/h/sid/summary.json".into(),
        usage_path: "/h/sid/usage.jsonl".into(),
        plan_path: "/h/sid/plan.md".into(),
        plan_draft_path: "/h/sid/plan.draft".into(),
        todos_path: "/h/sid/todos.json".into(),
    };
    let outcome = TurnOutcome {
        turn_id: crate::runtime::TurnId::new("sid:1"),
        billing_turn_id: "turn-2".into(),
        status: TurnStatus::Failed,
        session,
        text: String::new(),
        thinking: String::new(),
        tool_call_count: 0,
        tool_error_count: 5,
        error: Some("something broke".into()),
        usage_records: Vec::new(),
        usage: Default::default(),
    };
    let final_json = serde_json::to_value(final_from_outcome(&outcome)).unwrap();
    for key in expected_keys {
        assert!(
            final_json.as_object().unwrap().contains_key(*key),
            "final JSON missing key: {key}"
        );
    }
}
