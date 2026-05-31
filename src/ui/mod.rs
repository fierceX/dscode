pub mod engine;
pub mod replay;

/// Display abstracts terminal rendering. Two implementations:
/// - TerminalDisplay (REPL) — synchronous writes to stdout/stderr
/// - TuiDisplay (future) — event-driven TUI via mpsc channel
pub struct ToolResultDisplay<'a> {
    pub tool_name: &'a str,
    pub content_preview: &'a str,
    /// Full display content after tool-level truncation/noise filtering.
    ///
    /// This is not raw unbounded process output; `ToolRunner` has already applied
    /// configured max-byte truncation before this reaches the UI layer.
    pub content: &'a str,
    pub tool_use_id: Option<&'a str>,
    pub exit_code: Option<i32>,
}

pub trait Display: Send + Sync {
    fn render_thinking(&self, content: &str);
    fn render_text(&self, content: &str);
    fn render_tool_call(&self, name: &str, summary: &str);
    fn render_tool_result(&self, tool_name: &str, content_preview: &str);
    fn render_tool_result_detail(&self, result: &ToolResultDisplay<'_>) {
        self.render_tool_result(result.tool_name, result.content_preview);
    }
    fn render_stop(&self);
    fn render_error(&self, message: &str);
    fn render_retry(&self);
    fn render_info(&self, msg: &str);
    fn render_title_update(&self, model: &str, stats: &StatsSnapshot);
    fn render_sub_agent_status(
        &self,
        session_id: &str,
        status: &str,
        in_tokens: u64,
        out_tokens: u64,
    );
    /// Sub-agent complete output (thinking + text), sent after execution.
    /// Implementations: TUI stores for click-to-view detail; REPL prints directly.
    fn render_sub_agent_output(
        &self,
        _session_id: &str,
        _status: &str,
        _thinking: &str,
        _text: &str,
        _in_tokens: u64,
        _out_tokens: u64,
    ) {
    }
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
    pub total_cache_creation_tokens: u64,
    pub flash_cost_micros: u64,
    pub pro_cost_micros: u64,
    /// 信念度 B ∈ [0, 1]。0.0 表示未追踪
    pub belief: f64,
}

impl StatsSnapshot {
    fn cost_micros(&self) -> u64 {
        self.flash_cost_micros + self.pro_cost_micros
    }
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

    pub fn format_cost(&self) -> String {
        let micros = self.cost_micros();
        if micros < 1_000 {
            "¥0.00".to_string()
        } else if micros < 1_000_000 {
            format!("¥{:.3}", micros as f64 / 1_000_000.0)
        } else {
            format!("¥{:.2}", micros as f64 / 1_000_000.0)
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

pub async fn render_title_snapshot(
    ctx: &crate::context::AgentSharedContext,
    model_label: &str,
    belief: f64,
) {
    let stats = ctx.stats.snapshot().await;
    let snapshot = StatsSnapshot {
        current_turn_count: stats.current_turn_count,
        agent_request_count: stats.agent_request_count,
        total_input_tokens: stats.total_input_tokens,
        total_output_tokens: stats.total_output_tokens,
        current_context_tokens: stats.current_context_tokens,
        max_context_tokens: ctx.config.max_context_tokens as u64,
        total_cache_read_tokens: stats.total_cache_read_tokens,
        total_cache_creation_tokens: stats.total_cache_creation_tokens,
        flash_cost_micros: stats.flash_cost_micros,
        pro_cost_micros: stats.pro_cost_micros,
        belief,
    };
    ctx.display.render_title_update(model_label, &snapshot);
}
