use std::collections::VecDeque;

pub enum StormDecision {
    Allow,
    Suppress(String),
}

#[derive(Clone)]
struct StormEntry {
    name: String,
    args: String,
    mutating: bool,
}

pub struct StormBreaker {
    window: VecDeque<StormEntry>,
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

    /// Mutating and non-mutating calls keep independent histories.
    ///
    /// Repeated identical mutating calls still count across intervening
    /// mutations (a repeated edit loop must not immunize itself), while a
    /// successful mutation resets non-mutating history so the legitimate
    /// `Read → Edit/Write → Read` verification loop is not mistaken for a
    /// read storm.
    ///
    /// Suppression fires when the count for the same `(mutating, name, args)`
    /// class exceeds `threshold` — the first `threshold` identical calls stay
    /// allowed so graded tool feedback (e.g. the Edit soft no-op → 3-strike
    /// escalation) can complete before the breaker takes over.
    pub fn check(&mut self, name: &str, args: &str, mutating: bool) -> StormDecision {
        if mutating {
            self.window.retain(|entry| entry.mutating);
        }
        self.window.push_back(StormEntry {
            name: name.to_string(),
            args: args.to_string(),
            mutating,
        });
        while self.window.len() > self.max_window {
            self.window.pop_front();
        }

        let count = self
            .window
            .iter()
            .filter(|entry| entry.mutating == mutating && entry.name == name && entry.args == args)
            .count();
        if count > self.threshold {
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
