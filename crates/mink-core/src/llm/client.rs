use crate::context::AgentSharedContext;
use crate::protocol::Event;
use crate::session::usage::{UsageCapture, UsageKind};
use crate::sse::openai::OpenAIParser;
use anyhow::Result;
use futures::StreamExt;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
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

impl TokenParamKind {
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "max_tokens" | "max-tokens" | "max_tokens_param" => Some(Self::MaxTokens),
            "max_completion_tokens" | "max-completion-tokens" => Some(Self::MaxCompletionTokens),
            _ => None,
        }
    }
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
    client: Mutex<Option<reqwest::Client>>,
}

impl OpenAiCompatibleBackend {
    pub fn new(options: OpenAiCompatibleOptions) -> Self {
        Self {
            options,
            tool_choice: None,
            extra_body: std::collections::BTreeMap::new(),
            client: Mutex::new(None),
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

    fn http_client(&self) -> Result<reqwest::Client> {
        let mut slot = self
            .client
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if let Some(client) = slot.as_ref() {
            return Ok(client.clone());
        }
        let client = build_http_client()?;
        *slot = Some(client.clone());
        Ok(client)
    }

    pub fn from_config(config: &crate::config::ResolvedConfig) -> Self {
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
            token_param: config.openai_token_param,
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
        let client = AsyncLlClient::from_client(
            self.http_client()?.clone(),
            &request.api_key,
            &request.api_url,
        );
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
            .send_with_retry(request.display.as_ref(), body, &request.cancel)
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

pub struct AsyncLlClient {
    client: reqwest::Client,
    api_url: String,
    api_key: String,
}

pub(crate) struct SendFailure {
    pub(crate) error: anyhow::Error,
    pub(crate) attempt_count: u32,
}

fn build_http_client() -> reqwest::Result<reqwest::Client> {
    reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(30))
        .timeout(Duration::from_secs(600))
        .user_agent(concat!("mink/", env!("CARGO_PKG_VERSION")))
        .build()
}

impl AsyncLlClient {
    #[cfg(test)]
    pub fn new(api_key: &str, api_url: &str) -> Result<Self> {
        Ok(Self::from_client(build_http_client()?, api_key, api_url))
    }

    pub fn from_client(client: reqwest::Client, api_key: &str, api_url: &str) -> Self {
        Self {
            client,
            api_url: api_url.to_string(),
            api_key: api_key.to_string(),
        }
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
                            None => {
                                // 传输 EOF：缓冲可能残留未换行的最后一行
                                //（连接在帧尾被切断但 JSON 完整）。先送入
                                // parser 再 finish_eof，否则末帧事件丢失。
                                if !buf.is_empty() {
                                    let line = String::from_utf8_lossy(&buf).to_string();
                                    let l = line.trim();
                                    if !l.is_empty() {
                                        let full = format!("{l}\n");
                                        match parser.process_line(
                                            &full,
                                            &mut |e| {
                                                tx.send(Ok(e)).ok();
                                                Ok(())
                                            },
                                        ) {
                                            Ok(true) => clean_end = true,
                                            Ok(false) => {}
                                            Err(e) => {
                                                if is_fatal_parser_error(&e) {
                                                    tx.send(Err(e)).ok();
                                                    clean_end = true;
                                                } else {
                                                    decode_errors += 1;
                                                    if decode_errors > MAX_STREAM_ERRORS {
                                                        tx.send(Err(e)).ok();
                                                        clean_end = true;
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                                break 'outer;
                            }
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

    async fn send_with_retry(
        &self,
        display: &dyn crate::ui::Display,
        body: Vec<u8>,
        cancel: &crate::cancel::CancellationToken,
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
            match tokio::select! {
                result = req.send() => result,
                _ = cancel.cancelled() => {
                    return Err(SendFailure {
                        error: anyhow::anyhow!("request cancelled"),
                        attempt_count,
                    })
                }
            } {
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
                            .unwrap_or(2)
                            .min(10);
                        tokio::select! {
                            _ = tokio::time::sleep(Duration::from_secs(retry_after)) => {}
                            _ = cancel.cancelled() => {
                                return Err(SendFailure {
                                    error: anyhow::anyhow!("request cancelled"),
                                    attempt_count,
                                })
                            }
                        }
                    } else {
                        tokio::select! {
                            _ = tokio::time::sleep(RETRY_DELAY * 2u32.pow(attempt)) => {}
                            _ = cancel.cancelled() => {
                                return Err(SendFailure {
                                    error: anyhow::anyhow!("request cancelled"),
                                    attempt_count,
                                })
                            }
                        }
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
                    tokio::select! {
                        _ = tokio::time::sleep(RETRY_DELAY) => {}
                        _ = cancel.cancelled() => {
                            return Err(SendFailure {
                                error: anyhow::anyhow!("request cancelled"),
                                attempt_count,
                            })
                        }
                    }
                }
            }
            attempt += 1;
            display.render_info(&format!("Retrying ({}/{})...", attempt, MAX_RETRIES));
        }
    }
}

pub(crate) async fn stream_backend(
    backend: &Arc<dyn LlmBackend>,
    ctx: &AgentSharedContext,
    model_name: &str,
    model_alias: Option<&str>,
    messages_json: &[serde_json::Value],
    tools_json: &[serde_json::Value],
    system_prompt: &str,
) -> Result<LlmEventStream> {
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
        model_name.to_string(),
    );
    let response = match backend
        .stream(LlmRequest {
            purpose,
            model: model_name.to_string(),
            model_alias: model_alias.map(str::to_string),
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
    Ok(Box::pin(MeteredStream::new(
        response.events,
        capture,
        response.attempt_count,
    )))
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
#[path = "client_tests.rs"]
mod tests;
