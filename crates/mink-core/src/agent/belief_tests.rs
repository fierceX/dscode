use super::*;
use crate::guard::collector::SignalKind;

fn sig(kind: SignalKind, severity: f64) -> Signal {
    Signal {
        kind,
        severity,
        source: "test".into(),
        detail: "test".into(),
        source_tool: "test".into(),
        exit_code: None,
        matched_pattern: None,
        message: "test".into(),
    }
}

#[test]
fn initial_belief_is_075() {
    let bt = BeliefTracker::new(4);
    assert!((bt.belief() - 0.75).abs() < 1e-10);
}

#[test]
fn success_increases_belief() {
    let mut bt = BeliefTracker::new(4);
    bt.observe(&[]); // clean call
    assert!(bt.belief() > 0.5);
}

#[test]
fn failure_decreases_belief() {
    let mut bt = BeliefTracker::new(4);
    bt.observe(&[sig(SignalKind::ToolError, 0.9)]);
    assert!(bt.belief() < 0.75);
}

#[test]
fn max_prevents_double_counting() {
    let mut bt = BeliefTracker::new(4);
    bt.observe(&[
        sig(SignalKind::ToolError, 0.9),
        sig(SignalKind::ToolError, 0.8),
    ]);
    assert!(bt.belief() < 0.75);
    // β should be 1 + 0.9 = 1.9, not 1 + 1.7 = 2.7
    assert!((bt.beta_sum - 1.9).abs() < 0.01);
}

#[test]
fn window_slides_old_errors_out() {
    let mut bt = BeliefTracker::new(2);
    bt.observe(&[sig(SignalKind::ToolError, 0.9)]); // failure
    let b1 = bt.belief();
    bt.observe(&[]); // success
    bt.observe(&[]); // success — old error slides out
    assert!(bt.belief() > b1);
}

#[test]
fn reset_clears_state() {
    let mut bt = BeliefTracker::new(4);
    bt.observe(&[sig(SignalKind::ToolError, 1.0)]);
    bt.reset();
    assert!((bt.belief() - 0.75).abs() < 1e-10);
}

#[test]
fn decay_pulls_belief_toward_prior() {
    let mut bt = BeliefTracker::new(4);
    bt.observe(&[sig(SignalKind::ToolError, 1.0)]);
    let failed = bt.belief();
    assert!(failed < 0.75);
    bt.decay(0.6);
    let decayed = bt.belief();
    assert!(decayed > failed, "decay must pull belief back toward prior");
    assert!(decayed < 0.75);
    // 完全衰减因子 1.0 保持不变，0.0 等价完全重置。
    let mut keep = BeliefTracker::new(4);
    keep.observe(&[sig(SignalKind::ToolError, 1.0)]);
    let before = keep.belief();
    keep.decay(1.0);
    assert!((keep.belief() - before).abs() < 1e-12);
    keep.decay(0.0);
    assert!((keep.belief() - 0.75).abs() < 1e-10);
}
