use super::*;
use tokio;

fn temp_store() -> ConversationStore {
    use std::sync::atomic::{AtomicU64, Ordering};
    static CNT: AtomicU64 = AtomicU64::new(0);
    let n = CNT.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("conv-test-{}-{}", std::process::id(), n));
    let _ = std::fs::create_dir_all(&dir);
    ConversationStore::new(dir.join("conversation.jsonl"))
}

#[tokio::test]
async fn add_user_and_read_back() {
    let store = temp_store();
    store.ensure().await.unwrap();
    store.add_user("hello").await.unwrap();
    let lines = store.lines().await.unwrap();
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0]["role"], "user");
    assert_eq!(lines[0]["content"], "hello");
}

#[tokio::test]
async fn add_assistant_with_thinking_and_text() {
    let store = temp_store();
    store.ensure().await.unwrap();
    let calls = vec![];
    store
        .add_assistant("response", "thinking...", &calls)
        .await
        .unwrap();
    let lines = store.lines().await.unwrap();
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0]["role"], "assistant");
    let content = lines[0]["content"].as_array().unwrap();
    assert_eq!(content[0]["type"], "thinking");
    assert_eq!(content[0]["thinking"], "thinking...");
    assert_eq!(content[1]["type"], "text");
    assert_eq!(content[1]["text"], "response");
}

#[tokio::test]
async fn add_tool_results_then_lines() {
    let store = temp_store();
    store.ensure().await.unwrap();
    let mut result =
        crate::tools::runner::ToolExecution::test_result("id1", "TodoAdvance", "output");
    result.state_metadata = Some(json!({
        "todo_revision": 3,
        "todo_state_kind": "progress",
    }));
    let results = vec![result];
    store.add_tool_results(&results).await.unwrap();
    let lines = store.lines().await.unwrap();
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0]["role"], "user");
    assert_eq!(
        lines[0]["content"][0]["_mink"]["todo_revision"].as_u64(),
        Some(3)
    );
}

fn tool_call_event(id: &str) -> ToolCallEvent {
    ToolCallEvent {
        name: "Bash".into(),
        id: id.into(),
        input_json: json!({"command": "false"}),
        fields: Default::default(),
        parse_error: None,
    }
}

#[tokio::test]
async fn image_results_enter_history_like_text_results() {
    // Image read results are persisted exactly like text tool results: the
    // model-visible description text (tool_result) plus a plain-text
    // reference block. No pixel bytes are stored; the reference is a plain
    // text citation and is only removed by compaction.
    let store = temp_store();
    store.ensure().await.unwrap();
    let mut result =
        crate::tools::runner::ToolExecution::test_result("id_img", "Read", "img result");
    result.image_attachment = Some(crate::tools::image::ImageAttachment {
        image_id: "sha256:".to_string() + &"aa".repeat(32),
        format: crate::tools::image::ImageFormat::Png,
        width: 1024,
        height: 768,
        bytes: 118782,
        name: "page.png".to_string(),
    });
    store.add_tool_results(&[result]).await.unwrap();
    let lines = store.lines().await.unwrap();
    let content = lines[0]["content"].as_array().unwrap();
    // Text-equivalent: the tool_result carries the model-visible description.
    assert_eq!(content[0]["type"], "tool_result");
    assert_eq!(content[0]["content"], "img result");
    // Followed by the plain-text label and reference (metadata only).
    assert_eq!(content[1]["type"], "text");
    assert!(content[1]["text"].as_str().unwrap().contains("page.png"));
    assert_eq!(content[2]["type"], "tool_attachment");
    assert_eq!(content[2]["url"], "image://sha256:".to_string() + &"aa".repeat(32));
    assert_eq!(content[2]["bytes"], 118782);
    // No pixel data anywhere in the persisted message.
    let serialized = serde_json::to_string(&lines[0]).unwrap();
    assert!(!serialized.contains("base64"), "{serialized}");
}

#[tokio::test]
async fn repair_dangling_tool_uses_appends_synthetic_results() {
    let store = temp_store();
    store.ensure().await.unwrap();
    store.add_user("hi").await.unwrap();
    store
        .add_assistant("r", "", &[tool_call_event("id_a"), tool_call_event("id_b")])
        .await
        .unwrap();
    let results = vec![crate::tools::runner::ToolExecution::test_result(
        "id_a",
        "Bash",
        "Error: exit 1",
    )];
    store.add_tool_results(&results).await.unwrap();

    store.repair_dangling_tool_uses().await.unwrap();
    let lines = store.lines().await.unwrap();
    assert_eq!(lines.len(), 4);
    assert_eq!(lines[3]["role"], "user");
    let content = lines[3]["content"].as_array().unwrap();
    assert_eq!(content.len(), 1);
    assert_eq!(content[0]["type"], "tool_result");
    assert_eq!(content[0]["tool_use_id"], "id_b");

    // Idempotent: a second repair appends nothing.
    store.repair_dangling_tool_uses().await.unwrap();
    assert_eq!(store.lines().await.unwrap().len(), 4);
}

#[tokio::test]
async fn repair_dangling_tool_uses_noop_when_paired() {
    let store = temp_store();
    store.ensure().await.unwrap();
    store
        .add_assistant("r", "", &[tool_call_event("id_a"), tool_call_event("id_b")])
        .await
        .unwrap();
    let results: Vec<crate::tools::runner::ToolExecution> = ["id_a", "id_b"]
        .iter()
        .map(|id| crate::tools::runner::ToolExecution::test_result(*id, "Bash", "ok"))
        .collect();
    store.add_tool_results(&results).await.unwrap();

    store.repair_dangling_tool_uses().await.unwrap();
    assert_eq!(store.lines().await.unwrap().len(), 2);
}

#[tokio::test]
async fn cache_appended_not_invalidated() {
    let store = temp_store();
    store.ensure().await.unwrap();
    store.add_user("a").await.unwrap();
    let _ = store.lines().await.unwrap(); // populate cache
    store.add_user("b").await.unwrap();
    // cache should be updated, not None
    let lines = store.lines().await.unwrap();
    assert_eq!(lines.len(), 2);
}

#[tokio::test]
async fn active_suffix_cache_stays_pruned_when_full_history_is_read() {
    let store = temp_store();
    store.ensure().await.unwrap();
    for value in ["a", "b", "c", "d"] {
        store.add_user(value).await.unwrap();
    }
    assert_eq!(store.lines_from(0).await.unwrap().len(), 4);

    store.prune_cache_before(2).await;
    let active = store.lines_from(2).await.unwrap();
    assert_eq!(active.len(), 2);
    assert_eq!(active[0]["content"], "c");

    assert_eq!(store.lines().await.unwrap().len(), 4);
    let cache = store.cache.read().await;
    let cache = cache.as_ref().unwrap();
    assert_eq!(cache.start, 2);
    assert_eq!(cache.lines.len(), 2);
}

#[tokio::test]
async fn active_suffix_cache_accepts_new_messages() {
    let store = temp_store();
    store.ensure().await.unwrap();
    store.add_user("old").await.unwrap();
    store.add_user("active").await.unwrap();
    let _ = store.lines_from(0).await.unwrap();
    store.prune_cache_before(1).await;

    store.add_user("new").await.unwrap();

    let active = store.lines_from(1).await.unwrap();
    assert_eq!(active.len(), 2);
    assert_eq!(active[0]["content"], "active");
    assert_eq!(active[1]["content"], "new");
}

#[tokio::test]
async fn lines_from_rejects_boundary_beyond_history() {
    let store = temp_store();
    store.ensure().await.unwrap();
    store.add_user("only").await.unwrap();

    let error = store.lines_from(2).await.unwrap_err().to_string();
    assert!(error.contains("exceeds history length 1"), "{error}");
}

#[tokio::test]
async fn strict_lines_errors_on_bad_json_with_line_number() {
    let store = temp_store();
    store.ensure().await.unwrap();
    tokio::fs::write(
        store.path(),
        "{\"role\":\"user\",\"content\":\"ok\"}\nnot-json\n",
    )
    .await
    .unwrap();
    let err = store.lines().await.unwrap_err().to_string();
    assert!(err.contains("line 2"), "{err}");
}

#[tokio::test]
async fn lossy_lines_skips_bad_json() {
    let store = temp_store();
    store.ensure().await.unwrap();
    tokio::fs::write(
            store.path(),
            "{\"role\":\"user\",\"content\":\"ok\"}\nnot-json\n{\"role\":\"user\",\"content\":\"ok2\"}\n",
        )
        .await
        .unwrap();
    let lines = store.lines_lossy_with_warnings(|_| {}).await.unwrap();
    assert_eq!(lines.len(), 2);
    assert_eq!(lines[0]["content"], "ok");
    assert_eq!(lines[1]["content"], "ok2");
}

#[tokio::test]
async fn lossy_lines_reports_bad_json_warnings() {
    let store = temp_store();
    store.ensure().await.unwrap();
    tokio::fs::write(store.path(), "{\"role\":\"user\"}\nnot-json\n")
        .await
        .unwrap();
    let mut warnings = Vec::new();
    let lines = store
        .lines_lossy_with_warnings(|warning| warnings.push(warning))
        .await
        .unwrap();
    assert_eq!(lines.len(), 1);
    assert_eq!(warnings.len(), 1);
    assert!(warnings[0].contains("line 2"), "{}", warnings[0]);
}

#[tokio::test]
async fn strict_lines_skips_partial_trailing_jsonl_without_newline() {
    let store = temp_store();
    store.ensure().await.unwrap();
    tokio::fs::write(
        store.path(),
        "{\"role\":\"user\",\"content\":\"ok\"}\n{\"role\":\"assistant\"",
    )
    .await
    .unwrap();
    let lines = store.lines().await.unwrap();
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0]["content"], "ok");
}

#[tokio::test]
async fn append_repairs_partial_trailing_record_before_writing() {
    let store = temp_store();
    store.ensure().await.unwrap();
    tokio::fs::write(
        store.path(),
        "{\"role\":\"user\",\"content\":\"kept\"}\n{\"role\":\"assistant\"",
    )
    .await
    .unwrap();
    assert_eq!(store.lines().await.unwrap().len(), 1);

    store.add_user("next").await.unwrap();

    let lines = store.lines().await.unwrap();
    assert_eq!(lines.len(), 2);
    assert_eq!(lines[0]["content"], "kept");
    assert_eq!(lines[1]["content"], "next");
}

#[tokio::test]
async fn append_preserves_valid_record_without_final_newline() {
    let store = temp_store();
    store.ensure().await.unwrap();
    tokio::fs::write(store.path(), "{\"role\":\"user\",\"content\":\"kept\"}")
        .await
        .unwrap();
    assert_eq!(store.lines().await.unwrap().len(), 1);

    store.add_user("next").await.unwrap();

    let lines = store.lines().await.unwrap();
    assert_eq!(lines.len(), 2);
    assert_eq!(lines[0]["content"], "kept");
    assert_eq!(lines[1]["content"], "next");
}

#[test]
fn build_tool_call_summary_labels() {
    let mut f = std::collections::BTreeMap::new();
    f.insert("path".into(), "/tmp/test.txt".into());
    assert!(build_tool_call_summary("Read", &f).contains("test.txt"));

    let hashline = std::collections::BTreeMap::from([(
        "input".into(),
        "[src/a.rs#A1B2]\nPUT 1.=1:\n+new\n[src/b.rs#C3D4]\nCUT 2".into(),
    )]);
    let summary = build_tool_call_summary("Edit", &hashline);
    assert_eq!(summary, "Edit(2 sections, 2 ops: src/a.rs, src/b.rs)");

    let replace = std::collections::BTreeMap::from([
        ("path".into(), "src/a.rs".into()),
        ("edits".into(), "[{},{}]".into()),
    ]);
    assert_eq!(
        build_tool_call_summary("Edit", &replace),
        "Edit(src/a.rs, 2 edits)"
    );

    let mut f2 = std::collections::BTreeMap::new();
    f2.insert("command".into(), "echo hello".into());
    assert!(build_tool_call_summary("Bash", &f2).contains("echo hello"));

    let mut f3 = std::collections::BTreeMap::new();
    f3.insert("description".into(), "child task".into());
    assert!(build_tool_call_summary("SubAgent", &f3).contains("child task"));

    let mut todo = std::collections::BTreeMap::new();
    todo.insert("base_revision".into(), "4".into());
    todo.insert("add".into(), r#"[{"content":"one"}]"#.into());
    todo.insert(
        "update".into(),
        r#"[{"id":"T0001","content":"revised"}]"#.into(),
    );
    assert_eq!(
        build_tool_call_summary("TodoWrite", &todo),
        "TodoWrite(2 changes @r4)"
    );
    assert_eq!(
        build_tool_call_summary("TodoRead", &Default::default()),
        "TodoRead(state)"
    );
}

#[test]
fn first_line_handles_newlines() {
    assert_eq!(first_line("one\ntwo\nthree"), "one");
    assert_eq!(first_line("single"), "single");
    assert_eq!(first_line(""), "");
}
