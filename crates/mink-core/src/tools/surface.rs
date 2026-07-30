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
}

impl ToolResolutionContext {
    pub fn from_runtime(role: AgentRole, _config: &ToolConfig, read_only_fs_present: bool) -> Self {
        Self {
            role,
            filesystem_backend: if read_only_fs_present {
                FilesystemBackend::ReadOnlyVfs
            } else {
                FilesystemBackend::Local
            },
        }
    }

    pub fn role(&self) -> AgentRole {
        self.role
    }

    pub fn filesystem_backend(&self) -> FilesystemBackend {
        self.filesystem_backend
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
    by_name: BTreeMap<&'static str, usize>,
    hidden: BTreeMap<String, ToolHiddenReason>,
    fingerprint: String,
}

struct ToolHardDependencySpec {
    tool: &'static str,
    requires: &'static [&'static str],
}

const TOOL_HARD_DEPENDENCIES: &[ToolHardDependencySpec] = &[ToolHardDependencySpec {
    tool: "Edit",
    requires: &["Read"],
}];

impl ModelToolSurface {
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
            let metadata = match tool.availability {
                ToolBuildAvailability::Compiled { metadata } => metadata,
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
            match authorize_tool(metadata, config) {
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
            candidates.insert(metadata.name);
        }

        for dependency in TOOL_HARD_DEPENDENCIES {
            if candidates.contains(dependency.tool) {
                for required in dependency.requires {
                    ensure!(
                        candidates.contains(required),
                        "tool '{}' requires active tool '{}'",
                        dependency.tool,
                        required
                    );
                }
            }
        }

        let ordered: Vec<ModelTool> = catalog
            .iter_compiled()
            .filter(|(_, metadata)| candidates.contains(metadata.name))
            .map(|(tool, metadata)| ModelTool {
                metadata,
                schema: tool.schema.clone(),
            })
            .collect();
        let by_name = ordered
            .iter()
            .enumerate()
            .map(|(index, tool)| (tool.metadata.name, index))
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

    pub fn names(&self) -> impl Iterator<Item = &'static str> + '_ {
        self.ordered.iter().map(|tool| tool.metadata.name)
    }

    pub fn hidden(&self) -> &BTreeMap<String, ToolHiddenReason> {
        &self.hidden
    }

    pub fn fingerprint(&self) -> &str {
        &self.fingerprint
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
