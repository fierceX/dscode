use crate::agent::orchestrator::{new_orchestrator, OrchActor};
use crate::cancel::CancellationToken;
use crate::config::api_url;
use crate::context::{AgentSharedContext, ToolConfig};
use crate::runtime::config::{AgentRuntimeConfig, NoopDisplay, SessionInfo, SessionPolicy};
use crate::runtime::events::EventDisplay;
use crate::runtime::handle::AgentRuntime;
use crate::session::compaction::CompactionEngine;
use crate::session::metadata::{SessionSeed, sanitize_alias};
use crate::session::paths;
use crate::tools::snapshot::FileSnapshotStore;
use anyhow::{Result, bail};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;

pub async fn build_runtime(config: AgentRuntimeConfig) -> Result<AgentRuntime> {
    let AgentRuntimeConfig {
        mut config,
        home,
        cwd,
        session,
        first_prompt,
        display,
        event_sink,
        sub_stream_tx,
        #[cfg(test)]
        llm_override,
    } = config;


    let (sid, session_ref, session_alias) = resolve_session(&home, &cwd, session).await?;
    config.session_id = sid.clone();

    let spaths = paths::paths_for(&home, &cwd, &sid);
    let new_session = !spaths.events.exists();

    let (store, stats, artifacts) =
        crate::session::init::init_session_base(&home, &cwd, &sid).await?;
    crate::session::metadata::ensure_metadata(
        &spaths,
        &cwd,
        SessionSeed {
            alias: session_alias,
            title: first_prompt
                .as_deref()
                .and_then(crate::session::metadata::title_from_prompt),
            first_prompt,
        },
    )
    .await?;

    let api_url_str = api_url(&config);
    let shared_client = reqwest::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(30))
        .timeout(std::time::Duration::from_secs(600))
        .user_agent("mink/3.0")
        .build()?;

    let compaction = Arc::new(CompactionEngine::new(
        store.clone(),
        spaths.summary.clone(),
        spaths.plan.clone(),
        spaths.plan_draft.clone(),
        cwd.clone(),
        home.clone(),
        config.skills.clone(),
        api_url_str.clone(),
        &config,
        stats.clone(),
        shared_client,
    ));

    let cancel = CancellationToken::new();
    let mut event_display: Option<Arc<EventDisplay>> = None;
    let display: Arc<dyn crate::ui::Display> = match (event_sink.clone(), display) {
        (Some(sink), Some(delegate)) => {
            let ed = Arc::new(EventDisplay::new(sink, Some(delegate)));
            event_display = Some(ed.clone());
            ed
        }
        (Some(sink), None) => {
            let ed = Arc::new(EventDisplay::new(sink, None));
            event_display = Some(ed.clone());
            ed
        }
        (None, Some(display)) => display,
        (None, None) => Arc::new(NoopDisplay),
    };
    let ctx = Arc::new(AgentSharedContext {
        config: config.clone(),
        cwd: cwd.clone(),
        home: home.clone(),
        api_url: api_url_str,
        store,
        artifacts,
        snapshots: Arc::new(Mutex::new(FileSnapshotStore::default())),
        stats,
        compaction,
        cancel: cancel.clone(),
        display: display.clone(),
        sub_stream_tx,
        tool_config: ToolConfig::from_config(&config),
        events_path: spaths.events.clone(),
        summary_path: spaths.summary.clone(),
        plan_path: spaths.plan.clone(),
        plan_draft_path: spaths.plan_draft.clone(),
        immutable_prefix: Mutex::new(None),
        is_sub_agent: false,
        interrupt: Arc::new(AtomicBool::new(false)),
        event_log_warned: AtomicBool::new(false),
    });

    let (orchestrator, cmd_tx) = {
        #[cfg(test)]
        if let Some(llm) = llm_override {
            let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
            let actor = OrchActor::new_with_llm(ctx.clone(), cmd_rx, llm);
            (actor, cmd_tx)
        } else {
            new_orchestrator(ctx.clone())
        }
        #[cfg(not(test))]
        {
            new_orchestrator(ctx.clone())
        }
    };
    let orch_display = display.clone();
    let orch_handle = tokio::spawn(async move {
        if let Err(e) = orchestrator.run().await {
            orch_display.render_error(&format!("Orchestrator: {e}"));
        }
    });

    if new_session {
        ctx.log_event(serde_json::json!({"type":"session_start","session_id":sid}));
    }

    let session_info = SessionInfo::new(sid, session_ref, new_session, home, cwd, &spaths);
    Ok(AgentRuntime {
        ctx,
        cmd_tx,
        orch_handle,
        session: session_info,
        event_sink,
        event_display,
        stream_in_progress: Arc::new(AtomicBool::new(false)),
    })
}

async fn resolve_session(
    home: &std::path::Path,
    cwd: &std::path::Path,
    policy: SessionPolicy,
) -> Result<(String, String, Option<String>)> {
    match policy {
        SessionPolicy::New => {
            let sid = paths::chrono_session_id();
            Ok((sid.clone(), sid, None))
        }
        SessionPolicy::ContinueLatest => {
            let sid = paths::continue_session(home, cwd).await.unwrap_or_default();
            if sid.is_empty() {
                let sid = paths::chrono_session_id();
                Ok((sid.clone(), sid, None))
            } else {
                Ok((sid.clone(), sid, None))
            }
        }
        SessionPolicy::Resume(reference) => {
            let trimmed = reference.trim();
            if trimmed.is_empty() {
                bail!("invalid empty session reference");
            }
            if let Some(resolved) =
                crate::session::metadata::resolve_session_reference(home, cwd, trimmed).await?
            {
                Ok((resolved, trimmed.to_string(), None))
            } else {
                bail!("session not found: {trimmed}");
            }
        }
        SessionPolicy::UseOrCreate(reference) => {
            let trimmed = reference.trim();
            if trimmed.is_empty() {
                let sid = paths::chrono_session_id();
                return Ok((sid.clone(), sid, None));
            }
            if let Some(resolved) =
                crate::session::metadata::resolve_session_reference(home, cwd, trimmed).await?
            {
                Ok((resolved, trimmed.to_string(), None))
            } else {
                let alias = sanitize_alias(trimmed);
                if alias.is_none() {
                    bail!("invalid session name: {trimmed}");
                }
                Ok((paths::chrono_session_id(), trimmed.to_string(), alias))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::runtime::{AgentEvent, EventSink};
    use std::sync::Mutex;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn unique_temp_dir(name: &str) -> std::path::PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        std::env::temp_dir().join(format!("mink-runtime-{name}-{nanos}"))
    }

    #[tokio::test]
    async fn build_runtime_initializes_session_paths() {
        let home = unique_temp_dir("home");
        let cwd = unique_temp_dir("cwd");
        tokio::fs::create_dir_all(&cwd).await.unwrap();
        let cfg = Config {
            log_events: true,
            ..Config::default()
        };
        let runtime = build_runtime(AgentRuntimeConfig::from_config(
            cfg,
            home.clone(),
            cwd.clone(),
        ))
        .await
        .unwrap();
        let session = runtime.session_info().clone();

        assert!(!session.session_id.is_empty());
        assert!(session.is_new);
        assert_eq!(session.home, home);
        assert_eq!(session.cwd, cwd);
        assert!(session.events_path.exists());
        assert!(
            std::fs::read_to_string(&session.events_path)
                .unwrap()
                .contains("\"session_start\"")
        );

        runtime.shutdown().await.unwrap();
        let _ = tokio::fs::remove_dir_all(session.home).await;
        let _ = tokio::fs::remove_dir_all(session.cwd).await;
    }

    #[derive(Default)]
    struct RecordingSink {
        events: Mutex<Vec<AgentEvent>>,
    }

    impl EventSink for RecordingSink {
        fn on_event(&self, event: AgentEvent) {
            self.events.lock().unwrap().push(event);
        }
    }

    #[tokio::test]
    async fn runtime_event_sink_observes_existing_display_path() {
        let home = unique_temp_dir("event-home");
        let cwd = unique_temp_dir("event-cwd");
        tokio::fs::create_dir_all(&cwd).await.unwrap();
        let sink = Arc::new(RecordingSink::default());
        let cfg = Config {
            log_events: true,
            ..Config::default()
        };
        let runtime = build_runtime(
            AgentRuntimeConfig::from_config(cfg, home.clone(), cwd.clone())
                .with_event_sink(sink.clone()),
        )
        .await
        .unwrap();

        runtime.compact().await.unwrap();
        runtime.shutdown().await.unwrap();

        let events = sink.events.lock().unwrap();
        assert!(events.iter().any(|event| matches!(
            event,
            AgentEvent::Info { message } if message == "Compressing..."
        )));
        assert!(events.iter().any(|event| matches!(event, AgentEvent::Stop)));

        let _ = tokio::fs::remove_dir_all(home).await;
        let _ = tokio::fs::remove_dir_all(cwd).await;
    }

    // ── Mock LLM runtime integration tests ──────────────────────────

    /// Build a minimal mock LLM that returns Text + Stop for each call.
    fn mock_llm_hello() -> crate::llm::mock::MockLlmClient {
        use crate::protocol::{Event, StopEvent, TextEvent};
        crate::llm::mock::MockLlmClient::new(
            "flash",
            vec![
                vec![
                    Ok(Event::Text(TextEvent {
                        content: "Hello, world!".into(),
                    })),
                    Ok(Event::Stop(StopEvent {
                        reason: "end_turn".into(),
                    })),
                ],
                vec![
                    Ok(Event::Text(TextEvent {
                        content: "second".into(),
                    })),
                    Ok(Event::Stop(StopEvent {
                        reason: "end_turn".into(),
                    })),
                ],
                vec![
                    Ok(Event::Text(TextEvent {
                        content: "third".into(),
                    })),
                    Ok(Event::Stop(StopEvent {
                        reason: "end_turn".into(),
                    })),
                ],
            ],
        )
    }

    fn runtime_config_with_mock(
        home: &std::path::Path,
        cwd: &std::path::Path,
        mock: crate::llm::mock::MockLlmClient,
    ) -> AgentRuntimeConfig {
        let cfg = Config {
            model: "flash".into(),
            api_key: "test-key".into(),
            base_url: "https://example.invalid/v1".into(),
            max_context_tokens: 1_000_000,
            log_events: true,
            ..Config::default()
        };
        let mut rt_config =
            AgentRuntimeConfig::from_config(cfg, home.to_path_buf(), cwd.to_path_buf());
        rt_config.llm_override = Some(Arc::new(mock));
        rt_config
    }

    #[tokio::test]
    async fn run_turn_with_mock_llm_returns_ok_outcome() {
        let home = unique_temp_dir("mock-hello-home");
        let cwd = unique_temp_dir("mock-hello-cwd");
        tokio::fs::create_dir_all(&cwd).await.unwrap();

        let runtime = build_runtime(runtime_config_with_mock(&home, &cwd, mock_llm_hello()))
            .await
            .unwrap();

        let outcome = runtime.run_turn("say hello").await.unwrap();
        assert_eq!(outcome.status, crate::agent::orchestrator::TurnStatus::Ok);
        assert_eq!(outcome.tool_call_count, 0);
        assert_eq!(outcome.tool_error_count, 0);
        assert!(outcome.error.is_none());

        runtime.shutdown().await.unwrap();
        let _ = tokio::fs::remove_dir_all(home).await;
        let _ = tokio::fs::remove_dir_all(cwd).await;
    }

    #[tokio::test]
    async fn run_turn_with_mock_llm_emits_text_and_final_events() {
        let home = unique_temp_dir("mock-events-home");
        let cwd = unique_temp_dir("mock-events-cwd");
        tokio::fs::create_dir_all(&cwd).await.unwrap();
        let sink = Arc::new(RecordingSink::default());

        let mut rt_config = runtime_config_with_mock(&home, &cwd, mock_llm_hello());
        rt_config.event_sink = Some(sink.clone());

        let runtime = build_runtime(rt_config).await.unwrap();
        runtime.run_turn("say hello").await.unwrap();
        runtime.shutdown().await.unwrap();

        let events = sink.events.lock().unwrap();
        assert!(
            events.iter().any(|event| matches!(
                event,
                AgentEvent::Text { content } if content == "Hello, world!"
            )),
            "expected Text event with greeting"
        );
        assert!(
            events.iter().any(|event| matches!(
                event,
                AgentEvent::Final { outcome } if outcome.status == crate::agent::orchestrator::TurnStatus::Ok
            )),
            "expected Final event with Ok status"
        );

        let _ = tokio::fs::remove_dir_all(home).await;
        let _ = tokio::fs::remove_dir_all(cwd).await;
    }

    /// Run three consecutive turns with a mock LLM to verify the runtime
    /// can handle successive completions without state corruption.
    #[tokio::test]
    async fn consecutive_turns_with_mock_llm_all_succeed() {
        let home = unique_temp_dir("mock-consecutive-home");
        let cwd = unique_temp_dir("mock-consecutive-cwd");
        tokio::fs::create_dir_all(&cwd).await.unwrap();

        let runtime = build_runtime(runtime_config_with_mock(&home, &cwd, mock_llm_hello()))
            .await
            .unwrap();

        for msg in ["first", "second", "third"] {
            let outcome = runtime.run_turn(msg).await.unwrap();
            assert_eq!(outcome.status, crate::agent::orchestrator::TurnStatus::Ok);
            assert!(outcome.error.is_none());
        }

        runtime.shutdown().await.unwrap();
        let _ = tokio::fs::remove_dir_all(home).await;
        let _ = tokio::fs::remove_dir_all(cwd).await;
    }

    // ── Interrupt test with blocking mock LLM ──────────────────────

    /// A mock LLM whose stream never yields, used to test interrupt.
    /// A mock LLM whose first `stream()` call returns a never-yielding
    /// stream (for testing interrupt), and subsequent calls return a normal
    /// Text+Stop (for testing recovery).
    struct InterruptTestMockLlmClient {
        calls: std::sync::Mutex<u32>,
    }

    #[async_trait::async_trait]
    impl crate::llm::client::LlmClient for InterruptTestMockLlmClient {
        fn model(&self) -> &str {
            "flash"
        }
        async fn stream(
            &self,
            _ctx: &crate::context::AgentSharedContext,
            _messages_json: &[serde_json::Value],
            _tools_json: &[serde_json::Value],
            _system_prompt: &str,
        ) -> anyhow::Result<
            Box<dyn futures::Stream<Item = anyhow::Result<crate::protocol::Event>> + Unpin + Send>,
        > {
            let mut c = self.calls.lock().unwrap();
            *c += 1;
            if *c == 1 {
                Ok(Box::new(futures::stream::pending()))
            } else {
                use crate::protocol::{Event, StopEvent, TextEvent};
                Ok(Box::new(futures::stream::iter(vec![
                    Ok(Event::Text(TextEvent {
                        content: "recovered".into(),
                    })),
                    Ok(Event::Stop(StopEvent {
                        reason: "end_turn".into(),
                    })),
                ])))
            }
        }
    }

    fn runtime_config_with_blocking_mock(
        home: &std::path::Path,
        cwd: &std::path::Path,
    ) -> AgentRuntimeConfig {
        let cfg = Config {
            model: "flash".into(),
            api_key: "test-key".into(),
            base_url: "https://example.invalid/v1".into(),
            max_context_tokens: 1_000_000,
            log_events: true,
            ..Config::default()
        };
        let mut rt_config =
            AgentRuntimeConfig::from_config(cfg, home.to_path_buf(), cwd.to_path_buf());
        rt_config.llm_override = Some(Arc::new(InterruptTestMockLlmClient {
            calls: std::sync::Mutex::new(0),
        }));
        rt_config
    }

    #[tokio::test]
    async fn interrupt_mid_turn_returns_interrupted_and_next_turn_works() {
        let home = unique_temp_dir("mock-int-home");
        let cwd = unique_temp_dir("mock-int-cwd");
        tokio::fs::create_dir_all(&cwd).await.unwrap();

        let runtime = build_runtime(runtime_config_with_blocking_mock(&home, &cwd))
            .await
            .unwrap();

        // Snapshot the shared interrupt flag before moving the runtime.
        let interrupt_flag = runtime.interrupt_flag();

        // Wrap runtime so the spawned task can take ownership and return it.
        let turn_rt = std::sync::Arc::new(tokio::sync::Mutex::new(Some(runtime)));
        let turn_rt_clone = turn_rt.clone();

        let handle = tokio::spawn(async move {
            let runtime = turn_rt_clone.lock().await.take().unwrap();
            let outcome = runtime.run_turn("blocking turn").await;
            (runtime, outcome)
        });

        // Let the orchestrator enter the LLM stream loop.
        tokio::time::sleep(tokio::time::Duration::from_millis(200)).await;

        // The orchestrator's turn executor polls this flag every 25 ms.
        interrupt_flag.store(true, std::sync::atomic::Ordering::SeqCst);

        let (runtime, outcome) = handle.await.unwrap();
        let outcome = outcome.unwrap();
        assert_eq!(
            outcome.status,
            crate::agent::orchestrator::TurnStatus::Interrupted
        );

        // Next turn must still run successfully.
        let outcome2 = runtime.run_turn("recovery turn").await.unwrap();
        assert_eq!(
            outcome2.status,
            crate::agent::orchestrator::TurnStatus::Ok
        );

        runtime.shutdown().await.unwrap();
        let _ = tokio::fs::remove_dir_all(home).await;
        let _ = tokio::fs::remove_dir_all(cwd).await;
    }

    // ── Mock LLM with tool-use test ───────────────────────────────

    /// Mock LLM that exercises the full tool-execution pipeline:
    /// first turn returns a Bash tool call, second turn returns Text+Stop.
    fn mock_llm_tool_use() -> crate::llm::mock::MockLlmClient {
        use crate::protocol::{Event, StopEvent, TextEvent, ToolCallEvent};
        use serde_json::json;
        let mut fields = std::collections::BTreeMap::new();
        fields.insert("command".into(), "echo hello".into());
        crate::llm::mock::MockLlmClient::new(
            "flash",
            vec![
                // First LLM call: request a Bash tool execution
                vec![
                    Ok(Event::ToolCall(ToolCallEvent {
                        name: "Bash".into(),
                        id: "call_bash_1".into(),
                        input_json: json!({"command": "echo hello"}),
                        fields,
                        order: vec!["command".into()],
                    })),
                    Ok(Event::Stop(StopEvent {
                        reason: "tool_use".into(),
                    })),
                ],
                // Second LLM call: text response after tool execution
                vec![
                    Ok(Event::Text(TextEvent {
                        content: "all done".into(),
                    })),
                    Ok(Event::Stop(StopEvent {
                        reason: "end_turn".into(),
                    })),
                ],
            ],
        )
    }

    #[tokio::test]
    async fn tool_use_turn_executes_tool_and_returns_outcome() {
        let home = unique_temp_dir("mock-tool-home");
        let cwd = unique_temp_dir("mock-tool-cwd");
        tokio::fs::create_dir_all(&cwd).await.unwrap();

        let runtime = build_runtime(runtime_config_with_mock(&home, &cwd, mock_llm_tool_use()))
            .await
            .unwrap();

        let outcome = runtime.run_turn("run echo hello").await.unwrap();
        assert_eq!(outcome.status, crate::agent::orchestrator::TurnStatus::Ok);
        // At least one tool was executed
        assert!(
            outcome.tool_call_count >= 1,
            "expected at least 1 tool call, got {}",
            outcome.tool_call_count
        );
        // The LLM response after tool execution
        assert!(outcome.text.contains("all done"));

        runtime.shutdown().await.unwrap();
        let _ = tokio::fs::remove_dir_all(home).await;
        let _ = tokio::fs::remove_dir_all(cwd).await;
    }

    #[tokio::test]
    async fn tool_use_turn_emits_tool_call_and_tool_result_events() {
        let home = unique_temp_dir("mock-tool-ev-home");
        let cwd = unique_temp_dir("mock-tool-ev-cwd");
        tokio::fs::create_dir_all(&cwd).await.unwrap();
        let sink = Arc::new(RecordingSink::default());

        let mut rt_config = runtime_config_with_mock(&home, &cwd, mock_llm_tool_use());
        rt_config.event_sink = Some(sink.clone());

        let runtime = build_runtime(rt_config).await.unwrap();
        runtime.run_turn("run echo hello").await.unwrap();
        runtime.shutdown().await.unwrap();

        let events = sink.events.lock().unwrap();
        let has_tool_call = events.iter().any(|e| matches!(e, AgentEvent::ToolCall { .. }));
        let has_tool_result = events
            .iter()
            .any(|e| matches!(e, AgentEvent::ToolResult { .. }));
        let has_final = events
            .iter()
            .any(|e| matches!(e, AgentEvent::Final { .. }));

        assert!(has_tool_call, "expected ToolCall event");
        assert!(has_tool_result, "expected ToolResult event");
        assert!(has_final, "expected Final event");

        let _ = tokio::fs::remove_dir_all(home).await;
        let _ = tokio::fs::remove_dir_all(cwd).await;
    }

}

