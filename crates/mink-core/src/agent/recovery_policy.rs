use crate::tools::catalog::ToolCatalog;
use crate::tools::runtime_guidance::RenderedRuntimeGuidance;
use crate::tools::semantic_capabilities::{
    CapabilityCallContext, CapabilityProvider, ResolvedToolCapabilities, ToolSemanticCapability,
    call_satisfies_capability,
};
use std::collections::BTreeSet;

#[derive(Debug, Clone)]
pub struct InspectionAction {
    pub capability: ToolSemanticCapability,
    pub provider: CapabilityProvider,
}

#[derive(Debug, Clone)]
pub struct RecoveryPolicy {
    inspection_actions: Vec<InspectionAction>,
}

pub enum RecoveryFirstCallDecision {
    Allowed,
    Blocked(RenderedRuntimeGuidance),
}

impl RecoveryPolicy {
    pub fn from_resolved(resolved: &ResolvedToolCapabilities, catalog: &ToolCatalog) -> Self {
        use ToolSemanticCapability::*;
        let mut seen = BTreeSet::new();
        let mut inspection_actions = Vec::new();
        for capability in [
            PathRead,
            ContentSearch,
            PathDiscovery,
            ResourceRead,
            ResourceSearch,
            FocusedVerificationExec,
        ] {
            let Some(provider) = resolved.primary_provider(capability).cloned() else {
                continue;
            };
            let Some(tool) = catalog.get(&provider.tool) else {
                continue;
            };
            let metadata = match &tool.availability {
                crate::tools::catalog::ToolBuildAvailability::Compiled { metadata } => metadata,
                crate::tools::catalog::ToolBuildAvailability::FeatureUnavailable { .. } => continue,
            };
            if metadata.mutating && capability != FocusedVerificationExec {
                continue;
            }
            if seen.insert((provider.tool.clone(), capability)) {
                inspection_actions.push(InspectionAction {
                    capability,
                    provider,
                });
            }
        }
        Self { inspection_actions }
    }

    pub fn classify_first_call(
        &self,
        call: &CapabilityCallContext<'_>,
        _catalog: &ToolCatalog,
    ) -> RecoveryFirstCallDecision {
        if self
            .inspection_actions
            .iter()
            .any(|action| call_satisfies_capability(action.capability, &action.provider, call))
        {
            return RecoveryFirstCallDecision::Allowed;
        }
        let referenced_tools: BTreeSet<_> = self
            .inspection_actions
            .iter()
            .map(|action| action.provider.tool.clone())
            .collect();
        let content = if referenced_tools.is_empty() {
            "SIGNAL_RECOVERY guard: no tool call is allowed because no active inspection provider is available. Report the limitation and stop the turn.".to_string()
        } else {
            format!(
                "SIGNAL_RECOVERY guard: this call was not executed because it does not satisfy an approved first-step inspection. Use one of: {}.",
                referenced_tools
                    .iter()
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        };
        RecoveryFirstCallDecision::Blocked(RenderedRuntimeGuidance {
            content,
            referenced_tools,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::context::ToolConfig;
    use crate::resources::ResourceRouter;
    use crate::tools::catalog::ToolCatalog;
    use crate::tools::semantic_capabilities::ToolCapabilityRegistry;
    use crate::tools::surface::{
        AgentRole, FilesystemBackend, ModelToolSurface, ToolResolutionContext,
    };
    use serde_json::json;

    fn policy(names: &[&str]) -> RecoveryPolicy {
        let mut config = ToolConfig::from_config(&Config::default());
        config.enabled_tools = Some(names.iter().map(|name| (*name).to_string()).collect());
        let context = ToolResolutionContext::from_runtime(AgentRole::Primary, &config, false);
        let surface =
            ModelToolSurface::resolve(ToolCatalog::builtin().unwrap(), &config, &context).unwrap();
        let capabilities = ToolCapabilityRegistry::builtin()
            .resolve(&surface, &context)
            .unwrap();
        RecoveryPolicy::from_resolved(&capabilities, ToolCatalog::builtin().unwrap())
    }

    #[test]
    fn focused_execution_is_fail_closed() {
        let policy = policy(&["Bash"]);
        let router = ResourceRouter::with_builtin_handlers();
        let allowed = CapabilityCallContext {
            tool_name: "Bash",
            input: &json!({"command":"cargo test"}),
            resource_router: &router,
            filesystem_backend: FilesystemBackend::Local,
        };
        assert!(matches!(
            policy.classify_first_call(&allowed, ToolCatalog::builtin().unwrap()),
            RecoveryFirstCallDecision::Allowed
        ));
        let blocked = CapabilityCallContext {
            tool_name: "Bash",
            input: &json!({"command":"cargo test; rm file"}),
            resource_router: &router,
            filesystem_backend: FilesystemBackend::Local,
        };
        assert!(matches!(
            policy.classify_first_call(&blocked, ToolCatalog::builtin().unwrap()),
            RecoveryFirstCallDecision::Blocked(_)
        ));
    }

    #[test]
    fn no_inspection_surface_blocks_every_tool_call_without_references() {
        let policy = policy(&["Write"]);
        let router = ResourceRouter::with_builtin_handlers();
        let call = CapabilityCallContext {
            tool_name: "Write",
            input: &json!({"path":"x","content":"y"}),
            resource_router: &router,
            filesystem_backend: FilesystemBackend::Local,
        };
        match policy.classify_first_call(&call, ToolCatalog::builtin().unwrap()) {
            RecoveryFirstCallDecision::Allowed => panic!("write cannot inspect state"),
            RecoveryFirstCallDecision::Blocked(guidance) => {
                assert!(guidance.referenced_tools.is_empty());
                assert!(guidance.content.contains("no active inspection provider"));
            }
        }
    }
}
