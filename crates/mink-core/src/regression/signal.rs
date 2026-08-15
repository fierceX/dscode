use super::*;

#[tokio::test]
async fn signal_recovery_guard_blocks_first_write() -> anyhow::Result<()> {
    let h = harness("guard-blocks-write").await?;
    let llm = Arc::new(MockLlmBackend::new(
        "flash",
        vec![
            vec![
                Ok(Event::ToolCall(tool_call(
                    "Bash",
                    "call_fail_1",
                    json!({"command":"false"}),
                ))),
                Ok(Event::ToolCall(tool_call(
                    "Bash",
                    "call_fail_2",
                    json!({"command":"false"}),
                ))),
                Ok(Event::ToolCall(tool_call(
                    "Bash",
                    "call_fail_3",
                    json!({"command":"false"}),
                ))),
                Ok(Event::Stop(StopEvent {
                    reason: "tool_calls".into(),
                })),
            ],
            vec![
                Ok(Event::ToolCall(tool_call(
                    "Write",
                    "call_write",
                    json!({"path":"blocked.txt","content":"nope"}),
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
    let mut belief = BeliefTracker::new(16);
    let (decision, effects) = executor
        .execute("fail then write", Some(&mut belief))
        .await?;

    assert_eq!(decision, TurnDecision::Stop);
    assert!(effects.is_empty());
    assert!(!h.cwd.join("blocked.txt").exists());
    let lines = h.ctx.store.lines().await?;
    assert!(
        serde_json::to_string(&lines)?.contains("SIGNAL_RECOVERY guard"),
        "{}",
        serde_json::to_string_pretty(&lines)?
    );
    let events = tokio::fs::read_to_string(&h.ctx.events_path).await?;
    assert!(events.contains(r#""name":"Write""#), "{events}");
    assert!(
        events.contains(r#""reason":"recovery_guard","state":"blocked""#),
        "{events}"
    );
    assert_eq!(
        events.matches(r#""source_tool":"Write""#).count(),
        1,
        "{events}"
    );
    Ok(())
}

#[tokio::test]
async fn signal_recovery_guard_blocks_whole_batch() -> anyhow::Result<()> {
    let h = harness("guard-blocks-batch").await?;
    let llm = Arc::new(MockLlmBackend::new(
        "flash",
        vec![
            vec![
                Ok(Event::ToolCall(tool_call(
                    "Bash",
                    "call_fail_1",
                    json!({"command":"false"}),
                ))),
                Ok(Event::ToolCall(tool_call(
                    "Bash",
                    "call_fail_2",
                    json!({"command":"false"}),
                ))),
                Ok(Event::ToolCall(tool_call(
                    "Bash",
                    "call_fail_3",
                    json!({"command":"false"}),
                ))),
                Ok(Event::Stop(StopEvent {
                    reason: "tool_calls".into(),
                })),
            ],
            vec![
                Ok(Event::ToolCall(tool_call(
                    "Write",
                    "call_write_a",
                    json!({"path":"blocked.txt","content":"nope"}),
                ))),
                Ok(Event::ToolCall(tool_call(
                    "Write",
                    "call_write_b",
                    json!({"path":"blocked2.txt","content":"nope"}),
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
    let mut belief = BeliefTracker::new(16);
    let (decision, effects) = executor
        .execute("fail then write twice", Some(&mut belief))
        .await?;

    assert_eq!(decision, TurnDecision::Stop);
    assert!(effects.is_empty());
    assert!(!h.cwd.join("blocked.txt").exists());
    assert!(!h.cwd.join("blocked2.txt").exists());
    let lines = h.ctx.store.lines().await?;
    let serialized = serde_json::to_string(&lines)?;
    assert_eq!(
        serialized.matches("SIGNAL_RECOVERY guard").count(),
        2,
        "{}",
        serde_json::to_string_pretty(&lines)?
    );
    assert!(
        serialized.contains(r#""tool_use_id":"call_write_a""#),
        "{}",
        serde_json::to_string_pretty(&lines)?
    );
    assert!(
        serialized.contains(r#""tool_use_id":"call_write_b""#),
        "{}",
        serde_json::to_string_pretty(&lines)?
    );
    let guard_signals = executor
        .collected_signals()
        .iter()
        .filter(|signal| {
            signal.source_tool == "Write"
                && matches!(signal.kind, crate::guard::collector::SignalKind::ToolFailed)
        })
        .collect::<Vec<_>>();
    assert_eq!(guard_signals.len(), 2);
    assert!(guard_signals.iter().all(|signal| signal.severity == 0.9));
    Ok(())
}

#[tokio::test]
async fn signal_recovery_guard_allows_first_read() -> anyhow::Result<()> {
    let h = harness("guard-allows-read").await?;
    tokio::fs::write(h.cwd.join("ok.txt"), "ok\n").await?;
    let llm = Arc::new(MockLlmBackend::new(
        "flash",
        vec![
            vec![
                Ok(Event::ToolCall(tool_call(
                    "Bash",
                    "call_fail_1",
                    json!({"command":"false"}),
                ))),
                Ok(Event::ToolCall(tool_call(
                    "Bash",
                    "call_fail_2",
                    json!({"command":"false"}),
                ))),
                Ok(Event::ToolCall(tool_call(
                    "Bash",
                    "call_fail_3",
                    json!({"command":"false"}),
                ))),
                Ok(Event::Stop(StopEvent {
                    reason: "tool_calls".into(),
                })),
            ],
            vec![
                Ok(Event::ToolCall(tool_call(
                    "Read",
                    "call_read",
                    json!({"path":"ok.txt"}),
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
    let mut belief = BeliefTracker::new(16);
    let (decision, effects) = executor
        .execute("fail then read", Some(&mut belief))
        .await?;

    assert_eq!(decision, TurnDecision::Stop);
    assert!(effects.is_empty());
    let lines = h.ctx.store.lines().await?;
    assert!(serde_json::to_string(&lines)?.contains("ok"));
    Ok(())
}

#[tokio::test]
async fn stop_error_reasons_return_failed_and_unknown_reasons_stop() -> anyhow::Result<()> {
    let h = harness("stop-reasons").await?;
    let llm = Arc::new(MockLlmBackend::new(
        "flash",
        vec![vec![Ok(Event::Stop(StopEvent {
            reason: "max_tokens".into(),
        }))]],
    ));
    let mut executor = TurnExecutor::new(h.ctx.clone(), llm_backend_from_mock(llm));
    let (decision, effects) = executor.execute("too long", None).await?;
    assert_eq!(decision, TurnDecision::Failed("stop: max_tokens".into()));
    assert!(effects.is_empty());

    let h = harness("unknown-stop").await?;
    let llm = Arc::new(MockLlmBackend::new(
        "flash",
        vec![vec![Ok(Event::Stop(StopEvent {
            reason: "content_filter".into(),
        }))]],
    ));
    let mut executor = TurnExecutor::new(h.ctx.clone(), llm_backend_from_mock(llm));
    let (decision, effects) = executor.execute("unknown", None).await?;
    assert_eq!(decision, TurnDecision::Stop);
    assert!(effects.is_empty());
    Ok(())
}

#[tokio::test]
async fn preflight_rejects_context_that_cannot_fit_the_request_budget() -> anyhow::Result<()> {
    let h = harness_with_config(
        "preflight-compact-path",
        false,
        300,
        |cfg| {
            cfg.max_context_tokens = 1;
            cfg.context_compact_pct = 100;
        },
        None,
    )
    .await?;
    let llm = Arc::new(MockLlmBackend::new(
        "flash",
        vec![vec![Ok(Event::Stop(StopEvent {
            reason: "end_turn".into(),
        }))]],
    ));
    let mut executor = TurnExecutor::new(h.ctx.clone(), llm_backend_from_mock(llm));
    let error = executor
        .execute("large context estimate", None)
        .await
        .expect_err("an impossible request budget must fail before the LLM call");
    assert!(error.to_string().contains("over the request input budget"));
    Ok(())
}

#[tokio::test]
async fn clean_tool_call_with_belief_takes_decision_none_path() -> anyhow::Result<()> {
    let h = harness_with_config(
        "decision-none-path",
        false,
        300,
        |cfg| {
            cfg.max_turns = 1;
        },
        None,
    )
    .await?;
    tokio::fs::write(h.cwd.join("clean.txt"), "clean\n").await?;
    let llm = Arc::new(MockLlmBackend::new(
        "flash",
        vec![vec![
            Ok(Event::ToolCall(tool_call(
                "Read",
                "call_read",
                json!({"path":"clean.txt"}),
            ))),
            Ok(Event::Stop(StopEvent {
                reason: "tool_calls".into(),
            })),
        ]],
    ));
    let mut executor = TurnExecutor::new(h.ctx.clone(), llm_backend_from_mock(llm));
    let mut belief = BeliefTracker::new(16);
    let (decision, effects) = executor.execute("read clean", Some(&mut belief)).await?;
    assert_eq!(decision, TurnDecision::MaxTurnsExceeded);
    assert!(effects.is_empty());
    Ok(())
}

#[tokio::test]
async fn soft_only_editloop_does_not_inject_above_warn_zone() -> anyhow::Result<()> {
    // 不注入任何消息（记录但不干预），避免打断正常的写->编译->修流程。
    let h = harness("soft-only-no-inject").await?;
    let llm = Arc::new(MockLlmBackend::new(
        "flash",
        vec![
            vec![Ok(Event::Stop(StopEvent {
                reason: "tool_calls".into(),
            }))],
            vec![Ok(Event::Stop(StopEvent {
                reason: "end_turn".into(),
            }))],
        ],
    ));
    let mut executor = TurnExecutor::new(h.ctx.clone(), llm_backend_from_mock(llm));
    let mut belief = BeliefTracker::new(16);
    belief.observe(&[Signal {
        kind: SignalKind::EditLoop,
        severity: 0.9,
        source: "EditLoop".into(),
        detail: "loop".into(),
        source_tool: "EditLoop".into(),
        exit_code: None,
        matched_pattern: None,
        message: "loop".into(),
    }]);
    let (decision, effects) = executor
        .execute("recover without recent", Some(&mut belief))
        .await?;

    assert_eq!(decision, TurnDecision::Stop);
    assert!(effects.is_empty());
    assert!(
        !h.display
            .info
            .lock()
            .unwrap()
            .iter()
            .any(|msg| msg.starts_with("Injecting")),
        "{:?}",
        h.display.info.lock().unwrap()
    );
    let lines = h.ctx.store.lines().await?;
    assert!(
        !lines.iter().any(|line| {
            line["role"] == "user"
                && line["content"]
                    .as_str()
                    .is_some_and(|content| content.contains("[trajectory]"))
        }),
        "soft-only signal must not inject evidence",
    );
    Ok(())
}

#[tokio::test]
async fn turn_injects_hint_after_failed_tool_and_continues() -> anyhow::Result<()> {
    let h = harness("turn-inject-after-fail").await?;
    let llm = Arc::new(MockLlmBackend::new(
        "flash",
        vec![
            vec![
                Ok(Event::ToolCall(tool_call(
                    "Bash",
                    "call_fail",
                    json!({"command":"false"}),
                ))),
                Ok(Event::Stop(StopEvent {
                    reason: "tool_use".into(),
                })),
            ],
            vec![
                Ok(Event::Text(TextEvent {
                    content: "recovered".into(),
                })),
                Ok(Event::Stop(StopEvent {
                    reason: "end_turn".into(),
                })),
            ],
        ],
    ));
    let mut executor = TurnExecutor::new(h.ctx.clone(), llm_backend_from_mock(llm));
    let mut belief = BeliefTracker::new(16);
    let (decision, effects) = executor
        .execute("run failing command", Some(&mut belief))
        .await?;

    assert_eq!(decision, TurnDecision::Stop);
    assert!(effects.is_empty());
    assert!(belief.belief() < 0.70);
    let lines = h.ctx.store.lines().await?;
    assert!(
        lines.iter().any(|line| {
            line["role"] == "user"
                && line["content"].as_str().is_some_and(|content| {
                    content.contains("[trajectory]") && content.contains("[detector]")
                })
        }),
        "{}",
        serde_json::to_string_pretty(&lines)?
    );
    assert!(
        h.display
            .info
            .lock()
            .unwrap()
            .iter()
            .any(|msg| msg.starts_with("Injecting trajectory evidence")),
        "{:?}",
        h.display.info.lock().unwrap()
    );
    Ok(())
}

#[tokio::test]
async fn turn_aborts_when_tool_failures_push_belief_too_low() -> anyhow::Result<()> {
    // SubAgent 不可用时，Full 策略的 Abort 直接进入用户接管。
    let h = harness_with_config(
        "turn-abort-after-failures",
        false,
        300,
        |cfg| cfg.enabled_tools = Some(vec!["Bash".into()]),
        None,
    )
    .await?;
    let calls = (0..8)
        .map(|idx| {
            Ok(Event::ToolCall(tool_call(
                "Bash",
                &format!("call_fail_{idx}"),
                json!({"command":format!("false # {idx}")}),
            )))
        })
        .chain(std::iter::once(Ok(Event::Stop(StopEvent {
            reason: "tool_use".into(),
        }))))
        .collect::<Vec<_>>();
    let llm = Arc::new(MockLlmBackend::new("flash", vec![calls]));
    let mut executor = TurnExecutor::new(h.ctx.clone(), llm_backend_from_mock(llm));
    let mut belief = BeliefTracker::new(16);
    let (decision, effects) = executor
        .execute("run many failing commands", Some(&mut belief))
        .await?;

    assert_eq!(
        decision,
        TurnDecision::Failed(
            "signal handover: reliability belief fell below the abort threshold".into()
        )
    );
    assert!(effects.is_empty());
    assert!(belief.belief() < 0.30, "belief={}", belief.belief());
    assert!(
        h.display
            .info
            .lock()
            .unwrap()
            .iter()
            .any(|msg| msg.starts_with("error:DecisionEngine: handing over")),
        "{:?}",
        h.display.info.lock().unwrap()
    );
    Ok(())
}

#[tokio::test]
async fn abort_degrades_to_replan_then_continues() -> anyhow::Result<()> {
    // fresh 子代理产出新计划后父代理继续本轮（不再直接失败）。
    let h = harness("abort-degrade-to-replan").await?;
    let parent_failures = (0..8)
        .map(|idx| {
            Ok(Event::ToolCall(tool_call(
                "Bash",
                &format!("call_fail_{idx}"),
                json!({"command":format!("false # {idx}")}),
            )))
        })
        .chain(std::iter::once(Ok(Event::Stop(StopEvent {
            reason: "tool_use".into(),
        }))))
        .collect::<Vec<_>>();
    let replan_child = vec![
        Ok(Event::Text(TextEvent {
            content: "Plan: re-read the failing module, add a unit test, then fix the root cause."
                .into(),
        })),
        Ok(Event::Stop(StopEvent {
            reason: "end_turn".into(),
        })),
    ];
    let parent_continue = vec![
        Ok(Event::Text(TextEvent {
            content: "recovered".into(),
        })),
        Ok(Event::Stop(StopEvent {
            reason: "end_turn".into(),
        })),
    ];
    let llm = Arc::new(MockLlmBackend::new(
        "flash",
        vec![parent_failures, replan_child, parent_continue],
    ));
    let ctx = test_context_with_llm_backend(h.ctx.clone(), llm.clone());
    let mut executor = TurnExecutor::new(ctx, llm_backend_from_mock(llm));
    let mut belief = BeliefTracker::new(16);
    let (decision, effects) = executor
        .execute("run many failing commands", Some(&mut belief))
        .await?;

    assert_eq!(decision, TurnDecision::Stop);
    assert!(effects.is_empty());
    // 策略重启成功后信念被重置为新证据基线。
    assert!(
        (belief.belief() - 0.75).abs() < 1e-10,
        "belief={}",
        belief.belief()
    );
    let lines = h.ctx.store.lines().await?;
    assert!(
        lines.iter().any(|line| {
            line["role"] == "user"
                && line["content"]
                    .as_str()
                    .is_some_and(|content| content.contains("[replan]"))
        }),
        "{}",
        serde_json::to_string_pretty(&lines)?
    );
    Ok(())
}

#[tokio::test]
async fn second_warning_triggers_replan() -> anyhow::Result<()> {
    // 触发 fresh 子代理重新规划，成功后跳过恢复守卫。
    // （cooldown_turns=0 让两次 Warning 相邻出现，精确锻炼该升级路径。）
    let h = harness_with_config(
        "second-warning-replan",
        false,
        300,
        |cfg| cfg.signal.cooldown_turns = 0,
        None,
    )
    .await?;
    let failing_batch = |prefix: &str| {
        (0..3)
            .map(|idx| {
                Ok(Event::ToolCall(tool_call(
                    "Bash",
                    &format!("{prefix}_{idx}"),
                    json!({"command":format!("false # {prefix} {idx}")}),
                )))
            })
            .chain(std::iter::once(Ok(Event::Stop(StopEvent {
                reason: "tool_use".into(),
            }))))
            .collect::<Vec<_>>()
    };
    let replan_child = vec![
        Ok(Event::Text(TextEvent {
            content: "Plan: isolate the failing change and verify with one focused command.".into(),
        })),
        Ok(Event::Stop(StopEvent {
            reason: "end_turn".into(),
        })),
    ];
    let parent_finish = vec![
        Ok(Event::Text(TextEvent {
            content: "done".into(),
        })),
        Ok(Event::Stop(StopEvent {
            reason: "end_turn".into(),
        })),
    ];
    let llm = Arc::new(MockLlmBackend::new(
        "flash",
        vec![
            failing_batch("a"),
            failing_batch("b"),
            replan_child,
            parent_finish,
        ],
    ));
    let ctx = test_context_with_llm_backend(h.ctx.clone(), llm.clone());
    let mut executor = TurnExecutor::new(ctx, llm_backend_from_mock(llm));
    let mut belief = BeliefTracker::new(16);
    let (decision, effects) = executor
        .execute("recover via replan", Some(&mut belief))
        .await?;

    assert_eq!(decision, TurnDecision::Stop);
    assert!(effects.is_empty());
    let lines = h.ctx.store.lines().await?;
    assert!(
        lines.iter().any(|line| {
            line["role"] == "user"
                && line["content"]
                    .as_str()
                    .is_some_and(|content| content.contains("[replan]"))
        }),
        "{}",
        serde_json::to_string_pretty(&lines)?
    );
    Ok(())
}

/// 构造"读取 -> 编辑 -> 三次失败"的 mock 脚本，返回 (脚本, 原文件内容, 编辑后内容)。
/// 失败批：5 个互不相同的失败 Bash（2 次干净调用后 α=5，5 次失败使
/// β=6 → B≈0.455 < warn，确保触发 Warning 级回滚）。
fn failing_batch_n(n: usize, prefix: &str) -> Vec<anyhow::Result<Event>> {
    let mut batch: Vec<anyhow::Result<Event>> = (0..n)
        .map(|idx| {
            Ok(Event::ToolCall(tool_call(
                "Bash",
                &format!("{prefix}_{idx}"),
                json!({"command": format!("false {prefix} {idx}")}),
            )))
        })
        .collect();
    batch.push(Ok(Event::Stop(StopEvent {
        reason: "tool_use".into(),
    })));
    batch
}

fn rollback_script_after_edit(tag: &str) -> Vec<Vec<anyhow::Result<Event>>> {
    vec![
        vec![
            Ok(Event::ToolCall(tool_call(
                "Read",
                "call_read",
                // 范围读走快照记录路径（无选择器的 Read 是 full_read_preview，
                // 不记录回滚基线）。
                json!({"path":"a.rs:1-10"}),
            ))),
            Ok(Event::Stop(StopEvent {
                reason: "tool_use".into(),
            })),
        ],
        vec![
            Ok(Event::ToolCall(tool_call(
                "Edit",
                "call_edit",
                json!({"input": format!("[a.rs#{tag}]\nPUT 1.=1:\n+lineX\n")}),
            ))),
            Ok(Event::Stop(StopEvent {
                reason: "tool_use".into(),
            })),
        ],
        failing_batch_n(5, "fail"),
        vec![
            Ok(Event::Text(TextEvent {
                content: "done".into(),
            })),
            Ok(Event::Stop(StopEvent {
                reason: "end_turn".into(),
            })),
        ],
    ]
}

#[tokio::test]
async fn hashline_rollback_restores_last_read_baseline() -> anyhow::Result<()> {
    // 最后一次 Read 记录的基线（而不是 record_edit 记录的编辑后内容）。
    let h = harness("hashline-rollback").await?;
    let original = "line1\nline2\nline3\n";
    tokio::fs::write(h.cwd.join("a.rs"), original).await?;
    let tag = crate::tools::snapshot::compute_file_tag(original);
    let llm = Arc::new(MockLlmBackend::new(
        "flash",
        rollback_script_after_edit(&tag),
    ));
    let ctx = test_context_with_llm_backend(h.ctx.clone(), llm.clone());
    let mut executor = TurnExecutor::new(ctx, llm_backend_from_mock(llm));
    let mut belief = BeliefTracker::new(16);
    let (decision, _) = executor
        .execute("edit then fail", Some(&mut belief))
        .await?;
    assert_eq!(decision, TurnDecision::Stop);
    let on_disk = tokio::fs::read_to_string(h.cwd.join("a.rs")).await?;
    assert_eq!(
        on_disk, original,
        "hashline rollback must restore the last READ baseline"
    );
    Ok(())
}

#[tokio::test]
#[cfg(unix)]
async fn rollback_preserves_executable_permissions() -> anyhow::Result<()> {
    // 回滚经 atomic_replace 换文件时必须保留原权限（可执行脚本 +x）。
    use std::os::unix::fs::PermissionsExt;
    let h = harness("rollback-perms").await?;
    let original = "#!/bin/sh\necho hi\n";
    tokio::fs::write(h.cwd.join("a.rs"), original).await?;
    tokio::fs::set_permissions(h.cwd.join("a.rs"), std::fs::Permissions::from_mode(0o755)).await?;
    let tag = crate::tools::snapshot::compute_file_tag(original);
    let llm = Arc::new(MockLlmBackend::new(
        "flash",
        rollback_script_after_edit(&tag),
    ));
    let ctx = test_context_with_llm_backend(h.ctx.clone(), llm.clone());
    let mut executor = TurnExecutor::new(ctx, llm_backend_from_mock(llm));
    let mut belief = BeliefTracker::new(16);
    let (decision, _) = executor
        .execute("edit then fail", Some(&mut belief))
        .await?;
    assert_eq!(decision, TurnDecision::Stop);
    let meta = tokio::fs::metadata(h.cwd.join("a.rs")).await?;
    assert_eq!(
        meta.permissions().mode() & 0o777,
        0o755,
        "rollback must preserve executable permissions"
    );
    Ok(())
}

#[tokio::test]
async fn replace_mode_rollback_restores_last_read_baseline() -> anyhow::Result<()> {
    let h = harness_with_config(
        "replace-rollback",
        false,
        300,
        |cfg| cfg.edit_mode = crate::config::EditMode::Replace,
        None,
    )
    .await?;
    let original = "line1\nline2\nline3\n";
    tokio::fs::write(h.cwd.join("a.rs"), original).await?;
    let llm = Arc::new(MockLlmBackend::new(
        "flash",
        vec![
            vec![
                Ok(Event::ToolCall(tool_call(
                    "Read",
                    "call_read",
                    json!({"path":"a.rs:1-10"}),
                ))),
                Ok(Event::Stop(StopEvent {
                    reason: "tool_use".into(),
                })),
            ],
            vec![
                Ok(Event::ToolCall(tool_call(
                    "Edit",
                    "call_edit",
                    json!({
                        "path": "a.rs",
                        "edits": [{"old_text": "line1", "new_text": "lineX", "all": false}],
                    }),
                ))),
                Ok(Event::Stop(StopEvent {
                    reason: "tool_use".into(),
                })),
            ],
            failing_batch_n(5, "fail"),
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
    let ctx = test_context_with_llm_backend(h.ctx.clone(), llm.clone());
    let mut executor = TurnExecutor::new(ctx, llm_backend_from_mock(llm));
    let mut belief = BeliefTracker::new(16);
    let (decision, _) = executor
        .execute("edit then fail", Some(&mut belief))
        .await?;
    assert_eq!(decision, TurnDecision::Stop);
    let on_disk = tokio::fs::read_to_string(h.cwd.join("a.rs")).await?;
    assert_eq!(
        on_disk, original,
        "replace rollback must restore the read baseline"
    );
    Ok(())
}

#[tokio::test]
async fn rollback_scope_limited_to_recent_edit_window() -> anyhow::Result<()> {
    let h = harness("rollback-scope").await?;
    let original = "line1\nline2\nline3\n";
    tokio::fs::write(h.cwd.join("a.rs"), original).await?;
    let tag = crate::tools::snapshot::compute_file_tag(original);
    let mut script = vec![
        vec![
            Ok(Event::ToolCall(tool_call(
                "Read",
                "call_read",
                json!({"path":"a.rs:1-10"}),
            ))),
            Ok(Event::Stop(StopEvent {
                reason: "tool_use".into(),
            })),
        ],
        vec![
            Ok(Event::ToolCall(tool_call(
                "Edit",
                "call_edit",
                json!({"input": format!("[a.rs#{tag}]\nPUT 1.=1:\n+lineX\n")}),
            ))),
            Ok(Event::Stop(StopEvent {
                reason: "tool_use".into(),
            })),
        ],
    ];
    // 6 个成功的 Bash（互不相同，避免 StormBreaker 抑制）把编辑挤出回滚窗口。
    let mut clean_batch: Vec<anyhow::Result<Event>> = (0..6)
        .map(|idx| {
            Ok(Event::ToolCall(tool_call(
                "Bash",
                &format!("clean_{idx}"),
                json!({"command": format!("echo ok {idx}")}),
            )))
        })
        .collect();
    clean_batch.push(Ok(Event::Stop(StopEvent {
        reason: "tool_use".into(),
    })));
    script.push(clean_batch);
    // 11 次失败（α=11 由 8 次干净调用推高；β=12 > α 才进入警告区）。
    script.push(failing_batch_n(11, "fail"));
    script.push(vec![
        Ok(Event::Text(TextEvent {
            content: "done".into(),
        })),
        Ok(Event::Stop(StopEvent {
            reason: "end_turn".into(),
        })),
    ]);
    let llm = Arc::new(MockLlmBackend::new("flash", script));
    let ctx = test_context_with_llm_backend(h.ctx.clone(), llm.clone());
    let mut executor = TurnExecutor::new(ctx, llm_backend_from_mock(llm));
    let mut belief = BeliefTracker::new(16);
    let (decision, _) = executor
        .execute("edit then fail", Some(&mut belief))
        .await?;
    assert_eq!(decision, TurnDecision::Stop);
    let on_disk = tokio::fs::read_to_string(h.cwd.join("a.rs")).await?;
    assert_eq!(
        on_disk, "lineX\nline2\nline3\n",
        "edits outside the rollback window must survive"
    );
    Ok(())
}

#[tokio::test]
async fn repeated_soft_failures_trigger_evidence_injection() -> anyhow::Result<()> {
    let h = harness("repeated-soft-failures").await?;
    let soft_failure_batch = |n: usize| {
        vec![
            Ok(Event::ToolCall(tool_call(
                "Bash",
                &format!("soft_{n}"),
                json!({"command": format!("echo 'Traceback (most recent call last): fake {n}'")}),
            ))),
            Ok(Event::Stop(StopEvent {
                reason: "tool_use".into(),
            })),
        ]
    };
    let llm = Arc::new(MockLlmBackend::new(
        "flash",
        vec![
            soft_failure_batch(1),
            soft_failure_batch(2),
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
    let ctx = test_context_with_llm_backend(h.ctx.clone(), llm.clone());
    let mut executor = TurnExecutor::new(ctx, llm_backend_from_mock(llm));
    let mut belief = BeliefTracker::new(16);
    let (decision, _) = executor.execute("soft failures", Some(&mut belief)).await?;
    assert_eq!(decision, TurnDecision::Stop);
    let lines = h.ctx.store.lines().await?;
    assert!(
        lines.iter().any(|line| {
            line["role"] == "user"
                && line["content"]
                    .as_str()
                    .is_some_and(|content| content.contains("[trajectory]"))
        }),
        "{}",
        serde_json::to_string_pretty(&lines)?
    );
    Ok(())
}

#[tokio::test]
async fn clean_calls_do_not_open_soft_failure_gate() -> anyhow::Result<()> {
    // 成功调用不得推高 soft_failures 计数。14 次干净调用 + 1 次软失败
    // 使 B≈0.905 落在 [warn=0.90, remind=0.95) 区间，门控成为唯一决定因素：
    // 计数正确（soft=1）→ 沉默；计数被干净调用污染（=15）→ 误注入。
    let h = harness_with_config(
        "soft-gate-clean",
        false,
        300,
        |cfg| {
            cfg.signal.remind_threshold = 0.95;
            cfg.signal.warn_threshold = 0.90;
        },
        None,
    )
    .await?;
    let mut clean_batch: Vec<anyhow::Result<Event>> = (0..14)
        .map(|idx| {
            Ok(Event::ToolCall(tool_call(
                "Bash",
                &format!("clean_{idx}"),
                json!({"command": format!("echo ok {idx}")}),
            )))
        })
        .collect();
    clean_batch.push(Ok(Event::Stop(StopEvent {
        reason: "tool_use".into(),
    })));
    let llm = Arc::new(MockLlmBackend::new(
        "flash",
        vec![
            clean_batch,
            vec![
                Ok(Event::ToolCall(tool_call(
                    "Bash",
                    "soft_1",
                    json!({"command": "echo 'Traceback (most recent call last): fake'"}),
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
    let ctx = test_context_with_llm_backend(h.ctx.clone(), llm.clone());
    let mut executor = TurnExecutor::new(ctx, llm_backend_from_mock(llm));
    let mut belief = BeliefTracker::new(16);
    let (decision, _) = executor.execute("mostly clean", Some(&mut belief)).await?;
    assert_eq!(decision, TurnDecision::Stop);
    let lines = h.ctx.store.lines().await?;
    assert!(
        !lines.iter().any(|line| {
            line["role"] == "user"
                && line["content"]
                    .as_str()
                    .is_some_and(|content| content.contains("[trajectory]"))
        }),
        "clean calls must not open the soft-failure gate; store: {}",
        serde_json::to_string_pretty(&lines)?
    );
    Ok(())
}

#[tokio::test]
async fn replan_setup_failure_degrades_to_handover() -> anyhow::Result<()> {
    let h = harness("replan-setup-failure").await?;
    // 预置冲突使 SubAgentExecutor::new 失败：subagents 路径是普通文件而非
    // 目录时，任何 child home 都无法创建（replan id 唯一化后无法预知具体
    // 目录名，用父目录类型冲突作为确定性故障注入点）。
    let parent_session_dir = h
        .ctx
        .store
        .path()
        .parent()
        .expect("parent conversation has a session directory")
        .to_path_buf();
    tokio::fs::write(parent_session_dir.join("subagents"), b"").await?;
    let calls = (0..8)
        .map(|idx| {
            Ok(Event::ToolCall(tool_call(
                "Bash",
                &format!("call_fail_{idx}"),
                json!({"command":format!("false # {idx}")}),
            )))
        })
        .chain(std::iter::once(Ok(Event::Stop(StopEvent {
            reason: "tool_use".into(),
        }))))
        .collect::<Vec<_>>();
    let llm = Arc::new(MockLlmBackend::new("flash", vec![calls]));
    let ctx = test_context_with_llm_backend(h.ctx.clone(), llm.clone());
    let mut executor = TurnExecutor::new(ctx, llm_backend_from_mock(llm));
    let mut belief = BeliefTracker::new(16);
    let outcome = executor
        .execute("many failing commands", Some(&mut belief))
        .await;
    let (decision, _) = outcome.expect("replan setup failure must not fail the whole turn");
    assert_eq!(
        decision,
        TurnDecision::Failed(
            "signal handover: reliability belief fell below the abort threshold".into()
        )
    );
    Ok(())
}
