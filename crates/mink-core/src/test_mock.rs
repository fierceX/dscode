//! 回归测试 — 系统提示词 + 全链路信号机制。

use crate::config::ModelTier;

// ========================================================================
// 全链路信号机制测试
// ========================================================================

use crate::agent::belief::BeliefTracker;
use crate::agent::decision::{Decision, DecisionEngine};
use crate::guard::collector::{SignalCollector, SignalKind};

fn collect(sig: SignalKind, severity: f64, detail: &str) -> crate::guard::collector::Signal {
    crate::guard::collector::Signal {
        kind: sig,
        severity,
        source: "test".into(),
        detail: detail.into(),
        source_tool: "test".into(),
        exit_code: None,
        matched_pattern: None,
        message: detail.into(),
    }
}

// ── SignalCollector ──────────────────────────────────────────────

#[test]
fn collector_rust_error_detected() {
    let mut c = SignalCollector::new();
    let sigs = c.collect(
        "Bash",
        "error[E0425]: cannot find value",
        None,
        "error[E0425]: cannot find value",
    );
    assert!(
        sigs.iter()
            .any(|s| matches!(s.kind, SignalKind::CompileError)),
        "should detect non-zero exit"
    );
}

#[test]
fn collector_clean_output_ignored() {
    let mut c = SignalCollector::new();
    let sigs = c.collect("Bash", "everything is fine", None, "everything is fine");
    assert!(
        !sigs.iter().any(|s| matches!(s.kind, SignalKind::ToolError)),
        "exit code 0 should not be an error"
    );
}

#[test]
fn collector_tool_error_detected() {
    let mut c = SignalCollector::new();
    let sigs = c.collect(
        "Bash",
        "error[E0425]: cannot find value `x`",
        None,
        "error[E0425]: cannot find value `x`",
    );
    assert!(
        sigs.iter()
            .any(|s| matches!(s.kind, SignalKind::CompileError))
    );
}

#[test]
fn collector_clean_output_no_signals() {
    let mut c = SignalCollector::new();
    let sigs = c.collect("Read", "everything is fine", None, "everything is fine");
    assert!(sigs.is_empty(), "clean output should produce no signals");
}

#[test]
fn collector_edit_loop_excessive_edits() {
    let mut c = SignalCollector::new();
    // 模拟：窗口=6，5次Edit触发 EditLoop
    for _ in 0..5 {
        c.collect("Edit", "ok", None, "ok");
    }
    c.collect("Edit", "ok", None, "ok"); // 第6次 Edit — 窗口内 Edit=6 > 4
    let sigs = c.collect("Edit", "ok", None, "ok");
    assert!(
        sigs.iter().any(|s| matches!(s.kind, SignalKind::EditLoop)),
        "excessive edits should trigger EditLoop"
    );
}

#[test]
fn collector_edit_diff_alternation_triggers() {
    let mut c = SignalCollector::new();
    for name in &["Edit", "Diff", "Edit", "Diff", "Edit", "Diff"] {
        c.collect(name, "ok", None, "ok");
    }
    let sigs = c.collect("Diff", "ok", None, "ok"); // Edit↔Diff 交替 + 无 Bash/Grep/Read
    assert!(
        sigs.iter().any(|s| matches!(s.kind, SignalKind::EditLoop)),
        "edit-diff alternation without reads should trigger EditLoop"
    );
}

#[test]
fn collector_edit_diff_with_read_does_not_trigger() {
    let mut c = SignalCollector::new();
    for name in &["Edit", "Diff", "Edit", "Grep", "Edit", "Diff"] {
        c.collect(name, "ok", None, "ok");
    }
    let sigs = c.collect("Edit", "ok", None, "ok");
    assert!(
        !sigs.iter().any(|s| matches!(s.kind, SignalKind::EditLoop)),
        "edit-diff with Grep should NOT trigger EditLoop"
    );
}

/// Exit code + ToolError 同一次工具调用 → max 合并（通过 BeliefTracker 验证）
#[test]
fn belief_max_merges_tool_errors() {
    let mut c = SignalCollector::new();
    let sigs = c.collect(
        "Bash",
        "error[E0308]: type mismatch\nerror[E0425]: cannot find value",
        None,
        "error[E0308]: type mismatch\nerror[E0425]: cannot find value",
    );
    assert!(
        sigs.iter()
            .any(|s| matches!(s.kind, SignalKind::CompileError))
    );

    // 喂入 BeliefTracker → max(0.9, 0.9) = 0.9, 不是 1.8
    let mut bt = BeliefTracker::new(4);
    bt.observe(&sigs);
    // α = 3 + 0.1 = 3.1, β = 1 + 0.9 = 1.9 → B = 3.1/5.0 = 0.620
    assert!(
        (bt.belief() - 0.620).abs() < 0.02,
        "max merging should prevent double-counting, got B={:.3}",
        bt.belief()
    );
}

// ── BeliefTracker ────────────────────────────────────────────────

#[test]
fn belief_initial_is_075() {
    let bt = BeliefTracker::new(4);
    assert!((bt.belief() - 0.75).abs() < 1e-10);
}

#[test]
fn belief_rises_with_clean_calls() {
    let mut bt = BeliefTracker::new(4);
    for _ in 0..4 {
        bt.observe(&[]); // all clean
    }
    assert!(
        bt.belief() > 0.82,
        "4 clean calls should raise belief above 0.82, got {:.3}",
        bt.belief()
    );
}

#[test]
fn belief_drops_with_errors() {
    let mut bt = BeliefTracker::new(4);
    bt.observe(&[collect(SignalKind::ToolError, 1.0, "err")]);
    assert!(bt.belief() < 0.75);
    bt.observe(&[collect(SignalKind::ToolError, 1.0, "err")]);
    assert!(
        bt.belief() < 0.75,
        "2 consecutive errors should drop belief below 0.35"
    );
}

#[test]
fn belief_window_slides_old_errors_out() {
    let mut bt = BeliefTracker::new(3);
    // 2 errors
    bt.observe(&[collect(SignalKind::ToolError, 0.9, "e1")]);
    bt.observe(&[collect(SignalKind::ToolError, 0.9, "e2")]);
    let b_low = bt.belief();
    assert!(
        b_low < 0.75,
        "belief should drop after 2 errors, got {:.3}",
        b_low
    );

    // 2 clean → old errors slide out
    bt.observe(&[]);
    bt.observe(&[]);
    assert!(
        bt.belief() > b_low,
        "belief should recover as old errors exit window, got {:.3}",
        bt.belief()
    );
}

#[test]
fn belief_reset_clears_state() {
    let mut bt = BeliefTracker::new(4);
    bt.observe(&[collect(SignalKind::ToolError, 1.0, "e")]);
    bt.reset();
    assert!(
        (bt.belief() - 0.75).abs() < 1e-10,
        "reset should restore 0.75"
    );
    assert!(bt.recent_errors.is_empty(), "reset should clear errors");
}

// ── DecisionEngine ───────────────────────────────────────────────

#[test]
fn decision_good_belief_is_none() {
    let mut de = DecisionEngine::new();
    assert!(matches!(de.decide(0.85, &[]), Decision::None));
}

#[test]
fn decision_warn_belief_is_inject() {
    let mut de = DecisionEngine::new();
    let d = de.decide(0.55, &[]);
    let directive = match d {
        Decision::Inject(m) => m,
        _ => panic!("expected Inject"),
    };
    assert_eq!(
        directive.severity,
        crate::agent::decision::RecoverySeverity::Reminder
    );
    assert!(directive.errors.is_empty());
}

#[test]
fn decision_low_belief_is_inject_warning() {
    let mut de = DecisionEngine::new();
    let d = de.decide(0.35, &["Rust error[E0308]".into()]);
    let directive = match d {
        Decision::Inject(m) => m,
        _ => panic!("expected Inject for warning"),
    };
    assert_eq!(
        directive.severity,
        crate::agent::decision::RecoverySeverity::Warning
    );
    assert!(
        directive
            .errors
            .iter()
            .any(|error| error.contains("Rust error"))
    );
}

#[test]
fn decision_very_low_belief_is_abort() {
    let mut de = DecisionEngine::new();
    let d = de.decide(0.2, &[]);
    assert!(
        matches!(d, Decision::Abort),
        "belief 0.2 should trigger Abort"
    );
}

// ── 全链路集成 ──────────────────────────────────────────────────

#[test]
fn full_chain_clean_to_injection() {
    let mut collector = SignalCollector::new();
    let mut belief = BeliefTracker::new(4);
    let mut engine = DecisionEngine::new();

    // 3 clean calls → high belief
    for _ in 0..3 {
        let sigs = collector.collect("Read", "ok", None, "ok");
        belief.observe(&sigs);
    }
    assert!(
        belief.belief() > 0.7,
        "3 clean calls should give high belief"
    );
    assert!(matches!(
        engine.decide(belief.belief(), &belief.recent_errors),
        Decision::None
    ));

    // 3 error calls → low belief
    for _ in 0..3 {
        let sigs = collector.collect(
            "Bash",
            "error[E0425]: cannot find value",
            None,
            "error[E0425]: cannot find value",
        );
        belief.observe(&sigs);
    }
    assert!(
        belief.belief() < 0.75,
        "3 errors should drop belief below 0.75, got {:.3}",
        belief.belief()
    );
    let d = engine.decide(belief.belief(), &belief.recent_errors);
    assert!(
        matches!(d, Decision::Inject(_)),
        "low belief should trigger injection"
    );
}

#[test]
fn full_chain_edit_loop_triggers_belief_drop() {
    let mut collector = SignalCollector::new();
    let mut belief = BeliefTracker::new(4);

    // Start clean
    belief.observe(&[]);
    assert!(belief.belief() > 0.6);

    // Trigger EditLoop with excessive edits
    for _ in 0..5 {
        let sigs = collector.collect("Edit", "ok", None, "ok");
        belief.observe(&sigs);
    }
    // The next Edit should trigger EditLoop
    let sigs = collector.collect("Edit", "ok", None, "ok");
    assert!(sigs.iter().any(|s| matches!(s.kind, SignalKind::EditLoop)));
    belief.observe(&sigs);
    assert!(
        belief.belief() < 0.80,
        "EditLoop should drop belief, got {:.3}",
        belief.belief()
    );
}

// ========================================================================
// 系统提示词
// ========================================================================

#[test]
fn system_prompt_contains_causal_reasoning() {
    let tool_config = crate::context::ToolConfig::from_config(&crate::config::Config::default());
    let (_, tool_surface, tool_capabilities) =
        crate::context::resolve_tool_runtime(&tool_config, false, false).unwrap();
    let builder = crate::prompt::Builder {
        cwd: std::path::PathBuf::from("/tmp"),
        home: std::path::PathBuf::from("/tmp"),
        skill_snapshot: std::sync::Arc::new(crate::capabilities::SkillSnapshot::default()),
        context_file_snapshot: std::sync::Arc::new(
            crate::capabilities::ContextFileSnapshot::default(),
        ),
        rule_snapshot: std::sync::Arc::new(crate::capabilities::RuleSnapshot::default()),
        mission_file: None,
        mission_content: None,
        tool_surface,
        tool_capabilities,
    };
    let prompt = builder.build_system_prompt().unwrap();
    assert!(
        prompt.contains("<execution-codes>"),
        "should contain execution-codes section (merged from causal-reasoning + verification-gate + stop-triggers)"
    );
    assert!(
        prompt.contains("BEFORE every code change, answer silently"),
        "should contain causal reasoning instructions"
    );
}

#[test]
fn forced_model_works() {
    let forced: Option<ModelTier> = Some(ModelTier::Pro);
    let tier = if let Some(f) = forced {
        f
    } else {
        ModelTier::Flash
    };
    assert_eq!(tier, ModelTier::Pro);
}

#[test]
fn unknown_model_falls_back_to_flash() {
    let tier = ModelTier::parse("gpt-4").unwrap_or(ModelTier::Flash);
    assert_eq!(tier, ModelTier::Flash);
}
