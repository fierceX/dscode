use crate::tui::signal::TuiSignal;
use crate::ui::{
    Display, PresentedToolResultDisplay, StatsSnapshot, SubAgentStreamKind, SubAgentStreamSink,
    ToolCallDisplay,
};
use std::sync::mpsc;

pub struct TuiDisplay {
    tx: mpsc::Sender<TuiSignal>,
}

pub struct TuiSubAgentStreamSink {
    tx: mpsc::Sender<TuiSignal>,
}

impl TuiDisplay {
    pub fn new(tx: mpsc::Sender<TuiSignal>) -> Self {
        Self { tx }
    }
}

impl TuiSubAgentStreamSink {
    pub fn new(tx: mpsc::Sender<TuiSignal>) -> Self {
        Self { tx }
    }
}

impl Display for TuiDisplay {
    fn render_thinking(&self, c: &str) {
        let _ = self.tx.send(TuiSignal::Thinking(c.into()));
    }

    fn render_text(&self, c: &str) {
        let _ = self.tx.send(TuiSignal::Text(c.into()));
    }

    fn render_tool_call(&self, call: &ToolCallDisplay<'_>) {
        let _ = self.tx.send(TuiSignal::ToolCall {
            tool_use_id: Some(call.tool_use_id.into()),
            tool_name: call.tool_name.into(),
            summary: call.summary.into(),
        });
    }

    fn render_tool_result(&self, result: &PresentedToolResultDisplay<'_>) {
        let _ = self.tx.send(TuiSignal::ToolResult {
            tool_use_id: result.base.tool_use_id.map(str::to_owned),
            tool_name: result.base.tool_name.into(),
            content: result.base.content.into(),
            success: result.status.is_success(),
            exit_code: result.base.exit_code,
            result_kind: result.result_kind,
            presentation: result.presentation.cloned(),
            artifacts: result.artifacts.to_vec(),
        });
    }

    fn render_stop(&self, _reason: &str) {
        let _ = self.tx.send(TuiSignal::Stop);
    }

    fn render_error(&self, m: &str) {
        let _ = self.tx.send(TuiSignal::Error(m.into()));
    }

    fn render_retry(&self) {
        let _ = self.tx.send(TuiSignal::Retry);
    }

    fn render_signal(&self, _signal_kind: &str, _severity: f64, _message: &str) {}

    fn render_info(&self, m: &str) {
        let _ = self.tx.send(TuiSignal::Info(m.into()));
    }

    fn render_title_update(&self, m: &str, s: &StatsSnapshot) {
        let _ = self.tx.send(TuiSignal::TitleUpdate(m.into(), s.clone()));
    }

    fn render_sub_agent_status(&self, sid: &str, st: &str, it: u64, ot: u64) {
        let _ = self.tx.send(TuiSignal::SubAgentStatus {
            session_id: sid.into(),
            status: st.into(),
            in_tokens: it,
            out_tokens: ot,
        });
    }

    fn render_sub_agent_output(
        &self,
        sid: &str,
        st: &str,
        thinking: &str,
        text: &str,
        it: u64,
        ot: u64,
    ) {
        let _ = self.tx.send(TuiSignal::SubAgentOutput {
            session_id: sid.into(),
            status: st.into(),
            thinking: thinking.into(),
            text: text.into(),
            in_tokens: it,
            out_tokens: ot,
        });
    }

    fn render_prompt(&self) {}

    fn render_clear_line(&self) {}
}

impl SubAgentStreamSink for TuiSubAgentStreamSink {
    fn render_sub_agent_stream(&self, session_id: &str, kind: SubAgentStreamKind, content: &str) {
        let _ = self.tx.send(TuiSignal::SubAgentStream {
            session_id: session_id.into(),
            kind,
            content: content.into(),
        });
    }
}
