pub use crate::tools::metadata::ToolResultKind;
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
    pub success: bool,
    pub result_kind: ToolResultKind,
    pub presentation: Option<&'a ToolPresentation>,
    pub artifacts: &'a [ArtifactDisplay],
}

pub trait Display: Send + Sync {
    fn render_thinking(&self, content: &str);
    fn render_text(&self, content: &str);
    fn render_tool_call(&self, name: &str, summary: &str);
    fn render_tool_call_detail(&self, call: &ToolCallDisplay<'_>) {
        self.render_tool_call(call.tool_name, call.summary);
    }
    fn render_tool_result(&self, tool_name: &str, content_preview: &str);
    fn render_tool_result_detail(&self, result: &ToolResultDisplay<'_>) {
        self.render_tool_result(result.tool_name, result.content_preview);
    }
    fn render_tool_result_presented(&self, result: &PresentedToolResultDisplay<'_>) {
        self.render_tool_result_detail(&result.base);
    }
    fn render_stop(&self);
    /// 带结束原因的 stop（interrupted 等）。默认退化为 render_stop——
    /// 需要区分中断语义的实现（SDK）覆盖。
    fn render_stop_with_reason(&self, _reason: &str) {
        self.render_stop();
    }
    /// 信号（信念系统：工具失败/编辑循环检测等）。默认空实现——
    /// 需要实时信号的实现（SDK）覆盖。
    fn render_signal(&self, _signal_kind: &str, _severity: f64, _message: &str) {}
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

#[derive(Clone, Copy, Debug)]
pub enum SubAgentStreamKind {
    Thinking,
    Text,
}

pub trait SubAgentStreamSink: Send + Sync {
    fn render_sub_agent_stream(&self, session_id: &str, kind: SubAgentStreamKind, content: &str);
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
