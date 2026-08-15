//! 决策引擎 — 纯决策。根据信念度 + 错误列表输出 Decision。
//!
//! 冷却机制由引擎内部管理：每次 Inject 后设置冷却计数器，
//! 后续调用自动跳过注入直到冷却期结束。Abort 始终绕过冷却。

/// 默认冷却轮数：注入后跳过多少次 `decide()` 调用再允许下一次注入。
#[cfg(test)]
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
    pub severity: RecoverySeverity,
}

pub struct DecisionEngine {
    abort_threshold: f64,
    warn_threshold: f64,
    remind_threshold: f64,
    cooldown_turns: usize,
    /// 剩余冷却轮数，由引擎内部管理。
    cooldown_remaining: usize,
}

impl DecisionEngine {
    pub fn new() -> Self {
        Self::from_config(&crate::config::SignalConfig::default())
    }

    /// 全参数构造（阈值与冷却来自 SignalConfig）。
    pub(crate) fn from_config(s: &crate::config::SignalConfig) -> Self {
        Self {
            abort_threshold: s.abort_threshold,
            warn_threshold: s.warn_threshold,
            remind_threshold: s.remind_threshold,
            cooldown_turns: s.cooldown_turns,
            cooldown_remaining: 0,
        }
    }

    /// 决策入口（兼容旧签名：假定存在硬信号且无软失败累计）。
    #[cfg(test)]
    pub fn decide(&mut self, belief: f64) -> Decision {
        self.decide_with_signals(belief, 1, 0)
    }

    ///
    /// - `hard_signals` — 本批工具调用中硬信号的数量（ToolFailed/SafetyBlocked/
    ///   CompileError/TestFailure）。
    /// - `soft_failures` — 本用户输入内累计软失败次数（regex 嗅探类）。单次软失败
    ///   且信念仍在提醒区上方时不触发注入（记录不干预）；累计 >= 2 次软失败说明
    ///
    /// 冷却逻辑：引擎内部维护 `cooldown_remaining` 计数器。
    /// 每次调用递减，>0 时跳过注入（但保留 Abort）。
    /// Inject 发生后重置计数器。
    pub fn decide_with_signals(
        &mut self,
        belief: f64,
        hard_signals: usize,
        soft_failures: usize,
    ) -> Decision {
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

        // 仅单次软失败：提醒区之上不响应（记录但不干预）。
        if hard_signals == 0 && soft_failures <= 1 && belief >= self.warn_threshold {
            return Decision::None;
        }

        if belief < self.warn_threshold {
            self.cooldown_remaining = self.cooldown_turns;
            return Decision::Inject(RecoveryDirective {
                severity: RecoverySeverity::Warning,
            });
        }
        if belief < self.remind_threshold {
            self.cooldown_remaining = self.cooldown_turns;
            return Decision::Inject(RecoveryDirective {
                severity: RecoverySeverity::Reminder,
            });
        }
        Decision::None
    }

    /// 重置冷却状态（新用户输入时调用）。
    pub fn reset(&mut self) {
        self.cooldown_remaining = 0;
    }

    /// 当前剩余冷却轮数（用于调试和显示）。
    #[cfg(test)]
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
        assert!(matches!(de.decide(0.9), Decision::None));
    }

    #[test]
    fn warn_belief_injects() {
        let mut de = DecisionEngine::new();
        let d = de.decide(0.4);
        assert!(matches!(d, Decision::Inject(_)));
    }

    #[test]
    fn injected_message_triggers_signal_recovery_mode() {
        let mut de = DecisionEngine::new();
        let d = de.decide(0.4);
        assert!(matches!(d, Decision::Inject(_)));
        if let Decision::Inject(directive) = d {
            assert_eq!(directive.severity, RecoverySeverity::Warning);
        }
    }

    #[test]
    fn bad_belief_aborts() {
        let mut de = DecisionEngine::new();
        let d = de.decide(0.2);
        assert!(matches!(d, Decision::Abort));
    }

    #[test]
    fn cooldown_suppresses_inject() {
        let mut de = DecisionEngine::new();
        // 第一次调用：注入，设置冷却
        assert!(matches!(de.decide(0.4), Decision::Inject(_)));
        // 第二次调用：冷却期内，应返回 None
        assert!(matches!(de.decide(0.4), Decision::None));
        assert_eq!(de.cooldown_remaining(), 2); // 3→递减→2
    }

    #[test]
    fn cooldown_does_not_suppress_abort() {
        let mut de = DecisionEngine::new();
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
        let mut de = DecisionEngine::new();
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
        let mut de = DecisionEngine::new();
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
        let mut de = DecisionEngine::new();
        let d = de.decide(0.4);
        assert!(matches!(d, Decision::Inject(_)));
        if let Decision::Inject(directive) = d {
            assert_eq!(directive.severity, RecoverySeverity::Warning);
        }
    }

    #[test]
    fn default_engine_starts_without_cooldown() {
        let de = DecisionEngine::default();
        assert_eq!(de.cooldown_remaining(), 0);
    }

    #[test]
    fn soft_only_signals_do_not_inject_above_warn_zone() {
        let mut de = DecisionEngine::new();
        assert!(matches!(de.decide_with_signals(0.65, 0, 1), Decision::None));
        let mut de = DecisionEngine::new();
        assert!(matches!(
            de.decide_with_signals(0.65, 0, 2),
            Decision::Inject(_)
        ));
        // 同信念但有硬信号：注入。
        let mut de = DecisionEngine::new();
        assert!(matches!(
            de.decide_with_signals(0.65, 1, 0),
            Decision::Inject(_)
        ));
        // 警告区（0.4）即使仅软信号也注入。
        let mut de = DecisionEngine::new();
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
}
