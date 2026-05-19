//! 决策引擎 — 纯决策。根据信念度 + 错误列表输出 Decision。

pub enum Decision {
    None,
    Inject(String),
    Abort,
}

pub struct DecisionEngine {
    abort_threshold: f64,
    warn_threshold: f64,
    remind_threshold: f64,
    consecutive_abort_required: u32,
    consecutive_abort_count: u32,
}

impl DecisionEngine {
    pub fn new() -> Self {
        Self {
            abort_threshold: 0.30,
            warn_threshold: 0.50,
            remind_threshold: 0.70,
            consecutive_abort_required: 1,
            consecutive_abort_count: 0,
        }
    }

    pub fn decide(&mut self, belief: f64, errors: &[String]) -> Decision {
        if belief < self.abort_threshold {
            self.consecutive_abort_count += 1;
            if self.consecutive_abort_count >= self.consecutive_abort_required {
                return Decision::Abort;
            }
            return Decision::Inject(Self::format_critical(belief, errors));
        }
        self.consecutive_abort_count = 0;

        if belief < self.warn_threshold {
            return Decision::Inject(Self::format_warning(belief, errors));
        }
        if belief < self.remind_threshold {
            return Decision::Inject(Self::format_reminder(belief, errors));
        }
        Decision::None
    }

    fn format_reminder(b: f64, errors: &[String]) -> String {
        let error_section = if errors.is_empty() { String::new() } else {
            format!("\nRecent:\n{}", format_errors(errors, 3))
        };
        format!(
            "[System note: Some tool executions showed issues (belief {:.2}).{}]",
            b, error_section,
        )
    }

    fn format_warning(b: f64, errors: &[String]) -> String {
        let error_section = if errors.is_empty() { String::new() } else {
            format!("\nRecent errors:\n{}", format_errors(errors, 5))
        };
        format!(
            "[System note: Multiple failures detected (belief {:.2}). Adjust approach.{}]",
            b, error_section,
        )
    }

    fn format_critical(b: f64, errors: &[String]) -> String {
        let error_section = if errors.is_empty() { String::new() } else {
            format!("\nErrors:\n{}", format_errors(errors, errors.len().min(10)))
        };
        format!(
            "[CRITICAL: Execution quality severely degraded (belief {:.2}).{}]",
            b, error_section,
        )
    }
}

fn format_errors(errors: &[String], n: usize) -> String {
    errors.iter().rev().take(n)
        .map(|e| format!("- {}", e))
        .collect::<Vec<_>>()
        .join("\n")
}

impl Default for DecisionEngine {
    fn default() -> Self { Self::new() }
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
    fn bad_belief_aborts() {
        let mut de = DecisionEngine::new();
        let d = de.decide(0.2, &["error".into()]);
        assert!(matches!(d, Decision::Abort));
    }
}
