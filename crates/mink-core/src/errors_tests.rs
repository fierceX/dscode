use super::*;

#[test]
fn json_parse_error_is_parse() {
    let err = anyhow::anyhow!("json parse error: invalid syntax");
    let info = classify_anyhow(&err);
    assert_eq!(info.category, ErrorCategory::Parse);
    assert!(!info.recoverable);
}

#[test]
fn tool_error_is_tool() {
    let err = anyhow::anyhow!("Error: tool execution failed: no such file");
    let info = classify_anyhow(&err);
    assert_eq!(info.category, ErrorCategory::Tool);
    assert!(info.recoverable);
}

#[test]
fn auth_error_classification() {
    let err = anyhow::anyhow!("unauthorized: invalid api key");
    let info = classify_anyhow(&err);
    assert_eq!(info.category, ErrorCategory::Auth);
    assert!(!info.recoverable);
}

#[test]
fn rate_limit_classification() {
    let err = anyhow::anyhow!("rate limit exceeded");
    let info = classify_anyhow(&err);
    assert_eq!(info.category, ErrorCategory::RateLimit);
    assert!(info.recoverable);
}
