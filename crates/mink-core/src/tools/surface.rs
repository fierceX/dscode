use crate::context::ToolConfig;
use crate::tools::approval::{ToolAuthorization, ToolAuthorizationDeniedReason, authorize_tool};
use crate::tools::catalog::{ToolBuildAvailability, ToolCatalog, ToolDefaultActivation};
use crate::tools::metadata::ToolMetadata;
use anyhow::{Result, bail, ensure};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentRole {
    Primary,
    SubAgent,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FilesystemBackend {
    Local,
    ReadOnlyVfs,
}

#[derive(Debug, Clone, Copy)]
pub struct ToolResolutionContext {
    role: AgentRole,
    filesystem_backend: FilesystemBackend,
    edit_mode: crate::config::EditMode,
}

impl ToolResolutionContext {
    pub fn from_runtime(role: AgentRole, config: &ToolConfig, read_only_fs_present: bool) -> Self {
        Self {
            role,
            filesystem_backend: if read_only_fs_present {
                FilesystemBackend::ReadOnlyVfs
            } else {
                FilesystemBackend::Local
            },
            edit_mode: config.edit_mode,
        }
    }

    pub fn role(&self) -> AgentRole {
        self.role
    }

    pub fn filesystem_backend(&self) -> FilesystemBackend {
        self.filesystem_backend
    }

    pub fn edit_mode(&self) -> crate::config::EditMode {
        self.edit_mode
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolHiddenReason {
    NotWhitelisted,
    ExplicitOptInRequired,
    DeniedByApproval,
    ApprovalPromptUnavailable,
    UnavailableForRole,
    UnavailableForBackend,
    FeatureUnavailable,
}

#[derive(Debug, Clone)]
pub struct ModelTool {
    pub metadata: ToolMetadata,
    pub schema: serde_json::Value,
}

#[derive(Debug, Clone)]
pub struct ModelToolSurface {
    ordered: Vec<ModelTool>,
    by_name: BTreeMap<String, usize>,
    hidden: BTreeMap<String, ToolHiddenReason>,
    fingerprint: String,
}

struct ToolHardDependencySpec {
    tool: &'static str,
    requires: &'static [&'static str],
}

const TOOL_HARD_DEPENDENCIES: &[ToolHardDependencySpec] = &[
    ToolHardDependencySpec {
        tool: "Edit",
        requires: &["Read"],
    },
    ToolHardDependencySpec {
        tool: "TodoWrite",
        requires: &["TodoRead"],
    },
    ToolHardDependencySpec {
        tool: "TodoAdvance",
        requires: &["TodoRead"],
    },
];

impl ModelToolSurface {
    pub(crate) fn resolve_with_custom(
        catalog: &ToolCatalog,
        config: &ToolConfig,
        context: &ToolResolutionContext,
        custom_tools: &[crate::runtime::RegisteredCustomTool],
    ) -> Result<Self> {
        let custom_names = custom_tools
            .iter()
            .map(|tool| tool.definition.name.clone())
            .collect::<BTreeSet<_>>();
        let mut builtin_config = config.clone();
        if let Some(names) = &mut builtin_config.enabled_tools {
            names.retain(|name| !custom_names.contains(name));
        }
        builtin_config
            .tool_approval
            .retain(|name, _| !custom_names.contains(name));
        let mut surface = Self::resolve(catalog, &builtin_config, context)?;
        let whitelist = config.enabled_tools.as_ref();
        for tool in custom_tools {
            let definition = &tool.definition;
            if whitelist
                .as_ref()
                .is_some_and(|names| !names.contains(&definition.name))
            {
                continue;
            }
            if whitelist.is_none()
                && definition.activation == crate::runtime::ToolActivation::ExplicitOnly
            {
                continue;
            }
            let metadata = ToolMetadata {
                name: std::borrow::Cow::Owned(definition.name.clone()),
                summary: std::borrow::Cow::Owned(definition.summary.clone()),
                approval: definition.approval,
                result_kind: definition.result_kind,
                mutating: definition.mutating,
                storm_exempt: definition.storm_exempt,
                internal: false,
                discoverable: definition.discoverable,
                spawns_sub_agent: false,
            };
            match authorize_tool(&metadata, config) {
                ToolAuthorization::Allowed => {}
                ToolAuthorization::Denied {
                    reason: ToolAuthorizationDeniedReason::ExplicitDeny,
                } => {
                    surface
                        .hidden
                        .insert(definition.name.clone(), ToolHiddenReason::DeniedByApproval);
                    continue;
                }
                ToolAuthorization::Denied {
                    reason: ToolAuthorizationDeniedReason::PromptUnavailable,
                } => {
                    surface.hidden.insert(
                        definition.name.clone(),
                        ToolHiddenReason::ApprovalPromptUnavailable,
                    );
                    continue;
                }
            }
            let schema = serde_json::json!({"name": definition.name, "description": definition.summary, "input_schema": definition.input_schema});
            surface
                .by_name
                .insert(definition.name.clone(), surface.ordered.len());
            surface.ordered.push(ModelTool { metadata, schema });
        }
        for tool in custom_tools {
            let definition = &tool.definition;
            if surface.has(&definition.name) {
                for dependency in &definition.hard_dependencies {
                    ensure!(
                        surface.has(dependency),
                        "tool '{}' requires active tool '{}'",
                        definition.name,
                        dependency
                    );
                }
            }
        }
        surface.fingerprint = surface_fingerprint(&surface.ordered);
        Ok(surface)
    }

    pub fn resolve(
        catalog: &ToolCatalog,
        config: &ToolConfig,
        context: &ToolResolutionContext,
    ) -> Result<Self> {
        catalog.validate_configured_names(config)?;
        validate_dependency_graph(catalog)?;
        let whitelist = config
            .enabled_tools
            .as_ref()
            .map(|names| names.iter().map(String::as_str).collect::<BTreeSet<_>>());
        let explicitly_selected =
            |name: &str| whitelist.as_ref().is_some_and(|names| names.contains(name));
        let mut candidates = BTreeSet::new();
        let mut hidden = BTreeMap::new();

        for tool in catalog.iter() {
            let metadata = match &tool.availability {
                ToolBuildAvailability::Compiled { metadata } => metadata.clone(),
                ToolBuildAvailability::FeatureUnavailable { .. } => {
                    hidden.insert(tool.name.clone(), ToolHiddenReason::FeatureUnavailable);
                    continue;
                }
            };
            if whitelist
                .as_ref()
                .is_some_and(|names| !names.contains(tool.name.as_str()))
            {
                hidden.insert(tool.name.clone(), ToolHiddenReason::NotWhitelisted);
                continue;
            }
            if whitelist.is_none() && tool.default_activation == ToolDefaultActivation::ExplicitOnly
            {
                hidden.insert(tool.name.clone(), ToolHiddenReason::ExplicitOptInRequired);
                continue;
            }
            match authorize_tool(&metadata, config) {
                ToolAuthorization::Allowed => {}
                ToolAuthorization::Denied {
                    reason: ToolAuthorizationDeniedReason::ExplicitDeny,
                } => {
                    hidden.insert(tool.name.clone(), ToolHiddenReason::DeniedByApproval);
                    continue;
                }
                ToolAuthorization::Denied {
                    reason: ToolAuthorizationDeniedReason::PromptUnavailable,
                } => {
                    hidden.insert(
                        tool.name.clone(),
                        ToolHiddenReason::ApprovalPromptUnavailable,
                    );
                    continue;
                }
            }
            if context.role == AgentRole::SubAgent && metadata.name == "SubAgent" {
                hidden.insert(tool.name.clone(), ToolHiddenReason::UnavailableForRole);
                continue;
            }
            if context.filesystem_backend == FilesystemBackend::ReadOnlyVfs
                && metadata.name == "Edit"
            {
                if explicitly_selected("Edit") {
                    bail!(
                        "tool 'Edit' is unavailable with the read-only VFS backend because it requires a local editable snapshot provider"
                    );
                }
                hidden.insert(tool.name.clone(), ToolHiddenReason::UnavailableForBackend);
                continue;
            }
            candidates.insert(metadata.name.to_string());
        }

        for dependency in TOOL_HARD_DEPENDENCIES {
            if candidates.contains(dependency.tool) {
                for required in dependency.requires {
                    ensure!(
                        candidates.contains(*required),
                        "tool '{}' requires active tool '{}'",
                        dependency.tool,
                        required
                    );
                }
            }
        }

        let ordered: Vec<ModelTool> = catalog
            .iter_compiled()
            .filter(|(_, metadata)| candidates.contains(metadata.name.as_ref()))
            .map(|(tool, metadata)| {
                let schema = resolved_schema(tool.schema.clone(), metadata.name.as_ref(), config);
                ModelTool { metadata, schema }
            })
            .collect();
        let by_name = ordered
            .iter()
            .enumerate()
            .map(|(index, tool)| (tool.metadata.name.to_string(), index))
            .collect();
        let fingerprint = surface_fingerprint(&ordered);
        Ok(Self {
            ordered,
            by_name,
            hidden,
            fingerprint,
        })
    }

    pub fn has(&self, name: &str) -> bool {
        self.by_name.contains_key(name)
    }

    pub fn has_all(&self, names: &[&str]) -> bool {
        names.iter().all(|name| self.has(name))
    }

    pub fn has_any(&self, names: &[&str]) -> bool {
        names.iter().any(|name| self.has(name))
    }

    pub fn get(&self, name: &str) -> Option<&ModelTool> {
        self.by_name.get(name).map(|index| &self.ordered[*index])
    }

    pub fn schemas(&self) -> Vec<serde_json::Value> {
        self.ordered
            .iter()
            .map(|tool| tool.schema.clone())
            .collect()
    }

    pub fn names(&self) -> impl Iterator<Item = &str> + '_ {
        self.ordered.iter().map(|tool| tool.metadata.name.as_ref())
    }

    pub fn hidden(&self) -> &BTreeMap<String, ToolHiddenReason> {
        &self.hidden
    }

    pub fn fingerprint(&self) -> &str {
        &self.fingerprint
    }
}

/// A2：caps 占位符渲染——把真实配置值注入工具描述。
/// 占位符白名单见下；tools.json 与运行时描述字符串中的未知 {{...}} 一律
/// fail-fast（提示词纪律，tests/prompt_discipline.rs 同步执行）。
pub fn render_description_templates(desc: &str, config: &ToolConfig) -> String {
    let rendered = desc
        .replace(
            "{{CAP_TOOL_RESULT_MAX_BYTES}}",
            &config.tool_result_max_bytes.to_string(),
        )
        .replace(
            "{{CAP_MAX_SEARCH_RESULTS}}",
            &config.max_search_results.to_string(),
        )
        .replace(
            "{{CAP_MAX_SEARCH_FILES}}",
            &config.max_search_files.to_string(),
        );
    if let Some(start) = rendered.find("{{") {
        let rest = &rendered[start..];
        let end = rest.find("}}").map(|i| start + i + 2).unwrap_or(rendered.len());
        panic!(
            "unknown description template placeholder in tool description: {}; whitelist: {{CAP_TOOL_RESULT_MAX_BYTES}}, {{CAP_MAX_SEARCH_RESULTS}}, {{CAP_MAX_SEARCH_FILES}}",
            &rendered[start..end]
        );
    }
    rendered
}

fn resolved_schema(
    mut schema: serde_json::Value,
    name: &str,
    config: &ToolConfig,
) -> serde_json::Value {
    // 注入点说明（A2）：渲染必须在 surface_fingerprint 计算之前完成，
    // 保证"模型看到的描述"与"参与前缀指纹的描述"字节一致。
    if name == "Edit" {
        let mut schema = edit_schema(config);
        if let Some(desc) = schema.get("description").and_then(serde_json::Value::as_str) {
            let rendered = render_description_templates(desc, config);
            schema["description"] = serde_json::Value::String(rendered);
        }
        return schema;
    }
    // A1：结果标记协议——把"如何读结果"教给模型（全部来自现有实现行为）。
    let description = match (name, config.edit_mode) {
        ("Read", crate::config::EditMode::Hashline) => Some(
            "Read a local path or registered resource. Editable local non-raw output uses [PATH#TAG] plus numbered lines; raw output omits the header but still marks only its actual range as seen. Resource/VFS reads stay read-only and never mint tags. Output over {{CAP_TOOL_RESULT_MAX_BYTES}} bytes is rejected with an Error asking for a narrower line range; line numbers anchor later Edit or Read ranges.",
        ),
        ("Read", crate::config::EditMode::Replace) => Some(
            "Read a local path or registered resource using ordinary numbered output. Resource/VFS reads remain read-only. Output over {{CAP_TOOL_RESULT_MAX_BYTES}} bytes is rejected with an Error asking for a narrower line range; line numbers anchor later Edit or Read ranges.",
        ),
        ("Grep", crate::config::EditMode::Hashline) => Some(
            "Search local content or registered resources. Editable local results are grouped per file under [PATH#TAG], and only complete displayed match/context lines become seen. Read-only results retain ordinary search formatting. Results truncate at {{CAP_MAX_SEARCH_RESULTS}} matches or the output cap and end with a \"... truncated\" notice; use a narrower glob to converge.",
        ),
        ("Grep", crate::config::EditMode::Replace) => Some(
            "Search local content or registered resources using ordinary ripgrep-style path:line output. Results truncate at {{CAP_MAX_SEARCH_RESULTS}} matches or the output cap and end with a \"... truncated\" notice; use a narrower glob to converge.",
        ),
        ("Write", crate::config::EditMode::Hashline) => Some(
            "Create or fully overwrite a local file. A successful editable-size write records a new Hashline version and returns its [PATH#TAG] header without discarding older history. Failures return an Error line stating the reason (missing path, size limit, or write error).",
        ),
        ("Write", crate::config::EditMode::Replace) => Some(
            "Create or fully overwrite a local file while preserving the configured write-size and result-display protections. Failures return an Error line stating the reason (missing path, size limit, or write error).",
        ),
        _ => None,
    };
    if let Some(description) = description {
        schema["description"] = serde_json::Value::String(description.into());
    }
    if let Some(desc) = schema.get("description").and_then(serde_json::Value::as_str) {
        let rendered = render_description_templates(desc, config);
        schema["description"] = serde_json::Value::String(rendered);
    }
    schema
}

fn edit_schema(config: &ToolConfig) -> serde_json::Value {
    match config.edit_mode {
        crate::config::EditMode::Hashline => serde_json::json!({
            "name": "Edit",
            "description": format!("Apply Hashline sections to existing local files. Each section starts with [PATH#TAG], where TAG comes from a current-session Read/Grep/Write or successful Edit response. PUT N.=M replaces original snapshot lines, gap PUT inserts, CUT deletes/captures, bodyless @register PUT pastes, REM removes, and MV moves. Literal body lines start with + and are final file content. Every successful content change returns a new tag for the next Edit. Unknown or ambiguous stale tags require a fresh Read/Grep header. Seen-line enforcement is {}. Failures return an Error line with structured diagnostics (stale tag, ambiguous anchors, or permission reasons) and never apply partial sections. Legacy path/patch inputs and syntactic-block locators are unsupported.", if config.edit_enforce_seen_lines { "enabled" } else { "disabled" }),
            "input_schema": {
                "type": "object",
                "properties": {
                    "input": {
                        "type": "string",
                        "description": "Hashline patch containing one or more [PATH#TAG] sections. Example: [src/lib.rs#A1B2]\nPUT 10.=11:\n+new line"
                    }
                },
                "required": ["input"],
                "additionalProperties": false
            }
        }),
        crate::config::EditMode::Replace => serde_json::json!({
            "name": "Edit",
            "description": format!(
                "Replace content in one existing local file using ordered old_text/new_text edits. Matching is unique by default; all=true replaces every occurrence. Fuzzy matching is {} at threshold {:.3}. Failures return an Error line with structured diagnostics (ambiguous matches, missing file, or size limits) and never apply partial edits.",
                if config.edit_fuzzy_match { "enabled" } else { "disabled" },
                config.edit_fuzzy_threshold
            ),
            "input_schema": {
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "Path to an existing file" },
                    "edits": {
                        "type": "array",
                        "minItems": 1,
                        "items": {
                            "type": "object",
                            "properties": {
                                "old_text": { "type": "string" },
                                "new_text": { "type": "string" },
                                "all": { "type": "boolean", "default": false }
                            },
                            "required": ["old_text", "new_text"],
                            "additionalProperties": false
                        }
                    }
                },
                "required": ["path", "edits"],
                "additionalProperties": false
            }
        }),
    }
}

fn validate_dependency_graph(catalog: &ToolCatalog) -> Result<()> {
    let mut graph = BTreeMap::new();
    for dependency in TOOL_HARD_DEPENDENCIES {
        ensure!(
            catalog.get(dependency.tool).is_some(),
            "hard dependency references unknown tool '{}'",
            dependency.tool
        );
        ensure!(
            !dependency.requires.contains(&dependency.tool),
            "tool '{}' depends on itself",
            dependency.tool
        );
        for required in dependency.requires {
            ensure!(
                catalog.get(required).is_some(),
                "hard dependency for '{}' references unknown tool '{}'",
                dependency.tool,
                required
            );
        }
        graph.insert(dependency.tool, dependency.requires);
    }
    fn visit(
        node: &'static str,
        graph: &BTreeMap<&'static str, &'static [&'static str]>,
        visiting: &mut BTreeSet<&'static str>,
        visited: &mut BTreeSet<&'static str>,
    ) -> Result<()> {
        if visited.contains(node) {
            return Ok(());
        }
        ensure!(
            visiting.insert(node),
            "cycle in tool hard dependencies at '{node}'"
        );
        for dependency in graph.get(node).copied().unwrap_or_default() {
            visit(dependency, graph, visiting, visited)?;
        }
        visiting.remove(node);
        visited.insert(node);
        Ok(())
    }
    let mut visiting = BTreeSet::new();
    let mut visited = BTreeSet::new();
    for node in graph.keys().copied() {
        visit(node, &graph, &mut visiting, &mut visited)?;
    }
    Ok(())
}

fn surface_fingerprint(tools: &[ModelTool]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"mink-tool-surface-v1\0");
    for tool in tools {
        hasher.update(tool.metadata.name.as_bytes());
        hasher.update(b"\0");
        hasher.update(serde_json::to_vec(&tool.schema).unwrap_or_default());
        hasher.update(b"\0");
    }
    crate::util::hex_lower(hasher.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Config, ToolApprovalMode};

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
        // 渲染后的描述必须参与前缀指纹（A2 注入点约束）。
        let other = ToolConfig::from_config(&Config::default());
        assert_ne!(surface.fingerprint(), resolve(&other, AgentRole::Primary, false).unwrap().fingerprint());
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
}
