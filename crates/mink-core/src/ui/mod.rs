pub use crate::tools::metadata::{ToolResultKind, ToolStatus};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtifactDisplay {
    pub id: String,
    pub tool: String,
    pub bytes: u64,
    pub description: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlanTransitionDisplay {
    DraftSaved,
    DraftCancelled,
    Confirmed,
    Cleared,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanDisplay {
    pub transition: PlanTransitionDisplay,
    pub content: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TodoStatusDisplay {
    Pending,
    InProgress,
    Completed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TodoItemDisplay {
    pub id: String,
    pub content: String,
    pub status: TodoStatusDisplay,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TodoCountsDisplay {
    pub pending: usize,
    pub in_progress: usize,
    pub completed: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "change", rename_all = "snake_case")]
pub enum TodoChangeDisplay {
    Added { item: TodoItemDisplay },
    Updated { id: String, content: String },
    Removed { id: String },
    Completed { id: String },
    Activated { id: String },
    Paused { id: String },
    Reopened { id: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TodoDisplay {
    pub revision: u64,
    pub counts: TodoCountsDisplay,
    pub items: Vec<TodoItemDisplay>,
    pub changes: Vec<TodoChangeDisplay>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "data", rename_all = "snake_case")]
pub enum ToolPresentation {
    Plan(PlanDisplay),
    Todo(TodoDisplay),
}

pub struct ToolCallDisplay<'a> {
    pub tool_use_id: &'a str,
    pub tool_name: &'a str,
    pub summary: &'a str,
    /// 完整调用参数（实时流透传给前端结构化渲染；不可得时为 None）
    pub input: Option<&'a serde_json::Value>,
}

/// Display abstracts agent output from any concrete terminal implementation.
///
/// `mink-core` owns only this protocol-level contract so embedded Rust services
/// can drive the runtime without depending on REPL/TUI crates. Concrete terminal
/// implementations such as `TerminalDisplay` and `TuiDisplay` live in
/// `mink-cli`.
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

pub struct PresentedToolResultDisplay<'a> {
    pub base: ToolResultDisplay<'a>,
    pub status: ToolStatus,
    pub result_kind: ToolResultKind,
    pub presentation: Option<&'a ToolPresentation>,
    pub artifacts: &'a [ArtifactDisplay],
}

pub trait Display: Send + Sync {
    fn render_thinking(&self, content: &str);
    fn render_text(&self, content: &str);
    fn render_tool_call(&self, call: &ToolCallDisplay<'_>);
    fn render_tool_result(&self, result: &PresentedToolResultDisplay<'_>);
    fn render_stop(&self, reason: &str);
    fn render_signal(&self, signal_kind: &str, severity: f64, message: &str);
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

#[derive(Clone, Copy, Debug)]
pub enum SubAgentStreamKind {
    Thinking,
    Text,
}

pub trait SubAgentStreamSink: Send + Sync {
    fn render_sub_agent_stream(&self, session_id: &str, kind: SubAgentStreamKind, content: &str);
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct StatsSnapshot {
    pub current_turn_count: u64,
    pub agent_request_count: u64,
    pub total_input_tokens: u64,
    pub total_output_tokens: u64,
    pub current_context_tokens: u64,
    pub max_context_tokens: u64,
    pub total_cache_read_tokens: u64,
    pub total_cache_creation_tokens: u64,
    pub cost: crate::session::usage::UsageCost,
    /// 信念度 B ∈ [0, 1]。0.0 表示未追踪
    pub belief: f64,
}

impl StatsSnapshot {
    pub fn cache_pct(&self) -> String {
        let total = self.total_input_tokens + self.total_cache_read_tokens;
        self.total_cache_read_tokens
            .checked_mul(100)
            .and_then(|tokens| tokens.checked_div(total))
            .map_or_else(|| "—".to_string(), |pct| format!("{pct}%"))
    }

    pub fn ctx_pct(&self) -> String {
        self.current_context_tokens
            .checked_mul(100)
            .and_then(|tokens| tokens.checked_div(self.max_context_tokens))
            .map_or_else(|| "—".to_string(), |pct| format!("{pct}%"))
    }

    pub fn format_cost(&self) -> String {
        let nano = self.cost.known_nano_cny;
        let known = if nano < 1_000_000 {
            "¥0.00".to_string()
        } else if nano < 1_000_000_000 {
            format!("¥{:.3}", nano as f64 / 1_000_000_000.0)
        } else {
            format!("¥{:.2}", nano as f64 / 1_000_000_000.0)
        };
        if self.cost.unpriced_requests == 0 {
            known
        } else {
            format!("{known} + {} unpriced", self.cost.unpriced_requests)
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
    let usage = ctx.usage.summary();
    let snapshot = StatsSnapshot {
        current_turn_count: stats.current_turn_count,
        agent_request_count: stats.agent_request_count,
        total_input_tokens: stats.total_input_tokens,
        total_output_tokens: stats.total_output_tokens,
        current_context_tokens: stats.current_context_tokens,
        max_context_tokens: ctx.config.max_context_tokens as u64,
        total_cache_read_tokens: stats.total_cache_read_tokens,
        total_cache_creation_tokens: stats.total_cache_creation_tokens,
        cost: usage.cost,
        belief,
    };
    ctx.display.render_title_update(model_label, &snapshot);
}
