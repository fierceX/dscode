use std::collections::VecDeque;

pub enum StormDecision {
    Allow,
    Suppress(String),
}

pub struct StormBreaker {
    window: VecDeque<(String, String)>,
    max_window: usize,
    threshold: usize,
}

impl StormBreaker {
    pub fn new(max_window: usize, threshold: usize) -> Self {
        Self { window: VecDeque::new(), max_window, threshold }
    }

    /// Clear the window — call at the start of a new user turn (fresh intent).
    pub fn reset(&mut self) {
        self.window.clear();
    }

    pub fn check(&mut self, name: &str, args: &str, is_mutating: bool) -> StormDecision {
        if is_mutating {
            self.window.clear();
        }

        let key = (name.to_string(), args.to_string());
        self.window.push_back(key.clone());
        while self.window.len() > self.max_window {
            self.window.pop_front();
        }

        let count = self.window.iter().filter(|(n, a)| n == name && a == args).count();
        if count >= self.threshold {
            StormDecision::Suppress(format!(
                "Tool call suppressed: {} repeated {} times in window of {}. Rephrase or try different approach.",
                name, count, self.max_window
            ))
        } else {
            StormDecision::Allow
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allows_if_below_threshold() {
        let mut sb = StormBreaker::new(6, 3);
        assert!(matches!(sb.check("Read", r#"{"path":"/tmp/x"}"#, false), StormDecision::Allow));
        assert!(matches!(sb.check("Read", r#"{"path":"/tmp/x"}"#, false), StormDecision::Allow));
    }

    #[test]
    fn suppresses_at_threshold() {
        let mut sb = StormBreaker::new(6, 3);
        for _ in 0..3 {
            let d = sb.check("Bash", r#"{"command":"ls"}"#, false);
            if matches!(d, StormDecision::Suppress(_)) {
                return;
            }
        }
        panic!("should have suppressed");
    }

    #[test]
    fn mutating_call_clears_window() {
        let mut sb = StormBreaker::new(6, 3);
        sb.check("Read", r#"{"path":"/x"}"#, false);
        sb.check("Read", r#"{"path":"/x"}"#, false);
        sb.check("Write", r#"{"path":"/x","content":"hi"}"#, true);
        // After mutating, window is cleared
        let d = sb.check("Read", r#"{"path":"/x"}"#, false);
        assert!(matches!(d, StormDecision::Allow));
    }

    #[test]
    fn window_slides_old_entries_out() {
        let mut sb = StormBreaker::new(3, 3);
        sb.check("Bash", "a", false);
        sb.check("Bash", "b", false);
        sb.check("Bash", "c", false);
        // "a" should be evicted, so only 2 of "Bash/a"
        let d = sb.check("Bash", "a", false);
        assert!(matches!(d, StormDecision::Allow));
    }

    #[test]
    fn reset_clears_window() {
        let mut sb = StormBreaker::new(3, 3);
        sb.check("Read", "a", false);
        sb.check("Read", "a", false);
        sb.reset();
        let d = sb.check("Read", "a", false);
        assert!(matches!(d, StormDecision::Allow));
    }
}
