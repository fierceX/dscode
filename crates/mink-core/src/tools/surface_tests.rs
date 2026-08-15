use super::*;
use crate::config::{ResolvedConfig as Config, ToolApprovalMode};

fn resolve(config: &ToolConfig, role: AgentRole, vfs: bool) -> Result<ModelToolSurface> {
    let context = ToolResolutionContext::from_runtime(role, config, vfs);
    ModelToolSurface::resolve(ToolCatalog::builtin()?, config, &context)
}

#[test]
fn whitelist_and_dependency_are_resolved_in_two_passes() {
    let mut config = ToolConfig::from_config(&Config::default());
    config.enabled_tools = Some(vec!["Edit".into(), "Read".into()]);
    let surface = resolve(&config, AgentRole::Primary, false).unwrap();
    assert!(surface.has_all(&["Read", "Edit"]));
    config.enabled_tools = Some(vec!["Edit".into()]);
    assert!(
        resolve(&config, AgentRole::Primary, false)
            .unwrap_err()
            .to_string()
            .contains("requires active tool")
    );

    config.enabled_tools = Some(vec!["TodoRead".into(), "TodoWrite".into()]);
    let surface = resolve(&config, AgentRole::Primary, false).unwrap();
    assert!(surface.has_all(&["TodoRead", "TodoWrite"]));
    assert!(!surface.has("TodoAdvance"));
    assert!(
            surface.get("TodoWrite").unwrap().schema["input_schema"]["properties"]["add"]["items"]
                ["properties"]
                .get("status")
                .is_none(),
            "structure-only TodoWrite must not create an active item"
        );
    config.enabled_tools = Some(vec!["TodoWrite".into()]);
    let error = resolve(&config, AgentRole::Primary, false)
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("TodoWrite") && error.contains("TodoRead"),
        "{error}"
    );

    config.enabled_tools = Some(vec!["TodoRead".into(), "TodoAdvance".into()]);
    let surface = resolve(&config, AgentRole::Primary, false).unwrap();
    assert!(surface.has_all(&["TodoRead", "TodoAdvance"]));
    config.enabled_tools = Some(vec!["TodoAdvance".into()]);
    let error = resolve(&config, AgentRole::Primary, false)
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("TodoAdvance") && error.contains("TodoRead"),
        "{error}"
    );
}

#[test]
fn edit_mode_materializes_one_stable_schema_and_changes_fingerprint() {
    let hashline_config = ToolConfig::from_config(&Config::default());
    let first = resolve(&hashline_config, AgentRole::Primary, false).unwrap();
    let second = resolve(&hashline_config, AgentRole::Primary, false).unwrap();
    assert_eq!(first.fingerprint(), second.fingerprint());
    let hashline = &first.get("Edit").unwrap().schema;
    assert!(hashline.pointer("/input_schema/properties/input").is_some());
    assert!(hashline.pointer("/input_schema/properties/path").is_none());
    let edit_desc = hashline
        .pointer("/description")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();
    assert!(
        edit_desc.contains("[PATH#TAG]") && edit_desc.contains("N.=M"),
        "Edit description must guide tag and range semantics: {edit_desc}"
    );
    assert!(edit_desc.contains("returns a new tag"));
    assert!(edit_desc.contains("fresh Read/Grep header"));
    assert!(!edit_desc.contains("retry with it directly"));
    assert!(
        first.get("Read").unwrap().schema["description"]
            .as_str()
            .unwrap()
            .contains("[PATH#TAG]")
    );

    let replace = Config {
        edit_mode: crate::config::EditMode::Replace,
        edit_fuzzy_threshold: 0.91,
        ..Config::default()
    };
    let replace = resolve(
        &ToolConfig::from_config(&replace),
        AgentRole::Primary,
        false,
    )
    .unwrap();
    let schema = &replace.get("Edit").unwrap().schema;
    assert!(schema.pointer("/input_schema/properties/edits").is_some());
    assert!(schema.pointer("/input_schema/properties/input").is_none());
    assert!(
        !replace.get("Read").unwrap().schema["description"]
            .as_str()
            .unwrap()
            .contains("Hashline")
    );
    assert_ne!(first.fingerprint(), replace.fingerprint());
    assert!(schema["description"].as_str().unwrap().contains("0.910"));
}

#[test]
fn role_backend_and_approval_shrink_surface() {
    let config = ToolConfig::from_config(&Config::default());
    assert!(
        !resolve(&config, AgentRole::SubAgent, false)
            .unwrap()
            .has("SubAgent")
    );
    assert!(
        !resolve(&config, AgentRole::Primary, true)
            .unwrap()
            .has("Edit")
    );

    let mut write_mode = config;
    write_mode.tool_approval_mode = ToolApprovalMode::Write;
    assert!(
        !resolve(&write_mode, AgentRole::Primary, false)
            .unwrap()
            .has("Bash")
    );
}

#[test]
fn explicit_vfs_edit_is_an_error() {
    let mut config = ToolConfig::from_config(&Config::default());
    config.enabled_tools = Some(vec!["Read".into(), "Edit".into()]);
    assert!(
        resolve(&config, AgentRole::Primary, true)
            .unwrap_err()
            .to_string()
            .contains("read-only VFS")
    );
}

#[test]
fn default_surface_hides_python_sandbox() {
    let config = ToolConfig::from_config(&Config::default());
    assert!(
        !resolve(&config, AgentRole::Primary, false)
            .unwrap()
            .has("PythonSandbox")
    );
}

#[test]
fn caps_placeholders_render_real_config_values_into_descriptions() {
    let mut config = ToolConfig::from_config(&Config::default());
    config.tool_result_max_bytes = 12_345;
    config.max_search_results = 77;
    config.max_search_files = 4321;
    let surface = resolve(&config, AgentRole::Primary, false).unwrap();
    let bash_desc = surface.get("Bash").unwrap().schema["description"]
        .as_str()
        .unwrap();
    assert!(
        bash_desc.contains("12345 bytes"),
        "Bash description must render the configured result cap: {bash_desc}"
    );
    assert!(!bash_desc.contains("{{"));
    let read_desc = surface.get("Read").unwrap().schema["description"]
        .as_str()
        .unwrap();
    assert!(read_desc.contains("12345 bytes"), "{read_desc}");
    let grep_desc = surface.get("Grep").unwrap().schema["description"]
        .as_str()
        .unwrap();
    assert!(grep_desc.contains("77 matches"), "{grep_desc}");
    let glob_desc = surface.get("Glob").unwrap().schema["description"]
        .as_str()
        .unwrap();
    assert!(glob_desc.contains("4321 files"), "{glob_desc}");
    assert!(!glob_desc.contains("{{"));
    // 渲染后的描述必须参与前缀指纹。
    let other = ToolConfig::from_config(&Config::default());
    assert_ne!(
        surface.fingerprint(),
        resolve(&other, AgentRole::Primary, false)
            .unwrap()
            .fingerprint()
    );
}

#[test]
#[should_panic(expected = "unknown description template placeholder")]
fn unknown_description_placeholder_fails_fast() {
    let config = ToolConfig::from_config(&Config::default());
    render_description_templates("uses {{CAP_UNKNOWN}} here", &config);
}

#[cfg(feature = "python-sandbox")]
#[test]
fn compiled_python_sandbox_requires_runtime_enablement() {
    let mut config = ToolConfig::from_config(&Config::default());
    config.enabled_tools = Some(vec!["PythonSandbox".into()]);
    assert!(
        resolve(&config, AgentRole::Primary, false)
            .unwrap()
            .has("PythonSandbox")
    );
}
