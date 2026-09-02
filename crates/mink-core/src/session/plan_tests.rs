use super::*;

#[test]
fn project_full_request_only_applies_consumed_images() {
    // The compactor and the turn executor share this projection: consumed
    // image references must become text citations BEFORE the plan projection
    // and any token estimate (or history pictures would be counted as
    // visual tokens forever).
    let (root, _store) = store("full-request-projection");
    let messages = vec![
        serde_json::json!({"role": "user", "content": [
            serde_json::json!({"type": "tool_attachment", "tool_use_id": "a", "url": "image://sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", "format": "png", "width": 1024, "height": 768, "bytes": 100}),
        ]}),
        serde_json::json!({"role": "assistant", "content": [serde_json::json!({"type": "text", "text": "seen"})]}),
        serde_json::json!({"role": "user", "content": [
            serde_json::json!({"type": "tool_attachment", "tool_use_id": "b", "url": "image://sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb", "format": "png", "width": 64, "height": 32, "bytes": 200}),
        ]}),
    ];
    let projected = project_full_request(&messages).unwrap();
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
    assert_eq!(projected.len(), messages.len());
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

fn plan_result(id: &str, command: crate::tools::plan::PlanCommand) -> ToolExecution {
    let (name, content) = match command {
        crate::tools::plan::PlanCommand::SetDraft => ("PlanDraft", "Plan draft saved."),
        crate::tools::plan::PlanCommand::Confirm => {
            ("PlanConfirm", "Plan confirmed and locked in.")
        }
        crate::tools::plan::PlanCommand::Clear => ("PlanClear", "Plan cleared."),
    };
    let mut result = ToolExecution::test_result(id, name, content);
    result.plan_command = Some(command);
    result
}

fn plan_call(id: &str, name: &str) -> crate::protocol::ToolCallEvent {
    crate::sse::toolcall::build_tool_call_event(name, id, "{}").unwrap()
}

#[tokio::test]
async fn draft_confirm_clear_is_a_valid_lifecycle() {
    let (root, plan_store) = store("plan-lifecycle");
    let conversation = ConversationStore::new(root.join("conversation.jsonl"));
    conversation.ensure().await.unwrap();
    conversation
        .add_assistant("", "", &[plan_call("confirm", "PlanConfirm")])
        .await
        .unwrap();
    plan_store.set_draft("# Plan\n", 1024).unwrap();
    assert_eq!(plan_store.confirm().unwrap(), "# Plan\n");
    let mut confirmed = plan_result("confirm", crate::tools::plan::PlanCommand::Confirm);
    plan_store.bind_transition(&mut confirmed).unwrap();
    conversation.add_tool_results(&[confirmed]).await.unwrap();
    plan_store
        .finish_transition(
            &conversation,
            "confirm",
            crate::tools::plan::PlanCommand::Confirm,
        )
        .await
        .unwrap();
    assert_eq!(
        std::fs::read_to_string(root.join("plan.md")).unwrap(),
        "# Plan\n"
    );
    assert!(!root.join("plan.draft").exists());
    plan_store.clear().unwrap();
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

#[tokio::test]
async fn confirmed_plan_blocks_new_drafts_and_clear_removes_stale_draft() {
    let (root, plan_store) = store("plan-confirmed-state");
    let conversation = ConversationStore::new(root.join("conversation.jsonl"));
    conversation.ensure().await.unwrap();
    conversation
        .add_assistant("", "", &[plan_call("confirm", "PlanConfirm")])
        .await
        .unwrap();
    plan_store.set_draft("current plan", 1024).unwrap();
    plan_store.confirm().unwrap();
    let mut confirmed = plan_result("confirm", crate::tools::plan::PlanCommand::Confirm);
    plan_store.bind_transition(&mut confirmed).unwrap();
    conversation.add_tool_results(&[confirmed]).await.unwrap();
    plan_store
        .finish_transition(
            &conversation,
            "confirm",
            crate::tools::plan::PlanCommand::Confirm,
        )
        .await
        .unwrap();

    let error = plan_store
        .set_draft("next plan", 1024)
        .unwrap_err()
        .to_string();
    assert!(error.contains("confirmed plan exists"), "{error}");
    assert!(!root.join("plan.draft").exists());

    std::fs::write(root.join("plan.draft"), "legacy stale draft").unwrap();
    plan_store.clear().unwrap();
    assert!(!root.join("plan.md").exists());
    assert!(!root.join("plan.draft").exists());
    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn recovery_rolls_back_unbound_confirm_to_retryable_draft() {
    let (root, plan_store) = store("plan-unbound-confirm-recovery");
    plan_store.set_draft("# Retryable plan\n", 1024).unwrap();
    plan_store.confirm().unwrap();
    assert!(root.join("plan.md").exists());
    assert!(!root.join("plan.draft").exists());

    let conversation = ConversationStore::new(root.join("conversation.jsonl"));
    conversation.ensure().await.unwrap();
    plan_store.recover_pending(&conversation).await.unwrap();

    assert!(!root.join("plan.md").exists());
    assert_eq!(
        std::fs::read_to_string(root.join("plan.draft")).unwrap(),
        "# Retryable plan\n"
    );
    assert!(conversation.lines().await.unwrap().is_empty());
    let journal: serde_json::Value =
        serde_json::from_slice(&std::fs::read(root.join("plan-transaction.json")).unwrap())
            .unwrap();
    assert_eq!(journal["transactions"].as_array().unwrap().len(), 0);
    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn recovery_replays_bound_confirm_once() {
    let (root, plan_store) = store("plan-bound-confirm-recovery");
    let conversation = ConversationStore::new(root.join("conversation.jsonl"));
    conversation.ensure().await.unwrap();
    conversation
        .add_assistant(
            "",
            "",
            &[
                plan_call("confirm-1", "PlanConfirm"),
                plan_call("other-1", "Bash"),
            ],
        )
        .await
        .unwrap();

    plan_store.set_draft("# Durable plan\n", 1024).unwrap();
    plan_store.confirm().unwrap();
    let mut result = plan_result("confirm-1", crate::tools::plan::PlanCommand::Confirm);
    plan_store.bind_transition(&mut result).unwrap();

    plan_store.recover_pending(&conversation).await.unwrap();
    let once = conversation.lines().await.unwrap();
    assert_eq!(
        once.iter()
            .flat_map(|line| line["content"].as_array().into_iter().flatten())
            .filter(|block| block["tool_use_id"] == "confirm-1")
            .count(),
        1
    );
    let other_result_index = once
        .iter()
        .position(|line| {
            line["content"]
                .as_array()
                .into_iter()
                .flatten()
                .any(|block| block["tool_use_id"] == "other-1")
        })
        .unwrap();
    let transition_index = once
        .iter()
        .position(|line| {
            line["content"]
                .as_str()
                .is_some_and(|content| content.contains("<plan-transition state=\"confirmed\">"))
        })
        .unwrap();
    assert!(other_result_index < transition_index);
    assert_eq!(
        once.iter()
            .filter(|line| line["content"].as_str().is_some_and(|content| {
                content.contains("<plan-transition state=\"confirmed\">")
            }))
            .count(),
        1
    );
    assert_eq!(
        std::fs::read_to_string(root.join("plan.md")).unwrap(),
        "# Durable plan\n"
    );

    plan_store.recover_pending(&conversation).await.unwrap();
    assert_eq!(conversation.lines().await.unwrap().len(), once.len());
    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn recovery_replays_bound_clear_and_removes_active_plan() {
    let (root, plan_store) = store("plan-bound-clear-recovery");
    let conversation = ConversationStore::new(root.join("conversation.jsonl"));
    conversation.ensure().await.unwrap();

    conversation
        .add_assistant("", "", &[plan_call("confirm-1", "PlanConfirm")])
        .await
        .unwrap();
    plan_store.set_draft("# Plan to clear\n", 1024).unwrap();
    plan_store.confirm().unwrap();
    let mut confirmed = plan_result("confirm-1", crate::tools::plan::PlanCommand::Confirm);
    plan_store.bind_transition(&mut confirmed).unwrap();
    conversation.add_tool_results(&[confirmed]).await.unwrap();
    plan_store
        .finish_transition(
            &conversation,
            "confirm-1",
            crate::tools::plan::PlanCommand::Confirm,
        )
        .await
        .unwrap();

    conversation
        .add_assistant("", "", &[plan_call("clear-1", "PlanClear")])
        .await
        .unwrap();
    plan_store.clear().unwrap();
    let mut cleared = plan_result("clear-1", crate::tools::plan::PlanCommand::Clear);
    plan_store.bind_transition(&mut cleared).unwrap();
    // Simulate a crash after the successful result was appended but before
    // the cleared transition reached conversation history.
    conversation.add_tool_results(&[cleared]).await.unwrap();
    assert!(!root.join("plan.md").exists());

    plan_store.recover_pending(&conversation).await.unwrap();
    assert!(!root.join("plan.md").exists());
    let lines = conversation.lines().await.unwrap();
    assert_eq!(
        lines
            .iter()
            .filter(|line| line["content"]
                .as_str()
                .is_some_and(|content| { content.contains("<plan-transition state=\"cleared\">") }))
            .count(),
        1
    );
    assert_eq!(
        lines
            .iter()
            .flat_map(|line| line["content"].as_array().into_iter().flatten())
            .filter(|block| block["tool_use_id"] == "clear-1")
            .count(),
        1
    );
    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn recovery_rolls_back_unbound_clear_to_confirmed_plan() {
    let (root, plan_store) = store("plan-unbound-clear-recovery");
    let conversation = ConversationStore::new(root.join("conversation.jsonl"));
    conversation.ensure().await.unwrap();
    conversation
        .add_assistant("", "", &[plan_call("confirm", "PlanConfirm")])
        .await
        .unwrap();
    plan_store.set_draft("# Still active\n", 1024).unwrap();
    plan_store.confirm().unwrap();
    let mut confirmed = plan_result("confirm", crate::tools::plan::PlanCommand::Confirm);
    plan_store.bind_transition(&mut confirmed).unwrap();
    conversation.add_tool_results(&[confirmed]).await.unwrap();
    plan_store
        .finish_transition(
            &conversation,
            "confirm",
            crate::tools::plan::PlanCommand::Confirm,
        )
        .await
        .unwrap();

    plan_store.clear().unwrap();
    assert!(!root.join("plan.md").exists());
    plan_store.recover_pending(&conversation).await.unwrap();
    assert_eq!(
        std::fs::read_to_string(root.join("plan.md")).unwrap(),
        "# Still active\n"
    );
    assert_eq!(
        conversation
            .lines()
            .await
            .unwrap()
            .iter()
            .filter(|line| line["content"]
                .as_str()
                .is_some_and(|content| { content.contains("<plan-transition state=\"cleared\">") }))
            .count(),
        0
    );
    let _ = std::fs::remove_dir_all(root);
}
