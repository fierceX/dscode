use crate::tui::theme;
use ratatui::{
    Frame,
    layout::Rect,
    text::Text,
    widgets::{Block, Borders, Paragraph},
};

pub(super) fn render_input(f: &mut Frame, area: Rect, vis_lines: &[String]) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme::border());

    let inner = block.inner(area);
    f.render_widget(block, area);
    let text = Text::from(vis_lines.join("\n"));
    f.render_widget(Paragraph::new(text), inner);
}

/// Queued clipboard images above the input box.
pub(super) fn render_chips(f: &mut Frame, area: Rect, lines: &[String]) {
    let text = Text::from(lines.join("\n"));
    f.render_widget(Paragraph::new(text).style(theme::muted()), area);
}

/// Chip row content for the queued clipboard images: one line per terminal
/// row, truncated to `max_lines` with a trailing ellipsis.
pub(crate) fn chip_lines(
    images: &[crate::tui::state::PendingImage],
    width: usize,
    max_lines: usize,
) -> Vec<String> {
    if images.is_empty() || max_lines == 0 {
        return Vec::new();
    }
    let joined = images
        .iter()
        .enumerate()
        .map(|(index, image)| image.chip(index + 1))
        .collect::<Vec<_>>()
        .join(" ");
    let mut lines = split_at_visual_width(&joined, width.max(1));
    if lines.len() > max_lines {
        lines.truncate(max_lines);
        if let Some(last) = lines.last_mut() {
            // Make room for the ellipsis so the line still fits the width.
            if unicode_width::UnicodeWidthStr::width(last.as_str()) >= width.max(1) {
                last.pop();
            }
            last.push('…');
        }
    }
    lines
}

pub(crate) fn split_at_visual_width(s: &str, max_width: usize) -> Vec<String> {
    if s.is_empty() {
        return vec![String::new()];
    }
    let max_width = max_width.max(1);
    let mut lines: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut cur_w = 0usize;
    for ch in s.chars() {
        if ch == '\n' {
            lines.push(std::mem::take(&mut cur));
            cur_w = 0;
            continue;
        }
        let cw = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
        if cur_w + cw > max_width && !cur.is_empty() {
            lines.push(cur);
            cur = String::new();
            cur_w = 0;
        }
        cur.push(ch);
        cur_w += cw;
    }
    if !cur.is_empty() || s.ends_with('\n') {
        lines.push(cur);
    }
    lines
}

pub(crate) fn clamp_input_scroll(
    total_rows: usize,
    cursor_row: usize,
    visible_rows: usize,
    current_scroll: usize,
) -> usize {
    if total_rows == 0 || visible_rows == 0 {
        return 0;
    }

    let max_scroll = total_rows.saturating_sub(visible_rows);
    let scroll = current_scroll.min(max_scroll);
    if cursor_row < scroll {
        cursor_row
    } else if cursor_row >= scroll + visible_rows {
        cursor_row + 1 - visible_rows
    } else {
        scroll
    }
}
