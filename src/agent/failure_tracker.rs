use crate::errors::ErrorCategory;

/// Multi-signal turn failure tracker.
///
/// Accumulates repair/error signals across a single user turn.
/// When the accumulated count crosses a configurable threshold,
/// the caller is notified to escalate (e.g. switch to a stronger model).
///
/// Resets at the start of each user turn so fresh intent starts clean.
pub struct TurnFailureTracker {
    count: u32,
    threshold: u32,
    breakdown: Vec<(&'static str, u32)>,
}

impl TurnFailureTracker {
    pub fn new(threshold: u32) -> Self {
        Self { count: 0, threshold, breakdown: Vec::new() }
    }

    /// Reset counters — call at the start of each user turn.
    pub fn reset(&mut self) {
        self.count = 0;
        self.breakdown.clear();
    }

    /// Record a signal and return true only on the call where count
    /// crosses the configured threshold.
    pub fn note_and_crossed_threshold(&mut self, kind: &'static str) -> bool {
        let before = self.count;
        self.count += 1;
        if let Some((_, n)) = self.breakdown.iter_mut().find(|(k, _)| *k == kind) {
            *n += 1;
        } else {
            self.breakdown.push((kind, 1));
        }
        before < self.threshold && self.count >= self.threshold
    }

    /// Format a human-readable breakdown for logging/display.
    pub fn format_breakdown(&self) -> String {
        if self.breakdown.is_empty() {
            return format!("{} signal(s)", self.count);
        }
        let parts: Vec<String> = self.breakdown
            .iter()
            .filter(|(_, n)| *n > 0)
            .map(|(kind, n)| format!("{}× {}", n, kind))
            .collect();
        parts.join(", ")
    }
}

/// Convert an ErrorCategory to a signal kind string for TurnFailureTracker.
pub fn category_to_signal_kind(cat: ErrorCategory) -> &'static str {
    match cat {
        ErrorCategory::Parse => "parse_error",
        ErrorCategory::Tool => "tool_error",
        ErrorCategory::Network => "network_error",
        ErrorCategory::RateLimit => "rate_limit",
        ErrorCategory::Auth => "auth_error",
        ErrorCategory::Internal => "internal_error",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn does_not_cross_on_first_signal() {
        let mut t = TurnFailureTracker::new(3);
        assert!(!t.note_and_crossed_threshold("tool_error"));
    }

    #[test]
    fn crosses_at_threshold() {
        let mut t = TurnFailureTracker::new(3);
        assert!(!t.note_and_crossed_threshold("tool_error"));
        assert!(!t.note_and_crossed_threshold("tool_error"));
        assert!(t.note_and_crossed_threshold("tool_error"));
    }

    #[test]
    fn reset_clears_count() {
        let mut t = TurnFailureTracker::new(2);
        t.note_and_crossed_threshold("tool_error");
        t.reset();
        assert!(!t.note_and_crossed_threshold("tool_error"));
    }

    #[test]
    fn breakdown_includes_multiple_kinds() {
        let mut t = TurnFailureTracker::new(5);
        t.note_and_crossed_threshold("tool_error");
        t.note_and_crossed_threshold("parse_error");
        t.note_and_crossed_threshold("tool_error");
        let b = t.format_breakdown();
        assert!(b.contains("2× tool_error"));
        assert!(b.contains("1× parse_error"));
    }

    #[test]
    fn category_maps_correctly() {
        assert_eq!(category_to_signal_kind(ErrorCategory::Parse), "parse_error");
        assert_eq!(category_to_signal_kind(ErrorCategory::Tool), "tool_error");
        assert_eq!(category_to_signal_kind(ErrorCategory::Network), "network_error");
    }
}
