use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Style},
    text::Text,
    widgets::{Block, Borders, Paragraph},
};

pub(super) fn render_input(f: &mut Frame, area: Rect, vis_lines: &[String]) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::DarkGray));

    let inner = block.inner(area);
    f.render_widget(block, area);
    let text = Text::from(vis_lines.join("\n"));
    f.render_widget(Paragraph::new(text), inner);
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
