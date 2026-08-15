use super::*;

#[test]
fn snapshot_hides_completed_items_by_default() {
    let snapshot = TodoSnapshot {
        version: 1,
        revision: 3,
        next_id: 4,
        items: vec![
            crate::session::todo::TodoItem {
                id: "T0001".into(),
                content: "active".into(),
                status: TodoStatus::InProgress,
            },
            crate::session::todo::TodoItem {
                id: "T0002".into(),
                content: "done".into(),
                status: TodoStatus::Completed,
            },
        ],
    };
    let rendered = render_snapshot(&snapshot, false);
    assert!(rendered.contains("T0001: active"));
    assert!(!rendered.contains("T0002: done"));
    assert!(render_snapshot(&snapshot, true).contains("T0002: done"));
}

#[test]
fn todo_write_protocol_rejects_all_status_fields() {
    let update_error = serde_json::from_value::<TodoWriteArgs>(serde_json::json!({
        "base_revision": 2,
        "update": [{"id": "T0001", "status": "completed"}],
    }))
    .unwrap_err()
    .to_string();
    assert!(update_error.contains("unknown field"), "{update_error}");

    let add_error = serde_json::from_value::<TodoWriteArgs>(serde_json::json!({
        "base_revision": 2,
        "add": [{"content": "cannot start active", "status": "in_progress"}],
    }))
    .unwrap_err()
    .to_string();
    assert!(add_error.contains("unknown field"), "{add_error}");
}

#[test]
fn progress_result_contains_delta_and_materialized_projection() {
    let result = TodoTransitionResult {
        snapshot: TodoSnapshot {
            version: 1,
            revision: 5,
            next_id: 3,
            items: vec![
                crate::session::todo::TodoItem {
                    id: "T0001".into(),
                    content: "done".into(),
                    status: TodoStatus::Completed,
                },
                crate::session::todo::TodoItem {
                    id: "T0002".into(),
                    content: "active".into(),
                    status: TodoStatus::InProgress,
                },
            ],
        },
        completed: vec!["T0001".into()],
        activated: vec!["T0002".into()],
        paused: vec![],
        reopened: vec![],
    };
    let rendered = render_transition_result(&result, "InspectTodos");
    assert!(rendered.contains("<todo-event revision=\"5\" kind=\"progress\">"));
    assert!(rendered.contains("Completed: T0001"));
    assert!(rendered.contains("<current-todos revision=\"5\""));
    assert!(rendered.contains("T0002: active"));
}
