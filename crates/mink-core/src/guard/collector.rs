//! 信号采集器 — 自维护调用历史。

use std::collections::VecDeque;
use std::sync::OnceLock;

#[derive(Debug, Clone, PartialEq)]
pub enum SignalKind {
    ToolError,
    ToolFailed,
    EditLoop,
    SafetyBlocked,
    ArgumentError,
    TestFailure,
    CompileError,
}

#[derive(Debug, Clone)]
pub struct Signal {
    pub kind: SignalKind,
    pub severity: f64,
    pub source: String,
    pub detail: String,
    pub source_tool: String,
    pub exit_code: Option<i32>,
    pub matched_pattern: Option<String>,
    pub message: String,
}

impl Signal {
    fn new(
        kind: SignalKind,
        severity: f64,
        source_tool: impl Into<String>,
        exit_code: Option<i32>,
        matched_pattern: Option<String>,
        message: impl Into<String>,
    ) -> Self {
        let source_tool = source_tool.into();
        let message = message.into();
        Self {
            kind,
            severity,
            source: source_tool.clone(),
            detail: message.clone(),
            source_tool,
            exit_code,
            matched_pattern,
            message,
        }
    }
}

struct CompiledPatterns {
    patterns: Vec<(regex::Regex, f64, &'static str, SignalKind)>,
}

fn compiled_patterns() -> &'static CompiledPatterns {
    static PATTERNS: OnceLock<CompiledPatterns> = OnceLock::new();
    PATTERNS.get_or_init(|| {
        let raw: &[(&str, f64, &str, SignalKind)] = &[
            (
                r"error\[E\d+\]:",
                0.9,
                "Rust compilation error",
                SignalKind::CompileError,
            ),
            (
                r"error: aborting due to \d+ previous error",
                0.9,
                "Rust compilation error",
                SignalKind::CompileError,
            ),
            (
                r"FAILED [a-zA-Z0-9_/\\\.-]+::[a-zA-Z0-9_]+",
                0.8,
                "Test failure",
                SignalKind::TestFailure,
            ),
            (
                r"FAILURES===",
                0.8,
                "Pytest failure",
                SignalKind::TestFailure,
            ),
            (
                r"Traceback \(most recent call last\):",
                0.8,
                "Python exception",
                SignalKind::ToolError,
            ),
            (
                r"Permission denied|EACCES",
                0.5,
                "Permission denied",
                SignalKind::ToolError,
            ),
            (
                r"command not found|No such file|does not exist",
                0.5,
                "Not found",
                SignalKind::ToolError,
            ),
            (
                r"Timed? ?out|timeout|killed",
                0.3,
                "Timeout",
                SignalKind::ToolError,
            ),
        ];
        CompiledPatterns {
            patterns: raw
                .iter()
                .map(|(pat, w, d, k)| {
                    (
                        regex::Regex::new(pat).expect("invalid error pattern"),
                        *w,
                        *d,
                        k.clone(),
                    )
                })
                .collect(),
        }
    })
}

pub struct SignalCollector {
    call_history: VecDeque<String>,
    seq_window: usize,
}

impl SignalCollector {
    pub fn new() -> Self {
        Self {
            call_history: VecDeque::with_capacity(8),
            seq_window: 6,
        }
    }

    pub fn collect(
        &mut self,
        tool_name: &str,
        output: &str,
        exit_code: Option<i32>,
        full_content: &str,
        scan_error_patterns: bool,
    ) -> Vec<Signal> {
        let mut signals = Vec::new();

        if let Some(code) = exit_code {
            if code != 0 {
                signals.push(Signal::new(
                    SignalKind::ToolFailed,
                    1.0,
                    tool_name,
                    Some(code),
                    None,
                    format!("process exited with code {}", code),
                ));
            }
        } else if full_content.starts_with("Error:") {
            let first_line = full_content.lines().next().unwrap_or("Error").to_string();
            let kind = if full_content.contains("command blocked by bash safety policy") {
                SignalKind::SafetyBlocked
            } else if full_content.contains("no command provided")
                || full_content.contains("no path provided")
                || full_content.contains("invalid todo status")
            {
                SignalKind::ArgumentError
            } else {
                SignalKind::ToolFailed
            };
            signals.push(Signal::new(kind, 1.0, tool_name, None, None, first_line));
        }

        // 正则模式检测只适用于命令/诊断输出（Bash/Python 等）。
        // 内容返回型工具（Read/Glob/Grep 等）的输出是文件内容或搜索结果，
        // 对它们跑编译错误/超时/未找到等模式会产生大量误报
        // （例如源码里出现 "timeout"、"error[E0425]" 字样）。
        if scan_error_patterns
            && let Some(s) = self.detect_error(tool_name, output)
        {
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
        for (re, weight, detail, kind) in &cp.patterns {
            if re.is_match(output) {
                return Some(Signal::new(
                    kind.clone(),
                    *weight,
                    tool_name,
                    None,
                    Some(re.as_str().to_string()),
                    (*detail).to_string(),
                ));
            }
        }
        None
    }

    fn detect_edit_loop(&self, history: &VecDeque<String>) -> Option<Signal> {
        if history.len() < self.seq_window {
            return None;
        }
        let edit_count = history.iter().filter(|n| *n == "Edit").count();
        let has_diff = history.iter().any(|n| *n == "Diff");
        let has_read_op = history
            .iter()
            .any(|n| matches!(n.as_str(), "Bash" | "Grep" | "Read" | "Glob"));

        if edit_count > 4 {
            let severity = if edit_count == 5 {
                0.6
            } else if edit_count == 6 {
                0.8
            } else {
                0.9
            };
            return Some(Signal::new(
                SignalKind::EditLoop,
                severity,
                "EditLoop",
                None,
                None,
                format!(
                    "excessive edits: {} of {} calls are Edit",
                    edit_count, self.seq_window
                ),
            ));
        }
        if has_diff && !has_read_op && has_edit_diff_alternation(history) {
            let alt_count = count_edit_diff_alternations(history);
            let severity = if alt_count >= 3 {
                0.9
            } else if alt_count == 2 {
                0.7
            } else {
                0.4
            };
            return Some(Signal::new(
                SignalKind::EditLoop,
                severity,
                "EditLoop",
                None,
                None,
                format!(
                    "edit-diff loop ({} alternations) without read operations",
                    alt_count
                ),
            ));
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
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_rust_error() {
        let mut c = SignalCollector::new();
        let sigs = c.collect("Bash", "error[E0425]: cannot find value", None, "", true);
        assert!(
            sigs.iter()
                .any(|s| matches!(s.kind, SignalKind::CompileError))
        );
        assert!(sigs.iter().any(|s| s.matched_pattern.is_some()));
    }

    #[test]
    fn clean_output_no_signals() {
        let mut c = SignalCollector::new();
        let sigs = c.collect("Read", "everything is fine", None, "", false);
        assert!(sigs.is_empty());
    }

    #[test]
    fn content_tool_output_with_timeout_keyword_does_not_emit_pattern_signal() {
        let mut c = SignalCollector::new();
        let sigs = c.collect(
            "Read",
            "209:        let _ = tokio::time::timeout(std::time::Duration::from_secs(5), self)",
            None,
            "",
            false,
        );
        assert!(
            !sigs.iter().any(|s| matches!(s.kind, SignalKind::ToolError)),
            "Read output is file content; 'timeout' must not produce a ToolError signal"
        );
    }

    #[test]
    fn command_tool_output_with_timeout_keyword_emits_pattern_signal() {
        let mut c = SignalCollector::new();
        let sigs = c.collect("Bash", "Timed out after 60s", None, "", true);
        assert!(
            sigs.iter().any(|s| matches!(s.kind, SignalKind::ToolError)),
            "Bash diagnostics containing 'Timed out' must produce a ToolError signal"
        );
    }

    #[test]
    fn content_tool_output_with_compile_error_keyword_does_not_emit_pattern_signal() {
        let mut c = SignalCollector::new();
        let sigs = c.collect(
            "Read",
            "\"error[E0425]: cannot find value\" // test fixture string",
            None,
            "",
            false,
        );
        assert!(
            !sigs.iter().any(|s| matches!(s.kind, SignalKind::CompileError)),
            "Read output is file content; 'error[E0425]' must not produce a CompileError signal"
        );
    }

    #[test]
    fn detects_edit_loop_excessive_edits() {
        let mut c = SignalCollector::new();
        for _ in 0..5 {
            c.call_history.push_back("Edit".into());
        }
        c.call_history.push_back("Read".into());
        let sigs = c.collect("Edit", "ok", None, "", false);
        assert!(sigs.iter().any(|s| matches!(s.kind, SignalKind::EditLoop)));
    }

    #[test]
    fn detects_tool_failed_via_exit_code() {
        let mut c = SignalCollector::new();
        let sigs = c.collect("Bash", "output", Some(1), "output", false);
        assert!(
            sigs.iter()
                .any(|s| matches!(s.kind, SignalKind::ToolFailed))
        );
    }

    #[test]
    fn detects_tool_failed_via_error_prefix() {
        let mut c = SignalCollector::new();
        let sigs = c.collect("Read", "", None, "Error: file not found", false);
        assert!(
            sigs.iter()
                .any(|s| matches!(s.kind, SignalKind::ToolFailed))
        );
    }

    #[test]
    fn detects_safety_blocked() {
        let mut c = SignalCollector::new();
        let sigs = c.collect(
            "Bash",
            "",
            None,
            "Error: tool execution failed: Error: command blocked by bash safety policy (sudo)",
            false,
        );
        assert!(
            sigs.iter()
                .any(|s| matches!(s.kind, SignalKind::SafetyBlocked))
        );
    }

    #[test]
    fn exit_code_zero_does_not_fail() {
        let mut c = SignalCollector::new();
        let sigs = c.collect("Bash", "ok", Some(0), "ok", false);
        assert!(
            !sigs
                .iter()
                .any(|s| matches!(s.kind, SignalKind::ToolFailed))
        );
    }
}
