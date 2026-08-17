use super::*;
use crate::llm::mock::MockLlmBackend;
use crate::protocol::{Event, StopEvent, TextEvent};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};

struct OverflowOnceClient {
    calls: AtomicUsize,
}

#[async_trait::async_trait]
impl LlmBackend for OverflowOnceClient {
    fn name(&self) -> &str {
        "overflow-once"
    }

    async fn stream(
        &self,
        _request: crate::llm::client::LlmRequest,
    ) -> Result<crate::llm::client::LlmResponseStream> {
        if self.calls.fetch_add(1, AtomicOrdering::SeqCst) == 0 {
            anyhow::bail!("HTTP 400: maximum context length exceeded");
        }
        Ok(crate::llm::client::LlmResponseStream {
            events: Box::pin(futures::stream::iter(vec![
                Ok(Event::Text(crate::protocol::TextEvent {
                    content: "recovered".into(),
                })),
                Ok(Event::Stop(crate::protocol::StopEvent {
                    reason: "stop".into(),
                })),
            ])),
            attempt_count: 1,
        })
    }
}

#[tokio::test]
async fn signal_recovery_decision_noops_when_signal_policy_is_off() {
    let ctx = crate::regression::test_context_for_agent("turn-signal-disabled")
        .await
        .unwrap();
    let llm = Arc::new(MockLlmBackend::new("flash", vec![]));
    let mut executor = TurnExecutor::new(ctx, llm);
    let mut belief = crate::agent::belief::BeliefTracker::new(16);
    belief.observe(&[crate::guard::collector::Signal {
        kind: crate::guard::collector::SignalKind::ToolFailed,
        severity: 1.0,
        source_tool: "Bash".into(),
        exit_code: Some(1),
        matched_pattern: None,
        message: "failed".into(),
    }]);

    let decision = executor
        .decide_signal_recovery(false, Some(&mut belief))
        .await
        .unwrap();
    assert!(decision.is_none());
}

#[tokio::test]
async fn abort_replan_success_clears_signal_recovery_guard() {
    let replan_child = vec![
        Ok(Event::Text(TextEvent {
            content: "Plan: re-read the failing module, add a unit test, then fix the root cause."
                .into(),
        })),
        Ok(Event::Stop(StopEvent {
            reason: "end_turn".into(),
        })),
    ];
    let llm = Arc::new(MockLlmBackend::new("flash", vec![replan_child]));
    let ctx = crate::regression::test_context_for_agent_with_config_and_backend(
        "abort-replan-guard-clear",
        |_| {},
        llm.clone(),
    )
    .await
    .unwrap();
    let mut executor = TurnExecutor::new(ctx, llm.clone());
    executor.local.signal_recovery_guard = true;
    executor.local.guard_bypassed = false;
    executor.signal_processor.evidence_mut().hard_failures = 8;
    let mut belief = crate::agent::belief::BeliefTracker::new(16);
    for _ in 0..8 {
        belief.observe(&[crate::guard::collector::Signal {
            kind: crate::guard::collector::SignalKind::ToolFailed,
            severity: 1.0,
            source_tool: "Bash".into(),
            exit_code: Some(1),
            matched_pattern: None,
            message: "failed".into(),
        }]);
    }

    let decision = executor
        .decide_signal_recovery(true, Some(&mut belief))
        .await
        .unwrap();
    assert!(decision.is_none());
    assert!(!executor.local.signal_recovery_guard);
    assert!(!executor.local.guard_bypassed);
}

#[tokio::test]
async fn todo_sync_is_appended_once_when_file_revision_is_ahead() -> anyhow::Result<()> {
    let ctx = crate::regression::test_context_for_agent("turn-todo-sync").await?;
    ctx.store.add_user("existing stable history").await?;
    let stable_prefix = ctx.compaction.active_messages().await?;
    ctx.todo_store.apply_structure(
        0,
        crate::session::todo::TodoChanges {
            add: vec![crate::session::todo::TodoAdd {
                content: "active".into(),
            }],
            ..Default::default()
        },
    )?;
    ctx.todo_store.advance(
        1,
        crate::session::todo::TodoTransitions {
            activate: vec!["T0001".into()],
            ..Default::default()
        },
    )?;
    let llm = Arc::new(MockLlmBackend::new("flash", vec![]));
    let executor = TurnExecutor::new(ctx.clone(), llm);
    let mut messages = ctx.compaction.active_messages().await?;

    assert!(executor.reconcile_todo_state(&mut messages).await?);
    assert!(!executor.reconcile_todo_state(&mut messages).await?);
    assert!(messages.starts_with(&stable_prefix));
    assert_eq!(crate::session::todo::visible_revision(&messages)?, 2);
    assert_eq!(
        ctx.store
            .lines()
            .await?
            .iter()
            .filter(|message| { message["_mink"]["todo_state_kind"].as_str() == Some("sync") })
            .count(),
        1
    );
    Ok(())
}

#[tokio::test]
async fn todo_final_guard_reminds_once_but_does_not_force_a_loop() -> anyhow::Result<()> {
    let ctx = crate::regression::test_context_for_agent("turn-todo-final-guard").await?;
    ctx.todo_store.apply_structure(
        0,
        crate::session::todo::TodoChanges {
            add: vec![crate::session::todo::TodoAdd {
                content: "active".into(),
            }],
            ..Default::default()
        },
    )?;
    ctx.todo_store.advance(
        1,
        crate::session::todo::TodoTransitions {
            activate: vec!["T0001".into()],
            ..Default::default()
        },
    )?;
    let llm = Arc::new(MockLlmBackend::new("flash", vec![]));
    let mut executor = TurnExecutor::new(ctx.clone(), llm);

    assert!(executor.decide_next("stop", None, false).await?.is_none());
    assert_eq!(
        executor.decide_next("stop", None, false).await?,
        Some(TurnDecision::Stop)
    );
    let messages = ctx.store.lines().await?;
    assert_eq!(
        messages
            .iter()
            .filter(|message| {
                message["content"]
                    .as_str()
                    .is_some_and(|content| content.contains("<todo-final-reminder>"))
            })
            .count(),
        1
    );
    Ok(())
}

#[tokio::test]
async fn todo_final_guard_on_last_turn_records_reminder_but_stops() -> anyhow::Result<()> {
    let ctx = crate::regression::test_context_for_agent("turn-todo-final-guard-last-turn").await?;
    ctx.todo_store.apply_structure(
        0,
        crate::session::todo::TodoChanges {
            add: vec![crate::session::todo::TodoAdd {
                content: "active".into(),
            }],
            ..Default::default()
        },
    )?;
    ctx.todo_store.advance(
        1,
        crate::session::todo::TodoTransitions {
            activate: vec!["T0001".into()],
            ..Default::default()
        },
    )?;
    let llm = Arc::new(MockLlmBackend::new("flash", vec![]));
    let mut executor = TurnExecutor::new(ctx.clone(), llm);

    assert_eq!(
        executor.decide_next("stop", None, true).await?,
        Some(TurnDecision::Stop)
    );
    let messages = ctx.store.lines().await?;
    assert_eq!(
        messages
            .iter()
            .filter(|message| {
                message["content"]
                    .as_str()
                    .is_some_and(|content| content.contains("<todo-final-reminder>"))
            })
            .count(),
        1
    );
    Ok(())
}

#[tokio::test]
async fn todo_progress_guard_appends_at_most_one_reminder_per_turn() -> anyhow::Result<()> {
    let ctx = crate::regression::test_context_for_agent("turn-todo-progress-guard").await?;
    ctx.todo_store.apply_structure(
        0,
        crate::session::todo::TodoChanges {
            add: vec![crate::session::todo::TodoAdd {
                content: "active".into(),
            }],
            ..Default::default()
        },
    )?;
    ctx.todo_store.advance(
        1,
        crate::session::todo::TodoTransitions {
            activate: vec!["T0001".into()],
            ..Default::default()
        },
    )?;
    let llm = Arc::new(MockLlmBackend::new("flash", vec![]));
    let mut executor = TurnExecutor::new(ctx.clone(), llm);
    executor.local.successful_work_calls_since_todo_advance = 8;

    executor.maybe_append_todo_progress_reminder().await?;
    executor.maybe_append_todo_progress_reminder().await?;
    let messages = ctx.store.lines().await?;
    assert_eq!(messages.len(), 1);
    assert!(
        messages[0]["content"]
            .as_str()
            .unwrap()
            .contains("<todo-progress-reminder>")
    );
    Ok(())
}

#[tokio::test]
async fn context_overflow_compacts_and_retries_only_once() -> anyhow::Result<()> {
    let summary_backend = Arc::new(MockLlmBackend::new(
            "summary-model",
            vec![vec![
                Ok(Event::Text(crate::protocol::TextEvent {
                    content: "Task focus: recover\nLatest request: continue\nProgress: compacted\nTool evidence: none\nReflections: none".into(),
                })),
                Ok(Event::Stop(crate::protocol::StopEvent {
                    reason: "end_turn".into(),
                })),
            ]],
        ));
    let ctx = crate::regression::test_context_for_agent_with_config_and_backend(
        "turn-overflow-recovery",
        |config| {
            config.max_context_tokens = 64_000;
            config.context_reserve_tokens = 12_000;
            config.context_compact_tail_tokens = 1_000;
            config.context_compact_max_output_tokens = 2_048;
        },
        summary_backend,
    )
    .await?;
    for index in 0..3 {
        ctx.store
            .add_user(&format!("old request {index}: {}", "x".repeat(1_000)))
            .await?;
        ctx.store
            .add_assistant(
                &format!("old response {index}: {}", "y".repeat(1_000)),
                "",
                &[],
            )
            .await?;
    }
    let llm = Arc::new(OverflowOnceClient {
        calls: AtomicUsize::new(0),
    });
    let mut executor = TurnExecutor::new(ctx.clone(), llm.clone());

    let (decision, _) = executor.execute("continue", None).await?;

    assert_eq!(decision, TurnDecision::Stop);
    assert_eq!(llm.calls.load(AtomicOrdering::SeqCst), 2);
    assert_eq!(ctx.store.lines().await?.len(), 8);
    assert!(ctx.compaction.current_summary()?.is_some());
    Ok(())
}

#[test]
fn context_overflow_classifier_is_specific() {
    assert!(is_context_overflow_message(
        "HTTP 400: context_length_exceeded"
    ));
    assert!(is_context_overflow_message(
        "This model's maximum context length is 65536 tokens"
    ));
    assert!(!is_context_overflow_message(
        "HTTP 400: invalid tool schema"
    ));
    assert!(!is_context_overflow_message("request timed out"));
}

#[tokio::test]
async fn context_overflow_after_visible_output_is_not_recoverable() -> anyhow::Result<()> {
    let ctx = crate::regression::test_context_for_agent("turn-partial-overflow").await?;
    let backend = Arc::new(MockLlmBackend::new(
        "flash",
        vec![vec![
            Ok(Event::Text(crate::protocol::TextEvent {
                content: "partial".into(),
            })),
            Ok(Event::Retry(crate::protocol::RetryEvent {})),
            Ok(Event::Error(crate::protocol::ErrorEvent {
                message: "maximum context length exceeded".into(),
            })),
        ]],
    ));
    let mut executor = TurnExecutor::new(ctx, backend);

    let error = match executor.stream_llm_response(&[], "", &[], 0).await {
        Ok(_) => panic!("overflow after partial output should fail"),
        Err(error) => error,
    };

    assert!(error.downcast_ref::<ContextOverflowError>().is_none());
    Ok(())
}
