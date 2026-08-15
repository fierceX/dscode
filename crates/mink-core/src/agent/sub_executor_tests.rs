use super::*;
use crate::llm::mock::MockLlmBackend;
use crate::protocol::{Event, StopEvent, TextEvent};

#[tokio::test]
async fn child_inherits_resource_session_but_has_own_agent_session() {
    let mut parent = crate::regression::test_context_for_agent("sub-vfs-scope")
        .await
        .unwrap();
    Arc::get_mut(&mut parent)
        .expect("test context should be uniquely owned")
        .vfs_scope
        .resource_session_id = "tenant-knowledge".into();
    parent
        .todo_store
        .apply_structure(
            0,
            crate::session::todo::TodoChanges {
                add: vec![crate::session::todo::TodoAdd {
                    content: "parent-only task".into(),
                }],
                ..Default::default()
            },
        )
        .unwrap();

    let parent_snapshot = parent.capability_snapshot.clone();
    let config = parent.config.clone();
    let child = SubAgentExecutor::new(parent, "sub-agent-session".into(), false, config)
        .await
        .unwrap();
    assert_eq!(
        child.child_ctx.vfs_scope.resource_session_id,
        "tenant-knowledge"
    );
    assert_eq!(
        child.child_ctx.vfs_scope.agent_session_id,
        "sub-agent-session"
    );
    assert!(Arc::ptr_eq(
        &child.child_ctx.capability_snapshot,
        &parent_snapshot
    ));
    assert_eq!(child.child_ctx.session_layout, SessionLayout::Isolated);
    assert!(
        child
            .child_ctx
            .home
            .ends_with("subagents/sub-agent-session")
    );
    assert!(child.child_store.lines().await.unwrap().is_empty());
    assert!(child.child_ctx.todo_store.snapshot().items.is_empty());
}

#[tokio::test]
async fn child_rejects_session_id_path_escape() {
    let parent = crate::regression::test_context_for_agent("sub-invalid-id")
        .await
        .unwrap();
    let config = parent.config.clone();
    let error = SubAgentExecutor::new(parent, "../escape".into(), false, config)
        .await
        .err()
        .expect("path-like session id must fail");
    assert!(error.to_string().contains("invalid sub-agent session id"));
}

#[tokio::test]
async fn concurrent_children_idempotently_create_shared_parent() -> anyhow::Result<()> {
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_DIR: AtomicU64 = AtomicU64::new(0);
    let root = std::env::temp_dir().join(format!(
        "mink-subagent-parent-race-{}-{}",
        std::process::id(),
        NEXT_DIR.fetch_add(1, Ordering::Relaxed)
    ));
    let parent = root.join("parent-session");
    tokio::fs::create_dir_all(&parent).await?;
    let barrier = Arc::new(tokio::sync::Barrier::new(8));
    let mut tasks = tokio::task::JoinSet::new();

    for index in 0..8 {
        let parent = parent.clone();
        let barrier = barrier.clone();
        tasks.spawn(async move {
            let child = parent.join("subagents").join(format!("child-{index}"));
            barrier.wait().await;
            prepare_child_home(&parent, &child, false).await?;
            anyhow::Ok(child)
        });
    }

    while let Some(result) = tasks.join_next().await {
        let child = result??;
        assert!(child.is_dir());
    }
    let metadata = tokio::fs::symlink_metadata(parent.join("subagents")).await?;
    assert!(metadata.is_dir());
    assert!(!metadata.file_type().is_symlink());
    tokio::fs::remove_dir_all(root).await?;
    Ok(())
}

#[tokio::test]
async fn fork_inherits_full_history_and_compacted_projection() -> anyhow::Result<()> {
    let summary_backend = Arc::new(MockLlmBackend::new(
            "summary-model",
            vec![vec![
                Ok(Event::Text(TextEvent {
                    content: "Task focus: fork\nLatest request: inherit\nProgress: compacted\nTool evidence: none\nReflections: none".into(),
                })),
                Ok(Event::Stop(StopEvent {
                    reason: "end_turn".into(),
                })),
            ]],
        ));
    let parent = crate::regression::test_context_for_agent_with_config_and_backend(
        "sub-fork-context-state",
        |config| {
            config.max_context_tokens = 64_000;
            config.context_reserve_tokens = 12_000;
            config.context_compact_tail_tokens = 4_000;
            config.context_compact_max_output_tokens = 2_048;
        },
        summary_backend,
    )
    .await?;
    for index in 0..3 {
        parent
            .store
            .add_user(&format!("request {index}: {}", "x".repeat(6_000)))
            .await?;
        parent
            .store
            .add_assistant(&format!("progress {index}: {}", "y".repeat(6_000)), "", &[])
            .await?;
    }
    let full_history = parent.store.lines().await?;
    let resolved = crate::config::model_resolver(&parent.config).resolve(&parent.config.model);
    assert!(
        parent
            .compaction
            .evaluate_and_compact(
                "manual",
                0,
                crate::llm::client::LlmModelTarget::new(
                    &resolved.actual,
                    resolved.alias.as_deref(),
                ),
            )
            .await?
            .0
    );
    let projected = parent.compaction.active_messages().await?;
    let parent_dir = parent.store.path().parent().unwrap().to_path_buf();
    let inherited_artifact =
        parent
            .artifacts
            .write_text("Bash", "parent output", "parent artifact content")?;
    tokio::fs::write(parent_dir.join("future-state.bin"), b"preserved").await?;
    tokio::fs::write(
        parent_dir.join("stats.json"),
        r#"{"total_input_tokens":999,"total_output_tokens":777}"#,
    )
    .await?;
    parent.todo_store.apply_structure(
        0,
        crate::session::todo::TodoChanges {
            add: vec![crate::session::todo::TodoAdd {
                content: "inherited task".into(),
            }],
            ..Default::default()
        },
    )?;
    let parent_todos = parent
        .todo_store
        .advance(
            1,
            crate::session::todo::TodoTransitions {
                activate: vec!["T0001".into()],
                ..Default::default()
            },
        )?
        .snapshot;

    let config = parent.config.clone();
    let child = SubAgentExecutor::new(parent, "sub-fork-state".into(), true, config).await?;
    assert_eq!(child.child_store.lines().await?, full_history);
    assert_eq!(
        child.child_ctx.compaction.active_messages().await?,
        projected
    );
    assert_eq!(child.child_ctx.todo_store.snapshot(), parent_todos);
    assert_eq!(
        tokio::fs::read(child.child_ctx.home.join("future-state.bin")).await?,
        b"preserved"
    );
    let usage = child.child_ctx.stats.snapshot().await;
    assert_eq!(usage.total_input_tokens, 0);
    assert_eq!(usage.total_output_tokens, 0);
    assert_eq!(
        child
            .child_ctx
            .artifacts
            .read_text(&inherited_artifact.id)?,
        "parent artifact content"
    );
    let child_artifact =
        child
            .child_ctx
            .artifacts
            .write_text("Bash", "child output", "child artifact content")?;
    assert_ne!(child_artifact.id, inherited_artifact.id);
    assert_eq!(
        child
            .child_ctx
            .artifacts
            .read_text(&inherited_artifact.id)?,
        "parent artifact content"
    );
    Ok(())
}
