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

            let mut parser = OpenAIParser::new();
            'outer: loop {
                tokio::select! {
                    _ = cancel.cancelled() => {
                        tx.send(Ok(Event::Stop(crate::protocol::StopEvent { reason: "interrupted".into() }))).ok();
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
                                        Ok(true) => break 'outer,
                                        Err(e) => {
                                            decode_errors += 1;
                                            if decode_errors > MAX_STREAM_ERRORS {
                                                tx.send(Err(e)).ok();
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
                                    break 'outer;
                                }
                            }
                            None => break 'outer,
                        }
                    }
                }
            }
            parser.flush(&mut |e| { tx.send(Ok(e)).ok(); Ok(()) }).ok();
        });
    }

    pub async fn send_with_retry(&self, ctx: &AgentSharedContext, body: Vec<u8>) -> Result<reqwest::Response> {
        let start = std::time::Instant::now();
        let mut attempt: u32 = 0;

        loop {
            let req = self.client.post(&self.api_url)
                .body(body.clone())
                .header("Content-Type", "application/json")
                .header("Authorization", format!("Bearer {}", self.api_key));

            match req.send().await {
                Ok(resp) => {
                    let code = resp.status().as_u16();
                    if code < 400 { return Ok(resp); }
                    if !is_retryable(code) || !can_retry(attempt, start) {
                        let body_text = resp.text().await.unwrap_or_default();
                        let err = anyhow::anyhow!("HTTP {}: {}", code, body_text.trim());
                        ctx.display.render_error(&err.to_string());
                        return Err(err);
                    }
                    if code == 429 {
                        let retry_after = resp.headers().get("retry-after")
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
            ctx.display.render_info(&format!("Retrying ({}/{})...", attempt, MAX_RETRIES));
        }
    }
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
            &self.model_name, &messages_json, tools_json, system_prompt,
            ctx.max_tokens(),
        )?;

        if ctx.verbose() {
            let preview = String::from_utf8_lossy(&body);
            let truncated: String = preview.chars().take(200).collect();
            ctx.display.render_info(&format!("Request body ({}KB): {}...", body.len() / 1024, truncated));
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
