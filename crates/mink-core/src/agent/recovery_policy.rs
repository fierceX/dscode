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
#[path = "recovery_policy_tests.rs"]
mod tests;
