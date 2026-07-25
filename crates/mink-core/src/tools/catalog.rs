use crate::config::TOOL_DISABLE_MAP;
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
            ensure!(
                registry.insert(metadata.name, metadata).is_none(),
                "duplicate executor registration for '{}'",
                metadata.name
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
            ordered.push(CatalogTool {
                name,
                schema,
                availability,
            });
        }

        ensure!(
            registry.is_empty(),
            "compiled executors missing schemas: {}",
            registry.keys().copied().collect::<Vec<_>>().join(", ")
        );
        for (name, _) in FEATURE_GATED_TOOLS {
            ensure!(
                by_name.contains_key(*name),
                "feature declaration references unknown tool '{name}'"
            );
        }
        for (name, _) in TOOL_DISABLE_MAP {
            ensure!(
                by_name.contains_key(*name),
                "disable flag references unknown tool '{name}'"
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
            .filter_map(|tool| match tool.availability {
                ToolBuildAvailability::Compiled { metadata } => Some((tool, metadata)),
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;

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
}
