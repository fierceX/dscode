//! 信念追踪器 — 纯计算。滑动窗口 + 拉普拉斯平滑。

use crate::guard::collector::{Signal, SignalKind};
use std::collections::VecDeque;

struct Observation {
    success_weight: f64,
    failure_weight: f64,
}

impl Observation {
    fn from_signals(signals: &[Signal]) -> Self {
        let mut total_failure = 0.0_f64;
        for s in signals {
            match s.kind {
                SignalKind::ToolError
                | SignalKind::EditLoop
                | SignalKind::ToolFailed
                | SignalKind::SafetyBlocked
                | SignalKind::ArgumentError
                | SignalKind::TestFailure
                | SignalKind::CompileError => total_failure = total_failure.max(s.severity),
            }
        }
        Observation {
            success_weight: 1.0 - total_failure,
            failure_weight: total_failure,
        }
    }
}

pub struct BeliefTracker {
    window: VecDeque<Observation>,
    window_size: usize,
    alpha_sum: f64,
    beta_sum: f64,
    pub recent_errors: Vec<String>,
}

impl BeliefTracker {
    pub fn new(window_size: usize) -> Self {
        Self {
            window: VecDeque::with_capacity(window_size),
            window_size,
            alpha_sum: 3.0,
            beta_sum: 1.0,
            recent_errors: Vec::new(),
        }
    }

    /// 喂入一次工具调用的所有信号，更新窗口 + α/β
    pub fn observe(&mut self, signals: &[Signal]) {
        let obs = Observation::from_signals(signals);

        if self.window.len() >= self.window_size
            && let Some(old) = self.window.pop_front()
        {
            self.alpha_sum -= old.success_weight;
            self.beta_sum -= old.failure_weight;
        }

        self.alpha_sum += obs.success_weight;
        self.beta_sum += obs.failure_weight;
        self.window.push_back(obs);

        for s in signals {
            if !matches!(s.kind, SignalKind::EditLoop) {
                self.recent_errors.push(s.message.clone());
            }
        }
    }

    /// 当前信念度 [0.0, 1.0]
    pub fn belief(&self) -> f64 {
        self.alpha_sum / (self.alpha_sum + self.beta_sum)
    }

    /// 新用户输入时清空窗口
    pub fn reset(&mut self) {
        self.window.clear();
        self.alpha_sum = 3.0;
        self.beta_sum = 1.0;
        self.recent_errors.clear();
    }
}

#[cfg(test)]
mod tests {
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
        assert!(bt.recent_errors.is_empty());
    }
}
