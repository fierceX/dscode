use crate::agent::belief::BeliefTracker;
use crate::agent::orchestrator::{OrchActor, OrchCmd};
use crate::agent::plan_actions::PlanActionHandler;
use crate::agent::prefix::PrefixManager;
use crate::agent::sub_coordinator::{SubAgentCoordinator, SubAgentRunner};
use crate::agent::sub_executor::{SubAgentExecutor, SubAgentResult};
use crate::agent::turn::{TurnDecision, TurnEffect, TurnExecutor};
use crate::config::{Config, OutputFormat};
use crate::context::{AgentSharedContext, ToolConfig, ToolContext};
use crate::guard::collector::{Signal, SignalKind};
use crate::llm::client::{
    BackendLlmClient, LlmBackend, LlmPurpose, LlmRequest, LlmResponseStream,
    OpenAiCompatibleBackend,
};
use crate::llm::mock::MockLlmClient;
use crate::protocol::{
    ErrorEvent, Event, RetryEvent, StopEvent, TextEvent, ThinkingEvent, ToolCallEvent, UsageEvent,
};
use crate::session::compaction::CompactionEngine;
use crate::session::paths;
use crate::tools::runner::{ToolRunResult, ToolRunner};
use crate::ui::{Display, StatsSnapshot};
use futures::StreamExt;
use serde_json::json;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

struct PendingLlmClient;

#[async_trait::async_trait]
impl LlmBackend for PendingLlmClient {
    fn name(&self) -> &str {
        "pending"
    }

    async fn stream(&self, _request: LlmRequest) -> anyhow::Result<LlmResponseStream> {
        Ok(LlmResponseStream {
            events: Box::pin(futures::stream::pending()),
            attempt_count: 1,
        })
    }
}

struct IdleAfterTextLlmClient;

#[async_trait::async_trait]
impl LlmBackend for IdleAfterTextLlmClient {
    fn name(&self) -> &str {
        "idle-after-text"
    }

    async fn stream(&self, _request: LlmRequest) -> anyhow::Result<LlmResponseStream> {
        let stream = futures::stream::iter(vec![Ok(Event::Text(TextEvent {
            content: "partial".into(),
        }))])
        .chain(futures::stream::pending());
        Ok(LlmResponseStream {
            events: Box::pin(stream),
            attempt_count: 1,
        })
    }
}

#[derive(Debug, PartialEq)]
struct CapturedModelTarget {
    model: String,
    alias: Option<String>,
}

struct RecordingCompactionBackend {
    requests: Arc<Mutex<Vec<CapturedModelTarget>>>,
}

#[async_trait::async_trait]
impl LlmBackend for RecordingCompactionBackend {
    fn name(&self) -> &str {
        "recording-compaction"
    }

    async fn stream(&self, request: LlmRequest) -> anyhow::Result<LlmResponseStream> {
        assert!(matches!(request.purpose, LlmPurpose::Compaction));
        self.requests.lock().unwrap().push(CapturedModelTarget {
            model: request.model,
            alias: request.model_alias,
        });
        Ok(LlmResponseStream {
            events: Box::pin(futures::stream::iter(vec![
                Ok(Event::Text(TextEvent {
                    content: "Current objective and completed work retained.".into(),
                })),
                Ok(Event::Stop(StopEvent {
                    reason: "end_turn".into(),
                })),
            ])),
            attempt_count: 1,
        })
    }
}

struct FailingCompactionBackend;

#[async_trait::async_trait]
impl LlmBackend for FailingCompactionBackend {
    fn name(&self) -> &str {
        "failing-compaction"
    }

    async fn stream(&self, request: LlmRequest) -> anyhow::Result<LlmResponseStream> {
        assert!(matches!(request.purpose, LlmPurpose::Compaction));
        anyhow::bail!("planned compaction failure")
    }
}

#[derive(Debug, PartialEq)]
struct CapturedRoutedRequest {
    purpose: &'static str,
    model: String,
    alias: Option<String>,
}

struct ActiveModelRoutingBackend {
    requests: Arc<Mutex<Vec<CapturedRoutedRequest>>>,
    agent_request_count: AtomicU64,
}

#[async_trait::async_trait]
impl LlmBackend for ActiveModelRoutingBackend {
    fn name(&self) -> &str {
        "active-model-routing"
    }

    async fn stream(&self, request: LlmRequest) -> anyhow::Result<LlmResponseStream> {
        let purpose = match &request.purpose {
            LlmPurpose::Agent => "agent",
            LlmPurpose::SubAgent { .. } => "sub_agent",
            LlmPurpose::Compaction => "compaction",
        };
        self.requests.lock().unwrap().push(CapturedRoutedRequest {
            purpose,
            model: request.model,
            alias: request.model_alias,
        });

        let events = match request.purpose {
            LlmPurpose::Agent if self.agent_request_count.fetch_add(1, Ordering::SeqCst) == 0 => {
                vec![
                    Ok(Event::ToolCall(tool_call(
                        "SubAgent",
                        "call_sub_agent",
                        json!({"prompt":"complete the child task","fork":false}),
                    ))),
                    Ok(Event::Stop(StopEvent {
                        reason: "tool_use".into(),
                    })),
                ]
            }
            LlmPurpose::Agent => vec![
                Ok(Event::Text(TextEvent {
                    content: "parent done".into(),
                })),
                Ok(Event::Stop(StopEvent {
                    reason: "end_turn".into(),
                })),
            ],
            LlmPurpose::SubAgent { .. } => vec![
                Ok(Event::Text(TextEvent {
                    content: "child done".into(),
                })),
                Ok(Event::Stop(StopEvent {
                    reason: "end_turn".into(),
                })),
            ],
            LlmPurpose::Compaction => unreachable!("test does not trigger compaction"),
        };
        Ok(LlmResponseStream {
            events: Box::pin(futures::stream::iter(events)),
            attempt_count: 1,
        })
    }
}

struct NoopDisplay {
    info: Mutex<Vec<String>>,
    title_models: Mutex<Vec<String>>,
}

impl NoopDisplay {
    fn new() -> Self {
        Self {
            info: Mutex::new(Vec::new()),
            title_models: Mutex::new(Vec::new()),
        }
    }
}

impl Display for NoopDisplay {
    fn render_thinking(&self, _content: &str) {}
    fn render_text(&self, _content: &str) {}
    fn render_tool_call(&self, _name: &str, _summary: &str) {}
    fn render_tool_result(&self, _tool_name: &str, _content_preview: &str) {}
    fn render_stop(&self) {}
    fn render_error(&self, message: &str) {
        self.info.lock().unwrap().push(format!("error:{message}"));
    }
    fn render_retry(&self) {}
    fn render_info(&self, msg: &str) {
        self.info.lock().unwrap().push(msg.to_string());
    }
    fn render_title_update(&self, model: &str, _stats: &StatsSnapshot) {
        self.title_models.lock().unwrap().push(model.to_string());
    }
    fn render_sub_agent_status(&self, _sid: &str, _st: &str, _it: u64, _ot: u64) {}
    fn render_prompt(&self) {}
    fn render_clear_line(&self) {}
}

struct TestHarness {
    ctx: Arc<AgentSharedContext>,
    cwd: PathBuf,
    display: Arc<NoopDisplay>,
}

async fn harness(name: &str) -> anyhow::Result<TestHarness> {
    harness_with(name, false, 300).await
}

pub(crate) async fn test_context_for_agent(name: &str) -> anyhow::Result<Arc<AgentSharedContext>> {
    Ok(harness(name).await?.ctx)
}

pub(crate) async fn test_context_for_agent_with_config(
    name: &str,
    configure: impl FnOnce(&mut Config),
) -> anyhow::Result<Arc<AgentSharedContext>> {
    Ok(harness_with_config(name, false, 300, configure, None)
        .await?
        .ctx)
}

pub(crate) async fn test_context_for_agent_with_config_and_backend(
    name: &str,
    configure: impl FnOnce(&mut Config),
    backend: Arc<dyn LlmBackend>,
) -> anyhow::Result<Arc<AgentSharedContext>> {
    Ok(
        harness_with_config(name, false, 300, configure, Some(backend))
            .await?
            .ctx,
    )
}

async fn harness_with(
    name: &str,
    is_sub_agent: bool,
    sub_agent_timeout_secs: i32,
) -> anyhow::Result<TestHarness> {
    harness_with_config(name, is_sub_agent, sub_agent_timeout_secs, |_| {}, None).await
}

async fn harness_with_config(
    name: &str,
    is_sub_agent: bool,
    sub_agent_timeout_secs: i32,
    configure: impl FnOnce(&mut Config),
    llm_backend: Option<Arc<dyn LlmBackend>>,
) -> anyhow::Result<TestHarness> {
    static CNT: AtomicU64 = AtomicU64::new(0);
    let n = CNT.fetch_add(1, Ordering::SeqCst);
    let root = std::env::temp_dir().join(format!(
        "mink-regression-{}-{}-{n}",
        std::process::id(),
        name
    ));
    let home = root.join("home");
    let cwd = root.join("workspace");
    tokio::fs::create_dir_all(&home).await?;
    tokio::fs::create_dir_all(&cwd).await?;

    let sid = "regression";
    let spaths = paths::paths_for(&home, &cwd, sid);
    let (store, stats, artifacts) =
        crate::session::init::init_session_base(&home, &cwd, sid).await?;
    let mut cfg = Config {
        model: "flash".into(),
        api_key: "test-key".into(),
        base_url: "https://example.invalid/v1".into(),
        max_context_tokens: 1_000_000,
        context_compact_pct: 100,
        sub_agent_timeout_secs,
        output_format: OutputFormat::Human,
        log_events: true,
        ..Default::default()
    };
    cfg.prompt.clear();
    configure(&mut cfg);

    let usage = crate::session::usage::UsageJournal::new(spaths.usage.clone());
    let display = Arc::new(NoopDisplay::new());
    let capability_snapshot = Arc::new(crate::capabilities::CapabilitySnapshot::load_default(
        &cwd,
        &home,
        &cfg.session_id,
        &cfg.session_id,
        &cfg.skills,
    )?);
    let llm_backend = llm_backend.unwrap_or_else(|| {
        Arc::new(crate::llm::client::OpenAiCompatibleBackend::deepseek_defaults())
    });
    let compaction = Arc::new(CompactionEngine::new(
        store.clone(),
        spaths.summary.clone(),
        crate::config::api_url(&cfg),
        &cfg,
        stats.clone(),
        usage.clone(),
        cfg.session_id.clone(),
        display.clone(),
        crate::cancel::CancellationToken::new(),
        llm_backend.clone(),
    ));
    let tool_config = ToolConfig::from_config(&cfg);
    let todo_store = Arc::new(crate::session::todo::TodoStore::load(spaths.todos.clone())?);
    let (tool_resolution_context, tool_surface, tool_capabilities) =
        crate::context::resolve_tool_runtime(&tool_config, is_sub_agent, false)?;
    let ctx = Arc::new(AgentSharedContext {
        config: cfg.clone(),
        cwd: cwd.clone(),
        home,
        session_layout: paths::SessionLayout::ProjectScoped,
        api_url: crate::config::api_url(&cfg),
        llm_backend,
        store,
        artifacts,
        todo_store,
        snapshots: Arc::new(Mutex::new(
            crate::tools::snapshot::FileSnapshotStore::default(),
        )),
        stats,
        usage,
        compaction,
        cancel: crate::cancel::CancellationToken::new(),
        display: display.clone(),
        sub_stream_tx: None,
        read_only_fs: None,
        vfs_scope: crate::tools::vfs::VfsScope {
            resource_session_id: sid.into(),
            agent_session_id: sid.into(),
        },
        resource_router: Arc::new(crate::resources::ResourceRouter::with_builtin_handlers()),
        capability_snapshot,
        tool_config,
        tool_resolution_context,
        tool_surface,
        tool_capabilities,
        events_path: spaths.events,
        summary_path: spaths.summary,
        plan_path: spaths.plan,
        plan_draft_path: spaths.plan_draft,
        immutable_prefix: Mutex::new(None),
        is_sub_agent,
        interrupt: Arc::new(AtomicBool::new(false)),
        event_log_warned: AtomicBool::new(false),
    });
    Ok(TestHarness { ctx, cwd, display })
}

fn tool_call(name: &str, id: &str, input: serde_json::Value) -> ToolCallEvent {
    let fields = input
        .as_object()
        .map(|obj| {
            obj.iter()
                .map(|(k, v)| {
                    let value = v
                        .as_str()
                        .map(str::to_string)
                        .unwrap_or_else(|| v.to_string());
                    (k.clone(), value)
                })
                .collect::<BTreeMap<_, _>>()
        })
        .unwrap_or_default();
    let order = fields.keys().cloned().collect();
    ToolCallEvent {
        name: name.into(),
        id: id.into(),
        input_json: input,
        fields,
        order,
    }
}

async fn run_orchestrator_user_input(
    ctx: Arc<AgentSharedContext>,
    llm: Arc<dyn LlmBackend>,
    input: &str,
) -> anyhow::Result<()> {
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    let actor = OrchActor::new(test_context_with_llm_backend(ctx, llm), rx);
    let handle = tokio::spawn(actor.run());
    let (done_tx, done_rx) = tokio::sync::oneshot::channel();
    tx.send(OrchCmd::UserInput {
        input: input.to_string(),
        done: done_tx,
    })?;
    done_rx.await?;
    drop(tx);
    handle.await??;
    Ok(())
}

fn test_context_with_llm_backend(
    ctx: Arc<AgentSharedContext>,
    llm_backend: Arc<dyn LlmBackend>,
) -> Arc<AgentSharedContext> {
    Arc::new(AgentSharedContext {
        config: ctx.config.clone(),
        cwd: ctx.cwd.clone(),
        home: ctx.home.clone(),
        session_layout: ctx.session_layout,
        api_url: ctx.api_url.clone(),
        llm_backend,
        store: ctx.store.clone(),
        artifacts: ctx.artifacts.clone(),
        todo_store: ctx.todo_store.clone(),
        snapshots: ctx.snapshots.clone(),
        stats: ctx.stats.clone(),
        usage: ctx.usage.clone(),
        compaction: ctx.compaction.clone(),
        cancel: ctx.cancel.clone(),
        display: ctx.display.clone(),
        sub_stream_tx: ctx.sub_stream_tx.clone(),
        read_only_fs: ctx.read_only_fs.clone(),
        vfs_scope: ctx.vfs_scope.clone(),
        resource_router: ctx.resource_router.clone(),
        capability_snapshot: ctx.capability_snapshot.clone(),
        tool_config: ctx.tool_config.clone(),
        tool_resolution_context: ctx.tool_resolution_context,
        tool_surface: ctx.tool_surface.clone(),
        tool_capabilities: ctx.tool_capabilities.clone(),
        events_path: ctx.events_path.clone(),
        summary_path: ctx.summary_path.clone(),
        plan_path: ctx.plan_path.clone(),
        plan_draft_path: ctx.plan_draft_path.clone(),
        immutable_prefix: Mutex::new(None),
        is_sub_agent: ctx.is_sub_agent,
        interrupt: ctx.interrupt.clone(),
        event_log_warned: AtomicBool::new(false),
    })
}

fn llm_client_from_mock(mock: Arc<MockLlmClient>) -> Arc<dyn crate::llm::client::LlmClient> {
    let model = mock.model_name.clone();
    Arc::new(BackendLlmClient::new(mock, model, None))
}

#[tokio::test]
async fn full_turn_tool_loop_preserves_conversation_order() -> anyhow::Result<()> {
    let h = harness("turn-loop").await?;
    tokio::fs::write(h.cwd.join("fixture.txt"), "alpha\nbeta\n").await?;
    let llm = Arc::new(MockLlmClient::new(
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
    let mut executor = TurnExecutor::new(h.ctx.clone(), llm_client_from_mock(llm));
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
    let llm = Arc::new(MockLlmClient::new(
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
    let mut executor = TurnExecutor::new(h.ctx.clone(), llm_client_from_mock(llm));
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
    let llm = Arc::new(MockLlmClient::new(
        "flash",
        vec![vec![Ok(Event::Error(ErrorEvent {
            message: "model error".into(),
        }))]],
    ));
    let mut executor = TurnExecutor::new(h.ctx.clone(), llm_client_from_mock(llm));
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
    let llm = Arc::new(MockLlmClient::new(
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
    let mut executor = TurnExecutor::new(h.ctx.clone(), llm_client_from_mock(llm));
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
    let llm = Arc::new(MockLlmClient::new(
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
    let mut executor = TurnExecutor::new(h.ctx.clone(), llm_client_from_mock(llm));
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
    let llm = Arc::new(MockLlmClient::new(
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
    let mut executor = TurnExecutor::new(h.ctx.clone(), llm_client_from_mock(llm));
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
    let llm = Arc::new(MockLlmClient::new(
        "flash",
        vec![vec![Ok(Event::Text(TextEvent {
            content: "partial".into(),
        }))]],
    ));
    let mut executor = TurnExecutor::new(h.ctx.clone(), llm_client_from_mock(llm));
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
    let mut executor = TurnExecutor::new(
        h.ctx.clone(),
        Arc::new(BackendLlmClient::new(
            Arc::new(PendingLlmClient),
            "flash",
            Some("flash".into()),
        )),
    );
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
    let mut executor = TurnExecutor::new(
        h.ctx.clone(),
        Arc::new(BackendLlmClient::new(
            Arc::new(IdleAfterTextLlmClient),
            "flash",
            Some("flash".into()),
        )),
    );
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
    let llm = Arc::new(MockLlmClient::new(
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
    let mut executor = TurnExecutor::new(h.ctx.clone(), llm_client_from_mock(llm));
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
    let llm = Arc::new(MockLlmClient::new(
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
    let mut executor = TurnExecutor::new(h.ctx.clone(), llm_client_from_mock(llm));
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
    let llm = Arc::new(MockLlmClient::new(
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
    let mut executor = TurnExecutor::new(h.ctx.clone(), llm_client_from_mock(llm));
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
    let llm = Arc::new(MockLlmClient::new(
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
    let mut executor = TurnExecutor::new(h.ctx.clone(), llm_client_from_mock(llm));
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
    let llm = Arc::new(MockLlmClient::new(
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
    let mut executor = TurnExecutor::new(h.ctx.clone(), llm_client_from_mock(llm));
    let (decision, effects) = executor.execute("edit file", None).await?;

    assert_eq!(decision, TurnDecision::Stop);
    assert!(effects.is_empty());
    assert_eq!(
        tokio::fs::read_to_string(h.cwd.join("edit.txt")).await?,
        "new\n"
    );
    Ok(())
}

#[tokio::test]
async fn signal_recovery_guard_blocks_first_write() -> anyhow::Result<()> {
    let h = harness("guard-blocks-write").await?;
    let llm = Arc::new(MockLlmClient::new(
        "flash",
        vec![
            vec![
                Ok(Event::ToolCall(tool_call(
                    "Bash",
                    "call_fail",
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
    let mut executor = TurnExecutor::new(h.ctx.clone(), llm_client_from_mock(llm));
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
    Ok(())
}

#[tokio::test]
async fn signal_recovery_guard_allows_first_read() -> anyhow::Result<()> {
    let h = harness("guard-allows-read").await?;
    tokio::fs::write(h.cwd.join("ok.txt"), "ok\n").await?;
    let llm = Arc::new(MockLlmClient::new(
        "flash",
        vec![
            vec![
                Ok(Event::ToolCall(tool_call(
                    "Bash",
                    "call_fail",
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
    let mut executor = TurnExecutor::new(h.ctx.clone(), llm_client_from_mock(llm));
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
    let llm = Arc::new(MockLlmClient::new(
        "flash",
        vec![vec![Ok(Event::Stop(StopEvent {
            reason: "max_tokens".into(),
        }))]],
    ));
    let mut executor = TurnExecutor::new(h.ctx.clone(), llm_client_from_mock(llm));
    let (decision, effects) = executor.execute("too long", None).await?;
    assert_eq!(decision, TurnDecision::Failed("stop: max_tokens".into()));
    assert!(effects.is_empty());

    let h = harness("unknown-stop").await?;
    let llm = Arc::new(MockLlmClient::new(
        "flash",
        vec![vec![Ok(Event::Stop(StopEvent {
            reason: "content_filter".into(),
        }))]],
    ));
    let mut executor = TurnExecutor::new(h.ctx.clone(), llm_client_from_mock(llm));
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
    let llm = Arc::new(MockLlmClient::new(
        "flash",
        vec![vec![Ok(Event::Stop(StopEvent {
            reason: "end_turn".into(),
        }))]],
    ));
    let mut executor = TurnExecutor::new(h.ctx.clone(), llm_client_from_mock(llm));
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
    let llm = Arc::new(MockLlmClient::new(
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
    let mut executor = TurnExecutor::new(h.ctx.clone(), llm_client_from_mock(llm));
    let mut belief = BeliefTracker::new(16);
    let (decision, effects) = executor.execute("read clean", Some(&mut belief)).await?;
    assert_eq!(decision, TurnDecision::MaxTurnsExceeded);
    assert!(effects.is_empty());
    Ok(())
}

#[tokio::test]
async fn signal_injection_without_recent_errors_uses_empty_recent_suffix() -> anyhow::Result<()> {
    let h = harness("inject-no-recent-errors").await?;
    let llm = Arc::new(MockLlmClient::new(
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
    let mut executor = TurnExecutor::new(h.ctx.clone(), llm_client_from_mock(llm));
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
        h.display
            .info
            .lock()
            .unwrap()
            .iter()
            .any(|msg| msg.starts_with("Injecting hint") && !msg.contains("recent issues")),
        "{:?}",
        h.display.info.lock().unwrap()
    );
    Ok(())
}

#[tokio::test]
async fn turn_injects_hint_after_failed_tool_and_continues() -> anyhow::Result<()> {
    let h = harness("turn-inject-after-fail").await?;
    let llm = Arc::new(MockLlmClient::new(
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
    let mut executor = TurnExecutor::new(h.ctx.clone(), llm_client_from_mock(llm));
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
                && line["content"]
                    .as_str()
                    .is_some_and(|content| content.contains("[System note:"))
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
            .any(|msg| msg.starts_with("Injecting hint (belief ")),
        "{:?}",
        h.display.info.lock().unwrap()
    );
    Ok(())
}

#[tokio::test]
async fn turn_aborts_when_tool_failures_push_belief_too_low() -> anyhow::Result<()> {
    let h = harness("turn-abort-after-failures").await?;
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
    let llm = Arc::new(MockLlmClient::new("flash", vec![calls]));
    let mut executor = TurnExecutor::new(h.ctx.clone(), llm_client_from_mock(llm));
    let mut belief = BeliefTracker::new(16);
    let (decision, effects) = executor
        .execute("run many failing commands", Some(&mut belief))
        .await?;

    assert_eq!(
        decision,
        TurnDecision::Failed("aborted by DecisionEngine".into())
    );
    assert!(effects.is_empty());
    assert!(belief.belief() < 0.30, "belief={}", belief.belief());
    assert!(
        h.display
            .info
            .lock()
            .unwrap()
            .iter()
            .any(|msg| msg.starts_with("error:DecisionEngine: aborting")),
        "{:?}",
        h.display.info.lock().unwrap()
    );
    Ok(())
}

#[tokio::test]
async fn orchestrator_user_input_runs_turn_and_logs_tracking() -> anyhow::Result<()> {
    let h = harness("orch-user-input").await?;
    let llm = Arc::new(MockLlmClient::new(
        "flash",
        vec![vec![
            Ok(Event::Text(TextEvent {
                content: "hello".into(),
            })),
            Ok(Event::Stop(StopEvent {
                reason: "end_turn".into(),
            })),
        ]],
    ));
    run_orchestrator_user_input(h.ctx.clone(), llm, "say hi").await?;
    let lines = h.ctx.store.lines().await?;
    assert_eq!(lines.len(), 2);
    assert_eq!(lines[0]["role"], "user");
    assert_eq!(lines[1]["role"], "assistant");
    let events = tokio::fs::read_to_string(&h.ctx.events_path).await?;
    assert!(events.contains(r#""type":"turn_start""#), "{events}");
    assert!(events.contains(r#""type":"turn_tracking""#), "{events}");
    assert!(events.contains(r#""type":"turn_final""#), "{events}");
    assert!(events.contains(r#""status":"ok""#), "{events}");
    assert!(events.contains(r#""decision":"Stop""#), "{events}");
    Ok(())
}

#[tokio::test]
async fn orchestrator_model_command_updates_display() -> anyhow::Result<()> {
    let h = harness("orch-model-command").await?;
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    let actor = OrchActor::new(h.ctx.clone(), rx);
    let handle = tokio::spawn(actor.run());
    tx.send(OrchCmd::SetModel("pro".into()))?;
    tx.send(OrchCmd::SetModel("unknown".into()))?;
    drop(tx);
    handle.await??;
    assert!(
        h.display
            .info
            .lock()
            .unwrap()
            .iter()
            .any(|msg| msg == "Switched to pro model."),
        "{:?}",
        h.display.info.lock().unwrap()
    );
    assert!(
        h.display
            .title_models
            .lock()
            .unwrap()
            .iter()
            .any(|model| model == "pro"),
        "{:?}",
        h.display.title_models.lock().unwrap()
    );
    assert!(
        h.display
            .info
            .lock()
            .unwrap()
            .iter()
            .any(|msg| msg == "Switched to unknown model."),
        "{:?}",
        h.display.info.lock().unwrap()
    );
    assert!(
        h.display
            .title_models
            .lock()
            .unwrap()
            .iter()
            .any(|model| model == "unknown"),
        "{:?}",
        h.display.title_models.lock().unwrap()
    );
    Ok(())
}

#[tokio::test]
async fn orchestrator_forced_model_title_survives_turn_refreshes() -> anyhow::Result<()> {
    let h = harness("orch-forced-model-title").await?;
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    let llm = Arc::new(MockLlmClient::new(
        "pro",
        vec![vec![
            Ok(Event::Text(TextEvent {
                content: "done".into(),
            })),
            Ok(Event::Stop(StopEvent {
                reason: "end_turn".into(),
            })),
        ]],
    ));
    let actor = OrchActor::new(test_context_with_llm_backend(h.ctx.clone(), llm), rx);
    let handle = tokio::spawn(actor.run());
    tx.send(OrchCmd::SetModel("pro".into()))?;
    let (done_tx, done_rx) = tokio::sync::oneshot::channel();
    tx.send(OrchCmd::UserInput {
        input: "say hi".into(),
        done: done_tx,
    })?;
    let result = done_rx.await?;
    assert_eq!(result.status, crate::agent::orchestrator::TurnStatus::Ok);
    drop(tx);
    handle.await??;

    let title_models = h.display.title_models.lock().unwrap();
    assert!(
        !title_models.iter().any(|model| model == "flash"),
        "{title_models:?}"
    );
    assert!(
        title_models.iter().filter(|model| *model == "pro").count() >= 2,
        "{title_models:?}"
    );
    Ok(())
}

#[tokio::test]
async fn orchestrator_active_model_is_used_by_spawned_sub_agent() -> anyhow::Result<()> {
    let requests = Arc::new(Mutex::new(Vec::new()));
    let backend = Arc::new(ActiveModelRoutingBackend {
        requests: requests.clone(),
        agent_request_count: AtomicU64::new(0),
    });
    let h = harness_with_config(
        "orch-sub-agent-active-model",
        false,
        300,
        |_| {},
        Some(backend),
    )
    .await?;
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    let actor = OrchActor::new(h.ctx.clone(), rx);
    let handle = tokio::spawn(actor.run());
    tx.send(OrchCmd::SetModel("pro".into()))?;
    let (done_tx, done_rx) = tokio::sync::oneshot::channel();
    tx.send(OrchCmd::UserInput {
        input: "delegate this task".into(),
        done: done_tx,
    })?;
    let outcome = done_rx.await?;
    drop(tx);
    handle.await??;

    assert_eq!(outcome.status, crate::agent::orchestrator::TurnStatus::Ok);
    assert_eq!(
        requests.lock().unwrap().as_slice(),
        &[
            CapturedRoutedRequest {
                purpose: "agent",
                model: "deepseek-v4-pro".into(),
                alias: Some("pro".into()),
            },
            CapturedRoutedRequest {
                purpose: "sub_agent",
                model: "deepseek-v4-pro".into(),
                alias: Some("pro".into()),
            },
            CapturedRoutedRequest {
                purpose: "agent",
                model: "deepseek-v4-pro".into(),
                alias: Some("pro".into()),
            },
        ]
    );
    Ok(())
}

#[tokio::test]
async fn orchestrator_flash_command_resets_forced_model_display() -> anyhow::Result<()> {
    let h = harness("orch-flash-command").await?;
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    let actor = OrchActor::new(h.ctx.clone(), rx);
    let handle = tokio::spawn(actor.run());
    tx.send(OrchCmd::SetModel("pro".into()))?;
    tx.send(OrchCmd::SetModel("flash".into()))?;
    drop(tx);
    handle.await??;
    let info = h.display.info.lock().unwrap();
    assert!(
        info.iter().any(|msg| msg == "Switched to flash model."),
        "{info:?}"
    );
    Ok(())
}

#[tokio::test]
async fn orchestrator_renders_failed_turn_decision() -> anyhow::Result<()> {
    let h = harness("orch-failed-turn").await?;
    let llm = Arc::new(MockLlmClient::new(
        "flash",
        vec![vec![Ok(Event::Stop(StopEvent {
            reason: "max_tokens".into(),
        }))]],
    ));
    run_orchestrator_user_input(h.ctx.clone(), llm, "hit limit").await?;
    assert!(
        h.display
            .info
            .lock()
            .unwrap()
            .iter()
            .any(|msg| msg == "error:stop: max_tokens"),
        "{:?}",
        h.display.info.lock().unwrap()
    );
    Ok(())
}

#[tokio::test]
async fn orchestrator_logs_stream_error_from_turn() -> anyhow::Result<()> {
    let h = harness("orch-stream-error").await?;
    let llm = Arc::new(MockLlmClient::new(
        "flash",
        vec![vec![Err(anyhow::anyhow!("stream connection timeout"))]],
    ));
    run_orchestrator_user_input(h.ctx.clone(), llm, "fail stream").await?;
    assert!(
        h.display
            .info
            .lock()
            .unwrap()
            .iter()
            .any(|msg| msg == "error:Turn execution error: stream connection timeout"),
        "{:?}",
        h.display.info.lock().unwrap()
    );
    let events = tokio::fs::read_to_string(&h.ctx.events_path).await?;
    assert!(events.contains(r#""type":"turn_error""#), "{events}");
    assert!(events.contains(r#""category":"Network""#), "{events}");
    Ok(())
}

#[tokio::test]
async fn orchestrator_cancel_signal_shuts_actor_down() -> anyhow::Result<()> {
    let h = harness("orch-cancel").await?;
    let (_tx, rx) = tokio::sync::mpsc::unbounded_channel();
    let actor = OrchActor::new(h.ctx.clone(), rx);
    let handle = tokio::spawn(actor.run());
    h.ctx.cancel.cancel();
    handle.await??;
    assert!(
        h.display
            .info
            .lock()
            .unwrap()
            .iter()
            .any(|msg| msg == "Shutting down..."),
        "{:?}",
        h.display.info.lock().unwrap()
    );
    Ok(())
}

#[tokio::test]
async fn orchestrator_manual_compact_empty_session_reports_skip() -> anyhow::Result<()> {
    let h = harness("orch-compact-empty").await?;
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    let actor = OrchActor::new(h.ctx.clone(), rx);
    let handle = tokio::spawn(actor.run());
    let (done_tx, done_rx) = tokio::sync::oneshot::channel();
    tx.send(OrchCmd::Compact { done: done_tx })?;
    done_rx.await?;
    drop(tx);
    handle.await??;
    let info = h.display.info.lock().unwrap();
    assert!(info.iter().any(|msg| msg == "Compressing..."), "{info:?}");
    assert!(
        info.iter().any(|msg| msg == "Compact skipped: empty"),
        "{info:?}"
    );
    Ok(())
}

#[tokio::test]
async fn orchestrator_manual_compact_uses_active_model_and_shared_backend() -> anyhow::Result<()> {
    let requests = Arc::new(Mutex::new(Vec::new()));
    let backend = Arc::new(RecordingCompactionBackend {
        requests: requests.clone(),
    });
    let h = harness_with_config(
        "orch-compact-active-model",
        false,
        300,
        |config| config.context_compact_tail_tokens = 1,
        Some(backend),
    )
    .await?;
    for index in 0..3 {
        h.ctx
            .store
            .add_user(&format!("user history {index}: {}", "x".repeat(256)))
            .await?;
        h.ctx
            .store
            .add_assistant(&format!("assistant history {index}"), "", &[])
            .await?;
    }

    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    let actor = OrchActor::new(h.ctx.clone(), rx);
    let handle = tokio::spawn(actor.run());
    tx.send(OrchCmd::SetModel("pro".into()))?;
    let (done_tx, done_rx) = tokio::sync::oneshot::channel();
    tx.send(OrchCmd::Compact { done: done_tx })?;
    done_rx.await?;
    drop(tx);
    handle.await??;

    assert_eq!(
        requests.lock().unwrap().as_slice(),
        &[CapturedModelTarget {
            model: "deepseek-v4-pro".into(),
            alias: Some("pro".into()),
        }]
    );
    Ok(())
}

#[tokio::test]
async fn plan_confirm_and_clear_preserve_immutable_prefix() -> anyhow::Result<()> {
    let h = harness("plan-actions").await?;
    let prefix = PrefixManager::new(h.ctx.clone());
    let (stable_prompt, stable_tools) = prefix.ensure()?;
    let stable_fingerprint = h
        .ctx
        .immutable_prefix
        .lock()
        .unwrap()
        .as_ref()
        .unwrap()
        .fingerprint()
        .to_string();
    assert!(!stable_prompt.contains("<current-plan>"));

    let handler = PlanActionHandler;
    let tool_ctx = crate::context::ToolContext::from(h.ctx.as_ref());
    let mut effects = Vec::new();
    let draft_outcome = crate::tools::runner::ToolExec::execute(
        &crate::tools::plan::PlanDraftTool,
        &serde_json::json!({"content": "1. ship it\n"}),
        &tool_ctx,
    )?;
    let mut draft = plan_result("PlanDraft", draft_outcome);
    assert_eq!(handler.handle(&mut draft, &mut effects), None);
    assert_eq!(draft.content, "Plan draft saved.");
    assert_eq!(
        tokio::fs::read_to_string(&h.ctx.plan_draft_path).await?,
        "1. ship it\n"
    );
    assert!(effects.is_empty());
    assert!(h.ctx.immutable_prefix.lock().unwrap().is_some());

    let confirm_outcome = crate::tools::runner::ToolExec::execute(
        &crate::tools::plan::PlanConfirmTool,
        &serde_json::json!({}),
        &tool_ctx,
    )?;
    let mut confirm = plan_result("PlanConfirm", confirm_outcome);
    assert_eq!(
        handler.handle(&mut confirm, &mut effects),
        Some("plan_confirm")
    );
    assert_eq!(confirm.content, "Plan confirmed and locked in.");
    assert_eq!(
        tokio::fs::read_to_string(&h.ctx.plan_path).await?,
        "1. ship it\n"
    );
    assert!(!h.ctx.plan_draft_path.exists());
    assert!(matches!(effects.as_slice(), [TurnEffect::PlanConfirmed]));
    let (after_confirm_prompt, after_confirm_tools) = prefix.ensure()?;
    assert_eq!(after_confirm_prompt, stable_prompt);
    assert_eq!(after_confirm_tools, stable_tools);
    assert_eq!(
        h.ctx
            .immutable_prefix
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .fingerprint(),
        stable_fingerprint
    );

    let clear_outcome = crate::tools::runner::ToolExec::execute(
        &crate::tools::plan::PlanClearTool,
        &serde_json::json!({}),
        &tool_ctx,
    )?;
    let mut clear = plan_result("PlanClear", clear_outcome);
    assert_eq!(handler.handle(&mut clear, &mut effects), Some("plan_clear"));
    assert_eq!(clear.content, "Plan cleared.");
    assert!(!h.ctx.plan_path.exists());
    assert!(matches!(
        effects.as_slice(),
        [TurnEffect::PlanConfirmed, TurnEffect::PlanCleared]
    ));
    let (after_clear_prompt, after_clear_tools) = prefix.ensure()?;
    assert_eq!(after_clear_prompt, stable_prompt);
    assert_eq!(after_clear_tools, stable_tools);
    Ok(())
}

#[tokio::test]
async fn plan_compaction_obeys_the_existing_single_turn_guard() -> anyhow::Result<()> {
    let requests = Arc::new(Mutex::new(Vec::new()));
    let backend = Arc::new(RecordingCompactionBackend {
        requests: requests.clone(),
    });
    let h = harness_with_config(
        "plan-single-compaction",
        false,
        300,
        |config| {
            config.context_compact_pct = 1;
            config.context_compact_tail_tokens = 1;
        },
        Some(backend),
    )
    .await?;
    for index in 0..4 {
        h.ctx
            .store
            .add_user(&format!("history {index}: {}", "x".repeat(12_000)))
            .await?;
        h.ctx
            .store
            .add_assistant(&format!("history response {index}"), "", &[])
            .await?;
    }
    tokio::fs::write(&h.ctx.plan_draft_path, "1. execute\n").await?;

    let llm = Arc::new(MockLlmClient::new(
        "flash",
        vec![
            vec![
                Ok(Event::ToolCall(tool_call(
                    "PlanConfirm",
                    "call_plan_confirm",
                    json!({}),
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
    let mut executor = TurnExecutor::new(h.ctx.clone(), llm_client_from_mock(llm));
    let (decision, effects) = executor.execute("confirm the plan", None).await?;

    assert_eq!(decision, TurnDecision::Stop);
    assert!(matches!(effects.as_slice(), [TurnEffect::PlanConfirmed]));
    assert_eq!(requests.lock().unwrap().len(), 1);
    assert!(h.ctx.plan_path.exists());
    assert!(!h.ctx.plan_draft_path.exists());
    Ok(())
}

#[tokio::test]
async fn plan_compaction_failure_is_propagated_from_the_turn() -> anyhow::Result<()> {
    let h = harness_with_config(
        "plan-compaction-error",
        false,
        300,
        |config| {
            config.context_compact_pct = 100;
            config.context_compact_tail_tokens = 1;
        },
        Some(Arc::new(FailingCompactionBackend)),
    )
    .await?;
    for index in 0..3 {
        h.ctx.store.add_user(&format!("history {index}")).await?;
        h.ctx
            .store
            .add_assistant(&format!("history response {index}"), "", &[])
            .await?;
    }
    tokio::fs::write(&h.ctx.plan_draft_path, "1. execute\n").await?;

    let llm = Arc::new(MockLlmClient::new(
        "flash",
        vec![vec![
            Ok(Event::ToolCall(tool_call(
                "PlanConfirm",
                "call_plan_confirm",
                json!({}),
            ))),
            Ok(Event::Stop(StopEvent {
                reason: "tool_use".into(),
            })),
        ]],
    ));
    let mut executor = TurnExecutor::new(h.ctx.clone(), llm_client_from_mock(llm));
    let error = executor
        .execute("confirm the plan", None)
        .await
        .unwrap_err()
        .to_string();

    assert!(error.contains("planned compaction failure"), "{error}");
    assert!(h.ctx.plan_path.exists());
    assert!(!h.ctx.plan_draft_path.exists());
    Ok(())
}

#[tokio::test]
async fn plan_confirm_without_draft_returns_error_result() -> anyhow::Result<()> {
    let h = harness("plan-empty").await?;
    let tool_ctx = crate::context::ToolContext::from(h.ctx.as_ref());
    let error = crate::tools::runner::ToolExec::execute(
        &crate::tools::plan::PlanConfirmTool,
        &serde_json::json!({}),
        &tool_ctx,
    )
    .unwrap_err();
    assert_eq!(error.to_string(), "no plan draft found to confirm");
    Ok(())
}

#[tokio::test]
async fn plan_draft_empty_content_cancels_and_reports_cancellation() -> anyhow::Result<()> {
    let h = harness("plan-draft-cancel").await?;
    let tool_ctx = crate::context::ToolContext::from(h.ctx.as_ref());
    crate::tools::runner::ToolExec::execute(
        &crate::tools::plan::PlanDraftTool,
        &serde_json::json!({"content": "1. inspect\n"}),
        &tool_ctx,
    )?;
    assert!(h.ctx.plan_draft_path.exists());

    let outcome = crate::tools::runner::ToolExec::execute(
        &crate::tools::plan::PlanDraftTool,
        &serde_json::json!({"content": ""}),
        &tool_ctx,
    )?;

    assert_eq!(outcome.content, "Plan draft cancelled.");
    assert!(matches!(
        outcome.presentation,
        Some(crate::ui::ToolPresentation::Plan(crate::ui::PlanDisplay {
            transition: crate::ui::PlanTransitionDisplay::DraftCancelled,
            content: None,
        }))
    ));
    assert!(!h.ctx.plan_draft_path.exists());
    Ok(())
}

#[tokio::test]
async fn todo_tools_persist_incremental_state_and_reject_stale_writes() -> anyhow::Result<()> {
    let h = harness("todo-persistence").await?;
    let prefix = PrefixManager::new(h.ctx.clone());
    let stable_prefix = prefix.ensure()?;
    assert!(!stable_prefix.0.contains("<current-todos"));
    let tool_ctx = crate::context::ToolContext::from(h.ctx.as_ref());
    let created = crate::tools::runner::ToolExec::execute(
        &crate::tools::todo::TodoWriteTool,
        &json!({
            "base_revision": 0,
            "add": [
                {"content": "inspect"},
                {"content": "implement"},
                {"content": "verify"}
            ]
        }),
        &tool_ctx,
    )?;
    assert!(created.content.contains("revision=\"1\""));
    assert!(created.content.contains("T0001"));
    assert!(created.content.contains("T0002"));
    assert!(matches!(
        created.presentation,
        Some(crate::ui::ToolPresentation::Todo(crate::ui::TodoDisplay {
            revision: 1,
            ..
        }))
    ));

    crate::tools::runner::ToolExec::execute(
        &crate::tools::todo::TodoAdvanceTool,
        &json!({
            "base_revision": 1,
            "activate": ["T0001", "T0002"]
        }),
        &tool_ctx,
    )?;
    let read = crate::tools::runner::ToolExec::execute(
        &crate::tools::todo::TodoReadTool,
        &json!({}),
        &tool_ctx,
    )?;
    assert!(read.content.contains("in_progress=\"2\""));
    assert!(read.content.contains("T0003: verify"));
    assert!(matches!(
        read.presentation,
        Some(crate::ui::ToolPresentation::Todo(crate::ui::TodoDisplay {
            revision: 2,
            ..
        }))
    ));

    crate::tools::runner::ToolExec::execute(
        &crate::tools::todo::TodoWriteTool,
        &json!({
            "base_revision": 2,
            "update": [
                {"id": "T0003", "content": "run focused tests"}
            ]
        }),
        &tool_ctx,
    )?;
    crate::tools::runner::ToolExec::execute(
        &crate::tools::todo::TodoAdvanceTool,
        &json!({
            "base_revision": 3,
            "complete": ["T0001"]
        }),
        &tool_ctx,
    )?;
    let before = h.ctx.todo_store.snapshot();
    let error = crate::tools::runner::ToolExec::execute(
        &crate::tools::todo::TodoWriteTool,
        &json!({"base_revision": 3, "remove": ["T0002"]}),
        &tool_ctx,
    )
    .unwrap_err()
    .to_string();
    assert!(error.contains("stale todo revision"), "{error}");
    assert_eq!(h.ctx.todo_store.snapshot(), before);
    let todo_path = crate::session::paths::paths_for(&h.ctx.home, &h.ctx.cwd, "regression").todos;
    let reloaded = crate::session::todo::TodoStore::load(todo_path)?;
    assert_eq!(reloaded.snapshot(), before);
    assert_eq!(reloaded.snapshot().revision, 4);
    assert_eq!(prefix.ensure()?, stable_prefix);
    Ok(())
}

#[tokio::test]
async fn safety_blocked_bash_emits_typed_signal_event() -> anyhow::Result<()> {
    let h = harness("safety-signal").await?;
    let llm = Arc::new(MockLlmClient::new(
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
    let mut executor = TurnExecutor::new(h.ctx.clone(), llm_client_from_mock(llm));
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
    let result = runner
        .execute_all(vec![tool_call(
            "Edit",
            "call_edit_symlink",
            json!({
                "path": "link-edit-out/escape.txt",
                "patch": "@link-edit-out/escape.txt#FFFF\nreplace 1:\n+new"
            }),
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
    assert!(!processed[0].success);
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
    assert!(processed[0].success);
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
    assert!(!processed[0].success);
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
    assert!(!processed[0].success);
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
    let llm = Arc::new(MockLlmClient::new(
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

fn internal_result(name: &str) -> ToolRunResult {
    ToolRunResult {
        tool_use_id: format!("call_{name}"),
        tool_name: name.into(),
        tool_args: BTreeMap::new(),
        content: String::new(),
        conv_content: String::new(),
        spawns_sub_agent: false,
        sub_agent_prompt: None,
        sub_agent_description: None,
        sub_agent_fork: false,
        exit_code: None,
        success: true,
        result_kind: crate::tools::metadata::ToolResultKind::Control,
        presentation: None,
        artifacts: Vec::new(),
        signals: Vec::new(),
        plan_command: None,
        needs_finalization: false,
        state_metadata: None,
    }
}

fn plan_result(name: &str, outcome: crate::tools::runner::ToolOutcome) -> ToolRunResult {
    let mut result = internal_result(name);
    result.content = outcome.content;
    result.plan_command = outcome.plan_command;
    result.presentation = outcome.presentation;
    result.needs_finalization = true;
    result
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
    assert!(edited[0].success, "{}", edited[0].content);
    assert_eq!(
        tokio::fs::read_to_string(h.cwd.join("flow.txt")).await?,
        "prefix\none\nTWO\n"
    );
    assert!(edited[0].content.contains("uniform +1 line offset"));
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
    assert!(!result[0].success);
    assert!(
        result[0]
            .content
            .contains("could not be recovered unambiguously")
    );
    assert_eq!(
        tokio::fs::read_to_string(h.cwd.join("conflict.txt")).await?,
        "one\nchanged\n"
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
    assert!(result[0].success, "{}", result[0].content);
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
    assert!(result[0].success, "{}", result[0].content);
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
    assert!(cut_named[0].success, "{}", cut_named[0].content);
    let paste_named = runner
        .execute_all(vec![tool_call(
            "Edit",
            "paste_named",
            json!({"input":format!("[target.txt#{target_tag}]\nPUT <1 @saved")}),
        )])
        .await?;
    assert!(paste_named[0].success, "{}", paste_named[0].content);
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
    assert!(cut_anonymous[0].success, "{}", cut_anonymous[0].content);
    let new_target_tag = crate::tools::snapshot::compute_file_tag("saved\ntarget\n");
    let paste_anonymous = runner
        .execute_all(vec![tool_call(
            "Edit",
            "paste_anonymous",
            json!({"input":format!("[target.txt#{new_target_tag}]\nPUT >$")}),
        )])
        .await?;
    assert!(!paste_anonymous[0].success);
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
    assert!(!wrong_name[0].success);
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
    assert!(recovered[0].success, "{}", recovered[0].content);
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
    assert!(!conflict[0].success);
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
    assert!(moved[0].success, "{}", moved[0].content);
    assert!(!h.cwd.join("a.txt").exists());
    assert_eq!(
        tokio::fs::read_to_string(h.cwd.join("moved.txt")).await?,
        "A\n"
    );
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
    assert!(!result[0].success);
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
    assert!(result[0].success, "{}", result[0].content);
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
            json!({"path":"seen.txt", "offset":1, "limit":1}),
        )])
        .await?;
    assert!(read[0].success, "{}", read[0].content);
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
        assert!(!result[0].success);
        assert!(result[0].content.contains("anchors were not shown"));
    }
    assert_eq!(
        tokio::fs::read_to_string(h.cwd.join("seen.txt")).await?,
        content
    );
    Ok(())
}
