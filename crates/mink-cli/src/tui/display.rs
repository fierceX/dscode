use crate::tui::signal::TuiSignal;
use crate::ui::{
    Display, StatsSnapshot, SubAgentStreamKind, SubAgentStreamSink, ToolResultDisplay,
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

    fn render_tool_call(&self, n: &str, s: &str) {
        let _ = self.tx.send(TuiSignal::ToolCall(n.into(), s.into()));
    }

    fn render_tool_result(&self, n: &str, c: &str) {
        let _ = self.tx.send(TuiSignal::ToolResult {
            tool_name: n.into(),
            content: c.into(),
        });
    }

    fn render_tool_result_detail(&self, result: &ToolResultDisplay<'_>) {
        let _ = self.tx.send(TuiSignal::ToolResult {
            tool_name: result.tool_name.into(),
            content: result.content.into(),
        });
    }

    fn render_stop(&self) {
        let _ = self.tx.send(TuiSignal::Stop);
    }

    fn render_error(&self, m: &str) {
        let _ = self.tx.send(TuiSignal::Error(m.into()));
    }

    fn render_retry(&self) {
        let _ = self.tx.send(TuiSignal::Retry);
    }

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
