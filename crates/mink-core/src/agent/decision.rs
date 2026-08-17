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

#[cfg(test)]
#[path = "decision_tests.rs"]
mod tests;
