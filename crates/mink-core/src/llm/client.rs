use crate::context::AgentSharedContext;
use crate::protocol::Event;
use crate::session::usage::{UsageCapture, UsageKind};
use crate::sse::openai::OpenAIParser;
use anyhow::Result;
use futures::StreamExt;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;

const MAX_RETRIES: u32 = 2;
const RETRY_DELAY: Duration = Duration::from_secs(1);
const RETRY_MAX_TIME: Duration = Duration::from_secs(20);

const MAX_STREAM_ERRORS: u32 = 5;

#[derive(Debug, Clone, Copy)]
pub struct LlmModelTarget<'a> {
    pub model: &'a str,
    pub alias: Option<&'a str>,
}

impl<'a> LlmModelTarget<'a> {
    pub fn new(model: &'a str, alias: Option<&'a str>) -> Self {
        Self { model, alias }
    }
}

#[async_trait::async_trait]
pub trait LlmClient: Send + Sync {
    fn model(&self) -> &str;
    fn model_alias(&self) -> Option<&str> {
        None
    }
    fn model_label(&self) -> &str {
        self.model_alias().unwrap_or_else(|| self.model())
    }
    fn model_target(&self) -> LlmModelTarget<'_> {
        LlmModelTarget::new(self.model(), self.model_alias())
    }
    async fn stream(
        &self,
        ctx: &AgentSharedContext,
        messages_json: &[serde_json::Value],
        tools_json: &[serde_json::Value],
        system_prompt: &str,
    ) -> Result<Box<dyn futures::Stream<Item = Result<Event>> + Unpin + Send>>;
}

pub type LlmEvent = Event;
pub type LlmTextEvent = crate::protocol::TextEvent;
pub type LlmThinkingEvent = crate::protocol::ThinkingEvent;
pub type LlmToolCallEvent = crate::protocol::ToolCallEvent;
pub type LlmUsageEvent = crate::protocol::UsageEvent;
pub type LlmStopEvent = crate::protocol::StopEvent;
pub type LlmErrorEvent = crate::protocol::ErrorEvent;
pub type LlmRetryEvent = crate::protocol::RetryEvent;
pub type LlmCancelToken = crate::cancel::CancellationToken;
pub type LlmEventStream = Pin<Box<dyn futures::Stream<Item = Result<LlmEvent>> + Send>>;

pub struct LlmResponseStream {
    pub events: LlmEventStream,
    pub attempt_count: u32,
}

#[derive(Debug)]
pub struct LlmRequestFailure {
    pub attempt_count: u32,
    pub error: anyhow::Error,
}

impl std::fmt::Display for LlmRequestFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.error)
    }
}

impl std::error::Error for LlmRequestFailure {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.error.source()
    }
}

#[derive(Debug, Clone)]
pub enum LlmPurpose {
    Agent,
    SubAgent { session_id: String },
    Compaction,
}

pub struct LlmRequest {
    pub purpose: LlmPurpose,
    pub model: String,
    pub model_alias: Option<String>,
    pub api_url: String,
    pub api_key: String,
    pub system_prompt: String,
    pub messages: Vec<serde_json::Value>,
    pub tools: Vec<serde_json::Value>,
    pub max_tokens: i32,
    pub cancel: LlmCancelToken,
    pub verbose: bool,
    pub display: Arc<dyn crate::ui::Display>,
}

#[async_trait::async_trait]
pub trait LlmBackend: Send + Sync {
    fn name(&self) -> &str;
    async fn stream(&self, request: LlmRequest) -> Result<LlmResponseStream>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenParamKind {
    MaxTokens,
    MaxCompletionTokens,
}

#[derive(Debug, Clone)]
pub struct OpenAiCompatibleOptions {
    pub send_reasoning_effort: bool,
    pub reasoning_effort: Option<String>,
    pub include_usage: bool,
    pub token_param: TokenParamKind,
    pub parallel_tool_calls: Option<bool>,
}

impl Default for OpenAiCompatibleOptions {
    fn default() -> Self {
        Self {
            send_reasoning_effort: true,
            reasoning_effort: Some("max".to_string()),
            include_usage: true,
            token_param: TokenParamKind::MaxTokens,
            parallel_tool_calls: None,
        }
    }
}

pub struct OpenAiCompatibleBackend {
    options: OpenAiCompatibleOptions,
    tool_choice: Option<serde_json::Value>,
    extra_body: std::collections::BTreeMap<String, serde_json::Value>,
}

impl OpenAiCompatibleBackend {
    pub fn new(options: OpenAiCompatibleOptions) -> Self {
        Self {
            options,
            tool_choice: None,
            extra_body: std::collections::BTreeMap::new(),
        }
    }

    pub fn with_tool_choice(mut self, tool_choice: impl Into<serde_json::Value>) -> Self {
        self.tool_choice = Some(tool_choice.into());
        self
    }

    pub fn with_extra_body(
        mut self,
        extra_body: std::collections::BTreeMap<String, serde_json::Value>,
    ) -> Self {
        self.extra_body = extra_body;
        self
    }

    pub fn from_config(config: &crate::config::Config) -> Self {
        let token_param = match config.openai_token_param {
            crate::config::OpenAiTokenParamConfig::MaxTokens => TokenParamKind::MaxTokens,
            crate::config::OpenAiTokenParamConfig::MaxCompletionTokens => {
                TokenParamKind::MaxCompletionTokens
            }
        };
        let reasoning_effort = config
            .openai_reasoning_effort
            .as_deref()
            .map(str::trim)
            .filter(|value| {
                !value.is_empty()
                    && !matches!(
                        value.to_ascii_lowercase().as_str(),
                        "off" | "none" | "false" | "disabled"
                    )
            })
            .map(str::to_string);
        Self::new(OpenAiCompatibleOptions {
            send_reasoning_effort: reasoning_effort.is_some(),
            reasoning_effort,
            include_usage: config.openai_include_usage,
            token_param,
            parallel_tool_calls: None,
        })
        .with_extra_body(config.openai_extra_body.clone())
        .with_optional_tool_choice(config.openai_tool_choice.clone())
    }

    pub fn deepseek_defaults() -> Self {
        Self::new(OpenAiCompatibleOptions::default())
    }

    fn with_optional_tool_choice(mut self, tool_choice: Option<serde_json::Value>) -> Self {
        self.tool_choice = tool_choice;
        self
    }
}

#[async_trait::async_trait]
impl LlmBackend for OpenAiCompatibleBackend {
    fn name(&self) -> &str {
        "openai-compatible"
    }

    async fn stream(&self, request: LlmRequest) -> Result<LlmResponseStream> {
        let client = AsyncLlClient::new(&request.api_key, &request.api_url)?;
        let body = crate::llm::transport::build_openai_body_with_options_and_extensions(
            &request.model,
            &request.messages,
            &request.tools,
            &request.system_prompt,
            request.max_tokens,
            &self.options,
            self.tool_choice.as_ref(),
            &self.extra_body,
        )?;

        if request.verbose {
            let preview = String::from_utf8_lossy(&body);
            let truncated: String = preview.chars().take(200).collect();
            request.display.render_info(&format!(
                "Request body ({}KB): {}...",
                body.len() / 1024,
                truncated
            ));
        }

        let (resp, attempt_count) = client
            .send_body_with_retry(request.display.as_ref(), body)
            .await
            .map_err(|failure| LlmRequestFailure {
                attempt_count: failure.attempt_count,
                error: failure.error,
            })?;
        let (tx, rx) = mpsc::unbounded_channel();
        let cancel = request.cancel.linked_child_token();
        AsyncLlClient::spawn_stream_task(resp, cancel.clone(), tx);
        Ok(LlmResponseStream {
            events: Box::pin(SseEventStream { rx, cancel }),
            attempt_count,
        })
    }
}

pub struct BackendLlmClient {
    backend: Arc<dyn LlmBackend>,
    model_name: String,
    model_alias: Option<String>,
}

impl BackendLlmClient {
    pub fn new(
        backend: Arc<dyn LlmBackend>,
        model_name: impl Into<String>,
        model_alias: Option<String>,
    ) -> Self {
        Self {
            backend,
            model_name: model_name.into(),
            model_alias,
        }
    }
}

pub struct AsyncLlClient {
    client: reqwest::Client,
    api_url: String,
    api_key: String,
}

pub(crate) struct SendFailure {
    pub(crate) error: anyhow::Error,
    pub(crate) attempt_count: u32,
}

impl AsyncLlClient {
    pub fn new(api_key: &str, api_url: &str) -> Result<Self> {
        let client = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(30))
            .timeout(Duration::from_secs(600))
            .user_agent("mink/3.0")
            .build()?;
        Ok(Self {
            client,
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
}

#[async_trait::async_trait]
impl LlmClient for BackendLlmClient {
    fn model(&self) -> &str {
        &self.model_name
    }

    fn model_alias(&self) -> Option<&str> {
        self.model_alias.as_deref()
    }

    async fn stream(
        &self,
        ctx: &AgentSharedContext,
        messages_json: &[serde_json::Value],
        tools_json: &[serde_json::Value],
        system_prompt: &str,
    ) -> Result<Box<dyn futures::Stream<Item = Result<Event>> + Unpin + Send>> {
        let purpose = if ctx.is_sub_agent {
            LlmPurpose::SubAgent {
                session_id: ctx.config.session_id.clone(),
            }
        } else {
            LlmPurpose::Agent
        };
        let capture = ctx.usage.capture(
            ctx.usage_scope(if ctx.is_sub_agent {
                UsageKind::SubAgent
            } else {
                UsageKind::Agent
            }),
            self.model_name.clone(),
        );
        let response = match self
            .backend
            .stream(LlmRequest {
                purpose,
                model: self.model_name.clone(),
                model_alias: self.model_alias.clone(),
                api_url: ctx.api_url.clone(),
                api_key: ctx.api_key().to_string(),
                system_prompt: system_prompt.to_string(),
                messages: messages_json.to_vec(),
                tools: tools_json.to_vec(),
                max_tokens: crate::session::compaction::effective_max_tokens(&ctx.config),
                cancel: ctx.cancel.clone(),
                verbose: ctx.verbose(),
                display: ctx.display.clone(),
            })
            .await
        {
            Ok(response) => response,
            Err(error) => {
                let attempt_count = request_failure_attempt_count(&error);
                record_unreported(&capture, attempt_count, format!("request_failed: {error}"));
                return Err(error);
            }
        };
        Ok(Box::new(MeteredStream::new(
            response.events,
            capture,
            response.attempt_count,
        )))
    }
}

fn is_fatal_parser_error(e: &anyhow::Error) -> bool {
    let msg = e.to_string();
    msg.contains("parse tool call")
        || msg.contains("parse tool input")
        || msg.contains("tool input must be object")
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

pub(crate) fn request_failure_attempt_count(error: &anyhow::Error) -> u32 {
    error
        .downcast_ref::<LlmRequestFailure>()
        .map(|failure| failure.attempt_count)
        .unwrap_or(1)
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
        let client = AsyncLlClient::new("secret-key", &api_url)?;

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
        let client = AsyncLlClient::new("secret-key", &api_url)?;

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
    async fn backend_client_records_unreported_usage_when_request_fails() -> anyhow::Result<()> {
        let ctx = test_context("backend-request-failed", "https://example.invalid/v1").await?;
        let client = BackendLlmClient::new(Arc::new(FailingBackend), "custom-model", None);
        let result = client
            .stream(
                &ctx,
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
    async fn backend_client_preserves_request_failure_attempt_count() -> anyhow::Result<()> {
        let ctx = test_context("backend-attempt-failed", "https://example.invalid/v1").await?;
        let client = BackendLlmClient::new(Arc::new(AttemptFailingBackend), "custom-model", None);
        let result = client
            .stream(
                &ctx,
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
            "client",
            "client",
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
