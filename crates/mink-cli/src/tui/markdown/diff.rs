use super::normalize::strip_ansi;
use crate::tui::theme;
use ratatui::text::{Line, Span};

/// Detect whether the text looks like a unified diff.
///
/// This is a best-effort content heuristic. It is only called for
/// tool results from [`is_diff_eligible`](super::is_diff_eligible) tools
/// (Edit, Bash, Python, PythonSandbox), so false positives on raw file or
/// structured output are already excluded at the caller level. The entire
/// text is scanned because the diff section may appear after a tool-specific
/// header (e.g. Edit's post-edit snapshot).
pub(super) fn is_diff_like(text: &str) -> bool {
    text.lines().any(|l| {
        let t = l.trim();
        t.starts_with("diff --git")
            || t.starts_with("--- ")
            || t.starts_with("+++ ")
            || t.starts_with("@@")
    })
}

pub(super) fn render_diff(lines: &mut Vec<Line<'static>>, text: &str) {
    for raw in text.split('\n') {
        let clean = strip_ansi(raw);
        let trimmed = clean.trim();
        let style = if trimmed.starts_with("--- ") || trimmed.starts_with("+++ ") {
            theme::diff_header()
        } else if trimmed.starts_with("@@") {
            theme::secondary()
        } else if clean.starts_with('-') && !clean.starts_with("---") {
            theme::diff_remove()
        } else if clean.starts_with('+') && !clean.starts_with("+++") {
            theme::success()
        } else {
            theme::muted()
        };
        lines.push(Line::from(Span::styled(clean, style)));
    }
}
