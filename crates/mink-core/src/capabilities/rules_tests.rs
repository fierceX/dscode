use super::*;

#[test]
fn rule_snapshot_loads_builtin_rules() {
    let cwd = std::path::PathBuf::from("/tmp/project");
    let home = std::path::PathBuf::from("/tmp/home");
    let snapshot = build_default_rule_snapshot(&cwd, &home, "session", "session").unwrap();

    assert!(snapshot.by_name.contains_key("default-agent-rules"));
    assert_eq!(snapshot.always_apply.len(), 1);
    assert_eq!(snapshot.discoverable.len(), 1);
}
