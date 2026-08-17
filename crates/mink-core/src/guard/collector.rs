//! 信号采集器 — 自维护调用历史。

use crate::config::EditLoopWeights;
use crate::tools::metadata::{ToolBlocker, ToolFailureKind, ToolStatus};
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

impl SignalKind {
    /// 硬信号 = 确定性/安全类失败，单独出现即参与决策。
    /// 软信号（regex 嗅探、参数、编辑循环）单独出现且信念尚可时不干预
    /// normal flow.
    pub fn is_hard(&self) -> bool {
        matches!(
            self,
            SignalKind::ToolFailed
                | SignalKind::SafetyBlocked
                | SignalKind::CompileError
                | SignalKind::TestFailure
        )
    }
}

#[derive(Debug, Clone)]
pub struct Signal {
    pub kind: SignalKind,
    pub severity: f64,
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
        Self {
            kind,
            severity,
            source_tool: source_tool.into(),
            exit_code,
            matched_pattern,
            message: message.into(),
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
    weights: EditLoopWeights,
}

impl SignalCollector {
    /// Test-only convenience constructor. Production paths must use
    /// `with_weights` and the runtime `SignalConfig`.
    #[cfg(test)]
    pub fn new() -> Self {
        let config = crate::config::SignalConfig::default();
        Self::with_weights(config.seq_window, config.edit_loop_weights)
    }

    pub(crate) fn with_weights(seq_window: usize, weights: EditLoopWeights) -> Self {
        Self {
            call_history: VecDeque::with_capacity(seq_window.max(8)),
            seq_window,
            weights,
        }
    }

    pub fn collect(
        &mut self,
        tool_name: &str,
        status: ToolStatus,
        output: &str,
        exit_code: Option<i32>,
        scan_error_patterns: bool,
    ) -> Vec<Signal> {
        let mut signals = Vec::new();

        let status_signal = match status {
            ToolStatus::Succeeded => None,
            ToolStatus::Failed(kind) => Some((
                match kind {
                    ToolFailureKind::SafetyBlocked => SignalKind::SafetyBlocked,
                    ToolFailureKind::ArgumentInvalid
                    | ToolFailureKind::StaleTag
                    | ToolFailureKind::AmbiguousMatch
                    | ToolFailureKind::PathOutOfScope => SignalKind::ArgumentError,
                    ToolFailureKind::Timeout
                    | ToolFailureKind::ProcessFailed
                    | ToolFailureKind::Aborted
                    | ToolFailureKind::Unknown => SignalKind::ToolFailed,
                },
                kind.label().to_string(),
                1.0,
            )),
            ToolStatus::Blocked(blocker) => {
                // RecoveryGuard is a strong corrective signal, but not an
                // actual executor failure. Preserve its established 0.9 weight.
                let severity = match blocker {
                    ToolBlocker::RecoveryGuard => 0.9,
                    ToolBlocker::ToolSurface | ToolBlocker::StormBreaker => 1.0,
                };
                Some((
                    SignalKind::ToolFailed,
                    format!("blocked by {blocker:?}"),
                    severity,
                ))
            }
            // 用户主动中断不是模型失败：不产生信号、不喂信念。
            ToolStatus::Interrupted => None,
        };
        if let Some((kind, default_message, severity)) = status_signal {
            let message = output
                .lines()
                .next()
                .unwrap_or(&default_message)
                .to_string();
            signals.push(Signal::new(
                kind, severity, tool_name, exit_code, None, message,
            ));
        }

        // 正则模式检测只适用于命令/诊断输出（Bash/Python 等）。
        // 内容返回型工具（Read/Glob/Grep 等）的输出是文件内容或搜索结果，
        // 对它们跑编译错误/超时/未找到等模式会产生大量误报
        // （例如源码里出现 "timeout"、"error[E0425]" 字样）。
        if scan_error_patterns && let Some(s) = self.detect_error(tool_name, output) {
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
                self.weights.five_edits
            } else if edit_count == 6 {
                self.weights.six_edits
            } else {
                self.weights.excess_edits
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
                self.weights.three_or_more_edit_diff_alternations
            } else if alt_count == 2 {
                self.weights.two_edit_diff_alternations
            } else {
                self.weights.one_edit_diff_alternation
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

#[cfg(test)]
impl Default for SignalCollector {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
#[path = "collector_tests.rs"]
mod tests;
