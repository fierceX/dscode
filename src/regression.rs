use crate::agent::belief::BeliefTracker;
use crate::agent::orchestrator::{OrchActor, OrchCmd};
use crate::agent::plan_actions::PlanActionHandler;
use crate::agent::prefix::PrefixManager;
use crate::agent::sub_coordinator::SubAgentCoordinator;
use crate::agent::sub_executor::{SubAgentExecutor, SubAgentResult};
use crate::agent::turn::{TurnDecision, TurnEffect, TurnExecutor};
use crate::config::{Config, OutputFormat};
use crate::context::{AgentSharedContext, ToolConfig, ToolContext};
use crate::guard::collector::{Signal, SignalKind};
use crate::llm::client::{AsyncLlClient, LlmClient};
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
impl LlmClient for PendingLlmClient {
    fn model(&self) -> &str {
        "flash"
    }

    async fn stream(
        &self,
        _ctx: &AgentSharedContext,
        _messages_json: &[serde_json::Value],
        _tools_json: &[serde_json::Value],
        _system_prompt: &str,
    ) -> anyhow::Result<Box<dyn futures::Stream<Item = anyhow::Result<Event>> + Unpin + Send>> {
        Ok(Box::new(futures::stream::pending()))
    }
}

struct IdleAfterTextLlmClient;

#[async_trait::async_trait]
impl LlmClient for IdleAfterTextLlmClient {
    fn model(&self) -> &str {
        "flash"
    }

    async fn stream(
        &self,
        _ctx: &AgentSharedContext,
        _messages_json: &[serde_json::Value],
        _tools_json: &[serde_json::Value],
        _system_prompt: &str,
    ) -> anyhow::Result<Box<dyn futures::Stream<Item = anyhow::Result<Event>> + Unpin + Send>> {
        let stream = futures::stream::iter(vec![Ok(Event::Text(TextEvent {
            content: "partial".into(),
        }))])
        .chain(futures::stream::pending());
        Ok(Box::new(stream))
    }
}

struct NoopDisplay {
    info: Mutex<Vec<String>>,
}

impl NoopDisplay {
    fn new() -> Self {
        Self {
            info: Mutex::new(Vec::new()),
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
    fn render_title_update(&self, _model: &str, _stats: &StatsSnapshot) {}
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
    Ok(harness_with_config(name, false, 300, configure).await?.ctx)
}

async fn harness_with(
    name: &str,
    is_sub_agent: bool,
    sub_agent_timeout_secs: i32,
) -> anyhow::Result<TestHarness> {
    harness_with_config(name, is_sub_agent, sub_agent_timeout_secs, |_| {}).await
}

async fn harness_with_config(
    name: &str,
    is_sub_agent: bool,
    sub_agent_timeout_secs: i32,
    configure: impl FnOnce(&mut Config),
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

    let compaction = Arc::new(CompactionEngine::new(
        store.clone(),
        spaths.summary.clone(),
        spaths.plan.clone(),
        spaths.plan_draft.clone(),
        cwd.clone(),
        home.clone(),
        cfg.skills.clone(),
        crate::config::api_url(&cfg),
        &cfg,
        stats.clone(),
        reqwest::Client::new(),
    ));
    let display = Arc::new(NoopDisplay::new());
    let ctx = Arc::new(AgentSharedContext {
        config: cfg.clone(),
        cwd: cwd.clone(),
        home,
        api_url: crate::config::api_url(&cfg),
        store,
        artifacts,
        snapshots: Arc::new(Mutex::new(
            crate::tools::snapshot::FileSnapshotStore::default(),
        )),
        stats,
        compaction,
        cancel: crate::cancel::CancellationToken::new(),
        display: display.clone(),
        sub_stream_tx: None,
        tool_config: ToolConfig::from_config(&cfg),
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
    llm: Arc<dyn LlmClient>,
    input: &str,
) -> anyhow::Result<()> {
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    let actor = OrchActor::new_with_llm(ctx, rx, llm);
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
    let mut executor = TurnExecutor::new(h.ctx.clone(), llm);
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
    let mut executor = TurnExecutor::new(h.ctx.clone(), llm);
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
    let mut executor = TurnExecutor::new(h.ctx.clone(), llm);
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
    let mut executor = TurnExecutor::new(h.ctx.clone(), llm);
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
    let mut executor = TurnExecutor::new(h.ctx.clone(), llm);
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
    let mut executor = TurnExecutor::new(h.ctx.clone(), llm);
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
    let mut executor = TurnExecutor::new(h.ctx.clone(), llm);
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
    let h = harness_with_config("turn-first-event-timeout", false, 300, |cfg| {
        cfg.llm_first_event_timeout_secs = 1;
        cfg.llm_idle_timeout_secs = 10;
        cfg.llm_wait_heartbeat_secs = 0;
    })
    .await?;
    let mut executor = TurnExecutor::new(h.ctx.clone(), Arc::new(PendingLlmClient));
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
    let h = harness_with_config("turn-idle-timeout", false, 300, |cfg| {
        cfg.llm_first_event_timeout_secs = 10;
        cfg.llm_idle_timeout_secs = 1;
        cfg.llm_wait_heartbeat_secs = 0;
    })
    .await?;
    let mut executor = TurnExecutor::new(h.ctx.clone(), Arc::new(IdleAfterTextLlmClient));
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
    let h = harness_with_config("turn-max-turns", false, 300, |cfg| {
        cfg.max_turns = 1;
    })
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
    let mut executor = TurnExecutor::new(h.ctx.clone(), llm);
    let (decision, effects) = executor.execute("read until exhausted", None).await?;

    assert_eq!(decision, TurnDecision::MaxTurnsExceeded);
    assert!(effects.is_empty());
    assert_eq!(executor.tool_call_count(), 1);
    Ok(())
}

#[tokio::test]
async fn disabled_tool_call_persists_error_result_instead_of_being_dropped() -> anyhow::Result<()> {
    let h = harness_with_config("disabled-tool-result", false, 300, |cfg| {
        cfg.tool_disable.disable_bash = true;
    })
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
    let mut executor = TurnExecutor::new(h.ctx.clone(), llm);
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
            .contains("Bash tool is disabled"),
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
    let mut executor = TurnExecutor::new(h.ctx.clone(), llm);
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
    let mut executor = TurnExecutor::new(h.ctx.clone(), llm);
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
        .record(&h.cwd.join("edit.txt"), "old\n", 1);
    let patch = format!("@edit.txt#{}\nreplace 1:\n+new", snapshot.tag);
    let llm = Arc::new(MockLlmClient::new(
        "flash",
        vec![
            vec![
                Ok(Event::ToolCall(tool_call(
                    "Edit",
                    "call_edit",
                    json!({"path":"edit.txt","patch":patch}),
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
    let mut executor = TurnExecutor::new(h.ctx.clone(), llm);
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
    let mut executor = TurnExecutor::new(h.ctx.clone(), llm);
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
    let mut executor = TurnExecutor::new(h.ctx.clone(), llm);
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
    let mut executor = TurnExecutor::new(h.ctx.clone(), llm);
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
    let mut executor = TurnExecutor::new(h.ctx.clone(), llm);
    let (decision, effects) = executor.execute("unknown", None).await?;
    assert_eq!(decision, TurnDecision::Stop);
    assert!(effects.is_empty());
    Ok(())
}

#[tokio::test]
async fn preflight_compaction_path_runs_when_estimated_context_is_high() -> anyhow::Result<()> {
    let h = harness_with_config("preflight-compact-path", false, 300, |cfg| {
        cfg.max_context_tokens = 1;
        cfg.context_compact_pct = 100;
    })
    .await?;
    let llm = Arc::new(MockLlmClient::new(
        "flash",
        vec![vec![Ok(Event::Stop(StopEvent {
            reason: "end_turn".into(),
        }))]],
    ));
    let mut executor = TurnExecutor::new(h.ctx.clone(), llm);
    let (decision, effects) = executor.execute("large context estimate", None).await?;
    assert_eq!(decision, TurnDecision::Stop);
    assert!(effects.is_empty());
    Ok(())
}

#[tokio::test]
async fn clean_tool_call_with_belief_takes_decision_none_path() -> anyhow::Result<()> {
    let h = harness_with_config("decision-none-path", false, 300, |cfg| {
        cfg.max_turns = 1;
    })
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
    let mut executor = TurnExecutor::new(h.ctx.clone(), llm);
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
    let mut executor = TurnExecutor::new(h.ctx.clone(), llm);
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
    let mut executor = TurnExecutor::new(h.ctx.clone(), llm);
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
    let mut executor = TurnExecutor::new(h.ctx.clone(), llm);
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
            .info
            .lock()
            .unwrap()
            .iter()
            .any(|msg| msg == "error:Unknown model tier: unknown. Use /flash or /pro"),
        "{:?}",
        h.display.info.lock().unwrap()
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
    assert!(info.iter().any(|msg| msg == "切回 flash。"), "{info:?}");
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
async fn plan_confirm_and_clear_apply_file_side_effects_and_invalidate_prefix() -> anyhow::Result<()>
{
    let h = harness("plan-actions").await?;
    tokio::fs::write(&h.ctx.plan_draft_path, "1. ship it\n").await?;
    let prefix = PrefixManager::new(h.ctx.clone());
    let _ = prefix.ensure()?;
    assert!(h.ctx.immutable_prefix.lock().unwrap().is_some());

    let handler = PlanActionHandler::new(h.ctx.clone());
    let mut effects = Vec::new();
    let mut confirm = internal_result("PlanConfirm");
    handler.handle(&mut confirm, &mut effects, &prefix).await;
    assert_eq!(confirm.content, "Plan confirmed and locked in.");
    assert_eq!(
        tokio::fs::read_to_string(&h.ctx.plan_path).await?,
        "1. ship it\n"
    );
    assert_eq!(tokio::fs::read_to_string(&h.ctx.plan_draft_path).await?, "");
    assert!(matches!(effects.as_slice(), [TurnEffect::PlanConfirmed]));
    assert!(h.ctx.immutable_prefix.lock().unwrap().is_none());

    let _ = prefix.ensure()?;
    let mut clear = internal_result("PlanClear");
    handler.handle(&mut clear, &mut effects, &prefix).await;
    assert_eq!(clear.content, "Plan cleared.");
    assert_eq!(tokio::fs::read_to_string(&h.ctx.plan_path).await?, "");
    assert!(matches!(
        effects.as_slice(),
        [TurnEffect::PlanConfirmed, TurnEffect::PlanCleared]
    ));
    assert!(h.ctx.immutable_prefix.lock().unwrap().is_none());
    Ok(())
}

#[tokio::test]
async fn plan_confirm_without_draft_returns_error_result() -> anyhow::Result<()> {
    let h = harness("plan-empty").await?;
    let prefix = PrefixManager::new(h.ctx.clone());
    let _ = prefix.ensure()?;
    let handler = PlanActionHandler::new(h.ctx.clone());
    let mut effects = Vec::new();
    let mut confirm = internal_result("PlanConfirm");
    handler.handle(&mut confirm, &mut effects, &prefix).await;
    assert_eq!(confirm.content, "Error: no plan draft found to confirm.");
    assert!(effects.is_empty());
    assert!(h.ctx.immutable_prefix.lock().unwrap().is_some());
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
    let mut executor = TurnExecutor::new(h.ctx.clone(), llm);
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
    assert!(
        result[0]
            .content
            .contains("write blocked by file safety policy")
    );
    assert!(!outside.exists());
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
    assert!(
        result[0]
            .content
            .contains("write blocked by file safety policy"),
        "{}",
        result[0].content
    );
    assert!(!outside_dir.join("escape.txt").exists());
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
    assert!(
        result[0]
            .content
            .contains("write blocked by file safety policy"),
        "{}",
        result[0].content
    );
    assert_eq!(
        tokio::fs::read_to_string(outside_dir.join("escape.txt")).await?,
        "old"
    );
    Ok(())
}

#[tokio::test]
async fn sub_agent_recursion_is_rejected_without_running_child() -> anyhow::Result<()> {
    let h = harness_with("sub-recursion", true, 300).await?;
    let coordinator = SubAgentCoordinator::new(h.ctx.clone());
    let mut result = internal_result("SubAgent");
    result.spawns_sub_agent = true;
    result.sub_agent_prompt = Some("nested task".into());
    let runner = Arc::new(|_, _, _, _| {
        panic!("runner must not execute when recursion is blocked");
    });
    let processed = coordinator.process_with_runner(vec![result], runner).await;
    assert_eq!(processed.len(), 1);
    assert!(processed[0].content.contains("recursion blocked"));
    Ok(())
}

#[tokio::test]
async fn sub_agent_success_formats_result_and_records_usage() -> anyhow::Result<()> {
    let h = harness_with("sub-success", false, 300).await?;
    let coordinator = SubAgentCoordinator::new(h.ctx.clone());
    let mut result = internal_result("SubAgent");
    result.spawns_sub_agent = true;
    result.sub_agent_prompt = Some("task".into());
    let runner = Arc::new(|_, _, _, _| SubAgentResult {
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
    });
    let processed = coordinator.process_with_runner(vec![result], runner).await;

    assert_eq!(processed.len(), 1);
    assert!(processed[0].content.contains("] ok (in=10, out=5)"));
    assert!(processed[0].content.contains("Thinking: child thought"));
    assert!(processed[0].content.contains("Text: child text"));
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
    let coordinator = SubAgentCoordinator::new(h.ctx.clone());
    let mut result = internal_result("SubAgent");
    result.spawns_sub_agent = true;
    result.sub_agent_prompt = Some("panic task".into());
    let runner = Arc::new(|_, _, _, _| -> SubAgentResult {
        panic!("panic from test runner");
    });
    let processed = coordinator.process_with_runner(vec![result], runner).await;

    assert_eq!(processed.len(), 1);
    assert!(processed[0].content.contains("] failed (in=0, out=0)"));
    assert!(
        processed[0]
            .content
            .contains("Sub-agent thread panicked: panic from test runner"),
        "{}",
        processed[0].content
    );
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
async fn sub_agent_timeout_marks_incomplete() -> anyhow::Result<()> {
    let h = harness_with("sub-timeout", false, 0).await?;
    let coordinator = SubAgentCoordinator::new(h.ctx.clone());
    let mut result = internal_result("SubAgent");
    result.spawns_sub_agent = true;
    result.sub_agent_prompt = Some("slow task".into());
    let runner = Arc::new(|_, _, _, _| {
        std::thread::sleep(Duration::from_millis(50));
        SubAgentResult {
            status: "ok".into(),
            thinking: String::new(),
            text: "late".into(),
            usage: Default::default(),
        }
    });
    let processed = coordinator.process_with_runner(vec![result], runner).await;
    assert_eq!(processed[0].content, "Sub-agent did not complete.");
    Ok(())
}

#[tokio::test]
async fn sub_agent_collection_enters_timeout_even_when_more_than_limit_are_launched()
-> anyhow::Result<()> {
    let h = harness_with("sub-timeout-many", false, 0).await?;
    let coordinator = SubAgentCoordinator::new(h.ctx.clone());
    let mut calls = Vec::new();
    for idx in 0..9 {
        let mut result = internal_result("SubAgent");
        result.spawns_sub_agent = true;
        result.sub_agent_prompt = Some(format!("slow task {idx}"));
        calls.push(result);
    }
    let runner = Arc::new(|_, _, _, _| {
        std::thread::sleep(Duration::from_millis(50));
        SubAgentResult {
            status: "ok".into(),
            thinking: String::new(),
            text: "late".into(),
            usage: Default::default(),
        }
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
            .all(|r| r.content == "Sub-agent did not complete.")
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
            Ok(Event::Stop(StopEvent {
                reason: "end_turn".into(),
            })),
        ]],
    ));
    let executor =
        SubAgentExecutor::new_with_llm(h.ctx.clone(), "sub_mock".into(), true, llm).await?;
    let result = executor.execute("child task".into()).await;
    assert_eq!(result.status, "ok");
    assert_eq!(result.text, "child answer");
    assert!(
        result.thinking.is_empty(),
        "unexpected thinking: {}",
        result.thinking
    );
    assert_eq!(h.ctx.store.lines().await?.len(), 1);
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
    let client = AsyncLlClient::new("deepseek-v4-flash", &api_key, &api_url)?;
    let messages = vec![json!({"role":"user","content":"Reply with one short word: pong"})];
    let mut stream = client
        .stream(
            &h.ctx,
            &messages,
            &[],
            "You are a concise regression smoke test.",
        )
        .await?;
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
        signals: Vec::new(),
    }
}
