use super::*;

#[tokio::test]
async fn read_todo_via_session_resource() -> anyhow::Result<()> {
    let ctx = crate::regression::test_context_for_agent("session-resource-todo").await?;
    let tool_ctx = crate::context::ToolContext::from(ctx.as_ref());
    // 与 TodoRead 同一路径：会话内 store（空 todo）快照格式化
    let content = format_session_todo(&tool_ctx)?;
    assert!(content.starts_with("# session://current/todo"));
    assert!(content.contains("No todo items."));
    assert!(!content.contains("请使用 TodoRead")); // 透明委托：无引导提示
    Ok(())
}

#[test]
fn read_plan_via_session_resource() {
    let dir = std::path::Path::new("/tmp/mink-session-resource-test/sess-2");
    let _ = std::fs::remove_dir_all(dir.parent().unwrap());
    std::fs::create_dir_all(dir).unwrap();
    let none = format_session_plan(dir).unwrap();
    assert!(none.contains("Status: none"));
    std::fs::write(dir.join("plan.draft"), "Draft plan: step 1").unwrap();
    let draft = format_session_plan(dir).unwrap();
    assert!(draft.contains("Status: draft"));
    std::fs::write(dir.join("plan.md"), "Confirmed plan: final").unwrap();
    let confirmed = format_session_plan(dir).unwrap();
    assert!(confirmed.contains("Status: confirmed"));
    let _ = std::fs::remove_dir_all(dir.parent().unwrap());
}
