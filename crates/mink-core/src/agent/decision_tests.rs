use super::*;

#[test]
fn good_belief_does_nothing() {
    let mut de = DecisionEngine::from_config(&crate::config::SignalConfig::default());
    assert!(matches!(de.decide(0.9), Decision::None));
}

#[test]
fn warn_belief_injects() {
    let mut de = DecisionEngine::from_config(&crate::config::SignalConfig::default());
    let d = de.decide(0.4);
    assert!(matches!(d, Decision::Inject(_)));
}

#[test]
fn injected_message_triggers_signal_recovery_mode() {
    let mut de = DecisionEngine::from_config(&crate::config::SignalConfig::default());
    let d = de.decide(0.4);
    assert!(matches!(d, Decision::Inject(_)));
    if let Decision::Inject(directive) = d {
        assert_eq!(directive.severity, RecoverySeverity::Warning);
    }
}

#[test]
fn bad_belief_aborts() {
    let mut de = DecisionEngine::from_config(&crate::config::SignalConfig::default());
    let d = de.decide(0.2);
    assert!(matches!(d, Decision::Abort));
}

#[test]
fn cooldown_suppresses_inject() {
    let mut de = DecisionEngine::from_config(&crate::config::SignalConfig::default());
    // 第一次调用：注入，设置冷却
    assert!(matches!(de.decide(0.4), Decision::Inject(_)));
    // 第二次调用：冷却期内，应返回 None
    assert!(matches!(de.decide(0.4), Decision::None));
    assert_eq!(de.cooldown_remaining(), 2); // 3→递减→2
}

#[test]
fn cooldown_does_not_suppress_abort() {
    let mut de = DecisionEngine::from_config(&crate::config::SignalConfig::default());
    // 设置冷却
    de.cooldown_remaining = 5;
    // Abort 应绕过冷却
    let d = de.decide(0.2);
    assert!(matches!(d, Decision::Abort));
    // Abort 后冷却应清零
    assert_eq!(de.cooldown_remaining(), 0);
}

#[test]
fn cooldown_expires_after_enough_calls() {
    let mut de = DecisionEngine::from_config(&crate::config::SignalConfig::default());
    // 注入，冷却设为 3
    assert!(matches!(de.decide(0.4), Decision::Inject(_)));
    // 冷却期内：3→2→1→0，共 3 次 None
    assert!(matches!(de.decide(0.4), Decision::None));
    assert!(matches!(de.decide(0.4), Decision::None));
    assert!(matches!(de.decide(0.4), Decision::None));
    // 冷却结束，可再次注入
    assert!(matches!(de.decide(0.4), Decision::Inject(_)));
    // 再次进入冷却
    assert!(de.cooldown_remaining() == DEFAULT_COOLDOWN_TURNS);
}

#[test]
fn reset_clears_cooldown() {
    let mut de = DecisionEngine::from_config(&crate::config::SignalConfig::default());
    assert!(matches!(de.decide(0.4), Decision::Inject(_)));
    assert!(de.cooldown_remaining() > 0);
    de.reset();
    assert_eq!(de.cooldown_remaining(), 0);
}

#[test]
fn default_cooldown_turns_is_three() {
    assert_eq!(DEFAULT_COOLDOWN_TURNS, 3);
}

#[test]
fn warning_carries_only_belief_and_severity() {
    let mut de = DecisionEngine::from_config(&crate::config::SignalConfig::default());
    let d = de.decide(0.4);
    assert!(matches!(d, Decision::Inject(_)));
    if let Decision::Inject(directive) = d {
        assert_eq!(directive.severity, RecoverySeverity::Warning);
    }
}

#[test]
fn default_engine_starts_without_cooldown() {
    let de = DecisionEngine::from_config(&crate::config::SignalConfig::default());
    assert_eq!(de.cooldown_remaining(), 0);
}

#[test]
fn soft_only_signals_do_not_inject_above_warn_zone() {
    let mut de = DecisionEngine::from_config(&crate::config::SignalConfig::default());
    assert!(matches!(de.decide_with_signals(0.65, 0, 1), Decision::None));
    let mut de = DecisionEngine::from_config(&crate::config::SignalConfig::default());
    assert!(matches!(
        de.decide_with_signals(0.65, 0, 2),
        Decision::Inject(_)
    ));
    // 同信念但有硬信号：注入。
    let mut de = DecisionEngine::from_config(&crate::config::SignalConfig::default());
    assert!(matches!(
        de.decide_with_signals(0.65, 1, 0),
        Decision::Inject(_)
    ));
    // 警告区（0.4）即使仅软信号也注入。
    let mut de = DecisionEngine::from_config(&crate::config::SignalConfig::default());
    assert!(matches!(
        de.decide_with_signals(0.4, 0, 0),
        Decision::Inject(_)
    ));
}

#[test]
fn config_thresholds_are_honored() {
    let cfg = crate::config::SignalConfig {
        remind_threshold: 0.9,
        warn_threshold: 0.8,
        abort_threshold: 0.2,
        ..Default::default()
    };
    // 独立引擎逐项断言，避免冷却串扰。
    let mut above = DecisionEngine::from_config(&cfg);
    assert!(matches!(above.decide(0.95), Decision::None));
    let mut remind = DecisionEngine::from_config(&cfg);
    assert!(matches!(remind.decide(0.85), Decision::Inject(_)));
    let mut warn = DecisionEngine::from_config(&cfg);
    assert!(matches!(warn.decide(0.5), Decision::Inject(_)));
    let mut abort = DecisionEngine::from_config(&cfg);
    assert!(matches!(abort.decide(0.1), Decision::Abort));
}

#[test]
fn config_cooldown_is_honored() {
    let cfg = crate::config::SignalConfig {
        cooldown_turns: 1,
        ..Default::default()
    };
    let mut de = DecisionEngine::from_config(&cfg);
    assert!(matches!(de.decide(0.4), Decision::Inject(_)));
    // 冷却 1 轮：下一次 decide 立即允许注入。
    assert!(matches!(de.decide(0.4), Decision::None));
    assert!(matches!(de.decide(0.4), Decision::Inject(_)));
}
