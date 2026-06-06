use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::Path;

pub const PROTOCOL_VERSION: u32 = 1;

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct SdkRequest {
    pub version: Option<u32>,
    pub prompt: String,
    pub session_id: Option<String>,
    pub options: SdkOptions,
}

impl Default for SdkRequest {
    fn default() -> Self {
        Self {
            version: Some(PROTOCOL_VERSION),
            prompt: String::new(),
            session_id: None,
            options: SdkOptions::default(),
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct SdkOptions {
    pub disable_bash: bool,
    pub disable_sub_agent: bool,
    pub disable_web: bool,
    pub disable_python: bool,
    pub model: Option<String>,
    pub max_tokens: Option<i32>,
    pub max_turns: Option<i32>,
    pub tool_timeout: Option<i32>,
    pub sub_agent_timeout: Option<i32>,
    pub verbose: Option<bool>,
    pub session_id: Option<String>,
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
    pub session_id: String,
    pub session_ref: String,
    pub home: String,
    pub cwd: String,
    pub events_path: String,
    pub conversation_path: String,
    pub artifacts_dir: String,
    pub summary_path: String,
    pub tool_call_count: u32,
    pub tool_error_count: u32,
    pub error: Option<String>,
}

pub fn emit_json_line<T: Serialize>(value: &T) {
    if let Ok(line) = serde_json::to_string(value) {
        println!("{line}");
    }
}

pub fn emit_failed_parse(error: &str) {
    emit_json_line(&serde_json::json!({
        "type": "final",
        "version": PROTOCOL_VERSION,
        "status": SdkStatus::Failed.as_str(),
        "session_id": "",
        "session_ref": "",
        "home": "",
        "cwd": "",
        "events_path": "",
        "conversation_path": "",
        "artifacts_dir": "",
        "summary_path": "",
        "tool_call_count": 0,
        "tool_error_count": 0,
        "error": error,
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
        && !matches!(
            model,
            "flash" | "pro" | "deepseek-v4-flash" | "deepseek-v4-pro"
        )
    {
        return Err(format!(
            "invalid SDK request: model must be flash or pro, got {model}"
        ));
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
    Ok(())
}

pub fn path_string(path: &Path) -> String {
    path.to_string_lossy().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sdk_request_defaults_version_and_options() {
        let req: SdkRequest = serde_json::from_str(r#"{"prompt":"hi"}"#).unwrap();
        assert_eq!(req.version, Some(PROTOCOL_VERSION));
        assert_eq!(req.prompt, "hi");
        assert!(!req.options.disable_bash);
    }

    #[test]
    fn parse_agent_jsonl_request_rejects_missing_prompt() {
        let err = parse_agent_jsonl_request(r#"{"version":1}"#).unwrap_err();
        assert!(err.contains("missing required field prompt"));
    }

    #[test]
    fn parse_agent_jsonl_request_rejects_non_string_prompt() {
        let err = parse_agent_jsonl_request(r#"{"version":1,"prompt":123}"#).unwrap_err();
        assert!(err.contains("prompt must be a string"));
    }

    #[test]
    fn validate_sdk_request_rejects_bad_numeric_options() {
        let req =
            parse_agent_jsonl_request(r#"{"prompt":"hi","options":{"max_tokens":0}}"#).unwrap();
        let err = validate_sdk_request(&req).unwrap_err();
        assert!(err.contains("max_tokens must be greater than 0"));
    }

    #[test]
    fn validate_sdk_request_rejects_unknown_model() {
        let req =
            parse_agent_jsonl_request(r#"{"prompt":"hi","options":{"model":"gpt-4"}}"#).unwrap();
        let err = validate_sdk_request(&req).unwrap_err();
        assert!(err.contains("model must be flash or pro"));
    }
}
