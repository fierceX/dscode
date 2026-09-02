use super::*;

#[test]
fn sdk_request_defaults_version_and_options() {
    let req: SdkRequest = serde_json::from_str(r#"{"prompt":"hi"}"#).unwrap();
    assert_eq!(req.version, Some(PROTOCOL_VERSION));
    assert_eq!(req.prompt, "hi");
    assert_eq!(req.options.session.session_layout, None);
    assert_eq!(req.options.tools.skills, None);
    assert_eq!(req.options.tools.inline_skills, None);
    assert_eq!(req.options.tools.skill_discovery_policy, None);
    assert_eq!(req.options.context.max_context, None);
    assert_eq!(req.options.context.context_compact_pct, None);
}

#[test]
fn sdk_request_accepts_session_layout() {
    let req = parse_agent_jsonl_request(
        r#"{"prompt":"hi","options":{"session":{"session_layout":"home"}}}"#,
    )
    .unwrap();
    assert_eq!(
        req.options.session.session_layout,
        Some(SessionLayout::HomeScoped)
    );

    let req = parse_agent_jsonl_request(
        r#"{"prompt":"hi","options":{"session":{"session_layout":"isolated"}}}"#,
    )
    .unwrap();
    assert_eq!(
        req.options.session.session_layout,
        Some(SessionLayout::Isolated)
    );
}

#[test]
fn sdk_request_accepts_selected_skills() {
    let req = parse_agent_jsonl_request(
        r#"{"prompt":"hi","options":{"tools":{"skills":["debugging","verification"]}}}"#,
    )
    .unwrap();
    assert_eq!(
        req.options.tools.skills,
        Some(vec!["debugging".to_string(), "verification".to_string()])
    );
}

#[test]
fn sdk_request_accepts_inline_skills_and_policy() {
    let req = parse_agent_jsonl_request(
        r#"{
                "prompt":"hi",
                "options":{
                    "tools":{
                        "inline_skills":[{
                            "name":"company-policy",
                            "description":"Company policy",
                            "content":"private policy",
                            "exposure":"model_addressable",
                            "revision":"rev-1"
                        }],
                        "skill_discovery_policy":"runtime_only"
                    }
                }
            }"#,
    )
    .unwrap();
    let inline = req.options.tools.inline_skills.as_ref().unwrap();
    assert_eq!(inline[0].name, "company-policy");
    assert_eq!(
        inline[0].exposure,
        Some(SdkCapabilityExposure::ModelAddressable)
    );
    assert_eq!(
        req.options.tools.skill_discovery_policy,
        Some(SdkSkillDiscoveryPolicy::RuntimeOnly)
    );
}

#[test]
fn validate_sdk_request_rejects_invalid_inline_skill() {
    let req = parse_agent_jsonl_request(
            r#"{"prompt":"hi","options":{"tools":{"inline_skills":[{"name":"../secret","content":"x"}]}}}"#,
        )
        .unwrap();
    let err = validate_sdk_request(&req).unwrap_err();
    assert!(err.contains("invalid inline skill name"), "{err}");

    let req = parse_agent_jsonl_request(
        r#"{"prompt":"hi","options":{"tools":{"skills":[" debugging"]}}}"#,
    )
    .unwrap();
    let err = validate_sdk_request(&req).unwrap_err();
    assert!(err.contains("invalid skill name"), "{err}");

    let req = parse_agent_jsonl_request(
        r#"{"prompt":"hi","options":{"tools":{"inline_skills":[{"name":"empty","content":""}]}}}"#,
    )
    .unwrap();
    let err = validate_sdk_request(&req).unwrap_err();
    assert!(err.contains("content must be non-empty"), "{err}");
}

#[test]
fn sdk_request_rejects_unknown_session_layout() {
    let err = parse_agent_jsonl_request(
        r#"{"prompt":"hi","options":{"session":{"session_layout":"workspace"}}}"#,
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
        parse_agent_jsonl_request(r#"{"prompt":"hi","options":{"generation":{"max_tokens":0}}}"#)
            .unwrap();
    let err = validate_sdk_request(&req).unwrap_err();
    assert!(err.contains("max_tokens must be greater than 0"));

    let req = parse_agent_jsonl_request(
        r#"{"prompt":"hi","options":{"context":{"context_compact_pct":0}}}"#,
    )
    .unwrap();
    let err = validate_sdk_request(&req).unwrap_err();
    assert!(err.contains("context_compact_pct must be between 1 and 100"));

    let req = parse_agent_jsonl_request(
        r#"{"prompt":"hi","options":{"context":{"context_reserve_tokens":0}}}"#,
    )
    .unwrap();
    let err = validate_sdk_request(&req).unwrap_err();
    assert!(err.contains("context_reserve_tokens must be greater than 0"));

    let req = parse_agent_jsonl_request(
        r#"{"prompt":"hi","options":{"context":{"context_compact_tail_tokens":0}}}"#,
    )
    .unwrap();
    let err = validate_sdk_request(&req).unwrap_err();
    assert!(err.contains("context_compact_tail_tokens must be greater than 0"));

    let req = parse_agent_jsonl_request(
        r#"{"prompt":"hi","options":{"context":{"context_compact_max_output_tokens":0}}}"#,
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
                    "context":{
                        "max_context":64000,
                        "context_compact_pct":65,
                        "context_reserve_tokens":12000,
                        "context_compact_tail_tokens":16000,
                        "context_compact_max_output_tokens":4096,
                        "context_compact_input_reduction":true
                    }
                }
            }"#,
    )
    .unwrap();
    validate_sdk_request(&req).unwrap();
    assert_eq!(req.options.context.max_context, Some(64_000));
    assert_eq!(req.options.context.context_compact_pct, Some(65));
    assert_eq!(req.options.context.context_reserve_tokens, Some(12_000));
    assert_eq!(
        req.options.context.context_compact_tail_tokens,
        Some(16_000)
    );
    assert_eq!(
        req.options.context.context_compact_max_output_tokens,
        Some(4_096)
    );
    assert_eq!(
        req.options.context.context_compact_input_reduction,
        Some(true)
    );
}

#[test]
fn sdk_request_rejects_removed_plan_projection_tail() {
    let error = parse_agent_jsonl_request(
        r#"{"prompt":"hi","options":{"context":{"plan_projection_tail":false}}}"#,
    )
    .unwrap_err();
    assert!(error.contains("plan_projection_tail"), "{error}");
}

#[test]
fn validate_sdk_request_rejects_bad_tool_timeout_options() {
    let req =
        parse_agent_jsonl_request(r#"{"prompt":"hi","options":{"tools":{"tool_timeout":0}}}"#)
            .unwrap();
    let err = validate_sdk_request(&req).unwrap_err();
    assert!(err.contains("tool_timeout must be greater than 0"), "{err}");

    let req =
        parse_agent_jsonl_request(r#"{"prompt":"hi","options":{"tools":{"tool_timeout_max":4}}}"#)
            .unwrap();
    let err = validate_sdk_request(&req).unwrap_err();
    assert!(err.contains("tool_timeout_max must be at least 5"), "{err}");

    let req = parse_agent_jsonl_request(
        r#"{"prompt":"hi","options":{"tools":{"tool_timeout":30,"tool_timeout_max":900}}}"#,
    )
    .unwrap();
    validate_sdk_request(&req).unwrap();
    assert_eq!(req.options.tools.tool_timeout, Some(30));
    assert_eq!(req.options.tools.tool_timeout_max, Some(900));
}

#[test]
fn validate_sdk_request_rejects_bad_llm_timeout_options() {
    let req = parse_agent_jsonl_request(
        r#"{"prompt":"hi","options":{"generation":{"llm_idle_timeout":0}}}"#,
    )
    .unwrap();
    let err = validate_sdk_request(&req).unwrap_err();
    assert!(err.contains("llm_idle_timeout must be greater than 0"));

    let req = parse_agent_jsonl_request(
        r#"{"prompt":"hi","options":{"generation":{"llm_wait_heartbeat":-1}}}"#,
    )
    .unwrap();
    let err = validate_sdk_request(&req).unwrap_err();
    assert!(err.contains("llm_wait_heartbeat must be zero or greater"));
}

#[test]
fn validate_sdk_request_accepts_custom_model() {
    let req =
        parse_agent_jsonl_request(r#"{"prompt":"hi","options":{"provider":{"model":"gpt-4"}}}"#)
            .unwrap();
    validate_sdk_request(&req).unwrap();
}

#[test]
fn validate_sdk_request_rejects_empty_model() {
    let req = parse_agent_jsonl_request(r#"{"prompt":"hi","options":{"provider":{"model":" "}}}"#)
        .unwrap();
    let err = validate_sdk_request(&req).unwrap_err();
    assert!(err.contains("model must not be empty"));
}

#[test]
fn sdk_request_rejects_flat_legacy_options() {
    let error = parse_agent_jsonl_request(r#"{"prompt":"hi","options":{"max_context":64000}}"#)
        .unwrap_err();
    assert!(error.contains("unknown field `max_context`"), "{error}");
}
