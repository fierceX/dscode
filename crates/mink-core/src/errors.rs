use std::fmt;

/// ErrorCategory determines auto-model upgrade weight and retry strategy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorCategory {
    Network,
    Auth,
    RateLimit,
    Parse,
    Tool,
    Internal,
}

/// ErrorSeverity drives recovery decisions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorSeverity {
    Warning,
    Error,
    Fatal,
}

/// Structured error information for signal routing.
#[derive(Debug, Clone)]
pub struct ErrorInfo {
    pub category: ErrorCategory,
    pub severity: ErrorSeverity,
    pub recoverable: bool,
}

impl ErrorInfo {
    pub fn new(category: ErrorCategory, severity: ErrorSeverity, recoverable: bool) -> Self {
        Self {
            category,
            severity,
            recoverable,
        }
    }
}

impl fmt::Display for ErrorInfo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{:?}/{:?}/recoverable={}",
            self.category, self.severity, self.recoverable
        )
    }
}

pub fn classify_error_from_message(msg: &str) -> ErrorInfo {
    let lower = msg.to_lowercase();

    if lower.contains("safety policy")
        || lower.contains("tool execution failed")
        || lower.contains("command blocked")
    {
        return ErrorInfo::new(ErrorCategory::Tool, ErrorSeverity::Warning, true);
    }
    if lower.contains("tool not found")
        || lower.contains("unknown tool")
        || lower.contains("no tool")
    {
        return ErrorInfo::new(ErrorCategory::Parse, ErrorSeverity::Error, false);
    }
    if lower.contains("parse error") || lower.contains("json") || lower.contains("deserialize") {
        return ErrorInfo::new(ErrorCategory::Parse, ErrorSeverity::Error, false);
    }
    if lower.contains("stream")
        && (lower.contains("end of file")
            || lower.contains("connection")
            || lower.contains("timeout"))
    {
        return ErrorInfo::new(ErrorCategory::Network, ErrorSeverity::Warning, true);
    }
    if lower.contains("unauthorized")
        || lower.contains("forbidden")
        || lower.contains("auth")
        || lower.contains("api key")
    {
        return ErrorInfo::new(ErrorCategory::Auth, ErrorSeverity::Error, false);
    }
    if lower.contains("rate limit") || lower.contains("too many") {
        return ErrorInfo::new(ErrorCategory::RateLimit, ErrorSeverity::Warning, true);
    }

    ErrorInfo::new(ErrorCategory::Internal, ErrorSeverity::Error, false)
}

/// Classify an anyhow::Error by inspecting the error chain.
pub fn classify_anyhow(err: &anyhow::Error) -> ErrorInfo {
    let msg = format!("{}", err);
    if let Some(source_err) = err.root_cause().downcast_ref::<reqwest::Error>() {
        if let Some(status) = source_err.status() {
            return match status.as_u16() {
                400..=403 => ErrorInfo::new(ErrorCategory::Auth, ErrorSeverity::Error, false),
                404 | 422 => ErrorInfo::new(ErrorCategory::Parse, ErrorSeverity::Error, false),
                429 => ErrorInfo::new(ErrorCategory::RateLimit, ErrorSeverity::Warning, true),
                500..=599 => ErrorInfo::new(ErrorCategory::Network, ErrorSeverity::Warning, true),
                408 => ErrorInfo::new(ErrorCategory::Network, ErrorSeverity::Warning, true),
                _ => ErrorInfo::new(ErrorCategory::Internal, ErrorSeverity::Error, false),
            };
        }
        if source_err.is_connect() || source_err.is_timeout() {
            return ErrorInfo::new(ErrorCategory::Network, ErrorSeverity::Warning, true);
        }
    }
    classify_error_from_message(&msg)
}

#[cfg(test)]
#[path = "errors_tests.rs"]
mod tests;
