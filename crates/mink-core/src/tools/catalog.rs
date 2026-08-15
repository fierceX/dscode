use crate::context::ToolConfig;
use crate::tools::metadata::ToolMetadata;
use anyhow::{Result, bail, ensure};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::LazyLock;

const FEATURE_GATED_TOOLS: &[(&str, &str)] = &[("PythonSandbox", "python-sandbox")];

#[derive(Clone)]
pub struct CatalogTool {
    pub name: String,
    pub schema: serde_json::Value,
    pub availability: ToolBuildAvailability,
    pub default_activation: ToolDefaultActivation,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolDefaultActivation {
    Enabled,
    ExplicitOnly,
}

#[derive(Clone)]
pub enum ToolBuildAvailability {
    Compiled { metadata: ToolMetadata },
    FeatureUnavailable { required_feature: &'static str },
}

pub struct ToolCatalog {
    ordered: Vec<CatalogTool>,
    by_name: BTreeMap<String, usize>,
}

static BUILTIN: LazyLock<std::result::Result<ToolCatalog, String>> =
    LazyLock::new(|| ToolCatalog::load().map_err(|error| error.to_string()));

impl ToolCatalog {
    pub fn builtin() -> Result<&'static Self> {
        BUILTIN
            .as_ref()
            .map_err(|error| anyhow::anyhow!("invalid built-in tool catalog: {error}"))
    }

    fn load() -> Result<Self> {
        let schemas: Vec<serde_json::Value> = serde_json::from_str(crate::assets::TOOLS_JSON)?;
        let mut registry = BTreeMap::new();
        for tool in crate::tools::runner::tool_registry() {
            let metadata = tool.metadata();
            let name = metadata.name.to_string();
            ensure!(
                registry.insert(name.clone(), metadata).is_none(),
                "duplicate executor registration for '{}'",
                name
            );
        }

        let feature_gates: BTreeMap<_, _> = FEATURE_GATED_TOOLS.iter().copied().collect();
        let mut ordered = Vec::with_capacity(schemas.len());
        let mut by_name = BTreeMap::new();
        for schema in schemas {
            let name = schema
                .get("name")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| anyhow::anyhow!("tool schema is missing a string name"))?
                .to_string();
            ensure!(
                !by_name.contains_key(&name),
                "duplicate schema for tool '{name}'"
            );
            let availability = if let Some(metadata) = registry.remove(name.as_str()) {
                ToolBuildAvailability::Compiled { metadata }
            } else if let Some(required_feature) = feature_gates.get(name.as_str()) {
                ToolBuildAvailability::FeatureUnavailable { required_feature }
            } else {
                bail!("schema for '{name}' has no compiled executor or feature declaration");
            };
            by_name.insert(name.clone(), ordered.len());
            let default_activation = if name == "PythonSandbox" {
                ToolDefaultActivation::ExplicitOnly
            } else {
                ToolDefaultActivation::Enabled
            };
            ordered.push(CatalogTool {
                name,
                schema,
                availability,
                default_activation,
            });
        }

        ensure!(
            registry.is_empty(),
            "compiled executors missing schemas: {}",
            registry.keys().cloned().collect::<Vec<_>>().join(", ")
        );
        for (name, _) in FEATURE_GATED_TOOLS {
            ensure!(
                by_name.contains_key(*name),
                "feature declaration references unknown tool '{name}'"
            );
        }
        Ok(Self { ordered, by_name })
    }

    pub fn get(&self, name: &str) -> Option<&CatalogTool> {
        self.by_name.get(name).map(|index| &self.ordered[*index])
    }

    pub fn iter(&self) -> impl Iterator<Item = &CatalogTool> {
        self.ordered.iter()
    }

    pub fn iter_compiled(&self) -> impl Iterator<Item = (&CatalogTool, ToolMetadata)> {
        self.ordered
            .iter()
            .filter_map(|tool| match &tool.availability {
                ToolBuildAvailability::Compiled { metadata } => Some((tool, metadata.clone())),
                ToolBuildAvailability::FeatureUnavailable { .. } => None,
            })
    }

    pub fn order_of(&self, name: &str) -> Option<usize> {
        self.by_name.get(name).copied()
    }

    pub fn validate_configured_names(&self, config: &ToolConfig) -> Result<()> {
        if let Some(names) = &config.enabled_tools {
            let mut seen = BTreeSet::new();
            for name in names {
                ensure!(seen.insert(name), "duplicate enabled tool '{name}'");
                self.validate_explicit_name(name, "enabled_tools")?;
            }
        }
        for name in config.tool_approval.keys() {
            self.validate_explicit_name(name, "tool approval override")?;
        }
        Ok(())
    }

    fn validate_explicit_name(&self, name: &str, source: &str) -> Result<()> {
        let Some(tool) = self.get(name) else {
            bail!(
                "unknown tool '{name}' in {source}; known tools: {}",
                self.ordered
                    .iter()
                    .map(|tool| tool.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        };
        if let ToolBuildAvailability::FeatureUnavailable { required_feature } = tool.availability {
            bail!(
                "tool '{name}' in {source} requires the '{required_feature}' feature in this build"
            );
        }
        Ok(())
    }
}

pub fn validate_tool_config(config: &ToolConfig) -> Result<()> {
    ToolCatalog::builtin()?.validate_configured_names(config)
}

pub(crate) fn validate_custom_tools(tools: &[crate::runtime::RegisteredCustomTool]) -> Result<()> {
    let builtin = ToolCatalog::builtin()?;
    let mut names = BTreeSet::new();
    for tool in tools {
        let definition = &tool.definition;
        ensure!(
            !definition.name.trim().is_empty(),
            "custom tool name must not be empty"
        );
        ensure!(
            builtin.get(&definition.name).is_none(),
            "custom tool '{}' cannot override a built-in",
            definition.name
        );
        ensure!(
            names.insert(definition.name.clone()),
            "duplicate custom tool '{}'",
            definition.name
        );
        ensure!(
            definition
                .input_schema
                .get("type")
                .and_then(serde_json::Value::as_str)
                == Some("object"),
            "custom tool '{}' schema must be an object",
            definition.name
        );
        ensure!(
            definition.input_schema.get("additionalProperties") == Some(&serde_json::json!(false)),
            "custom tool '{}' schema must set additionalProperties:false",
            definition.name
        );
        ensure!(
            !definition.mutating
                || definition.execution == crate::runtime::ToolExecutionMode::Sequential,
            "mutating custom tool '{}' must be Sequential",
            definition.name
        );
        for offer in &definition.semantic_capabilities {
            ensure!(
                offer.priority > 0,
                "custom tool '{}' capability priority must be nonzero",
                definition.name
            );
        }
    }
    for tool in tools {
        let definition = &tool.definition;
        for required in &definition.hard_dependencies {
            ensure!(
                builtin.get(required).is_some() || names.contains(required),
                "custom tool '{}' requires missing tool '{}'",
                definition.name,
                required
            );
        }
    }
    fn visit(
        name: &str,
        definitions: &BTreeMap<String, crate::runtime::ToolDefinition>,
        visiting: &mut BTreeSet<String>,
        visited: &mut BTreeSet<String>,
    ) -> Result<()> {
        if visited.contains(name) {
            return Ok(());
        }
        ensure!(
            visiting.insert(name.to_string()),
            "custom tool dependency cycle includes '{name}'"
        );
        if let Some(definition) = definitions.get(name) {
            for dependency in &definition.hard_dependencies {
                if definitions.contains_key(dependency) {
                    visit(dependency, definitions, visiting, visited)?;
                }
            }
        }
        visiting.remove(name);
        visited.insert(name.to_string());
        Ok(())
    }
    let definitions = tools
        .iter()
        .map(|tool| (tool.definition.name.clone(), tool.definition.clone()))
        .collect::<BTreeMap<_, _>>();
    let mut visiting = BTreeSet::new();
    let mut visited = BTreeSet::new();
    for name in definitions.keys() {
        visit(name, &definitions, &mut visiting, &mut visited)?;
    }
    Ok(())
}

#[cfg(test)]
#[path = "catalog_tests.rs"]
mod tests;
