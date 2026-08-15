use super::*;

#[tokio::test]
async fn orchestrator_user_input_runs_turn_and_logs_tracking() -> anyhow::Result<()> {
    let h = harness("orch-user-input").await?;
    let llm = Arc::new(MockLlmBackend::new(
        "flash",
        vec![vec![
            Ok(Event::Text(TextEvent {
                content: "hello".into(),
            })),
            Ok(Event::Stop(StopEvent {
                reason: "end_turn".into(),
            })),
        ]],
    ));
    run_orchestrator_user_input(h.ctx.clone(), llm, "say hi").await?;
    let lines = h.ctx.store.lines().await?;
    assert_eq!(lines.len(), 2);
    assert_eq!(lines[0]["role"], "user");
    assert_eq!(lines[1]["role"], "assistant");
    let events = tokio::fs::read_to_string(&h.ctx.events_path).await?;
    assert!(events.contains(r#""type":"turn_start""#), "{events}");
    assert!(events.contains(r#""type":"turn_tracking""#), "{events}");
    assert!(events.contains(r#""type":"turn_final""#), "{events}");
    assert!(events.contains(r#""status":"ok""#), "{events}");
    assert!(events.contains(r#""decision":"Stop""#), "{events}");
    Ok(())
}

#[tokio::test]
async fn orchestrator_model_command_updates_display() -> anyhow::Result<()> {
    let h = harness("orch-model-command").await?;
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    let actor = OrchActor::new(h.ctx.clone(), rx);
    let handle = tokio::spawn(actor.run());
    tx.send(OrchCmd::SetModel("pro".into()))?;
    tx.send(OrchCmd::SetModel("unknown".into()))?;
    drop(tx);
    handle.await??;
    assert!(
        h.display
            .info
            .lock()
            .unwrap()
            .iter()
            .any(|msg| msg == "Switched to pro model."),
        "{:?}",
        h.display.info.lock().unwrap()
    );
    assert!(
        h.display
            .title_models
            .lock()
            .unwrap()
            .iter()
            .any(|model| model == "pro"),
        "{:?}",
        h.display.title_models.lock().unwrap()
    );
    assert!(
        h.display
            .info
            .lock()
            .unwrap()
            .iter()
            .any(|msg| msg == "Switched to unknown model."),
        "{:?}",
        h.display.info.lock().unwrap()
    );
    assert!(
        h.display
            .title_models
            .lock()
            .unwrap()
            .iter()
            .any(|model| model == "unknown"),
        "{:?}",
        h.display.title_models.lock().unwrap()
    );
    Ok(())
}

#[tokio::test]
async fn orchestrator_forced_model_title_survives_turn_refreshes() -> anyhow::Result<()> {
    let h = harness("orch-forced-model-title").await?;
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    let llm = Arc::new(MockLlmBackend::new(
        "pro",
        vec![vec![
            Ok(Event::Text(TextEvent {
                content: "done".into(),
            })),
            Ok(Event::Stop(StopEvent {
                reason: "end_turn".into(),
            })),
        ]],
    ));
    let actor = OrchActor::new(test_context_with_llm_backend(h.ctx.clone(), llm), rx);
    let handle = tokio::spawn(actor.run());
    tx.send(OrchCmd::SetModel("pro".into()))?;
    let (done_tx, done_rx) = tokio::sync::oneshot::channel();
    tx.send(OrchCmd::UserInput {
        input: "say hi".into(),
        done: done_tx,
    })?;
    let result = done_rx.await?;
    assert_eq!(result.status, crate::agent::orchestrator::TurnStatus::Ok);
    drop(tx);
    handle.await??;

    let title_models = h.display.title_models.lock().unwrap();
    assert!(
        !title_models.iter().any(|model| model == "flash"),
        "{title_models:?}"
    );
    assert!(
        title_models.iter().filter(|model| *model == "pro").count() >= 2,
        "{title_models:?}"
    );
    Ok(())
}

#[tokio::test]
async fn orchestrator_active_model_is_used_by_spawned_sub_agent() -> anyhow::Result<()> {
    let requests = Arc::new(Mutex::new(Vec::new()));
    let backend = Arc::new(ActiveModelRoutingBackend {
        requests: requests.clone(),
        agent_request_count: AtomicU64::new(0),
    });
    let h = harness_with_config(
        "orch-sub-agent-active-model",
        false,
        300,
        |_| {},
        Some(backend),
    )
    .await?;
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    let actor = OrchActor::new(h.ctx.clone(), rx);
    let handle = tokio::spawn(actor.run());
    tx.send(OrchCmd::SetModel("pro".into()))?;
    let (done_tx, done_rx) = tokio::sync::oneshot::channel();
    tx.send(OrchCmd::UserInput {
        input: "delegate this task".into(),
        done: done_tx,
    })?;
    let outcome = done_rx.await?;
    drop(tx);
    handle.await??;

    assert_eq!(outcome.status, crate::agent::orchestrator::TurnStatus::Ok);
    assert_eq!(
        requests.lock().unwrap().as_slice(),
        &[
            CapturedRoutedRequest {
                purpose: "agent",
                model: "deepseek-v4-pro".into(),
                alias: Some("pro".into()),
            },
            CapturedRoutedRequest {
                purpose: "sub_agent",
                model: "deepseek-v4-pro".into(),
                alias: Some("pro".into()),
            },
            CapturedRoutedRequest {
                purpose: "agent",
                model: "deepseek-v4-pro".into(),
                alias: Some("pro".into()),
            },
        ]
    );
    Ok(())
}

#[tokio::test]
async fn orchestrator_flash_command_resets_forced_model_display() -> anyhow::Result<()> {
    let h = harness("orch-flash-command").await?;
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    let actor = OrchActor::new(h.ctx.clone(), rx);
    let handle = tokio::spawn(actor.run());
    tx.send(OrchCmd::SetModel("pro".into()))?;
    tx.send(OrchCmd::SetModel("flash".into()))?;
    drop(tx);
    handle.await??;
    let info = h.display.info.lock().unwrap();
    assert!(
        info.iter().any(|msg| msg == "Switched to flash model."),
        "{info:?}"
    );
    Ok(())
}

#[tokio::test]
async fn orchestrator_renders_failed_turn_decision() -> anyhow::Result<()> {
    let h = harness("orch-failed-turn").await?;
    let llm = Arc::new(MockLlmBackend::new(
        "flash",
        vec![vec![Ok(Event::Stop(StopEvent {
            reason: "max_tokens".into(),
        }))]],
    ));
    run_orchestrator_user_input(h.ctx.clone(), llm, "hit limit").await?;
    assert!(
        h.display
            .info
            .lock()
            .unwrap()
            .iter()
            .any(|msg| msg == "error:stop: max_tokens"),
        "{:?}",
        h.display.info.lock().unwrap()
    );
    Ok(())
}

#[tokio::test]
async fn orchestrator_logs_stream_error_from_turn() -> anyhow::Result<()> {
    let h = harness("orch-stream-error").await?;
    let llm = Arc::new(MockLlmBackend::new(
        "flash",
        vec![vec![Err(anyhow::anyhow!("stream connection timeout"))]],
    ));
    run_orchestrator_user_input(h.ctx.clone(), llm, "fail stream").await?;
    assert!(
        h.display
            .info
            .lock()
            .unwrap()
            .iter()
            .any(|msg| msg == "error:Turn execution error: stream connection timeout"),
        "{:?}",
        h.display.info.lock().unwrap()
    );
    let events = tokio::fs::read_to_string(&h.ctx.events_path).await?;
    assert!(events.contains(r#""type":"turn_error""#), "{events}");
    assert!(events.contains(r#""category":"Network""#), "{events}");
    Ok(())
}

#[tokio::test]
async fn orchestrator_cancel_signal_shuts_actor_down() -> anyhow::Result<()> {
    let h = harness("orch-cancel").await?;
    let (_tx, rx) = tokio::sync::mpsc::unbounded_channel();
    let actor = OrchActor::new(h.ctx.clone(), rx);
    let handle = tokio::spawn(actor.run());
    h.ctx.cancel.cancel();
    handle.await??;
    assert!(
        h.display
            .info
            .lock()
            .unwrap()
            .iter()
            .any(|msg| msg == "Shutting down..."),
        "{:?}",
        h.display.info.lock().unwrap()
    );
    Ok(())
}

#[tokio::test]
async fn orchestrator_manual_compact_empty_session_reports_skip() -> anyhow::Result<()> {
    let h = harness("orch-compact-empty").await?;
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    let actor = OrchActor::new(h.ctx.clone(), rx);
    let handle = tokio::spawn(actor.run());
    let (done_tx, done_rx) = tokio::sync::oneshot::channel();
    tx.send(OrchCmd::Compact { done: done_tx })?;
    done_rx.await??;
    drop(tx);
    handle.await??;
    let info = h.display.info.lock().unwrap();
    assert!(info.iter().any(|msg| msg == "Compressing..."), "{info:?}");
    assert!(
        info.iter().any(|msg| msg == "Compact skipped: empty"),
        "{info:?}"
    );
    Ok(())
}

#[tokio::test]
async fn orchestrator_manual_compact_uses_active_model_and_shared_backend() -> anyhow::Result<()> {
    let requests = Arc::new(Mutex::new(Vec::new()));
    let backend = Arc::new(RecordingCompactionBackend {
        requests: requests.clone(),
    });
    let h = harness_with_config(
        "orch-compact-active-model",
        false,
        300,
        |config| config.context_compact_tail_tokens = 1,
        Some(backend),
    )
    .await?;
    for index in 0..3 {
        h.ctx
            .store
            .add_user(&format!("user history {index}: {}", "x".repeat(256)))
            .await?;
        h.ctx
            .store
            .add_assistant(&format!("assistant history {index}"), "", &[])
            .await?;
    }

    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    let actor = OrchActor::new(h.ctx.clone(), rx);
    let handle = tokio::spawn(actor.run());
    tx.send(OrchCmd::SetModel("pro".into()))?;
    let (done_tx, done_rx) = tokio::sync::oneshot::channel();
    tx.send(OrchCmd::Compact { done: done_tx })?;
    done_rx.await??;
    drop(tx);
    handle.await??;

    assert_eq!(
        requests.lock().unwrap().as_slice(),
        &[CapturedModelTarget {
            model: "deepseek-v4-pro".into(),
            alias: Some("pro".into()),
        }]
    );
    Ok(())
}

#[tokio::test]
async fn plan_confirm_and_clear_preserve_immutable_prefix() -> anyhow::Result<()> {
    let h = harness("plan-actions").await?;
    let prefix = PrefixManager::new(h.ctx.clone());
    let (stable_prompt, stable_tools) = prefix.ensure()?;
    let stable_fingerprint = h
        .ctx
        .immutable_prefix
        .lock()
        .unwrap()
        .as_ref()
        .unwrap()
        .fingerprint()
        .to_string();
    assert!(!stable_prompt.contains("<current-plan>"));

    let handler = PlanActionHandler;
    let tool_ctx = crate::context::ToolContext::from(h.ctx.as_ref());
    let mut effects = Vec::new();
    let draft_outcome = crate::tools::runner::ToolExec::execute(
        &crate::tools::plan::PlanDraftTool,
        &serde_json::json!({"content": "1. ship it\n"}),
        &tool_ctx,
    )?;
    let mut draft = plan_result("PlanDraft", draft_outcome);
    assert_eq!(handler.handle(&mut draft, &mut effects), None);
    assert_eq!(draft.content, "Plan draft saved.");
    assert_eq!(
        tokio::fs::read_to_string(&h.ctx.plan_draft_path).await?,
        "1. ship it\n"
    );
    assert!(effects.is_empty());
    assert!(h.ctx.immutable_prefix.lock().unwrap().is_some());

    let confirm_outcome = crate::tools::runner::ToolExec::execute(
        &crate::tools::plan::PlanConfirmTool,
        &serde_json::json!({}),
        &tool_ctx,
    )?;
    let mut confirm = plan_result("PlanConfirm", confirm_outcome);
    assert_eq!(
        handler.handle(&mut confirm, &mut effects),
        Some("plan_confirm")
    );
    assert_eq!(confirm.content, "Plan confirmed and locked in.");
    assert_eq!(
        tokio::fs::read_to_string(&h.ctx.plan_path).await?,
        "1. ship it\n"
    );
    assert!(!h.ctx.plan_draft_path.exists());
    assert!(matches!(effects.as_slice(), [TurnEffect::PlanConfirmed]));
    let (after_confirm_prompt, after_confirm_tools) = prefix.ensure()?;
    assert_eq!(after_confirm_prompt, stable_prompt);
    assert_eq!(after_confirm_tools, stable_tools);
    assert_eq!(
        h.ctx
            .immutable_prefix
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .fingerprint(),
        stable_fingerprint
    );

    let clear_outcome = crate::tools::runner::ToolExec::execute(
        &crate::tools::plan::PlanClearTool,
        &serde_json::json!({}),
        &tool_ctx,
    )?;
    let mut clear = plan_result("PlanClear", clear_outcome);
    assert_eq!(handler.handle(&mut clear, &mut effects), Some("plan_clear"));
    assert_eq!(clear.content, "Plan cleared.");
    assert!(!h.ctx.plan_path.exists());
    assert!(matches!(
        effects.as_slice(),
        [TurnEffect::PlanConfirmed, TurnEffect::PlanCleared]
    ));
    let (after_clear_prompt, after_clear_tools) = prefix.ensure()?;
    assert_eq!(after_clear_prompt, stable_prompt);
    assert_eq!(after_clear_tools, stable_tools);
    Ok(())
}

#[tokio::test]
async fn plan_compaction_obeys_the_existing_single_turn_guard() -> anyhow::Result<()> {
    let requests = Arc::new(Mutex::new(Vec::new()));
    let backend = Arc::new(RecordingCompactionBackend {
        requests: requests.clone(),
    });
    let h = harness_with_config(
        "plan-single-compaction",
        false,
        300,
        |config| {
            config.context_compact_pct = 1;
            config.context_compact_tail_tokens = 1;
        },
        Some(backend),
    )
    .await?;
    for index in 0..4 {
        h.ctx
            .store
            .add_user(&format!("history {index}: {}", "x".repeat(12_000)))
            .await?;
        h.ctx
            .store
            .add_assistant(&format!("history response {index}"), "", &[])
            .await?;
    }
    tokio::fs::write(&h.ctx.plan_draft_path, "1. execute\n").await?;

    let llm = Arc::new(MockLlmBackend::new(
        "flash",
        vec![
            vec![
                Ok(Event::ToolCall(tool_call(
                    "PlanConfirm",
                    "call_plan_confirm",
                    json!({}),
                ))),
                Ok(Event::Stop(StopEvent {
                    reason: "tool_use".into(),
                })),
            ],
            vec![Ok(Event::Stop(StopEvent {
                reason: "end_turn".into(),
            }))],
        ],
    ));
    let mut executor = TurnExecutor::new(h.ctx.clone(), llm_backend_from_mock(llm));
    let (decision, effects) = executor.execute("confirm the plan", None).await?;

    assert_eq!(decision, TurnDecision::Stop);
    assert!(matches!(effects.as_slice(), [TurnEffect::PlanConfirmed]));
    assert_eq!(requests.lock().unwrap().len(), 1);
    assert!(h.ctx.plan_path.exists());
    assert!(!h.ctx.plan_draft_path.exists());
    Ok(())
}

#[tokio::test]
async fn plan_compaction_failure_is_propagated_from_the_turn() -> anyhow::Result<()> {
    let h = harness_with_config(
        "plan-compaction-error",
        false,
        300,
        |config| {
            config.context_compact_pct = 100;
            config.context_compact_tail_tokens = 1;
        },
        Some(Arc::new(FailingCompactionBackend)),
    )
    .await?;
    for index in 0..3 {
        h.ctx.store.add_user(&format!("history {index}")).await?;
        h.ctx
            .store
            .add_assistant(&format!("history response {index}"), "", &[])
            .await?;
    }
    tokio::fs::write(&h.ctx.plan_draft_path, "1. execute\n").await?;

    let llm = Arc::new(MockLlmBackend::new(
        "flash",
        vec![vec![
            Ok(Event::ToolCall(tool_call(
                "PlanConfirm",
                "call_plan_confirm",
                json!({}),
            ))),
            Ok(Event::Stop(StopEvent {
                reason: "tool_use".into(),
            })),
        ]],
    ));
    let mut executor = TurnExecutor::new(h.ctx.clone(), llm_backend_from_mock(llm));
    let error = executor
        .execute("confirm the plan", None)
        .await
        .unwrap_err()
        .to_string();

    assert!(error.contains("planned compaction failure"), "{error}");
    assert!(h.ctx.plan_path.exists());
    assert!(!h.ctx.plan_draft_path.exists());
    Ok(())
}

#[tokio::test]
async fn plan_confirm_without_draft_returns_error_result() -> anyhow::Result<()> {
    let h = harness("plan-empty").await?;
    let tool_ctx = crate::context::ToolContext::from(h.ctx.as_ref());
    let error = crate::tools::runner::ToolExec::execute(
        &crate::tools::plan::PlanConfirmTool,
        &serde_json::json!({}),
        &tool_ctx,
    )
    .unwrap_err();
    assert_eq!(error.to_string(), "no plan draft found to confirm");
    Ok(())
}

#[tokio::test]
async fn plan_draft_empty_content_cancels_and_reports_cancellation() -> anyhow::Result<()> {
    let h = harness("plan-draft-cancel").await?;
    let tool_ctx = crate::context::ToolContext::from(h.ctx.as_ref());
    crate::tools::runner::ToolExec::execute(
        &crate::tools::plan::PlanDraftTool,
        &serde_json::json!({"content": "1. inspect\n"}),
        &tool_ctx,
    )?;
    assert!(h.ctx.plan_draft_path.exists());

    let outcome = crate::tools::runner::ToolExec::execute(
        &crate::tools::plan::PlanDraftTool,
        &serde_json::json!({"content": ""}),
        &tool_ctx,
    )?;

    assert_eq!(outcome.content, "Plan draft cancelled.");
    assert!(matches!(
        outcome.presentation,
        Some(crate::ui::ToolPresentation::Plan(crate::ui::PlanDisplay {
            transition: crate::ui::PlanTransitionDisplay::DraftCancelled,
            content: None,
        }))
    ));
    assert!(!h.ctx.plan_draft_path.exists());
    Ok(())
}

#[tokio::test]
async fn todo_tools_persist_incremental_state_and_reject_stale_writes() -> anyhow::Result<()> {
    let h = harness("todo-persistence").await?;
    let prefix = PrefixManager::new(h.ctx.clone());
    let stable_prefix = prefix.ensure()?;
    assert!(!stable_prefix.0.contains("<current-todos"));
    let tool_ctx = crate::context::ToolContext::from(h.ctx.as_ref());
    let created = crate::tools::runner::ToolExec::execute(
        &crate::tools::todo::TodoWriteTool,
        &json!({
            "base_revision": 0,
            "add": [
                {"content": "inspect"},
                {"content": "implement"},
                {"content": "verify"}
            ]
        }),
        &tool_ctx,
    )?;
    assert!(created.content.contains("revision=\"1\""));
    assert!(created.content.contains("T0001"));
    assert!(created.content.contains("T0002"));
    assert!(matches!(
        created.presentation,
        Some(crate::ui::ToolPresentation::Todo(crate::ui::TodoDisplay {
            revision: 1,
            ..
        }))
    ));

    crate::tools::runner::ToolExec::execute(
        &crate::tools::todo::TodoAdvanceTool,
        &json!({
            "base_revision": 1,
            "activate": ["T0001", "T0002"]
        }),
        &tool_ctx,
    )?;
    let read = crate::tools::runner::ToolExec::execute(
        &crate::tools::todo::TodoReadTool,
        &json!({}),
        &tool_ctx,
    )?;
    assert!(read.content.contains("in_progress=\"2\""));
    assert!(read.content.contains("T0003: verify"));
    assert!(matches!(
        read.presentation,
        Some(crate::ui::ToolPresentation::Todo(crate::ui::TodoDisplay {
            revision: 2,
            ..
        }))
    ));

    crate::tools::runner::ToolExec::execute(
        &crate::tools::todo::TodoWriteTool,
        &json!({
            "base_revision": 2,
            "update": [
                {"id": "T0003", "content": "run focused tests"}
            ]
        }),
        &tool_ctx,
    )?;
    crate::tools::runner::ToolExec::execute(
        &crate::tools::todo::TodoAdvanceTool,
        &json!({
            "base_revision": 3,
            "complete": ["T0001"]
        }),
        &tool_ctx,
    )?;
    let before = h.ctx.todo_store.snapshot();
    let error = crate::tools::runner::ToolExec::execute(
        &crate::tools::todo::TodoWriteTool,
        &json!({"base_revision": 3, "remove": ["T0002"]}),
        &tool_ctx,
    )
    .unwrap_err()
    .to_string();
    assert!(error.contains("stale todo revision"), "{error}");
    assert_eq!(h.ctx.todo_store.snapshot(), before);
    let todo_path = crate::session::paths::paths_for(&h.ctx.home, &h.ctx.cwd, "regression").todos;
    let reloaded = crate::session::todo::TodoStore::load(todo_path)?;
    assert_eq!(reloaded.snapshot(), before);
    assert_eq!(reloaded.snapshot().revision, 4);
    assert_eq!(prefix.ensure()?, stable_prefix);
    Ok(())
}
