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
    /// 构造时配置的先验，reset/decay 时回拉到该值。
    alpha_prior: f64,
    beta_prior: f64,
}

impl BeliefTracker {
    #[cfg(test)]
    pub fn new(window_size: usize) -> Self {
        Self::new_with_priors(window_size, 3.0, 1.0)
    }

    pub fn new_with_priors(window_size: usize, alpha_prior: f64, beta_prior: f64) -> Self {
        Self {
            window: VecDeque::with_capacity(window_size),
            window_size,
            alpha_sum: alpha_prior,
            beta_sum: beta_prior,
            alpha_prior,
            beta_prior,
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
    }

    /// 当前信念度 [0.0, 1.0]
    pub fn belief(&self) -> f64 {
        self.alpha_sum / (self.alpha_sum + self.beta_sum)
    }

    /// 新用户输入时清空窗口
    pub fn reset(&mut self) {
        self.window.clear();
        self.alpha_sum = self.alpha_prior;
        self.beta_sum = self.beta_prior;
    }

    /// 回拉 factor 比例，替代硬重置——跨轮重复失败可累积升级，单次偶然失败
    /// 自然消退。factor = 0 等价于完全重置，factor = 1 不衰减。
    pub fn decay(&mut self, factor: f64) {
        let (alpha_prior, beta_prior) = (self.alpha_prior, self.beta_prior);
        let (a, b) = (self.alpha_sum, self.beta_sum);
        self.alpha_sum = a * factor + alpha_prior * (1.0 - factor);
        self.beta_sum = b * factor + beta_prior * (1.0 - factor);
        self.window.clear();
    }
}

#[cfg(test)]
#[path = "belief_tests.rs"]
mod tests;
