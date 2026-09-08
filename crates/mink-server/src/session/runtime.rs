use anyhow::{Result, anyhow};
use mink::runtime::{AgentEvent, AgentEventStream, AgentOptions, AgentRuntime, SessionInfo};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;
use tokio::sync::broadcast;
use tokio::task::JoinHandle;

const TURN_SHUTDOWN_GRACE: std::time::Duration = std::time::Duration::from_secs(5);
const TURN_JOIN_GRACE: std::time::Duration = std::time::Duration::from_secs(1);
const OWNED_SHUTDOWN_JOIN_GRACE: std::time::Duration = std::time::Duration::from_secs(12);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimePhase {
    Idle,
    Running,
    Cancelling,
    Closing,
    Closed,
}

enum GuardedTurnResult {
    Finished(Result<()>),
    ForceClose {
        reason: String,
        saw_stop: bool,
        saw_final: bool,
        turn_id: String,
        session: Box<SessionInfo>,
    },
}

struct RuntimeState {
    runtime: Option<AgentRuntime>,
    phase: RuntimePhase,
    turn_task: Option<JoinHandle<Result<()>>>,
    last_idle: Instant,
    forced_terminal: Option<ForcedTerminal>,
}

struct ForcedTerminal {
    reason: String,
    saw_stop: bool,
    saw_final: bool,
    turn_id: String,
    session: Box<SessionInfo>,
}

pub struct SessionRuntime {
    state: Arc<Mutex<RuntimeState>>,
    event_tx: broadcast::Sender<String>,
    stream_sequence: Arc<AtomicU64>,
}

impl SessionRuntime {
    pub async fn open(options: AgentOptions) -> Result<Self> {
        let runtime = AgentRuntime::start(options).await?;
        let (event_tx, _) = broadcast::channel(1024);
        Ok(Self {
            state: Arc::new(Mutex::new(RuntimeState {
                runtime: Some(runtime),
                phase: RuntimePhase::Idle,
                turn_task: None,
                last_idle: Instant::now(),
                forced_terminal: None,
            })),
            event_tx,
            stream_sequence: Arc::new(AtomicU64::new(1)),
        })
    }

    pub fn phase(&self) -> RuntimePhase {
        self.state.lock().unwrap_or_else(|e| e.into_inner()).phase
    }
    pub fn running(&self) -> bool {
        matches!(
            self.phase(),
            RuntimePhase::Running | RuntimePhase::Cancelling | RuntimePhase::Closing
        )
    }
    pub fn closed(&self) -> bool {
        self.phase() == RuntimePhase::Closed
    }
    pub fn idle_for(&self) -> Option<std::time::Duration> {
        let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        (state.phase == RuntimePhase::Idle).then(|| state.last_idle.elapsed())
    }
    pub fn event_receiver(&self) -> broadcast::Receiver<String> {
        self.event_tx.subscribe()
    }

    pub fn start_turn(&self, input: String) -> Result<()> {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.phase != RuntimePhase::Idle {
            anyhow::bail!("session runtime is {:?}", state.phase);
        }
        if state
            .turn_task
            .as_ref()
            .is_some_and(|task| !task.is_finished())
        {
            anyhow::bail!("session turn cleanup is still running");
        }
        state.turn_task.take();
        let runtime = state
            .runtime
            .as_ref()
            .ok_or_else(|| anyhow!("session runtime is closed"))?;
        let session = runtime.session_info().clone();
        let stream = runtime.stream_turn(input)?;
        state.phase = RuntimePhase::Running;
        let shared = self.state.clone();
        let tx = self.event_tx.clone();
        let stream_sequence = self.stream_sequence.clone();
        state.turn_task = Some(tokio::spawn(async move {
            let guarded = run_turn_guarded(&tx, &stream_sequence, &shared, stream, session).await;
            let result = match guarded {
                GuardedTurnResult::Finished(result) => {
                    let mut state = shared.lock().unwrap_or_else(|e| e.into_inner());
                    if !matches!(state.phase, RuntimePhase::Closing | RuntimePhase::Closed) {
                        state.phase = RuntimePhase::Idle;
                        state.last_idle = Instant::now();
                    }
                    result
                }
                GuardedTurnResult::ForceClose {
                    reason,
                    saw_stop,
                    saw_final,
                    turn_id,
                    session,
                } => {
                    let runtime = {
                        let mut state = shared.lock().unwrap_or_else(|e| e.into_inner());
                        state.forced_terminal = Some(ForcedTerminal {
                            reason: reason.clone(),
                            saw_stop,
                            saw_final,
                            turn_id,
                            session,
                        });
                        state.phase = RuntimePhase::Closing;
                        state.runtime.take()
                    };
                    let owns_runtime_shutdown = runtime.is_some();
                    let shutdown_error = if let Some(runtime) = runtime {
                        runtime
                            .shutdown()
                            .await
                            .err()
                            .map(|error| error.to_string())
                    } else {
                        None
                    };
                    if owns_runtime_shutdown {
                        finalize_shutdown(
                            &shared,
                            &tx,
                            &stream_sequence,
                            shutdown_error.as_deref(),
                        );
                    }
                    match shutdown_error {
                        Some(error) => {
                            Err(anyhow!("{reason}; forced runtime shutdown failed: {error}"))
                        }
                        None => Err(anyhow!(reason)),
                    }
                }
            };
            if let Err(error) = &result {
                eprintln!("[mink-server] turn task ended abnormally: {error:#}");
            }
            result
        }));
        Ok(())
    }

    pub fn session_id(&self) -> String {
        self.state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .runtime
            .as_ref()
            .map(|runtime| runtime.session_info().session_id.clone())
            .unwrap_or_default()
    }

    pub fn interrupt(&self) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if matches!(
            state.phase,
            RuntimePhase::Running | RuntimePhase::Cancelling
        ) {
            state.phase = RuntimePhase::Cancelling;
            if let Some(runtime) = &state.runtime {
                runtime.interrupt_current_turn();
            }
        }
    }

    pub async fn shutdown(&self) -> Result<()> {
        let (mut task, runtime) = {
            let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            if state.phase == RuntimePhase::Closed {
                return Ok(());
            }
            state.phase = RuntimePhase::Closing;
            if let Some(runtime) = &state.runtime {
                runtime.interrupt_current_turn();
            }
            (state.turn_task.take(), state.runtime.take())
        };
        let turn_join_grace = if runtime.is_none() {
            // The timeout cleanup task may already own AgentRuntime::shutdown,
            // whose own bounded turn/actor cleanup can exceed five seconds.
            OWNED_SHUTDOWN_JOIN_GRACE
        } else {
            TURN_SHUTDOWN_GRACE
        };
        let mut errors = Vec::new();
        if let Some(handle) = task.as_mut() {
            match tokio::time::timeout(turn_join_grace, handle).await {
                Ok(result) => {
                    task = None;
                    record_turn_join(result, &mut errors);
                }
                Err(_) => {
                    errors.push(format!(
                        "turn did not stop within {}s",
                        turn_join_grace.as_secs()
                    ));
                }
            }
        }
        let shutdown_error = if let Some(runtime) = runtime {
            runtime
                .shutdown()
                .await
                .err()
                .map(|error| error.to_string())
        } else {
            None
        };
        if let Some(error) = &shutdown_error {
            errors.push(format!("runtime shutdown failed: {error}"));
        }
        if let Some(mut handle) = task {
            match tokio::time::timeout(TURN_JOIN_GRACE, &mut handle).await {
                Ok(result) => record_turn_join(result, &mut errors),
                Err(_) => {
                    handle.abort();
                    match handle.await {
                        Err(error) if error.is_cancelled() => {}
                        result => record_turn_join(result, &mut errors),
                    }
                    errors.push("turn task required forced abort after runtime shutdown".into());
                }
            }
        }
        finalize_shutdown(
            &self.state,
            &self.event_tx,
            &self.stream_sequence,
            shutdown_error.as_deref(),
        );
        if errors.is_empty() {
            Ok(())
        } else {
            Err(anyhow!(errors.join("; ")))
        }
    }
}

fn finalize_shutdown(
    state: &Arc<Mutex<RuntimeState>>,
    tx: &broadcast::Sender<String>,
    stream_sequence: &AtomicU64,
    shutdown_error: Option<&str>,
) {
    let terminal = {
        let mut state = state.lock().unwrap_or_else(|e| e.into_inner());
        state.phase = RuntimePhase::Closed;
        state.forced_terminal.take()
    };
    let Some(terminal) = terminal else { return };
    let error = match shutdown_error {
        Some(shutdown_error) => format!(
            "{}; forced runtime shutdown failed: {shutdown_error}",
            terminal.reason
        ),
        None => terminal.reason,
    };
    publish_forced_timeout_final(
        tx,
        stream_sequence,
        &terminal.turn_id,
        &terminal.session,
        &error,
        terminal.saw_stop,
        terminal.saw_final,
    );
}

fn record_turn_join(
    result: std::result::Result<Result<()>, tokio::task::JoinError>,
    errors: &mut Vec<String>,
) {
    match result {
        Ok(Ok(())) => {}
        Ok(Err(error)) => errors.push(format!("turn task failed: {error:#}")),
        Err(error) => errors.push(format!("turn task panicked: {error}")),
    }
}

fn agent_event_to_json(event: &AgentEvent, stream_sequence: u64) -> serde_json::Value {
    let mut value = serde_json::to_value(event)
        .unwrap_or_else(|_| serde_json::json!({"type":"serialization_error"}));
    value["stream_sequence"] = stream_sequence.into();
    if value.get("type").and_then(serde_json::Value::as_str) == Some("final") {
        value["type"] = "turn_final".into();
    }
    value
}

async fn run_turn_guarded(
    tx: &broadcast::Sender<String>,
    stream_sequence: &AtomicU64,
    state: &Arc<Mutex<RuntimeState>>,
    mut stream: AgentEventStream,
    session: SessionInfo,
) -> GuardedTurnResult {
    let timeout_secs = std::env::var("MINK_SERVER_TURN_TIMEOUT")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(1200);
    let deadline = tokio::time::sleep(std::time::Duration::from_secs(timeout_secs));
    tokio::pin!(deadline);
    let mut saw_stop = false;
    let mut saw_final = false;
    let turn_id = stream.turn_id().to_string();
    loop {
        tokio::select! {
            event = stream.recv() => match event {
                Some(event) => {
                    saw_stop |= matches!(&event.kind, mink::runtime::AgentEventKind::Stop { .. });
                    saw_final |= matches!(&event.kind, mink::runtime::AgentEventKind::Final { .. });
                    let sequence = stream_sequence.fetch_add(1, Ordering::Relaxed);
                    let _ = tx.send(agent_event_to_json(&event, sequence).to_string());
                }
                None => {
                    return GuardedTurnResult::Finished(stream.outcome().await.map(|_| ()).map_err(Into::into));
                },
            },
            _ = &mut deadline => {
                {
                    let mut state = state.lock().unwrap_or_else(|e| e.into_inner());
                    if state.phase == RuntimePhase::Running {
                        state.phase = RuntimePhase::Cancelling;
                    }
                }
                stream.cancel();
                let sequence = stream_sequence.fetch_add(1, Ordering::Relaxed);
                let _ = tx.send(serde_json::json!({
                    "type":"turn_error",
                    "stream_sequence": sequence,
                    "error":"turn timed out"
                }).to_string());
                break;
            }
        }
    }

    let grace = async {
        while let Some(event) = stream.recv().await {
            saw_stop |= matches!(&event.kind, mink::runtime::AgentEventKind::Stop { .. });
            saw_final |= matches!(&event.kind, mink::runtime::AgentEventKind::Final { .. });
            let sequence = stream_sequence.fetch_add(1, Ordering::Relaxed);
            let _ = tx.send(agent_event_to_json(&event, sequence).to_string());
        }
        stream.outcome().await
    };
    match tokio::time::timeout(TURN_SHUTDOWN_GRACE, grace).await {
        Ok(Ok(outcome)) => GuardedTurnResult::Finished(Err(anyhow!(
            "turn timed out after {timeout_secs}s (ended as {:?})",
            outcome.status
        ))),
        Ok(Err(error)) => GuardedTurnResult::Finished(Err(anyhow!(
            "turn timed out after {timeout_secs}s and cancellation failed: {error}"
        ))),
        Err(_) => GuardedTurnResult::ForceClose {
            reason: format!(
                "turn timed out after {timeout_secs}s and did not stop within {}s",
                TURN_SHUTDOWN_GRACE.as_secs()
            ),
            saw_stop,
            saw_final,
            turn_id,
            session: Box::new(session),
        },
    }
}

fn publish_forced_timeout_final(
    tx: &broadcast::Sender<String>,
    stream_sequence: &AtomicU64,
    turn_id: &str,
    session: &SessionInfo,
    error: &str,
    saw_stop: bool,
    saw_final: bool,
) {
    if !saw_stop {
        let sequence = stream_sequence.fetch_add(1, Ordering::Relaxed);
        let _ = tx.send(
            serde_json::json!({
                "type": "stop",
                "turn_id": turn_id,
                "reason": "timeout",
                "stream_sequence": sequence,
            })
            .to_string(),
        );
    }
    if !saw_final {
        let sequence = stream_sequence.fetch_add(1, Ordering::Relaxed);
        let _ = tx.send(
            serde_json::json!({
            "type": "turn_final",
            "turn_id": turn_id,
            "stream_sequence": sequence,
            "outcome": {
                "turn_id": turn_id,
                "billing_turn_id": turn_id,
                "status": "failed",
                "session": session,
                "text": "",
                "thinking": "",
                "tool_call_count": 0,
                "tool_error_count": 1,
                "error": error,
                "usage_records": [],
                "usage": {
                    "request_count": 0,
                    "reported_request_count": 0,
                    "unreported_request_count": 0,
                    "attempt_count": 0,
                    "tokens": {
                        "input_tokens": 0,
                        "cache_read_tokens": 0,
                        "cache_creation_tokens": 0,
                        "output_tokens": 0
                    }
                }
            }
            })
            .to_string(),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session_info() -> SessionInfo {
        let path = std::path::PathBuf::from("/tmp/mink-server-runtime-test");
        SessionInfo {
            session_id: "session".into(),
            session_ref: "session".into(),
            is_new: false,
            home: path.clone(),
            cwd: path.clone(),
            events_path: path.join("events.jsonl"),
            conversation_path: path.join("conversation.jsonl"),
            artifacts_dir: path.join("artifacts"),
            summary_path: path.join("summary.md"),
            usage_path: path.join("usage.jsonl"),
            plan_path: path.join("plan.md"),
            plan_draft_path: path.join("plan.draft"),
            todos_path: path.join("todos.json"),
        }
    }

    #[test]
    fn forced_terminal_is_published_once_after_closed() {
        let (tx, mut rx) = broadcast::channel(8);
        let sequence = AtomicU64::new(1);
        let state = Arc::new(Mutex::new(RuntimeState {
            runtime: None,
            phase: RuntimePhase::Closing,
            turn_task: None,
            last_idle: Instant::now(),
            forced_terminal: Some(ForcedTerminal {
                reason: "turn timed out".into(),
                saw_stop: false,
                saw_final: false,
                turn_id: "turn".into(),
                session: Box::new(session_info()),
            }),
        }));

        finalize_shutdown(&state, &tx, &sequence, Some("shutdown error"));
        assert_eq!(state.lock().unwrap().phase, RuntimePhase::Closed);
        let stop: serde_json::Value = serde_json::from_str(&rx.try_recv().unwrap()).unwrap();
        let final_event: serde_json::Value = serde_json::from_str(&rx.try_recv().unwrap()).unwrap();
        assert_eq!(stop["type"], "stop");
        assert_eq!(final_event["type"], "turn_final");
        assert_eq!(final_event["outcome"]["status"], "failed");
        assert!(
            final_event["outcome"]["error"]
                .as_str()
                .unwrap()
                .contains("shutdown error")
        );

        finalize_shutdown(&state, &tx, &sequence, None);
        assert!(rx.try_recv().is_err());
    }
}
