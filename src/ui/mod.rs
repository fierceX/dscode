pub mod engine;
pub mod replay;

/// Display abstracts terminal rendering. Two implementations:
/// - TerminalDisplay (REPL) — synchronous writes to stdout/stderr
/// - TuiDisplay (future) — event-driven TUI via mpsc channel
pub trait Display: Send + Sync {
    fn render_thinking(&self, content: &str);
    fn render_text(&self, content: &str);
    fn render_tool_call(&self, name: &str, summary: &str);
    fn render_tool_result(&self, tool_name: &str, content_preview: &str);
    fn render_stop(&self);
    fn render_error(&self, message: &str);
    fn render_retry(&self);
    fn render_info(&self, msg: &str);
    fn render_title_update(&self, model: &str, stats: &StatsSnapshot);
    fn render_sub_agent_status(&self, session_id: &str, status: &str, in_tokens: u64, out_tokens: u64);
    fn render_prompt(&self);
    fn render_clear_line(&self);
}

#[derive(Debug, Clone, Default)]
pub struct StatsSnapshot {
    pub current_turn_count: u64,
    pub agent_request_count: u64,
    pub total_input_tokens: u64,
    pub total_output_tokens: u64,
    pub current_context_tokens: u64,
    pub max_context_tokens: u64,
    pub total_cache_read_tokens: u64,
}

impl StatsSnapshot {
    pub fn cache_pct(&self) -> String {
        let total = self.total_input_tokens + self.total_cache_read_tokens;
        if total > 0 {
            format!("{}%", self.total_cache_read_tokens * 100 / total)
        } else {
            "—".to_string()
        }
    }

    pub fn ctx_pct(&self) -> String {
        if self.max_context_tokens > 0 {
            let pct = self.current_context_tokens * 100 / self.max_context_tokens;
            format!("{}%", pct)
        } else {
            "—".to_string()
        }
    }

    pub fn fmt_num(n: u64) -> String {
        let s = n.to_string();
        let mut buf = String::with_capacity(s.len() + s.len() / 3);
        let off = s.len() % 3;
        let first = if off == 0 { 3 } else { off };
        buf.push_str(&s[..first]);
        for i in (first..s.len()).step_by(3) {
            buf.push(',');
            buf.push_str(&s[i..i + 3]);
        }
        buf
    }
}
