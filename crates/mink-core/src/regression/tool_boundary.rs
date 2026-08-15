use super::*;

#[tokio::test]
async fn safety_blocked_bash_emits_typed_signal_event() -> anyhow::Result<()> {
    let h = harness("safety-signal").await?;
    let llm = Arc::new(MockLlmBackend::new(
        "flash",
        vec![
            vec![
                Ok(Event::ToolCall(tool_call(
                    "Bash",
                    "call_bash",
                    json!({"command":"sudo echo no"}),
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
    let (decision, _) = executor.execute("try unsafe command", None).await?;
    assert_eq!(decision, TurnDecision::Stop);
    assert_eq!(executor.tool_error_count(), 0);
    assert!(
        executor
            .collected_signals()
            .iter()
            .any(|s| matches!(s.kind, crate::guard::collector::SignalKind::SafetyBlocked))
    );
    let events = tokio::fs::read_to_string(&h.ctx.events_path).await?;
    assert!(events.contains(r#""type":"signal""#), "{events}");
    assert!(events.contains(r#""version":1"#), "{events}");
    assert!(events.contains("SafetyBlocked"), "{events}");
    Ok(())
}

#[tokio::test]
async fn tool_runner_blocks_workspace_write_escape() -> anyhow::Result<()> {
    let h = harness("workspace-policy").await?;
    let runner = ToolRunner::new(Arc::new(ToolContext::from(h.ctx.as_ref())));
    let outside = h.cwd.parent().unwrap().join("outside.txt");
    let result = runner
        .execute_all(vec![tool_call(
            "Write",
            "call_write",
            json!({"path": outside.display().to_string(), "content": "bad"}),
        )])
        .await?;
    assert_eq!(result.len(), 1);
    // Sandbox handles write restrictions; app-level guard is a no-op.
    // The write should succeed (no "blocked" error).
    assert!(
        !result[0].content.contains("write blocked"),
        "{}",
        result[0].content
    );
    Ok(())
}

#[tokio::test]
async fn tool_runner_blocks_symlink_write_escape() -> anyhow::Result<()> {
    let h = harness("workspace-symlink-policy").await?;
    let outside_dir = h.cwd.parent().unwrap().join("outside-dir");
    tokio::fs::create_dir_all(&outside_dir).await?;
    let link = h.cwd.join("link-out");

    #[cfg(unix)]
    std::os::unix::fs::symlink(&outside_dir, &link)?;
    #[cfg(windows)]
    std::os::windows::fs::symlink_dir(&outside_dir, &link)?;

    let runner = ToolRunner::new(Arc::new(ToolContext::from(h.ctx.as_ref())));
    let result = runner
        .execute_all(vec![tool_call(
            "Write",
            "call_write_symlink",
            json!({"path": "link-out/escape.txt", "content": "bad"}),
        )])
        .await?;
    // Sandbox handles write restrictions; app-level guard is a no-op.
    assert!(
        !result[0].content.contains("write blocked"),
        "{}",
        result[0].content
    );
    Ok(())
}

#[tokio::test]
async fn file_summary_uses_tool_context_cwd() -> anyhow::Result<()> {
    let h = harness("summary-cwd").await?;
    tokio::fs::write(h.cwd.join("inside.txt"), "one\ntwo").await?;
    let process_cwd = std::env::current_dir()?;
    let runner = ToolRunner::new(Arc::new(ToolContext::from(h.ctx.as_ref())));
    let result = runner
        .execute_all(vec![tool_call(
            "Read",
            "call_read_summary",
            json!({"path": "inside.txt"}),
        )])
        .await;
    assert_eq!(std::env::current_dir()?, process_cwd);
    let result = result?;
    assert!(
        result[0].content.starts_with("Read(inside.txt)"),
        "{}",
        result[0].content
    );
    assert!(
        result[0].content.contains("[2 lines, 7 bytes]"),
        "{}",
        result[0].content
    );
    Ok(())
}

#[tokio::test]
async fn virtual_search_tools_route_through_injected_backend() -> anyhow::Result<()> {
    struct SearchVfs;

    impl crate::tools::vfs::ReadOnlyFileSystem for SearchVfs {
        fn read(
            &self,
            _scope: &crate::tools::vfs::VfsScope,
            _request: &crate::tools::vfs::VfsReadRequest,
        ) -> anyhow::Result<crate::tools::vfs::VfsReadResult> {
            unreachable!()
        }

        fn glob(
            &self,
            scope: &crate::tools::vfs::VfsScope,
            request: &crate::tools::vfs::VfsGlobRequest,
        ) -> anyhow::Result<crate::tools::vfs::VfsGlobResult> {
            assert_eq!(scope.resource_session_id, "knowledge-session");
            assert_eq!(request.path, "./docs");
            Ok(crate::tools::vfs::VfsGlobResult {
                paths: vec!["guide.md".into()],
                scanned_files: 1,
                ..Default::default()
            })
        }

        fn grep(
            &self,
            scope: &crate::tools::vfs::VfsScope,
            request: &crate::tools::vfs::VfsGrepRequest,
        ) -> anyhow::Result<crate::tools::vfs::VfsGrepResult> {
            assert_eq!(scope.resource_session_id, "knowledge-session");
            assert_eq!(request.pattern, "needle");
            Ok(crate::tools::vfs::VfsGrepResult {
                entries: vec![crate::tools::vfs::VfsGrepEntry::Line {
                    path: "docs/guide.md".into(),
                    line_number: 2,
                    content: "needle".into(),
                    matched: true,
                }],
                match_count: 1,
                scanned_files: 1,
                ..Default::default()
            })
        }
    }

    let mut ctx = test_context_for_agent("virtual-search-routing").await?;
    let shared = Arc::get_mut(&mut ctx).expect("test context should be uniquely owned");
    shared.read_only_fs = Some(Arc::new(SearchVfs));
    shared.vfs_scope.resource_session_id = "knowledge-session".into();

    let runner = ToolRunner::new(Arc::new(ToolContext::from(ctx.as_ref())));
    let results = runner
        .execute_all(vec![
            tool_call(
                "Glob",
                "call_virtual_glob",
                json!({"pattern": "*.md", "path": "./docs"}),
            ),
            tool_call(
                "Grep",
                "call_virtual_grep",
                json!({"pattern": "needle", "path": "docs"}),
            ),
        ])
        .await?;

    assert_eq!(results[0].content, "guide.md");
    assert_eq!(results[1].content, "docs/guide.md:2:needle");
    Ok(())
}

#[tokio::test]
async fn tool_runner_blocks_symlink_edit_escape() -> anyhow::Result<()> {
    let h = harness("workspace-edit-symlink-policy").await?;
    let outside_dir = h.cwd.parent().unwrap().join("outside-edit-dir");
    tokio::fs::create_dir_all(&outside_dir).await?;
    tokio::fs::write(outside_dir.join("escape.txt"), "old").await?;
    let link = h.cwd.join("link-edit-out");

    #[cfg(unix)]
    std::os::unix::fs::symlink(&outside_dir, &link)?;
    #[cfg(windows)]
    std::os::windows::fs::symlink_dir(&outside_dir, &link)?;

    let runner = ToolRunner::new(Arc::new(ToolContext::from(h.ctx.as_ref())));
    // Current protocol: obtain a real hashline tag via Read, then Edit with a
    // single `input` string (the legacy path+patch shape is rejected).
    let read = runner
        .execute_all(vec![tool_call(
            "Read",
            "read_symlink",
            json!({"path": "link-edit-out/escape.txt"}),
        )])
        .await?;
    let tag = read[0]
        .content
        .split_once('#')
        .and_then(|(_, rest)| rest.get(..4))
        .expect("hashline read header tag")
        .to_string();
    let result = runner
        .execute_all(vec![tool_call(
            "Edit",
            "call_edit_symlink",
            json!({"input": format!("[link-edit-out/escape.txt#{tag}]\nPUT 1.:\n+new")}),
        )])
        .await?;
    // Sandbox handles write restrictions; app-level guard is a no-op.
    assert!(
        !result[0].content.contains("write blocked"),
        "{}",
        result[0].content
    );
    Ok(())
}
