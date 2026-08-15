use super::*;

#[tokio::test]
async fn full_turn_tool_loop_preserves_conversation_order() -> anyhow::Result<()> {
    let h = harness("turn-loop").await?;
    tokio::fs::write(h.cwd.join("fixture.txt"), "alpha\nbeta\n").await?;
    let llm = Arc::new(MockLlmBackend::new(
        "flash",
        vec![
            vec![
                Ok(Event::ToolCall(tool_call(
                    "Read",
                    "call_read",
                    json!({"path":"fixture.txt"}),
                ))),
                Ok(Event::Stop(StopEvent {
                    reason: "tool_use".into(),
                })),
            ],
            vec![
                Ok(Event::Text(TextEvent {
                    content: "done".into(),
                })),
                Ok(Event::Stop(StopEvent {
                    reason: "end_turn".into(),
                })),
            ],
        ],
    ));
    let mut executor = TurnExecutor::new(h.ctx.clone(), llm_backend_from_mock(llm));
    let (decision, effects) = executor.execute("read fixture", None).await?;

    assert_eq!(decision, TurnDecision::Stop);
    assert!(effects.is_empty());
    assert_eq!(executor.tool_call_count(), 1);
    let lines = h.ctx.store.lines().await?;
    assert_eq!(lines.len(), 4);
    assert_eq!(lines[0]["role"], "user");
    assert_eq!(lines[1]["role"], "assistant");
    assert_eq!(lines[1]["content"][0]["type"], "thinking");
    assert_eq!(lines[1]["content"][2]["type"], "tool_use");
    assert_eq!(lines[2]["role"], "user");
    assert_eq!(lines[2]["content"][0]["type"], "tool_result");
    assert_eq!(lines[3]["role"], "assistant");
    assert_eq!(lines[3]["content"][1]["text"], "done");
    Ok(())
}

#[tokio::test]
async fn turn_retry_thinking_usage_and_stop_are_persisted() -> anyhow::Result<()> {
    let h = harness("turn-retry-usage").await?;
    let llm = Arc::new(MockLlmBackend::new(
        "flash",
        vec![vec![
            Ok(Event::Text(TextEvent {
                content: "stale".into(),
            })),
            Ok(Event::Retry(RetryEvent {})),
            Ok(Event::Thinking(ThinkingEvent {
                content: "think".into(),
            })),
            Ok(Event::Text(TextEvent {
                content: "final".into(),
            })),
            Ok(Event::Usage(UsageEvent {
                input_tokens: 11,
                output_tokens: 7,
                cache_read_input_tokens: 3,
                cache_creation_input_tokens: 2,
            })),
            Ok(Event::Stop(StopEvent {
                reason: "stop".into(),
            })),
        ]],
    ));
    let mut executor = TurnExecutor::new(h.ctx.clone(), llm_backend_from_mock(llm));
    let (decision, effects) = executor.execute("retry once", None).await?;

    assert_eq!(decision, TurnDecision::Stop);
    assert!(effects.is_empty());
    let lines = h.ctx.store.lines().await?;
    assert_eq!(lines[1]["content"][0]["thinking"], "think");
    assert_eq!(lines[1]["content"][1]["text"], "final");
    assert!(!serde_json::to_string(&lines[1])?.contains("stale"));
    let stats = h.ctx.stats.snapshot().await;
    assert_eq!(stats.total_input_tokens, 11);
    assert_eq!(stats.total_output_tokens, 7);
    assert_eq!(stats.total_cache_read_tokens, 3);
    assert_eq!(stats.total_cache_creation_tokens, 2);
    Ok(())
}

#[tokio::test]
async fn turn_error_event_returns_error_and_logs_event() -> anyhow::Result<()> {
    let h = harness("turn-error-event").await?;
    let llm = Arc::new(MockLlmBackend::new(
        "flash",
        vec![vec![Ok(Event::Error(ErrorEvent {
            message: "model error".into(),
        }))]],
    ));
    let mut executor = TurnExecutor::new(h.ctx.clone(), llm_backend_from_mock(llm));
    let err = executor
        .execute("trigger model error", None)
        .await
        .unwrap_err()
        .to_string();

    assert_eq!(err, "model error");
    let events = tokio::fs::read_to_string(&h.ctx.events_path).await?;
    assert!(events.contains(r#""type":"error""#), "{events}");
    assert!(events.contains(r#""message":"model error""#), "{events}");
    Ok(())
}

#[tokio::test]
async fn turn_cancel_after_stream_returns_interrupted_without_assistant() -> anyhow::Result<()> {
    let h = harness("turn-cancel-after-stream").await?;
    h.ctx.cancel.cancel();
    let llm = Arc::new(MockLlmBackend::new(
        "flash",
        vec![vec![
            Ok(Event::Text(TextEvent {
                content: "not persisted".into(),
            })),
            Ok(Event::Stop(StopEvent {
                reason: "end_turn".into(),
            })),
        ]],
    ));
    let mut executor = TurnExecutor::new(h.ctx.clone(), llm_backend_from_mock(llm));
    let (decision, effects) = executor.execute("cancel now", None).await?;

    assert_eq!(decision, TurnDecision::Interrupted);
    assert!(effects.is_empty());
    let lines = h.ctx.store.lines().await?;
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0]["role"], "user");
    Ok(())
}

#[tokio::test]
async fn turn_scavenges_text_tool_call_and_executes_it() -> anyhow::Result<()> {
    let h = harness("turn-scavenge-tool").await?;
    tokio::fs::write(h.cwd.join("scavenge.txt"), "found\n").await?;
    let llm = Arc::new(MockLlmBackend::new(
        "flash",
        vec![
            vec![
                Ok(Event::Text(TextEvent {
                    content:
                        r#"<tool_call>{"name":"Read","arguments":{"path":"scavenge.txt"}}</tool_call>"#
                            .into(),
                })),
                Ok(Event::Stop(StopEvent {
                    reason: "tool_use".into(),
                })),
            ],
            vec![
                Ok(Event::Text(TextEvent {
                    content: "done".into(),
                })),
                Ok(Event::Stop(StopEvent {
                    reason: "end_turn".into(),
                })),
            ],
        ],
    ));
    let mut executor = TurnExecutor::new(h.ctx.clone(), llm_backend_from_mock(llm));
    let (decision, effects) = executor.execute("recover tool call", None).await?;

    assert_eq!(decision, TurnDecision::Stop);
    assert!(effects.is_empty());
    assert_eq!(executor.tool_call_count(), 1);
    let lines = h.ctx.store.lines().await?;
    assert!(
        lines[1]["content"]
            .as_array()
            .unwrap()
            .iter()
            .any(|block| block["type"] == "tool_use"),
        "{}",
        lines[1]
    );
    assert_eq!(lines[2]["content"][0]["type"], "tool_result");
    assert!(
        lines[2]["content"][0]["content"]
            .as_str()
            .unwrap()
            .contains("found")
    );
    let events = tokio::fs::read_to_string(&h.ctx.events_path).await?;
    assert!(events.contains(r#""type":"scavenge""#), "{events}");
    Ok(())
}

#[tokio::test]
async fn turn_scavenged_tool_call_after_end_turn_continues_loop() -> anyhow::Result<()> {
    let h = harness("turn-scavenge-end-turn").await?;
    tokio::fs::write(h.cwd.join("scavenge-end.txt"), "found\n").await?;
    let llm = Arc::new(MockLlmBackend::new(
        "flash",
        vec![
            vec![
                Ok(Event::Text(TextEvent {
                    content:
                        r#"<tool_call>{"name":"Read","arguments":{"path":"scavenge-end.txt"}}</tool_call>"#
                            .into(),
                })),
                Ok(Event::Stop(StopEvent {
                    reason: "end_turn".into(),
                })),
            ],
            vec![
                Ok(Event::Text(TextEvent {
                    content: "done".into(),
                })),
                Ok(Event::Stop(StopEvent {
                    reason: "end_turn".into(),
                })),
            ],
        ],
    ));
    let mut executor = TurnExecutor::new(h.ctx.clone(), llm_backend_from_mock(llm));
    let (decision, effects) = executor.execute("recover after end_turn", None).await?;

    assert_eq!(decision, TurnDecision::Stop);
    assert!(effects.is_empty());
    assert_eq!(executor.tool_call_count(), 1);
    let lines = h.ctx.store.lines().await?;
    assert_eq!(lines.len(), 4);
    assert_eq!(lines[2]["content"][0]["type"], "tool_result");
    assert_eq!(lines[3]["content"][1]["text"], "done");
    Ok(())
}

#[tokio::test]
async fn turn_stream_without_stop_event_fails_without_assistant_message() -> anyhow::Result<()> {
    let h = harness("turn-missing-stop").await?;
    let llm = Arc::new(MockLlmBackend::new(
        "flash",
        vec![vec![Ok(Event::Text(TextEvent {
            content: "partial".into(),
        }))]],
    ));
    let mut executor = TurnExecutor::new(h.ctx.clone(), llm_backend_from_mock(llm));
    let err = executor
        .execute("missing stop", None)
        .await
        .unwrap_err()
        .to_string();

    assert!(err.contains("stream ended without stop event"), "{err}");
    let lines = h.ctx.store.lines().await?;
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0]["role"], "user");
    Ok(())
}

#[tokio::test]
async fn turn_llm_first_event_timeout_fails_with_clear_error() -> anyhow::Result<()> {
    let h = harness_with_config(
        "turn-first-event-timeout",
        false,
        300,
        |cfg| {
            cfg.llm_first_event_timeout_secs = 1;
            cfg.llm_idle_timeout_secs = 10;
            cfg.llm_wait_heartbeat_secs = 0;
        },
        None,
    )
    .await?;
    let mut executor = TurnExecutor::new(h.ctx.clone(), Arc::new(PendingLlmBackend));
    let err = executor
        .execute("model never starts", None)
        .await
        .unwrap_err()
        .to_string();

    assert!(err.contains("first event timeout"), "{err}");
    let events = tokio::fs::read_to_string(&h.ctx.events_path).await?;
    assert!(
        events.contains(r#""category":"llm_first_event_timeout""#),
        "{events}"
    );
    Ok(())
}

#[tokio::test]
async fn turn_llm_idle_timeout_fails_after_partial_stream() -> anyhow::Result<()> {
    let h = harness_with_config(
        "turn-idle-timeout",
        false,
        300,
        |cfg| {
            cfg.llm_first_event_timeout_secs = 10;
            cfg.llm_idle_timeout_secs = 1;
            cfg.llm_wait_heartbeat_secs = 0;
        },
        None,
    )
    .await?;
    let mut executor = TurnExecutor::new(h.ctx.clone(), Arc::new(IdleAfterTextLlmBackend));
    let err = executor
        .execute("model stalls", None)
        .await
        .unwrap_err()
        .to_string();

    assert!(err.contains("idle timeout"), "{err}");
    let events = tokio::fs::read_to_string(&h.ctx.events_path).await?;
    assert!(
        events.contains(r#""category":"llm_idle_timeout""#),
        "{events}"
    );
    Ok(())
}

#[tokio::test]
async fn turn_max_turns_exhaustion_is_failed_not_stop() -> anyhow::Result<()> {
    let h = harness_with_config(
        "turn-max-turns",
        false,
        300,
        |cfg| {
            cfg.max_turns = 1;
        },
        None,
    )
    .await?;
    tokio::fs::write(h.cwd.join("fixture.txt"), "alpha\n").await?;
    let llm = Arc::new(MockLlmBackend::new(
        "flash",
        vec![vec![
            Ok(Event::ToolCall(tool_call(
                "Read",
                "call_1",
                json!({"path":"fixture.txt"}),
            ))),
            Ok(Event::Stop(StopEvent {
                reason: "tool_calls".into(),
            })),
        ]],
    ));
    let mut executor = TurnExecutor::new(h.ctx.clone(), llm_backend_from_mock(llm));
    let (decision, effects) = executor.execute("read until exhausted", None).await?;

    assert_eq!(decision, TurnDecision::MaxTurnsExceeded);
    assert!(effects.is_empty());
    assert_eq!(executor.tool_call_count(), 1);
    Ok(())
}

#[tokio::test]
async fn disabled_tool_call_persists_error_result_instead_of_being_dropped() -> anyhow::Result<()> {
    let h = harness_with_config(
        "disabled-tool-result",
        false,
        300,
        |cfg| {
            cfg.enabled_tools = Some(vec!["Read".into()]);
        },
        None,
    )
    .await?;
    let llm = Arc::new(MockLlmBackend::new(
        "flash",
        vec![
            vec![
                Ok(Event::ToolCall(tool_call(
                    "Bash",
                    "call_bash",
                    json!({"command":"echo should-not-run"}),
                ))),
                Ok(Event::Stop(StopEvent {
                    reason: "tool_calls".into(),
                })),
            ],
            vec![
                Ok(Event::Text(TextEvent {
                    content: "done".into(),
                })),
                Ok(Event::Stop(StopEvent {
                    reason: "end_turn".into(),
                })),
            ],
        ],
    ));
    let mut executor = TurnExecutor::new(h.ctx.clone(), llm_backend_from_mock(llm));
    let (decision, effects) = executor.execute("try disabled bash", None).await?;

    assert_eq!(decision, TurnDecision::Stop);
    assert!(effects.is_empty());
    let lines = h.ctx.store.lines().await?;
    assert!(
        lines[1]["content"]
            .as_array()
            .unwrap()
            .iter()
            .any(|block| block["type"] == "tool_use" && block["name"] == "Bash")
    );
    assert!(
        lines[2]["content"][0]["content"]
            .as_str()
            .unwrap()
            .contains("Tool 'Bash' is unavailable"),
        "{}",
        lines[2]
    );
    Ok(())
}

#[tokio::test]
async fn invalid_scavenged_tool_call_is_logged_and_ignored() -> anyhow::Result<()> {
    let h = harness("invalid-scavenge").await?;
    let llm = Arc::new(MockLlmBackend::new(
        "flash",
        vec![vec![
            Ok(Event::Text(TextEvent {
                content: r#"<tool_call>{"name":"Read","arguments":[]}</tool_call>"#.into(),
            })),
            Ok(Event::Stop(StopEvent {
                reason: "end_turn".into(),
            })),
        ]],
    ));
    let mut executor = TurnExecutor::new(h.ctx.clone(), llm_backend_from_mock(llm));
    let (decision, effects) = executor.execute("bad scavenged call", None).await?;

    assert_eq!(decision, TurnDecision::Stop);
    assert!(effects.is_empty());
    assert_eq!(executor.tool_call_count(), 0);
    let events = tokio::fs::read_to_string(&h.ctx.events_path).await?;
    assert!(
        events.contains("discarded invalid scavenged call Read"),
        "{events}"
    );
    Ok(())
}

#[tokio::test]
async fn duplicate_scavenged_tool_call_is_deduplicated_against_official_call() -> anyhow::Result<()>
{
    let h = harness("duplicate-scavenge").await?;
    tokio::fs::write(h.cwd.join("dup.txt"), "once\n").await?;
    let llm = Arc::new(MockLlmBackend::new(
        "flash",
        vec![
            vec![
                Ok(Event::ToolCall(tool_call(
                    "Read",
                    "call_read",
                    json!({"path":"dup.txt"}),
                ))),
                Ok(Event::Text(TextEvent {
                    content:
                        r#"<tool_call>{"name":"Read","arguments":{"path":"dup.txt"}}</tool_call>"#
                            .into(),
                })),
                Ok(Event::Stop(StopEvent {
                    reason: "tool_calls".into(),
                })),
            ],
            vec![Ok(Event::Stop(StopEvent {
                reason: "end_turn".into(),
            }))],
        ],
    ));
    let mut executor = TurnExecutor::new(h.ctx.clone(), llm_backend_from_mock(llm));
    let (decision, effects) = executor.execute("dedupe scavenged", None).await?;
    assert_eq!(decision, TurnDecision::Stop);
    assert!(effects.is_empty());
    assert_eq!(executor.tool_call_count(), 1);
    Ok(())
}

#[tokio::test]
async fn edit_tool_result_uses_full_edit_preview_branch() -> anyhow::Result<()> {
    let h = harness("edit-preview").await?;
    tokio::fs::write(h.cwd.join("edit.txt"), "old\n").await?;
    let snapshot = h
        .ctx
        .snapshots
        .lock()
        .unwrap()
        .record(&h.cwd.join("edit.txt"), "old\n", [1]);
    let patch = format!("[edit.txt#{}]\nPUT 1.=1:\n+new", snapshot.tag);
    let llm = Arc::new(MockLlmBackend::new(
        "flash",
        vec![
            vec![
                Ok(Event::ToolCall(tool_call(
                    "Edit",
                    "call_edit",
                    json!({"input":patch}),
                ))),
                Ok(Event::Stop(StopEvent {
                    reason: "tool_calls".into(),
                })),
            ],
            vec![
                Ok(Event::Text(TextEvent {
                    content: "done".into(),
                })),
                Ok(Event::Stop(StopEvent {
                    reason: "end_turn".into(),
                })),
            ],
        ],
    ));
    let mut executor = TurnExecutor::new(h.ctx.clone(), llm_backend_from_mock(llm));
    let (decision, effects) = executor.execute("edit file", None).await?;

    assert_eq!(decision, TurnDecision::Stop);
    assert!(effects.is_empty());
    assert_eq!(
        tokio::fs::read_to_string(h.cwd.join("edit.txt")).await?,
        "new\n"
    );
    Ok(())
}
