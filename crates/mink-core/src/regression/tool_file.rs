use super::*;

#[test]
fn typed_events_keep_legacy_replay_type_names() {
    let events = vec![
        crate::events::EventLog::UserInput {
            version: 1,
            content: "u".into(),
        },
        crate::events::EventLog::ToolCall {
            version: 1,
            name: "Read".into(),
            id: "call".into(),
            input: json!({"path":"a.txt"}),
        },
        crate::events::EventLog::ToolResult {
            version: 1,
            tool_use_id: "call".into(),
            name: "Read".into(),
            content: "Read(a.txt) [1 lines, 1 bytes]\nx".into(),
        },
    ];
    let types = events
        .into_iter()
        .map(|event| {
            serde_json::to_value(event).unwrap()["type"]
                .as_str()
                .unwrap()
                .to_string()
        })
        .collect::<Vec<_>>();
    assert_eq!(types, ["user_input", "tool_call", "tool_result"]);
}

#[tokio::test]
async fn hashline_read_edit_and_stale_recovery_flow() -> anyhow::Result<()> {
    let h = harness("hashline-flow").await?;
    tokio::fs::write(h.cwd.join("flow.txt"), "one\ntwo\n").await?;
    let runner = ToolRunner::new(Arc::new(ToolContext::from(h.ctx.as_ref())));
    let read = runner
        .execute_all(vec![tool_call(
            "Read",
            "read_flow",
            json!({"path":"flow.txt"}),
        )])
        .await?;
    let tag = crate::tools::snapshot::compute_file_tag("one\ntwo\n");
    assert!(read[0].content.contains(&format!("[flow.txt#{tag}]")));

    tokio::fs::write(h.cwd.join("flow.txt"), "prefix\none\ntwo\n").await?;
    let edited = runner
        .execute_all(vec![tool_call(
            "Edit",
            "edit_flow",
            json!({"input":format!("[flow.txt#{tag}]\nPUT 2.=2:\n+TWO")}),
        )])
        .await?;
    assert!(edited[0].succeeded(), "{}", edited[0].content);
    assert_eq!(
        tokio::fs::read_to_string(h.cwd.join("flow.txt")).await?,
        "prefix\none\nTWO\n"
    );
    assert!(edited[0].content.contains("uniform +1 line offset"));
    let new_tag = crate::tools::snapshot::compute_file_tag("prefix\none\nTWO\n");
    assert!(edited[0].content.contains(&format!("[flow.txt#{new_tag}]")));
    assert!(edited[0].content.contains("firstChangedLine: 3"));
    assert!(edited[0].content.contains("Diff:"));
    assert_eq!(edited[0].conv_content, edited[0].content);
    Ok(())
}

#[tokio::test]
async fn hashline_full_turn_persists_complete_edit_result_and_reuses_new_tag() -> anyhow::Result<()>
{
    let h = harness("hashline-turn-result").await?;
    tokio::fs::write(h.cwd.join("turn.txt"), "one\ntwo\n").await?;
    let runner = ToolRunner::new(Arc::new(ToolContext::from(h.ctx.as_ref())));
    runner
        .execute_all(vec![tool_call(
            "Read",
            "seed_turn",
            json!({"path":"turn.txt"}),
        )])
        .await?;
    let original_tag = crate::tools::snapshot::compute_file_tag("one\ntwo\n");
    tokio::fs::write(h.cwd.join("turn.txt"), "prefix\none\ntwo\n").await?;
    let first_text = "prefix\none\nTWO\n";
    let first_tag = crate::tools::snapshot::compute_file_tag(first_text);

    let llm = Arc::new(MockLlmBackend::new(
        "flash",
        vec![
            vec![
                Ok(Event::ToolCall(tool_call(
                    "Edit",
                    "turn_edit_one",
                    json!({"input":format!("[turn.txt#{original_tag}]\nPUT 2.=2:\n+TWO")}),
                ))),
                Ok(Event::Stop(StopEvent {
                    reason: "tool_use".into(),
                })),
            ],
            vec![
                Ok(Event::ToolCall(tool_call(
                    "Edit",
                    "turn_edit_two",
                    json!({"input":format!("[turn.txt#{first_tag}]\nPUT 3.=3:\n+THREE")}),
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
    executor.execute("edit twice", None).await?;

    assert_eq!(
        tokio::fs::read_to_string(h.cwd.join("turn.txt")).await?,
        "prefix\none\nTHREE\n"
    );
    let lines = h.ctx.store.lines().await?;
    let first_result = lines[2]["content"][0]["content"]
        .as_str()
        .expect("first Edit result content");
    assert!(first_result.contains(&format!("[turn.txt#{first_tag}]")));
    assert!(first_result.contains("firstChangedLine: 3"));
    assert!(first_result.contains("Diff:"));
    assert!(first_result.contains("uniform +1 line offset"));
    let second_result = lines[4]["content"][0]["content"]
        .as_str()
        .expect("second Edit result content");
    assert!(second_result.contains("firstChangedLine: 3"));
    Ok(())
}

#[tokio::test]
async fn hashline_unknown_tag_reports_current_tag() -> anyhow::Result<()> {
    let h = harness("hashline-unknown-tag").await?;
    tokio::fs::write(h.cwd.join("u.txt"), "one\n").await?;
    let runner = ToolRunner::new(Arc::new(ToolContext::from(h.ctx.as_ref())));
    let result = runner
        .execute_all(vec![tool_call(
            "Edit",
            "edit_unknown",
            json!({"input":"[u.txt#DEAD]\nPUT 1.=1:\n+TWO"}),
        )])
        .await?;
    assert!(!result[0].succeeded());
    assert!(
        result[0]
            .content
            .contains("does not belong to this session")
    );
    assert!(result[0].content.contains("Do not invent tags"));
    assert!(result[0].content.contains("must not be used to retry"));
    assert!(result[0].content.contains("* 1:one"));
    assert!(!result[0].content.contains("retry with the current tag"));

    let current_tag = crate::tools::snapshot::compute_file_tag("one\n");
    let retry = runner
        .execute_all(vec![tool_call(
            "Edit",
            "edit_computed_hash",
            json!({"input":format!("[u.txt#{current_tag}]\nPUT 1.=1:\n+TWO")}),
        )])
        .await?;
    assert!(
        !retry[0].succeeded(),
        "a diagnostic hash must not authorize Edit"
    );
    assert!(retry[0].content.contains("does not belong to this session"));
    Ok(())
}

#[tokio::test]
async fn hashline_noop_softens_twice_then_fails_and_resets() -> anyhow::Result<()> {
    let h = harness("hashline-noop").await?;
    tokio::fs::write(h.cwd.join("noop.txt"), "same\n").await?;
    let runner = ToolRunner::new(Arc::new(ToolContext::from(h.ctx.as_ref())));
    runner
        .execute_all(vec![tool_call(
            "Read",
            "read_noop",
            json!({"path":"noop.txt"}),
        )])
        .await?;
    let tag = crate::tools::snapshot::compute_file_tag("same\n");
    // An explainable no-op (body already at the target) is idempotent now; use
    // a register cut/paste round-trip to exercise the unexplained soft no-op path.
    let payload = format!("[noop.txt#{tag}]\nCUT 1.=1 @r\nPUT >1 @r");

    for (index, expected) in [(1, "soft no-op 1/2"), (2, "soft no-op 2/2")] {
        let result = runner
            .execute_all(vec![tool_call(
                "Edit",
                &format!("noop_{index}"),
                json!({"input":payload.clone()}),
            )])
            .await?;
        assert!(result[0].succeeded(), "{}", result[0].content);
        assert!(result[0].content.contains(expected));
        assert!(result[0].signals.is_empty());
    }
    let third = runner
        .execute_all(vec![tool_call(
            "Edit",
            "noop_3",
            json!({"input":payload.clone()}),
        )])
        .await?;
    assert!(!third[0].succeeded());
    assert!(third[0].content.contains("will continue to fail"));

    let alternate = runner
        .execute_all(vec![tool_call(
            "Edit",
            "noop_alternate",
            json!({"input":format!("[noop.txt#{tag}]\nCUT 1\nPUT >1")}),
        )])
        .await?;
    assert!(alternate[0].succeeded());
    assert!(alternate[0].content.contains("soft no-op 1/2"));

    let changed = runner
        .execute_all(vec![tool_call(
            "Edit",
            "noop_change",
            json!({"input":format!("[noop.txt#{tag}]\nPUT 1:\n+changed")}),
        )])
        .await?;
    assert!(changed[0].succeeded(), "{}", changed[0].content);
    let changed_tag = crate::tools::snapshot::compute_file_tag("changed\n");
    let after_commit = runner
        .execute_all(vec![tool_call(
            "Edit",
            "noop_after_commit",
            json!({"input":format!("[noop.txt#{changed_tag}]\nCUT 1\nPUT >1")}),
        )])
        .await?;
    assert!(after_commit[0].succeeded());
    assert!(after_commit[0].content.contains("soft no-op 1/2"));
    Ok(())
}

#[tokio::test]
async fn hashline_batch_noop_preflight_prevents_partial_commit() -> anyhow::Result<()> {
    let h = harness("hashline-batch-noop").await?;
    tokio::fs::write(h.cwd.join("change.txt"), "old\n").await?;
    tokio::fs::write(h.cwd.join("same.txt"), "same\n").await?;
    let runner = ToolRunner::new(Arc::new(ToolContext::from(h.ctx.as_ref())));
    runner
        .execute_all(vec![
            tool_call("Read", "read_change", json!({"path":"change.txt"})),
            tool_call("Read", "read_same", json!({"path":"same.txt"})),
        ])
        .await?;
    let change_tag = crate::tools::snapshot::compute_file_tag("old\n");
    let same_tag = crate::tools::snapshot::compute_file_tag("same\n");
    let result = runner
        .execute_all(vec![tool_call(
            "Edit",
            "batch_noop",
            json!({"input":format!(
                "[change.txt#{change_tag}]\nPUT 1:\n+new\n[same.txt#{same_tag}]\nPUT 1:\n+same"
            )}),
        )])
        .await?;
    assert!(!result[0].succeeded());
    assert!(result[0].content.contains("no files were committed"));
    assert_eq!(
        tokio::fs::read_to_string(h.cwd.join("change.txt")).await?,
        "old\n"
    );
    Ok(())
}

#[tokio::test]
async fn oversized_hashline_result_shares_artifact_url_with_model_and_ui() -> anyhow::Result<()> {
    let h = harness_with_config(
        "hashline-artifact",
        false,
        300,
        |config| config.tool_result_max_bytes = 500,
        None,
    )
    .await?;
    let original = (1..=80)
        .map(|line| format!("old-{line:03}"))
        .collect::<Vec<_>>()
        .join("\n")
        + "\n";
    tokio::fs::write(h.cwd.join("large.txt"), &original).await?;
    let runner = ToolRunner::new(Arc::new(ToolContext::from(h.ctx.as_ref())));
    runner
        .execute_all(vec![tool_call(
            "Write",
            "seed_large",
            json!({"path":"large.txt", "content":original.clone()}),
        )])
        .await?;
    let tag = crate::tools::snapshot::compute_file_tag(&original);
    let body = (1..=80)
        .map(|line| format!("+new-{line:03}"))
        .collect::<Vec<_>>()
        .join("\n");
    let result = runner
        .execute_all(vec![tool_call(
            "Edit",
            "edit_large",
            json!({"input":format!("[large.txt#{tag}]\nPUT 1.=80:\n{body}")}),
        )])
        .await?;
    assert!(result[0].succeeded(), "{}", result[0].content);
    assert!(result[0].content.contains("artifact://"));
    assert_eq!(result[0].conv_content, result[0].content);
    assert_eq!(result[0].artifacts.len(), 1);
    Ok(())
}

#[tokio::test]
async fn hashline_stale_error_reports_current_tag() -> anyhow::Result<()> {
    let h = harness("hashline-stale-tag").await?;
    tokio::fs::write(h.cwd.join("conflict.txt"), "one\ntwo\n").await?;
    let runner = ToolRunner::new(Arc::new(ToolContext::from(h.ctx.as_ref())));
    runner
        .execute_all(vec![tool_call(
            "Read",
            "read_conflict",
            json!({"path":"conflict.txt"}),
        )])
        .await?;
    let stale_tag = crate::tools::snapshot::compute_file_tag("one\ntwo\n");
    tokio::fs::write(h.cwd.join("conflict.txt"), "one\nchanged\n").await?;
    let result = runner
        .execute_all(vec![tool_call(
            "Edit",
            "edit_stale",
            json!({"input":format!("[conflict.txt#{stale_tag}]\nPUT 2.=2:\n+TWO")}),
        )])
        .await?;
    assert!(!result[0].succeeded());
    assert!(
        result[0]
            .content
            .contains("Current content hash (diagnostic)")
    );
    assert!(
        result[0]
            .content
            .contains("drifted outside a successful Edit")
    );
    assert!(result[0].content.contains("* 2:changed"));
    // 外部审查 #1：外部漂移时不得把旧 tag 推荐为“current snapshot”，
    // 必须明确提示该 tag 已过期并要求重新 Read。
    assert!(
        result[0].content.contains("cannot be reused"),
        "stale error must warn the last known snapshot cannot be reused: {}",
        result[0].content
    );
    assert!(
        !result[0].content.contains("may be reused directly"),
        "{}",
        result[0].content
    );
    Ok(())
}

#[tokio::test]
async fn hashline_changed_anchor_fails_closed() -> anyhow::Result<()> {
    let h = harness("hashline-conflict").await?;
    tokio::fs::write(h.cwd.join("conflict.txt"), "one\ntwo\n").await?;
    let runner = ToolRunner::new(Arc::new(ToolContext::from(h.ctx.as_ref())));
    runner
        .execute_all(vec![tool_call(
            "Read",
            "read_conflict",
            json!({"path":"conflict.txt"}),
        )])
        .await?;
    let tag = crate::tools::snapshot::compute_file_tag("one\ntwo\n");
    tokio::fs::write(h.cwd.join("conflict.txt"), "one\nchanged\n").await?;
    let result = runner
        .execute_all(vec![tool_call(
            "Edit",
            "edit_conflict",
            json!({"input":format!("[conflict.txt#{tag}]\nPUT 2.=2:\n+TWO")}),
        )])
        .await?;
    assert!(!result[0].succeeded());
    assert!(
        result[0]
            .content
            .contains("could not be recovered unambiguously")
    );
    assert!(
        result[0]
            .content
            .contains("Current content hash (diagnostic)")
    );
    assert!(result[0].content.contains("* 2:changed"));
    Ok(())
}

#[tokio::test]
async fn hashline_stale_error_distinguishes_prior_edit_response_tag() -> anyhow::Result<()> {
    let h = harness("hashline-edit-tag-provenance").await?;
    tokio::fs::write(h.cwd.join("provenance.txt"), "one\ntwo\n").await?;
    let runner = ToolRunner::new(Arc::new(ToolContext::from(h.ctx.as_ref())));
    runner
        .execute_all(vec![tool_call(
            "Read",
            "read_provenance",
            json!({"path":"provenance.txt"}),
        )])
        .await?;
    let old_tag = crate::tools::snapshot::compute_file_tag("one\ntwo\n");
    let first = runner
        .execute_all(vec![tool_call(
            "Edit",
            "edit_provenance",
            json!({"input":format!("[provenance.txt#{old_tag}]\nPUT 2:\n+changed")}),
        )])
        .await?;
    assert!(first[0].succeeded(), "{}", first[0].content);
    let edit_tag = crate::tools::snapshot::compute_file_tag("one\nchanged\n");
    assert!(
        first[0]
            .content
            .contains(&format!("[provenance.txt#{edit_tag}]"))
    );

    let stale = runner
        .execute_all(vec![tool_call(
            "Edit",
            "stale_after_edit",
            json!({"input":format!("[provenance.txt#{old_tag}]\nPUT 2:\n+again")}),
        )])
        .await?;
    assert!(!stale[0].succeeded());
    assert!(stale[0].content.contains("earlier successful Edit"));
    assert!(
        stale[0]
            .content
            .contains(&format!("[provenance.txt#{edit_tag}]"))
    );
    Ok(())
}

#[tokio::test]
async fn hashline_inconsistent_anchor_offsets_fail_closed_with_context() -> anyhow::Result<()> {
    let h = harness("hashline-inconsistent-offsets").await?;
    let original = "top\nleft\nmiddle\nright\nbottom\n";
    tokio::fs::write(h.cwd.join("offsets.txt"), original).await?;
    let runner = ToolRunner::new(Arc::new(ToolContext::from(h.ctx.as_ref())));
    runner
        .execute_all(vec![tool_call(
            "Read",
            "read_offsets",
            json!({"path":"offsets.txt"}),
        )])
        .await?;
    let tag = crate::tools::snapshot::compute_file_tag(original);
    tokio::fs::write(
        h.cwd.join("offsets.txt"),
        "top\nleft\nmiddle\ninserted\nright\nbottom\n",
    )
    .await?;
    let result = runner
        .execute_all(vec![tool_call(
            "Edit",
            "edit_offsets",
            json!({"input":format!(
                "[offsets.txt#{tag}]\nPUT 2:\n+LEFT\nPUT 4:\n+RIGHT"
            )}),
        )])
        .await?;
    assert!(!result[0].succeeded());
    assert!(result[0].content.contains("inconsistent offset"));
    assert!(result[0].content.contains("* 2:left"));
    assert!(result[0].content.contains("* 4:inserted"));
    assert_eq!(result[0].conv_content, result[0].content);
    assert_eq!(
        tokio::fs::read_to_string(h.cwd.join("offsets.txt")).await?,
        "top\nleft\nmiddle\ninserted\nright\nbottom\n"
    );
    Ok(())
}

#[tokio::test]
async fn hashline_grep_and_cross_file_clipboard_flow() -> anyhow::Result<()> {
    let h = harness("hashline-grep-clipboard").await?;
    tokio::fs::write(h.cwd.join("a.txt"), "keep\nneedle\n").await?;
    tokio::fs::write(h.cwd.join("b.txt"), "tail\n").await?;
    let runner = ToolRunner::new(Arc::new(ToolContext::from(h.ctx.as_ref())));
    let grep = runner
        .execute_all(vec![tool_call(
            "Grep",
            "grep_hashline",
            json!({"pattern":"needle","path":"."}),
        )])
        .await?;
    let a_tag = crate::tools::snapshot::compute_file_tag("keep\nneedle\n");
    assert!(
        grep[0].content.contains(&format!("[a.txt#{a_tag}]")),
        "{}",
        grep[0].content
    );
    runner
        .execute_all(vec![tool_call("Read", "read_b", json!({"path":"b.txt"}))])
        .await?;
    let b_tag = crate::tools::snapshot::compute_file_tag("tail\n");
    let result = runner
        .execute_all(vec![tool_call(
            "Edit",
            "move_clipboard",
            json!({"input":format!("[a.txt#{a_tag}]\nCUT 2\n[b.txt#{b_tag}]\nPUT <1")}),
        )])
        .await?;
    assert!(result[0].succeeded(), "{}", result[0].content);
    let new_a_tag = crate::tools::snapshot::compute_file_tag("keep\n");
    let new_b_tag = crate::tools::snapshot::compute_file_tag("needle\ntail\n");
    assert!(result[0].content.contains(&format!("[a.txt#{new_a_tag}]")));
    assert!(result[0].content.contains(&format!("[b.txt#{new_b_tag}]")));
    assert_eq!(
        tokio::fs::read_to_string(h.cwd.join("a.txt")).await?,
        "keep\n"
    );
    assert_eq!(
        tokio::fs::read_to_string(h.cwd.join("b.txt")).await?,
        "needle\ntail\n"
    );
    Ok(())
}

#[tokio::test]
async fn hashline_grep_does_not_mark_truncated_context_as_seen() -> anyhow::Result<()> {
    let h = harness("hashline-grep-seen-boundary").await?;
    let content = format!("needle\n{}\n", "x".repeat(110_000));
    let path = h.cwd.join("wide.txt");
    tokio::fs::write(&path, &content).await?;
    let runner = ToolRunner::new(Arc::new(ToolContext::from(h.ctx.as_ref())));
    let result = runner
        .execute_all(vec![tool_call(
            "Grep",
            "grep_wide",
            json!({"pattern":"needle","path":".","context":1}),
        )])
        .await?;
    assert!(result[0].content.contains("1:needle"));
    assert!(!result[0].content.contains("2:"));

    let tag = crate::tools::snapshot::compute_file_tag(&content);
    let versions = h
        .ctx
        .snapshots
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .versions(&path, &tag);
    assert_eq!(versions.len(), 1);
    assert_eq!(
        versions[0].seen_lines,
        std::collections::BTreeSet::from([1])
    );
    Ok(())
}

#[tokio::test]
async fn replace_exact_fuzzy_and_all_flow() -> anyhow::Result<()> {
    let h = harness_with_config(
        "replace-flow",
        false,
        300,
        |config| config.edit_mode = crate::config::EditMode::Replace,
        None,
    )
    .await?;
    tokio::fs::write(h.cwd.join("replace.txt"), "alpha   \nbeta\nalpha   \n").await?;
    let runner = ToolRunner::new(Arc::new(ToolContext::from(h.ctx.as_ref())));
    let result = runner
        .execute_all(vec![tool_call(
            "Edit",
            "replace_all",
            json!({
                "path":"replace.txt",
                "edits":[
                    {"old_text":"alpha   ", "new_text":"ALPHA", "all":true},
                    {"old_text":"beta ", "new_text":"BETA"}
                ]
            }),
        )])
        .await?;
    assert!(result[0].succeeded(), "{}", result[0].content);
    assert!(result[0].content.contains("firstChangedLine: 1"));
    assert!(result[0].content.contains("matchStrategy: exact"));
    assert!(result[0].content.contains("matchCount: 2"));
    assert!(result[0].content.contains("Diff:"));
    assert_eq!(result[0].conv_content, result[0].content);
    assert_eq!(
        tokio::fs::read_to_string(h.cwd.join("replace.txt")).await?,
        "ALPHA\nBETA\nALPHA\n"
    );
    Ok(())
}

#[tokio::test]
async fn hashline_named_register_persists_but_anonymous_register_does_not() -> anyhow::Result<()> {
    let h = harness("hashline-register-lifetime").await?;
    tokio::fs::write(h.cwd.join("named.txt"), "saved\ntail\n").await?;
    tokio::fs::write(h.cwd.join("anonymous.txt"), "local\ntail\n").await?;
    tokio::fs::write(h.cwd.join("target.txt"), "target\n").await?;
    let runner = ToolRunner::new(Arc::new(ToolContext::from(h.ctx.as_ref())));
    runner
        .execute_all(vec![
            tool_call("Read", "read_named", json!({"path":"named.txt"})),
            tool_call("Read", "read_anonymous", json!({"path":"anonymous.txt"})),
            tool_call("Read", "read_target", json!({"path":"target.txt"})),
        ])
        .await?;
    let named_tag = crate::tools::snapshot::compute_file_tag("saved\ntail\n");
    let anonymous_tag = crate::tools::snapshot::compute_file_tag("local\ntail\n");
    let target_tag = crate::tools::snapshot::compute_file_tag("target\n");

    let cut_named = runner
        .execute_all(vec![tool_call(
            "Edit",
            "cut_named",
            json!({"input":format!("[named.txt#{named_tag}]\nCUT 1 @saved")}),
        )])
        .await?;
    assert!(cut_named[0].succeeded(), "{}", cut_named[0].content);
    let paste_named = runner
        .execute_all(vec![tool_call(
            "Edit",
            "paste_named",
            json!({"input":format!("[target.txt#{target_tag}]\nPUT <1 @saved")}),
        )])
        .await?;
    assert!(paste_named[0].succeeded(), "{}", paste_named[0].content);
    assert_eq!(
        tokio::fs::read_to_string(h.cwd.join("target.txt")).await?,
        "saved\ntarget\n"
    );

    let cut_anonymous = runner
        .execute_all(vec![tool_call(
            "Edit",
            "cut_anonymous",
            json!({"input":format!("[anonymous.txt#{anonymous_tag}]\nCUT 1")}),
        )])
        .await?;
    assert!(cut_anonymous[0].succeeded(), "{}", cut_anonymous[0].content);
    let new_target_tag = crate::tools::snapshot::compute_file_tag("saved\ntarget\n");
    let paste_anonymous = runner
        .execute_all(vec![tool_call(
            "Edit",
            "paste_anonymous",
            json!({"input":format!("[target.txt#{new_target_tag}]\nPUT >$")}),
        )])
        .await?;
    assert!(!paste_anonymous[0].succeeded());
    assert!(paste_anonymous[0].content.contains("prior unlabeled CUT"));
    Ok(())
}

#[tokio::test]
async fn hashline_path_recovery_requires_both_filename_and_tag() -> anyhow::Result<()> {
    let h = harness("hashline-path-recovery").await?;
    tokio::fs::create_dir_all(h.cwd.join("pkg")).await?;
    tokio::fs::write(h.cwd.join("pkg/file.txt"), "one\ntwo\n").await?;
    let runner = ToolRunner::new(Arc::new(ToolContext::from(h.ctx.as_ref())));
    runner
        .execute_all(vec![tool_call(
            "Read",
            "read_nested",
            json!({"path":"pkg/file.txt"}),
        )])
        .await?;
    let tag = crate::tools::snapshot::compute_file_tag("one\ntwo\n");
    let wrong_name = runner
        .execute_all(vec![tool_call(
            "Edit",
            "wrong_name",
            json!({"input":format!("[other.txt#{tag}]\nPUT 2:\n+TWO")}),
        )])
        .await?;
    assert!(!wrong_name[0].succeeded());
    assert_eq!(
        tokio::fs::read_to_string(h.cwd.join("pkg/file.txt")).await?,
        "one\ntwo\n"
    );

    let recovered = runner
        .execute_all(vec![tool_call(
            "Edit",
            "recover_name",
            json!({"input":format!("[file.txt#{tag}]\nPUT 2:\n+TWO")}),
        )])
        .await?;
    assert!(recovered[0].succeeded(), "{}", recovered[0].content);
    assert!(recovered[0].content.contains("matched its filename"));
    assert_eq!(
        tokio::fs::read_to_string(h.cwd.join("pkg/file.txt")).await?,
        "one\nTWO\n"
    );
    Ok(())
}

#[tokio::test]
async fn hashline_move_preflight_and_edit_then_move_are_safe() -> anyhow::Result<()> {
    let h = harness("hashline-move").await?;
    tokio::fs::write(h.cwd.join("a.txt"), "a\n").await?;
    tokio::fs::write(h.cwd.join("b.txt"), "b\n").await?;
    let runner = ToolRunner::new(Arc::new(ToolContext::from(h.ctx.as_ref())));
    runner
        .execute_all(vec![
            tool_call("Read", "read_a", json!({"path":"a.txt"})),
            tool_call("Read", "read_b", json!({"path":"b.txt"})),
        ])
        .await?;
    let a_tag = crate::tools::snapshot::compute_file_tag("a\n");
    let b_tag = crate::tools::snapshot::compute_file_tag("b\n");
    let conflict = runner
        .execute_all(vec![tool_call(
            "Edit",
            "move_conflict",
            json!({"input":format!("[a.txt#{a_tag}]\nMV same.txt\n[b.txt#{b_tag}]\nMV same.txt")}),
        )])
        .await?;
    assert!(!conflict[0].succeeded());
    assert!(h.cwd.join("a.txt").exists());
    assert!(h.cwd.join("b.txt").exists());
    assert!(!h.cwd.join("same.txt").exists());

    let moved = runner
        .execute_all(vec![tool_call(
            "Edit",
            "edit_then_move",
            json!({"input":format!("[a.txt#{a_tag}]\nPUT 1:\n+A\nMV moved.txt")}),
        )])
        .await?;
    assert!(moved[0].succeeded(), "{}", moved[0].content);
    let moved_tag = crate::tools::snapshot::compute_file_tag("A\n");
    assert!(moved[0].content.contains("Edit(a.txt): moved -> moved.txt"));
    assert!(
        moved[0]
            .content
            .contains(&format!("[moved.txt#{moved_tag}]"))
    );
    assert!(moved[0].content.contains("firstChangedLine: 1"));
    assert!(!h.cwd.join("a.txt").exists());
    assert_eq!(
        tokio::fs::read_to_string(h.cwd.join("moved.txt")).await?,
        "A\n"
    );
    Ok(())
}

#[tokio::test]
async fn hashline_remove_reports_removed_status_and_diff() -> anyhow::Result<()> {
    let h = harness("hashline-remove-result").await?;
    tokio::fs::write(h.cwd.join("remove.txt"), "gone\n").await?;
    let runner = ToolRunner::new(Arc::new(ToolContext::from(h.ctx.as_ref())));
    runner
        .execute_all(vec![tool_call(
            "Read",
            "read_remove",
            json!({"path":"remove.txt"}),
        )])
        .await?;
    let tag = crate::tools::snapshot::compute_file_tag("gone\n");
    let result = runner
        .execute_all(vec![tool_call(
            "Edit",
            "remove_file",
            json!({"input":format!("[remove.txt#{tag}]\nREM")}),
        )])
        .await?;
    assert!(result[0].succeeded(), "{}", result[0].content);
    assert!(result[0].content.contains("Edit(remove.txt): removed"));
    assert!(result[0].content.contains("linesRemoved: 1"));
    assert!(result[0].content.contains("Diff:"));
    assert!(!h.cwd.join("remove.txt").exists());
    Ok(())
}

#[tokio::test]
async fn replace_enforces_limit_after_crlf_shape_restoration() -> anyhow::Result<()> {
    let h = harness_with_config(
        "replace-crlf-size",
        false,
        300,
        |config| {
            config.edit_mode = crate::config::EditMode::Replace;
            config.file_write_max_bytes = 5;
        },
        None,
    )
    .await?;
    tokio::fs::write(h.cwd.join("crlf.txt"), b"a\r\nb").await?;
    let runner = ToolRunner::new(Arc::new(ToolContext::from(h.ctx.as_ref())));
    let result = runner
        .execute_all(vec![tool_call(
            "Edit",
            "replace_crlf",
            json!({
                "path":"crlf.txt",
                "edits":[{"old_text":"b", "new_text":"c\n"}]
            }),
        )])
        .await?;
    assert!(!result[0].succeeded());
    assert!(result[0].content.contains("file_write_max_bytes"));
    assert_eq!(tokio::fs::read(h.cwd.join("crlf.txt")).await?, b"a\r\nb");
    Ok(())
}

#[tokio::test]
async fn hashline_grep_handles_maximum_context_without_overflow() -> anyhow::Result<()> {
    let h = harness("hashline-max-context").await?;
    tokio::fs::write(h.cwd.join("context.txt"), "before\nneedle\nafter\n").await?;
    let runner = ToolRunner::new(Arc::new(ToolContext::from(h.ctx.as_ref())));
    let result = runner
        .execute_all(vec![tool_call(
            "Grep",
            "grep_max_context",
            json!({"pattern":"needle", "path":".", "context":usize::MAX}),
        )])
        .await?;
    assert!(result[0].succeeded(), "{}", result[0].content);
    assert!(result[0].content.contains("1:before"));
    assert!(result[0].content.contains("3:after"));
    Ok(())
}

#[tokio::test]
async fn hashline_truncated_seen_line_error_does_not_grant_retry() -> anyhow::Result<()> {
    let h = harness_with_config(
        "hashline-seen-error-limit",
        false,
        300,
        |config| {
            config.edit_enforce_seen_lines = true;
            config.tool_result_max_bytes = 100;
        },
        None,
    )
    .await?;
    let hidden = "x".repeat(60);
    let content = format!("shown\n{hidden}\n");
    tokio::fs::write(h.cwd.join("seen.txt"), &content).await?;
    let runner = ToolRunner::new(Arc::new(ToolContext::from(h.ctx.as_ref())));
    let read = runner
        .execute_all(vec![tool_call(
            "Read",
            "read_seen_one",
            json!({"path":"seen.txt:1-1"}),
        )])
        .await?;
    assert!(read[0].succeeded(), "{}", read[0].content);
    let tag = crate::tools::snapshot::compute_file_tag(&content);
    let input = format!("[seen.txt#{tag}]\nPUT 2:\n+changed");
    for call_id in ["seen_first", "seen_retry"] {
        let result = runner
            .execute_all(vec![tool_call(
                "Edit",
                call_id,
                json!({"input":input.clone()}),
            )])
            .await?;
        assert!(!result[0].succeeded());
        assert!(
            result[0].content.contains("truncated"),
            "{}",
            result[0].content
        );
    }
    assert_eq!(
        tokio::fs::read_to_string(h.cwd.join("seen.txt")).await?,
        content
    );
    Ok(())
}

#[tokio::test]
async fn read_rejects_unknown_fields_with_expected_message() -> anyhow::Result<()> {
    let h = harness_with_config("read-unknown-field", false, 300, |_| {}, None).await?;
    let runner = ToolRunner::new(Arc::new(ToolContext::from(h.ctx.as_ref())));
    let result = runner
        .execute_all(vec![tool_call(
            "Read",
            "read_unknown",
            json!({"path": "a.md", "selector": "1-2"}),
        )])
        .await?;
    assert!(!result[0].succeeded());
    assert!(
        result[0].content.contains("unknown field") && result[0].content.contains("path"),
        "{}",
        result[0].content
    );
    Ok(())
}

#[tokio::test]
async fn read_empty_selector_reports_helpful_error() -> anyhow::Result<()> {
    let h = harness_with_config("read-empty-selector", false, 300, |_| {}, None).await?;
    let runner = ToolRunner::new(Arc::new(ToolContext::from(h.ctx.as_ref())));
    let result = runner
        .execute_all(vec![tool_call(
            "Read",
            "read_empty_sel",
            json!({"path": ":45-50"}),
        )])
        .await?;
    assert!(!result[0].succeeded());
    assert!(
        result[0].content.contains("must be appended to a path"),
        "{}",
        result[0].content
    );
    Ok(())
}

#[tokio::test]
async fn read_oversized_file_returns_preview_with_selector_guidance() -> anyhow::Result<()> {
    let h = harness_with_config(
        "read-preview",
        false,
        300,
        |config| {
            config.tool_result_max_bytes = 200_000;
        },
        None,
    )
    .await?;
    let content = (1..=30_000)
        .map(|i| format!("line {i}"))
        .collect::<Vec<_>>()
        .join("\n");
    tokio::fs::write(h.cwd.join("big.txt"), &content).await?;
    let runner = ToolRunner::new(Arc::new(ToolContext::from(h.ctx.as_ref())));
    let result = runner
        .execute_all(vec![tool_call(
            "Read",
            "read_big",
            json!({"path": "big.txt"}),
        )])
        .await?;
    assert!(result[0].succeeded(), "{}", result[0].content);
    assert!(
        result[0].content.contains("file too large")
            && result[0].content.contains("showing first 200")
            && result[0].content.contains("more than 200 lines")
            && result[0].content.contains("start-end")
            && result[0].content.contains("1:line 1")
            && result[0].content.contains("line 30000")
            && result[0].content.contains("\n...\n"),
        "{}",
        result[0].content
    );
    // A range read still works on the same file.
    let ranged = runner
        .execute_all(vec![tool_call(
            "Read",
            "read_big_range",
            json!({"path": "big.txt:1-2"}),
        )])
        .await?;
    assert!(ranged[0].succeeded(), "{}", ranged[0].content);
    assert!(ranged[0].content.contains("line 1"));
    Ok(())
}

#[tokio::test]
async fn read_missing_file_suggests_glob() -> anyhow::Result<()> {
    let h = harness_with_config("read-missing", false, 300, |_| {}, None).await?;
    let runner = ToolRunner::new(Arc::new(ToolContext::from(h.ctx.as_ref())));
    let result = runner
        .execute_all(vec![tool_call(
            "Read",
            "read_missing",
            json!({"path": "missing.txt"}),
        )])
        .await?;
    assert!(!result[0].succeeded());
    assert!(
        result[0].content.contains("Use Glob(pattern="),
        "{}",
        result[0].content
    );
    Ok(())
}

#[tokio::test]
async fn read_memo_short_circuits_repeated_read_and_write_invalidates() -> anyhow::Result<()> {
    let h = harness_with_config("read-memo", false, 300, |_| {}, None).await?;
    tokio::fs::write(h.cwd.join("a.md"), "line 1\nline 2\n").await?;
    let runner = ToolRunner::new(Arc::new(ToolContext::from(h.ctx.as_ref())));
    let first = runner
        .execute_all(vec![tool_call(
            "Read",
            "memo_first",
            json!({"path": "a.md"}),
        )])
        .await?;
    assert!(first[0].succeeded(), "{}", first[0].content);
    assert!(
        first[0].content.contains("1:line 1"),
        "{}",
        first[0].content
    );

    // Identical full read hits the memo and returns a short "reuse" response.
    let second = runner
        .execute_all(vec![tool_call(
            "Read",
            "memo_second",
            json!({"path": "a.md"}),
        )])
        .await?;
    assert!(second[0].succeeded(), "{}", second[0].content);
    assert!(
        second[0].content.contains("unchanged, no edits since")
            && second[0].content.contains("Reuse that content")
            && !second[0].content.contains("1:line 1"),
        "{}",
        second[0].content
    );

    // A successful Write invalidates the memo; the next read returns full content.
    let written = runner
        .execute_all(vec![tool_call(
            "Write",
            "memo_write",
            json!({"path": "a.md", "content": "changed\n"}),
        )])
        .await?;
    assert!(written[0].succeeded(), "{}", written[0].content);
    let third = runner
        .execute_all(vec![tool_call(
            "Read",
            "memo_third",
            json!({"path": "a.md"}),
        )])
        .await?;
    assert!(
        third[0].content.contains("1:changed") && !third[0].content.contains("Reuse that content"),
        "{}",
        third[0].content
    );
    Ok(())
}

#[tokio::test]
async fn write_reports_json_validity_note() -> anyhow::Result<()> {
    let h = harness_with_config("write-json-note", false, 300, |_| {}, None).await?;
    let runner = ToolRunner::new(Arc::new(ToolContext::from(h.ctx.as_ref())));
    let broken = runner
        .execute_all(vec![tool_call(
            "Write",
            "json_broken",
            json!({"path": "config.json", "content": "{\n  \"key\": \"value\"\n"}),
        )])
        .await?;
    assert!(broken[0].succeeded(), "{}", broken[0].content);
    assert!(
        broken[0].content.contains("JSON parse failed at line"),
        "{}",
        broken[0].content
    );

    let valid = runner
        .execute_all(vec![tool_call(
            "Write",
            "json_valid",
            json!({"path": "config.json", "content": "{\n  \"key\": \"value\"\n}\n"}),
        )])
        .await?;
    assert!(valid[0].succeeded(), "{}", valid[0].content);
    assert!(
        valid[0].content.contains("JSON parse: ok"),
        "{}",
        valid[0].content
    );

    // Non-JSON targets get no note.
    let plain = runner
        .execute_all(vec![tool_call(
            "Write",
            "json_plain",
            json!({"path": "notes.txt", "content": "hello"}),
        )])
        .await?;
    assert!(
        !plain[0].content.contains("JSON parse"),
        "{}",
        plain[0].content
    );
    Ok(())
}

#[tokio::test]
async fn bash_result_includes_exec_metadata_header() -> anyhow::Result<()> {
    let h = harness_with_config("bash-header", false, 300, |_| {}, None).await?;
    let runner = ToolRunner::new(Arc::new(ToolContext::from(h.ctx.as_ref())));
    let result = runner
        .execute_all(vec![tool_call(
            "Bash",
            "bash_header",
            json!({"command": "echo hi"}),
        )])
        .await?;
    assert!(result[0].succeeded(), "{}", result[0].content);
    assert!(
        result[0].content.starts_with("Exit code: 0")
            || result[0].content.starts_with("Exit code:0"),
        "{}",
        result[0].content
    );
    assert!(
        result[0].content.contains("Wall time:"),
        "{}",
        result[0].content
    );
    Ok(())
}

#[tokio::test]
async fn edit_already_applied_patch_returns_idempotent_success() -> anyhow::Result<()> {
    let h = harness_with_config("edit-idempotent", false, 300, |_| {}, None).await?;
    tokio::fs::write(h.cwd.join("a.md"), "one\ntwo\nthree\n").await?;
    let runner = ToolRunner::new(Arc::new(ToolContext::from(h.ctx.as_ref())));
    let read = runner
        .execute_all(vec![tool_call(
            "Read",
            "idem_read",
            json!({"path": "a.md"}),
        )])
        .await?;
    assert!(read[0].succeeded(), "{}", read[0].content);
    let tag = read[0]
        .content
        .split_once('#')
        .and_then(|(_, rest)| rest.get(..4))
        .expect("hashline read header tag")
        .to_string();
    let patch_body = "PUT 2.=2:\n+CHANGED";
    let patch = format!("[a.md#{tag}]\n{patch_body}");
    let first = runner
        .execute_all(vec![tool_call(
            "Edit",
            "idem_edit1",
            json!({"input": patch}),
        )])
        .await?;
    assert!(first[0].succeeded(), "{}", first[0].content);
    // Re-read for the current tag, then retry the same body: idempotent success.
    let reread = runner
        .execute_all(vec![tool_call(
            "Read",
            "idem_reread",
            json!({"path": "a.md"}),
        )])
        .await?;
    assert!(reread[0].succeeded(), "{}", reread[0].content);
    let current_tag = reread[0]
        .content
        .split_once('#')
        .and_then(|(_, rest)| rest.get(..4))
        .expect("hashline read header tag")
        .to_string();
    let retry = runner
        .execute_all(vec![tool_call(
            "Edit",
            "idem_edit2",
            json!({"input": format!("[a.md#{current_tag}]\n{patch_body}")}),
        )])
        .await?;
    assert!(retry[0].succeeded(), "{}", retry[0].content);
    assert!(
        retry[0].content.contains("already applied (idempotent)"),
        "{}",
        retry[0].content
    );
    Ok(())
}
