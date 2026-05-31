use super::normalize::strip_ansi;
use ratatui::{
    style::{Color, Modifier, Style},
    text::{Line, Span},
};

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
    let gray = Style::default().fg(Color::Rgb(100, 100, 100));
    let red = Style::default().fg(Color::Rgb(255, 100, 100));
    let green = Style::default().fg(Color::Rgb(100, 200, 100));
    let cyan = Style::default().fg(Color::Cyan);
    let yellow = Style::default()
        .fg(Color::Yellow)
        .add_modifier(Modifier::BOLD);

    for raw in text.split('\n') {
        let clean = strip_ansi(raw);
        let trimmed = clean.trim();
        let style = if trimmed.starts_with("--- ") || trimmed.starts_with("+++ ") {
            yellow
        } else if trimmed.starts_with("@@") {
            cyan
        } else if clean.starts_with('-') && !clean.starts_with("---") {
            red
        } else if clean.starts_with('+') && !clean.starts_with("+++") {
            green
        } else {
            gray
        };
        lines.push(Line::from(Span::styled(clean, style)));
    }
}
