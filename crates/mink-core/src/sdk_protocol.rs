use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::io::Write;
use std::path::Path;

use crate::session::paths::SessionLayout;

pub const PROTOCOL_VERSION: u32 = 2;

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
    pub model: Option<String>,
    pub max_tokens: Option<i32>,
    pub max_turns: Option<i32>,
    pub max_context: Option<usize>,
    pub context_compact_pct: Option<u8>,
    pub context_reserve_tokens: Option<usize>,
    pub context_compact_tail_tokens: Option<usize>,
    pub context_compact_max_output_tokens: Option<i32>,
    pub context_compact_input_reduction: Option<bool>,
    pub tool_timeout: Option<i32>,
    pub sub_agent_timeout: Option<i32>,
    pub llm_first_event_timeout: Option<i32>,
    pub llm_idle_timeout: Option<i32>,
    pub llm_wait_heartbeat: Option<i32>,
    pub verbose: Option<bool>,
    pub enabled_tools: Option<Vec<String>>,
    pub skills: Option<Vec<String>>,
    pub inline_skills: Option<Vec<SdkInlineSkill>>,
    pub skill_discovery_policy: Option<SdkSkillDiscoveryPolicy>,
    pub session_id: Option<String>,
    pub session_layout: Option<SessionLayout>,
    pub stream_events: Option<bool>,
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
    if let Some(model) = opts.model.as_deref()
        && model.trim().is_empty()
    {
        return Err("invalid SDK request: model must not be empty".to_string());
    }
    if let Some(max_tokens) = opts.max_tokens
        && max_tokens <= 0
    {
        return Err("invalid SDK request: max_tokens must be greater than 0".to_string());
    }
    if let Some(max_turns) = opts.max_turns
        && max_turns <= 0
    {
        return Err("invalid SDK request: max_turns must be greater than 0".to_string());
    }
    if let Some(pct) = opts.context_compact_pct
        && !(1..=100).contains(&pct)
    {
        return Err(
            "invalid SDK request: context_compact_pct must be between 1 and 100".to_string(),
        );
    }
    if let Some(tokens) = opts.context_reserve_tokens
        && tokens == 0
    {
        return Err(
            "invalid SDK request: context_reserve_tokens must be greater than 0".to_string(),
        );
    }
    if let Some(tokens) = opts.context_compact_tail_tokens
        && tokens == 0
    {
        return Err(
            "invalid SDK request: context_compact_tail_tokens must be greater than 0".to_string(),
        );
    }
    if let Some(tokens) = opts.context_compact_max_output_tokens
        && tokens <= 0
    {
        return Err(
            "invalid SDK request: context_compact_max_output_tokens must be greater than 0"
                .to_string(),
        );
    }
    if let Some(tool_timeout) = opts.tool_timeout
        && tool_timeout <= 0
    {
        return Err("invalid SDK request: tool_timeout must be greater than 0".to_string());
    }
    if let Some(sub_agent_timeout) = opts.sub_agent_timeout
        && sub_agent_timeout <= 0
    {
        return Err("invalid SDK request: sub_agent_timeout must be greater than 0".to_string());
    }
    if let Some(timeout) = opts.llm_first_event_timeout
        && timeout <= 0
    {
        return Err(
            "invalid SDK request: llm_first_event_timeout must be greater than 0".to_string(),
        );
    }
    if let Some(timeout) = opts.llm_idle_timeout
        && timeout <= 0
    {
        return Err("invalid SDK request: llm_idle_timeout must be greater than 0".to_string());
    }
    if let Some(timeout) = opts.llm_wait_heartbeat
        && timeout < 0
    {
        return Err("invalid SDK request: llm_wait_heartbeat must be zero or greater".to_string());
    }
    if let Some(skills) = &opts.skills {
        for skill in skills {
            validate_capability_name(skill, "skill")?;
        }
    }
    if let Some(inline_skills) = &opts.inline_skills {
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
    let trimmed = name.trim();
    if trimmed.is_empty()
        || trimmed != name
        || trimmed.starts_with('.')
        || trimmed.contains('/')
        || trimmed.contains('\\')
        || trimmed.contains("..")
    {
        return Err(format!("invalid SDK request: invalid {label} name: {name}"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sdk_request_defaults_version_and_options() {
        let req: SdkRequest = serde_json::from_str(r#"{"prompt":"hi"}"#).unwrap();
        assert_eq!(req.version, Some(PROTOCOL_VERSION));
        assert_eq!(req.prompt, "hi");
        assert_eq!(req.options.session_layout, None);
        assert_eq!(req.options.skills, None);
        assert_eq!(req.options.inline_skills, None);
        assert_eq!(req.options.skill_discovery_policy, None);
        assert_eq!(req.options.max_context, None);
        assert_eq!(req.options.context_compact_pct, None);
    }

    #[test]
    fn sdk_request_accepts_session_layout() {
        let req =
            parse_agent_jsonl_request(r#"{"prompt":"hi","options":{"session_layout":"home"}}"#)
                .unwrap();
        assert_eq!(req.options.session_layout, Some(SessionLayout::HomeScoped));

        let req =
            parse_agent_jsonl_request(r#"{"prompt":"hi","options":{"session_layout":"isolated"}}"#)
                .unwrap();
        assert_eq!(req.options.session_layout, Some(SessionLayout::Isolated));
    }

    #[test]
    fn sdk_request_accepts_selected_skills() {
        let req = parse_agent_jsonl_request(
            r#"{"prompt":"hi","options":{"skills":["debugging","verification"]}}"#,
        )
        .unwrap();
        assert_eq!(
            req.options.skills,
            Some(vec!["debugging".to_string(), "verification".to_string()])
        );
    }

    #[test]
    fn sdk_request_accepts_inline_skills_and_policy() {
        let req = parse_agent_jsonl_request(
            r#"{
                "prompt":"hi",
                "options":{
                    "inline_skills":[{
                        "name":"company-policy",
                        "description":"Company policy",
                        "content":"private policy",
                        "exposure":"model_addressable",
                        "revision":"rev-1"
                    }],
                    "skill_discovery_policy":"runtime_only"
                }
            }"#,
        )
        .unwrap();
        let inline = req.options.inline_skills.as_ref().unwrap();
        assert_eq!(inline[0].name, "company-policy");
        assert_eq!(
            inline[0].exposure,
            Some(SdkCapabilityExposure::ModelAddressable)
        );
        assert_eq!(
            req.options.skill_discovery_policy,
            Some(SdkSkillDiscoveryPolicy::RuntimeOnly)
        );
    }

    #[test]
    fn validate_sdk_request_rejects_invalid_inline_skill() {
        let req = parse_agent_jsonl_request(
            r#"{"prompt":"hi","options":{"inline_skills":[{"name":"../secret","content":"x"}]}}"#,
        )
        .unwrap();
        let err = validate_sdk_request(&req).unwrap_err();
        assert!(err.contains("invalid inline skill name"), "{err}");

        let req =
            parse_agent_jsonl_request(r#"{"prompt":"hi","options":{"skills":[" debugging"]}}"#)
                .unwrap();
        let err = validate_sdk_request(&req).unwrap_err();
        assert!(err.contains("invalid skill name"), "{err}");

        let req = parse_agent_jsonl_request(
            r#"{"prompt":"hi","options":{"inline_skills":[{"name":"empty","content":""}]}}"#,
        )
        .unwrap();
        let err = validate_sdk_request(&req).unwrap_err();
        assert!(err.contains("content must be non-empty"), "{err}");
    }

    #[test]
    fn sdk_request_rejects_unknown_session_layout() {
        let err = parse_agent_jsonl_request(
            r#"{"prompt":"hi","options":{"session_layout":"workspace"}}"#,
        )
        .unwrap_err();
        assert!(err.contains("workspace"));
    }

    #[test]
    fn parse_agent_jsonl_request_rejects_missing_prompt() {
        let err = parse_agent_jsonl_request(r#"{"version":2}"#).unwrap_err();
        assert!(err.contains("missing required field prompt"));
    }

    #[test]
    fn parse_agent_jsonl_request_rejects_non_string_prompt() {
        let err = parse_agent_jsonl_request(r#"{"version":2,"prompt":123}"#).unwrap_err();
        assert!(err.contains("prompt must be a string"));
    }

    #[test]
    fn validate_sdk_request_rejects_bad_numeric_options() {
        let req =
            parse_agent_jsonl_request(r#"{"prompt":"hi","options":{"max_tokens":0}}"#).unwrap();
        let err = validate_sdk_request(&req).unwrap_err();
        assert!(err.contains("max_tokens must be greater than 0"));

        let req =
            parse_agent_jsonl_request(r#"{"prompt":"hi","options":{"context_compact_pct":0}}"#)
                .unwrap();
        let err = validate_sdk_request(&req).unwrap_err();
        assert!(err.contains("context_compact_pct must be between 1 and 100"));

        let req =
            parse_agent_jsonl_request(r#"{"prompt":"hi","options":{"context_reserve_tokens":0}}"#)
                .unwrap();
        let err = validate_sdk_request(&req).unwrap_err();
        assert!(err.contains("context_reserve_tokens must be greater than 0"));

        let req = parse_agent_jsonl_request(
            r#"{"prompt":"hi","options":{"context_compact_tail_tokens":0}}"#,
        )
        .unwrap();
        let err = validate_sdk_request(&req).unwrap_err();
        assert!(err.contains("context_compact_tail_tokens must be greater than 0"));

        let req = parse_agent_jsonl_request(
            r#"{"prompt":"hi","options":{"context_compact_max_output_tokens":0}}"#,
        )
        .unwrap();
        let err = validate_sdk_request(&req).unwrap_err();
        assert!(err.contains("context_compact_max_output_tokens must be greater than 0"));
    }

    #[test]
    fn sdk_request_accepts_explicit_compaction_policy() {
        let req = parse_agent_jsonl_request(
            r#"{
                "prompt":"hi",
                "options":{
                    "max_context":64000,
                    "context_compact_pct":65,
                    "context_reserve_tokens":12000,
                    "context_compact_tail_tokens":16000,
                    "context_compact_max_output_tokens":4096,
                    "context_compact_input_reduction":true
                }
            }"#,
        )
        .unwrap();
        validate_sdk_request(&req).unwrap();
        assert_eq!(req.options.max_context, Some(64_000));
        assert_eq!(req.options.context_compact_pct, Some(65));
        assert_eq!(req.options.context_reserve_tokens, Some(12_000));
        assert_eq!(req.options.context_compact_tail_tokens, Some(16_000));
        assert_eq!(req.options.context_compact_max_output_tokens, Some(4_096));
        assert_eq!(req.options.context_compact_input_reduction, Some(true));
    }

    #[test]
    fn validate_sdk_request_rejects_bad_llm_timeout_options() {
        let req = parse_agent_jsonl_request(r#"{"prompt":"hi","options":{"llm_idle_timeout":0}}"#)
            .unwrap();
        let err = validate_sdk_request(&req).unwrap_err();
        assert!(err.contains("llm_idle_timeout must be greater than 0"));

        let req =
            parse_agent_jsonl_request(r#"{"prompt":"hi","options":{"llm_wait_heartbeat":-1}}"#)
                .unwrap();
        let err = validate_sdk_request(&req).unwrap_err();
        assert!(err.contains("llm_wait_heartbeat must be zero or greater"));
    }

    #[test]
    fn validate_sdk_request_accepts_custom_model() {
        let req =
            parse_agent_jsonl_request(r#"{"prompt":"hi","options":{"model":"gpt-4"}}"#).unwrap();
        validate_sdk_request(&req).unwrap();
    }

    #[test]
    fn validate_sdk_request_rejects_empty_model() {
        let req = parse_agent_jsonl_request(r#"{"prompt":"hi","options":{"model":" "}}"#).unwrap();
        let err = validate_sdk_request(&req).unwrap_err();
        assert!(err.contains("model must not be empty"));
    }
}
