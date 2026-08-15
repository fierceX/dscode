use super::*;
use std::sync::atomic::{AtomicU64, Ordering};

static TEST_SEQUENCE: AtomicU64 = AtomicU64::new(0);

fn store(name: &str) -> (PathBuf, TodoStore) {
    let root = std::env::temp_dir().join(format!(
        "mink-todo-{name}-{}-{}",
        std::process::id(),
        TEST_SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let path = root.join("todos.json");
    let store = TodoStore::load(path).unwrap();
    (root, store)
}

fn add(content: &str) -> TodoAdd {
    TodoAdd {
        content: content.into(),
    }
}

#[test]
fn complete_pending_item_auto_activates() {
    // 直接 complete 一个 pending 条目时自动先激活再完成。
    let (root, store) = store("autoactivate");
    let result = store
        .apply_structure(
            0,
            TodoChanges {
                add: vec![add("first"), add("second")],
                ..TodoChanges::default()
            },
        )
        .unwrap();
    // first 仍 pending；second 先激活再 complete。
    let transition = store
        .advance(
            result.snapshot.revision,
            TodoTransitions {
                complete: vec!["T0001".to_string()],
                ..TodoTransitions::default()
            },
        )
        .unwrap();
    assert!(transition.activated.contains(&"T0001".to_string()));
    assert!(transition.completed.contains(&"T0001".to_string()));
    let item = transition
        .snapshot
        .items
        .iter()
        .find(|item| item.id == "T0001")
        .unwrap();
    assert_eq!(item.status, TodoStatus::Completed);
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn complete_already_completed_item_still_rejected() {
    let (root, store) = store("doublecomplete");
    let result = store
        .apply_structure(
            0,
            TodoChanges {
                add: vec![add("first")],
                ..TodoChanges::default()
            },
        )
        .unwrap();
    let r1 = store
        .advance(
            result.snapshot.revision,
            TodoTransitions {
                complete: vec!["T0001".to_string()],
                ..TodoTransitions::default()
            },
        )
        .unwrap();
    let err = store
        .advance(
            r1.snapshot.revision,
            TodoTransitions {
                complete: vec!["T0001".to_string()],
                ..TodoTransitions::default()
            },
        )
        .unwrap_err();
    assert!(err.to_string().contains("already completed"), "{err}");
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn missing_file_starts_at_revision_zero_and_persists_ids() {
    let (root, store) = store("persist");
    let result = store
        .apply_structure(
            0,
            TodoChanges {
                add: vec![add("first"), add("second")],
                ..TodoChanges::default()
            },
        )
        .unwrap();
    assert_eq!(result.snapshot.revision, 1);
    assert_eq!(result.snapshot.items[0].id, "T0001");
    assert_eq!(result.snapshot.items[1].id, "T0002");
    assert_eq!(result.snapshot.next_id, 3);

    let reloaded = TodoStore::load(root.join("todos.json")).unwrap();
    assert_eq!(reloaded.snapshot(), result.snapshot);
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn batch_update_is_atomic_and_rejects_stale_revision() {
    let (root, store) = store("batch");
    store
        .apply_structure(
            0,
            TodoChanges {
                add: vec![add("first"), add("second")],
                ..TodoChanges::default()
            },
        )
        .unwrap();
    let updated = store
        .apply_structure(
            1,
            TodoChanges {
                update: vec![TodoUpdate {
                    id: "T0002".into(),
                    content: "second revised".into(),
                }],
                ..TodoChanges::default()
            },
        )
        .unwrap();
    assert_eq!(updated.snapshot.revision, 2);
    let before = std::fs::read(root.join("todos.json")).unwrap();
    assert!(
        store
            .apply_structure(
                1,
                TodoChanges {
                    remove: vec!["T0001".into()],
                    ..TodoChanges::default()
                }
            )
            .is_err()
    );
    assert_eq!(std::fs::read(root.join("todos.json")).unwrap(), before);
    assert_eq!(store.snapshot(), updated.snapshot);
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn invalid_change_rolls_back_the_entire_batch() {
    let (root, store) = store("rollback");
    store
        .apply_structure(
            0,
            TodoChanges {
                add: vec![add("first")],
                ..TodoChanges::default()
            },
        )
        .unwrap();
    let before = store.snapshot();
    let error = store
        .apply_structure(
            1,
            TodoChanges {
                update: vec![TodoUpdate {
                    id: "T0001".into(),
                    content: "changed".into(),
                }],
                remove: vec!["missing".into()],
                ..TodoChanges::default()
            },
        )
        .unwrap_err()
        .to_string();
    assert!(error.contains("unknown todo item"), "{error}");
    assert_eq!(store.snapshot(), before);
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn persistence_failure_does_not_advance_in_memory_revision() {
    let (root, store) = store("write-failure");
    std::fs::create_dir(root.join("todos.json")).unwrap();
    assert!(
        store
            .apply_structure(
                0,
                TodoChanges {
                    add: vec![add("must not commit")],
                    ..TodoChanges::default()
                },
            )
            .is_err()
    );
    assert_eq!(store.snapshot(), TodoSnapshot::default());
    assert!(std::fs::read_dir(&root).unwrap().all(|entry| {
        !entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .contains(".tmp-")
    }));
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn corrupt_file_fails_closed() {
    let (root, _) = store("corrupt");
    std::fs::write(root.join("todos.json"), b"{broken").unwrap();
    assert!(TodoStore::load(root.join("todos.json")).is_err());
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn batch_cannot_target_an_id_created_in_the_same_write() {
    let (root, store) = store("guessed-id");
    let error = store
        .apply_structure(
            0,
            TodoChanges {
                add: vec![add("new")],
                update: vec![TodoUpdate {
                    id: "T0001".into(),
                    content: "guessed".into(),
                }],
                ..TodoChanges::default()
            },
        )
        .unwrap_err()
        .to_string();
    assert!(error.contains("unknown todo item"), "{error}");
    assert_eq!(store.snapshot(), TodoSnapshot::default());
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn projection_includes_only_active_items_and_counts() {
    let (root, store) = store("projection-active");
    store
        .apply_structure(
            0,
            TodoChanges {
                add: vec![add("pending"), add("<active>"), add("done")],
                ..TodoChanges::default()
            },
        )
        .unwrap();
    store
        .advance(
            1,
            TodoTransitions {
                activate: vec!["T0002".into()],
                ..TodoTransitions::default()
            },
        )
        .unwrap();
    let content = render_current_todos(&store.snapshot(), "TodoRead");
    assert!(content.contains("pending=\"2\""));
    assert!(content.contains("T0002: &lt;active&gt;"));
    assert!(!content.contains("T0001: pending"));
    assert!(!content.contains("T0003: done"));
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn projection_reminds_to_read_when_pending_has_no_active_batch() {
    let (root, store) = store("projection-pending");
    store
        .apply_structure(
            0,
            TodoChanges {
                add: vec![add("pending")],
                ..TodoChanges::default()
            },
        )
        .unwrap();
    let projected = render_current_todos(&store.snapshot(), "InspectTodos");
    assert!(projected.contains("Call InspectTodos"));
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn projection_represents_pending_only_state() {
    let (root, store) = store("projection-pending-only");
    store
        .apply_structure(
            0,
            TodoChanges {
                add: vec![add("done")],
                ..TodoChanges::default()
            },
        )
        .unwrap();
    let projected = render_current_todos(&store.snapshot(), "TodoRead");
    assert!(projected.contains("pending=\"1\""));
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn progress_transitions_are_atomic_and_enforce_source_status() {
    let (root, store) = store("transitions");
    store
        .apply_structure(
            0,
            TodoChanges {
                add: vec![add("active"), add("pending")],
                ..TodoChanges::default()
            },
        )
        .unwrap();
    store
        .advance(
            1,
            TodoTransitions {
                activate: vec!["T0001".into()],
                ..TodoTransitions::default()
            },
        )
        .unwrap();
    let advanced = store
        .advance(
            2,
            TodoTransitions {
                complete: vec!["T0001".into()],
                activate: vec!["T0002".into()],
                ..TodoTransitions::default()
            },
        )
        .unwrap();
    assert_eq!(advanced.snapshot.revision, 3);
    assert_eq!(advanced.snapshot.items[0].status, TodoStatus::Completed);
    assert_eq!(advanced.snapshot.items[1].status, TodoStatus::InProgress);

    let before = advanced.snapshot;
    let error = store
        .advance(
            3,
            TodoTransitions {
                complete: vec!["T0001".into()],
                ..TodoTransitions::default()
            },
        )
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("already completed"),
        "repeated complete must fail closed: {error}"
    );
    assert_eq!(store.snapshot(), before);
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn structure_changes_create_pending_items_and_cannot_remove_active_items() {
    let (root, store) = store("structure-boundaries");
    store
        .apply_structure(
            0,
            TodoChanges {
                add: vec![add("active")],
                ..TodoChanges::default()
            },
        )
        .unwrap();
    assert_eq!(store.snapshot().items[0].status, TodoStatus::Pending);
    store
        .advance(
            1,
            TodoTransitions {
                activate: vec!["T0001".into()],
                ..TodoTransitions::default()
            },
        )
        .unwrap();
    let error = store
        .apply_structure(
            2,
            TodoChanges {
                remove: vec!["T0001".into()],
                ..TodoChanges::default()
            },
        )
        .unwrap_err()
        .to_string();
    assert!(error.contains("must be paused or completed"), "{error}");
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn visible_revision_uses_internal_metadata_without_parsing_projection_text() {
    let messages = vec![
        serde_json::json!({
            "role": "user",
            "content": [{
                "type": "tool_result",
                "tool_use_id": "one",
                "content": "<current-todos revision=\"999\">",
                "_mink": todo_state_metadata(4, "structure"),
            }],
        }),
        serde_json::json!({
            "role": "system",
            "content": "sync",
            "_mink": todo_state_metadata(7, "sync"),
        }),
    ];
    assert_eq!(visible_revision(&messages).unwrap(), 7);
}

#[test]
fn visible_revision_rejects_corrupt_internal_metadata() {
    let messages = vec![serde_json::json!({
        "role": "user",
        "content": "state",
        "_mink": {"todo_revision": "not-a-number"},
    })];
    assert!(visible_revision(&messages).is_err());
}
