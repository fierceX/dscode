use super::*;
use crate::llm::mock::MockLlmBackend;
use crate::protocol::StopEvent;
use serde_json::json;
use std::sync::Mutex;

#[derive(Debug)]
struct CapturedSummaryRequest {
    model: String,
    model_alias: Option<String>,
    system_prompt: String,
    messages: Vec<Value>,
    max_tokens: i32,
}

#[derive(Default)]
struct CapturingSummaryBackend {
    requests: Mutex<Vec<CapturedSummaryRequest>>,
}

struct PendingSummaryBackend;

#[async_trait::async_trait]
impl LlmBackend for PendingSummaryBackend {
    fn name(&self) -> &str {
        "pending-summary"
    }

    async fn stream(&self, _request: LlmRequest) -> Result<crate::llm::client::LlmResponseStream> {
        Ok(crate::llm::client::LlmResponseStream {
            events: Box::pin(futures::stream::pending()),
            attempt_count: 1,
        })
    }
}

#[async_trait::async_trait]
impl LlmBackend for CapturingSummaryBackend {
    fn name(&self) -> &str {
        "capturing-summary"
    }

    async fn stream(&self, request: LlmRequest) -> Result<crate::llm::client::LlmResponseStream> {
        self.requests.lock().unwrap().push(CapturedSummaryRequest {
            model: request.model,
            model_alias: request.model_alias,
            system_prompt: request.system_prompt,
            messages: request.messages,
            max_tokens: request.max_tokens,
        });
        Ok(crate::llm::client::LlmResponseStream {
                events: Box::pin(futures::stream::iter(vec![
                    Ok(Event::Text(TextEvent {
                        content: "Task focus: test\nLatest request: compact\nProgress: retained\nErrors: (none)\nDecisions: use cargo test\nTool evidence: cargo test\nReflections: none".into(),
                    })),
                    Ok(Event::Stop(StopEvent {
                        reason: "end_turn".into(),
                    })),
                ])),
                attempt_count: 1,
            })
    }
}

fn summary_backend() -> Arc<dyn LlmBackend> {
    Arc::new(MockLlmBackend::new(
            "summary-model",
            vec![vec![
                Ok(Event::Text(TextEvent {
                    content: "Task focus: test\nLatest request: compact\nProgress: retained\nErrors: (none)\nDecisions: (none)\nTool evidence: none\nReflections: none".into(),
                })),
                Ok(Event::Stop(StopEvent {
                    reason: "end_turn".into(),
                })),
            ]],
        ))
}

async fn add_tool_history(ctx: &crate::context::AgentSharedContext) -> anyhow::Result<()> {
    ctx.store.add_user("fix the test suite").await?;
    ctx.store
        .add_assistant(
            "I will run the tests.",
            "private reasoning",
            &[crate::protocol::ToolCallEvent {
                name: "Bash".into(),
                id: "bash-1".into(),
                input_json: json!({"command":"cargo test"}),
                fields: Default::default(),
                parse_error: None,
            }],
        )
        .await?;
    ctx.store
        .add_tool_results(&[crate::tools::runner::ToolExecution::test_result(
            "bash-1",
            "Bash",
            "Process completed with exit code 1.",
        )])
        .await?;
    ctx.store.add_user("keep the API stable").await?;
    Ok(())
}

async fn compact(
    ctx: &crate::context::AgentSharedContext,
    trigger: &str,
    context_tokens: usize,
) -> anyhow::Result<(bool, String)> {
    let resolved = crate::config::model_resolver(&ctx.config).resolve(&ctx.config.model);
    ctx.compaction
        .evaluate_and_compact(
            trigger,
            context_tokens,
            LlmModelTarget::new(&resolved.actual, resolved.alias.as_deref()),
        )
        .await
}

#[test]
fn output_tokens_use_explicit_reserve() {
    let config = Config {
        max_context_tokens: 64_000,
        max_tokens: 81_920,
        context_reserve_tokens: 12_000,
        context_compact_max_output_tokens: 2_048,
        ..Config::default()
    };
    assert_eq!(effective_max_tokens(&config), 12_000);
    assert_eq!(request_input_limit(&config), 52_000);
    assert_eq!(compaction_max_output_tokens(&config), 2_048);
}

#[test]
fn trigger_uses_explicit_percentage_and_reserve() {
    let config = Config {
        max_context_tokens: 64_000,
        context_compact_pct: 90,
        context_reserve_tokens: 12_000,
        ..Config::default()
    };
    assert_eq!(compaction_trigger_tokens(&config), 52_000);
}

#[test]
fn zero_context_window_keeps_request_budget_unbounded() {
    let config = Config {
        max_context_tokens: 0,
        max_tokens: 16_000,
        ..Config::default()
    };
    assert_eq!(effective_max_tokens(&config), 16_000);
    assert_eq!(request_input_limit(&config), usize::MAX);
}

#[test]
fn cut_point_can_compact_completed_tool_exchanges_in_one_user_turn() {
    let messages = vec![
        json!({"role":"user","content":"fix it"}),
        json!({"role":"assistant","content":[{"type":"tool_use","id":"a","name":"Read","input":{"path":"a"}}]}),
        json!({"role":"user","content":[{"type":"tool_result","tool_use_id":"a","content":"x".repeat(2000)}]}),
        json!({"role":"assistant","content":[{"type":"tool_use","id":"b","name":"Read","input":{"path":"b"}}]}),
        json!({"role":"user","content":[{"type":"tool_result","tool_use_id":"b","content":"new"}]}),
    ];
    let cut = find_compaction_cut_point(&messages, 10);
    assert!(cut > 0);
    assert_eq!(messages[cut]["role"], "assistant");
}

#[test]
fn cut_point_keeps_recent_user_messages_over_token_budget() {
    let mut messages = Vec::new();
    for turn in 0..3 {
        messages.push(json!({"role":"user","content":format!("user {turn}")}));
        messages.push(json!({"role":"assistant","content":[{"type":"tool_use","id":format!("t{turn}"),"name":"Read","input":{"path":"a"}}]}));
        messages.push(json!({"role":"user","content":[{"type":"tool_result","tool_use_id":format!("t{turn}"),"content":"x".repeat(2000)}]}));
    }
    let cut = find_compaction_cut_point(&messages, 100);
    assert!(cut > 0, "expected a compaction boundary");
    let users_in_tail = messages[cut..]
        .iter()
        .filter(|m| is_real_user_message(m))
        .count();
    assert!(
        users_in_tail >= COMPACTION_MIN_TAIL_USER_MESSAGES,
        "cut={cut} retained {users_in_tail} user messages"
    );
    assert!(is_safe_context_start(&messages[cut]));
}

#[test]
fn cut_point_ignores_runtime_injected_user_messages() {
    // 引擎注入的 user-role 消息（todo progress/final reminder、todo
    // sync、signal recovery，以及带 internal 标记的消息）不得计入
    // "真实 user 消息"：否则同轮多个内部消息会让守卫保留它们却裁掉
    // 上一条真实用户约束。
    let mut messages = Vec::new();
    messages.push(json!({"role":"user","content":"head constraint"}));
    messages.push(json!({"role":"assistant","content":[{"type":"tool_use","id":"t1","name":"Read","input":{"path":"a"}}]}));
    messages.push(json!({"role":"user","content":[{"type":"tool_result","tool_use_id":"t1","content":"x".repeat(3000)}]}));
    // Injected messages in the middle of the history: string-prefix
    // markers, the final-reminder marker, and the metadata flag path.
    messages.push(json!({"role":"user","content":"<todo-progress-reminder>reassess the active batch</todo-progress-reminder>"}));
    messages
        .push(json!({"role":"user","content":"<todo-sync revision=\"3\">projection</todo-sync>"}));
    messages.push(json!({"role":"user","content":"[System note: belief 0.5 is below the recovery threshold. Enter SIGNAL_RECOVERY mode.]"}));
    messages.push(json!({"role":"user","content":"<todo-final-reminder>finish verified work or pause</todo-final-reminder>"}));
    messages.push(json!({"role":"user","content":"plain injected text","internal":true}));
    // Two real constraints near the tail, after the injected messages.
    messages.push(json!({"role":"user","content":"latest constraint A"}));
    messages.push(json!({"role":"assistant","content":[{"type":"tool_use","id":"t2","name":"Read","input":{"path":"b"}}]}));
    messages.push(json!({"role":"user","content":[{"type":"tool_result","tool_use_id":"t2","content":"y".repeat(200)}]}));
    messages.push(json!({"role":"user","content":"latest constraint B"}));
    messages.push(json!({"role":"assistant","content":[{"type":"tool_use","id":"t3","name":"Read","input":{"path":"c"}}]}));
    messages.push(json!({"role":"user","content":[{"type":"tool_result","tool_use_id":"t3","content":"z".repeat(3000)}]}));

    let real_users_total = messages.iter().filter(|m| is_real_user_message(m)).count();
    assert_eq!(
        real_users_total, 3,
        "injected messages must not count as real users"
    );
    let cut = find_compaction_cut_point(&messages, 800);
    assert!(cut > 0, "expected a compaction boundary");
    let users_in_tail = messages[cut..]
        .iter()
        .filter(|m| is_real_user_message(m))
        .count();
    assert!(
        users_in_tail >= COMPACTION_MIN_TAIL_USER_MESSAGES,
        "cut={cut} retained {users_in_tail} real user messages"
    );
    assert!(is_safe_context_start(&messages[cut]));
}

#[test]
fn cut_point_with_few_users_keeps_token_based_boundary() {
    let messages = vec![
        json!({"role":"user","content":"fix it"}),
        json!({"role":"assistant","content":[{"type":"tool_use","id":"a","name":"Read","input":{"path":"a"}}]}),
        json!({"role":"user","content":[{"type":"tool_result","tool_use_id":"a","content":"x".repeat(2000)}]}),
    ];
    let cut = find_compaction_cut_point(&messages, 100);
    assert_eq!(messages[cut]["role"], "assistant");
}

#[tokio::test]
async fn startup_rebuilds_missing_or_stale_summary_projection() -> anyhow::Result<()> {
    let ctx = crate::regression::test_context_for_agent_with_config_and_backend(
        "compact-rebuild-projection-source",
        |config| config.context_compact_tail_tokens = 1,
        summary_backend(),
    )
    .await?;
    let state_path = ctx.summary_path.with_file_name("context-state.json");
    let state = CompactionState {
        active_start: 0,
        summary: "authoritative summary".into(),
    };
    crate::session::atomic_file::atomic_replace(&state_path, &serde_json::to_vec_pretty(&state)?)?;
    std::fs::write(&ctx.summary_path, "stale projection\n")?;

    let engine = CompactionEngine::new(
        ctx.store.clone(),
        ctx.summary_path.clone(),
        ctx.config.base_url.clone(),
        &ctx.config,
        ctx.stats.clone(),
        ctx.usage.clone(),
        ctx.config.session_id.clone(),
        ctx.display.clone(),
        ctx.cancel.clone(),
        ctx.interrupt.clone(),
        summary_backend(),
        None,
    )?;

    assert_eq!(
        engine.current_summary()?.as_deref(),
        Some("authoritative summary")
    );
    assert_eq!(
        std::fs::read_to_string(&ctx.summary_path)?,
        "authoritative summary\n"
    );
    Ok(())
}

#[tokio::test]
async fn startup_repairs_legacy_cut_on_internal_user_message() -> anyhow::Result<()> {
    let ctx = crate::regression::test_context_for_agent_with_config_and_backend(
        "compact-repair-internal-boundary",
        |_| {},
        summary_backend(),
    )
    .await?;
    ctx.store.add_user("head constraint").await?;
    ctx.store.add_assistant("head response", "", &[]).await?;
    ctx.store.add_runtime_user("plain injected text").await?;
    ctx.store
        .add_assistant("next safe boundary", "", &[])
        .await?;

    let state_path = ctx.summary_path.with_file_name("context-state.json");
    let state = CompactionState {
        active_start: 2,
        summary: "authoritative summary".into(),
    };
    crate::session::atomic_file::atomic_replace(&state_path, &serde_json::to_vec_pretty(&state)?)?;

    let engine = CompactionEngine::new(
        ctx.store.clone(),
        ctx.summary_path.clone(),
        ctx.config.base_url.clone(),
        &ctx.config,
        ctx.stats.clone(),
        ctx.usage.clone(),
        ctx.config.session_id.clone(),
        ctx.display.clone(),
        ctx.cancel.clone(),
        ctx.interrupt.clone(),
        summary_backend(),
        None,
    )?;
    engine.validate_startup().await?;
    assert_eq!(engine.current_state()?.active_start, 3);
    Ok(())
}

#[tokio::test]
async fn compaction_keeps_full_history_and_persists_state() -> anyhow::Result<()> {
    let ctx = crate::regression::test_context_for_agent_with_config_and_backend(
        "compact-keeps-history",
        |config| {
            config.max_context_tokens = 64_000;
            config.context_compact_pct = 65;
            config.context_reserve_tokens = 12_000;
            config.context_compact_tail_tokens = 16_000;
            config.context_compact_max_output_tokens = 2_048;
        },
        summary_backend(),
    )
    .await?;
    for index in 0..4 {
        ctx.store
            .add_user(&format!("request {index}: {}", "x".repeat(8_000)))
            .await?;
        ctx.store
            .add_assistant(&format!("progress {index}: {}", "y".repeat(8_000)), "", &[])
            .await?;
    }

    let full_history = ctx.store.lines().await?;
    let (compacted, _) = compact(&ctx, "manual", 50_000).await?;
    assert!(compacted);
    assert_eq!(ctx.store.lines().await?, full_history);

    let projected = ctx.compaction.active_messages().await?;
    assert!(projected.len() < full_history.len());
    let persisted = load_state(&ctx.summary_path.with_file_name("context-state.json"))?;
    assert!(persisted.active_start > 0);
    assert!(!persisted.summary.is_empty());
    assert!(
        !ctx.summary_path
            .with_file_name("context-state.json")
            .with_extension("json.tmp")
            .exists()
    );
    Ok(())
}

#[tokio::test]
async fn compaction_interrupts_pending_summary_without_committing() -> anyhow::Result<()> {
    let ctx = crate::regression::test_context_for_agent_with_config_and_backend(
        "compact-interrupt",
        |config| {
            config.max_context_tokens = 64_000;
            config.context_reserve_tokens = 8_000;
            config.context_compact_tail_tokens = 1;
        },
        Arc::new(PendingSummaryBackend),
    )
    .await?;
    for index in 0..3 {
        ctx.store
            .add_user(&format!("request {index}: {}", "x".repeat(2_000)))
            .await?;
        ctx.store
            .add_assistant(&format!("progress {index}: {}", "y".repeat(2_000)), "", &[])
            .await?;
    }
    let compaction = ctx.compaction.clone();
    let task = tokio::spawn(async move {
        compaction
            .evaluate_and_compact("manual", 0, LlmModelTarget::new("flash", None))
            .await
    });
    tokio::time::sleep(std::time::Duration::from_millis(40)).await;
    ctx.interrupt.store(true, Ordering::SeqCst);
    let error = tokio::time::timeout(std::time::Duration::from_secs(2), task)
        .await??
        .unwrap_err()
        .to_string();
    assert!(error.contains("compaction interrupted"), "{error}");
    assert_eq!(
        load_state(&ctx.summary_path.with_file_name("context-state.json"))?.active_start,
        0
    );
    Ok(())
}

#[tokio::test]
async fn enabled_input_reduction_changes_only_the_summary_request() -> anyhow::Result<()> {
    let backend = Arc::new(CapturingSummaryBackend::default());
    let ctx = crate::regression::test_context_for_agent_with_config_and_backend(
        "compact-input-reduction",
        |config| {
            config.max_context_tokens = 64_000;
            config.context_reserve_tokens = 8_000;
            config.context_compact_tail_tokens = 1;
            config.context_compact_max_output_tokens = 1_234;
            config.context_compact_input_reduction = true;
        },
        backend.clone(),
    )
    .await?;
    add_tool_history(&ctx).await?;

    let full_history = ctx.store.lines().await?;
    let (compacted, _) = ctx
        .compaction
        .evaluate_and_compact(
            "manual",
            0,
            LlmModelTarget::new("active-summary-model", Some("active-summary")),
        )
        .await?;
    assert!(compacted);
    assert_eq!(ctx.store.lines().await?, full_history);

    let guard = backend.requests.lock().unwrap();
    let request = guard.first().expect("summary request captured");
    assert_eq!(request.model, "active-summary-model");
    assert_eq!(request.model_alias.as_deref(), Some("active-summary"));
    assert_eq!(request.max_tokens, 1_234);
    assert!(
        request
            .system_prompt
            .starts_with("Summarize coding-agent history")
    );
    let serialized = serde_json::to_string(&request.messages)?;
    assert!(serialized.contains("command=cargo test"));
    assert!(serialized.contains("Process completed with exit code 1."));
    assert!(!serialized.contains("private reasoning"));
    assert!(serialized.contains("seven non-empty fields"));
    for field in ["Task focus:", "Errors:", "Decisions:", "Reflections:"] {
        assert!(serialized.contains(field), "instruction missing {field}");
    }
    assert!(serialized.contains("Write (none) for any field without content."));
    Ok(())
}

#[tokio::test]
async fn disabled_input_reduction_sends_original_structured_history() -> anyhow::Result<()> {
    let backend = Arc::new(CapturingSummaryBackend::default());
    let ctx = crate::regression::test_context_for_agent_with_config_and_backend(
        "compact-without-input-reduction",
        |config| {
            config.max_context_tokens = 64_000;
            config.context_reserve_tokens = 8_000;
            config.context_compact_tail_tokens = 1;
            config.context_compact_max_output_tokens = 1_234;
            config.context_compact_input_reduction = false;
        },
        backend.clone(),
    )
    .await?;
    add_tool_history(&ctx).await?;

    assert!(compact(&ctx, "manual", 0).await?.0);

    let guard = backend.requests.lock().unwrap();
    let request = guard.first().expect("summary request captured");
    let serialized = serde_json::to_string(&request.messages)?;
    assert!(serialized.contains("private reasoning"));
    assert!(serialized.contains("\"type\":\"tool_use\""));
    assert!(serialized.contains("cargo test"));
    assert!(!serialized.contains("<conversation>"));
    Ok(())
}

#[tokio::test]
async fn repeated_compaction_advances_boundary_and_merges_previous_summary() -> anyhow::Result<()> {
    let backend = Arc::new(CapturingSummaryBackend::default());
    let ctx = crate::regression::test_context_for_agent_with_config_and_backend(
        "compact-repeatedly",
        |config| {
            config.max_context_tokens = 64_000;
            config.context_reserve_tokens = 8_000;
            config.context_compact_tail_tokens = 1;
            config.context_compact_max_output_tokens = 1_234;
            config.context_compact_input_reduction = true;
        },
        backend.clone(),
    )
    .await?;
    for index in 0..3 {
        ctx.store
            .add_user(&format!(
                "first batch request {index}: {}",
                "x".repeat(2_000)
            ))
            .await?;
        ctx.store
            .add_assistant(
                &format!("first batch progress {index}: {}", "y".repeat(2_000)),
                "",
                &[],
            )
            .await?;
    }

    assert!(compact(&ctx, "manual", 0).await?.0);
    let state_path = ctx.summary_path.with_file_name("context-state.json");
    let first_state = load_state(&state_path)?;
    assert!(first_state.active_start > 0);

    for index in 0..3 {
        ctx.store
            .add_user(&format!(
                "second batch request {index}: {}",
                "a".repeat(2_000)
            ))
            .await?;
        ctx.store
            .add_assistant(
                &format!("second batch progress {index}: {}", "b".repeat(2_000)),
                "",
                &[],
            )
            .await?;
    }
    let full_history = ctx.store.lines().await?;

    assert!(compact(&ctx, "manual", 0).await?.0);

    let second_state = load_state(&state_path)?;
    assert!(second_state.active_start > first_state.active_start);
    assert_eq!(ctx.store.lines().await?, full_history);
    let projected = ctx.compaction.active_messages().await?;
    assert!(projected.len() < full_history.len());
    assert_eq!(projected[0]["role"], "system");
    assert!(
        projected[0]["content"]
            .as_str()
            .is_some_and(|c| c.contains("<context-snapshot>"))
    );
    let last_user = projected
        .iter()
        .rposition(|m| {
            m.get("role").and_then(Value::as_str) == Some("user")
                && m.get("content").is_some_and(Value::is_string)
        })
        .expect("compacted projection keeps a real user message");
    for (index, message) in projected.iter().enumerate() {
        if index > last_user {
            assert_ne!(
                message.get("role").and_then(Value::as_str),
                Some("system"),
                "system fragment appears after the last user message"
            );
        }
    }
    let snapshots = projected
        .iter()
        .filter(|m| {
            m.get("role").and_then(Value::as_str) == Some("system")
                && m.get("content")
                    .and_then(Value::as_str)
                    .is_some_and(|c| c.contains("<context-snapshot>"))
        })
        .count();
    assert_eq!(snapshots, 1, "context snapshot must appear exactly once");

    let guard = backend.requests.lock().unwrap();
    assert_eq!(guard.len(), 2);
    let second_instruction = guard[1]
        .messages
        .last()
        .and_then(|message| message.get("content"))
        .and_then(Value::as_str)
        .expect("second summary instruction");
    let encoded_previous = serde_json::to_string(&Some(first_state.summary.as_str()))?;
    assert!(second_instruction.contains(&format!("Previous context snapshot: {encoded_previous}")));
    let second_request = serde_json::to_string(&guard[1].messages)?;
    assert!(second_request.contains("second batch request"));
    Ok(())
}

#[tokio::test]
async fn summary_preserves_tool_commands_and_paths() -> anyhow::Result<()> {
    let backend = Arc::new(MockLlmBackend::new(
            "summary-model",
            vec![vec![
                Ok(Event::Text(TextEvent {
                    content: "Task focus: fix build\nLatest request: continue\nProgress: Read(src/lib.rs)\nErrors: Bash(cargo test) failed\nDecisions: (none)\nTool evidence: Bash(cargo test) failed\nReflections: none".into(),
                })),
                Ok(Event::Stop(StopEvent {
                    reason: "end_turn".into(),
                })),
            ]],
        ));
    let ctx = crate::regression::test_context_for_agent_with_config_and_backend(
        "compact-preserves-tool-evidence",
        |config| {
            config.max_context_tokens = 64_000;
            config.context_compact_tail_tokens = 1;
        },
        backend,
    )
    .await?;
    ctx.store.add_user("fix the build").await?;
    ctx.store.add_assistant("checking", "", &[]).await?;

    let (compacted, _) = compact(&ctx, "manual", 0).await?;

    assert!(compacted);
    let summary = ctx.compaction.current_summary()?.unwrap();
    assert!(summary.contains("Read(src/lib.rs)"));
    assert!(summary.contains("Bash(cargo test)"));
    Ok(())
}

#[tokio::test]
async fn zero_context_window_disables_auto_but_allows_manual_compaction() -> anyhow::Result<()> {
    let ctx = crate::regression::test_context_for_agent_with_config_and_backend(
        "compact-zero-window",
        |config| {
            config.max_context_tokens = 0;
            config.context_compact_tail_tokens = 4_000;
        },
        summary_backend(),
    )
    .await?;
    for index in 0..3 {
        ctx.store
            .add_user(&format!("request {index}: {}", "x".repeat(4_000)))
            .await?;
        ctx.store
            .add_assistant(&format!("progress {index}: {}", "y".repeat(4_000)), "", &[])
            .await?;
    }

    let (automatic, reason) = compact(&ctx, "auto", usize::MAX).await?;
    assert!(!automatic);
    assert_eq!(reason, "automatic compaction disabled");

    let (manual, _) = compact(&ctx, "manual", usize::MAX).await?;
    assert!(manual);
    Ok(())
}
