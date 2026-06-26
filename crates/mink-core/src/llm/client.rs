use crate::context::AgentSharedContext;
use crate::protocol::Event;
use crate::session::usage::{UsageCapture, UsageKind};
use crate::sse::openai::OpenAIParser;
use anyhow::Result;
use futures::StreamExt;
use std::time::Duration;
use tokio::sync::mpsc;

pub struct LlmRequestContext {
    pub max_tokens: i32,
    pub verbose: bool,
    pub cancel: crate::cancel::CancellationToken,
    pub display: std::sync::Arc<dyn crate::ui::Display>,
    pub usage: std::sync::Arc<crate::session::usage::UsageJournal>,
    pub usage_scope: crate::session::usage::UsageScope,
}

impl LlmRequestContext {
    pub fn from_agent(ctx: &AgentSharedContext) -> Self {
        Self {
            max_tokens: ctx.max_tokens(),
            verbose: ctx.verbose(),
            cancel: ctx.cancel.clone(),
            display: ctx.display.clone(),
            usage: ctx.usage.clone(),
            usage_scope: ctx.usage_scope(if ctx.is_sub_agent {
                UsageKind::SubAgent
            } else {
                UsageKind::Agent
            }),
        }
    }
}

const MAX_RETRIES: u32 = 2;
const RETRY_DELAY: Duration = Duration::from_secs(1);
const RETRY_MAX_TIME: Duration = Duration::from_secs(20);

const MAX_STREAM_ERRORS: u32 = 5;

#[async_trait::async_trait]
pub trait LlmClient: Send + Sync {
    fn model(&self) -> &str;
    async fn stream(
        &self,
        ctx: &AgentSharedContext,
        messages_json: &[serde_json::Value],
        tools_json: &[serde_json::Value],
        system_prompt: &str,
    ) -> Result<Box<dyn futures::Stream<Item = Result<Event>> + Unpin + Send>>;
}

pub struct AsyncLlClient {
    client: reqwest::Client,
    model_name: String,
    api_url: String,
    api_key: String,
}

pub(crate) struct SendFailure {
    pub(crate) error: anyhow::Error,
    pub(crate) attempt_count: u32,
}

impl AsyncLlClient {
    pub fn new(model: &str, api_key: &str, api_url: &str) -> Result<Self> {
        let client = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(30))
            .timeout(Duration::from_secs(600))
            .user_agent("mink/3.0")
            .build()?;
        Ok(Self {
            client,
            model_name: model.to_string(),
            api_url: api_url.to_string(),
            api_key: api_key.to_string(),
        })
    }

    fn spawn_stream_task(
        resp: reqwest::Response,
        cancel: crate::cancel::CancellationToken,
        tx: mpsc::UnboundedSender<Result<Event>>,
    ) {
        tokio::spawn(async move {
            let mut byte_stream = resp.bytes_stream();
            let mut buf: Vec<u8> = Vec::new();
            let mut decode_errors = 0u32;
            let mut clean_end = false;

            let mut parser = OpenAIParser::new();
            'outer: loop {
                tokio::select! {
                    _ = cancel.cancelled() => {
                        tx.send(Ok(Event::Stop(crate::protocol::StopEvent { reason: "interrupted".into() }))).ok();
                        clean_end = true;
                        break 'outer;
                    }
                    chunk = byte_stream.next() => {
                        match chunk {
                            Some(Ok(bytes)) => {
                                buf.extend_from_slice(&bytes);
                                while let Some(pos) = buf.iter().position(|&b| b == b'\n') {
                                    let line = String::from_utf8_lossy(&buf[..=pos]).to_string();
                                    buf.drain(..=pos);
                                    let l = line.trim();
                                    if l.is_empty() { continue; }
                                    let full = format!("{l}\n");
                                    match parser.process_line(&full, &mut |e| { tx.send(Ok(e)).ok(); Ok(()) }) {
                                        Ok(true) => {
                                            clean_end = true;
                                            break 'outer;
                                        }
                                        Err(e) => {
                                            if is_fatal_parser_error(&e) {
                                                tx.send(Err(e)).ok();
                                                clean_end = true;
                                                break 'outer;
                                            }
                                            decode_errors += 1;
                                            if decode_errors > MAX_STREAM_ERRORS {
                                                tx.send(Err(e)).ok();
                                                clean_end = true;
                                                break 'outer;
                                            }
                                        }
                                        _ => {}
                                    }
                                }
                            }
                            Some(Err(e)) => {
                                decode_errors += 1;
                                if decode_errors > MAX_STREAM_ERRORS {
                                    tx.send(Err(anyhow::anyhow!("stream: {e}"))).ok();
                                    clean_end = true;
                                    break 'outer;
                                }
                            }
                            None => break 'outer,
                        }
                    }
                }
            }
            if !clean_end
                && let Err(e) = parser.finish_eof(&mut |e| {
                    tx.send(Ok(e)).ok();
                    Ok(())
                })
            {
                tx.send(Err(e)).ok();
            }
            parser
                .flush(&mut |e| {
                    tx.send(Ok(e)).ok();
                    Ok(())
                })
                .ok();
        });
    }

    pub async fn send_with_retry(
        &self,
        ctx: &AgentSharedContext,
        body: Vec<u8>,
    ) -> Result<reqwest::Response> {
        self.send_with_retry_counted(ctx, body)
            .await
            .map(|(response, _)| response)
            .map_err(|failure| failure.error)
    }

    async fn send_with_retry_counted(
        &self,
        ctx: &AgentSharedContext,
        body: Vec<u8>,
    ) -> std::result::Result<(reqwest::Response, u32), SendFailure> {
        self.send_body_with_retry(ctx.display.as_ref(), body).await
    }

    async fn send_body_with_retry(
        &self,
        display: &dyn crate::ui::Display,
        body: Vec<u8>,
    ) -> std::result::Result<(reqwest::Response, u32), SendFailure> {
        let start = std::time::Instant::now();
        let mut attempt: u32 = 0;

        loop {
            let attempt_count = attempt.saturating_add(1);
            let req = self
                .client
                .post(&self.api_url)
                .body(body.clone())
                .header("Content-Type", "application/json")
                .header("Authorization", format!("Bearer {}", self.api_key));

            match req.send().await {
                Ok(resp) => {
                    let code = resp.status().as_u16();
                    if code < 400 {
                        return Ok((resp, attempt_count));
                    }
                    if !is_retryable(code) || !can_retry(attempt, start) {
                        let body_text = resp.text().await.unwrap_or_default();
                        let err = anyhow::anyhow!("HTTP {}: {}", code, body_text.trim());
                        display.render_error(&err.to_string());
                        return Err(SendFailure {
                            error: err,
                            attempt_count,
                        });
                    }
                    if code == 429 {
                        let retry_after = resp
                            .headers()
                            .get("retry-after")
                            .and_then(|v| v.to_str().ok())
                            .and_then(|v| v.parse::<u64>().ok())
                            .unwrap_or(2);
                        tokio::time::sleep(Duration::from_secs(retry_after)).await;
                    } else {
                        tokio::time::sleep(RETRY_DELAY * 2u32.pow(attempt)).await;
                    }
                }
                Err(e) => {
                    if !can_retry(attempt, start) {
                        let err = anyhow::anyhow!("{}", e);
                        display.render_error(&err.to_string());
                        return Err(SendFailure {
                            error: err,
                            attempt_count,
                        });
                    }
                    tokio::time::sleep(RETRY_DELAY).await;
                }
            }
            attempt += 1;
            display.render_info(&format!("Retrying ({}/{})...", attempt, MAX_RETRIES));
        }
    }

    pub async fn stream_request(
        &self,
        request_ctx: &LlmRequestContext,
        messages_json: &[serde_json::Value],
        tools_json: &[serde_json::Value],
        system_prompt: &str,
    ) -> Result<Box<dyn futures::Stream<Item = Result<Event>> + Unpin + Send>> {
        let body = crate::llm::transport::build_openai_body(
            &self.model_name,
            messages_json,
            tools_json,
            system_prompt,
            request_ctx.max_tokens,
        )?;

        if request_ctx.verbose {
            let preview = String::from_utf8_lossy(&body);
            let truncated: String = preview.chars().take(200).collect();
            request_ctx.display.render_info(&format!(
                "Request body ({}KB): {}...",
                body.len() / 1024,
                truncated
            ));
        }

        let capture = request_ctx
            .usage
            .capture(request_ctx.usage_scope.clone(), self.model_name.clone());
        let (resp, attempt_count) = match self
            .send_body_with_retry(request_ctx.display.as_ref(), body)
            .await
        {
            Ok(result) => result,
            Err(failure) => {
                record_unreported(
                    &capture,
                    failure.attempt_count,
                    format!("request_failed: {}", failure.error),
                );
                return Err(failure.error);
            }
        };

        let (tx, rx) = mpsc::unbounded_channel();
        let cancel = request_ctx.cancel.linked_child_token();
        Self::spawn_stream_task(resp, cancel.clone(), tx);

        Ok(Box::new(MeteredStream::new(
            SseEventStream { rx, cancel },
            capture,
            attempt_count,
        )))
    }
}

fn is_fatal_parser_error(e: &anyhow::Error) -> bool {
    let msg = e.to_string();
    msg.contains("parse tool call")
        || msg.contains("parse tool input")
        || msg.contains("tool input must be object")
}

#[async_trait::async_trait]
impl LlmClient for AsyncLlClient {
    fn model(&self) -> &str {
        &self.model_name
    }

    async fn stream(
        &self,
        ctx: &AgentSharedContext,
        messages_json: &[serde_json::Value],
        tools_json: &[serde_json::Value],
        system_prompt: &str,
    ) -> Result<Box<dyn futures::Stream<Item = Result<Event>> + Unpin + Send>> {
        self.stream_request(
            &LlmRequestContext::from_agent(ctx),
            messages_json,
            tools_json,
            system_prompt,
        )
        .await
    }
}

fn is_retryable(code: u16) -> bool {
    matches!(code, 408 | 409 | 425 | 429 | 500 | 502 | 503 | 504)
}

fn can_retry(attempt: u32, start: std::time::Instant) -> bool {
    attempt < MAX_RETRIES && start.elapsed() <= RETRY_MAX_TIME
}

pub struct SseEventStream {
    rx: mpsc::UnboundedReceiver<Result<Event>>,
    cancel: crate::cancel::CancellationToken,
}

pub(crate) struct MeteredStream<S> {
    inner: S,
    capture: UsageCapture,
    attempt_count: u32,
    completed: bool,
}

impl<S> MeteredStream<S> {
    pub(crate) fn new(inner: S, capture: UsageCapture, attempt_count: u32) -> Self {
        Self {
            inner,
            capture,
            attempt_count,
            completed: false,
        }
    }

    fn finish_unreported(&mut self, reason: impl Into<String>) {
        if self.completed {
            return;
        }
        self.completed = true;
        record_unreported(&self.capture, self.attempt_count, reason);
    }
}

impl<S> Drop for MeteredStream<S> {
    fn drop(&mut self) {
        self.finish_unreported("stream_dropped_before_usage");
    }
}

impl<S> futures::Stream for MeteredStream<S>
where
    S: futures::Stream<Item = Result<Event>> + Unpin,
{
    type Item = Result<Event>;

    fn poll_next(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        match std::pin::Pin::new(&mut self.inner).poll_next(cx) {
            std::task::Poll::Ready(Some(Ok(Event::Usage(usage)))) => {
                if !self.completed {
                    self.completed = true;
                    if let Err(error) = self.capture.reported(&usage, self.attempt_count) {
                        record_unreported(
                            &self.capture,
                            self.attempt_count,
                            format!("invalid_provider_usage: {error}"),
                        );
                    }
                }
                std::task::Poll::Ready(Some(Ok(Event::Usage(usage))))
            }
            std::task::Poll::Ready(Some(Ok(Event::UsageUnavailable))) => {
                self.finish_unreported("provider_usage_missing");
                std::task::Poll::Ready(Some(Ok(Event::UsageUnavailable)))
            }
            std::task::Poll::Ready(Some(Err(error))) => {
                self.finish_unreported(format!("stream_error: {error}"));
                std::task::Poll::Ready(Some(Err(error)))
            }
            std::task::Poll::Ready(None) => {
                self.finish_unreported("stream_ended_without_usage");
                std::task::Poll::Ready(None)
            }
            other => other,
        }
    }
}

fn record_unreported(capture: &UsageCapture, attempt_count: u32, reason: impl Into<String>) {
    if let Err(error) = capture.unreported(attempt_count, reason) {
        eprintln!("[mink] Warning: failed to record LLM usage: {error}");
    }
}

impl Drop for SseEventStream {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

impl futures::Stream for SseEventStream {
    type Item = Result<Event>;

    fn poll_next(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        self.rx.poll_recv(cx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cancel::CancellationToken;
    use crate::config::{Config, OutputFormat};
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
        fn render_tool_call(&self, _name: &str, _summary: &str) {}
        fn render_tool_result(&self, _tool_name: &str, _content_preview: &str) {}
        fn render_stop(&self) {}
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
        let client = AsyncLlClient::new("deepseek-v4-flash", "secret-key", &api_url)?;

        let resp = client
            .send_with_retry(&ctx, br#"{"ping":true}"#.to_vec())
            .await?;
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
        let client = AsyncLlClient::new("deepseek-v4-flash", "secret-key", &api_url)?;

        let err = client
            .send_with_retry(&ctx, br#"{"ping":true}"#.to_vec())
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("HTTP 400"), "{err}");
        assert_eq!(seen.load(Ordering::SeqCst), 1);
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
        let client = AsyncLlClient::new("deepseek-v4-flash", "secret-key", &api_url)?;
        let mut stream = client
            .stream(
                &ctx,
                &[json!({"role":"user","content":"ping"})],
                &[],
                "system",
            )
            .await?;

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
            "client",
            "client",
            &cfg.skills,
        )?);
        let compaction = Arc::new(CompactionEngine::new_with_usage(
            store.clone(),
            spaths.summary.clone(),
            spaths.plan.clone(),
            spaths.plan_draft.clone(),
            cwd.clone(),
            home.clone(),
            capability_snapshot.clone(),
            api_url.to_string(),
            &cfg,
            stats.clone(),
            usage.clone(),
            "client".into(),
            Arc::new(TestDisplay::new()),
            CancellationToken::new(),
        ));
        Ok(Arc::new(AgentSharedContext {
            config: cfg.clone(),
            cwd: cwd.clone(),
            home,
            session_layout: crate::session::paths::SessionLayout::ProjectScoped,
            api_url: api_url.to_string(),
            store,
            artifacts,
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
            tool_config: ToolConfig::from_config(&cfg),
            events_path: spaths.events,
            summary_path: spaths.summary,
            plan_path: spaths.plan,
            plan_draft_path: spaths.plan_draft,
            immutable_prefix: Mutex::new(None),
            is_sub_agent: false,
            interrupt: Arc::new(AtomicBool::new(false)),
            event_log_warned: AtomicBool::new(false),
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
}
