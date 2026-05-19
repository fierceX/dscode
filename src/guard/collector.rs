//! 信号采集器 — 自维护调用历史。

use std::collections::VecDeque;
use std::sync::OnceLock;

#[derive(Debug, Clone, PartialEq)]
pub enum SignalKind {
    ToolError,
    ToolFailed,
    EditLoop,
}

#[derive(Debug, Clone)]
pub struct Signal {
    pub kind: SignalKind,
    pub severity: f64,
    pub source: String,
    pub detail: String,
}

struct CompiledPatterns {
    patterns: Vec<(regex::Regex, f64, &'static str)>,
}

fn compiled_patterns() -> &'static CompiledPatterns {
    static PATTERNS: OnceLock<CompiledPatterns> = OnceLock::new();
    PATTERNS.get_or_init(|| {
        let raw: &[(&str, f64, &str)] = &[
            (r"error\[E\d+\]:",                        0.9, "Rust compilation error"),
            (r"error: aborting due to \d+ previous error", 0.9, "Rust compilation error"),
            (r"FAILED [a-zA-Z0-9_/\\\.-]+::[a-zA-Z0-9_]+", 0.8, "Test failure"),
            (r"FAILURES===",                           0.8, "Pytest failure"),
            (r"Traceback \(most recent call last\):",  0.8, "Python exception"),
            (r"Permission denied|EACCES",              0.5, "Permission denied"),
            (r"command not found|No such file|does not exist", 0.5, "Not found"),
            (r"Timed? ?out|timeout|killed",            0.3, "Timeout"),
        ];
        CompiledPatterns {
            patterns: raw.iter().map(|(pat, w, d)| {
                (regex::Regex::new(pat).expect("invalid error pattern"), *w, *d)
            }).collect(),
        }
    })
}

pub struct SignalCollector {
    call_history: VecDeque<String>,
    seq_window: usize,
}

impl SignalCollector {
    pub fn new() -> Self {
        Self { call_history: VecDeque::with_capacity(8), seq_window: 6 }
    }

    pub fn collect(&mut self, tool_name: &str, output: &str, exit_code: Option<i32>, full_content: &str) -> Vec<Signal> {
        let mut signals = Vec::new();

        if let Some(code) = exit_code {
            if code != 0 {
                signals.push(Signal {
                    kind: SignalKind::ToolFailed, severity: 1.0,
                    source: tool_name.into(), detail: format!("process exited with code {}", code),
                });
            }
        } else if full_content.starts_with("Error:") {
            signals.push(Signal {
                kind: SignalKind::ToolFailed, severity: 1.0,
                source: tool_name.into(),
                detail: full_content.lines().next().unwrap_or("Error").to_string(),
            });
        }

        if let Some(s) = self.detect_error(tool_name, output) {
            signals.push(s);
        }

        self.call_history.push_back(tool_name.to_string());
        if self.call_history.len() > self.seq_window {
            self.call_history.pop_front();
        }
        if let Some(s) = self.detect_edit_loop(&self.call_history) {
            signals.push(s);
        }
        signals
    }

    fn detect_error(&self, tool_name: &str, output: &str) -> Option<Signal> {
        let cp = compiled_patterns();
        for (re, weight, detail) in &cp.patterns {
            if re.is_match(output) {
                return Some(Signal {
                    kind: SignalKind::ToolError, severity: *weight,
                    source: tool_name.into(), detail: (*detail).into(),
                });
            }
        }
        None
    }

    fn detect_edit_loop(&self, history: &VecDeque<String>) -> Option<Signal> {
        if history.len() < self.seq_window { return None; }
        let edit_count = history.iter().filter(|n| *n == "Edit").count();
        let has_diff = history.iter().any(|n| *n == "Diff");
        let has_read_op = history.iter().any(|n| matches!(n.as_str(), "Bash" | "Grep" | "Read" | "Glob"));

        if edit_count > 4 {
            let severity = if edit_count == 5 { 0.6 } else if edit_count == 6 { 0.8 } else { 0.9 };
            return Some(Signal {
                kind: SignalKind::EditLoop, severity,
                source: "EditLoop".into(),
                detail: format!("excessive edits: {} of {} calls are Edit", edit_count, self.seq_window),
            });
        }
        if has_diff && !has_read_op && has_edit_diff_alternation(history) {
            let alt_count = count_edit_diff_alternations(history);
            let severity = if alt_count >= 3 { 0.9 } else if alt_count == 2 { 0.7 } else { 0.4 };
            return Some(Signal {
                kind: SignalKind::EditLoop, severity,
                source: "EditLoop".into(),
                detail: format!("edit-diff loop ({} alternations) without read operations", alt_count),
            });
        }
        None
    }
}

fn has_edit_diff_alternation(history: &VecDeque<String>) -> bool {
    count_edit_diff_alternations(history) >= 2
}

fn count_edit_diff_alternations(history: &VecDeque<String>) -> usize {
    let mut count = 0;
    let mut prev: Option<&str> = None;
    for name in history {
        match (prev, name.as_str()) {
            (Some("Edit"), "Diff") | (Some("Diff"), "Edit") => count += 1,
            _ => {}
        }
        prev = Some(name.as_str());
    }
    count
}

impl Default for SignalCollector {
    fn default() -> Self { Self::new() }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_rust_error() {
        let mut c = SignalCollector::new();
        let sigs = c.collect("Bash", "error[E0425]: cannot find value", None, "");
        assert!(sigs.iter().any(|s| matches!(s.kind, SignalKind::ToolError)));
    }

    #[test]
    fn clean_output_no_signals() {
        let mut c = SignalCollector::new();
        let sigs = c.collect("Read", "everything is fine", None, "");
        assert!(sigs.is_empty());
    }

    #[test]
    fn detects_edit_loop_excessive_edits() {
        let mut c = SignalCollector::new();
        for _ in 0..5 { c.call_history.push_back("Edit".into()); }
        c.call_history.push_back("Read".into());
        let sigs = c.collect("Edit", "ok", None, "");
        assert!(sigs.iter().any(|s| matches!(s.kind, SignalKind::EditLoop)));
    }

    #[test]
    fn detects_tool_failed_via_exit_code() {
        let mut c = SignalCollector::new();
        let sigs = c.collect("Bash", "output", Some(1), "output");
        assert!(sigs.iter().any(|s| matches!(s.kind, SignalKind::ToolFailed)));
    }

    #[test]
    fn detects_tool_failed_via_error_prefix() {
        let mut c = SignalCollector::new();
        let sigs = c.collect("Read", "", None, "Error: file not found");
        assert!(sigs.iter().any(|s| matches!(s.kind, SignalKind::ToolFailed)));
    }

    #[test]
    fn exit_code_zero_does_not_fail() {
        let mut c = SignalCollector::new();
        let sigs = c.collect("Bash", "ok", Some(0), "ok");
        assert!(!sigs.iter().any(|s| matches!(s.kind, SignalKind::ToolFailed)));
    }
}
