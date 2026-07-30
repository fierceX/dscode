use crate::agent::orchestrator::TurnStatus;
use crate::capabilities::{CapabilityExposure, RuntimeSkill, SkillDiscoveryPolicy};
use crate::config::{Config, OutputFormat};
use crate::runtime::TurnOutcome;
use crate::sdk_protocol::{
    PROTOCOL_VERSION, SdkCapabilityExposure, SdkFinal, SdkRequest, SdkSkillDiscoveryPolicy,
    SdkStatus, path_string,
};

/// Apply Agent JSONL SDK request options to the complete mink config.
///
/// This is the single mapping used by `mink-core --agent-jsonl` and by Rust
/// callers that want to accept the same SDK request schema. It intentionally
/// mutates the existing [`Config`] instead of constructing a separate reduced
/// config type.
pub fn apply_sdk_request_options(cfg: &mut Config, req: &SdkRequest) {
    let opts = &req.options;
    if let Some(model) = &opts.model {
        cfg.model = model.clone();
        cfg.cli_overrides.model = true;
    }
    if let Some(max_tokens) = opts.max_tokens {
        cfg.max_tokens = max_tokens;
        cfg.cli_overrides.max_tokens = true;
    }
    if let Some(max_turns) = opts.max_turns {
        cfg.max_turns = max_turns;
        cfg.cli_overrides.max_turns = true;
    }
    if let Some(max_context) = opts.max_context {
        cfg.max_context_tokens = max_context;
        cfg.cli_overrides.max_context_tokens = true;
    }
    if let Some(pct) = opts.context_compact_pct {
        cfg.context_compact_pct = pct;
    }
    if let Some(tokens) = opts.context_reserve_tokens {
        cfg.context_reserve_tokens = tokens;
    }
    if let Some(tokens) = opts.context_compact_tail_tokens {
        cfg.context_compact_tail_tokens = tokens;
    }
    if let Some(tokens) = opts.context_compact_max_output_tokens {
        cfg.context_compact_max_output_tokens = tokens;
    }
    if let Some(enabled) = opts.context_compact_input_reduction {
        cfg.context_compact_input_reduction = enabled;
    }
    if let Some(tool_timeout) = opts.tool_timeout {
        cfg.tool_timeout_secs = tool_timeout;
        cfg.cli_overrides.tool_timeout_secs = true;
    }
    if let Some(sub_agent_timeout) = opts.sub_agent_timeout {
        cfg.sub_agent_timeout_secs = sub_agent_timeout;
        cfg.cli_overrides.sub_agent_timeout_secs = true;
    }
    if let Some(timeout) = opts.llm_first_event_timeout {
        cfg.llm_first_event_timeout_secs = timeout;
        cfg.cli_overrides.llm_first_event_timeout_secs = true;
    }
    if let Some(timeout) = opts.llm_idle_timeout {
        cfg.llm_idle_timeout_secs = timeout;
        cfg.cli_overrides.llm_idle_timeout_secs = true;
    }
    if let Some(timeout) = opts.llm_wait_heartbeat {
        cfg.llm_wait_heartbeat_secs = timeout;
        cfg.cli_overrides.llm_wait_heartbeat_secs = true;
    }
    if let Some(verbose) = opts.verbose {
        cfg.verbose = verbose;
    }
    if opts.stream_events == Some(false) {
        cfg.output_format = OutputFormat::Human;
    }
    if let Some(session_id) = req
        .session_id
        .as_deref()
        .or(opts.session_id.as_deref())
        .filter(|s| !s.is_empty())
    {
        cfg.session_id = session_id.to_string();
    }
    if let Some(mission) = &req.mission {
        cfg.mission_content = Some(mission.clone());
    }
    if let Some(tools) = &opts.enabled_tools {
        cfg.enabled_tools = Some(tools.clone());
    }
    if let Some(skills) = &opts.skills {
        cfg.skills = skills.clone();
    }
}

pub fn runtime_skills_from_sdk_request(req: &SdkRequest) -> Vec<RuntimeSkill> {
    req.options
        .inline_skills
        .as_deref()
        .unwrap_or(&[])
        .iter()
        .map(|skill| {
            RuntimeSkill::new(
                skill.name.clone(),
                skill.description.clone(),
                skill.content.clone(),
            )
            .with_exposure(capability_exposure_from_sdk(
                skill
                    .exposure
                    .unwrap_or(SdkCapabilityExposure::ModelAddressable),
            ))
            .with_optional_revision(skill.revision.clone())
        })
        .collect()
}

pub fn skill_discovery_policy_from_sdk_request(req: &SdkRequest) -> Option<SkillDiscoveryPolicy> {
    req.options
        .skill_discovery_policy
        .map(skill_discovery_policy_from_sdk)
}

fn capability_exposure_from_sdk(exposure: SdkCapabilityExposure) -> CapabilityExposure {
    match exposure {
        SdkCapabilityExposure::ModelDiscoverable => CapabilityExposure::ModelDiscoverable,
        SdkCapabilityExposure::ModelAddressable => CapabilityExposure::ModelAddressable,
        SdkCapabilityExposure::HostOnly => CapabilityExposure::HostOnly,
    }
}

fn skill_discovery_policy_from_sdk(policy: SdkSkillDiscoveryPolicy) -> SkillDiscoveryPolicy {
    match policy {
        SdkSkillDiscoveryPolicy::Defaults => SkillDiscoveryPolicy::Defaults,
        SdkSkillDiscoveryPolicy::RuntimeOnly => SkillDiscoveryPolicy::RuntimeOnly,
        SdkSkillDiscoveryPolicy::ExplicitOnly => SkillDiscoveryPolicy::ExplicitOnly,
    }
}

pub fn sdk_status_from_turn(status: TurnStatus) -> SdkStatus {
    match status {
        TurnStatus::Ok => SdkStatus::Ok,
        TurnStatus::Failed => SdkStatus::Failed,
        TurnStatus::Interrupted => SdkStatus::Interrupted,
        TurnStatus::MaxTurnsExceeded => SdkStatus::MaxTurnsExceeded,
    }
}

pub fn exit_code_from_turn(status: TurnStatus) -> i32 {
    match status {
        TurnStatus::Ok => 0,
        TurnStatus::Failed => 1,
        TurnStatus::Interrupted => 130,
        TurnStatus::MaxTurnsExceeded => 2,
    }
}

pub fn final_from_outcome(outcome: &TurnOutcome) -> SdkFinal {
    let session = &outcome.session;
    SdkFinal {
        event_type: "final",
        version: PROTOCOL_VERSION,
        status: sdk_status_from_turn(outcome.status),
        billing_turn_id: outcome.billing_turn_id.clone(),
        session_id: session.session_id.clone(),
        session_ref: session.session_ref.clone(),
        home: path_string(&session.home),
        cwd: path_string(&session.cwd),
        events_path: path_string(&session.events_path),
        conversation_path: path_string(&session.conversation_path),
        artifacts_dir: path_string(&session.artifacts_dir),
        summary_path: path_string(&session.summary_path),
        usage_path: path_string(&session.usage_path),
        tool_call_count: outcome.tool_call_count,
        tool_error_count: outcome.tool_error_count,
        error: outcome.error.clone(),
        usage_records: outcome.usage_records.clone(),
        usage: outcome.usage.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::runtime::SessionInfo;

    #[test]
    fn sdk_request_options_map_to_config_once() {
        let req = crate::sdk_protocol::parse_agent_jsonl_request(
            r#"{
                "prompt": "hi",
                "session_id": "outer",
                "mission": "mission",
                "options": {
                    "model": "pro",
                    "max_tokens": 123,
                    "max_turns": 4,
                    "max_context": 64000,
                    "context_compact_pct": 65,
                    "context_reserve_tokens": 12000,
                    "context_compact_tail_tokens": 16000,
                    "context_compact_max_output_tokens": 4096,
                    "context_compact_input_reduction": true,
                    "tool_timeout": 5,
                    "sub_agent_timeout": 6,
                    "llm_first_event_timeout": 7,
                    "llm_idle_timeout": 8,
                    "llm_wait_heartbeat": 9,
                    "verbose": true,
                    "enabled_tools": ["Read", "Bash"],
                    "skills": ["debugging", "verification"],
                    "inline_skills": [{
                        "name": "company-policy",
                        "description": "Company policy",
                        "content": "private policy",
                        "exposure": "model_addressable",
                        "revision": "rev-1"
                    }],
                    "skill_discovery_policy": "runtime_only",
                    "session_id": "inner",
                    "session_layout": "home",
                    "stream_events": false
                }
            }"#,
        )
        .unwrap();
        let mut cfg = Config::default();

        apply_sdk_request_options(&mut cfg, &req);

        assert_eq!(cfg.model, "pro");
        assert_eq!(cfg.max_tokens, 123);
        assert_eq!(cfg.max_turns, 4);
        assert_eq!(cfg.max_context_tokens, 64_000);
        assert_eq!(cfg.context_compact_pct, 65);
        assert_eq!(cfg.context_reserve_tokens, 12_000);
        assert_eq!(cfg.context_compact_tail_tokens, 16_000);
        assert_eq!(cfg.context_compact_max_output_tokens, 4_096);
        assert!(cfg.context_compact_input_reduction);
        assert_eq!(cfg.tool_timeout_secs, 5);
        assert_eq!(cfg.sub_agent_timeout_secs, 6);
        assert_eq!(cfg.llm_first_event_timeout_secs, 7);
        assert_eq!(cfg.llm_idle_timeout_secs, 8);
        assert_eq!(cfg.llm_wait_heartbeat_secs, 9);
        assert!(cfg.verbose);
        assert_eq!(cfg.output_format, OutputFormat::Human);
        assert_eq!(cfg.session_id, "outer");
        assert_eq!(cfg.mission_content.as_deref(), Some("mission"));
        assert_eq!(
            cfg.enabled_tools,
            Some(vec!["Read".to_string(), "Bash".to_string()])
        );
        assert_eq!(
            cfg.skills,
            vec!["debugging".to_string(), "verification".to_string()]
        );
        let runtime_skills = runtime_skills_from_sdk_request(&req);
        assert_eq!(runtime_skills.len(), 1);
        assert_eq!(runtime_skills[0].name, "company-policy");
        assert_eq!(
            runtime_skills[0].exposure,
            CapabilityExposure::ModelAddressable
        );
        assert_eq!(runtime_skills[0].revision.as_deref(), Some("rev-1"));
        assert_eq!(
            skill_discovery_policy_from_sdk_request(&req),
            Some(SkillDiscoveryPolicy::RuntimeOnly)
        );
        assert_eq!(
            req.options.session_layout,
            Some(crate::runtime::SessionLayout::HomeScoped)
        );
        assert!(cfg.cli_overrides.model);
        assert!(cfg.cli_overrides.max_tokens);
        assert!(cfg.cli_overrides.max_turns);
        assert!(cfg.cli_overrides.max_context_tokens);
        assert!(cfg.cli_overrides.tool_timeout_secs);
    }

    #[test]
    fn sdk_max_context_override_is_checked_with_merged_compaction_defaults() {
        let req = crate::sdk_protocol::parse_agent_jsonl_request(
            r#"{"prompt":"hi","options":{"max_context":64000}}"#,
        )
        .unwrap();
        let mut cfg = Config::default();

        apply_sdk_request_options(&mut cfg, &req);

        let error = crate::config::validate_runtime_config(&cfg)
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("context_reserve_tokens (64000) must be less than max_context (64000)"),
            "{error}"
        );
    }

    #[test]
    fn turn_status_maps_to_sdk_status_and_exit_code() {
        assert_eq!(sdk_status_from_turn(TurnStatus::Ok), SdkStatus::Ok);
        assert_eq!(sdk_status_from_turn(TurnStatus::Failed), SdkStatus::Failed);
        assert_eq!(
            sdk_status_from_turn(TurnStatus::Interrupted),
            SdkStatus::Interrupted
        );
        assert_eq!(
            sdk_status_from_turn(TurnStatus::MaxTurnsExceeded),
            SdkStatus::MaxTurnsExceeded
        );
        assert_eq!(exit_code_from_turn(TurnStatus::Ok), 0);
        assert_eq!(exit_code_from_turn(TurnStatus::Failed), 1);
        assert_eq!(exit_code_from_turn(TurnStatus::Interrupted), 130);
        assert_eq!(exit_code_from_turn(TurnStatus::MaxTurnsExceeded), 2);
    }

    #[test]
    fn final_from_outcome_contains_all_required_fields() {
        use std::path::PathBuf;
        let session = SessionInfo {
            session_id: "sid-1".into(),
            session_ref: "ref-1".into(),
            is_new: true,
            home: PathBuf::from("/tmp/home"),
            cwd: PathBuf::from("/tmp/cwd"),
            events_path: PathBuf::from("/tmp/home/sid-1/events.jsonl"),
            conversation_path: PathBuf::from("/tmp/home/sid-1/conversation.jsonl"),
            artifacts_dir: PathBuf::from("/tmp/home/sid-1/artifacts"),
            summary_path: PathBuf::from("/tmp/home/sid-1/summary.json"),
            usage_path: PathBuf::from("/tmp/home/sid-1/usage.jsonl"),
            plan_path: PathBuf::from("/tmp/home/sid-1/plan.md"),
            plan_draft_path: PathBuf::from("/tmp/home/sid-1/plan.draft"),
        };
        let outcome = TurnOutcome {
            billing_turn_id: "turn-1".into(),
            status: TurnStatus::Ok,
            session: session.clone(),
            text: "hello".into(),
            thinking: "hmm".into(),
            tool_call_count: 3,
            tool_error_count: 1,
            error: None,
            usage_records: Vec::new(),
            usage: Default::default(),
        };

        let final_json = serde_json::to_value(final_from_outcome(&outcome)).unwrap();
        assert_eq!(final_json["type"], "final");
        assert_eq!(final_json["version"], PROTOCOL_VERSION);
        assert_eq!(final_json["session_id"], "sid-1");
        assert_eq!(final_json["session_ref"], "ref-1");
        assert_eq!(final_json["home"], "/tmp/home");
        assert_eq!(final_json["cwd"], "/tmp/cwd");
        assert_eq!(final_json["tool_call_count"], 3);
        assert_eq!(final_json["tool_error_count"], 1);
        assert!(final_json["error"].is_null());
    }

    #[test]
    fn final_fields_match_python_sdk_contract() {
        // Python SDK reads these fields from the final JSON line.
        // Every mink-core --agent-jsonl run emits these keys.
        let expected_keys = &[
            "type",
            "version",
            "status",
            "billing_turn_id",
            "session_id",
            "session_ref",
            "home",
            "cwd",
            "events_path",
            "conversation_path",
            "artifacts_dir",
            "summary_path",
            "usage_path",
            "usage_records",
            "usage",
            "tool_call_count",
            "tool_error_count",
            "error",
        ];
        let session = SessionInfo {
            session_id: "sid".into(),
            session_ref: "ref".into(),
            is_new: false,
            home: "/h".into(),
            cwd: "/c".into(),
            events_path: "/h/sid/events.jsonl".into(),
            conversation_path: "/h/sid/conversation.jsonl".into(),
            artifacts_dir: "/h/sid/artifacts".into(),
            summary_path: "/h/sid/summary.json".into(),
            usage_path: "/h/sid/usage.jsonl".into(),
            plan_path: "/h/sid/plan.md".into(),
            plan_draft_path: "/h/sid/plan.draft".into(),
        };
        let outcome = TurnOutcome {
            billing_turn_id: "turn-2".into(),
            status: TurnStatus::Failed,
            session,
            text: String::new(),
            thinking: String::new(),
            tool_call_count: 0,
            tool_error_count: 5,
            error: Some("something broke".into()),
            usage_records: Vec::new(),
            usage: Default::default(),
        };
        let final_json = serde_json::to_value(final_from_outcome(&outcome)).unwrap();
        for key in expected_keys {
            assert!(
                final_json.as_object().unwrap().contains_key(*key),
                "final JSON missing key: {key}"
            );
        }
    }
}
