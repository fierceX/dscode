use super::*;

#[test]
fn cli_defaults_track_runtime_option_defaults() {
    let cli = CliConfig::default();

    let provider = crate::runtime::ProviderOptions::default();
    assert_eq!(cli.model, provider.model);
    assert_eq!(cli.model_aliases, provider.model_aliases);
    assert_eq!(cli.api_key, provider.api_key);
    assert_eq!(cli.base_url, provider.base_url);
    assert_eq!(cli.openai_reasoning_effort, provider.reasoning_effort);
    assert_eq!(cli.openai_include_usage, provider.include_usage);
    assert_eq!(cli.openai_token_param, provider.token_param);
    assert_eq!(cli.openai_tool_choice, provider.tool_choice);
    assert_eq!(cli.openai_extra_body, provider.extra_body);

    let generation = crate::runtime::GenerationOptions::default();
    assert_eq!(cli.max_tokens, generation.max_tokens);
    assert_eq!(cli.max_turns, generation.max_turns);
    assert_eq!(
        cli.llm_first_event_timeout_secs,
        generation.first_event_timeout_secs
    );
    assert_eq!(cli.llm_idle_timeout_secs, generation.idle_timeout_secs);
    assert_eq!(cli.llm_wait_heartbeat_secs, generation.wait_heartbeat_secs);

    let context = crate::runtime::ContextPolicy::default();
    assert_eq!(cli.max_context_tokens, context.max_context_tokens);
    assert_eq!(cli.context_compact_pct, context.compact_pct);
    assert_eq!(cli.context_reserve_tokens, context.reserve_tokens);
    assert_eq!(cli.context_compact_tail_tokens, context.compact_tail_tokens);
    assert_eq!(
        cli.context_compact_max_output_tokens,
        context.compact_max_output_tokens
    );
    assert_eq!(
        cli.context_compact_input_reduction,
        context.compact_input_reduction
    );
    assert_eq!(cli.plan_projection_tail, context.plan_projection_tail);

    let tools = crate::runtime::ToolOptions::default();
    assert_eq!(cli.tool_timeout_secs, tools.timeout_secs);
    assert_eq!(cli.tool_timeout_max_secs, tools.timeout_max_secs);
    assert_eq!(cli.sub_agent_timeout_secs, tools.sub_agent_timeout_secs);
    assert_eq!(cli.tool_result_max_bytes, tools.result_max_bytes);
    assert_eq!(cli.file_write_max_bytes, tools.file_write_max_bytes);
    assert_eq!(cli.edit_mode, tools.edit_mode);
    assert_eq!(cli.edit_fuzzy_match, tools.edit_fuzzy_match);
    assert_eq!(cli.edit_fuzzy_threshold, tools.edit_fuzzy_threshold);
    assert_eq!(cli.edit_enforce_seen_lines, tools.edit_enforce_seen_lines);
    assert_eq!(cli.max_search_files, tools.max_search_files);
    assert_eq!(cli.max_search_results, tools.max_search_results);
    assert_eq!(cli.enabled_tools, tools.enabled_tools);
    assert_eq!(cli.tool_approval_mode, tools.approval_mode);
    assert_eq!(cli.tool_approval, tools.approval);
}

#[test]
fn parse_size_bytes_plain() {
    assert_eq!(parse_size_bytes("100").unwrap(), 100);
    assert_eq!(parse_size_bytes("0").unwrap(), 0);
}

#[test]
fn parse_size_bytes_k() {
    assert_eq!(parse_size_bytes("1k").unwrap(), 1000);
    assert_eq!(parse_size_bytes("50k").unwrap(), 50_000);
}

#[test]
fn parse_size_bytes_m() {
    assert_eq!(parse_size_bytes("1m").unwrap(), 1_000_000);
    assert_eq!(parse_size_bytes("5M").unwrap(), 5_000_000);
}

#[test]
fn parse_size_bytes_g() {
    assert_eq!(parse_size_bytes("1g").unwrap(), 1_000_000_000);
}

#[test]
fn parse_size_bytes_empty_error() {
    assert!(parse_size_bytes("").is_err());
}

#[test]
fn parse_args_model_provider() {
    let cfg = parse_args(vec!["-m".into(), "flash".into()]).unwrap();
    assert_eq!(cfg.model, "flash");
}

#[test]
fn parse_args_model_accepts_custom_model_name() {
    let cfg = parse_args(vec!["-m".into(), "gpt-4.1".into()]).unwrap();
    assert_eq!(cfg.model, "gpt-4.1");
}

#[test]
fn parse_args_config_rejects_invalid_toml() {
    let err = parse_args(vec!["--config".into(), "max_tokens =".into()])
        .unwrap_err()
        .to_string();
    assert!(err.contains("TOML"), "{err}");
}

#[test]
fn parse_args_flags() {
    let cfg = parse_args(vec!["-v".into(), "-i".into(), "--print".into()]).unwrap();
    assert!(cfg.verbose);
    assert!(cfg.interactive);
    assert_eq!(cfg.output_format, OutputFormat::StreamJson);
    // --print marks the output-format flag so [generation] TOML cannot
    // outrank the explicit CLI choice.
    assert!(cfg.cli_overrides.output_format);
}

#[test]
fn parse_args_skill_flag_collects_unique_names() {
    let cfg = parse_args(vec![
        "--skill".into(),
        "debugging".into(),
        "--skill".into(),
        "review".into(),
        "--skill".into(),
        "debugging".into(),
    ])
    .unwrap();
    assert_eq!(cfg.skills, vec!["debugging", "review"]);
    assert!(cfg.cli_overrides.skills);

    let err = parse_args(vec!["--skill".into(), "  ".into()]).unwrap_err();
    assert!(err.to_string().contains("skill name must not be empty"));
}

#[test]
fn parse_args_prefab_accepts_default_and_path() {
    let default = parse_args(vec!["--prefab".into()]).unwrap();
    assert_eq!(default.prefab.as_deref(), Some("default"));

    let path = parse_args(vec!["--prefab=./templates/custom".into()]).unwrap();
    assert_eq!(path.prefab.as_deref(), Some("./templates/custom"));

    let empty = parse_args(vec!["--prefab=".into()]).unwrap_err();
    assert!(empty.to_string().contains("missing value for --prefab"));
}

#[test]
fn skill_flag_outranks_file_and_sdk_skills() {
    let mut cfg = parse_args(vec!["--skill".into(), "from_cli".into()]).unwrap();
    let defaults = CliConfig::default();
    let file = MinkConfigFile {
        tools: ToolsConfigFile {
            skills: Some(vec!["from_file".into()]),
            ..Default::default()
        },
        ..Default::default()
    };
    apply_config_sources(&mut cfg, &defaults, Some(&file), Some(&file), Some(&file));
    assert_eq!(cfg.skills, vec!["from_cli"]);

    let request = mink::sdk_protocol::SdkRequest {
        options: mink::sdk_protocol::SdkOptions {
            tools: mink::sdk_protocol::SdkToolOptions {
                skills: Some(vec!["from_sdk".into()]),
                ..Default::default()
            },
            ..Default::default()
        },
        ..Default::default()
    };
    super::apply_sdk_request_options(&mut cfg, &request);
    assert_eq!(cfg.skills, vec!["from_cli"]);
}

#[test]
fn env_size_limits_lose_to_config_files() {
    // Documented priority: CLI > --config > project .minkrc > user .minkrc >
    // env > defaults. Env limits are applied before file sources, so a
    // [tools] value in any file layer outranks the env value.
    let defaults = CliConfig::default();
    let mut cfg = defaults.clone();
    super::apply_size_limit_envs(&mut cfg, Some(111), None, Some(222), Some(333));
    assert_eq!(cfg.tool_result_max_bytes, 111);
    assert_eq!(cfg.max_search_files, 222);
    assert_eq!(cfg.max_search_results, 333);

    let project = MinkConfigFile {
        tools: ToolsConfigFile {
            max_search_files: Some(9_999),
            ..Default::default()
        },
        ..Default::default()
    };
    apply_config_sources(&mut cfg, &defaults, None, Some(&project), None);
    assert_eq!(cfg.max_search_files, 9_999);
    assert_eq!(cfg.max_search_results, 333);
    assert_eq!(cfg.tool_result_max_bytes, 111);
}

#[test]
fn parse_args_selects_full_and_inline_tui_modes() {
    assert_eq!(
        parse_args(vec!["--tui".into()]).unwrap().tui_mode,
        TuiMode::Full
    );
    assert_eq!(
        parse_args(vec!["--tui=full".into()]).unwrap().tui_mode,
        TuiMode::Full
    );
    assert_eq!(
        parse_args(vec!["--tui=inline".into()]).unwrap().tui_mode,
        TuiMode::Inline
    );
}

#[test]
fn parse_args_enabled_tools_is_the_only_tool_selection_cli() {
    let selected = parse_args(vec![
        "--enabled-tools".into(),
        "Read, Bash,PythonSandbox".into(),
    ])
    .unwrap();
    assert_eq!(
        selected.enabled_tools,
        Some(vec!["Read".into(), "Bash".into(), "PythonSandbox".into()])
    );

    let none = parse_args(vec!["--enabled-tools".into(), "none".into()]).unwrap();
    assert_eq!(none.enabled_tools, Some(Vec::new()));
    assert!(parse_args(vec!["--disable-bash".into()]).is_err());
}

#[test]
fn parse_args_session_accepts_separate_and_equals_forms() {
    let separate = parse_args(vec!["--session".into(), "feature-x".into()]).unwrap();
    assert_eq!(separate.session_id, "feature-x");

    let equals = parse_args(vec!["--session=feature-x".into()]).unwrap();
    assert_eq!(equals.session_id, "feature-x");

    let empty = parse_args(vec!["--session=".into()]).unwrap_err();
    assert!(empty.to_string().contains("missing value for --session"));
}

#[test]
fn parse_args_session_requires_value() {
    let at_end = parse_args(vec!["--session".into()]).unwrap_err();
    assert!(at_end.to_string().contains("missing value for --session"));

    let before_flag = parse_args(vec!["--session".into(), "--continue".into()]).unwrap_err();
    assert!(
        before_flag
            .to_string()
            .contains("missing value for --session")
    );
}

#[test]
fn parse_args_rejects_multiple_prompts() {
    let err = parse_args(vec!["first".into(), "second".into()]).unwrap_err();
    assert!(
        err.to_string()
            .contains("unexpected extra argument: second")
    );
}

#[test]
fn parse_args_option_like_token_is_not_consumed_as_value() {
    let err = parse_args(vec!["--model".into(), "--session".into()]).unwrap_err();
    assert!(err.to_string().contains("missing value for --model"));
}

#[test]
fn parse_args_agent_jsonl_enables_single_shot_protocol() {
    let cfg = parse_args(vec!["--agent-jsonl".into()]).unwrap();
    assert!(cfg.agent_jsonl);
    assert_eq!(cfg.output_format, OutputFormat::StreamJson);
}

#[test]
fn agent_jsonl_applies_cli_config_without_file_io() {
    let toml = "[tools]\nmax_search_files = 15000\nmax_search_results = 10000";
    let mut cfg = parse_args(vec!["--agent-jsonl".into(), "--config".into(), toml.into()]).unwrap();
    apply_config_file(&mut cfg).unwrap();
    assert_eq!(cfg.max_search_files, 15000);
    assert_eq!(cfg.max_search_results, 10000);
}

#[test]
fn parse_args_json_rpc_is_removed() {
    let err = parse_args(vec!["--json-rpc".into()]).unwrap_err();
    assert!(err.to_string().contains("unknown option"));
}

#[test]
fn parse_args_approval_mode() {
    let toml = "[tools]\napproval_mode = \"write\"";
    let mut cfg = parse_args(vec!["--config".into(), toml.into()]).unwrap();
    let defaults = CliConfig::default();
    let cli = cfg.cli_config.take();
    apply_config_sources(&mut cfg, &defaults, None, None, cli.as_ref());
    cfg.cli_config = cli;
    assert_eq!(cfg.tool_approval_mode, ToolApprovalMode::Write);
}

#[test]
fn parse_args_prompt() {
    let cfg = parse_args(vec!["hello world".into()]).unwrap();
    assert_eq!(cfg.prompt, "hello world");
}

#[test]
fn parse_args_unknown_flag_error() {
    assert!(parse_args(vec!["--unknown".into()]).is_err());
}

#[test]
fn parse_args_llm_timeout_via_config() {
    let toml =
        "[generation]\nllm_first_event_timeout = 7\nllm_idle_timeout = 8\nllm_wait_heartbeat = 9";
    let mut cfg = parse_args(vec!["--config".into(), toml.into()]).unwrap();
    let defaults = CliConfig::default();
    let cli = cfg.cli_config.take();
    apply_config_sources(&mut cfg, &defaults, None, None, cli.as_ref());
    cfg.cli_config = cli;
    assert_eq!(cfg.llm_first_event_timeout_secs, 7);
    assert_eq!(cfg.llm_idle_timeout_secs, 8);
    assert_eq!(cfg.llm_wait_heartbeat_secs, 9);
}

#[test]
fn config_llm_timeout_via_toml() {
    let mut cfg = parse_args(vec![
        "--config".into(),
        "[generation]\nllm_wait_heartbeat = 0".into(),
    ])
    .unwrap();
    let defaults = CliConfig::default();
    let cli = cfg.cli_config.take();
    apply_config_sources(&mut cfg, &defaults, None, None, cli.as_ref());
    cfg.cli_config = cli;
    assert_eq!(cfg.llm_wait_heartbeat_secs, 0);
}

#[test]
fn parse_config_file_overrides_model() {
    let toml_str = r#"
[provider]
model = "pro"
[generation]
max_tokens = 163840
llm_first_event_timeout = 11
llm_idle_timeout = 22
llm_wait_heartbeat = 3
[context]
max_context = "500K"
[tools]
tool_timeout = 120
"#;
    let parsed: MinkConfigFile = toml::from_str(toml_str).unwrap();
    assert_eq!(parsed.provider.model.unwrap(), "pro");
    assert_eq!(parsed.generation.max_tokens.unwrap(), 163840);
    assert_eq!(parsed.context.max_context.unwrap(), "500K");
    assert_eq!(parsed.tools.tool_timeout.unwrap(), 120);
    assert_eq!(parsed.generation.llm_first_event_timeout.unwrap(), 11);
    assert_eq!(parsed.generation.llm_idle_timeout.unwrap(), 22);
    assert_eq!(parsed.generation.llm_wait_heartbeat.unwrap(), 3);
}

#[test]
fn parse_config_file_openai_compatible_options() {
    let toml_str = r#"
[provider]
openai_reasoning_effort = "off"
openai_include_usage = false
openai_token_param = "max_completion_tokens"
openai_tool_choice = "auto"

[provider.openai_extra_body]
enable_thinking = true
thinking_budget = 8192
temperature = 0.2
"#;
    let parsed: MinkConfigFile = toml::from_str(toml_str).unwrap();
    assert_eq!(parsed.provider.openai_reasoning_effort.unwrap(), "off");
    assert_eq!(parsed.provider.openai_include_usage, Some(false));
    assert_eq!(
        parsed.provider.openai_token_param.unwrap(),
        "max_completion_tokens".to_string()
    );
    assert_eq!(
        parsed.provider.openai_tool_choice.unwrap(),
        serde_json::json!("auto")
    );
    let extra_body = parsed.provider.openai_extra_body.unwrap();
    assert_eq!(extra_body["enable_thinking"], serde_json::json!(true));
    assert_eq!(extra_body["thinking_budget"], serde_json::json!(8192));
    assert_eq!(extra_body["temperature"], serde_json::json!(0.2));
}

#[test]
fn parse_config_file_partial_fields() {
    // Only setting one field should not require others
    let toml_str = "[generation]\nlog_events = false";
    let parsed: MinkConfigFile = toml::from_str(toml_str).unwrap();
    assert!(!parsed.generation.log_events.unwrap());
    assert!(parsed.provider.model.is_none());
    assert!(parsed.provider.api_key.is_none());
    assert!(toml::from_str::<MinkConfigFile>("log_events = false").is_err());
}

#[test]
fn parse_config_file_tools_approval() {
    let toml_str = r#"
[tools]
approval_mode = "write"

[tools.approval]
Bash = "prompt"
Read = "allow"
"#;
    let parsed: MinkConfigFile = toml::from_str(toml_str).unwrap();
    let tools = parsed.tools;
    assert_eq!(tools.approval_mode.unwrap(), ToolApprovalMode::Write);
    let approval = tools.approval.unwrap();
    assert_eq!(approval["Bash"], ToolApprovalPolicy::Prompt);
    assert_eq!(approval["Read"], ToolApprovalPolicy::Allow);
}

#[test]
fn config_cli_flags_beat_project_config() {
    let defaults = CliConfig::default();
    let project = MinkConfigFile {
        provider: ProviderConfigFile {
            model: Some("pro".into()),
            ..Default::default()
        },
        generation: GenerationConfigFile {
            max_turns: Some(99),
            ..Default::default()
        },
        ..Default::default()
    };
    let mut cfg = CliConfig {
        model: "flash".into(),
        max_turns: 12,
        // `--model flash` marks the flag; max_turns has no CLI flag, so the
        // TOML layer legitimately owns it (12 was only a pre-set default).
        cli_overrides: CliOverrides {
            model: true,
            ..Default::default()
        },
        ..Default::default()
    };
    apply_config_sources(&mut cfg, &defaults, None, Some(&project), None);
    assert_eq!(cfg.model, "flash");
    assert_eq!(cfg.max_turns, 99);
}

#[test]
fn config_project_overrides_user_config() {
    let defaults = CliConfig::default();
    let user = MinkConfigFile {
        provider: ProviderConfigFile {
            api_key: Some("user-key".into()),
            model: Some("flash".into()),
            ..Default::default()
        },
        generation: GenerationConfigFile {
            max_turns: Some(10),
            ..Default::default()
        },
        ..Default::default()
    };
    let project = MinkConfigFile {
        provider: ProviderConfigFile {
            api_key: Some("project-key".into()),
            model: Some("pro".into()),
            ..Default::default()
        },
        generation: GenerationConfigFile {
            max_turns: Some(20),
            ..Default::default()
        },
        ..Default::default()
    };
    let mut cfg = CliConfig::default();
    apply_config_sources(&mut cfg, &defaults, Some(&user), Some(&project), None);
    assert_eq!(cfg.api_key, "project-key");
    assert_eq!(cfg.model, "pro");
    assert_eq!(cfg.max_turns, 20);
}

#[test]
fn config_user_overrides_default() {
    let defaults = CliConfig::default();
    let user = MinkConfigFile {
        provider: ProviderConfigFile {
            api_key: Some("user-key".into()),
            base_url: Some("https://user.example".into()),
            ..Default::default()
        },
        ..Default::default()
    };
    let mut cfg = CliConfig::default();
    apply_config_sources(&mut cfg, &defaults, Some(&user), None, None);
    assert_eq!(cfg.api_key, "user-key");
    assert_eq!(cfg.base_url, "https://user.example");
}

#[test]
fn config_file_sets_compaction_policy() {
    let defaults = CliConfig::default();
    let user = MinkConfigFile {
        context: ContextConfigFile {
            context_compact_pct: Some(72),
            context_reserve_tokens: Some(8_000),
            context_compact_tail_tokens: Some(12_000),
            context_compact_max_output_tokens: Some(2_048),
            context_compact_input_reduction: Some(true),
            ..Default::default()
        },
        ..Default::default()
    };
    let mut cfg = CliConfig::default();
    apply_config_sources(&mut cfg, &defaults, Some(&user), None, None);
    assert_eq!(cfg.context_compact_pct, 72);
    assert_eq!(cfg.context_reserve_tokens, 8_000);
    assert_eq!(cfg.context_compact_tail_tokens, 12_000);
    assert_eq!(cfg.context_compact_max_output_tokens, 2_048);
    assert!(cfg.context_compact_input_reduction);
}

#[test]
fn invalid_compaction_policy_keeps_defaults() {
    let defaults = CliConfig::default();
    let user = MinkConfigFile {
        context: ContextConfigFile {
            context_compact_pct: Some(0),
            context_reserve_tokens: Some(0),
            context_compact_tail_tokens: Some(0),
            context_compact_max_output_tokens: Some(0),
            ..Default::default()
        },
        ..Default::default()
    };
    let mut cfg = CliConfig::default();
    apply_config_sources(&mut cfg, &defaults, Some(&user), None, None);
    assert_eq!(cfg.context_compact_pct, defaults.context_compact_pct);
    assert_eq!(cfg.context_reserve_tokens, defaults.context_reserve_tokens);
    assert_eq!(
        cfg.context_compact_tail_tokens,
        defaults.context_compact_tail_tokens
    );
    assert_eq!(
        cfg.context_compact_max_output_tokens,
        defaults.context_compact_max_output_tokens
    );
}

#[test]
fn runtime_config_rejects_unusable_context_budget_combinations() {
    let mut cfg = CliConfig {
        max_context_tokens: 64_000,
        ..CliConfig::default()
    };
    let error = validate_runtime_config(&cfg).unwrap_err().to_string();
    assert!(
        error.contains("context_reserve_tokens (64000) must be less than max_context (64000)"),
        "{error}"
    );

    cfg.context_reserve_tokens = 12_000;
    let error = validate_runtime_config(&cfg).unwrap_err().to_string();
    assert!(
        error.contains("context_compact_tail_tokens (256000) must be less than"),
        "{error}"
    );

    cfg.context_compact_tail_tokens = 16_000;
    validate_runtime_config(&cfg).unwrap();

    cfg.context_compact_max_output_tokens = 64_000;
    let error = validate_runtime_config(&cfg).unwrap_err().to_string();
    assert!(
        error.contains(
            "context_compact_max_output_tokens (64000) must be less than max_context (64000)"
        ),
        "{error}"
    );
}

#[test]
fn runtime_config_allows_zero_context_window() {
    let cfg = CliConfig {
        max_context_tokens: 0,
        ..CliConfig::default()
    };
    validate_runtime_config(&cfg).unwrap();
}

#[test]
fn config_file_sets_openai_compatible_options() {
    let defaults = CliConfig::default();
    let project = MinkConfigFile {
        provider: ProviderConfigFile {
            openai_reasoning_effort: Some("off".into()),
            openai_include_usage: Some(false),
            openai_token_param: Some("max_completion_tokens".into()),
            openai_tool_choice: Some(serde_json::json!("auto")),
            openai_extra_body: Some(BTreeMap::from([
                ("enable_thinking".to_string(), serde_json::json!(true)),
                ("thinking_budget".to_string(), serde_json::json!(8192)),
            ])),
            ..Default::default()
        },
        ..Default::default()
    };
    let mut cfg = CliConfig::default();
    apply_config_sources(&mut cfg, &defaults, None, Some(&project), None);
    assert_eq!(cfg.openai_reasoning_effort, None);
    assert!(!cfg.openai_include_usage);
    assert_eq!(cfg.openai_token_param, TokenParamKind::MaxCompletionTokens);
    assert_eq!(cfg.openai_tool_choice, Some(serde_json::json!("auto")));
    assert_eq!(
        cfg.openai_extra_body["enable_thinking"],
        serde_json::json!(true)
    );
    assert_eq!(
        cfg.openai_extra_body["thinking_budget"],
        serde_json::json!(8192)
    );
}

#[test]
fn config_file_log_events_overrides_env_default() {
    let defaults = CliConfig::default();
    let project = MinkConfigFile {
        generation: GenerationConfigFile {
            log_events: Some(true),
            ..Default::default()
        },
        ..Default::default()
    };
    let mut cfg = CliConfig::default();
    apply_log_events_env_value(&mut cfg, "0");
    apply_config_sources(&mut cfg, &defaults, None, Some(&project), None);
    assert!(cfg.log_events);
}

#[test]
fn config_file_invalid_llm_timeouts_are_ignored() {
    let defaults = CliConfig::default();
    let project = MinkConfigFile {
        tools: ToolsConfigFile {
            tool_timeout: Some(0),
            tool_timeout_max: Some(4),
            sub_agent_timeout: Some(-5),
            ..Default::default()
        },
        generation: GenerationConfigFile {
            llm_first_event_timeout: Some(0),
            llm_idle_timeout: Some(-5),
            llm_wait_heartbeat: Some(-1),
            ..Default::default()
        },
        ..Default::default()
    };
    let mut cfg = CliConfig::default();
    apply_config_sources(&mut cfg, &defaults, None, Some(&project), None);
    assert_eq!(cfg.tool_timeout_secs, defaults.tool_timeout_secs);
    assert_eq!(cfg.tool_timeout_max_secs, defaults.tool_timeout_max_secs);
    assert_eq!(cfg.sub_agent_timeout_secs, defaults.sub_agent_timeout_secs);
    assert_eq!(
        cfg.llm_first_event_timeout_secs,
        defaults.llm_first_event_timeout_secs
    );
    assert_eq!(cfg.llm_idle_timeout_secs, defaults.llm_idle_timeout_secs);
    assert_eq!(
        cfg.llm_wait_heartbeat_secs,
        defaults.llm_wait_heartbeat_secs
    );
}

#[test]
fn config_parse_toml_via_cli() {
    let mut cfg = parse_args(vec![
        "--config".into(),
        "[generation]\nmax_turns = 50\n[tools]\ntool_timeout = 300\ntool_timeout_max = 1200".into(),
    ])
    .unwrap();
    let defaults = CliConfig::default();
    let cli = cfg.cli_config.take();
    apply_config_sources(&mut cfg, &defaults, None, None, cli.as_ref());
    cfg.cli_config = cli;
    assert_eq!(cfg.max_turns, 50);
    assert_eq!(cfg.tool_timeout_secs, 300);
    assert_eq!(cfg.tool_timeout_max_secs, 1200);
}

#[test]
fn edit_configuration_defaults_and_cli_overrides_are_typed() {
    let defaults = CliConfig::default();
    assert_eq!(defaults.edit_mode, EditMode::Hashline);
    assert!(defaults.edit_fuzzy_match);
    assert_eq!(defaults.edit_fuzzy_threshold, 0.95);
    assert!(!defaults.edit_enforce_seen_lines);

    let cfg = parse_args(vec![
        "--edit-mode".into(),
        "replace".into(),
        "--edit-fuzzy-match".into(),
        "false".into(),
        "--edit-fuzzy-threshold".into(),
        "0.88".into(),
        "--edit-enforce-seen-lines".into(),
        "true".into(),
    ])
    .unwrap();
    assert_eq!(cfg.edit_mode, EditMode::Replace);
    assert!(!cfg.edit_fuzzy_match);
    assert_eq!(cfg.edit_fuzzy_threshold, 0.88);
    assert!(cfg.edit_enforce_seen_lines);
}

#[test]
fn plan_projection_tail_config_defaults_true_and_toml_overrides() {
    assert!(CliConfig::default().plan_projection_tail);

    let defaults = CliConfig::default();
    let mut cfg = CliConfig::default();
    apply_config_sources(&mut cfg, &defaults, None, None, None);
    assert!(cfg.plan_projection_tail);

    let file: MinkConfigFile = toml::from_str("[context]\nplan_projection_tail = false").unwrap();
    assert_eq!(file.context.plan_projection_tail, Some(false));

    apply_config_sources(&mut cfg, &defaults, None, None, Some(&file));
    assert!(!cfg.plan_projection_tail);
}

#[test]
fn edit_configuration_toml_and_threshold_validation_fail_fast() {
    let file: MinkConfigFile = toml::from_str(
            "[tools.edit]\nmode = 'replace'\nfuzzy_match = false\nfuzzy_threshold = 0.9\nenforce_seen_lines = true",
        )
        .unwrap();
    assert_eq!(file.tools.edit.mode, Some(EditMode::Replace));
    assert_eq!(file.tools.edit.fuzzy_match, Some(false));
    assert_eq!(file.tools.edit.fuzzy_threshold, Some(0.9));
    assert_eq!(file.tools.edit.enforce_seen_lines, Some(true));
    assert!(toml::from_str::<MinkConfigFile>("[tools.edit]\nmode = 'patch'").is_err());

    let mut cfg = CliConfig {
        edit_fuzzy_threshold: f64::NAN,
        ..CliConfig::default()
    };
    assert!(
        validate_runtime_config(&cfg)
            .unwrap_err()
            .to_string()
            .contains("finite")
    );
    cfg.edit_fuzzy_threshold = 1.01;
    assert!(validate_runtime_config(&cfg).is_err());
}

#[test]
fn tool_timeout_max_below_five_is_rejected_by_cli_validation() {
    let mut cfg = CliConfig {
        tool_timeout_max_secs: 4,
        ..CliConfig::default()
    };
    assert!(
        validate_runtime_config(&cfg)
            .unwrap_err()
            .to_string()
            .contains("tool_timeout_max_secs must be at least 5 seconds")
    );
    cfg.tool_timeout_max_secs = 5;
    assert!(validate_runtime_config(&cfg).is_ok());
}

#[test]
fn sdk_grouped_options_map_to_cli_config() {
    let request = mink::sdk_protocol::parse_agent_jsonl_request(
        r#"{
            "prompt": "hi",
            "session_id": "outer",
            "mission": "mission",
            "options": {
                "provider": {"model": "pro"},
                "generation": {"max_tokens": 123, "max_turns": 4},
                "context": {
                    "max_context": 64000,
                    "context_reserve_tokens": 12000,
                    "context_compact_tail_tokens": 16000,
                    "context_compact_max_output_tokens": 4096
                },
                "tools": {
                    "tool_timeout": 5,
                    "tool_timeout_max": 900,
                    "enabled_tools": ["Read", "Bash"],
                    "edit_mode": "replace"
                },
                "output": {"verbose": true, "stream_events": false},
                "signal": {"policy": "evidence"}
            }
        }"#,
    )
    .unwrap();
    let mut config = CliConfig::default();

    apply_sdk_request_options(&mut config, &request);

    assert_eq!(config.model, "pro");
    assert_eq!(config.max_tokens, 123);
    assert_eq!(config.max_turns, 4);
    assert_eq!(config.max_context_tokens, 64_000);
    assert_eq!(config.context_reserve_tokens, 12_000);
    assert_eq!(config.context_compact_tail_tokens, 16_000);
    assert_eq!(config.context_compact_max_output_tokens, 4_096);
    assert_eq!(config.tool_timeout_secs, 5);
    assert_eq!(config.tool_timeout_max_secs, 900);
    assert_eq!(
        config.enabled_tools,
        Some(vec!["Read".to_string(), "Bash".to_string()])
    );
    assert_eq!(config.edit_mode, EditMode::Replace);
    assert_eq!(config.output_format, OutputFormat::Human);
    assert!(config.verbose);
    assert_eq!(config.session_id, "outer");
    assert_eq!(config.mission_content.as_deref(), Some("mission"));
    assert_eq!(config.signal_policy, SignalPolicy::Evidence);
    validate_runtime_config(&config).unwrap();
}

#[test]
fn sdk_context_override_is_validated_after_defaults_merge() {
    let request = mink::sdk_protocol::parse_agent_jsonl_request(
        r#"{"prompt":"hi","options":{"context":{"max_context":64000}}}"#,
    )
    .unwrap();
    let mut config = CliConfig::default();

    apply_sdk_request_options(&mut config, &request);

    let error = validate_runtime_config(&config).unwrap_err().to_string();
    assert!(
        error.contains("context_reserve_tokens (64000) must be less than max_context (64000)"),
        "{error}"
    );
}
