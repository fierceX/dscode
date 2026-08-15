use super::*;

#[tokio::test]
async fn sub_agent_recursion_is_rejected_without_running_child() -> anyhow::Result<()> {
    let h = harness_with("sub-recursion", true, 300).await?;
    let coordinator = SubAgentCoordinator::new(h.ctx.clone(), h.ctx.config.clone());
    let mut result = internal_result("SubAgent");
    result.spawns_sub_agent = true;
    result.sub_agent_prompt = Some("nested task".into());
    let runner: SubAgentRunner = Arc::new(|_, _, _, _, _| {
        Box::pin(async {
            panic!("runner must not execute when recursion is blocked");
        })
    });
    let processed = coordinator.process_with_runner(vec![result], runner).await;
    assert_eq!(processed.len(), 1);
    assert!(processed[0].content.contains("recursion blocked"));
    assert!(!processed[0].succeeded());
    Ok(())
}

#[tokio::test]
async fn sub_agent_success_formats_result_and_records_usage() -> anyhow::Result<()> {
    let h = harness_with("sub-success", false, 300).await?;
    let coordinator = SubAgentCoordinator::new(h.ctx.clone(), h.ctx.config.clone());
    let mut result = internal_result("SubAgent");
    result.spawns_sub_agent = true;
    result.sub_agent_prompt = Some("task".into());
    let runner: SubAgentRunner = Arc::new(|_, _, _, _, _| {
        Box::pin(async {
            SubAgentResult {
                status: "ok".into(),
                thinking: "child thought".into(),
                text: "child text".into(),
                usage: crate::session::stats::Stats {
                    agent_request_count: 2,
                    total_input_tokens: 10,
                    total_output_tokens: 5,
                    total_cache_read_tokens: 3,
                    total_cache_creation_tokens: 1,
                    ..Default::default()
                },
            }
        })
    });
    let processed = coordinator.process_with_runner(vec![result], runner).await;

    assert_eq!(processed.len(), 1);
    assert!(processed[0].content.contains("] ok (in=10, out=5)"));
    assert!(processed[0].content.contains("Thinking: child thought"));
    assert!(processed[0].content.contains("Text: child text"));
    assert!(processed[0].succeeded());
    let stats = h.ctx.stats.snapshot().await;
    assert_eq!(stats.sub_agent_request_count, 1);
    assert_eq!(stats.agent_request_count, 2);
    assert_eq!(stats.total_input_tokens, 10);
    assert_eq!(stats.total_output_tokens, 5);
    Ok(())
}

#[tokio::test]
async fn sub_agent_runner_panic_is_reported_as_failed_result() -> anyhow::Result<()> {
    let h = harness_with("sub-panic", false, 300).await?;
    let coordinator = SubAgentCoordinator::new(h.ctx.clone(), h.ctx.config.clone());
    let mut result = internal_result("SubAgent");
    result.spawns_sub_agent = true;
    result.sub_agent_prompt = Some("panic task".into());
    let runner: SubAgentRunner = Arc::new(|_, _, _, _, _| {
        Box::pin(async {
            panic!("panic from test runner");
        })
    });
    let processed = coordinator.process_with_runner(vec![result], runner).await;

    assert_eq!(processed.len(), 1);
    assert!(processed[0].content.contains("] failed (in=0, out=0)"));
    assert!(
        processed[0]
            .content
            .contains("Sub-agent task panicked: panic from test runner"),
        "{}",
        processed[0].content
    );
    assert!(!processed[0].succeeded());
    assert!(
        h.display
            .info
            .lock()
            .unwrap()
            .iter()
            .any(|msg| msg.starts_with("error:[sub-agent ") && msg.contains("failed")),
        "{:?}",
        h.display.info.lock().unwrap()
    );
    Ok(())
}

#[tokio::test]
async fn sub_agent_runner_sync_panic_is_reported_as_failed_result() -> anyhow::Result<()> {
    let h = harness_with("sub-sync-panic", false, 300).await?;
    let coordinator = SubAgentCoordinator::new(h.ctx.clone(), h.ctx.config.clone());
    let mut result = internal_result("SubAgent");
    result.spawns_sub_agent = true;
    result.sub_agent_prompt = Some("sync panic task".into());
    let runner: SubAgentRunner = Arc::new(|_, _, _, _, _| {
        panic!("sync panic from test runner");
    });
    let processed = coordinator.process_with_runner(vec![result], runner).await;

    assert_eq!(processed.len(), 1);
    assert!(processed[0].content.contains("] failed (in=0, out=0)"));
    assert!(
        processed[0]
            .content
            .contains("Sub-agent task panicked: sync panic from test runner"),
        "{}",
        processed[0].content
    );
    Ok(())
}

#[tokio::test]
async fn sub_agent_timeout_marks_incomplete() -> anyhow::Result<()> {
    let h = harness_with("sub-timeout", false, 0).await?;
    let coordinator = SubAgentCoordinator::new(h.ctx.clone(), h.ctx.config.clone());
    let mut result = internal_result("SubAgent");
    result.spawns_sub_agent = true;
    result.sub_agent_prompt = Some("slow task".into());
    let runner: SubAgentRunner = Arc::new(|_, _, _, _, _| {
        Box::pin(async move {
            tokio::time::sleep(Duration::from_millis(200)).await;
            SubAgentResult {
                status: "ok".into(),
                thinking: String::new(),
                text: "late".into(),
                usage: Default::default(),
            }
        })
    });
    let processed = coordinator.process_with_runner(vec![result], runner).await;
    assert_eq!(processed[0].content, "Sub-agent timed out after 0s.");
    assert!(!processed[0].succeeded());
    Ok(())
}

#[tokio::test]
async fn sub_agent_collection_enters_timeout_even_when_more_than_limit_are_launched()
-> anyhow::Result<()> {
    let h = harness_with("sub-timeout-many", false, 0).await?;
    let coordinator = SubAgentCoordinator::new(h.ctx.clone(), h.ctx.config.clone());
    let mut calls = Vec::new();
    for idx in 0..9 {
        let mut result = internal_result("SubAgent");
        result.spawns_sub_agent = true;
        result.sub_agent_prompt = Some(format!("slow task {idx}"));
        calls.push(result);
    }
    let runner: SubAgentRunner = Arc::new(|_, _, _, _, _| {
        Box::pin(async {
            tokio::time::sleep(Duration::from_millis(50)).await;
            SubAgentResult {
                status: "ok".into(),
                thinking: String::new(),
                text: "late".into(),
                usage: Default::default(),
            }
        })
    });
    let processed = tokio::time::timeout(
        Duration::from_millis(100),
        coordinator.process_with_runner(calls, runner),
    )
    .await?;
    assert_eq!(processed.len(), 9);
    assert!(
        processed
            .iter()
            .all(|r| r.content == "Sub-agent timed out after 0s.")
    );
    Ok(())
}

#[tokio::test]
async fn sub_agent_executor_with_mock_llm_captures_child_output() -> anyhow::Result<()> {
    let h = harness("sub-executor-mock").await?;
    h.ctx.store.add_user("parent context").await?;
    let llm = Arc::new(MockLlmBackend::new(
        "flash",
        vec![vec![
            Ok(Event::Text(TextEvent {
                content: "child answer".into(),
            })),
            Ok(Event::Usage(UsageEvent {
                input_tokens: 10,
                output_tokens: 5,
                cache_read_input_tokens: 3,
                cache_creation_input_tokens: 1,
            })),
            Ok(Event::Stop(StopEvent {
                reason: "end_turn".into(),
            })),
        ]],
    ));
    let parent = test_context_with_llm_backend(h.ctx.clone(), llm);
    let executor = SubAgentExecutor::new(
        parent.clone(),
        "sub_mock".into(),
        true,
        parent.config.clone(),
    )
    .await?;
    let result = executor.execute("child task".into()).await;
    assert_eq!(result.status, "ok");
    assert_eq!(result.text, "child answer");
    assert!(
        result.thinking.is_empty(),
        "unexpected thinking: {}",
        result.thinking
    );
    assert_eq!(h.ctx.store.lines().await?.len(), 1);
    let records = h.ctx.usage.all_records()?;
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].kind, crate::session::usage::UsageKind::SubAgent);
    assert_eq!(records[0].origin_session_id, "sub_mock");
    Ok(())
}

#[tokio::test]
#[ignore = "requires MINK_REAL_API=1 and DEEPSEEK_API_KEY"]
async fn real_deepseek_api_smoke_streams_response() -> anyhow::Result<()> {
    if std::env::var("MINK_REAL_API").ok().as_deref() != Some("1") {
        eprintln!("skipping real API regression: set MINK_REAL_API=1");
        return Ok(());
    }
    let api_key = match std::env::var("DEEPSEEK_API_KEY") {
        Ok(value) if !value.trim().is_empty() => value,
        _ => {
            eprintln!("skipping real API regression: DEEPSEEK_API_KEY is not set");
            return Ok(());
        }
    };
    let h = harness("real-api").await?;
    let base_url = std::env::var("DEEPSEEK_BASE_URL")
        .unwrap_or_else(|_| "https://api.deepseek.com/v1".to_string());
    let api_url = format!("{}/chat/completions", base_url.trim_end_matches('/'));
    let messages = vec![json!({"role":"user","content":"Reply with one short word: pong"})];
    let response = OpenAiCompatibleBackend::deepseek_defaults()
        .stream(LlmRequest {
            purpose: LlmPurpose::Agent,
            model: "deepseek-v4-flash".into(),
            model_alias: Some("flash".into()),
            api_url,
            api_key,
            system_prompt: "You are a concise regression smoke test.".into(),
            messages,
            tools: Vec::new(),
            max_tokens: h.ctx.max_tokens(),
            cancel: h.ctx.cancel.clone(),
            verbose: h.ctx.verbose(),
            display: h.ctx.display.clone(),
        })
        .await?;
    let mut stream = response.events;
    let mut saw_text = false;
    let mut saw_stop = false;
    while let Some(event) = futures::StreamExt::next(&mut stream).await {
        match event? {
            Event::Text(text) if !text.content.trim().is_empty() => saw_text = true,
            Event::Stop(_) => {
                saw_stop = true;
                break;
            }
            _ => {}
        }
    }
    assert!(saw_text, "real API stream did not yield text");
    assert!(saw_stop, "real API stream did not yield stop");
    Ok(())
}
