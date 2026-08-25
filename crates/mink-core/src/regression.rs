use crate::agent::belief::BeliefTracker;
use crate::agent::orchestrator::{OrchActor, OrchCmd};
use crate::agent::plan_actions::PlanActionHandler;
use crate::agent::prefix::PrefixManager;
use crate::agent::sub_coordinator::{SubAgentCoordinator, SubAgentRunner};
use crate::agent::sub_executor::{SubAgentExecutor, SubAgentResult};
use crate::agent::turn::{TurnDecision, TurnExecutor};
use crate::config::{OutputFormat, ResolvedConfig as Config};
use crate::context::{AgentSharedContext, ToolConfig, ToolContext};
use crate::guard::collector::{Signal, SignalKind};
use crate::llm::client::{
    LlmBackend, LlmPurpose, LlmRequest, LlmResponseStream, OpenAiCompatibleBackend,
};
use crate::llm::mock::MockLlmBackend;
use crate::protocol::{
    ErrorEvent, Event, RetryEvent, StopEvent, TextEvent, ThinkingEvent, ToolCallEvent, UsageEvent,
};
use crate::session::compaction::CompactionEngine;
use crate::session::paths;
use crate::tools::catalog::ToolCatalog;
use crate::tools::runner::{ToolExecution, ToolRunner};
use crate::ui::{Display, StatsSnapshot};
use futures::StreamExt;
use serde_json::json;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

struct PendingLlmBackend;

#[async_trait::async_trait]
impl LlmBackend for PendingLlmBackend {
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

struct IdleAfterTextLlmBackend;

#[async_trait::async_trait]
impl LlmBackend for IdleAfterTextLlmBackend {
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
    fn render_tool_call(&self, _call: &crate::ui::ToolCallDisplay<'_>) {}
    fn render_tool_result(&self, _result: &crate::ui::PresentedToolResultDisplay<'_>) {}
    fn render_stop(&self, _reason: &str) {}
    fn render_signal(&self, _kind: &str, _severity: f64, _message: &str) {}
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
    fn render_sub_agent_output(
        &self,
        _sid: &str,
        _st: &str,
        _th: &str,
        _tx: &str,
        _it: u64,
        _ot: u64,
    ) {
    }
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
    let (store, stats, artifacts) = crate::session::init::init_session_base_at(&spaths).await?;
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
        &cfg.skills,
    )?);
    let llm_backend = llm_backend.unwrap_or_else(|| {
        Arc::new(crate::llm::client::OpenAiCompatibleBackend::deepseek_defaults())
    });
    let cancel = crate::cancel::CancellationToken::new();
    let interrupt = Arc::new(AtomicBool::new(false));
    let event_log_writer = crate::session::event_log::EventLogWriter::start(spaths.events.clone());
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
        Some(event_log_writer.clone()),
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
        prefix_source: None,
        model_capabilities: Arc::new(
            crate::capabilities::model_capabilities::SessionModelCapabilities::unsupported(
                "test",
            ),
        ),
        image_cache: Arc::new(crate::session::image_cache::ImageCache::new(
            &std::env::temp_dir(),
        )),
        this_turn_image_ids: Arc::new(Mutex::new(std::collections::HashSet::new())),
        warned_image_ids: Arc::new(Mutex::new(std::collections::HashSet::new())),
        events_path: spaths.events,
        summary_path: spaths.summary,
        plan_path: spaths.plan,
        plan_draft_path: spaths.plan_draft,
        immutable_prefix: Mutex::new(None),
        is_sub_agent,
        interrupt,
        event_log_warned: AtomicBool::new(false),
        event_log_writer: Some(event_log_writer),
        stream_flush_last: Mutex::new(None),
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
    ToolCallEvent {
        name: name.into(),
        id: id.into(),
        input_json: input,
        fields,
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
    let turn_id = crate::runtime::TurnId::new("test-turn");
    let emitter = std::sync::Arc::new(crate::runtime::TurnEventEmitter::new(
        turn_id.clone(),
        None,
        None,
    ));
    tx.send(OrchCmd::UserInput {
        input: input.to_string(),
        turn_id,
        emitter,
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
        prefix_source: ctx.prefix_source.clone(),
        model_capabilities: ctx.model_capabilities.clone(),
        image_cache: ctx.image_cache.clone(),
        this_turn_image_ids: ctx.this_turn_image_ids.clone(),
        warned_image_ids: ctx.warned_image_ids.clone(),
        events_path: ctx.events_path.clone(),
        summary_path: ctx.summary_path.clone(),
        plan_path: ctx.plan_path.clone(),
        plan_draft_path: ctx.plan_draft_path.clone(),
        immutable_prefix: Mutex::new(None),
        is_sub_agent: ctx.is_sub_agent,
        interrupt: ctx.interrupt.clone(),
        event_log_warned: AtomicBool::new(false),
        event_log_writer: ctx.event_log_writer.clone(),
        stream_flush_last: Mutex::new(None),
    })
}

fn llm_backend_from_mock(mock: Arc<MockLlmBackend>) -> Arc<dyn LlmBackend> {
    mock
}

fn internal_result(name: &str) -> ToolExecution {
    ToolExecution {
        tool_use_id: format!("call_{name}"),
        tool_name: name.into(),
        tool_args: BTreeMap::new(),
        content: String::new(),
        conv_content: String::new(),
        spawns_sub_agent: false,
        sub_agent_prompt: None,
        sub_agent_fork: false,
        exit_code: None,
        status: crate::tools::metadata::ToolStatus::Succeeded,
        result_kind: crate::tools::metadata::ToolResultKind::Control,
        presentation: None,
        artifacts: Vec::new(),
        signals: Vec::new(),
        plan_command: None,
        needs_finalization: false,
        state_metadata: None,
        image_attachment: None,
    }
}

fn plan_result(name: &str, outcome: crate::tools::runner::ToolOutcome) -> ToolExecution {
    let mut result = internal_result(name);
    result.content = outcome.content;
    result.plan_command = outcome.plan_command;
    result.presentation = outcome.presentation;
    result.needs_finalization = true;
    result
}

#[path = "regression/fixture.rs"]
mod fixture;
#[path = "regression/orchestrator_session.rs"]
mod orchestrator_session;
#[path = "regression/signal.rs"]
mod signal;
#[path = "regression/sub_agent.rs"]
mod sub_agent;
#[path = "regression/tool_boundary.rs"]
mod tool_boundary;
#[path = "regression/tool_file.rs"]
mod tool_file;
#[path = "regression/turn.rs"]
mod turn;
