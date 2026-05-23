//! Shared utility functions used across the codebase.

/// Truncate a string to at most `n` bytes on a UTF-8 character boundary,
/// appending "..." if truncation occurred.
pub(crate) fn truncate_str(s: &str, n: usize) -> String {
    if s.len() <= n {
        return s.to_string();
    }
    let mut end = n;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}...", &s[..end])
}

/// Format a token count for display.
/// Examples: 0 → "0", 500 → "500", 1234 → "1.2K", 1234567 → "1.23M"
pub(crate) fn fmt_k(n: u64) -> String {
    if n < 1000 {
        return n.to_string();
    }
    if n >= 1_000_000 {
        let m = n / 1_000_000;
        let rest = n % 1_000_000;
        format!("{}.{:02}M", m, rest / 10_000)
    } else {
        let k = n / 1000;
        let rem = n % 1000;
        format!("{}.{}K", k, rem / 100)
    }
}
