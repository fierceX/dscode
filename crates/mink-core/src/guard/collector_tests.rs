use super::*;

#[test]
fn detects_rust_error() {
    let mut c = SignalCollector::new();
    let sigs = c.collect(
        "Bash",
        ToolStatus::Succeeded,
        "error[E0425]: cannot find value",
        None,
        true,
    );
    assert!(
        sigs.iter()
            .any(|s| matches!(s.kind, SignalKind::CompileError))
    );
    assert!(sigs.iter().any(|s| s.matched_pattern.is_some()));
}

#[test]
fn command_test_failure_is_a_diagnostic_signal() {
    let mut collector = SignalCollector::new();
    let signals = collector.collect(
        "Bash",
        ToolStatus::Failed(ToolFailureKind::ProcessFailed),
        "FAILED tests/runtime.rs::recovers_after_failure",
        Some(1),
        true,
    );
    assert!(
        signals
            .iter()
            .any(|signal| matches!(signal.kind, SignalKind::TestFailure))
    );
}

#[test]
fn clean_output_no_signals() {
    let mut c = SignalCollector::new();
    let sigs = c.collect(
        "Read",
        ToolStatus::Succeeded,
        "everything is fine",
        None,
        false,
    );
    assert!(sigs.is_empty());
}

#[test]
fn content_tool_output_with_timeout_keyword_does_not_emit_pattern_signal() {
    let mut c = SignalCollector::new();
    let sigs = c.collect(
        "Read",
        ToolStatus::Succeeded,
        "209:        let _ = tokio::time::timeout(std::time::Duration::from_secs(5), self)",
        None,
        false,
    );
    assert!(
        !sigs.iter().any(|s| matches!(s.kind, SignalKind::ToolError)),
        "Read output is file content; 'timeout' must not produce a ToolError signal"
    );
}

#[test]
fn command_tool_output_with_timeout_keyword_emits_pattern_signal() {
    let mut c = SignalCollector::new();
    let sigs = c.collect(
        "Bash",
        ToolStatus::Succeeded,
        "Timed out after 60s",
        None,
        true,
    );
    assert!(
        sigs.iter().any(|s| matches!(s.kind, SignalKind::ToolError)),
        "Bash diagnostics containing 'Timed out' must produce a ToolError signal"
    );
}

#[test]
fn content_tool_output_with_compile_error_keyword_does_not_emit_pattern_signal() {
    let mut c = SignalCollector::new();
    let sigs = c.collect(
        "Read",
        ToolStatus::Succeeded,
        "\"error[E0425]: cannot find value\" // test fixture string",
        None,
        false,
    );
    assert!(
        !sigs
            .iter()
            .any(|s| matches!(s.kind, SignalKind::CompileError)),
        "Read output is file content; 'error[E0425]' must not produce a CompileError signal"
    );
}

#[test]
fn detects_edit_loop_excessive_edits() {
    let mut c = SignalCollector::new();
    for _ in 0..5 {
        c.call_history.push_back("Edit".into());
    }
    c.call_history.push_back("Read".into());
    let sigs = c.collect("Edit", ToolStatus::Succeeded, "ok", None, false);
    assert!(sigs.iter().any(|s| matches!(s.kind, SignalKind::EditLoop)));
}

#[test]
fn detects_tool_failed_via_exit_code() {
    let mut c = SignalCollector::new();
    let sigs = c.collect(
        "Bash",
        ToolStatus::Failed(ToolFailureKind::ProcessFailed),
        "output",
        Some(1),
        false,
    );
    assert!(
        sigs.iter()
            .any(|s| matches!(s.kind, SignalKind::ToolFailed))
    );
}

#[test]
fn display_error_prefix_does_not_define_status() {
    let mut c = SignalCollector::new();
    let sigs = c.collect(
        "Read",
        ToolStatus::Succeeded,
        "Error: file not found",
        None,
        false,
    );
    assert!(sigs.is_empty());
}

#[test]
fn detects_safety_blocked() {
    let mut c = SignalCollector::new();
    let sigs = c.collect(
        "Bash",
        ToolStatus::Failed(ToolFailureKind::SafetyBlocked),
        "command blocked by bash safety policy (sudo)",
        None,
        false,
    );
    assert!(
        sigs.iter()
            .any(|s| matches!(s.kind, SignalKind::SafetyBlocked))
    );
}

#[test]
fn recovery_guard_preserves_non_extreme_failure_weight() {
    let mut collector = SignalCollector::new();
    let signals = collector.collect(
        "Edit",
        ToolStatus::Blocked(ToolBlocker::RecoveryGuard),
        "recovery guard blocked Edit",
        None,
        false,
    );

    assert_eq!(signals.len(), 1);
    assert_eq!(signals[0].severity, 0.9);
    assert!(matches!(signals[0].kind, SignalKind::ToolFailed));
}

#[test]
fn exit_code_zero_does_not_fail() {
    let mut c = SignalCollector::new();
    let sigs = c.collect("Bash", ToolStatus::Succeeded, "ok", Some(0), false);
    assert!(
        !sigs
            .iter()
            .any(|s| matches!(s.kind, SignalKind::ToolFailed))
    );
}
