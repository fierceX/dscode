use crate::protocol::UsageEvent;
use crate::session::usage::UsageSummary;
use crate::tools::metadata::{ToolResultKind, ToolStatus};
use crate::ui::{ArtifactDisplay, ToolPresentation};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Typed events.jsonl vocabulary.
///
/// `type` values and field names are the durable offline/replay contract and
/// must not change. `version` is optional on the variants where the historical
/// writer emitted no version field; newer typed writes preserve that byte
/// shape by using `None`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum EventLog {
    SessionStart {
        session_id: String,
    },
    UserInput {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        version: Option<u8>,
        content: String,
    },
    RuntimeTurnStarted {
        turn_id: String,
    },
    TurnStart {
        model: String,
        model_alias: Option<String>,
        belief: f64,
        forced_model: Option<String>,
    },
    Thinking {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        version: Option<u8>,
        content: String,
    },
    Text {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        version: Option<u8>,
        content: String,
    },
    ToolCall {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        version: Option<u8>,
        name: String,
        id: String,
        input: Value,
    },
    ToolResult {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        version: Option<u8>,
        tool_use_id: String,
        name: String,
        content: String,
        #[serde(default = "default_tool_status")]
        status: ToolStatus,
        #[serde(default)]
        exit_code: Option<i32>,
        #[serde(default = "default_tool_result_kind")]
        result_kind: ToolResultKind,
        #[serde(default)]
        presentation: Option<ToolPresentation>,
        #[serde(default)]
        artifacts: Vec<ArtifactDisplay>,
    },
    Usage {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        version: Option<u8>,
        input_tokens: i64,
        output_tokens: i64,
        cache_read_input_tokens: i64,
        cache_creation_input_tokens: i64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        context_tokens: Option<usize>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        max_context: Option<usize>,
        kind: String,
    },
    Signal {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        version: Option<u8>,
        signal_kind: String,
        severity: f64,
        source_tool: String,
        exit_code: Option<i32>,
        matched_pattern: Option<String>,
        message: String,
    },
    Compact {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        version: Option<u8>,
        trigger: String,
        result: String,
    },
    CompactionCheck {
        trigger: String,
        pressure_source: String,
        local_tokens: usize,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        provider_baseline_tokens: Option<usize>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        calibrated_tokens: Option<usize>,
        threshold_tokens: usize,
        projection_generation: u64,
    },
    TurnTracking {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        version: Option<u8>,
        decision: String,
        tool_call_count: u32,
        tool_error_count: u32,
        belief: f64,
        model: String,
    },
    TurnFinal {
        billing_turn_id: String,
        status: String,
        tool_call_count: u32,
        tool_error_count: u32,
        elapsed_ms: u64,
        error: Option<String>,
        usage: UsageSummary,
    },
    TurnError {
        error: String,
        category: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        severity: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        belief: Option<f64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        model: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        elapsed_ms: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        idle_ms: Option<u64>,
    },
    /// 前缀构建/失效重建时的完整快照：使任意请求的 system prompt 与 tools
    /// 可离线重建（"模型可见 = 日志可重建"）。仅在构建时写，不逐请求写。
    PrefixSnapshot {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        version: Option<u8>,
        fingerprint: String,
        dependency_fingerprint: String,
        system_prompt: String,
        tools_json: Vec<Value>,
    },
    ToolSurface {
        role: String,
        filesystem_backend: String,
        active: Vec<String>,
        hidden: Vec<Value>,
        surface_fingerprint: String,
    },
    ToolCapabilityResolution {
        bindings: Vec<Value>,
        capability_fingerprint: String,
    },
    PromptWorkflowResolution {
        active_workflows: Vec<String>,
        workflow_fingerprint: String,
    },
    SubAgent {
        session_id: String,
        status: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        input_tokens: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        output_tokens: Option<u64>,
    },
    Scavenge {
        note: String,
    },
    SignalRecoveryGuard {
        action: String,
        tool: String,
        tool_use_id: String,
        reason: String,
        guard_blocks: usize,
    },
    SignalRollbackError {
        path: String,
        error: String,
    },
    SignalRollback {
        files: Vec<Value>,
    },
    SignalReplanError {
        attempts: usize,
        session_id: String,
        error: String,
    },
    SignalReplan {
        attempts: usize,
        session_id: String,
        status: String,
        text_len: usize,
    },
    SignalHandover {
        belief: f64,
        edited_paths: Vec<String>,
        evidence: String,
        options: Vec<String>,
    },
    LlmWait {
        phase: String,
        elapsed_secs: u64,
        idle_secs: u64,
    },
    Stop {
        reason: String,
    },
    Retry,
    Error {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        version: Option<u8>,
        message: String,
    },
}

fn default_tool_status() -> ToolStatus {
    ToolStatus::Succeeded
}

fn default_tool_result_kind() -> ToolResultKind {
    ToolResultKind::Text
}

impl EventLog {
    pub fn usage(usage: &UsageEvent, kind: &str) -> Self {
        Self::Usage {
            version: None,
            input_tokens: usage.input_tokens,
            output_tokens: usage.output_tokens,
            cache_read_input_tokens: usage.cache_read_input_tokens,
            cache_creation_input_tokens: usage.cache_creation_input_tokens,
            context_tokens: None,
            max_context: None,
            kind: kind.to_string(),
        }
    }
}

#[cfg(test)]
#[path = "events_tests.rs"]
mod tests;
