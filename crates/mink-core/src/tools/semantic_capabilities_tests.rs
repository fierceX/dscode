use super::*;
use crate::config::ResolvedConfig as Config;
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
    assert!(!resolved.has(FileEdit));
    assert!(!resolved.has(HashlineEdit));
    assert!(!resolved.has(ContentReplaceEdit));
    assert!(resolved.has(FileOverwrite));
}

#[test]
fn edit_modes_expose_mutually_exclusive_semantic_facts() {
    for (mode, snapshot, hashline, replace) in [
        (crate::config::EditMode::Hashline, true, true, false),
        (crate::config::EditMode::Replace, false, false, true),
    ] {
        let mut config = ToolConfig::from_config(&Config {
            edit_mode: mode,
            ..Config::default()
        });
        config.enabled_tools = Some(vec!["Read".into(), "Edit".into()]);
        let context = ToolResolutionContext::from_runtime(AgentRole::Primary, &config, false);
        let surface =
            ModelToolSurface::resolve(ToolCatalog::builtin().unwrap(), &config, &context).unwrap();
        let resolved = ToolCapabilityRegistry::builtin()
            .resolve(&surface, &context)
            .unwrap();
        assert_eq!(resolved.has(EditableSnapshotRead), snapshot);
        assert_eq!(resolved.has(HashlineEdit), hashline);
        assert_eq!(resolved.has(ContentReplaceEdit), replace);
        assert!(resolved.has(FileEdit));
    }
}
