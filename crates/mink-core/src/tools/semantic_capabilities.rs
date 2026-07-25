use crate::resources::ResourceRouter;
use crate::tools::catalog::ToolCatalog;
use crate::tools::surface::{FilesystemBackend, ModelToolSurface, ToolResolutionContext};
use anyhow::{Result, ensure};
use sha2::{Digest, Sha256};
use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::LazyLock;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ToolSemanticCapability {
    PathRead,
    EditableSnapshotRead,
    ResourceRead,
    PathDiscovery,
    ContentSearch,
    ResourceSearch,
    FileCreate,
    FileOverwrite,
    AnchoredEdit,
    ShellExec,
    FocusedVerificationExec,
    HostPythonExec,
    SandboxedPythonExec,
    DataCompute,
    WebSearch,
    HttpFetch,
    TodoState,
    PlanConfirm,
    PlanClear,
    Delegation,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ProviderTier {
    Fallback,
    Specialized,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum CapabilityUseScope {
    Unconditional,
    FilesystemPath,
    LocalPath,
    LocalNonRawPath,
    RegisteredResource,
    HttpUrl,
    FocusedVerificationCommand,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CapabilityAvailability {
    Always,
    WebEnabled,
    LocalFilesystemBackend,
}

#[derive(Debug, Clone, Copy)]
pub struct CapabilityOfferSpec {
    pub provider_tool: &'static str,
    pub capability: ToolSemanticCapability,
    pub tier: ProviderTier,
    pub priority: u16,
    pub available_if: CapabilityAvailability,
    pub use_scope: CapabilityUseScope,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CapabilityProvider {
    pub tool: &'static str,
    pub tier: ProviderTier,
    pub priority: u16,
    pub use_scope: CapabilityUseScope,
}

#[derive(Debug, Clone)]
pub struct CapabilityBinding {
    pub capability: ToolSemanticCapability,
    pub primary: CapabilityProvider,
    pub alternatives: Vec<CapabilityProvider>,
}

#[derive(Debug, Clone)]
pub struct ResolvedToolCapabilities {
    bindings: BTreeMap<ToolSemanticCapability, CapabilityBinding>,
    fingerprint: String,
}

pub struct ToolCapabilityRegistry {
    offers: &'static [CapabilityOfferSpec],
}

use CapabilityAvailability::{Always, LocalFilesystemBackend, WebEnabled};
use CapabilityUseScope::{
    FilesystemPath, FocusedVerificationCommand, HttpUrl, LocalNonRawPath, LocalPath,
    RegisteredResource, Unconditional,
};
use ProviderTier::{Fallback, Specialized};
use ToolSemanticCapability::*;

macro_rules! offer {
    ($tool:literal, $cap:ident, $tier:ident, $priority:literal, $availability:ident, $scope:ident) => {
        CapabilityOfferSpec {
            provider_tool: $tool,
            capability: $cap,
            tier: $tier,
            priority: $priority,
            available_if: $availability,
            use_scope: $scope,
        }
    };
}

static OFFERS: &[CapabilityOfferSpec] = &[
    offer!("Read", PathRead, Specialized, 100, Always, FilesystemPath),
    offer!(
        "Read",
        ResourceRead,
        Specialized,
        100,
        Always,
        RegisteredResource
    ),
    offer!(
        "Read",
        EditableSnapshotRead,
        Specialized,
        100,
        LocalFilesystemBackend,
        LocalNonRawPath
    ),
    offer!("Read", HttpFetch, Specialized, 80, WebEnabled, HttpUrl),
    offer!(
        "Glob",
        PathDiscovery,
        Specialized,
        100,
        Always,
        FilesystemPath
    ),
    offer!(
        "Grep",
        ContentSearch,
        Specialized,
        100,
        Always,
        FilesystemPath
    ),
    offer!(
        "Grep",
        ResourceSearch,
        Specialized,
        100,
        Always,
        RegisteredResource
    ),
    offer!("Write", FileCreate, Specialized, 100, Always, LocalPath),
    offer!("Write", FileOverwrite, Specialized, 100, Always, LocalPath),
    offer!(
        "Edit",
        AnchoredEdit,
        Specialized,
        100,
        LocalFilesystemBackend,
        LocalPath
    ),
    offer!("Bash", ShellExec, Specialized, 100, Always, Unconditional),
    offer!(
        "Bash",
        FocusedVerificationExec,
        Specialized,
        100,
        Always,
        FocusedVerificationCommand
    ),
    offer!(
        "Bash",
        PathRead,
        Fallback,
        10,
        LocalFilesystemBackend,
        LocalPath
    ),
    offer!(
        "Bash",
        ContentSearch,
        Fallback,
        10,
        LocalFilesystemBackend,
        LocalPath
    ),
    offer!(
        "Bash",
        PathDiscovery,
        Fallback,
        10,
        LocalFilesystemBackend,
        LocalPath
    ),
    offer!(
        "Python",
        HostPythonExec,
        Specialized,
        100,
        Always,
        Unconditional
    ),
    offer!(
        "Python",
        DataCompute,
        Specialized,
        80,
        Always,
        Unconditional
    ),
    offer!(
        "PythonSandbox",
        SandboxedPythonExec,
        Specialized,
        100,
        Always,
        Unconditional
    ),
    offer!(
        "PythonSandbox",
        DataCompute,
        Specialized,
        100,
        Always,
        Unconditional
    ),
    offer!(
        "WebSearch",
        WebSearch,
        Specialized,
        100,
        WebEnabled,
        Unconditional
    ),
    offer!("WebFetch", HttpFetch, Specialized, 100, WebEnabled, HttpUrl),
    offer!(
        "TodoWrite",
        TodoState,
        Specialized,
        100,
        Always,
        Unconditional
    ),
    offer!(
        "PlanConfirm",
        PlanConfirm,
        Specialized,
        100,
        Always,
        Unconditional
    ),
    offer!(
        "PlanClear",
        PlanClear,
        Specialized,
        100,
        Always,
        Unconditional
    ),
    offer!(
        "SubAgent",
        Delegation,
        Specialized,
        100,
        Always,
        Unconditional
    ),
];

const CAPABILITY_HARD_DEPENDENCIES: &[(ToolSemanticCapability, &[ToolSemanticCapability])] =
    &[(AnchoredEdit, &[EditableSnapshotRead])];

static BUILTIN: LazyLock<ToolCapabilityRegistry> =
    LazyLock::new(|| ToolCapabilityRegistry { offers: OFFERS });

impl ToolCapabilityRegistry {
    pub fn builtin() -> &'static Self {
        &BUILTIN
    }

    pub fn resolve(
        &self,
        surface: &ModelToolSurface,
        context: &ToolResolutionContext,
    ) -> Result<ResolvedToolCapabilities> {
        self.validate()?;
        let catalog = ToolCatalog::builtin()?;
        let mut grouped: BTreeMap<ToolSemanticCapability, Vec<CapabilityProvider>> =
            BTreeMap::new();
        for offer in self.offers {
            if !surface.has(offer.provider_tool)
                || !availability_matches(offer.available_if, context)
            {
                continue;
            }
            grouped
                .entry(offer.capability)
                .or_default()
                .push(CapabilityProvider {
                    tool: offer.provider_tool,
                    tier: offer.tier,
                    priority: offer.priority,
                    use_scope: offer.use_scope,
                });
        }
        let mut bindings = BTreeMap::new();
        for (capability, mut providers) in grouped {
            providers.sort_by(|left, right| compare_provider(left, right, catalog));
            let primary = providers.remove(0);
            bindings.insert(
                capability,
                CapabilityBinding {
                    capability,
                    primary,
                    alternatives: providers,
                },
            );
        }
        for (capability, requirements) in CAPABILITY_HARD_DEPENDENCIES {
            if bindings.contains_key(capability) {
                for required in *requirements {
                    ensure!(
                        bindings.contains_key(required),
                        "capability '{capability:?}' requires '{required:?}'"
                    );
                }
            }
        }
        let fingerprint = capability_fingerprint(&bindings);
        Ok(ResolvedToolCapabilities {
            bindings,
            fingerprint,
        })
    }

    fn validate(&self) -> Result<()> {
        let catalog = ToolCatalog::builtin()?;
        let mut pairs = BTreeSet::new();
        let capabilities: BTreeSet<_> = self.offers.iter().map(|offer| offer.capability).collect();
        for offer in self.offers {
            ensure!(
                catalog.get(offer.provider_tool).is_some(),
                "capability provider '{}' is not a known tool",
                offer.provider_tool
            );
            ensure!(
                offer.priority > 0,
                "capability offer priority must be nonzero"
            );
            ensure!(
                pairs.insert((offer.provider_tool, offer.capability)),
                "duplicate capability offer for '{}' and '{:?}'",
                offer.provider_tool,
                offer.capability
            );
        }
        for (capability, requirements) in CAPABILITY_HARD_DEPENDENCIES {
            ensure!(capabilities.contains(capability));
            for required in *requirements {
                ensure!(capabilities.contains(required));
                ensure!(capability != required, "capability cannot depend on itself");
            }
        }
        let graph: BTreeMap<_, _> = CAPABILITY_HARD_DEPENDENCIES.iter().copied().collect();
        fn visit(
            node: ToolSemanticCapability,
            graph: &BTreeMap<ToolSemanticCapability, &[ToolSemanticCapability]>,
            visiting: &mut BTreeSet<ToolSemanticCapability>,
            visited: &mut BTreeSet<ToolSemanticCapability>,
        ) -> Result<()> {
            if visited.contains(&node) {
                return Ok(());
            }
            ensure!(
                visiting.insert(node),
                "cycle in capability hard dependencies at '{node:?}'"
            );
            for dependency in graph.get(&node).copied().unwrap_or_default() {
                visit(*dependency, graph, visiting, visited)?;
            }
            visiting.remove(&node);
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
}

impl ResolvedToolCapabilities {
    pub fn has(&self, capability: ToolSemanticCapability) -> bool {
        self.bindings.contains_key(&capability)
    }

    pub fn binding(&self, capability: ToolSemanticCapability) -> Option<&CapabilityBinding> {
        self.bindings.get(&capability)
    }

    pub fn primary_provider(
        &self,
        capability: ToolSemanticCapability,
    ) -> Option<&CapabilityProvider> {
        self.binding(capability).map(|binding| &binding.primary)
    }

    pub fn iter(&self) -> impl Iterator<Item = (&ToolSemanticCapability, &CapabilityBinding)> {
        self.bindings.iter()
    }

    pub fn fingerprint(&self) -> &str {
        &self.fingerprint
    }
}

fn availability_matches(
    availability: CapabilityAvailability,
    context: &ToolResolutionContext,
) -> bool {
    match availability {
        CapabilityAvailability::Always => true,
        CapabilityAvailability::WebEnabled => context.web_enabled(),
        CapabilityAvailability::LocalFilesystemBackend => {
            context.filesystem_backend() == FilesystemBackend::Local
        }
    }
}

fn compare_provider(
    left: &CapabilityProvider,
    right: &CapabilityProvider,
    catalog: &ToolCatalog,
) -> Ordering {
    right
        .tier
        .cmp(&left.tier)
        .then_with(|| right.priority.cmp(&left.priority))
        .then_with(|| {
            catalog
                .order_of(left.tool)
                .unwrap_or(usize::MAX)
                .cmp(&catalog.order_of(right.tool).unwrap_or(usize::MAX))
        })
        .then_with(|| left.tool.cmp(right.tool))
}

fn capability_fingerprint(
    bindings: &BTreeMap<ToolSemanticCapability, CapabilityBinding>,
) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"mink-tool-capabilities-v1\0");
    for (capability, binding) in bindings {
        hasher.update(format!("{capability:?}\0").as_bytes());
        for provider in std::iter::once(&binding.primary).chain(&binding.alternatives) {
            hasher.update(
                format!(
                    "{}:{:?}:{}:{:?}\0",
                    provider.tool, provider.tier, provider.priority, provider.use_scope
                )
                .as_bytes(),
            );
        }
    }
    crate::util::hex_lower(hasher.finalize())
}

pub struct CapabilityCallContext<'a> {
    pub tool_name: &'a str,
    pub input: &'a serde_json::Value,
    pub resource_router: &'a ResourceRouter,
    pub filesystem_backend: FilesystemBackend,
}

pub fn call_satisfies_capability(
    capability: ToolSemanticCapability,
    provider: &CapabilityProvider,
    call: &CapabilityCallContext<'_>,
) -> bool {
    if call.tool_name != provider.tool {
        return false;
    }
    match provider.use_scope {
        CapabilityUseScope::Unconditional => true,
        CapabilityUseScope::FilesystemPath => {
            if matches!(provider.tool, "Glob" | "Grep") {
                let path = call
                    .input
                    .get("path")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or(".");
                return !call.resource_router.can_handle(path)
                    && !call.resource_router.is_url_like(path);
            }
            let Ok(target) = classify_path_input(call) else {
                return false;
            };
            matches!(
                target,
                crate::tools::file::ReadTargetClass::Filesystem { .. }
            )
        }
        CapabilityUseScope::LocalPath => {
            let Ok(target) = classify_path_input(call) else {
                return false;
            };
            matches!(
                target,
                crate::tools::file::ReadTargetClass::Filesystem {
                    backend: FilesystemBackend::Local,
                    ..
                }
            )
        }
        CapabilityUseScope::LocalNonRawPath => {
            let Ok(target) = classify_path_input(call) else {
                return false;
            };
            matches!(
                target,
                crate::tools::file::ReadTargetClass::Filesystem {
                    backend: FilesystemBackend::Local,
                    raw: false
                }
            )
        }
        CapabilityUseScope::RegisteredResource => {
            let Some(path) = call.input.get("path").and_then(serde_json::Value::as_str) else {
                return false;
            };
            let Ok(selection) = crate::resources::selector::split_read_path_selection(path) else {
                return false;
            };
            call.resource_router.can_handle(&selection.path)
        }
        CapabilityUseScope::HttpUrl => {
            let key = if provider.tool == "WebFetch" {
                "url"
            } else {
                "path"
            };
            call.input
                .get(key)
                .and_then(serde_json::Value::as_str)
                .is_some_and(|url| url.starts_with("http://") || url.starts_with("https://"))
        }
        CapabilityUseScope::FocusedVerificationCommand => {
            capability == FocusedVerificationExec
                && call
                    .input
                    .get("command")
                    .and_then(serde_json::Value::as_str)
                    .is_some_and(crate::tools::bash::is_focused_verification_command)
        }
    }
}

fn classify_path_input(
    call: &CapabilityCallContext<'_>,
) -> Result<crate::tools::file::ReadTargetClass> {
    crate::tools::file::classify_read_target(
        call.input,
        call.resource_router,
        call.filesystem_backend,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::context::ToolConfig;
    use crate::tools::surface::{AgentRole, ModelToolSurface, ToolResolutionContext};

    fn resolved(names: &[&str], vfs: bool) -> ResolvedToolCapabilities {
        let mut config = ToolConfig::from_config(&Config::default());
        config.enabled_tools = Some(names.iter().map(|name| (*name).to_string()).collect());
        let context = ToolResolutionContext::from_runtime(AgentRole::Primary, &config, vfs);
        let surface =
            ModelToolSurface::resolve(ToolCatalog::builtin().unwrap(), &config, &context).unwrap();
        ToolCapabilityRegistry::builtin()
            .resolve(&surface, &context)
            .unwrap()
    }

    #[test]
    fn specialized_provider_wins_and_fallback_remains() {
        let resolved = resolved(&["Read", "Grep", "Glob", "Bash"], false);
        assert_eq!(resolved.primary_provider(PathRead).unwrap().tool, "Read");
        assert_eq!(
            resolved.binding(PathRead).unwrap().alternatives[0].tool,
            "Bash"
        );
        assert_eq!(
            resolved.primary_provider(ContentSearch).unwrap().tool,
            "Grep"
        );
        assert_eq!(
            resolved.primary_provider(PathDiscovery).unwrap().tool,
            "Glob"
        );
    }

    #[test]
    fn vfs_removes_local_only_capabilities() {
        let resolved = resolved(&["Read", "Glob", "Grep", "Write"], true);
        assert!(resolved.has(PathRead));
        assert!(!resolved.has(EditableSnapshotRead));
        assert!(!resolved.has(AnchoredEdit));
        assert!(resolved.has(FileOverwrite));
    }
}
