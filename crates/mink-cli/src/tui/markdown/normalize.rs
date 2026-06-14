pub(crate) fn normalize_markdown_input(input: &str, preserve_ansi: bool) -> String {
    if preserve_ansi {
        crate::tui::sanitize::normalize_tui_input(input)
    } else {
        crate::tui::sanitize::sanitize_tui_text(input)
    }
}

pub(crate) fn strip_ansi(s: &str) -> String {
    crate::tui::sanitize::strip_ansi(s)
}
