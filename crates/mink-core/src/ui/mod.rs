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

/// LLM 等待心跳消息的稳定前缀。
///
/// mink-core 是消息格式的唯一来源（[`llm_wait_heartbeat_message`]）；
/// mink-cli/TUI 等展示层通过 [`parse_llm_wait_heartbeat_elapsed`] 识别心跳，
/// 不再各自复制字面量。
pub const LLM_WAIT_HEARTBEAT_PREFIX: &str = "Waiting for model response...";

/// 构造 LLM 等待心跳消息（`Display::render_info` 的稳定文本契约）。
pub fn llm_wait_heartbeat_message(elapsed_secs: u64, idle_secs: u64) -> String {
    format!("{LLM_WAIT_HEARTBEAT_PREFIX} elapsed={elapsed_secs}s idle={idle_secs}s")
}

/// 从心跳消息解析 elapsed 秒数；非心跳消息返回 `None`。
pub fn parse_llm_wait_heartbeat_elapsed(msg: &str) -> Option<u64> {
    let rest = msg.strip_prefix(LLM_WAIT_HEARTBEAT_PREFIX)?;
    let digits: String = rest
        .split("elapsed=")
        .nth(1)?
        .chars()
        .take_while(char::is_ascii_digit)
        .collect();
    digits.parse().ok()
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
    /// 信念度 B ∈ [0, 1]。0.0 表示未追踪
    pub belief: f64,
}

impl StatsSnapshot {
    pub fn cache_pct(&self) -> String {
        // Provider prompt accounting is partitioned into mutually exclusive
        // uncached, cache-read, and cache-creation tokens. Creation tokens are
        // misses, so omitting them would overstate the hit rate (even 100% for
        // a request that created more cache than it read).
        let total = self
            .total_input_tokens
            .saturating_add(self.total_cache_read_tokens)
            .saturating_add(self.total_cache_creation_tokens);
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
        belief,
    };
    ctx.display.render_title_update(model_label, &snapshot);
}

#[cfg(test)]
mod stats_snapshot_tests {
    use super::StatsSnapshot;

    #[test]
    fn cache_pct_includes_cache_creation_in_prompt_total() {
        let snapshot = StatsSnapshot {
            total_cache_read_tokens: 40,
            total_cache_creation_tokens: 60,
            ..Default::default()
        };
        assert_eq!(snapshot.cache_pct(), "40%");
    }

    #[test]
    fn cache_pct_handles_mixed_and_empty_prompt_partitions() {
        let mixed = StatsSnapshot {
            total_input_tokens: 20,
            total_cache_read_tokens: 30,
            total_cache_creation_tokens: 50,
            ..Default::default()
        };
        assert_eq!(mixed.cache_pct(), "30%");
        assert_eq!(StatsSnapshot::default().cache_pct(), "—");
    }
}

#[cfg(test)]
mod llm_wait_heartbeat_tests {
    use super::{llm_wait_heartbeat_message, parse_llm_wait_heartbeat_elapsed};

    #[test]
    fn heartbeat_message_format_is_pinned_and_parseable() {
        // 精确文本被 CLI/TUI 依赖；变更此处必须同步更新展示层。
        assert_eq!(
            llm_wait_heartbeat_message(30, 0),
            "Waiting for model response... elapsed=30s idle=0s"
        );
        assert_eq!(
            parse_llm_wait_heartbeat_elapsed(&llm_wait_heartbeat_message(60, 45)),
            Some(60)
        );
    }

    #[test]
    fn non_heartbeat_messages_are_rejected() {
        assert_eq!(parse_llm_wait_heartbeat_elapsed("Retrying (1/3)..."), None);
        assert_eq!(
            parse_llm_wait_heartbeat_elapsed("Waiting for model response..."),
            None
        );
        assert_eq!(
            parse_llm_wait_heartbeat_elapsed("Waiting for model response... idle=1s"),
            None
        );
    }
}
