use crate::agent::orchestrator::{OrchCmd, TurnRunResult, TurnStatus};
use crate::cancel::CancellationToken;
use crate::context::AgentSharedContext;
use crate::runtime::config::SessionInfo;
use crate::runtime::events::{EventDisplay, TurnEventSubscription};
use crate::runtime::{AgentEvent, EventSink};
use crate::session::store::ConversationStore;
use anyhow::{Result, bail};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;

#[derive(Debug, Clone)]
pub struct TurnOutcome {
    pub billing_turn_id: String,
    pub status: TurnStatus,
    pub session: SessionInfo,
    pub text: String,
    pub thinking: String,
    pub tool_call_count: u32,
    pub tool_error_count: u32,
    pub error: Option<String>,
    pub usage_records: Vec<crate::session::usage::UsageRecord>,
    pub usage: crate::session::usage::UsageSummary,
}

impl TurnOutcome {
    pub(crate) fn from_run_result(
        result: TurnRunResult,
        session: SessionInfo,
        text: String,
        thinking: String,
    ) -> Self {
        Self {
            billing_turn_id: result.billing_turn_id,
            status: result.status,
            session,
            text,
            thinking,
            tool_call_count: result.tool_call_count,
            tool_error_count: result.tool_error_count,
            error: result.error,
            usage_records: result.usage_records,
            usage: result.usage,
        }
    }
}

/// A stream of events from an in-progress turn.
///
/// Call [`AgentEventStream::recv`] to pull the next event. When the stream
/// yields `None`, call [`AgentEventStream::outcome`] to finalize.
pub struct AgentEventStream {
    pub rx: tokio::sync::mpsc::UnboundedReceiver<AgentEvent>,
    pub handle: JoinHandle<TurnOutcome>,
}

impl AgentEventStream {
    pub async fn recv(&mut self) -> Option<AgentEvent> {
        self.rx.recv().await
    }

    pub async fn outcome(self) -> Result<TurnOutcome> {
        Ok(self.handle.await?)
    }
}

/// Running embedded mink instance.
pub struct AgentRuntime {
    pub(crate) ctx: Arc<AgentSharedContext>,
    pub(crate) cmd_tx: mpsc::UnboundedSender<OrchCmd>,
    pub(crate) orch_handle: JoinHandle<()>,
    pub(crate) session: SessionInfo,
    pub(crate) event_sink: Option<Arc<dyn EventSink>>,
    pub(crate) event_display: Arc<EventDisplay>,
    pub(crate) stream_in_progress: Arc<AtomicBool>,
}

struct StreamTurnGuard {
    _subscription: TurnEventSubscription,
    flag: Arc<AtomicBool>,
}

impl StreamTurnGuard {
    fn new(subscription: TurnEventSubscription, flag: Arc<AtomicBool>) -> Self {
        Self {
            _subscription: subscription,
            flag,
        }
    }
}

impl Drop for StreamTurnGuard {
    fn drop(&mut self) {
        self.flag.store(false, Ordering::SeqCst);
    }
}

impl AgentRuntime {
    pub async fn start(config: crate::runtime::AgentRuntimeConfig) -> Result<Self> {
        crate::runtime::build_runtime(config).await
    }

    pub async fn start_with_options(options: crate::runtime::AgentOptions) -> Result<Self> {
        Self::start(options.into_runtime_config()).await
    }

    pub async fn run_turn(&self, input: impl Into<String>) -> Result<TurnOutcome> {
        let (done_tx, done_rx) = oneshot::channel();
        self.cmd_tx.send(OrchCmd::UserInput {
            input: input.into(),
            done: done_tx,
        })?;
        let result = done_rx.await.unwrap_or_else(|e| {
            TurnRunResult::failed(format!("orchestrator dropped turn result: {e}"))
        });
        let (text, thinking) = last_assistant_text(&self.ctx.store).await;
        let outcome = TurnOutcome::from_run_result(result, self.session.clone(), text, thinking);
        if let Some(event_sink) = &self.event_sink {
            event_sink.on_event(AgentEvent::Final {
                outcome: outcome.clone(),
            });
        }
        Ok(outcome)
    }

    /// Execute a turn and stream events as they happen.
    ///
    /// Prefer [`AgentRuntime::try_stream_turn`] in service code that needs to
    /// report concurrent-turn conflicts as ordinary errors.
    ///
    /// # Panics
    ///
    /// Panics if another `stream_turn()` is already in progress.
    pub fn stream_turn(&self, input: impl Into<String>) -> AgentEventStream {
        self.try_stream_turn(input)
            .expect("stream_turn already in progress")
    }

    /// Execute a turn and stream events as they happen.
    ///
    /// This is the non-panicking form of [`AgentRuntime::stream_turn`]. It
    /// returns an error when another streaming turn is already active on the
    /// same runtime.
    pub fn try_stream_turn(&self, input: impl Into<String>) -> Result<AgentEventStream> {
        if self.stream_in_progress.swap(true, Ordering::SeqCst) {
            bail!("stream_turn already in progress");
        }
        let flag = self.stream_in_progress.clone();
        let (tx, rx) = mpsc::unbounded_channel();
        let input = input.into();
        let cmd_tx = self.cmd_tx.clone();
        let session = self.session.clone();
        let store = self.ctx.store.clone();
        let event_display = self.event_display.clone();
        let event_sink = self.event_sink.clone();

        let subscription = event_display.subscribe_turn(tx.clone());

        let handle = tokio::spawn(async move {
            let _guard = StreamTurnGuard::new(subscription, flag);
            let (done_tx, done_rx) = oneshot::channel();
            let result = match cmd_tx.send(OrchCmd::UserInput {
                input,
                done: done_tx,
            }) {
                Ok(()) => done_rx
                    .await
                    .unwrap_or_else(|e| TurnRunResult::failed(format!("orchestrator: {e}"))),
                Err(e) => TurnRunResult::failed(format!("orchestrator command failed: {e}")),
            };

            let (text, thinking) = last_assistant_text(&store).await;

            let outcome = TurnOutcome::from_run_result(result, session, text, thinking);
            let final_event = AgentEvent::Final {
                outcome: outcome.clone(),
            };
            if let Some(sink) = event_sink {
                sink.on_event(final_event.clone());
            }
            let _ = tx.send(final_event);
            outcome
        });

        Ok(AgentEventStream { rx, handle })
    }

    pub async fn compact(&self) -> Result<()> {
        let (done_tx, done_rx) = oneshot::channel();
        self.cmd_tx.send(OrchCmd::Compact { done: done_tx })?;
        let _ = done_rx.await;
        Ok(())
    }

    pub fn set_model(&self, model: impl Into<String>) -> Result<()> {
        self.cmd_tx.send(OrchCmd::SetModel(model.into()))?;
        Ok(())
    }

    pub fn interrupt_current_turn(&self) {
        self.ctx.interrupt.store(true, Ordering::SeqCst);
    }

    pub async fn shutdown(self) -> Result<()> {
        let _ = self.ctx.stats.flush().await;
        self.ctx.cancel.cancel();
        let _ = tokio::time::timeout(std::time::Duration::from_secs(5), self.orch_handle).await;
        Ok(())
    }

    pub fn session_info(&self) -> &SessionInfo {
        &self.session
    }

    #[doc(hidden)]
    pub fn command_sender(&self) -> mpsc::UnboundedSender<OrchCmd> {
        self.cmd_tx.clone()
    }

    #[doc(hidden)]
    pub fn cancel_token(&self) -> CancellationToken {
        self.ctx.cancel.clone()
    }

    #[doc(hidden)]
    pub fn interrupt_flag(&self) -> Arc<AtomicBool> {
        self.ctx.interrupt.clone()
    }
}

async fn last_assistant_text(store: &ConversationStore) -> (String, String) {
    let Ok(messages) = store.lines().await else {
        return (String::new(), String::new());
    };
    extract_text_thinking(&messages)
}

fn extract_text_thinking(messages: &[serde_json::Value]) -> (String, String) {
    let mut text = String::new();
    let mut thinking = String::new();
    for msg in messages.iter().rev() {
        if msg.get("role").and_then(|v| v.as_str()) != Some("assistant") {
            continue;
        }
        if let Some(content) = msg.get("content").and_then(|v| v.as_array()) {
            for item in content {
                match item.get("type").and_then(|v| v.as_str()) {
                    Some("text") => {
                        text = item
                            .get("text")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .into();
                    }
                    Some("thinking") => {
                        thinking = item
                            .get("thinking")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .into();
                    }
                    _ => {}
                }
            }
        }
        break;
    }
    (text, thinking)
}
