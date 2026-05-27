use crate::context::AgentSharedContext;
use crate::protocol::Event;
use crate::sse::openai::OpenAIParser;
use anyhow::Result;
use futures::StreamExt;
use std::time::Duration;
use tokio::sync::mpsc;

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

impl AsyncLlClient {
    pub fn new(model: &str, api_key: &str, api_url: &str) -> Result<Self> {
        let client = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(30))
            .timeout(Duration::from_secs(600))
            .user_agent("dscode/3.0")
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
        let start = std::time::Instant::now();
        let mut attempt: u32 = 0;

        loop {
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
                        return Ok(resp);
                    }
                    if !is_retryable(code) || !can_retry(attempt, start) {
                        let body_text = resp.text().await.unwrap_or_default();
                        let err = anyhow::anyhow!("HTTP {}: {}", code, body_text.trim());
                        ctx.display.render_error(&err.to_string());
                        return Err(err);
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
                        ctx.display.render_error(&err.to_string());
                        return Err(err);
                    }
                    tokio::time::sleep(RETRY_DELAY).await;
                }
            }
            attempt += 1;
            ctx.display
                .render_info(&format!("Retrying ({}/{})...", attempt, MAX_RETRIES));
        }
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
        let body = crate::llm::transport::build_openai_body(
            &self.model_name,
            messages_json,
            tools_json,
            system_prompt,
            ctx.max_tokens(),
        )?;

        if ctx.verbose() {
            let preview = String::from_utf8_lossy(&body);
            let truncated: String = preview.chars().take(200).collect();
            ctx.display.render_info(&format!(
                "Request body ({}KB): {}...",
                body.len() / 1024,
                truncated
            ));
        }

        let resp = self.send_with_retry(ctx, body).await?;

        let (tx, rx) = mpsc::unbounded_channel();
        let cancel = ctx.cancel.child_token();

        Self::spawn_stream_task(resp, cancel, tx);

        Ok(Box::new(SseEventStream { rx }))
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
            "dscode-client-test-{}-{name}-{n}",
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
        let cfg = Config {
            model: "flash".into(),
            api_key: "secret-key".into(),
            base_url: api_url.to_string(),
            output_format: OutputFormat::Human,
            ..Default::default()
        };
        let compaction = Arc::new(CompactionEngine::new(
            store.clone(),
            spaths.summary.clone(),
            spaths.plan.clone(),
            spaths.plan_draft.clone(),
            cwd.clone(),
            home.clone(),
            Vec::new(),
            api_url.to_string(),
            &cfg,
            stats.clone(),
            reqwest::Client::new(),
        ));
        Ok(Arc::new(AgentSharedContext {
            config: cfg.clone(),
            cwd,
            home,
            api_url: api_url.to_string(),
            store,
            stats,
            compaction,
            cancel: CancellationToken::new(),
            display: Arc::new(TestDisplay::new()),
            sub_stream_tx: None,
            tool_config: ToolConfig::from_config(&cfg),
            events_path: spaths.events,
            summary_path: spaths.summary,
            plan_path: spaths.plan,
            plan_draft_path: spaths.plan_draft,
            immutable_prefix: Mutex::new(None),
            is_sub_agent: false,
            interrupt: Arc::new(AtomicBool::new(false)),
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
