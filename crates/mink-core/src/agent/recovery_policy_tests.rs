use super::*;
use crate::config::ResolvedConfig as Config;
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
        policy.classify_first_call(&allowed),
        RecoveryFirstCallDecision::Allowed
    ));
    let blocked = CapabilityCallContext {
        tool_name: "Bash",
        input: &json!({"command":"cargo test; rm file"}),
        resource_router: &router,
        filesystem_backend: FilesystemBackend::Local,
    };
    assert!(matches!(
        policy.classify_first_call(&blocked),
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
    match policy.classify_first_call(&call) {
        RecoveryFirstCallDecision::Allowed => panic!("write cannot inspect state"),
        RecoveryFirstCallDecision::Blocked(guidance) => {
            assert!(guidance.referenced_tools.is_empty());
            assert!(guidance.content.contains("no active inspection provider"));
        }
    }
}
