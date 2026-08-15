use crate::runtime::{TurnId, TurnOutcome};
use crate::tools::metadata::{ToolResultKind, ToolStatus};
use crate::ui::{
    ArtifactDisplay, Display, PresentedToolResultDisplay, StatsSnapshot, ToolCallDisplay,
    ToolPresentation,
};
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentEvent {
    pub turn_id: Option<TurnId>,
    pub sequence: u64,
    #[serde(flatten)]
    pub kind: AgentEventKind,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AgentEventKind {
    TurnStarted,
    Thinking {
        content: String,
    },
    Text {
        content: String,
    },
    ToolCall {
        id: String,
        name: String,
        summary: String,
        input: serde_json::Value,
    },
    ToolResult {
        tool_use_id: Option<String>,
        tool_name: String,
        content_preview: String,
        content: String,
        status: ToolStatus,
        exit_code: Option<i32>,
        result_kind: ToolResultKind,
        presentation: Option<ToolPresentation>,
        artifacts: Vec<ArtifactDisplay>,
    },
    Signal {
        signal_kind: String,
        severity: f64,
        message: String,
    },
    Stop {
        reason: String,
    },
    Retry,
    Error {
        message: String,
    },
    Info {
        message: String,
    },
    TitleUpdate {
        model: String,
        stats: StatsSnapshot,
    },
    SubAgentStatus {
        session_id: String,
        status: String,
        in_tokens: u64,
        out_tokens: u64,
    },
    SubAgentOutput {
        session_id: String,
        status: String,
        thinking: String,
        text: String,
        in_tokens: u64,
        out_tokens: u64,
    },
    Prompt,
    ClearLine,
    Final {
        outcome: Box<TurnOutcome>,
    },
}

#[async_trait::async_trait]
pub trait EventSink: Send + Sync {
    async fn on_event(&self, event: AgentEvent) -> Result<(), String>;
}

pub(crate) struct EventDispatcher {
    tx: Mutex<Option<tokio::sync::mpsc::Sender<AgentEvent>>>,
    task: Mutex<Option<tokio::task::JoinHandle<Result<(), String>>>>,
    failure: Arc<Mutex<Option<String>>>,
}

impl EventDispatcher {
    pub(crate) fn new(sink: Arc<dyn EventSink>) -> Arc<Self> {
        let (tx, mut rx) = tokio::sync::mpsc::channel(1024);
        let failure = Arc::new(Mutex::new(None));
        let task_failure = failure.clone();
        let task = tokio::spawn(async move {
            while let Some(event) = rx.recv().await {
                if let Err(error) = sink.on_event(event).await {
                    eprintln!("[mink] event observer failed: {error}");
                    *task_failure.lock().unwrap_or_else(|e| e.into_inner()) = Some(error.clone());
                    return Err(error);
                }
            }
            Ok(())
        });
        Arc::new(Self {
            tx: Mutex::new(Some(tx)),
            task: Mutex::new(Some(task)),
            failure,
        })
    }

    fn dispatch(&self, event: AgentEvent) {
        let failure = {
            let tx = self.tx.lock().unwrap_or_else(|e| e.into_inner());
            tx.as_ref().and_then(|tx| match tx.try_send(event) {
                Ok(()) => None,
                Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {
                    Some("event observer queue overflowed (capacity 1024)")
                }
                Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {
                    Some("event observer stopped before runtime shutdown")
                }
            })
        };
        if let Some(message) = failure {
            self.stop_with_failure(message);
        }
    }

    fn stop_with_failure(&self, message: impl Into<String>) {
        let message = message.into();
        let mut failure = self.failure.lock().unwrap_or_else(|e| e.into_inner());
        if failure.is_some() {
            return;
        }
        eprintln!("[mink] {message}");
        *failure = Some(message);
        self.tx.lock().unwrap_or_else(|e| e.into_inner()).take();
        if let Some(task) = self.task.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
            task.abort();
        }
    }

    pub(crate) async fn shutdown(&self) -> Result<(), String> {
        self.shutdown_with_timeout(std::time::Duration::from_secs(5))
            .await
    }

    async fn shutdown_with_timeout(&self, timeout: std::time::Duration) -> Result<(), String> {
        self.tx.lock().unwrap_or_else(|e| e.into_inner()).take();
        let mut task = self.task.lock().unwrap_or_else(|e| e.into_inner()).take();
        if let Some(handle) = task.as_mut() {
            match tokio::time::timeout(timeout, &mut *handle).await {
                Ok(Ok(Ok(()))) => {}
                Ok(Ok(Err(error))) => self.stop_with_failure(error),
                Ok(Err(error)) if error.is_cancelled() => {}
                Ok(Err(error)) => {
                    self.stop_with_failure(format!("event observer task failed: {error}"))
                }
                Err(_) => {
                    handle.abort();
                    let _ = handle.await;
                    self.stop_with_failure(format!(
                        "event observer shutdown timed out after {:.3}s",
                        timeout.as_secs_f64()
                    ));
                }
            }
        }
        self.failure
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
            .map_or(Ok(()), Err)
    }
}

pub(crate) struct TurnEventEmitter {
    turn_id: TurnId,
    next_sequence: AtomicU64,
    tx: Option<tokio::sync::mpsc::UnboundedSender<AgentEvent>>,
    dispatcher: Option<Arc<EventDispatcher>>,
}

impl TurnEventEmitter {
    pub(crate) fn new(
        turn_id: TurnId,
        tx: Option<tokio::sync::mpsc::UnboundedSender<AgentEvent>>,
        dispatcher: Option<Arc<EventDispatcher>>,
    ) -> Self {
        Self {
            turn_id,
            next_sequence: AtomicU64::new(1),
            tx,
            dispatcher,
        }
    }

    pub(crate) fn emit(&self, kind: AgentEventKind) {
        let event = AgentEvent {
            turn_id: Some(self.turn_id.clone()),
            sequence: self.next_sequence.fetch_add(1, Ordering::Relaxed),
            kind,
        };
        if let Some(tx) = &self.tx {
            let _ = tx.send(event.clone());
        }
        if let Some(dispatcher) = &self.dispatcher {
            dispatcher.dispatch(event);
        }
    }
}

pub(crate) struct EventDisplay {
    dispatcher: Option<Arc<EventDispatcher>>,
    next_control_sequence: AtomicU64,
    current_turn: Mutex<Option<Arc<TurnEventEmitter>>>,
}

impl EventDisplay {
    pub(crate) fn new(dispatcher: Option<Arc<EventDispatcher>>) -> Self {
        Self {
            dispatcher,
            next_control_sequence: AtomicU64::new(1),
            current_turn: Mutex::new(None),
        }
    }

    pub(crate) fn begin_turn(&self, emitter: Arc<TurnEventEmitter>) {
        *self.current_turn.lock().unwrap_or_else(|e| e.into_inner()) = Some(emitter);
    }

    pub(crate) fn dispatcher(&self) -> Option<Arc<EventDispatcher>> {
        self.dispatcher.clone()
    }

    pub(crate) fn end_turn(&self, turn_id: &TurnId) {
        let mut current = self.current_turn.lock().unwrap_or_else(|e| e.into_inner());
        if current
            .as_ref()
            .is_some_and(|emitter| &emitter.turn_id == turn_id)
        {
            *current = None;
        }
    }

    fn emit(&self, kind: AgentEventKind) {
        let emitter = self
            .current_turn
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        if let Some(emitter) = emitter {
            emitter.emit(kind);
        } else if let Some(dispatcher) = &self.dispatcher {
            dispatcher.dispatch(AgentEvent {
                turn_id: None,
                sequence: self.next_control_sequence.fetch_add(1, Ordering::Relaxed),
                kind,
            });
        }
    }
}

impl Display for EventDisplay {
    fn render_thinking(&self, content: &str) {
        self.emit(AgentEventKind::Thinking {
            content: content.into(),
        });
    }
    fn render_text(&self, content: &str) {
        self.emit(AgentEventKind::Text {
            content: content.into(),
        });
    }
    fn render_tool_call(&self, call: &ToolCallDisplay<'_>) {
        self.emit(AgentEventKind::ToolCall {
            id: call.tool_use_id.into(),
            name: call.tool_name.into(),
            summary: call.summary.into(),
            input: call.input.cloned().unwrap_or(serde_json::Value::Null),
        });
    }
    fn render_tool_result(&self, result: &PresentedToolResultDisplay<'_>) {
        self.emit(AgentEventKind::ToolResult {
            tool_use_id: result.base.tool_use_id.map(Into::into),
            tool_name: result.base.tool_name.into(),
            content_preview: result.base.content_preview.into(),
            content: result.base.content.into(),
            status: result.status,
            exit_code: result.base.exit_code,
            result_kind: result.result_kind,
            presentation: result.presentation.cloned(),
            artifacts: result.artifacts.to_vec(),
        });
    }
    fn render_signal(&self, signal_kind: &str, severity: f64, message: &str) {
        self.emit(AgentEventKind::Signal {
            signal_kind: signal_kind.into(),
            severity,
            message: message.into(),
        });
    }
    fn render_stop(&self, reason: &str) {
        self.emit(AgentEventKind::Stop {
            reason: reason.into(),
        });
    }
    fn render_error(&self, message: &str) {
        self.emit(AgentEventKind::Error {
            message: message.into(),
        });
    }
    fn render_retry(&self) {
        self.emit(AgentEventKind::Retry);
    }
    fn render_info(&self, msg: &str) {
        self.emit(AgentEventKind::Info {
            message: msg.into(),
        });
    }
    fn render_title_update(&self, model: &str, stats: &StatsSnapshot) {
        self.emit(AgentEventKind::TitleUpdate {
            model: model.into(),
            stats: stats.clone(),
        });
    }
    fn render_sub_agent_status(
        &self,
        session_id: &str,
        status: &str,
        in_tokens: u64,
        out_tokens: u64,
    ) {
        self.emit(AgentEventKind::SubAgentStatus {
            session_id: session_id.into(),
            status: status.into(),
            in_tokens,
            out_tokens,
        });
    }
    fn render_sub_agent_output(
        &self,
        session_id: &str,
        status: &str,
        thinking: &str,
        text: &str,
        in_tokens: u64,
        out_tokens: u64,
    ) {
        self.emit(AgentEventKind::SubAgentOutput {
            session_id: session_id.into(),
            status: status.into(),
            thinking: thinking.into(),
            text: text.into(),
            in_tokens,
            out_tokens,
        });
    }
    fn render_prompt(&self) {
        self.emit(AgentEventKind::Prompt);
    }
    fn render_clear_line(&self) {
        self.emit(AgentEventKind::ClearLine);
    }
}

#[cfg(test)]
mod tests {
    use super::{AgentEvent, AgentEventKind, EventDispatcher, EventSink};
    use std::sync::Arc;

    #[test]
    fn shared_server_protocol_fixture_is_real_agent_event_json() {
        let fixture = include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../mink-server/protocol-fixtures/agent-events.json"
        ));
        let expected: serde_json::Value = serde_json::from_str(fixture).unwrap();
        let events: Vec<AgentEvent> = serde_json::from_value(expected.clone()).unwrap();
        assert_eq!(serde_json::to_value(events).unwrap(), expected);
    }

    struct BlockingSink;

    #[async_trait::async_trait]
    impl EventSink for BlockingSink {
        async fn on_event(&self, _event: AgentEvent) -> Result<(), String> {
            std::future::pending().await
        }
    }

    fn info_event(sequence: u64) -> AgentEvent {
        AgentEvent {
            turn_id: None,
            sequence,
            kind: AgentEventKind::Info {
                message: sequence.to_string(),
            },
        }
    }

    #[tokio::test]
    async fn observer_overflow_stops_only_the_observer() {
        let dispatcher = EventDispatcher::new(Arc::new(BlockingSink));
        for sequence in 0..2048 {
            dispatcher.dispatch(info_event(sequence));
        }

        let error = dispatcher.shutdown().await.unwrap_err();
        assert!(error.contains("overflowed"));
    }

    struct FailingSink;

    #[async_trait::async_trait]
    impl EventSink for FailingSink {
        async fn on_event(&self, _event: AgentEvent) -> Result<(), String> {
            Err("observer failure".into())
        }
    }

    #[tokio::test]
    async fn observer_failure_is_reported_at_shutdown() {
        let dispatcher = EventDispatcher::new(Arc::new(FailingSink));
        dispatcher.dispatch(info_event(1));
        tokio::task::yield_now().await;
        dispatcher.dispatch(info_event(2));
        let error = dispatcher.shutdown().await.unwrap_err();
        assert!(error.contains("observer failure"));
        assert!(!error.contains("stopped before runtime shutdown"));
    }

    #[tokio::test]
    async fn observer_shutdown_timeout_aborts_and_reports_failure() {
        let dispatcher = EventDispatcher::new(Arc::new(BlockingSink));
        dispatcher.dispatch(info_event(1));
        tokio::task::yield_now().await;
        let error = dispatcher
            .shutdown_with_timeout(std::time::Duration::from_millis(10))
            .await
            .unwrap_err();
        assert!(error.contains("shutdown timed out"));
    }
}
