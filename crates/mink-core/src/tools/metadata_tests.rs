use super::*;

#[test]
fn failure_adapter_covers_stable_failure_kinds() {
    let cases = [
        ("operation timed out", None, ToolFailureKind::Timeout),
        (
            "invalid tag: stale snapshot",
            None,
            ToolFailureKind::StaleTag,
        ),
        (
            "multiple matches found",
            None,
            ToolFailureKind::AmbiguousMatch,
        ),
        (
            "path is outside workspace",
            None,
            ToolFailureKind::PathOutOfScope,
        ),
        (
            "blocked by bash safety policy",
            None,
            ToolFailureKind::SafetyBlocked,
        ),
        ("invalid argument", None, ToolFailureKind::ArgumentInvalid),
        ("interrupted by user", None, ToolFailureKind::Aborted),
        (
            "unclassified internal failure",
            None,
            ToolFailureKind::Unknown,
        ),
        ("command failed", Some(2), ToolFailureKind::ProcessFailed),
    ];
    for (content, exit_code, expected) in cases {
        assert_eq!(classify_failure_kind(content, exit_code), expected);
    }
}

#[test]
fn status_exposes_failure_without_reading_display_text() {
    assert_eq!(
        ToolStatus::Failed(ToolFailureKind::ArgumentInvalid).failure_kind(),
        Some(ToolFailureKind::ArgumentInvalid)
    );
    assert_eq!(
        ToolStatus::Interrupted.failure_kind(),
        Some(ToolFailureKind::Aborted)
    );
    assert_eq!(ToolStatus::Succeeded.failure_kind(), None);
    assert_eq!(
        ToolStatus::Blocked(ToolBlocker::RecoveryGuard).failure_kind(),
        None
    );
}
