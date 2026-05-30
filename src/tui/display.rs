use crate::tui::signal::TuiSignal;
use crate::ui::{Display, StatsSnapshot};
use std::sync::mpsc;

pub struct TuiDisplay {
    tx: mpsc::Sender<TuiSignal>,
}

impl TuiDisplay {
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

    fn render_tool_result(&self, _: &str, c: &str) {
        let _ = self.tx.send(TuiSignal::ToolResult(c.into()));
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
