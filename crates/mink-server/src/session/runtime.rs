//! Per-session runtime wrapper: start/stream/interrupt/shutdown with a
//! running flag. 事件流直接从嵌入的 mink 库消费（try_stream_turn 的
//! AgentEvent 流），经广播通道转手给 SSE——服务层不判断轮次/seq 逻辑。

use anyhow::Result;
use mink::runtime::{AgentEvent, AgentOptions, AgentRuntime};
use serde_json::json;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::sync::broadcast;

pub struct SessionRuntime {
    runtime: Arc<AgentRuntime>,
    running: Arc<AtomicBool>,
    event_tx: broadcast::Sender<String>,
}

impl SessionRuntime {
    /// Build and start a runtime for the given options.
    pub async fn open(options: AgentOptions) -> Result<Self> {
        let runtime = Arc::new(AgentRuntime::start_with_options(options).await?);
        let (event_tx, _) = broadcast::channel(1024);
        Ok(Self {
            runtime,
            running: Arc::new(AtomicBool::new(false)),
            event_tx,
        })
    }

    pub fn running(&self) -> bool {
        self.running.load(Ordering::SeqCst)
    }

    /// 订阅实时事件流（SSE 长连接从这里取数据，纯转手）
    pub fn event_receiver(&self) -> broadcast::Receiver<String> {
        self.event_tx.subscribe()
    }

    /// Submit a user input. The turn runs on its own task; 每个 AgentEvent
    /// 转为 SSE JSON 帧广播（不经 events.jsonl 轮询）。Returns an error
    /// when a turn is already in progress on this runtime.
    pub fn start_turn(&self, input: String) -> Result<()> {
        let stream = self.runtime.try_stream_turn(input)?;
        self.running.store(true, Ordering::SeqCst);
        let running = self.running.clone();
        let tx = self.event_tx.clone();
        let runtime = self.runtime.clone();
        tokio::spawn(async move {
            let outcome = run_turn_guarded(&runtime, &tx, stream).await;
            // 无论正常/超时/panic，running 必须复位（否则会话永久卡死）
            running.store(false, Ordering::SeqCst);
            if let Err(e) = &outcome {
                eprintln!("[mink-server] turn task ended abnormally: {e}");
            }
        });
        Ok(())
    }

    /// Resolved session id (the core generates/derives it from the policy).
    pub fn session_id(&self) -> String {
        self.runtime.session_info().session_id.clone()
    }

    /// Cancel the in-flight turn (Ctrl+C semantics; idempotent when idle).
    pub fn interrupt(&self) {
        self.runtime.interrupt_current_turn();
    }

    pub async fn shutdown(self) -> Result<()> {
        match Arc::try_unwrap(self.runtime) {
            Ok(rt) => rt.shutdown().await,
            Err(_) => {
                // turn task 或并发 handler 仍持有引用（边缘情况）——记录并放弃
                eprintln!("[mink-server] runtime still referenced during shutdown");
                Ok(())
            }
        }
    }
}

/// AgentEvent（展示层事件）→ SSE JSON 帧。富字段（result_kind 等）不在
/// 该流中——前端按工具名兜底（与 TUI 语义一致）。
fn agent_event_to_json(ev: &AgentEvent) -> serde_json::Value {
    match ev {
        AgentEvent::Thinking { content } => json!({ "type": "thinking", "content": content }),
        AgentEvent::Text { content } => json!({ "type": "text", "content": content }),
        AgentEvent::ToolCall {
            id,
            name,
            summary,
            input,
        } => {
            json!({ "type": "tool_call", "id": id, "name": name, "summary": summary, "input": input })
        }
        AgentEvent::ToolResult {
            tool_name,
            content,
            exit_code,
            ..
        } => json!({
            "type": "tool_result",
            "name": tool_name,
            "content": content,
            "exit_code": exit_code,
        }),
        AgentEvent::Signal {
            signal_kind,
            severity,
            message,
        } => json!({
            "type": "signal",
            "signal_kind": signal_kind,
            "severity": severity,
            "message": message,
        }),
        AgentEvent::Stop { reason } => json!({ "type": "stop", "reason": reason }),
        AgentEvent::Retry => json!({ "type": "retry" }),
        AgentEvent::Error { message } => json!({ "type": "turn_error", "error": message }),
        AgentEvent::Info { message } => json!({ "type": "info", "message": message }),
        AgentEvent::TitleUpdate { model, stats } => json!({
            "type": "title_update",
            "model": model,
            "tokens_in": stats.total_input_tokens,
            "tokens_out": stats.total_output_tokens,
            "cost_micros": stats.flash_cost_micros + stats.pro_cost_micros,
            "belief": stats.belief,
            "cache_read": stats.total_cache_read_tokens + stats.total_cache_creation_tokens,
            "context_tokens": stats.current_context_tokens,
            "max_context": stats.max_context_tokens,
        }),
        AgentEvent::SubAgentStatus {
            session_id, status, ..
        } => {
            json!({ "type": "sub_agent", "session_id": session_id, "status": status })
        }
        AgentEvent::SubAgentOutput { .. } => json!({ "type": "sub_agent_output" }),
        _ => json!({ "type": "event" }),
    }
}

/// 执行 turn 事件循环：广播 AgentEvent + 超时保护（默认 20 分钟）。
/// panic 防御：核心 future 理论上不 panic，但任何异常都不能让
/// running 标志卡死——外层 catch_unwind 兜底。
async fn run_turn_guarded(
    runtime: &AgentRuntime,
    tx: &tokio::sync::broadcast::Sender<String>,
    stream: mink::runtime::AgentEventStream,
) -> anyhow::Result<()> {
    let timeout_secs = std::env::var("MINK_SERVER_TURN_TIMEOUT")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(1200);
    let _ = tx.send(r#"{"type":"turn_start","reason":"submit"}"#.to_string());
    let loop_fut = async {
        let mut stream = stream;
        while let Some(ev) = stream.recv().await {
            if tx.send(agent_event_to_json(&ev).to_string()).is_err() {
                // 无订阅者：继续排空，事件仍由核心持久化
            }
        }
        let _ = stream.outcome().await;
    };
    match tokio::time::timeout(std::time::Duration::from_secs(timeout_secs), loop_fut).await {
        Ok(()) => Ok(()),
        Err(_) => {
            // LLM/工具挂起：中断当前 turn（Ctrl+C 语义），广播超时提示
            runtime.interrupt_current_turn();
            let _ = tx.send(r#"{"type":"turn_error","error":"turn timed out"}"#.to_string());
            anyhow::bail!("turn timed out after {timeout_secs}s")
        }
    }
}
