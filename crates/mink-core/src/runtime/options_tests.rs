use super::*;

#[test]
fn options_convert_to_runtime_config_without_losing_config_fields() {
    let runtime_config = AgentOptions::new("/tmp/mink-home", "/tmp/project")
        .with_model("pro")
        .with_api_key("test-key")
        .with_base_url("https://example.invalid/v1")
        .with_session(SessionPolicy::UseOrCreate("work".to_string()))
        .with_max_tokens(123)
        .with_max_turns(7)
        .with_max_context_tokens(456)
        .with_context_compact_pct(70)
        .with_context_reserve_tokens(71)
        .with_context_compact_tail_tokens(72)
        .with_context_compact_max_output_tokens(73)
        .with_context_compact_input_reduction(true)
        .with_tool_timeout_secs(10)
        .with_sub_agent_timeout_secs(11)
        .with_llm_timeouts(12, 13, 14)
        .with_tool_result_max_bytes(15)
        .with_file_write_max_bytes(16)
        .with_search_limits(17, 18)
        .with_output_format(OutputFormat::StreamJson)
        .with_verbose(true)
        .with_log_events(false)
        .with_openai_reasoning_effort("high")
        .with_openai_include_usage(false)
        .with_openai_token_param(TokenParamKind::MaxCompletionTokens)
        .with_openai_tool_choice("auto")
        .with_openai_extra_body(BTreeMap::from([(
            "enable_thinking".to_string(),
            serde_json::json!(true),
        )]))
        .with_runtime_skill_content("runtime-rust", "Runtime Rust", "runtime body")
        .with_skill_discovery_policy(SkillDiscoveryPolicy::RuntimeOnly)
        .with_selected_skills(["runtime-rust"])
        .with_mission_content("mission")
        .with_enabled_tools(vec!["Read".to_string(), "Bash".to_string()])
        .with_tool_approval_mode(ToolApprovalMode::Write)
        .with_first_prompt("hello")
        .into_runtime_config();

    assert_eq!(runtime_config.home, PathBuf::from("/tmp/mink-home"));
    assert_eq!(runtime_config.cwd, PathBuf::from("/tmp/project"));
    assert_eq!(runtime_config.session_layout, SessionLayout::Isolated);
    assert_eq!(
        runtime_config.skill_discovery_policy,
        SkillDiscoveryPolicy::RuntimeOnly
    );
    assert_eq!(runtime_config.runtime_skills.len(), 1);
    assert_eq!(runtime_config.runtime_skills[0].name, "runtime-rust");
    assert!(matches!(
        runtime_config.session,
        SessionPolicy::UseOrCreate(ref value) if value == "work"
    ));
    assert_eq!(runtime_config.first_prompt.as_deref(), Some("hello"));

    let cfg = runtime_config.config;
    assert_eq!(cfg.model, "pro");
    assert_eq!(cfg.api_key, "test-key");
    assert_eq!(cfg.base_url, "https://example.invalid/v1");
    assert_eq!(cfg.max_tokens, 123);
    assert_eq!(cfg.max_turns, 7);
    assert_eq!(cfg.max_context_tokens, 456);
    assert_eq!(cfg.context_compact_pct, 70);
    assert_eq!(cfg.context_reserve_tokens, 71);
    assert_eq!(cfg.context_compact_tail_tokens, 72);
    assert_eq!(cfg.context_compact_max_output_tokens, 73);
    assert!(cfg.context_compact_input_reduction);
    assert_eq!(cfg.tool_timeout_secs, 10);
    assert_eq!(cfg.sub_agent_timeout_secs, 11);
    assert_eq!(cfg.llm_first_event_timeout_secs, 12);
    assert_eq!(cfg.llm_idle_timeout_secs, 13);
    assert_eq!(cfg.llm_wait_heartbeat_secs, 14);
    assert_eq!(cfg.tool_result_max_bytes, 15);
    assert_eq!(cfg.file_write_max_bytes, 16);
    assert_eq!(cfg.max_search_files, 17);
    assert_eq!(cfg.max_search_results, 18);
    assert_eq!(cfg.output_format, OutputFormat::StreamJson);
    assert!(cfg.verbose);
    assert!(!cfg.log_events);
    assert_eq!(cfg.openai_reasoning_effort.as_deref(), Some("high"));
    assert!(!cfg.openai_include_usage);
    assert_eq!(cfg.openai_token_param, TokenParamKind::MaxCompletionTokens);
    assert_eq!(cfg.openai_tool_choice, Some(serde_json::json!("auto")));
    assert_eq!(
        cfg.openai_extra_body["enable_thinking"],
        serde_json::json!(true)
    );
    assert_eq!(cfg.skills, vec!["runtime-rust"]);
    assert_eq!(cfg.mission_content.as_deref(), Some("mission"));
    assert_eq!(
        cfg.enabled_tools,
        Some(vec!["Read".to_string(), "Bash".to_string()])
    );
    assert_eq!(cfg.tool_approval_mode, ToolApprovalMode::Write);
    // per-tool approval 的 options 便捷方法已删除（零外部调用），
    // 嵌入方经 Config/CLI [tools.approval] 配置。
}

#[test]
fn options_use_typed_runtime_controls() {
    let runtime_config = AgentOptions::new("/tmp/mink-home", "/tmp/project")
        .with_first_prompt("metadata")
        .with_session(SessionPolicy::UseOrCreate("typed-session".into()))
        .into_runtime_config();

    assert_eq!(runtime_config.first_prompt.as_deref(), Some("metadata"));
    assert!(matches!(
        runtime_config.session,
        SessionPolicy::UseOrCreate(ref value) if value == "typed-session"
    ));
}

#[test]
fn options_explicit_first_prompt_is_metadata_only() {
    let runtime_config = AgentOptions::new("/tmp/mink-home", "/tmp/project")
        .with_first_prompt("metadata only")
        .into_runtime_config();

    assert!(runtime_config.config.prompt.is_empty());
    assert_eq!(
        runtime_config.first_prompt.as_deref(),
        Some("metadata only")
    );
}

#[test]
fn options_preserve_session_selection_semantics() {
    let runtime_config = AgentOptions::new("/tmp/mink-home", "/tmp/project")
        .with_session(SessionPolicy::UseOrCreate("existing-or-alias".into()))
        .into_runtime_config();
    assert!(matches!(
        runtime_config.session,
        SessionPolicy::UseOrCreate(ref value) if value == "existing-or-alias"
    ));

    let runtime_config = AgentOptions::new("/tmp/mink-home", "/tmp/project")
        .with_session(SessionPolicy::ContinueLatest)
        .into_runtime_config();
    assert!(matches!(
        runtime_config.session,
        SessionPolicy::ContinueLatest
    ));
}

#[test]
fn options_explicit_session_is_authoritative() {
    let runtime_config = AgentOptions::new("/tmp/mink-home", "/tmp/project")
        .with_session(SessionPolicy::New)
        .into_runtime_config();

    assert!(matches!(runtime_config.session, SessionPolicy::New));
}

#[test]
fn options_can_override_session_layout() {
    let runtime_config = AgentOptions::new("/tmp/mink-home", "/tmp/project")
        .with_home_scoped_sessions()
        .into_runtime_config();
    assert_eq!(runtime_config.session_layout, SessionLayout::HomeScoped);

    let runtime_config = AgentOptions::new("/tmp/mink-home", "/tmp/project")
        .with_project_scoped_sessions()
        .into_runtime_config();
    assert_eq!(runtime_config.session_layout, SessionLayout::ProjectScoped);

    let runtime_config = AgentOptions::new("/tmp/mink-home", "/tmp/project")
        .with_direct_sessions()
        .into_runtime_config();
    assert_eq!(runtime_config.session_layout, SessionLayout::Direct);
}
