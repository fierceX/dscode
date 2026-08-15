use super::*;
use serde_json::json;

#[test]
fn fingerprint_is_deterministic() {
    let p1 = ImmutablePrefix::new(
        "you are an agent".into(),
        vec![json!({"name":"Bash"})],
        "deps".into(),
    );
    let p2 = ImmutablePrefix::new(
        "you are an agent".into(),
        vec![json!({"name":"Bash"})],
        "deps".into(),
    );
    assert_eq!(p1.fingerprint(), p2.fingerprint());
}

#[test]
fn fingerprint_changes_on_system_prompt_change() {
    let p1 = ImmutablePrefix::new(
        "you are an agent".into(),
        vec![json!({"name":"Bash"})],
        "deps".into(),
    );
    let p2 = ImmutablePrefix::new(
        "you are a different agent".into(),
        vec![json!({"name":"Bash"})],
        "deps".into(),
    );
    assert_ne!(p1.fingerprint(), p2.fingerprint());
}

#[test]
fn fingerprint_changes_on_tools_change() {
    let p1 = ImmutablePrefix::new("agent".into(), vec![json!({"name":"Bash"})], "deps".into());
    let p2 = ImmutablePrefix::new(
        "agent".into(),
        vec![json!({"name":"Bash"}), json!({"name":"Read"})],
        "deps".into(),
    );
    assert_ne!(p1.fingerprint(), p2.fingerprint());
}

#[test]
fn fingerprint_changes_on_dependency_fingerprint_change() {
    let p1 = ImmutablePrefix::new("agent".into(), vec![json!({"name":"Bash"})], "a".into());
    let p2 = ImmutablePrefix::new("agent".into(), vec![json!({"name":"Bash"})], "b".into());
    assert_ne!(p1.fingerprint(), p2.fingerprint());
}

#[test]
fn verify_fingerprint_succeeds() {
    let p = ImmutablePrefix::new("agent".into(), vec![json!({"name":"Bash"})], "deps".into());
    assert!(p.verify_fingerprint());
}
