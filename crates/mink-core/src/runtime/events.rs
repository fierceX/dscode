use crate::runtime::TurnOutcome;
use crate::ui::{Display, StatsSnapshot, ToolResultDisplay};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

#[derive(Debug, Clone)]
pub enum AgentEvent {
    Thinking {
        content: String,
    },
    Text {
        content: String,
    },
    ToolCall {
        name: String,
        summary: String,
    },
    ToolResult {
        tool_name: String,
        content_preview: String,
        content: String,
        tool_use_id: Option<String>,
        exit_code: Option<i32>,
    },
    Stop,
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

pub trait EventSink: Send + Sync {
    fn on_event(&self, event: AgentEvent);
}

pub(crate) struct EventDisplay {
    sink: Option<Arc<dyn EventSink>>,
    delegate: Option<Arc<dyn Display>>,
    next_turn_subscription_id: AtomicU64,
    turn_tx: std::sync::Mutex<Option<TurnEventChannel>>,
}

struct TurnEventChannel {
    id: u64,
    tx: tokio::sync::mpsc::UnboundedSender<AgentEvent>,
}

pub(crate) struct TurnEventSubscription {
    display: Arc<EventDisplay>,
    id: u64,
}

impl EventDisplay {
    pub(crate) fn new(
        sink: Option<Arc<dyn EventSink>>,
        delegate: Option<Arc<dyn Display>>,
    ) -> Self {
        Self {
            sink,
            delegate,
            next_turn_subscription_id: AtomicU64::new(1),
            turn_tx: std::sync::Mutex::new(None),
        }
    }

    pub(crate) fn subscribe_turn(
        self: &Arc<Self>,
        tx: tokio::sync::mpsc::UnboundedSender<AgentEvent>,
    ) -> TurnEventSubscription {
        let id = self
            .next_turn_subscription_id
            .fetch_add(1, Ordering::Relaxed);
        *self.turn_tx.lock().unwrap() = Some(TurnEventChannel { id, tx });
        TurnEventSubscription {
            display: self.clone(),
            id,
        }
    }

    fn clear_turn_channel(&self, id: u64) {
        let mut turn_tx = self.turn_tx.lock().unwrap();
        if turn_tx.as_ref().is_some_and(|channel| channel.id == id) {
            *turn_tx = None;
        }
    }

    fn emit(&self, event: AgentEvent) {
        let turn_tx = self
            .turn_tx
            .lock()
            .unwrap()
            .as_ref()
            .map(|channel| channel.tx.clone());
        if let Some(tx) = turn_tx {
            let _ = tx.send(event.clone());
        }
        if let Some(sink) = &self.sink {
            sink.on_event(event);
        }
    }
}

impl Drop for TurnEventSubscription {
    fn drop(&mut self) {
        self.display.clear_turn_channel(self.id);
    }
}

impl Display for EventDisplay {
    fn render_thinking(&self, content: &str) {
        self.emit(AgentEvent::Thinking {
            content: content.to_string(),
        });
        if let Some(delegate) = &self.delegate {
            delegate.render_thinking(content);
        }
    }

    fn render_text(&self, content: &str) {
        self.emit(AgentEvent::Text {
            content: content.to_string(),
        });
        if let Some(delegate) = &self.delegate {
            delegate.render_text(content);
        }
    }

    fn render_tool_call(&self, name: &str, summary: &str) {
        self.emit(AgentEvent::ToolCall {
            name: name.to_string(),
            summary: summary.to_string(),
        });
        if let Some(delegate) = &self.delegate {
            delegate.render_tool_call(name, summary);
        }
    }

    fn render_tool_result(&self, tool_name: &str, content_preview: &str) {
        self.emit(AgentEvent::ToolResult {
            tool_name: tool_name.to_string(),
            content_preview: content_preview.to_string(),
            content: content_preview.to_string(),
            tool_use_id: None,
            exit_code: None,
        });
        if let Some(delegate) = &self.delegate {
            delegate.render_tool_result(tool_name, content_preview);
        }
    }

    fn render_tool_result_detail(&self, result: &ToolResultDisplay<'_>) {
        self.emit(AgentEvent::ToolResult {
            tool_name: result.tool_name.to_string(),
            content_preview: result.content_preview.to_string(),
            content: result.content.to_string(),
            tool_use_id: result.tool_use_id.map(ToString::to_string),
            exit_code: result.exit_code,
        });
        if let Some(delegate) = &self.delegate {
            delegate.render_tool_result_detail(result);
        }
    }

    fn render_stop(&self) {
        self.emit(AgentEvent::Stop);
        if let Some(delegate) = &self.delegate {
            delegate.render_stop();
        }
    }

    fn render_error(&self, message: &str) {
        self.emit(AgentEvent::Error {
            message: message.to_string(),
        });
        if let Some(delegate) = &self.delegate {
            delegate.render_error(message);
        }
    }

    fn render_retry(&self) {
        self.emit(AgentEvent::Retry);
        if let Some(delegate) = &self.delegate {
            delegate.render_retry();
        }
    }

    fn render_info(&self, msg: &str) {
        self.emit(AgentEvent::Info {
            message: msg.to_string(),
        });
        if let Some(delegate) = &self.delegate {
            delegate.render_info(msg);
        }
    }

    fn render_title_update(&self, model: &str, stats: &StatsSnapshot) {
        self.emit(AgentEvent::TitleUpdate {
            model: model.to_string(),
            stats: stats.clone(),
        });
        if let Some(delegate) = &self.delegate {
            delegate.render_title_update(model, stats);
        }
    }

    fn render_sub_agent_status(
        &self,
        session_id: &str,
        status: &str,
        in_tokens: u64,
        out_tokens: u64,
    ) {
        self.emit(AgentEvent::SubAgentStatus {
            session_id: session_id.to_string(),
            status: status.to_string(),
            in_tokens,
            out_tokens,
        });
        if let Some(delegate) = &self.delegate {
            delegate.render_sub_agent_status(session_id, status, in_tokens, out_tokens);
        }
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
        self.emit(AgentEvent::SubAgentOutput {
            session_id: session_id.to_string(),
            status: status.to_string(),
            thinking: thinking.to_string(),
            text: text.to_string(),
            in_tokens,
            out_tokens,
        });
        if let Some(delegate) = &self.delegate {
            delegate
                .render_sub_agent_output(session_id, status, thinking, text, in_tokens, out_tokens);
        }
    }

    fn render_prompt(&self) {
        self.emit(AgentEvent::Prompt);
        if let Some(delegate) = &self.delegate {
            delegate.render_prompt();
        }
    }

    fn render_clear_line(&self) {
        self.emit(AgentEvent::ClearLine);
        if let Some(delegate) = &self.delegate {
            delegate.render_clear_line();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[derive(Default)]
    struct RecordingSink {
        events: Mutex<Vec<AgentEvent>>,
    }

    impl EventSink for RecordingSink {
        fn on_event(&self, event: AgentEvent) {
            self.events.lock().unwrap().push(event);
        }
    }

    #[test]
    fn event_display_maps_display_calls() {
        let sink = Arc::new(RecordingSink::default());
        let display = EventDisplay::new(Some(sink.clone()), None);

        display.render_text("hello");
        display.render_tool_result_detail(&ToolResultDisplay {
            tool_name: "Bash",
            content_preview: "ok",
            content: "full",
            tool_use_id: Some("call_1"),
            exit_code: Some(0),
        });

        let events = sink.events.lock().unwrap();
        assert!(matches!(
            &events[0],
            AgentEvent::Text { content } if content == "hello"
        ));
        assert!(matches!(
            &events[1],
            AgentEvent::ToolResult {
                tool_name,
                content_preview,
                content,
                tool_use_id,
                exit_code,
            } if tool_name == "Bash"
                && content_preview == "ok"
                && content == "full"
                && tool_use_id.as_deref() == Some("call_1")
                && *exit_code == Some(0)
        ));
    }

    /// Verify every Display method maps to the correct AgentEvent variant.
    /// This is the contract that ensures TurnExecutor events — which flow
    /// through Display — are fully observable by Rust callers via EventSink.
    #[test]
    fn all_display_methods_have_event_variants() {
        let sink = Arc::new(RecordingSink::default());
        let display = EventDisplay::new(Some(sink.clone()), None);

        display.render_thinking("think");
        display.render_text("answer");
        display.render_tool_call("Bash", "ls -la");
        display.render_tool_result("Grep", "found 3 matches");
        display.render_tool_result_detail(&ToolResultDisplay {
            tool_name: "Read",
            content_preview: "preview",
            content: "full content",
            tool_use_id: Some("call_2"),
            exit_code: Some(0),
        });
        display.render_stop();
        display.render_error("something broke");
        display.render_retry();
        display.render_info("compressing...");
        display.render_title_update("pro", &StatsSnapshot::default());
        display.render_sub_agent_status("sub-1", "running", 100, 50);
        display.render_sub_agent_output("sub-2", "done", "hmm", "yes", 200, 80);
        display.render_prompt();
        display.render_clear_line();

        let events = sink.events.lock().unwrap();
        assert!(
            matches!(&events[0], AgentEvent::Thinking { content } if content == "think"),
            "Thinking"
        );
        assert!(
            matches!(&events[1], AgentEvent::Text { content } if content == "answer"),
            "Text"
        );
        assert!(
            matches!(&events[2], AgentEvent::ToolCall { name, summary } if name == "Bash" && summary == "ls -la"),
            "ToolCall"
        );
        assert!(
            matches!(&events[3], AgentEvent::ToolResult { tool_name, .. } if tool_name == "Grep"),
            "ToolResult render_tool_result"
        );
        assert!(
            matches!(&events[4], AgentEvent::ToolResult { tool_name, tool_use_id, .. }
                if tool_name == "Read" && tool_use_id.as_deref() == Some("call_2")),
            "ToolResult render_tool_result_detail"
        );
        assert!(matches!(&events[5], AgentEvent::Stop), "Stop");
        assert!(
            matches!(&events[6], AgentEvent::Error { message } if message == "something broke"),
            "Error"
        );
        assert!(matches!(&events[7], AgentEvent::Retry), "Retry");
        assert!(
            matches!(&events[8], AgentEvent::Info { message } if message == "compressing..."),
            "Info"
        );
        assert!(
            matches!(&events[9], AgentEvent::TitleUpdate { model, .. } if model == "pro"),
            "TitleUpdate"
        );
        assert!(
            matches!(&events[10], AgentEvent::SubAgentStatus { session_id, status, .. }
                if session_id == "sub-1" && status == "running"),
            "SubAgentStatus"
        );
        assert!(
            matches!(&events[11], AgentEvent::SubAgentOutput { session_id, text, .. }
                if session_id == "sub-2" && text == "yes"),
            "SubAgentOutput"
        );
        assert!(matches!(&events[12], AgentEvent::Prompt), "Prompt");
        assert!(matches!(&events[13], AgentEvent::ClearLine), "ClearLine");
    }
}
