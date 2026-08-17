use super::*;

#[test]
fn allows_if_below_threshold() {
    let mut sb = StormBreaker::new(6, 3);
    assert!(matches!(
        sb.check("Read", r#"{"path":"/tmp/x"}"#),
        StormDecision::Allow
    ));
    assert!(matches!(
        sb.check("Read", r#"{"path":"/tmp/x"}"#),
        StormDecision::Allow
    ));
}

#[test]
fn suppresses_after_threshold() {
    // The first `threshold` identical calls are allowed so graded tool
    // feedback (Edit soft no-op escalation) can complete; suppression
    // starts from the threshold+1-th identical call.
    let mut sb = StormBreaker::new(6, 3);
    for i in 0..3 {
        assert!(
            matches!(
                sb.check("Bash", r#"{"command":"ls"}"#),
                StormDecision::Allow
            ),
            "call {} should be allowed",
            i + 1
        );
    }
    assert!(matches!(
        sb.check("Bash", r#"{"command":"ls"}"#),
        StormDecision::Suppress(_)
    ));
}

#[test]
fn mutating_calls_share_window_and_suppress() {
    // Repeated identical mutating calls are exactly the edit-loop storm the
    // breaker exists to stop; they must count toward the threshold instead
    // of being permanently immunized by a window clear.
    let mut sb = StormBreaker::new(6, 3);
    for _ in 0..3 {
        assert!(matches!(
            sb.check("Write", r#"{"path":"/x","content":"hi"}"#),
            StormDecision::Allow
        ));
    }
    assert!(matches!(
        sb.check("Write", r#"{"path":"/x","content":"hi"}"#),
        StormDecision::Suppress(_)
    ));
}

#[test]
fn mutating_call_does_not_clear_other_counts() {
    let mut sb = StormBreaker::new(6, 3);
    sb.check("Read", r#"{"path":"/x"}"#);
    sb.check("Read", r#"{"path":"/x"}"#);
    sb.check("Write", r#"{"path":"/x","content":"hi"}"#);
    assert!(matches!(
        sb.check("Read", r#"{"path":"/x"}"#),
        StormDecision::Allow
    ));
    assert!(matches!(
        sb.check("Read", r#"{"path":"/x"}"#),
        StormDecision::Suppress(_)
    ));
}

#[test]
fn window_slides_old_entries_out() {
    let mut sb = StormBreaker::new(3, 3);
    sb.check("Bash", "a");
    sb.check("Bash", "b");
    sb.check("Bash", "c");
    // "a" should be evicted, so only 2 of "Bash/a"
    let d = sb.check("Bash", "a");
    assert!(matches!(d, StormDecision::Allow));
}

#[test]
fn reset_clears_window() {
    let mut sb = StormBreaker::new(3, 3);
    sb.check("Read", "a");
    sb.check("Read", "a");
    sb.reset();
    let d = sb.check("Read", "a");
    assert!(matches!(d, StormDecision::Allow));
}
