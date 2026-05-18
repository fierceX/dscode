use std::collections::VecDeque;

/// Controller state machine for stall detection and control actions.
///
/// Uses P(stall) = 1 - 0.5^k for continuous Bayesian-inspired stall probability,
/// replacing the old hard-count threshold approach.
pub struct Controller {
    // Bayesian stall probability
    no_progress_count: u32,
    stall_probability: f64,

    // Fix loop detection (per user turn)
    tool_call_count: u32,
    had_end_turn: bool,

    // Context pressure history (sliding window)
    context_pressure_history: VecDeque<f32>,
    cache_hit_history: VecDeque<u8>,
}

/// Control action produced by the controller.
#[derive(Debug, Clone, PartialEq)]
pub enum ControlAction {
    /// Inject a reflection hint into the conversation to change strategy.
    InjectReflectionHint,
    /// Switch to a stronger model (e.g. Pro).
    UpgradeModel,
    /// Request human intervention — the agent is stuck.
    Abort,
}

impl Controller {
    /// Create a new Controller.
    pub fn new() -> Self {
        Self {
            no_progress_count: 0,
            stall_probability: 0.0,
            tool_call_count: 0,
            had_end_turn: false,
            context_pressure_history: VecDeque::new(),
            cache_hit_history: VecDeque::new(),
        }
    }

    /// Reset per-turn counters (tool call tracking).
    /// Call at the start of each user input.
    pub fn reset_per_turn(&mut self) {
        self.tool_call_count = 0;
        self.had_end_turn = false;
    }

    /// Record a tool call during the current turn.
    pub fn note_tool_call(&mut self) {
        self.tool_call_count += 1;
    }

    /// Record that the turn ended normally (end_turn was received).
    pub fn note_end_turn(&mut self) {
        self.had_end_turn = true;
    }

    /// Update stall probability based on whether progress was made.
    ///
    /// `progress_made`: true if the error situation improved or a stop was reached.
    ///
    /// P(stall) = 1 - 0.5^k where k = consecutive no-progress count.
    ///   k=0 → P=0, k=1 → P=0.5, k=3 → P=0.875, k=5 → P=0.969, k=10 → P=0.999
    pub fn note_progress(&mut self, progress_made: bool) {
        if progress_made {
            self.no_progress_count = 0;
        } else {
            self.no_progress_count += 1;
        }
        self.stall_probability = 1.0 - 0.5_f64.powi(self.no_progress_count as i32);
    }

    /// Record an error (convenience wrapper: no progress).
    pub fn note_error(&mut self, error_decreased: bool) {
        self.note_progress(error_decreased);
    }

    /// Record context pressure observation.
    pub fn record_context_pressure(&mut self, pressure: f32) {
        self.context_pressure_history.push_back(pressure);
        if self.context_pressure_history.len() > 10 {
            self.context_pressure_history.pop_front();
        }
    }

    /// Record cache hit ratio observation.
    pub fn record_cache_hit(&mut self, hit: u8) {
        self.cache_hit_history.push_back(hit);
        if self.cache_hit_history.len() > 10 {
            self.cache_hit_history.pop_front();
        }
    }

    /// Current stall probability.
    pub fn stall_probability(&self) -> f64 {
        self.stall_probability
    }

    /// Current no-progress count.
    pub fn no_progress_count(&self) -> u32 {
        self.no_progress_count
    }

    /// Whether a fix loop is detected:
    ///   tool_call_count > 15 && !had_end_turn
    pub fn has_fix_loop(&self) -> bool {
        self.tool_call_count > 15 && !self.had_end_turn
    }

    /// Get the current control action based on stall probability.
    ///
    ///   > 0.99 + k ≥ 10 → Abort
    ///   > 0.95          → UpgradeModel
    ///   > 0.80          → InjectReflectionHint
    ///   ≤ 0.80          → None
    pub fn get_control_action(&self) -> Option<ControlAction> {
        if self.stall_probability > 0.99 && self.no_progress_count >= 10 {
            return Some(ControlAction::Abort);
        }
        if self.stall_probability > 0.95 {
            return Some(ControlAction::UpgradeModel);
        }
        if self.stall_probability > 0.80 {
            return Some(ControlAction::InjectReflectionHint);
        }
        None
    }

    /// Check if the controller is in a "locked" state (stall probability high).
    /// Used as a condition to keep model locked at pro.
    pub fn is_locked(&self) -> bool {
        self.stall_probability > 0.80
    }

    /// Reset stall probability (e.g. after a successful turn).
    /// Gradually unlocks the model.
    pub fn reset_stall(&mut self) {
        self.no_progress_count = 0;
        self.stall_probability = 0.0;
    }

    /// Format a human-readable breakdown of current state.
    pub fn format_state(&self) -> String {
        format!(
            "P(stall)={:.3}, k={}, tools={}, end_turn={}, locked={}",
            self.stall_probability,
            self.no_progress_count,
            self.tool_call_count,
            self.had_end_turn,
            self.is_locked(),
        )
    }

    /// Return a structured JSON snapshot of the controller's current state.
    /// Used for logging at every turn boundary.
    pub fn snapshot(&self) -> serde_json::Value {
        serde_json::json!({
            "k": self.no_progress_count,
            "P_stall": self.stall_probability,
            "locked": self.is_locked(),
            "fix_loop": self.has_fix_loop(),
            "tool_call_count": self.tool_call_count,
            "had_end_turn": self.had_end_turn,
            "control_action": match self.get_control_action() {
                Some(ControlAction::InjectReflectionHint) => "InjectReflectionHint",
                Some(ControlAction::UpgradeModel) => "UpgradeModel",
                Some(ControlAction::Abort) => "Abort",
                None => "None",
            },
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stall_probability_zero_when_progressing() {
        let mut c = Controller::new();
        c.note_error(true); // error decreased
        assert_eq!(c.stall_probability(), 0.0);
    }

    #[test]
    fn stall_probability_k3() {
        let mut c = Controller::new();
        c.note_error(false);
        c.note_error(false);
        c.note_error(false);
        let p = c.stall_probability();
        assert!((p - 0.875).abs() < 1e-10, "expected 0.875, got {p}");
    }

    #[test]
    fn stall_probability_k5() {
        let mut c = Controller::new();
        for _ in 0..5 {
            c.note_error(false);
        }
        let p = c.stall_probability();
        assert!(p > 0.968, "expected >0.968, got {p}");
    }

    #[test]
    fn get_control_action_at_p08() {
        let mut c = Controller::new();
        // k=3 → P=0.875 → >0.80
        c.note_error(false);
        c.note_error(false);
        c.note_error(false);
        assert_eq!(c.get_control_action(), Some(ControlAction::InjectReflectionHint));
    }

    #[test]
    fn get_control_action_at_p095() {
        let mut c = Controller::new();
        // k=5 → P=0.969 → >0.95
        for _ in 0..5 {
            c.note_error(false);
        }
        assert_eq!(c.get_control_action(), Some(ControlAction::UpgradeModel));
    }

    #[test]
    fn get_control_action_abort_at_p099_k10() {
        let mut c = Controller::new();
        for _ in 0..10 {
            c.note_error(false);
        }
        assert_eq!(c.get_control_action(), Some(ControlAction::Abort));
    }

    #[test]
    fn fix_loop_detected() {
        let mut c = Controller::new();
        c.note_tool_call(); // 1
        // not enough calls yet
        assert!(!c.has_fix_loop());
        for _ in 0..15 {
            c.note_tool_call();
        }
        // 16 calls, no end_turn
        assert!(c.has_fix_loop());
    }

    #[test]
    fn fix_loop_not_detected_with_end_turn() {
        let mut c = Controller::new();
        for _ in 0..16 {
            c.note_tool_call();
        }
        c.note_end_turn();
        assert!(!c.has_fix_loop());
    }

    #[test]
    fn reset_clears_stall() {
        let mut c = Controller::new();
        c.note_error(false);
        c.note_error(false);
        c.note_error(false);
        assert!(c.stall_probability() > 0.8);
        c.reset_stall();
        assert_eq!(c.stall_probability(), 0.0);
    }

    #[test]
    fn reset_per_turn_clears_tool_count() {
        let mut c = Controller::new();
        c.note_tool_call();
        c.note_tool_call();
        c.reset_per_turn();
        assert_eq!(c.tool_call_count, 0);
        assert!(!c.had_end_turn);
    }

    #[test]
    fn record_context_pressure_maintains_window() {
        let mut c = Controller::new();
        for i in 0..20 {
            c.record_context_pressure(i as f32 / 10.0);
        }
        assert!(c.context_pressure_history.len() <= 10);
    }

    #[test]
    fn format_state_contains_key_fields() {
        let mut c = Controller::new();
        c.note_error(false);
        let s = c.format_state();
        assert!(s.contains("P(stall)="));
        assert!(s.contains("k="));
        assert!(s.contains("tools="));
        assert!(s.contains("end_turn="));
    }
}
