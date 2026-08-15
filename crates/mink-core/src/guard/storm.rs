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
        Self {
            window: VecDeque::new(),
            max_window,
            threshold,
        }
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

        let count = self
            .window
            .iter()
            .filter(|(n, a)| n == name && a == args)
            .count();
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
#[path = "storm_tests.rs"]
mod tests;
