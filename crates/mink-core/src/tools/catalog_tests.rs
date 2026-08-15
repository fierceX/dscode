use super::*;
use crate::config::ResolvedConfig as Config;
use std::sync::Arc;

struct TestTool(crate::runtime::ToolDefinition);

#[async_trait::async_trait]
impl crate::runtime::AgentTool for TestTool {
    fn definition(&self) -> crate::runtime::ToolDefinition {
        self.0.clone()
    }

    async fn execute(
        &self,
        _input: serde_json::Value,
        _ctx: crate::runtime::ToolExecutionContext,
    ) -> std::result::Result<crate::runtime::ToolOutput, crate::runtime::ToolError> {
        Ok(crate::runtime::ToolOutput::text("ok"))
    }
}

fn custom_definition(name: &str) -> crate::runtime::ToolDefinition {
    crate::runtime::ToolDefinition::new(
        name,
        "test tool",
        serde_json::json!({
            "type": "object",
            "properties": {},
            "additionalProperties": false
        }),
    )
}

fn custom_tool(definition: crate::runtime::ToolDefinition) -> crate::runtime::RegisteredCustomTool {
    let executor: Arc<dyn crate::runtime::AgentTool> = Arc::new(TestTool(definition.clone()));
    crate::runtime::RegisteredCustomTool {
        definition,
        executor,
    }
}

#[test]
fn catalog_joins_every_schema_and_executor() {
    let catalog = ToolCatalog::builtin().unwrap();
    assert!(catalog.get("Read").is_some());
    assert_eq!(
        catalog.iter_compiled().count(),
        crate::tools::runner::tool_registry().len()
    );
}
#[test]
fn schemas_declare_exactly_the_runtime_accepted_fields() {
    const CONTRACT: &[(&str, &[&str])] = &[
        ("Read", &["path"]),
        ("Glob", &["pattern", "path"]),
        ("Grep", &["pattern", "path", "glob", "context"]),
        ("Write", &["path", "content"]),
        ("Edit", &["input"]),
        ("Bash", &["command", "timeout"]),
        ("Python", &["script", "script_file", "timeout"]),
        ("TodoRead", &["include_completed"]),
        ("TodoWrite", &["base_revision", "add", "update", "remove"]),
        (
            "TodoAdvance",
            &["base_revision", "complete", "activate", "pause", "reopen"],
        ),
        ("PlanDraft", &["content"]),
        ("PlanConfirm", &[]),
        ("PlanClear", &[]),
        ("SubAgent", &["prompt", "description", "fork"]),
        ("PythonSandbox", &["script", "script_file", "timeout"]),
    ];
    let catalog = ToolCatalog::builtin().unwrap();
    let mut covered = BTreeSet::new();
    for (name, fields) in CONTRACT {
        covered.insert(*name);
        let tool = catalog
            .get(name)
            .unwrap_or_else(|| panic!("schema for '{name}' missing from catalog"));
        let schema = &tool.schema["input_schema"];
        assert_eq!(
            schema.get("type").and_then(serde_json::Value::as_str),
            Some("object"),
            "{name} input schema must be an object"
        );
        assert_eq!(
            schema.get("additionalProperties"),
            Some(&serde_json::json!(false)),
            "{name} schema must reject unknown fields (additionalProperties: false)"
        );
        let declared: BTreeSet<&str> = schema["properties"]
            .as_object()
            .expect("properties object")
            .keys()
            .map(String::as_str)
            .collect();
        let expected: BTreeSet<&str> = fields.iter().copied().collect();
        assert_eq!(
            declared, expected,
            "{name} schema properties drift from the runtime contract"
        );
    }
    for (tool, _) in catalog.iter_compiled() {
        assert!(
            covered.contains(tool.name.as_str()),
            "no contract entry for compiled tool '{}'",
            tool.name
        );
    }
}

#[test]
fn configured_names_fail_fast() {
    let mut config = ToolConfig::from_config(&Config::default());
    config.enabled_tools = Some(vec!["Read".into(), "Read".into()]);
    assert!(
        ToolCatalog::builtin()
            .unwrap()
            .validate_configured_names(&config)
            .unwrap_err()
            .to_string()
            .contains("duplicate")
    );
    config.enabled_tools = Some(vec!["NoSuchTool".into()]);
    assert!(
        ToolCatalog::builtin()
            .unwrap()
            .validate_configured_names(&config)
            .unwrap_err()
            .to_string()
            .contains("unknown tool")
    );
}

#[test]
fn custom_tool_validation_rejects_duplicate_builtin_schema_dependency_and_cycles() {
    let builtin = custom_tool(custom_definition("Read"));
    assert!(
        validate_custom_tools(&[builtin])
            .unwrap_err()
            .to_string()
            .contains("built-in")
    );

    let duplicate = vec![
        custom_tool(custom_definition("Duplicate")),
        custom_tool(custom_definition("Duplicate")),
    ];
    assert!(
        validate_custom_tools(&duplicate)
            .unwrap_err()
            .to_string()
            .contains("duplicate")
    );

    let mut bad_schema = custom_definition("BadSchema");
    bad_schema.input_schema = serde_json::json!({"type": "string"});
    assert!(
        validate_custom_tools(&[custom_tool(bad_schema)])
            .unwrap_err()
            .to_string()
            .contains("schema")
    );

    let mut missing = custom_definition("MissingDependency");
    missing.hard_dependencies.push("NoSuchTool".into());
    assert!(
        validate_custom_tools(&[custom_tool(missing)])
            .unwrap_err()
            .to_string()
            .contains("missing tool")
    );

    let mut first = custom_definition("CycleA");
    first.hard_dependencies.push("CycleB".into());
    let mut second = custom_definition("CycleB");
    second.hard_dependencies.push("CycleA".into());
    assert!(
        validate_custom_tools(&[custom_tool(first), custom_tool(second)])
            .unwrap_err()
            .to_string()
            .contains("dependency cycle")
    );

    let mut mutating = custom_definition("MutatingParallel");
    mutating.mutating = true;
    assert!(
        validate_custom_tools(&[custom_tool(mutating)])
            .unwrap_err()
            .to_string()
            .contains("Sequential")
    );
}

#[cfg(not(feature = "python-sandbox"))]
#[test]
fn feature_unavailable_is_not_reported_as_unknown() {
    let mut config = ToolConfig::from_config(&Config::default());
    config.enabled_tools = Some(vec!["PythonSandbox".into()]);
    let error = ToolCatalog::builtin()
        .unwrap()
        .validate_configured_names(&config)
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("python-sandbox") && error.contains("feature"),
        "{error}"
    );
    assert!(!error.contains("unknown tool"), "{error}");
}
