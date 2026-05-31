use super::normalize::strip_ansi;
use crate::tui::theme;
use ratatui::text::{Line, Span};

pub(super) fn is_diff_like(text: &str) -> bool {
    text.lines().take(5).any(|l| {
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
