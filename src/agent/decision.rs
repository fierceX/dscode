//! 控制器 — 接收信号，做出决策。不关心信号怎么采集的。

use crate::guard::collector::{Signal, SignalKind};

/// 决策结果
#[derive(Debug, Clone, PartialEq)]
pub enum Decision {
    None,
    InjectHint(String),
    Abort,
}

/// 控制器 — 内部计数，基于阈值做出决策。
pub struct Controller {
    error_count: u32,
    slow_count: u32,
    repeated_count: u32,

    max_errors_before_abort: u32,
}

impl Controller {
    pub fn new() -> Self {
        Self {
            error_count: 0,
            slow_count: 0,
            repeated_count: 0,
            max_errors_before_abort: 5,
        }
    }

    pub fn reset(&mut self) {
        self.error_count = 0;
        self.slow_count = 0;
        self.repeated_count = 0;
    }

    pub fn feed(&mut self, signal: &Signal) {
        match signal.kind {
            SignalKind::ToolError => self.error_count += 1,
            SignalKind::SlowExecution => self.slow_count += 1,
            SignalKind::MassiveOutput => {}
            SignalKind::RepeatedCall => self.repeated_count += 1,
        }
    }

    pub fn decide(&self) -> Decision {
        if self.error_count >= self.max_errors_before_abort {
            return Decision::Abort;
        }
        if self.repeated_count >= 3 {
            return Decision::InjectHint("尝试不同的方法".into());
        }
        Decision::None
    }

    pub fn error_count(&self) -> u32 { self.error_count }

    pub fn snapshot(&self) -> serde_json::Value {
        serde_json::json!({
            "error_count": self.error_count,
            "slow_count": self.slow_count,
            "repeated_count": self.repeated_count,
            "decision": format!("{:?}", self.decide()),
        })
    }
}

impl Default for Controller {
    fn default() -> Self { Self::new() }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::guard::collector::SignalKind;

    fn sig(kind: SignalKind) -> Signal {
        Signal { kind, severity: 1.0, source: "test".into(), detail: "test".into() }
    }

    #[test]
    fn no_errors_no_decision() {
        let c = Controller::new();
        assert_eq!(c.decide(), Decision::None);
    }

    #[test]
    fn repeated_calls_inject_hint() {
        let mut c = Controller::new();
        for _ in 0..3 { c.feed(&sig(SignalKind::RepeatedCall)); }
        assert_eq!(c.decide(), Decision::InjectHint("尝试不同的方法".into()));
    }

    #[test]
    fn five_errors_abort() {
        let mut c = Controller::new();
        for _ in 0..5 { c.feed(&sig(SignalKind::ToolError)); }
        assert_eq!(c.decide(), Decision::Abort);
    }

    #[test]
    fn reset_clears_state() {
        let mut c = Controller::new();
        c.feed(&sig(SignalKind::ToolError));
        c.reset();
        assert_eq!(c.decide(), Decision::None);
    }
}
