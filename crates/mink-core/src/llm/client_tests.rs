use super::*;
use crate::cancel::CancellationToken;
use crate::config::{OutputFormat, ResolvedConfig as Config};
use crate::context::{AgentSharedContext, ToolConfig};
use crate::session::compaction::CompactionEngine;
use crate::session::paths;
use crate::session::stats::StatsTracker;
use crate::session::store::ConversationStore;
use crate::ui::{Display, StatsSnapshot};
use serde_json::json;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

struct TestDisplay {
    messages: Mutex<Vec<String>>,
}

impl TestDisplay {
    fn new() -> Self {
        Self {
            messages: Mutex::new(Vec::new()),
        }
    }
}

impl Display for TestDisplay {
    fn render_thinking(&self, _content: &str) {}
    fn render_text(&self, _content: &str) {}
    fn render_tool_call(&self, _call: &crate::ui::ToolCallDisplay<'_>) {}
    fn render_tool_result(&self, _result: &crate::ui::PresentedToolResultDisplay<'_>) {}
    fn render_stop(&self, _reason: &str) {}
    fn render_signal(&self, _kind: &str, _severity: f64, _message: &str) {}
    fn render_error(&self, message: &str) {
        self.messages
            .lock()
            .unwrap()
            .push(format!("error:{message}"));
    }
    fn render_retry(&self) {}
    fn render_info(&self, msg: &str) {
        self.messages.lock().unwrap().push(msg.to_string());
    }
    fn render_title_update(&self, _model: &str, _stats: &StatsSnapshot) {}
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

#[tokio::test]
#[ignore = "requires local loopback sockets"]
async fn send_with_retry_retries_429_and_preserves_authorization() -> anyhow::Result<()> {
    let responses = vec![
        http_response(429, &[("retry-after", "0")], "rate limited"),
        http_response(200, &[], "ok"),
    ];
    let (api_url, seen, _server) = start_http_server(responses).await?;
    let ctx = test_context("client-retry", &api_url).await?;
    let client = AsyncLlClient::new("secret-key", &api_url)?;

    let (resp, _) = client
        .send_with_retry(
            ctx.display.as_ref(),
            br#"{"ping":true}"#.to_vec(),
            &ctx.cancel,
        )
        .await
        .map_err(|failure| failure.error)?;
    assert_eq!(resp.status().as_u16(), 200);
    assert_eq!(seen.load(Ordering::SeqCst), 2);
    Ok(())
}

#[tokio::test]
#[ignore = "requires local loopback sockets"]
async fn send_with_retry_does_not_retry_non_retryable_400() -> anyhow::Result<()> {
    let responses = vec![http_response(400, &[], "bad request")];
    let (api_url, seen, _server) = start_http_server(responses).await?;
    let ctx = test_context("client-400", &api_url).await?;
    let client = AsyncLlClient::new("secret-key", &api_url)?;

    let err = client
        .send_with_retry(
            ctx.display.as_ref(),
            br#"{"ping":true}"#.to_vec(),
            &ctx.cancel,
        )
        .await
        .unwrap_err()
        .error
        .to_string();
    assert!(err.contains("HTTP 400"), "{err}");
    assert_eq!(seen.load(Ordering::SeqCst), 1);
    Ok(())
}

#[tokio::test]
#[ignore = "requires local loopback sockets"]
async fn retry_after_is_capped() -> anyhow::Result<()> {
    let responses = vec![
        http_response(429, &[("retry-after", "10000")], "rate limited"),
        http_response(429, &[("retry-after", "10000")], "rate limited"),
        http_response(429, &[("retry-after", "10000")], "rate limited"),
    ];
    let (api_url, seen, _server) = start_http_server(responses).await?;
    let ctx = test_context("client-retry-cap", &api_url).await?;
    let client = AsyncLlClient::new("secret-key", &api_url)?;

    let start = std::time::Instant::now();
    let err = client
        .send_with_retry(
            ctx.display.as_ref(),
            br#"{"ping":true}"#.to_vec(),
            &ctx.cancel,
        )
        .await
        .unwrap_err()
        .error
        .to_string();
    // Uncapped, retry-after 10000 would park each attempt for hours;
    // capped at 10s the failure arrives after ~2 sleeps + the 20s budget.
    assert!(err.contains("HTTP 429"), "{err}");
    assert!(start.elapsed() < std::time::Duration::from_secs(30));
    assert_eq!(seen.load(Ordering::SeqCst), 3);
    Ok(())
}

#[tokio::test]
#[ignore = "requires local loopback sockets"]
async fn send_is_cancellable() -> anyhow::Result<()> {
    let responses = vec![http_response(200, &[], "ok")];
    let (api_url, _seen, _server) = start_http_server(responses).await?;
    let ctx = test_context("client-cancel", &api_url).await?;
    let client = AsyncLlClient::new("secret-key", &api_url)?;

    let cancel = CancellationToken::new();
    cancel.cancel();
    let ctx_clone = ctx.clone();
    let (tx, rx) = tokio::sync::oneshot::channel();
    tokio::spawn(async move {
        // Bypass ctx.cancel (not cancelled) to exercise the parameter wiring.
        let result = client
            .send_with_retry(
                ctx_clone.display.as_ref(),
                br#"{"ping":true}"#.to_vec(),
                &cancel,
            )
            .await;
        let _ = tx.send(result);
    });
    let result = tokio::time::timeout(std::time::Duration::from_secs(2), rx).await?;
    match result {
        Ok(Err(failure)) => {
            assert_eq!(failure.error.to_string(), "request cancelled");
        }
        Ok(Ok(_)) => panic!("send unexpectedly succeeded"),
        Err(e) => panic!("task join failed: {e}"),
    }
    Ok(())
}
#[tokio::test]
#[ignore = "requires local loopback sockets"]
async fn stream_parses_sse_text_usage_and_stop() -> anyhow::Result<()> {
    let body = format!(
        "data: {}\n\ndata: {}\n\ndata: [DONE]\n\n",
        json!({"choices":[{"delta":{"content":"pong"}}]}),
        json!({"choices":[{"finish_reason":"stop","delta":{}}],"usage":{"prompt_tokens":7,"completion_tokens":2,"prompt_tokens_details":{"cached_tokens":3}}})
    );
    let responses = vec![http_response(
        200,
        &[("content-type", "text/event-stream")],
        &body,
    )];
    let (api_url, seen, _server) = start_http_server(responses).await?;
    let ctx = test_context("client-stream", &api_url).await?;
    let response = OpenAiCompatibleBackend::deepseek_defaults()
        .stream(LlmRequest {
            purpose: LlmPurpose::Agent,
            model: "deepseek-v4-flash".into(),
            model_alias: Some("flash".into()),
            api_url,
            api_key: "secret-key".into(),
            system_prompt: "system".into(),
            messages: vec![json!({"role":"user","content":"ping"})],
            tools: Vec::new(),
            max_tokens: ctx.max_tokens(),
            cancel: ctx.cancel.clone(),
            verbose: ctx.verbose(),
            display: ctx.display.clone(),
        })
        .await?;
    let mut stream = response.events;

    let mut text = String::new();
    let mut usage = None;
    let mut stop = None;
    while let Some(event) = stream.next().await {
        match event? {
            Event::Text(t) => text.push_str(&t.content),
            Event::Usage(u) => usage = Some(u),
            Event::Stop(s) => {
                stop = Some(s.reason);
                break;
            }
            _ => {}
        }
    }
    let usage = usage.expect("usage event");
    assert_eq!(text, "pong");
    assert_eq!(usage.input_tokens, 4);
    assert_eq!(usage.cache_read_input_tokens, 3);
    assert_eq!(usage.output_tokens, 2);
    assert_eq!(stop.as_deref(), Some("stop"));
    assert_eq!(seen.load(Ordering::SeqCst), 1);
    Ok(())
}

struct FailingBackend;

#[async_trait::async_trait]
impl LlmBackend for FailingBackend {
    fn name(&self) -> &str {
        "failing"
    }

    async fn stream(&self, _request: LlmRequest) -> Result<LlmResponseStream> {
        anyhow::bail!("backend unavailable")
    }
}

struct AttemptFailingBackend;

#[async_trait::async_trait]
impl LlmBackend for AttemptFailingBackend {
    fn name(&self) -> &str {
        "attempt-failing"
    }

    async fn stream(&self, _request: LlmRequest) -> Result<LlmResponseStream> {
        Err(LlmRequestFailure {
            attempt_count: 3,
            error: anyhow::anyhow!("transport unavailable"),
        }
        .into())
    }
}

#[tokio::test]
async fn backend_request_records_unreported_usage_when_request_fails() -> anyhow::Result<()> {
    let ctx = test_context("backend-request-failed", "https://example.invalid/v1").await?;
    let result = stream_backend(
        &(Arc::new(FailingBackend) as Arc<dyn LlmBackend>),
        &ctx,
        "custom-model",
        None,
        &[json!({"role":"user","content":"ping"})],
        &[],
        "system",
    )
    .await;
    let err = match result {
        Ok(_) => anyhow::bail!("expected backend failure"),
        Err(error) => error.to_string(),
    };
    assert!(err.contains("backend unavailable"), "{err}");
    let records = ctx.usage.all_records()?;
    assert_eq!(records.len(), 1);
    assert_eq!(
        records[0].status,
        crate::session::usage::UsageStatus::Unreported
    );
    assert_eq!(records[0].kind, crate::session::usage::UsageKind::Agent);
    assert_eq!(records[0].model, "custom-model");
    assert_eq!(records[0].attempt_count, 1);
    assert!(
        records[0]
            .reason
            .as_deref()
            .is_some_and(|reason| reason.contains("request_failed: backend unavailable")),
        "{:?}",
        records[0].reason
    );
    Ok(())
}

#[tokio::test]
async fn backend_request_preserves_request_failure_attempt_count() -> anyhow::Result<()> {
    let ctx = test_context("backend-attempt-failed", "https://example.invalid/v1").await?;
    let result = stream_backend(
        &(Arc::new(AttemptFailingBackend) as Arc<dyn LlmBackend>),
        &ctx,
        "custom-model",
        None,
        &[json!({"role":"user","content":"ping"})],
        &[],
        "system",
    )
    .await;
    let err = match result {
        Ok(_) => anyhow::bail!("expected backend failure"),
        Err(error) => error.to_string(),
    };
    assert!(err.contains("transport unavailable"), "{err}");
    let records = ctx.usage.all_records()?;
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].attempt_count, 3);
    assert!(
        records[0]
            .reason
            .as_deref()
            .is_some_and(|reason| reason.contains("request_failed: transport unavailable")),
        "{:?}",
        records[0].reason
    );
    Ok(())
}

async fn test_context(name: &str, api_url: &str) -> anyhow::Result<Arc<AgentSharedContext>> {
    static CNT: AtomicU64 = AtomicU64::new(0);
    let n = CNT.fetch_add(1, Ordering::SeqCst);
    let root = std::env::temp_dir().join(format!(
        "mink-client-test-{}-{name}-{n}",
        std::process::id()
    ));
    let home = root.join("home");
    let cwd = root.join("workspace");
    tokio::fs::create_dir_all(&home).await?;
    tokio::fs::create_dir_all(&cwd).await?;
    let spaths = paths::paths_for(&home, &cwd, "client");
    let store = Arc::new(ConversationStore::new(spaths.conversation.clone()));
    store.ensure().await?;
    let stats = StatsTracker::load(&spaths.stats).await?;
    let usage = crate::session::usage::UsageJournal::new(spaths.usage.clone());
    let artifacts = Arc::new(crate::session::artifacts::ArtifactManager::new(
        spaths.artifacts.clone(),
    ));
    artifacts.ensure()?;
    let cfg = Config {
        model: "flash".into(),
        api_key: "secret-key".into(),
        base_url: api_url.to_string(),
        output_format: OutputFormat::Human,
        ..Default::default()
    };
    let capability_snapshot = Arc::new(crate::capabilities::CapabilitySnapshot::load_default(
        &cwd,
        &home,
        &cfg.skills,
    )?);
    let llm_backend = Arc::new(OpenAiCompatibleBackend::deepseek_defaults());
    let compaction = Arc::new(CompactionEngine::new(
        store.clone(),
        spaths.summary.clone(),
        api_url.to_string(),
        &cfg,
        stats.clone(),
        usage.clone(),
        "client".into(),
        Arc::new(TestDisplay::new()),
        CancellationToken::new(),
        Arc::new(AtomicBool::new(false)),
        llm_backend.clone(),
        None,
    )?);
    let tool_config = ToolConfig::from_config(&cfg);
    let todo_store = Arc::new(crate::session::todo::TodoStore::load(spaths.todos.clone())?);
    let (tool_resolution_context, tool_surface, tool_capabilities) =
        crate::context::resolve_tool_runtime(&tool_config, false, false, &[])?;
    Ok(Arc::new(AgentSharedContext {
        config: cfg.clone(),
        cwd: cwd.clone(),
        home,
        session_layout: crate::session::paths::SessionLayout::ProjectScoped,
        api_url: api_url.to_string(),
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
        cancel: CancellationToken::new(),
        display: Arc::new(TestDisplay::new()),
        sub_stream_tx: None,
        read_only_fs: None,
        vfs_scope: crate::tools::vfs::VfsScope {
            resource_session_id: "client".into(),
            agent_session_id: "client".into(),
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
            crate::capabilities::model_capabilities::SessionModelCapabilities::unsupported("test"),
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
        is_sub_agent: false,
        interrupt: Arc::new(AtomicBool::new(false)),
        event_log_warned: AtomicBool::new(false),
        event_log_writer: None,
        stream_flush_last: Mutex::new(None),
    }))
}

fn http_response(status: u16, headers: &[(&str, &str)], body: &str) -> String {
    let reason = match status {
        200 => "OK",
        400 => "Bad Request",
        429 => "Too Many Requests",
        _ => "Status",
    };
    let mut response = format!(
        "HTTP/1.1 {status} {reason}\r\ncontent-length: {}\r\n",
        body.len()
    );
    for (key, value) in headers {
        response.push_str(&format!("{key}: {value}\r\n"));
    }
    response.push_str("connection: close\r\n\r\n");
    response.push_str(body);
    response
}

async fn start_http_server(
    responses: Vec<String>,
) -> anyhow::Result<(String, Arc<AtomicUsize>, tokio::task::JoinHandle<()>)> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let addr = listener.local_addr()?;
    let seen = Arc::new(AtomicUsize::new(0));
    let seen_server = seen.clone();
    let responses = Arc::new(Mutex::new(responses.into_iter()));
    let handle = tokio::spawn(async move {
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                return;
            };
            let idx = seen_server.fetch_add(1, Ordering::SeqCst);
            let mut buf = vec![0u8; 8192];
            let Ok(n) = socket.read(&mut buf).await else {
                return;
            };
            let request = String::from_utf8_lossy(&buf[..n]);
            assert!(request.contains("POST /chat/completions HTTP/1.1"));
            assert!(
                request
                    .to_ascii_lowercase()
                    .contains("authorization: bearer secret-key"),
                "{request}"
            );
            assert!(
                request
                    .to_ascii_lowercase()
                    .contains(&format!("user-agent: mink/{}", env!("CARGO_PKG_VERSION"))),
                "{request}"
            );
            let response = {
                let mut responses = responses.lock().unwrap();
                responses
                    .next()
                    .unwrap_or_else(|| panic!("missing mock response for request {idx}"))
            };
            let _ = socket.write_all(response.as_bytes()).await;
        }
    });
    Ok((format!("http://{addr}/chat/completions"), seen, handle))
}

#[tokio::test]
async fn stream_eof_residual_frame_without_trailing_newline_is_parsed() -> anyhow::Result<()> {
    // 连接在最后一帧后被切断：帧 JSON 完整但没有尾部换行、也没有 [DONE]。
    // 传输层 EOF 时必须把缓冲残留送入 parser，否则 finish_reason/usage 丢失。
    let body = format!(
        "data: {}\n\ndata: {}",
        json!({"choices":[{"delta":{"content":"tail"}}]}),
        json!({"choices":[{"finish_reason":"stop","delta":{}}],"usage":{"prompt_tokens":5,"completion_tokens":1}})
    );
    let responses = vec![http_response(
        200,
        &[("content-type", "text/event-stream")],
        &body,
    )];
    let (api_url, _seen, _server) = start_http_server(responses).await?;
    let ctx = test_context("client-eof-residual", &api_url).await?;
    let response = OpenAiCompatibleBackend::deepseek_defaults()
        .stream(LlmRequest {
            purpose: LlmPurpose::Agent,
            model: "deepseek-v4-flash".into(),
            model_alias: Some("flash".into()),
            api_url,
            api_key: "secret-key".into(),
            system_prompt: "system".into(),
            messages: vec![json!({"role":"user","content":"ping"})],
            tools: Vec::new(),
            max_tokens: ctx.max_tokens(),
            cancel: ctx.cancel.clone(),
            verbose: ctx.verbose(),
            display: ctx.display.clone(),
        })
        .await?;
    let mut stream = response.events;

    let mut text = String::new();
    let mut stop = None;
    while let Some(event) = stream.next().await {
        match event? {
            Event::Text(t) => text.push_str(&t.content),
            Event::Stop(s) => {
                stop = Some(s.reason);
                break;
            }
            _ => {}
        }
    }
    assert_eq!(text, "tail");
    assert_eq!(stop.as_deref(), Some("stop"));
    Ok(())
}
