/// Truncate a string to at most `limit` bytes on a UTF-8 character boundary.
pub(crate) fn truncate_str(value: &str, limit: usize) -> String {
    if value.len() <= limit {
        return value.to_string();
    }
    let mut end = limit;
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}...", &value[..end])
}
