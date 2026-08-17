use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::io::Write;
use std::path::Path;

use crate::session::paths::SessionLayout;

pub const PROTOCOL_VERSION: u32 = 3;

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct SdkRequest {
    pub version: Option<u32>,
    pub prompt: String,
    pub session_id: Option<String>,
    pub mission: Option<String>,
    pub options: SdkOptions,
}

impl Default for SdkRequest {
    fn default() -> Self {
        Self {
            version: Some(PROTOCOL_VERSION),
            prompt: String::new(),
            session_id: None,
            mission: None,
            options: SdkOptions::default(),
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SdkOptions {
    pub provider: SdkProviderOptions,
    pub generation: SdkGenerationOptions,
    pub context: SdkContextOptions,
    pub tools: SdkToolOptions,
    pub session: SdkSessionOptions,
    pub output: SdkOutputOptions,
    pub signal: SdkSignalOptions,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SdkProviderOptions {
    pub model: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SdkGenerationOptions {
    pub max_tokens: Option<i32>,
    pub max_turns: Option<i32>,
    pub llm_first_event_timeout: Option<i32>,
    pub llm_idle_timeout: Option<i32>,
    pub llm_wait_heartbeat: Option<i32>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SdkContextOptions {
    pub max_context: Option<usize>,
    pub context_compact_pct: Option<u8>,
    pub context_reserve_tokens: Option<usize>,
    pub context_compact_tail_tokens: Option<usize>,
    pub context_compact_max_output_tokens: Option<i32>,
    pub context_compact_input_reduction: Option<bool>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SdkToolOptions {
    pub tool_timeout: Option<i32>,
    pub sub_agent_timeout: Option<i32>,
    pub enabled_tools: Option<Vec<String>>,
    pub edit_mode: Option<crate::config::EditMode>,
    pub edit_fuzzy_match: Option<bool>,
    pub edit_fuzzy_threshold: Option<f64>,
    pub edit_enforce_seen_lines: Option<bool>,
    pub skills: Option<Vec<String>>,
    pub inline_skills: Option<Vec<SdkInlineSkill>>,
    pub skill_discovery_policy: Option<SdkSkillDiscoveryPolicy>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SdkSessionOptions {
    pub session_id: Option<String>,
    pub session_layout: Option<SessionLayout>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SdkOutputOptions {
    pub verbose: Option<bool>,
    pub stream_events: Option<bool>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SdkSignalOptions {
    pub policy: Option<crate::config::SignalPolicy>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct SdkInlineSkill {
    pub name: String,
    pub description: String,
    pub content: String,
    pub exposure: Option<SdkCapabilityExposure>,
    pub revision: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SdkCapabilityExposure {
    ModelDiscoverable,
    ModelAddressable,
    HostOnly,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SdkSkillDiscoveryPolicy {
    Defaults,
    RuntimeOnly,
    ExplicitOnly,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SdkStatus {
    Ok,
    Failed,
    Interrupted,
    MaxTurnsExceeded,
}

impl SdkStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Failed => "failed",
            Self::Interrupted => "interrupted",
            Self::MaxTurnsExceeded => "max_turns_exceeded",
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct SdkFinal {
    #[serde(rename = "type")]
    pub event_type: &'static str,
    pub version: u32,
    pub status: SdkStatus,
    pub billing_turn_id: String,
    pub session_id: String,
    pub session_ref: String,
    pub home: String,
    pub cwd: String,
    pub events_path: String,
    pub conversation_path: String,
    pub artifacts_dir: String,
    pub summary_path: String,
    pub usage_path: String,
    pub tool_call_count: u32,
    pub tool_error_count: u32,
    pub error: Option<String>,
    pub usage_records: Vec<crate::session::usage::UsageRecord>,
    pub usage: crate::session::usage::UsageSummary,
}

pub fn emit_json_line<T: Serialize>(value: &T) {
    if let Ok(line) = serde_json::to_string(value) {
        let mut stdout = std::io::stdout().lock();
        let _ = writeln!(stdout, "{line}");
        let _ = stdout.flush();
    }
}

pub fn emit_failed_parse(error: &str) {
    emit_json_line(&serde_json::json!({
        "type": "final",
        "version": PROTOCOL_VERSION,
        "status": SdkStatus::Failed.as_str(),
        "billing_turn_id": "",
        "session_id": "",
        "session_ref": "",
        "home": "",
        "cwd": "",
        "events_path": "",
        "conversation_path": "",
        "artifacts_dir": "",
        "summary_path": "",
        "usage_path": "",
        "tool_call_count": 0,
        "tool_error_count": 0,
        "error": error,
        "usage_records": [],
        "usage": crate::session::usage::UsageSummary::default(),
    }));
}

pub fn parse_agent_jsonl_request(input: &str) -> Result<SdkRequest, String> {
    let value: Value =
        serde_json::from_str(input).map_err(|e| format!("invalid SDK request JSON: {e}"))?;
    match value.get("prompt") {
        Some(Value::String(_)) => {}
        Some(_) => return Err("invalid SDK request: prompt must be a string".to_string()),
        None => return Err("invalid SDK request: missing required field prompt".to_string()),
    }
    serde_json::from_value(value).map_err(|e| format!("invalid SDK request: {e}"))
}

pub fn validate_sdk_request(req: &SdkRequest) -> Result<(), String> {
    let opts = &req.options;
    let provider = &opts.provider;
    let generation = &opts.generation;
    let context = &opts.context;
    let tools = &opts.tools;
    if let Some(model) = provider.model.as_deref()
        && model.trim().is_empty()
    {
        return Err("invalid SDK request: model must not be empty".to_string());
    }
    if let Some(max_tokens) = generation.max_tokens
        && max_tokens <= 0
    {
        return Err("invalid SDK request: max_tokens must be greater than 0".to_string());
    }
    if let Some(max_turns) = generation.max_turns
        && max_turns <= 0
    {
        return Err("invalid SDK request: max_turns must be greater than 0".to_string());
    }
    if let Some(pct) = context.context_compact_pct
        && !(1..=100).contains(&pct)
    {
        return Err(
            "invalid SDK request: context_compact_pct must be between 1 and 100".to_string(),
        );
    }
    if let Some(tokens) = context.context_reserve_tokens
        && tokens == 0
    {
        return Err(
            "invalid SDK request: context_reserve_tokens must be greater than 0".to_string(),
        );
    }
    if let Some(tokens) = context.context_compact_tail_tokens
        && tokens == 0
    {
        return Err(
            "invalid SDK request: context_compact_tail_tokens must be greater than 0".to_string(),
        );
    }
    if let Some(tokens) = context.context_compact_max_output_tokens
        && tokens <= 0
    {
        return Err(
            "invalid SDK request: context_compact_max_output_tokens must be greater than 0"
                .to_string(),
        );
    }
    if let Some(tool_timeout) = tools.tool_timeout
        && tool_timeout <= 0
    {
        return Err("invalid SDK request: tool_timeout must be greater than 0".to_string());
    }
    if let Some(sub_agent_timeout) = tools.sub_agent_timeout
        && sub_agent_timeout <= 0
    {
        return Err("invalid SDK request: sub_agent_timeout must be greater than 0".to_string());
    }
    if let Some(timeout) = generation.llm_first_event_timeout
        && timeout <= 0
    {
        return Err(
            "invalid SDK request: llm_first_event_timeout must be greater than 0".to_string(),
        );
    }
    if let Some(timeout) = generation.llm_idle_timeout
        && timeout <= 0
    {
        return Err("invalid SDK request: llm_idle_timeout must be greater than 0".to_string());
    }
    if let Some(timeout) = generation.llm_wait_heartbeat
        && timeout < 0
    {
        return Err("invalid SDK request: llm_wait_heartbeat must be zero or greater".to_string());
    }
    if let Some(skills) = &tools.skills {
        for skill in skills {
            validate_capability_name(skill, "skill")?;
        }
    }
    if let Some(inline_skills) = &tools.inline_skills {
        for skill in inline_skills {
            validate_capability_name(&skill.name, "inline skill")?;
            if skill.content.trim().is_empty() {
                return Err(format!(
                    "invalid SDK request: inline skill '{}' content must be non-empty",
                    skill.name
                ));
            }
        }
    }
    Ok(())
}

pub fn path_string(path: &Path) -> String {
    path.to_string_lossy().to_string()
}

fn validate_capability_name(name: &str, label: &str) -> Result<(), String> {
    if !crate::capabilities::source::is_valid_skill_name(name) {
        return Err(format!("invalid SDK request: invalid {label} name: {name}"));
    }
    Ok(())
}

#[cfg(test)]
#[path = "sdk_protocol_tests.rs"]
mod tests;
