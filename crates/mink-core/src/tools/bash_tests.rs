use super::*;
use std::fs;
use std::time::{SystemTime, UNIX_EPOCH};

fn temp_dir(name: &str) -> std::path::PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    std::env::temp_dir().join(format!("{name}-{}-{nanos}", std::process::id()))
}

#[test]
fn empty_command_error() {
    assert!(execute("", None, 600).is_err());
    assert!(execute("   ", None, 600).is_err());
}

#[test]
fn blocked_command_error() {
    assert!(execute("sudo rm /tmp/foo", None, 600).is_err());
}

#[test]
fn misuse_detector_routes_file_reading_to_read() {
    assert_eq!(
        bash_misuse_capability("cat src/main.rs"),
        Some(crate::tools::semantic_capabilities::ToolSemanticCapability::PathRead)
    );
}

#[test]
fn misuse_detector_routes_search_to_grep() {
    assert_eq!(
        bash_misuse_capability("rg TODO src"),
        Some(crate::tools::semantic_capabilities::ToolSemanticCapability::ContentSearch)
    );
}

#[test]
fn misuse_detector_routes_discovery_to_glob() {
    assert_eq!(
        bash_misuse_capability("find . -name '*.rs'"),
        Some(crate::tools::semantic_capabilities::ToolSemanticCapability::PathDiscovery)
    );
    assert_eq!(
        bash_misuse_capability("rg --files"),
        Some(crate::tools::semantic_capabilities::ToolSemanticCapability::PathDiscovery)
    );
}

#[test]
fn misuse_detector_allows_build_commands() {
    assert!(bash_misuse_capability("cargo test").is_none());
    assert!(bash_misuse_capability("git status --short").is_none());
    assert!(is_focused_verification_command("cargo test"));
    assert!(is_focused_verification_command("git status --short"));
    assert!(!is_focused_verification_command("cargo test; rm file"));
    assert!(!is_focused_verification_command("cargo test > out"));
}

fn routing_state(
    names: &[&str],
) -> (
    crate::tools::surface::ModelToolSurface,
    crate::tools::semantic_capabilities::ResolvedToolCapabilities,
) {
    let mut config =
        crate::context::ToolConfig::from_config(&crate::config::ResolvedConfig::default());
    config.enabled_tools = Some(names.iter().map(|name| (*name).to_string()).collect());
    let resolution = crate::tools::surface::ToolResolutionContext::from_runtime(
        crate::tools::surface::AgentRole::Primary,
        &config,
        false,
    );
    let surface = crate::tools::surface::ModelToolSurface::resolve(
        crate::tools::catalog::ToolCatalog::builtin().unwrap(),
        &config,
        &resolution,
    )
    .unwrap();
    let capabilities = crate::tools::semantic_capabilities::ToolCapabilityRegistry::builtin()
        .resolve(&surface, &resolution)
        .unwrap();
    (surface, capabilities)
}

#[test]
fn misuse_guidance_follows_resolved_provider_matrix() {
    let (bash, bash_caps) = routing_state(&["Bash"]);
    assert!(bash_misuse_guidance("cat file", &bash, &bash_caps).is_none());
    assert!(bash_misuse_guidance("rg term", &bash, &bash_caps).is_none());
    assert!(bash_misuse_guidance("find .", &bash, &bash_caps).is_none());

    let (read, read_caps) = routing_state(&["Bash", "Read"]);
    let guidance = bash_misuse_guidance("cat file", &read, &read_caps).unwrap();
    assert_eq!(
        guidance.referenced_tools,
        ["Read".to_string()].into_iter().collect()
    );
    assert!(bash_misuse_guidance("rg term", &read, &read_caps).is_none());

    let (grep, grep_caps) = routing_state(&["Bash", "Grep"]);
    assert!(bash_misuse_guidance("rg term", &grep, &grep_caps).is_some());
    assert!(bash_misuse_guidance("cat file", &grep, &grep_caps).is_none());

    let (glob, glob_caps) = routing_state(&["Bash", "Glob"]);
    assert!(bash_misuse_guidance("find .", &glob, &glob_caps).is_some());
    assert!(bash_misuse_guidance("cat file", &glob, &glob_caps).is_none());
}

#[test]
fn simple_echo_works() {
    let (result, _) = execute("echo hello", None, 600).unwrap();
    assert!(result.contains("hello"));
}

#[tokio::test]
async fn execute_works_inside_tokio_runtime() {
    let (result, code) = execute_with_interrupt("echo async-ok", None, 600, None).unwrap();
    assert_eq!(code, Some(0));
    assert!(result.contains("async-ok"));
}

#[test]
fn execute_in_dir_uses_requested_cwd() {
    let dir = temp_dir("mink-bash-cwd");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("marker.txt"), "ok").unwrap();

    let (result, code) = execute_with_interrupt_in_dir(
        "test -f marker.txt && echo found",
        None,
        600,
        None,
        Some(&dir),
    )
    .unwrap();

    assert_eq!(code, Some(0));
    assert!(result.contains("found"));
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn timeout_kills_long_command() {
    let (result, _) = execute("sleep 10; echo done", Some(1), 600).unwrap();
    assert!(result.contains("timed out"));
    assert!(!result.contains("done"));
}

#[test]
fn timeout_reports_exit_124() {
    let (result, code) = execute("sleep 5", Some(1), 600).unwrap();
    assert!(result.contains("timed out"));
    assert_eq!(code, Some(124));
}

#[test]
fn background_daemon_does_not_hang() {
    let start = std::time::Instant::now();
    let result = execute("sleep 2 &", None, 600);
    assert!(result.is_ok());
    assert!(start.elapsed() < std::time::Duration::from_secs(3));
}

#[test]
fn interrupt_kills_long_command() {
    let interrupt = AtomicBool::new(true);
    let (result, code) =
        execute_with_interrupt("sleep 10; echo done", Some(30), 600, Some(&interrupt)).unwrap();
    assert_eq!(code, Some(130));
    assert!(result.contains("interrupted"));
    assert!(!result.contains("done"));
}

#[test]
fn default_timeout_is_stable_without_execution_history() {
    let (result, _) = execute("sleep 1; echo done", None, 5).unwrap();
    assert!(result.contains("done"));
}
