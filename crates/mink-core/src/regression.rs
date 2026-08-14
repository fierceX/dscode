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
use crate::tools::catalog::ToolCatalog;
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
    let cancel = crate::cancel::CancellationToken::new();
    let interrupt = Arc::new(AtomicBool::new(false));
    let compaction = Arc::new(CompactionEngine::new(
        store.clone(),
        spaths.summary.clone(),
        crate::config::api_url(&cfg),
        &cfg,
        stats.clone(),
        usage.clone(),
        cfg.session_id.clone(),
        display.clone(),
        cancel.clone(),
        interrupt.clone(),
        llm_backend.clone(),
    )?);
    let tool_config = ToolConfig::from_config(&cfg);
    let todo_store = Arc::new(crate::session::todo::TodoStore::load(spaths.todos.clone())?);
    let (tool_resolution_context, tool_surface, tool_capabilities) =
        crate::context::resolve_tool_runtime(&tool_config, is_sub_agent, false, &[])?;
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
        read_memo: Arc::new(Mutex::new(crate::tools::read_memo::ReadMemo::new())),
        memo_epoch: compaction.memo_epoch(),
        memo_mutation: Arc::new(std::sync::atomic::AtomicU64::new(0)),
        snapshots: Arc::new(Mutex::new(
            crate::tools::snapshot::FileSnapshotStore::default(),
        )),
        stats,
        usage,
        compaction,
        cancel,
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
        custom_tools: Arc::new(Vec::new()),
        events_path: spaths.events,
        summary_path: spaths.summary,
        plan_path: spaths.plan,
        plan_draft_path: spaths.plan_draft,
        immutable_prefix: Mutex::new(None),
        is_sub_agent,
        interrupt,
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
        read_memo: ctx.read_memo.clone(),
        memo_epoch: ctx.memo_epoch.clone(),
        memo_mutation: ctx.memo_mutation.clone(),
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
        custom_tools: ctx.custom_tools.clone(),
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
async fn signal_recovery_guard_blocks_whole_batch() -> anyhow::Result<()> {
    let h = harness("guard-blocks-batch").await?;
    let llm = Arc::new(MockLlmClient::new(
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
    let mut executor = TurnExecutor::new(h.ctx.clone(), llm_client_from_mock(llm));
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
async fn soft_only_editloop_does_not_inject_above_warn_zone() -> anyhow::Result<()> {
    // SIGNAL_RESPONSE_REDESIGN S3c：软信号单独出现且信念仍在提醒区之上时，
    // 不注入任何消息（记录但不干预），避免打断正常的写->编译->修流程。
    let h = harness("soft-only-no-inject").await?;
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
    // R3 被禁用时（replan_max_attempts=0），Abort 直接进入 R4 用户接管。
    let h = harness_with_config(
        "turn-abort-after-failures",
        false,
        300,
        |cfg| cfg.signal.replan_max_attempts = 0,
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
    let llm = Arc::new(MockLlmClient::new("flash", vec![calls]));
    let mut executor = TurnExecutor::new(h.ctx.clone(), llm_client_from_mock(llm));
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
    // SIGNAL_RESPONSE_REDESIGN R4 降级链：非交互环境 Abort 先尝试 R3 策略重启，
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
    let llm = Arc::new(MockLlmClient::new(
        "flash",
        vec![parent_failures, replan_child, parent_continue],
    ));
    let ctx = test_context_with_llm_backend(h.ctx.clone(), llm.clone());
    let mut executor = TurnExecutor::new(ctx, llm_client_from_mock(llm));
    let mut belief = BeliefTracker::new(16);
    let (decision, effects) = executor
        .execute("run many failing commands", Some(&mut belief))
        .await?;

    assert_eq!(decision, TurnDecision::Stop);
    assert!(effects.is_empty());
    // R3 成功后信念被重置为新证据基线。
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
    // SIGNAL_RESPONSE_REDESIGN R3 升级路径：同一输入内连续第 2 次 Warning
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
    let llm = Arc::new(MockLlmClient::new(
        "flash",
        vec![
            failing_batch("a"),
            failing_batch("b"),
            replan_child,
            parent_finish,
        ],
    ));
    let ctx = test_context_with_llm_backend(h.ctx.clone(), llm.clone());
    let mut executor = TurnExecutor::new(ctx, llm_client_from_mock(llm));
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
    // B1 修复目标：Warning 级回滚必须把编辑循环窗口内的文件恢复到
    // 最后一次 Read 记录的基线（而不是 record_edit 记录的编辑后内容）。
    let h = harness("hashline-rollback").await?;
    let original = "line1\nline2\nline3\n";
    tokio::fs::write(h.cwd.join("a.rs"), original).await?;
    let tag = crate::tools::snapshot::compute_file_tag(original);
    let llm = Arc::new(MockLlmClient::new(
        "flash",
        rollback_script_after_edit(&tag),
    ));
    let ctx = test_context_with_llm_backend(h.ctx.clone(), llm.clone());
    let mut executor = TurnExecutor::new(ctx, llm_client_from_mock(llm));
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
    // N2 修复：回滚经 atomic_replace 换文件，必须保留原权限（可执行脚本 +x）。
    use std::os::unix::fs::PermissionsExt;
    let h = harness("rollback-perms").await?;
    let original = "#!/bin/sh\necho hi\n";
    tokio::fs::write(h.cwd.join("a.rs"), original).await?;
    tokio::fs::set_permissions(h.cwd.join("a.rs"), std::fs::Permissions::from_mode(0o755)).await?;
    let tag = crate::tools::snapshot::compute_file_tag(original);
    let llm = Arc::new(MockLlmClient::new(
        "flash",
        rollback_script_after_edit(&tag),
    ));
    let ctx = test_context_with_llm_backend(h.ctx.clone(), llm.clone());
    let mut executor = TurnExecutor::new(ctx, llm_client_from_mock(llm));
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
    // B1 修复目标（Replace 模式）：Replace 编辑不记录快照，回滚目标即最后一次 Read。
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
    let llm = Arc::new(MockLlmClient::new(
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
    let mut executor = TurnExecutor::new(ctx, llm_client_from_mock(llm));
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
    // D2 修复目标：只回滚最近窗口内的编辑；窗口之前的编辑保持不动。
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
    let llm = Arc::new(MockLlmClient::new("flash", script));
    let ctx = test_context_with_llm_backend(h.ctx.clone(), llm.clone());
    let mut executor = TurnExecutor::new(ctx, llm_client_from_mock(llm));
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
    // D1 修复目标：单次软失败不干预；累计 >= 2 次软失败必须触发 R1 证据注入。
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
    let llm = Arc::new(MockLlmClient::new(
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
    let mut executor = TurnExecutor::new(ctx, llm_client_from_mock(llm));
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
    // N1 修复目标：成功调用不得推高 soft_failures 计数。14 次干净调用 + 1 次软失败
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
    let llm = Arc::new(MockLlmClient::new(
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
    let mut executor = TurnExecutor::new(ctx, llm_client_from_mock(llm));
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
    // B3 修复目标：R3 子代理初始化失败必须降级为接管/失败，而不是整轮 Err。
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
    let llm = Arc::new(MockLlmClient::new("flash", vec![calls]));
    let ctx = test_context_with_llm_backend(h.ctx.clone(), llm.clone());
    let mut executor = TurnExecutor::new(ctx, llm_client_from_mock(llm));
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
    done_rx.await??;
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
    done_rx.await??;
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
        error_code: None,
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

    let llm = Arc::new(MockLlmClient::new(
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
    let mut executor = TurnExecutor::new(h.ctx.clone(), llm_client_from_mock(llm));
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
    assert!(!result[0].success);
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
        !retry[0].success,
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
        assert!(result[0].success, "{}", result[0].content);
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
    assert!(!third[0].success);
    assert!(third[0].content.contains("will continue to fail"));

    let alternate = runner
        .execute_all(vec![tool_call(
            "Edit",
            "noop_alternate",
            json!({"input":format!("[noop.txt#{tag}]\nCUT 1\nPUT >1")}),
        )])
        .await?;
    assert!(alternate[0].success);
    assert!(alternate[0].content.contains("soft no-op 1/2"));

    let changed = runner
        .execute_all(vec![tool_call(
            "Edit",
            "noop_change",
            json!({"input":format!("[noop.txt#{tag}]\nPUT 1:\n+changed")}),
        )])
        .await?;
    assert!(changed[0].success, "{}", changed[0].content);
    let changed_tag = crate::tools::snapshot::compute_file_tag("changed\n");
    let after_commit = runner
        .execute_all(vec![tool_call(
            "Edit",
            "noop_after_commit",
            json!({"input":format!("[noop.txt#{changed_tag}]\nCUT 1\nPUT >1")}),
        )])
        .await?;
    assert!(after_commit[0].success);
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
    assert!(!result[0].success);
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
    assert!(result[0].success, "{}", result[0].content);
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
    assert!(!result[0].success);
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
    assert!(!result[0].success);
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
    assert!(first[0].success, "{}", first[0].content);
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
    assert!(!stale[0].success);
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
    assert!(!result[0].success);
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
    assert!(result[0].success, "{}", result[0].content);
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
    assert!(result[0].success, "{}", result[0].content);
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
    assert!(result[0].success, "{}", result[0].content);
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
            json!({"path":"seen.txt:1-1"}),
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
    assert!(!result[0].success);
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
    assert!(!result[0].success);
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
    assert!(result[0].success, "{}", result[0].content);
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
    assert!(ranged[0].success, "{}", ranged[0].content);
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
    assert!(!result[0].success);
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
    assert!(first[0].success, "{}", first[0].content);
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
    assert!(second[0].success, "{}", second[0].content);
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
    assert!(written[0].success, "{}", written[0].content);
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
    assert!(broken[0].success, "{}", broken[0].content);
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
    assert!(valid[0].success, "{}", valid[0].content);
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
    assert!(result[0].success, "{}", result[0].content);
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
    assert!(read[0].success, "{}", read[0].content);
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
    assert!(first[0].success, "{}", first[0].content);
    // Re-read for the current tag, then retry the same body: idempotent success.
    let reread = runner
        .execute_all(vec![tool_call(
            "Read",
            "idem_reread",
            json!({"path": "a.md"}),
        )])
        .await?;
    assert!(reread[0].success, "{}", reread[0].content);
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
    assert!(retry[0].success, "{}", retry[0].content);
    assert!(
        retry[0].content.contains("already applied (idempotent)"),
        "{}",
        retry[0].content
    );
    Ok(())
}

/// P0-J: run one trace fixture (JSON: setup.files/config, steps, asserts).
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
        outputs.push((result[0].success, result[0].content.clone()));
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
    run_trace_fixture(include_str!("../tests/fixtures/traces/repeated_read.json")).await?;
    run_trace_fixture(include_str!("../tests/fixtures/traces/param_guess.json")).await?;
    run_trace_fixture(include_str!("../tests/fixtures/traces/no_change_loop.json")).await?;
    run_trace_fixture(include_str!("../tests/fixtures/traces/disabled_tool.json")).await?;
    run_trace_fixture(include_str!("../tests/fixtures/traces/big_file.json")).await?;
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
        assert!(read[0].success, "{}", read[0].content);
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
    assert!(multi[0].success, "{}", multi[0].content);
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
            !result[0].success,
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
            !result[0].success,
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
    assert!(result[0].success, "{}", result[0].content);
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
    assert!(raw[0].success, "{}", raw[0].content);
    let non_raw = runner
        .execute_all(vec![tool_call(
            "Read",
            "non_raw_after_raw",
            json!({"path": "a.md:1-2"}),
        )])
        .await?;
    assert!(non_raw[0].success, "{}", non_raw[0].content);
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
    assert!(read[0].success, "{}", read[0].content);
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
    assert!(edit[0].success, "{}", edit[0].content);
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
    assert!(result[0].success, "{}", result[0].content);
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
    assert!(read[0].success, "{}", read[0].content);
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
    assert!(edit[0].success, "{}", edit[0].content);
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
