use super::*;

/// Run one trace fixture (JSON: setup.files/config, steps, asserts).
/// `{{PATCH_BODY}}` is substituted with the latest Read's hashline tag so the
/// fixture retries the same body against the current snapshot.
async fn run_trace_fixture(fixture: &str) -> anyhow::Result<()> {
    let spec: serde_json::Value = serde_json::from_str(fixture)?;
    let name = spec["name"].as_str().unwrap_or("fixture");
    let files = spec["setup"]["files"]
        .as_object()
        .cloned()
        .unwrap_or_default();
    let config = spec["setup"]["config"]
        .as_object()
        .cloned()
        .unwrap_or_default();
    let mut max_bytes = None;
    let mut enabled: Option<Vec<String>> = None;
    if let Some(value) = config.get("tool_result_max_bytes").and_then(|v| v.as_u64()) {
        max_bytes = Some(value as usize);
    }
    if let Some(list) = config.get("enabled_tools").and_then(|v| v.as_array()) {
        enabled = Some(
            list.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect(),
        );
    }
    let h = harness_with_config(
        &format!("trace-{name}"),
        false,
        300,
        |cfg| {
            if let Some(bytes) = max_bytes {
                cfg.tool_result_max_bytes = bytes;
            }
            if let Some(names) = &enabled {
                cfg.enabled_tools = Some(names.clone());
            }
        },
        None,
    )
    .await?;
    for (path, content) in &files {
        let content = match content.as_str() {
            // 真实样本：0b3d46c0 方案.md 2094 行 / 154707 字节（全库 89 次 too-large）。
            Some("{{BIG_FILE}}") => {
                let mut body = String::new();
                for i in 1..=2094 {
                    body.push_str(&format!("line {i} 施工方案内容\n"));
                }
                let target = 154_707usize;
                if body.len() < target {
                    body.push_str(&"x".repeat(target - body.len()));
                } else {
                    body.truncate(target);
                }
                body
            }
            // 真实样本：1b777dd7 compliance_report.md（196 行报告形态）。
            Some("{{REPORT_FILE}}") => (1..=200).map(|i| format!("line {i} 报告内容\n")).collect(),
            _ => content.as_str().unwrap_or_default().to_string(),
        };
        tokio::fs::write(h.cwd.join(path), content).await?;
    }
    let runner = ToolRunner::new(Arc::new(ToolContext::from(h.ctx.as_ref())));
    let mut latest_read: Option<(String, String)> = None;
    let steps = spec["steps"].as_array().cloned().unwrap_or_default();
    let mut outputs = Vec::new();
    for step in &steps {
        let tool = step["tool"].as_str().expect("fixture tool name");
        let mut input = step["input"].clone();
        if input
            .get("input")
            .and_then(|v| v.as_str())
            .is_some_and(|body| body.contains("{{PATCH_BODY}}"))
        {
            let (path, tag) = latest_read
                .clone()
                .expect("{{PATCH_BODY}} requires a prior Read for the tag");
            *input.get_mut("input").unwrap() =
                serde_json::Value::String(format!("[{path}#{tag}]\nPUT 1.=1:\n+same"));
        }
        let read_path = input
            .get("path")
            .and_then(|v| v.as_str())
            .map(str::to_string);
        let result = runner
            .execute_all(vec![tool_call(tool, &format!("{name}_step"), input)])
            .await?;
        outputs.push((result[0].succeeded(), result[0].content.clone()));
        if tool == "Read" {
            latest_read = result[0]
                .content
                .split_once('#')
                .and_then(|(_, rest)| rest.get(..4))
                .map(|tag| {
                    (
                        read_path.unwrap_or_else(|| "a.md".to_string()),
                        tag.to_string(),
                    )
                });
        }
    }
    for assert in spec["asserts"].as_array().cloned().unwrap_or_default() {
        let index = assert["after_step"].as_u64().expect("after_step") as usize;
        let (success, content) = &outputs[index];
        if let Some(expected_success) = assert.get("success").and_then(|v| v.as_bool()) {
            assert_eq!(
                *success, expected_success,
                "{name}: step {index} success mismatch: {content}"
            );
        }
        if let Some(needle) = assert.get("contains").and_then(|v| v.as_str()) {
            assert!(
                content.contains(needle),
                "{name}: step {index} missing {needle:?}: {content}"
            );
        }
    }
    Ok(())
}

#[tokio::test]
async fn trace_fixtures_regress_behaviors() -> anyhow::Result<()> {
    run_trace_fixture(include_str!(
        "../../tests/fixtures/traces/repeated_read.json"
    ))
    .await?;
    run_trace_fixture(include_str!("../../tests/fixtures/traces/param_guess.json")).await?;
    run_trace_fixture(include_str!(
        "../../tests/fixtures/traces/no_change_loop.json"
    ))
    .await?;
    run_trace_fixture(include_str!(
        "../../tests/fixtures/traces/disabled_tool.json"
    ))
    .await?;
    run_trace_fixture(include_str!("../../tests/fixtures/traces/big_file.json")).await?;
    Ok(())
}

#[tokio::test]
async fn jsonl_and_multi_file_edit_validity_notes() -> anyhow::Result<()> {
    let h = harness_with_config("jsonl-notes", false, 300, |_| {}, None).await?;
    tokio::fs::write(h.cwd.join("a.jsonl"), "{\"a\":1}\n{\"b\":2}\n").await?;
    tokio::fs::write(h.cwd.join("bad.jsonl"), "{\"a\":1}\nnot json\n").await?;
    tokio::fs::write(h.cwd.join("a.json"), "{\"a\":1}\n").await?;
    tokio::fs::write(h.cwd.join("b.json"), "{\"b\":2}\n").await?;
    let runner = ToolRunner::new(Arc::new(ToolContext::from(h.ctx.as_ref())));
    // 合法 JSONL（多行独立 JSON）必须报 ok，而不是在第二行误判失败。
    let ok = runner
        .execute_all(vec![tool_call(
            "Write",
            "jsonl_ok",
            json!({"path": "a.jsonl", "content": "{\"a\":1}\n{\"b\":2}\n"}),
        )])
        .await?;
    assert!(
        ok[0].content.contains("JSON parse: ok"),
        "{}",
        ok[0].content
    );
    let bad = runner
        .execute_all(vec![tool_call(
            "Write",
            "jsonl_bad",
            json!({"path": "bad.jsonl", "content": "{\"a\":1}\nnot json\n"}),
        )])
        .await?;
    assert!(
        bad[0].content.contains("JSON parse failed at line 2"),
        "{}",
        bad[0].content
    );
    // 多 section Edit：每个 JSON 目标都要校验，不只第一个。
    for file in ["a.json", "b.json"] {
        let read = runner
            .execute_all(vec![tool_call(
                "Read",
                &format!("read_{file}"),
                json!({"path": file}),
            )])
            .await?;
        assert!(read[0].succeeded(), "{}", read[0].content);
    }
    let a_tag = crate::tools::snapshot::compute_file_tag("{\"a\":1}\n");
    let b_tag = crate::tools::snapshot::compute_file_tag("{\"b\":2}\n");
    let multi = runner
        .execute_all(vec![tool_call(
            "Edit",
            "multi_json",
            json!({"input": format!(
                "[a.json#{a_tag}]\nPUT 1.=1:\n+{{\"a\": 2}}\n[b.json#{b_tag}]\nPUT 1.=1:\n+{{\"b\": 3}}"
            )}),
        )])
        .await?;
    assert!(multi[0].succeeded(), "{}", multi[0].content);
    assert!(
        multi[0].content.contains("JSON parse: ok (a.json)")
            && multi[0].content.contains("JSON parse: ok (b.json)"),
        "{}",
        multi[0].content
    );
    Ok(())
}

#[tokio::test]
async fn tools_reject_unknown_fields_at_runtime() -> anyhow::Result<()> {
    // 外部审查 #6：schema 一致性必须落到 runtime——每个工具的 executor
    // 都必须拒绝未声明的字段（serde deny_unknown_fields）。
    let h = harness_with_config(
        "unknown-fields",
        false,
        300,
        |cfg| {
            // PythonSandbox is explicit-only: list every compiled tool so the
            // surface includes it and the executor (not the surface gate)
            // decides the unknown-field outcome for each tool.
            let names: Vec<String> = ToolCatalog::builtin()
                .unwrap()
                .iter_compiled()
                .map(|(_, metadata)| metadata.name.to_string())
                .collect();
            cfg.enabled_tools = Some(names);
        },
        None,
    )
    .await?;
    let names: Vec<String> = ToolCatalog::builtin()
        .unwrap()
        .iter_compiled()
        .map(|(_, metadata)| metadata.name.to_string())
        .collect();
    let runner = ToolRunner::new(Arc::new(ToolContext::from(h.ctx.as_ref())));
    for name in &names {
        let result = runner
            .execute_all(vec![tool_call(
                name,
                &format!("unknown_{name}"),
                json!({"__unknown_field__": 1}),
            )])
            .await?;
        assert!(
            !result[0].succeeded(),
            "{name}: unknown field was accepted: {}",
            result[0].content
        );
        assert!(
            result[0].content.contains("unknown field"),
            "{name}: error does not name the unknown field: {}",
            result[0].content
        );
    }
    Ok(())
}

#[tokio::test]
async fn failed_read_does_not_seed_memo() -> anyhow::Result<()> {
    // 外部审查 #2：失败的范围 Read（Hashline 输出超限）不得写入 memo，
    // 否则第二次相同 Read 会让模型“复用”从未收到的内容。
    let h = harness_with_config(
        "failed-read-memo",
        false,
        300,
        |config| config.tool_result_max_bytes = 60,
        None,
    )
    .await?;
    tokio::fs::write(h.cwd.join("wide.txt"), "abcdefghij\n".repeat(5)).await?;
    let runner = ToolRunner::new(Arc::new(ToolContext::from(h.ctx.as_ref())));
    for call_id in ["failed_1", "failed_2"] {
        let result = runner
            .execute_all(vec![tool_call(
                "Read",
                call_id,
                json!({"path": "wide.txt"}),
            )])
            .await?;
        assert!(
            !result[0].succeeded(),
            "{call_id}: oversized Hashline read unexpectedly succeeded: {}",
            result[0].content
        );
        assert!(
            !result[0].content.contains("unchanged, no edits since"),
            "{call_id}: memo was seeded by a failed read: {}",
            result[0].content
        );
    }
    Ok(())
}

#[tokio::test]
async fn sub_agent_accepts_declared_schema_fields() -> anyhow::Result<()> {
    // 外部审查：SubAgent schema 声明 prompt/description/fork，executor 必须
    // 接受全部合法字段（deny_unknown_fields 不能误伤合法调用）。
    let h = harness_with_config("subagent-schema", false, 300, |_| {}, None).await?;
    let runner = ToolRunner::new(Arc::new(ToolContext::from(h.ctx.as_ref())));
    let result = runner
        .execute_all(vec![tool_call(
            "SubAgent",
            "sub_schema",
            json!({"prompt": "do the work", "description": "schema check", "fork": true}),
        )])
        .await?;
    assert!(result[0].succeeded(), "{}", result[0].content);
    Ok(())
}

#[tokio::test]
async fn read_memo_distinguishes_raw_and_non_raw() -> anyhow::Result<()> {
    // raw 读与 non-raw 读不得共享 memo：raw:1-20 后 non-raw 1-20 必须返回
    // 带行号/header 的完整输出，而不是 "reuse" 短响应。
    let h = harness_with_config("memo-raw", false, 300, |_| {}, None).await?;
    tokio::fs::write(h.cwd.join("a.md"), "one\ntwo\nthree\n").await?;
    let runner = ToolRunner::new(Arc::new(ToolContext::from(h.ctx.as_ref())));
    let raw = runner
        .execute_all(vec![tool_call(
            "Read",
            "raw_first",
            json!({"path": "a.md:raw:1-2"}),
        )])
        .await?;
    assert!(raw[0].succeeded(), "{}", raw[0].content);
    let non_raw = runner
        .execute_all(vec![tool_call(
            "Read",
            "non_raw_after_raw",
            json!({"path": "a.md:1-2"}),
        )])
        .await?;
    assert!(non_raw[0].succeeded(), "{}", non_raw[0].content);
    assert!(
        non_raw[0].content.contains("1:one") && !non_raw[0].content.contains("Reuse that content"),
        "non-raw read must not hit a raw memo: {}",
        non_raw[0].content
    );
    // non-raw 读之后，相同 non-raw 读命中 memo。
    let second = runner
        .execute_all(vec![tool_call(
            "Read",
            "non_raw_second",
            json!({"path": "a.md:1-2"}),
        )])
        .await?;
    assert!(
        second[0].content.contains("Reuse that content"),
        "{}",
        second[0].content
    );
    Ok(())
}

#[tokio::test]
async fn oversized_raw_read_does_not_seed_memo() -> anyhow::Result<()> {
    // raw 输出超限（Replace 模式 / 超 editable limit，无 full_text 路径）
    // 必须拒绝且不写 memo，第二次相同 raw 读仍完整执行。
    let h = harness_with_config(
        "memo-raw-oversize",
        false,
        300,
        |cfg| cfg.tool_result_max_bytes = 60,
        None,
    )
    .await?;
    tokio::fs::write(h.cwd.join("wide.txt"), "abcdefghij\n".repeat(7)).await?;
    let runner = ToolRunner::new(Arc::new(ToolContext::from(h.ctx.as_ref())));
    for call_id in ["raw_1", "raw_2"] {
        let result = runner
            .execute_all(vec![tool_call(
                "Read",
                call_id,
                json!({"path": "wide.txt:raw"}),
            )])
            .await?;
        // Full raw reads beyond the budget are answered with the preview
        // (success, no memo); the key assertion is that no memo is seeded,
        // so the second identical read still performs the full path instead
        // of asking the model to "reuse" truncated content.
        assert!(
            !result[0].content.contains("Reuse that content"),
            "{call_id}: raw memo seeded by an oversized read: {}",
            result[0].content
        );
        // The preview itself is subject to the same truncation protection, so
        // only the memo-free guarantee is asserted here.
        assert!(
            !result[0].content.contains("1:abcdefghij"),
            "{call_id}: raw content should not be served: {}",
            result[0].content
        );
    }
    Ok(())
}

#[tokio::test]
async fn replace_idempotent_edit_skips_write_and_keeps_memo() -> anyhow::Result<()> {
    // 幂等 Replace 不得写盘（mtime 不变）、不得报告 updated、不得 bump
    // mutation（同一文件后续 Read 仍命中 memo）。
    let h = harness_with_config(
        "replace-idem-write",
        false,
        300,
        |cfg| cfg.edit_mode = crate::config::EditMode::Replace,
        None,
    )
    .await?;
    let path = h.cwd.join("a.txt");
    tokio::fs::write(&path, "alpha beta gamma\n").await?;
    let before = tokio::fs::metadata(&path).await?.modified()?;
    let runner = ToolRunner::new(Arc::new(ToolContext::from(h.ctx.as_ref())));
    // 先读一次以获得 memo 条目。
    let read = runner
        .execute_all(vec![tool_call(
            "Read",
            "idem_read",
            json!({"path": "a.txt"}),
        )])
        .await?;
    assert!(read[0].succeeded(), "{}", read[0].content);
    // 幂等替换（fuzzy 候选存在，old==new）。
    let edit = runner
        .execute_all(vec![tool_call(
            "Edit",
            "idem_edit",
            json!({
                "path": "a.txt",
                "edits": [{"old_text": "beta", "new_text": "beta"}]
            }),
        )])
        .await?;
    assert!(edit[0].succeeded(), "{}", edit[0].content);
    assert!(
        edit[0].content.contains("already applied (idempotent)"),
        "{}",
        edit[0].content
    );
    assert!(!edit[0].content.contains("updated"), "{}", edit[0].content);
    let after = tokio::fs::metadata(&path).await?.modified()?;
    assert_eq!(before, after, "idempotent edit must not rewrite the file");
    // mutation 未 bump：memo 仍有效。
    let second_read = runner
        .execute_all(vec![tool_call(
            "Read",
            "idem_read2",
            json!({"path": "a.txt"}),
        )])
        .await?;
    assert!(
        second_read[0].content.contains("Reuse that content"),
        "memo must survive an idempotent edit: {}",
        second_read[0].content
    );
    Ok(())
}

#[tokio::test]
async fn json_note_stays_within_result_budget() -> anyhow::Result<()> {
    // JSON 注记必须与正文一起经过统一 formatter：输出 + 注记 ≤ 预算。
    let h = harness_with_config(
        "json-note-budget",
        false,
        300,
        |cfg| cfg.tool_result_max_bytes = 300,
        None,
    )
    .await?;
    let runner = ToolRunner::new(Arc::new(ToolContext::from(h.ctx.as_ref())));
    // 内容接近预算上限，注记追加后仍须受保护。
    let payload = format!("{{\"key\":\"{}\"}}\n", "x".repeat(240));
    let result = runner
        .execute_all(vec![tool_call(
            "Write",
            "json_note_big",
            json!({"path": "big.json", "content": payload}),
        )])
        .await?;
    assert!(result[0].succeeded(), "{}", result[0].content);
    assert!(
        result[0].content.len() <= 300 + 100,
        "output + JSON note exceeds budget: {} bytes: {}",
        result[0].content.len(),
        result[0].content
    );
    assert!(
        result[0].content.contains("JSON parse"),
        "note missing: {}",
        result[0].content
    );
    Ok(())
}

#[tokio::test]
async fn memo_not_seeded_when_composed_output_exceeds_budget() -> anyhow::Result<()> {
    // 长路径 + 接近上限：rendered 内容本身 ≤ max，但 runner 追加的摘要使
    // composed 超限并截断——memo 不得记录，第二次相同读必须完整执行。
    let h = harness_with_config(
        "memo-composed-budget",
        false,
        300,
        |cfg| cfg.tool_result_max_bytes = 200,
        None,
    )
    .await?;
    let long_dir = "d".repeat(70);
    let dir = h.cwd.join(&long_dir);
    tokio::fs::create_dir_all(&dir).await?;
    let content = (1..=3)
        .map(|i| format!("line {i} {}", "x".repeat(40)))
        .collect::<Vec<_>>()
        .join("\n")
        + "\n";
    tokio::fs::write(dir.join("f.txt"), &content).await?;
    let runner = ToolRunner::new(Arc::new(ToolContext::from(h.ctx.as_ref())));
    for call_id in ["memo_c1", "memo_c2"] {
        let result = runner
            .execute_all(vec![tool_call(
                "Read",
                call_id,
                json!({"path": format!("{long_dir}/f.txt")}),
            )])
            .await?;
        assert!(
            !result[0].content.contains("Reuse that content"),
            "{call_id}: memo seeded despite truncated composed output: {}",
            result[0].content
        );
    }
    Ok(())
}

#[tokio::test]
async fn hashline_idempotent_edit_keeps_memo_valid() -> anyhow::Result<()> {
    // hashline 幂等成功不写盘、不 bump mutation：同一文件后续 Read 仍命中 memo。
    let h = harness_with_config("hashline-idem-memo", false, 300, |_| {}, None).await?;
    tokio::fs::write(h.cwd.join("a.md"), "one\ntwo\nthree\n").await?;
    let runner = ToolRunner::new(Arc::new(ToolContext::from(h.ctx.as_ref())));
    let read = runner
        .execute_all(vec![tool_call("Read", "h_read", json!({"path": "a.md"}))])
        .await?;
    assert!(read[0].succeeded(), "{}", read[0].content);
    let tag = read[0]
        .content
        .split_once('#')
        .and_then(|(_, rest)| rest.get(..4))
        .expect("hashline read header tag")
        .to_string();
    let edit = runner
        .execute_all(vec![tool_call(
            "Edit",
            "h_idem",
            json!({"input": format!("[a.md#{tag}]\nPUT 1.=1:\n+one")}),
        )])
        .await?;
    assert!(edit[0].succeeded(), "{}", edit[0].content);
    assert!(
        edit[0].content.contains("already applied (idempotent)"),
        "{}",
        edit[0].content
    );
    let second = runner
        .execute_all(vec![tool_call("Read", "h_read2", json!({"path": "a.md"}))])
        .await?;
    assert!(
        second[0].content.contains("Reuse that content"),
        "hashline idempotent must not invalidate memos: {}",
        second[0].content
    );
    Ok(())
}
