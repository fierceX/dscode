use crate::tui::signal::TuiSignal;
use crate::ui::{
    Display, PresentedToolResultDisplay, StatsSnapshot, SubAgentStreamKind, SubAgentStreamSink,
    ToolCallDisplay, ToolResultDisplay,
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
        let _ = self.tx.send(TuiSignal::ToolCall {
            tool_use_id: None,
            tool_name: n.into(),
            summary: s.into(),
        });
    }

    fn render_tool_call_detail(&self, call: &ToolCallDisplay<'_>) {
        let _ = self.tx.send(TuiSignal::ToolCall {
            tool_use_id: Some(call.tool_use_id.into()),
            tool_name: call.tool_name.into(),
            summary: call.summary.into(),
        });
    }

    fn render_tool_result(&self, n: &str, c: &str) {
        let _ = self.tx.send(TuiSignal::ToolResult {
            tool_use_id: None,
            tool_name: n.into(),
            content: c.into(),
            success: true,
            exit_code: None,
            result_kind: crate::ui::ToolResultKind::Text,
            presentation: None,
            artifacts: Vec::new(),
        });
    }

    fn render_tool_result_detail(&self, result: &ToolResultDisplay<'_>) {
        let _ = self.tx.send(TuiSignal::ToolResult {
            tool_use_id: result.tool_use_id.map(str::to_owned),
            tool_name: result.tool_name.into(),
            content: result.content.into(),
            success: result.exit_code.is_none_or(|code| code == 0),
            exit_code: result.exit_code,
            result_kind: crate::ui::ToolResultKind::Text,
            presentation: None,
            artifacts: Vec::new(),
        });
    }

    fn render_tool_result_presented(&self, result: &PresentedToolResultDisplay<'_>) {
        let _ = self.tx.send(TuiSignal::ToolResult {
            tool_use_id: result.base.tool_use_id.map(str::to_owned),
            tool_name: result.base.tool_name.into(),
            content: result.base.content.into(),
            success: result.success,
            exit_code: result.base.exit_code,
            result_kind: result.result_kind,
            presentation: result.presentation.cloned(),
            artifacts: result.artifacts.to_vec(),
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
