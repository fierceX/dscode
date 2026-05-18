/// Shared utility functions used across the codebase.

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
