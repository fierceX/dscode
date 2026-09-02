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
    tools: Vec<Value>,
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

    fn cache_projection(
        &self,
        request: &LlmRequest,
        source_prefix_len: usize,
    ) -> Option<LlmCacheProjection> {
        (source_prefix_len <= request.messages.len()).then(|| LlmCacheProjection {
            model: request.model.clone(),
            system_prompt: request.system_prompt.clone(),
            tools: request.tools.clone(),
            messages: request.messages[..source_prefix_len].to_vec(),
        })
    }

    async fn stream(&self, request: LlmRequest) -> Result<crate::llm::client::LlmResponseStream> {
        self.requests.lock().unwrap().push(CapturedSummaryRequest {
            model: request.model,
            model_alias: request.model_alias,
            system_prompt: request.system_prompt,
            messages: request.messages,
            tools: request.tools,
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

#[tokio::test]
async fn provider_usage_calibrates_auto_but_not_preflight() -> anyhow::Result<()> {
    let ctx = crate::regression::test_context_for_agent_with_config_and_backend(
        "provider-pressure-calibration",
        |config| {
            config.max_context_tokens = 1_000_000;
            config.context_compact_pct = 80;
            config.context_reserve_tokens = 100_000;
        },
        summary_backend(),
    )
    .await?;
    ctx.compaction.record_agent_request(
        "flash",
        "prefix-a",
        798_000,
        "mock".into(),
        "system".into(),
        vec![],
        None,
    );
    ctx.compaction.record_agent_usage(&UsageEvent {
        input_tokens: 400_000,
        output_tokens: 999_999,
        cache_read_input_tokens: 200_000,
        cache_creation_input_tokens: 80_000,
    });

    let auto = ctx
        .compaction
        .pressure_decision("auto", 800_000, "flash", Some("prefix-a"), None);
    assert_eq!(auto.source, "provider_calibrated");
    assert_eq!(auto.provider_baseline_tokens, Some(680_000));
    assert_eq!(auto.effective_tokens, 682_000);

    let preflight =
        ctx.compaction
            .pressure_decision("preflight", 800_000, "flash", Some("prefix-a"), None);
    assert_eq!(preflight.source, "local_preflight");
    assert_eq!(preflight.effective_tokens, 800_000);

    let smaller =
        ctx.compaction
            .pressure_decision("auto", 700_000, "flash", Some("prefix-a"), None);
    assert_eq!(smaller.effective_tokens, 582_000);
    let clamped = ctx
        .compaction
        .pressure_decision("auto", 0, "flash", Some("prefix-a"), None);
    assert_eq!(clamped.effective_tokens, 0);
    ctx.compaction.record_agent_usage(&UsageEvent {
        input_tokens: -1,
        output_tokens: 0,
        cache_read_input_tokens: 0,
        cache_creation_input_tokens: 0,
    });
    let after_invalid_usage =
        ctx.compaction
            .pressure_decision("auto", 800_000, "flash", Some("prefix-a"), None);
    assert_eq!(after_invalid_usage.effective_tokens, 682_000);
    let changed =
        ctx.compaction
            .pressure_decision("auto", 800_000, "other", Some("prefix-a"), None);
    assert_eq!(changed.source, "local_fallback");
    ctx.compaction
        .projection_generation
        .fetch_add(1, Ordering::SeqCst);
    let after_compaction =
        ctx.compaction
            .pressure_decision("auto", 800_000, "flash", Some("prefix-a"), None);
    assert_eq!(after_compaction.source, "local_fallback");
    Ok(())
}

#[tokio::test]
async fn provider_calibration_requires_actual_projection_compatibility() -> anyhow::Result<()> {
    let ctx = crate::regression::test_context_for_agent_with_config_and_backend(
        "provider-projection-calibration",
        |config| config.max_context_tokens = 1_000_000,
        summary_backend(),
    )
    .await?;
    let narrowed = vec![json!({"name":"Read"})];
    let full = vec![json!({"name":"Read"}), json!({"name":"Edit"})];
    let previous = LlmCacheProjection {
        model: "flash".into(),
        system_prompt: "routed system".into(),
        tools: narrowed.clone(),
        messages: vec![
            json!({"role":"user","content":"inspect"}),
            json!({"role":"user","content":"original router guidance"}),
        ],
    };
    ctx.compaction.record_agent_request(
        "flash",
        "source-prefix",
        700_000,
        "router-llm-backend".into(),
        "source system".into(),
        full.clone(),
        Some(previous.clone()),
    );
    ctx.compaction.record_agent_usage(&UsageEvent {
        input_tokens: 500_000,
        output_tokens: 1,
        cache_read_input_tokens: 100_000,
        cache_creation_input_tokens: 0,
    });

    let compatible = LlmCacheProjection {
        messages: vec![
            json!({"role":"user","content":"inspect"}),
            json!({"role":"user","content":"original router guidance"}),
            json!({"role":"assistant","content":"done"}),
        ],
        ..previous.clone()
    };
    let calibrated = ctx.compaction.pressure_decision(
        "auto",
        710_000,
        "flash",
        Some("source-prefix"),
        Some(&compatible),
    );
    assert_eq!(calibrated.source, "provider_calibrated");
    assert_eq!(calibrated.effective_tokens, 610_000);

    let restored_tools = LlmCacheProjection {
        tools: full,
        ..compatible.clone()
    };
    let changed_tools = ctx.compaction.pressure_decision(
        "auto",
        710_000,
        "flash",
        Some("source-prefix"),
        Some(&restored_tools),
    );
    assert_eq!(changed_tools.source, "local_fallback");

    let changed_guidance = LlmCacheProjection {
        messages: vec![
            json!({"role":"user","content":"inspect"}),
            json!({"role":"user","content":"different router guidance"}),
        ],
        ..previous
    };
    let changed_messages = ctx.compaction.pressure_decision(
        "auto",
        710_000,
        "flash",
        Some("source-prefix"),
        Some(&changed_guidance),
    );
    assert_eq!(changed_messages.source, "local_fallback");
    Ok(())
}

#[tokio::test]
async fn stored_projection_hashes_messages_and_shares_usage_baseline() -> anyhow::Result<()> {
    let ctx = crate::regression::test_context_for_agent_with_config_and_backend(
        "hashed-provider-projection",
        |_| {},
        summary_backend(),
    )
    .await?;
    let unique_pixels = "data:image/png;base64,UNIQUE_REQUEST_ONLY_PIXEL_PAYLOAD";
    ctx.compaction.record_agent_request(
        "flash",
        "prefix",
        100,
        "openai-compatible".into(),
        "system".into(),
        vec![],
        Some(LlmCacheProjection {
            model: "flash".into(),
            system_prompt: "system".into(),
            tools: vec![],
            messages: vec![json!({
                "role":"user",
                "content":[{"type":"image_url","image_url":{"url":unique_pixels}}]
            })],
        }),
    );
    ctx.compaction.record_agent_usage(&UsageEvent {
        input_tokens: 100,
        output_tokens: 1,
        cache_read_input_tokens: 0,
        cache_creation_input_tokens: 0,
    });

    let state = ctx
        .compaction
        .prompt_usage
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let latest = state.latest_request.as_ref().expect("latest request");
    let baseline = state.baseline.as_ref().expect("usage baseline");
    assert!(Arc::ptr_eq(latest, &baseline.request));
    assert_eq!(
        latest
            .projection
            .as_ref()
            .expect("projection snapshot")
            .message_hashes
            .len(),
        1
    );
    let retained = format!("{state:?}");
    assert!(!retained.contains(unique_pixels), "{retained}");
    assert!(!retained.contains("data:image"), "{retained}");
    Ok(())
}

#[tokio::test]
async fn cache_aligned_summary_reuses_agent_system_tools_and_prefix() -> anyhow::Result<()> {
    let backend = Arc::new(CapturingSummaryBackend::default());
    let ctx = crate::regression::test_context_for_agent_with_config_and_backend(
        "cache-aligned-summary",
        |config| {
            config.max_context_tokens = 64_000;
            config.context_reserve_tokens = 8_000;
            config.context_compact_tail_tokens = 1;
            config.context_compact_input_reduction = true;
        },
        backend.clone(),
    )
    .await?;
    for index in 0..4 {
        ctx.store
            .add_user(&format!("request {index}: {}", "x".repeat(2_000)))
            .await?;
        ctx.store
            .add_assistant(&format!("progress {index}"), "", &[])
            .await?;
    }
    let messages =
        crate::llm::image_projection::project_consumed_attachments(&ctx.store.lines().await?);
    let system = "stable agent system".to_string();
    let tools = vec![json!({"name":"Read","description":"read"})];
    let resolved = crate::config::model_resolver(&ctx.config).resolve(&ctx.config.model);
    let projection = LlmCacheProjection {
        model: resolved.actual.clone(),
        system_prompt: system.clone(),
        tools: tools.clone(),
        messages: messages.clone(),
    };
    ctx.compaction.record_agent_request(
        &resolved.actual,
        &prefix_fingerprint(&system, &tools),
        10_000,
        backend.name().into(),
        system.clone(),
        tools.clone(),
        Some(projection),
    );

    let (did_compact, reason) = compact(&ctx, "manual", 0).await?;
    assert!(did_compact);
    assert!(reason.contains("input_mode=cache_aligned"), "{reason}");
    let requests = backend.requests.lock().unwrap();
    let request = requests.last().expect("summary request captured");
    assert_eq!(request.system_prompt, system);
    assert_eq!(request.tools, tools);
    assert_eq!(request.messages.last().unwrap()["role"], "user");
    assert_eq!(
        request.messages.last().unwrap()["content"],
        COMPACTION_INSTRUCTION
    );
    assert_eq!(
        request.messages[..request.messages.len() - 1],
        messages[..request.messages.len() - 1]
    );
    Ok(())
}

#[tokio::test]
async fn raw_partial_aligned_summary_degrades_unconsumed_attachment() -> anyhow::Result<()> {
    let backend = Arc::new(CapturingSummaryBackend::default());
    let ctx = crate::regression::test_context_for_agent_with_config_and_backend(
        "raw-aligned-unconsumed-image",
        |config| {
            config.max_context_tokens = 64_000;
            config.context_reserve_tokens = 8_000;
            config.context_compact_input_reduction = false;
        },
        backend.clone(),
    )
    .await?;
    let image_url = format!("image://{}", "a".repeat(64));
    let active = vec![
        json!({"role":"user","content":"initial request"}),
        json!({"role":"assistant","content":"interrupted before inspecting the next image"}),
        json!({
            "role":"user",
            "content":[{
                "type":"tool_attachment",
                "tool_use_id":"image-1",
                "url":image_url,
                "format":"png",
                "width":64,
                "height":32,
                "bytes":128
            }]
        }),
        json!({"role":"user","content":"latest request after another interruption"}),
    ];
    let system = "stable agent system".to_string();
    let tools = vec![json!({"name":"Read","description":"read"})];
    let resolved = crate::config::model_resolver(&ctx.config).resolve(&ctx.config.model);
    let mut recent_messages = active.clone();
    recent_messages[2]["content"][0] = json!({
        "type":"image_url",
        "image_url":{"url":"data:image/png;base64,AA=="}
    });
    ctx.compaction.record_agent_request(
        &resolved.actual,
        &prefix_fingerprint(&system, &tools),
        1_000,
        backend.name().into(),
        system,
        tools,
        Some(LlmCacheProjection {
            model: resolved.actual.clone(),
            system_prompt: "stable agent system".into(),
            tools: vec![json!({"name":"Read","description":"read"})],
            messages: recent_messages,
        }),
    );

    let input = ctx.compaction.build_summary_input(
        &active,
        active.len(),
        None,
        false,
        LlmModelTarget::new(&resolved.actual, resolved.alias.as_deref()),
    )?;

    assert_eq!(input.meta.input_mode, "partial_aligned");
    assert_eq!(input.meta.aligned_messages, 2);
    let serialized = serde_json::to_string(&input.messages)?;
    assert!(!serialized.contains("\"type\":\"tool_attachment\""));
    assert!(!serialized.contains("\"type\":\"image_url\""));
    assert!(serialized.contains("[image png 64x32: image://"));
    assert_eq!(
        input.messages.last().unwrap()["content"],
        COMPACTION_INSTRUCTION
    );
    Ok(())
}

#[tokio::test]
async fn partial_alignment_rolls_back_before_incomplete_tool_exchange() -> anyhow::Result<()> {
    let backend = Arc::new(CapturingSummaryBackend::default());
    let ctx = crate::regression::test_context_for_agent_with_config_and_backend(
        "aligned-tool-exchange-boundary",
        |config| {
            config.max_context_tokens = 64_000;
            config.context_reserve_tokens = 8_000;
            config.context_compact_input_reduction = true;
        },
        backend.clone(),
    )
    .await?;
    let active = vec![
        json!({"role":"user","content":"inspect the image"}),
        json!({"role":"assistant","content":[{
            "type":"tool_use",
            "id":"image-read-1",
            "name":"Read",
            "input":{"path":"diagram.png"}
        }]}),
        json!({"role":"user","content":[
            {"type":"tool_result","tool_use_id":"image-read-1","content":"Image captured."},
            {
                "type":"tool_attachment",
                "tool_use_id":"image-read-1",
                "url":format!("image://{}", "b".repeat(64)),
                "format":"png",
                "width":80,
                "height":40,
                "bytes":128
            }
        ]}),
        json!({"role":"user","content":"continue after interruption"}),
    ];
    let system = "stable agent system".to_string();
    let tools = vec![json!({"name":"Read","description":"read"})];
    let resolved = crate::config::model_resolver(&ctx.config).resolve(&ctx.config.model);
    let mut recent_messages = active.clone();
    recent_messages[2]["content"][1] = json!({
        "type":"image_url",
        "image_url":{"url":"data:image/png;base64,AA=="}
    });
    ctx.compaction.record_agent_request(
        &resolved.actual,
        &prefix_fingerprint(&system, &tools),
        1_000,
        backend.name().into(),
        system.clone(),
        tools.clone(),
        Some(LlmCacheProjection {
            model: resolved.actual.clone(),
            system_prompt: system,
            tools,
            messages: recent_messages,
        }),
    );

    let input = ctx.compaction.build_summary_input(
        &active,
        active.len(),
        None,
        false,
        LlmModelTarget::new(&resolved.actual, resolved.alias.as_deref()),
    )?;

    assert_eq!(input.meta.input_mode, "partial_aligned");
    assert_eq!(input.meta.aligned_messages, 1);
    let wire = crate::llm::transport::convert_messages_to_openai(&input.messages)?;
    let serialized = serde_json::to_string(&wire)?;
    assert!(serialized.contains("[tool Read id=image-read-1] path=diagram.png"));
    assert!(serialized.contains("[tool_result id=image-read-1] Image captured."));
    assert!(!serialized.contains("\"tool_calls\""));
    Ok(())
}

#[tokio::test]
async fn summary_tool_call_fails_explicitly_without_advancing_context() -> anyhow::Result<()> {
    let backend = Arc::new(MockLlmBackend::new(
        "summary-tool-call",
        vec![vec![
            Ok(Event::ToolCall(crate::protocol::ToolCallEvent {
                name: "Read".into(),
                id: "summary-read-1".into(),
                input_json: json!({"path":"src/lib.rs"}),
                fields: Default::default(),
                parse_error: None,
            })),
            Ok(Event::Stop(StopEvent {
                reason: "tool_use".into(),
            })),
        ]],
    ));
    let ctx = crate::regression::test_context_for_agent_with_config_and_backend(
        "summary-tool-call-invalid",
        |config| {
            config.max_context_tokens = 64_000;
            config.context_reserve_tokens = 8_000;
            config.context_compact_tail_tokens = 1;
        },
        backend,
    )
    .await?;
    add_tool_history(&ctx).await?;
    let before = ctx.compaction.active_messages().await?;

    let error = compact(&ctx, "manual", 0).await.unwrap_err();

    assert!(
        error
            .to_string()
            .contains("compaction attempted invalid tool call Read (summary-read-1)"),
        "{error:#}"
    );
    assert_eq!(ctx.compaction.active_messages().await?, before);
    assert!(ctx.compaction.read_summary().await.is_none());
    Ok(())
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
    assert_eq!(projected[0]["role"], "user");
    assert_eq!(projected[0]["internal"], true);
    assert!(
        projected[0]["content"]
            .as_str()
            .is_some_and(|c| c.contains("<compacted-summary>"))
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
            m.get("content")
                .and_then(Value::as_str)
                .is_some_and(|c| c.contains("<compacted-summary>"))
        })
        .count();
    assert_eq!(snapshots, 1, "context snapshot must appear exactly once");

    let guard = backend.requests.lock().unwrap();
    assert_eq!(guard.len(), 2);
    let second_request = serde_json::to_string(&guard[1].messages)?;
    assert!(second_request.contains("<compacted-summary>"));
    assert!(guard[1].messages.iter().any(|message| {
        message
            .get("content")
            .and_then(Value::as_str)
            .is_some_and(|content| content.contains(&first_state.summary))
    }));
    assert!(second_request.contains("second batch request"));
    Ok(())
}

#[tokio::test]
async fn compacted_history_projects_active_plan_after_summary_and_removes_it_on_clear()
-> anyhow::Result<()> {
    let ctx = crate::regression::test_context_for_agent_with_config_and_backend(
        "active-plan-checkpoint",
        |config| {
            config.max_context_tokens = 64_000;
            config.context_reserve_tokens = 8_000;
            config.context_compact_tail_tokens = 1;
        },
        summary_backend(),
    )
    .await?;
    for index in 0..3 {
        ctx.store
            .add_user(&format!("request {index}: {}", "x".repeat(2_000)))
            .await?;
        ctx.store.add_assistant("progress", "", &[]).await?;
    }
    tokio::fs::write(&ctx.plan_path, "# Active plan\n1. implement\n2. verify\n").await?;
    assert!(compact(&ctx, "manual", 0).await?.0);

    let projected = ctx.compaction.active_messages().await?;
    assert!(
        projected[0]["content"]
            .as_str()
            .is_some_and(|content| content.contains("<compacted-summary>"))
    );
    assert!(
        projected[1]["content"]
            .as_str()
            .is_some_and(|content| content.contains("<active-plan-checkpoint>\n# Active plan"))
    );
    assert_eq!(projected[0]["role"], "user");
    assert_eq!(projected[1]["role"], "user");
    assert_eq!(projected[0]["internal"], true);
    assert_eq!(projected[1]["internal"], true);

    tokio::fs::remove_file(&ctx.plan_path).await?;
    let cleared = ctx.compaction.active_messages().await?;
    assert!(cleared.iter().all(|message| {
        !message["content"]
            .as_str()
            .is_some_and(|content| content.contains("<active-plan-checkpoint>"))
    }));
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
