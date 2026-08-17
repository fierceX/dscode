use super::*;
use crate::config::{ToolApprovalMode, ToolApprovalPolicy};
use crate::context::ToolConfig;
use crate::tools::approval::{ToolAuthorization, authorize_tool, denied_message};

#[tokio::test]
async fn successful_display_text_may_start_with_error_prefix() {
    let shared = crate::regression::test_context_for_agent("runner-error-prefix-success")
        .await
        .unwrap();
    let ctx = crate::context::ToolContext::from(shared.as_ref());
    let call = ToolCallEvent {
        name: "Grep".into(),
        id: "call-error-prefix".into(),
        input_json: serde_json::json!({}),
        fields: BTreeMap::new(),
    };
    let result = format_dispatched_result(
        &ctx,
        &call,
        ToolExecOutput {
            content: "Error: this is literal file content".into(),
            is_bash: false,
            conv_content: String::new(),
            exit_code: None,
            wall_ms: None,
            no_mutation: false,
            memo_candidate: None,
            spawns_sub_agent: false,
            status: ToolStatus::Succeeded,
            diagnostics: Vec::new(),
            plan_command: None,
            state_metadata: None,
            result_kind: ToolResultKind::Search,
            presentation: None,
        },
    );
    assert_eq!(result.status, ToolStatus::Succeeded);
}

#[test]
fn format_tool_result_truncates_large() {
    let s = "line0\n".repeat(500);
    let result = format_tool_result(&s, 100);
    assert!(result.len() <= 100 + 100); // head + tail + marker
    assert!(result.contains("truncated"));
}

#[test]
fn format_tool_result_short_passes_through() {
    let s = "short";
    assert_eq!(format_tool_result(s, 100), "short");
}

#[test]
fn filter_bash_noise_strips_ansi() {
    let input = "\x1b[32mgreen text\x1b[0m";
    let result = filter_bash_noise(input);
    assert!(!result.contains('\x1b'));
    assert!(result.contains("green text"));
}

#[test]
fn filter_bash_noise_compresses_repeats() {
    let input = "line1\nline1\nline1\nline2";
    let result = filter_bash_noise(input);
    assert!(result.contains("repeated 2 times"));
    assert!(result.contains("line1"));
    assert!(result.contains("line2"));
}

#[test]
fn tool_registry_matches_schema() {
    let schema: Vec<serde_json::Value> =
        serde_json::from_str(crate::assets::TOOLS_JSON).expect("tools schema should parse");
    let schema_names: std::collections::BTreeSet<String> = schema
        .iter()
        .map(|tool| {
            tool.get("name")
                .and_then(serde_json::Value::as_str)
                .expect("schema tool name")
                .to_string()
        })
        .collect();
    let registry = tool_registry();
    let registry_names: std::collections::BTreeSet<String> = registry
        .iter()
        .map(|tool| tool.metadata().name.to_string())
        .collect();

    for name in &schema_names {
        if name == "PythonSandbox" && cfg!(not(feature = "python-sandbox")) {
            continue;
        }
        assert!(
            registry_names.contains(name),
            "schema tool missing executor: {name}"
        );
    }
    for tool in registry {
        assert!(
            schema_names.contains(tool.metadata().name.as_ref()),
            "registry tool missing schema: {}",
            tool.metadata().name
        );
    }
    for expected in [
        "PlanDraft",
        "PlanConfirm",
        "PlanClear",
        "TodoWrite",
        "TodoRead",
        "TodoAdvance",
        "SubAgent",
    ] {
        assert!(registry_names.contains(expected));
    }
}

#[test]
fn tool_schema_order_is_stable_and_descriptions_are_self_contained() {
    let schema: Vec<serde_json::Value> =
        serde_json::from_str(crate::assets::TOOLS_JSON).expect("tools schema should parse");
    let names: Vec<&str> = schema
        .iter()
        .map(|tool| {
            tool.get("name")
                .and_then(serde_json::Value::as_str)
                .expect("schema tool name")
        })
        .collect();
    let pos = |name: &str| {
        names
            .iter()
            .position(|candidate| *candidate == name)
            .expect("tool should exist in schema")
    };

    assert!(pos("Glob") < pos("Bash"));
    assert!(pos("Grep") < pos("Bash"));

    for tool in &schema {
        let own_name = tool["name"].as_str().unwrap();
        let serialized = serde_json::to_string(tool).unwrap();
        for peer in &names {
            if *peer == own_name {
                continue;
            }
            assert!(
                !serialized.contains(&format!("`{peer}`"))
                    && !serialized.contains(&format!("use {peer}"))
                    && !serialized.contains(&format!("Use {peer}"))
                    && !serialized.contains(&format!("{peer} tool")),
                "schema '{own_name}' contains peer-tool routing for '{peer}'"
            );
        }
    }

    let plan_draft = schema
        .iter()
        .find(|tool| tool["name"] == "PlanDraft")
        .expect("PlanDraft schema");
    assert!(
        plan_draft["description"]
            .as_str()
            .is_some_and(|description| description.contains("empty content string"))
    );
    assert!(
        plan_draft["input_schema"]["properties"]["content"]["description"]
            .as_str()
            .is_some_and(|description| description.contains("empty string"))
    );
}

#[test]
fn registry_metadata_is_complete() {
    // summary 字段已删除：模型可见描述来自 tools.json schema（由
    // catalog 一致性测试钉住），registry 元数据只保留行为属性。
    for tool in tool_registry() {
        let meta = tool.metadata();
        assert!(!meta.name.is_empty(), "tool name is empty");
    }
}

#[test]
fn mutating_tools_are_write_or_exec_tier() {
    for tool in tool_registry() {
        let meta = tool.metadata();
        if meta.mutating {
            assert!(
                matches!(meta.approval, ApprovalTier::Write | ApprovalTier::Exec),
                "{} is mutating but not write/exec tier",
                meta.name
            );
        }
    }
}

#[test]
fn expected_tool_metadata_contracts() {
    let meta = |name: &str| {
        tool_registry()
            .iter()
            .find(|tool| tool.metadata().name == name)
            .expect("tool should exist")
            .metadata()
    };

    assert_eq!(meta("Read").approval, ApprovalTier::Read);
    assert_eq!(meta("Read").result_kind, ToolResultKind::FileRead);
    assert_eq!(meta("Write").approval, ApprovalTier::Write);
    assert_eq!(meta("Write").result_kind, ToolResultKind::FileWrite);
    assert_eq!(meta("Edit").approval, ApprovalTier::Write);
    assert_eq!(meta("Edit").result_kind, ToolResultKind::Edit);
    assert_eq!(meta("Bash").approval, ApprovalTier::Exec);
    assert_eq!(meta("Bash").result_kind, ToolResultKind::Command);
    assert_eq!(meta("Glob").approval, ApprovalTier::Read);
    assert_eq!(meta("Glob").result_kind, ToolResultKind::Search);
    assert_eq!(meta("Grep").approval, ApprovalTier::Read);
    assert_eq!(meta("Grep").result_kind, ToolResultKind::Search);
    assert_eq!(meta("SubAgent").approval, ApprovalTier::Exec);
    assert_eq!(meta("SubAgent").result_kind, ToolResultKind::SubAgent);
    assert!(meta("SubAgent").spawns_sub_agent);
    // internal/discoverable 元数据字段已删除（零生产消费）。
}

#[test]
fn approval_yolo_allows_exec_tools() {
    let config = approval_test_config(ToolApprovalMode::Yolo, []);
    let bash = tool_registry()
        .iter()
        .find(|tool| tool.metadata().name == "Bash")
        .unwrap()
        .metadata();
    assert_eq!(authorize_tool(&bash, &config), ToolAuthorization::Allowed);
}

#[test]
fn approval_write_mode_blocks_exec_but_allows_write() {
    let config = approval_test_config(ToolApprovalMode::Write, []);
    let meta = |name: &str| {
        tool_registry()
            .iter()
            .find(|tool| tool.metadata().name == name)
            .unwrap()
            .metadata()
    };

    assert_eq!(
        authorize_tool(&meta("Read"), &config),
        ToolAuthorization::Allowed
    );
    assert_eq!(
        authorize_tool(&meta("Write"), &config),
        ToolAuthorization::Allowed
    );
    assert!(matches!(
        authorize_tool(&meta("Bash"), &config),
        ToolAuthorization::Denied { .. }
    ));
}

#[test]
fn approval_per_tool_overrides_mode() {
    let config = approval_test_config(
        ToolApprovalMode::Write,
        [
            ("Bash".to_string(), ToolApprovalPolicy::Allow),
            ("Read".to_string(), ToolApprovalPolicy::Deny),
        ],
    );
    let meta = |name: &str| {
        tool_registry()
            .iter()
            .find(|tool| tool.metadata().name == name)
            .unwrap()
            .metadata()
    };

    assert_eq!(
        authorize_tool(&meta("Bash"), &config),
        ToolAuthorization::Allowed
    );
    let read = meta("Read");
    let reason = match authorize_tool(&read, &config) {
        ToolAuthorization::Denied { reason } => denied_message(&read, reason),
        ToolAuthorization::Allowed => panic!("Read should be denied"),
    };
    assert!(reason.contains("deny"), "{reason}");
}

#[test]
fn policy_gate_blocks_tools_disabled_by_whitelist_before_execution() {
    let mut config = approval_test_config(ToolApprovalMode::Yolo, []);
    config.enabled_tools = Some(vec!["Read".into()]);
    let storm = Mutex::new(StormBreaker::new(6, 3));
    let resolution = crate::tools::surface::ToolResolutionContext::from_runtime(
        crate::tools::surface::AgentRole::Primary,
        &config,
        false,
    );
    let surface = crate::tools::surface::ModelToolSurface::resolve(
        crate::tools::catalog::ToolCatalog::builtin().unwrap(),
        &config,
        &resolution,
    )
    .unwrap();
    let gate = ToolPolicyGate {
        surface: &surface,
        storm: &storm,
    };
    let call = test_call("Bash");
    let metadata = tool_registry()
        .iter()
        .find(|tool| tool.metadata().name == "Bash")
        .map(|tool| tool.metadata());

    let blocked = gate
        .evaluate(&call, metadata)
        .expect("Bash should be blocked by enabled_tools");

    assert_eq!(blocked.tool_name, "Bash");
    assert!(blocked.content.contains("unavailable"));
}

#[test]
fn policy_gate_blocks_tools_hidden_by_role_or_backend() {
    let config = approval_test_config(ToolApprovalMode::Yolo, []);
    let catalog = crate::tools::catalog::ToolCatalog::builtin().unwrap();
    let storm = Mutex::new(StormBreaker::new(6, 3));

    let sub_agent_resolution = crate::tools::surface::ToolResolutionContext::from_runtime(
        crate::tools::surface::AgentRole::SubAgent,
        &config,
        false,
    );
    let sub_agent_surface =
        crate::tools::surface::ModelToolSurface::resolve(catalog, &config, &sub_agent_resolution)
            .unwrap();
    let sub_agent_gate = ToolPolicyGate {
        surface: &sub_agent_surface,
        storm: &storm,
    };
    let sub_agent_metadata = tool_registry()
        .iter()
        .find(|tool| tool.metadata().name == "SubAgent")
        .map(|tool| tool.metadata());
    let blocked = sub_agent_gate
        .evaluate(&test_call("SubAgent"), sub_agent_metadata)
        .expect("SubAgent should be blocked outside the sub-agent surface");
    assert!(blocked.content.contains("UnavailableForRole"));

    let vfs_resolution = crate::tools::surface::ToolResolutionContext::from_runtime(
        crate::tools::surface::AgentRole::Primary,
        &config,
        true,
    );
    let vfs_surface =
        crate::tools::surface::ModelToolSurface::resolve(catalog, &config, &vfs_resolution)
            .unwrap();
    let vfs_gate = ToolPolicyGate {
        surface: &vfs_surface,
        storm: &storm,
    };
    let edit_metadata = tool_registry()
        .iter()
        .find(|tool| tool.metadata().name == "Edit")
        .map(|tool| tool.metadata());
    let blocked = vfs_gate
        .evaluate(&test_call("Edit"), edit_metadata)
        .expect("Edit should be blocked outside the VFS surface");
    assert!(blocked.content.contains("UnavailableForBackend"));
}

fn test_call(name: &str) -> ToolCallEvent {
    ToolCallEvent {
        name: name.to_string(),
        id: "call_test".to_string(),
        input_json: serde_json::json!({}),
        fields: BTreeMap::new(),
    }
}

fn approval_test_config<const N: usize>(
    mode: ToolApprovalMode,
    overrides: [(String, ToolApprovalPolicy); N],
) -> ToolConfig {
    ToolConfig {
        tool_timeout_secs: 600,
        sub_agent_timeout_secs: 300,
        tool_result_max_bytes: 100_000,
        file_write_max_bytes: 1_048_576,
        edit_mode: crate::config::EditMode::Hashline,
        edit_fuzzy_match: true,
        edit_fuzzy_threshold: 0.95,
        edit_enforce_seen_lines: false,
        max_search_files: 5000,
        max_search_results: 1000,
        enabled_tools: None,
        tool_approval_mode: mode,
        tool_approval: overrides.into_iter().collect(),
        sandbox_python: crate::config::SandboxPythonConfig::default(),
    }
}

#[test]
fn all_tool_result_kind_variants_have_expected_coverage() {
    let kinds: std::collections::BTreeSet<&'static str> = tool_registry()
        .iter()
        .map(|tool| match tool.metadata().result_kind {
            ToolResultKind::Text => "Text",
            ToolResultKind::FileRead => "FileRead",
            ToolResultKind::FileWrite => "FileWrite",
            ToolResultKind::Edit => "Edit",
            ToolResultKind::Command => "Command",
            ToolResultKind::Search => "Search",
            ToolResultKind::Control => "Control",
            ToolResultKind::SubAgent => "SubAgent",
        })
        .collect();

    for expected in [
        "FileRead",
        "FileWrite",
        "Edit",
        "Command",
        "Search",
        "Control",
        "SubAgent",
    ] {
        assert!(kinds.contains(expected), "missing result kind {expected}");
    }
}
