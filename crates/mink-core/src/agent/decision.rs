//! 决策引擎 — 纯决策。根据信念度 + 错误列表输出 Decision。
//!
//! 冷却机制由引擎内部管理：每次 Inject 后设置冷却计数器，
//! 后续调用自动跳过注入直到冷却期结束。Abort 始终绕过冷却。

/// 默认冷却轮数：注入后跳过多少次 `decide()` 调用再允许下一次注入。
pub const DEFAULT_COOLDOWN_TURNS: usize = 3;

pub enum Decision {
    None,
    Inject(RecoveryDirective),
    Abort,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecoverySeverity {
    Reminder,
    Warning,
}

#[derive(Debug, Clone)]
pub struct RecoveryDirective {
    pub belief: f64,
    pub severity: RecoverySeverity,
    pub errors: Vec<String>,
}

pub struct DecisionEngine {
    abort_threshold: f64,
    warn_threshold: f64,
    remind_threshold: f64,
    /// 剩余冷却轮数，由引擎内部管理。
    cooldown_remaining: usize,
}

impl DecisionEngine {
    pub fn new() -> Self {
        Self {
            abort_threshold: 0.30,
            warn_threshold: 0.50,
            remind_threshold: 0.70,
            cooldown_remaining: 0,
        }
    }

    /// 决策入口。
    ///
    /// - `belief` — 当前信念度 [0.0, 1.0]
    /// - `errors` — 最近的错误列表（用于注入内容）
    ///
    /// 冷却逻辑：引擎内部维护 `cooldown_remaining` 计数器。
    /// 每次调用递减，>0 时跳过注入（但保留 Abort）。
    /// Inject 发生后重置计数器。
    pub fn decide(&mut self, belief: f64, errors: &[String]) -> Decision {
        // Abort 是安全机制，始终绕过冷却
        if belief < self.abort_threshold {
            self.cooldown_remaining = 0; // 中止后清除冷却
            return Decision::Abort;
        }

        // 冷却期内跳过注入
        if self.cooldown_remaining > 0 {
            self.cooldown_remaining -= 1;
            return Decision::None;
        }

        if belief < self.warn_threshold {
            self.cooldown_remaining = DEFAULT_COOLDOWN_TURNS;
            return Decision::Inject(RecoveryDirective {
                belief,
                severity: RecoverySeverity::Warning,
                errors: errors.iter().rev().take(5).cloned().collect(),
            });
        }
        if belief < self.remind_threshold {
            self.cooldown_remaining = DEFAULT_COOLDOWN_TURNS;
            return Decision::Inject(RecoveryDirective {
                belief,
                severity: RecoverySeverity::Reminder,
                errors: errors.iter().rev().take(3).cloned().collect(),
            });
        }
        Decision::None
    }

    /// 重置冷却状态（新用户输入时调用）。
    pub fn reset(&mut self) {
        self.cooldown_remaining = 0;
    }

    /// 当前剩余冷却轮数（用于调试和显示）。
    pub fn cooldown_remaining(&self) -> usize {
        self.cooldown_remaining
    }
}

impl Default for DecisionEngine {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn good_belief_does_nothing() {
        let mut de = DecisionEngine::new();
        assert!(matches!(de.decide(0.9, &[]), Decision::None));
    }

    #[test]
    fn warn_belief_injects() {
        let mut de = DecisionEngine::new();
        let d = de.decide(0.4, &["Rust error".into()]);
        assert!(matches!(d, Decision::Inject(_)));
    }

    #[test]
    fn injected_message_triggers_signal_recovery_mode() {
        let mut de = DecisionEngine::new();
        let d = de.decide(0.4, &["Rust error".into()]);
        assert!(matches!(d, Decision::Inject(_)));
        if let Decision::Inject(directive) = d {
            assert_eq!(directive.severity, RecoverySeverity::Warning);
            assert_eq!(directive.errors, vec!["Rust error"]);
            assert_eq!(directive.belief, 0.4);
        }
    }

    #[test]
    fn bad_belief_aborts() {
        let mut de = DecisionEngine::new();
        let d = de.decide(0.2, &["error".into()]);
        assert!(matches!(d, Decision::Abort));
    }

    #[test]
    fn cooldown_suppresses_inject() {
        let mut de = DecisionEngine::new();
        // 第一次调用：注入，设置冷却
        assert!(matches!(
            de.decide(0.4, &["err".into()]),
            Decision::Inject(_)
        ));
        // 第二次调用：冷却期内，应返回 None
        assert!(matches!(de.decide(0.4, &["err".into()]), Decision::None));
        assert_eq!(de.cooldown_remaining(), 2); // 3→递减→2
    }

    #[test]
    fn cooldown_does_not_suppress_abort() {
        let mut de = DecisionEngine::new();
        // 设置冷却
        de.cooldown_remaining = 5;
        // Abort 应绕过冷却
        let d = de.decide(0.2, &["error".into()]);
        assert!(matches!(d, Decision::Abort));
        // Abort 后冷却应清零
        assert_eq!(de.cooldown_remaining(), 0);
    }

    #[test]
    fn cooldown_expires_after_enough_calls() {
        let mut de = DecisionEngine::new();
        // 注入，冷却设为 3
        assert!(matches!(
            de.decide(0.4, &["err".into()]),
            Decision::Inject(_)
        ));
        // 冷却期内：3→2→1→0，共 3 次 None
        assert!(matches!(de.decide(0.4, &["err".into()]), Decision::None));
        assert!(matches!(de.decide(0.4, &["err".into()]), Decision::None));
        assert!(matches!(de.decide(0.4, &["err".into()]), Decision::None));
        // 冷却结束，可再次注入
        assert!(matches!(
            de.decide(0.4, &["err".into()]),
            Decision::Inject(_)
        ));
        // 再次进入冷却
        assert!(de.cooldown_remaining() == DEFAULT_COOLDOWN_TURNS);
    }

    #[test]
    fn reset_clears_cooldown() {
        let mut de = DecisionEngine::new();
        assert!(matches!(
            de.decide(0.4, &["err".into()]),
            Decision::Inject(_)
        ));
        assert!(de.cooldown_remaining() > 0);
        de.reset();
        assert_eq!(de.cooldown_remaining(), 0);
    }

    #[test]
    fn default_cooldown_turns_is_three() {
        assert_eq!(DEFAULT_COOLDOWN_TURNS, 3);
    }

    #[test]
    fn warning_without_errors_omits_error_section() {
        let mut de = DecisionEngine::new();
        let d = de.decide(0.4, &[]);
        assert!(matches!(d, Decision::Inject(_)));
        if let Decision::Inject(directive) = d {
            assert!(directive.errors.is_empty());
        }
    }

    #[test]
    fn default_engine_starts_without_cooldown() {
        let de = DecisionEngine::default();
        assert_eq!(de.cooldown_remaining(), 0);
    }
}
