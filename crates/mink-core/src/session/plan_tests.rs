use super::*;

#[test]
fn project_full_request_applies_consumed_images_then_plan() {
    // The compactor and the turn executor share this projection: consumed
    // image references must become text citations BEFORE the plan projection
    // and any token estimate (or history pictures would be counted as
    // visual tokens forever).
    let (root, store) = store("full-request-projection");
    store.set_draft("# Plan\n", 1024).unwrap();
    store.confirm().unwrap();
    let plan_path = root.join("plan.md");
    let messages = vec![
        serde_json::json!({"role": "user", "content": [
            serde_json::json!({"type": "tool_attachment", "tool_use_id": "a", "url": "image://sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", "format": "png", "width": 1024, "height": 768, "bytes": 100}),
        ]}),
        serde_json::json!({"role": "assistant", "content": [serde_json::json!({"type": "text", "text": "seen"})]}),
        serde_json::json!({"role": "user", "content": [
            serde_json::json!({"type": "tool_attachment", "tool_use_id": "b", "url": "image://sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb", "format": "png", "width": 64, "height": 32, "bytes": 200}),
        ]}),
    ];
    let projected = project_full_request(&plan_path, true, &messages).unwrap();
    // Consumed reference (before last assistant) -> text citation.
    assert_eq!(projected[0]["content"][0]["type"], "text");
    assert!(
        projected[0]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("[Previously attached image"),
        "{}",
        projected[0]
    );
    // Unconsumed (after last assistant) stays a tool_attachment.
    assert_eq!(projected[2]["content"][0]["type"], "tool_attachment");
    // Plan is the last projected message (tail mode).
    assert!(
        projected.last().unwrap()["content"]
            .as_str()
            .unwrap()
            .contains("<current-plan>")
    );
    let _ = std::fs::remove_dir_all(root);
}

fn store(name: &str) -> (PathBuf, PlanStore) {
    let root = std::env::temp_dir().join(format!(
        "mink-{name}-{}-{}",
        std::process::id(),
        TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let store = PlanStore::new(root.join("plan.md"), root.join("plan.draft"));
    (root, store)
}

#[test]
fn draft_confirm_clear_is_a_valid_lifecycle() {
    let (root, store) = store("plan-lifecycle");
    store.set_draft("# Plan\n", 1024).unwrap();
    assert_eq!(store.confirm().unwrap(), "# Plan\n");
    assert_eq!(
        std::fs::read_to_string(root.join("plan.md")).unwrap(),
        "# Plan\n"
    );
    assert!(!root.join("plan.draft").exists());
    store.clear().unwrap();
    assert!(!root.join("plan.md").exists());
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn invalid_transitions_fail_without_mutating_state() {
    let (root, store) = store("plan-invalid");
    assert!(store.confirm().is_err());
    assert!(store.clear().is_err());
    assert!(store.set_draft("oversized", 4).is_err());
    store.set_draft("", 4).unwrap();
    assert!(!root.join("plan.md").exists());
    assert!(!root.join("plan.draft").exists());
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn confirmed_plan_blocks_new_drafts_and_clear_removes_stale_draft() {
    let (root, store) = store("plan-confirmed-state");
    store.set_draft("current plan", 1024).unwrap();
    store.confirm().unwrap();

    let error = store.set_draft("next plan", 1024).unwrap_err().to_string();
    assert!(error.contains("confirmed plan exists"), "{error}");
    assert!(!root.join("plan.draft").exists());

    std::fs::write(root.join("plan.draft"), "legacy stale draft").unwrap();
    store.clear().unwrap();
    assert!(!root.join("plan.md").exists());
    assert!(!root.join("plan.draft").exists());
    let _ = std::fs::remove_dir_all(root);
}

fn plan_content_message() -> serde_json::Value {
    serde_json::json!({
        "role": "system",
        "content": "<current-plan>\n1. implement\n2. verify\n</current-plan>",
    })
}

#[test]
fn current_plan_projection_is_dynamic_and_not_persisted() {
    let (root, store) = store("plan-projection");
    let base = vec![
        serde_json::json!({"role": "system", "content": "<context-snapshot>old</context-snapshot>"}),
        serde_json::json!({"role": "user", "content": "continue"}),
    ];

    // No plan file: both modes return the base untouched.
    assert_eq!(
        project_current_plan(&root.join("plan.md"), &base, false).unwrap(),
        base
    );
    assert_eq!(
        project_current_plan(&root.join("plan.md"), &base, true).unwrap(),
        base
    );

    store.set_draft("1. implement\n2. verify\n", 1024).unwrap();
    store.confirm().unwrap();

    // Legacy head projection: plan inserted after the leading system messages.
    let projected = project_current_plan(&root.join("plan.md"), &base, false).unwrap();
    assert_eq!(projected.len(), base.len() + 1);
    assert_eq!(projected[0], base[0]);
    assert_eq!(projected[1], plan_content_message());
    assert_eq!(projected[2], base[1]);
    assert_eq!(base.len(), 2);

    // Default tail projection: plan appended as the last message.
    let projected = project_current_plan(&root.join("plan.md"), &base, true).unwrap();
    assert_eq!(projected.len(), base.len() + 1);
    assert_eq!(projected[0], base[0]);
    assert_eq!(projected[1], base[1]);
    assert_eq!(projected[2], plan_content_message());

    store.clear().unwrap();
    assert_eq!(
        project_current_plan(&root.join("plan.md"), &base, true).unwrap(),
        base
    );
    let _ = std::fs::remove_dir_all(root);
}
