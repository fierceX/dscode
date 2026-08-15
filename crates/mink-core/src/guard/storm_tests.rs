use super::*;

#[test]
fn allows_if_below_threshold() {
    let mut sb = StormBreaker::new(6, 3);
    assert!(matches!(
        sb.check("Read", r#"{"path":"/tmp/x"}"#, false),
        StormDecision::Allow
    ));
    assert!(matches!(
        sb.check("Read", r#"{"path":"/tmp/x"}"#, false),
        StormDecision::Allow
    ));
}

#[test]
fn suppresses_at_threshold() {
    let mut sb = StormBreaker::new(6, 3);
    for _ in 0..3 {
        let d = sb.check("Bash", r#"{"command":"ls"}"#, false);
        if matches!(d, StormDecision::Suppress(_)) {
            return;
        }
    }
    panic!("should have suppressed");
}

#[test]
fn mutating_call_clears_window() {
    let mut sb = StormBreaker::new(6, 3);
    sb.check("Read", r#"{"path":"/x"}"#, false);
    sb.check("Read", r#"{"path":"/x"}"#, false);
    sb.check("Write", r#"{"path":"/x","content":"hi"}"#, true);
    // After mutating, window is cleared
    let d = sb.check("Read", r#"{"path":"/x"}"#, false);
    assert!(matches!(d, StormDecision::Allow));
}

#[test]
fn window_slides_old_entries_out() {
    let mut sb = StormBreaker::new(3, 3);
    sb.check("Bash", "a", false);
    sb.check("Bash", "b", false);
    sb.check("Bash", "c", false);
    // "a" should be evicted, so only 2 of "Bash/a"
    let d = sb.check("Bash", "a", false);
    assert!(matches!(d, StormDecision::Allow));
}

#[test]
fn reset_clears_window() {
    let mut sb = StormBreaker::new(3, 3);
    sb.check("Read", "a", false);
    sb.check("Read", "a", false);
    sb.reset();
    let d = sb.check("Read", "a", false);
    assert!(matches!(d, StormDecision::Allow));
}
