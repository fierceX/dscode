use std::sync::OnceLock;

/// 信号类型
#[derive(Debug, Clone, PartialEq)]
pub enum SignalKind {
    ToolError,
    SlowExecution,
    MassiveOutput,
    RepeatedCall,
}

/// 单个信号
#[derive(Debug, Clone)]
pub struct Signal {
    pub kind: SignalKind,
    pub severity: f64,
    pub source: String,
    pub detail: String,
}

/// 预编译的检测模式（全局静态，只编译一次）
struct CompiledPatterns {
    patterns: Vec<(regex::Regex, f64, &'static str)>,
}

fn compiled_patterns() -> &'static CompiledPatterns {
    static PATTERNS: OnceLock<CompiledPatterns> = OnceLock::new();
    PATTERNS.get_or_init(|| {
        let raw: &[(&str, f64, &str)] = &[
            (r"error\[E\d+\]:",                        1.0, "Rust compilation error"),
            (r"error: aborting due to \d+ previous error", 1.0, "Rust compilation error"),
            (r"FAILED [a-zA-Z0-9_/\\\.-]+::[a-zA-Z0-9_]+", 0.8, "Test failure"),
            (r"FAILURES===",                           0.8, "Pytest failure"),
            (r"Traceback \(most recent call last\):",  0.8, "Python exception"),
            (r"exit code: \d+|Exited with code \d+",   0.5, "Non-zero exit"),
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

/// 信号采集器 — 只负责采集，不知道决策逻辑。
pub struct SignalCollector {
    slow_threshold_ms: u64,
    large_output_bytes: usize,
}

impl SignalCollector {
    pub fn new() -> Self {
        Self {
            slow_threshold_ms: 5000,
            large_output_bytes: 100_000,
        }
    }

    /// 采集信号。每个工具执行完成后调用一次。
    pub fn collect(
        &self,
        tool_name: &str,
        elapsed_ms: u64,
        output_len: usize,
        output: &str,
    ) -> Vec<Signal> {
        let mut signals = Vec::new();

        // ① 工具输出错误检测（替换旧的 error.sh 子进程）
        if let Some(sig) = self.detect_error(tool_name, output) {
            signals.push(sig);
        }

        // ② 执行过慢
        if elapsed_ms > self.slow_threshold_ms {
            signals.push(Signal {
                kind: SignalKind::SlowExecution,
                severity: 0.3,
                source: tool_name.into(),
                detail: format!("slow execution: {}ms", elapsed_ms),
            });
        }

        // ③ 输出过大
        if output_len > self.large_output_bytes {
            signals.push(Signal {
                kind: SignalKind::MassiveOutput,
                severity: 0.2,
                source: tool_name.into(),
                detail: format!("large output: {} bytes", output_len),
            });
        }

        signals
    }

    fn detect_error(&self, tool_name: &str, output: &str) -> Option<Signal> {
        let cp = compiled_patterns();
        for (re, weight, detail) in &cp.patterns {
            if re.is_match(output) {
                return Some(Signal {
                    kind: SignalKind::ToolError,
                    severity: *weight,
                    source: tool_name.into(),
                    detail: (*detail).into(),
                });
            }
        }
        None
    }
}

impl Default for SignalCollector {
    fn default() -> Self { Self::new() }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_rust_compilation_error() {
        let c = SignalCollector::new();
        let sigs = c.collect("Bash", 100, 50, "error[E0425]: cannot find value `x`");
        assert!(sigs.iter().any(|s| matches!(s.kind, SignalKind::ToolError)));
    }

    #[test]
    fn detects_test_failure() {
        let c = SignalCollector::new();
        let sigs = c.collect("Bash", 100, 200,
            "FAILED tests/test_main.py::test_foo - AssertionError");
        assert!(sigs.iter().any(|s| matches!(s.kind, SignalKind::ToolError)));
    }

    #[test]
    fn detects_slow_execution() {
        let c = SignalCollector::new();
        let sigs = c.collect("Bash", 10_000, 100, "done");
        assert!(sigs.iter().any(|s| matches!(s.kind, SignalKind::SlowExecution)));
    }

    #[test]
    fn clean_output_no_signals() {
        let c = SignalCollector::new();
        let sigs = c.collect("Read", 10, 50, "everything is fine");
        assert!(sigs.is_empty());
    }

    #[test]
    fn not_slow_below_threshold() {
        let c = SignalCollector::new();
        let sigs = c.collect("Read", 4999, 50, "output");
        assert!(!sigs.iter().any(|s| matches!(s.kind, SignalKind::SlowExecution)));
    }
}
