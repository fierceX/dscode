#[cfg(test)]
pub use mink::runtime::ToolResultDisplay;
#[cfg(feature = "tui")]
pub use mink::runtime::{
    ArtifactDisplay, PlanDisplay, PlanTransitionDisplay, SubAgentStreamKind, TodoChangeDisplay,
    TodoCountsDisplay, TodoDisplay, TodoItemDisplay, TodoStatusDisplay, ToolPresentation,
    ToolResultKind,
};
pub use mink::runtime::{
    PresentedToolResultDisplay, StatsSnapshot, SubAgentStreamSink, ToolCallDisplay,
};

pub trait Display: Send + Sync {
    fn render_thinking(&self, content: &str);
    fn render_text(&self, content: &str);
    fn render_tool_call(&self, call: &ToolCallDisplay<'_>);
    fn render_tool_result(&self, result: &PresentedToolResultDisplay<'_>);
    fn render_stop(&self, reason: &str);
    fn render_signal(&self, signal_kind: &str, severity: f64, message: &str);
    fn render_error(&self, message: &str);
    fn render_retry(&self);
    fn render_info(&self, message: &str);
    fn render_title_update(&self, model: &str, stats: &StatsSnapshot);
    fn render_sub_agent_status(
        &self,
        session_id: &str,
        status: &str,
        in_tokens: u64,
        out_tokens: u64,
    );
    fn render_sub_agent_output(
        &self,
        session_id: &str,
        status: &str,
        thinking: &str,
        text: &str,
        in_tokens: u64,
        out_tokens: u64,
    );
    fn render_prompt(&self);
    fn render_clear_line(&self);
}

pub mod engine;
pub mod replay;
