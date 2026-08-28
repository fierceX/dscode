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
                approval: definition.approval,
                result_kind: definition.result_kind,
                mutating: definition.mutating,
                storm_exempt: definition.storm_exempt,
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

/// Statically augment the Read tool description for image-capable sessions
/// (v7 §3.4). Unsupported sessions return the schemas unchanged, preserving
/// the byte-for-byte text-only behavior. The advertised format list is
/// generated from the model's `allowed_mime` set (review fix).
pub fn augment_read_schema_for_image(
    tools: Vec<serde_json::Value>,
    capabilities: &crate::capabilities::model_capabilities::SessionModelCapabilities,
) -> Vec<serde_json::Value> {
    let Some(limits) = capabilities.image_input.limits() else {
        return tools;
    };
    let mut tools = tools;
    for tool in tools.iter_mut() {
        if tool.get("name").and_then(serde_json::Value::as_str) != Some("Read") {
            continue;
        }
        if let Some(description) = tool
            .get_mut("description")
            .and_then(|value| value.as_str())
            .map(str::to_string)
        {
            let mut description = description;
            let formats = limits
                .allowed_mime
                .iter()
                .map(|format| match format {
                    crate::tools::image::ImageFormat::Png => "PNG",
                    crate::tools::image::ImageFormat::Jpeg => "JPEG",
                    crate::tools::image::ImageFormat::Gif => "GIF",
                    crate::tools::image::ImageFormat::Webp => "WebP",
                })
                .collect::<Vec<_>>()
                .join("/");
            description.push_str("\n\nRead can capture supported raster images (");
            description.push_str(&formats);
            description.push_str(") and attach them to the next model request. ");
            description.push_str(&format!(
                "Image capture limits: {} images and {} bytes per request.",
                limits.max_images_per_request, limits.max_image_bytes_per_request
            ));
            tool["description"] = serde_json::Value::String(description);
        }
    }
    tools
}

#[cfg(test)]
mod image_augment_tests {
    use super::*;
    use crate::capabilities::model_capabilities::{
        ImageInputCapability, OpenAiChatImageUrlLimits, SessionModelCapabilities,
    };

    fn capabilities(image: ImageInputCapability) -> SessionModelCapabilities {
        let mut caps = SessionModelCapabilities {
            version: 1,
            initial_model: "m".into(),
            image_input: image,
            capability_fingerprint: String::new(),
        };
        caps.capability_fingerprint = caps.image_input.fingerprint();
        caps
    }

    fn read_schema(description: &str) -> serde_json::Value {
        serde_json::json!({"name": "Read", "description": description, "input_schema": {}})
    }

    #[test]
    fn unsupported_session_leaves_schemas_unchanged() {
        let tools = vec![read_schema("Read one path.")];
        let out = augment_read_schema_for_image(
            tools.clone(),
            &capabilities(ImageInputCapability::Unsupported),
        );
        assert_eq!(out, tools);
    }

    #[test]
    fn image_capable_session_augments_read_description() {
        let tools = vec![read_schema("Read one path.")];
        let out = augment_read_schema_for_image(
            tools,
            &capabilities(ImageInputCapability::OpenAiChatImageUrl(
                OpenAiChatImageUrlLimits::default(),
            )),
        );
        let description = out[0]["description"].as_str().unwrap();
        assert!(
            description.contains("capture supported raster images"),
            "{description}"
        );
        // Format list is generated from allowed_mime; per-request cap was
        // raised to 64MB so a single 20MB image is always admissible.
        assert!(description.contains("(PNG/JPEG/GIF/WebP)"), "{description}");
        assert!(
            description.contains("Image capture limits: 600 images and 16777216 bytes"),
            "{description}"
        );
    }
}

/// 将真实配置值注入工具描述中的 caps 占位符。
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
        let end = rest
            .find("}}")
            .map(|i| start + i + 2)
            .unwrap_or(rendered.len());
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
    // 渲染必须在 surface_fingerprint 计算之前完成，
    // 保证"模型看到的描述"与"参与前缀指纹的描述"字节一致。
    if name == "Edit" {
        let mut schema = edit_schema(config);
        if let Some(desc) = schema
            .get("description")
            .and_then(serde_json::Value::as_str)
        {
            let rendered = render_description_templates(desc, config);
            schema["description"] = serde_json::Value::String(rendered);
        }
        return schema;
    }
    // 将结果标记协议追加到工具描述，让模型正确读取现有输出。
    let description = match (name, config.edit_mode) {
        ("Read", crate::config::EditMode::Hashline) => Some(
            "Read a local path or registered resource. Editable local non-raw output uses [PATH#TAG] plus numbered lines and marks its actual range as seen; raw output omits the header and does not create a snapshot or advance seen-lines. Resource/VFS reads stay read-only and never mint tags. Output over {{CAP_TOOL_RESULT_MAX_BYTES}} bytes is rejected with an Error asking for a narrower line range; line numbers anchor later Edit or Read ranges.",
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
    if let Some(desc) = schema
        .get("description")
        .and_then(serde_json::Value::as_str)
    {
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
    if let Some(node) = super::first_dependency_cycle(graph.keys().copied(), |node| {
        graph.get(node).copied().unwrap_or_default().to_vec()
    }) {
        bail!("cycle in tool hard dependencies at '{node}'");
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
    crate::capabilities::fingerprint::hex_lower(hasher.finalize())
}

#[cfg(test)]
#[path = "surface_tests.rs"]
mod tests;
