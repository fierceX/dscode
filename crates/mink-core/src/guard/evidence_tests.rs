use super::*;

fn args(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
    pairs
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

#[test]
fn detects_consecutive_repeats() {
    let mut t = EvidenceTracker::new(8, 4);
    for _ in 0..3 {
        t.record(
            "Grep",
            &args(&[("pattern", "fn load")]),
            "",
            false,
            false,
            vec![],
        );
    }
    t.record("Read", &args(&[("path", "a.rs")]), "", false, false, vec![]);
    let batch = t.render(4_000, 0.75);
    assert!(batch.text.contains("repeated 3 consecutive times"));
}

#[test]
fn failure_cluster_survives_budget() {
    let mut t = EvidenceTracker::new(8, 4);
    t.record(
        "Bash",
        &args(&[("command", "cargo test")]),
        "ProcessFailed",
        true,
        true,
        vec![],
    );
    t.record(
        "Bash",
        &args(&[("command", "cargo test")]),
        "ProcessFailed",
        true,
        true,
        vec![],
    );
    let batch = t.render(200, 0.4);
    assert!(batch.text.contains("[detector]"));
    assert!(batch.text.chars().count() <= 220);
}

#[test]
fn freshness_dedup() {
    let mut t = EvidenceTracker::new(8, 2);
    let batch = t.render(4_000, 0.5);
    let hash = batch.hash;
    assert!(t.is_fresh(hash));
    t.mark_injected(hash);
    assert!(!t.is_fresh(hash));
}

#[test]
fn edited_paths_deduped_in_order() {
    let mut t = EvidenceTracker::new(8, 2);
    t.record(
        "Edit",
        &args(&[("path", "a.rs")]),
        "",
        false,
        false,
        vec!["a.rs".into()],
    );
    t.record(
        "Edit",
        &args(&[("path", "a.rs")]),
        "StaleTag",
        true,
        true,
        vec!["a.rs".into()],
    );
    t.record(
        "Edit",
        &args(&[("path", "b.rs")]),
        "",
        false,
        false,
        vec!["b.rs".into()],
    );
    assert_eq!(t.edited_paths, vec!["a.rs", "b.rs"]);
}

#[test]
fn clean_records_do_not_count_as_soft_failures() {
    // soft_failures 只计软失败，成功调用不得推高计数。
    let mut t = EvidenceTracker::new(8, 2);
    t.record("Read", &args(&[("path", "a.rs")]), "", false, false, vec![]);
    t.record(
        "Bash",
        &args(&[("command", "echo ok")]),
        "",
        false,
        false,
        vec![],
    );
    assert_eq!(t.soft_failures, 0);
    assert_eq!(t.hard_failures, 0);
    t.record(
        "Bash",
        &args(&[("command", "x")]),
        "ToolError",
        false,
        true,
        vec![],
    );
    assert_eq!(t.soft_failures, 1);
    t.record(
        "Bash",
        &args(&[("command", "false")]),
        "ProcessFailed",
        true,
        true,
        vec![],
    );
    assert_eq!(t.hard_failures, 1);
    assert_eq!(t.soft_failures, 1);
}
