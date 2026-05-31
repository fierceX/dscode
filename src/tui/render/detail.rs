use crate::tui::markdown::render_md_with_tables_with_width;
use crate::tui::state::{MsgKind, TuiState};
use crate::tui::theme;
use ratatui::{
    Frame,
    layout::Rect,
    text::{Line, Span, Text},
    widgets::{Block, Borders, Paragraph},
};

pub(super) fn render_detail_content(
    f: &mut Frame,
    area: Rect,
    session_id: &str,
    scroll: usize,
    show_borders: bool,
    state: &TuiState,
) {
    let inner_w = area
        .width
        .saturating_sub(if show_borders { 2 } else { 0 })
        .max(1);
    let all_lines = detail_lines_for_session_with_width(state, session_id, inner_w);

    let viewport = detail_viewport_height(area.height, show_borders);
    let max_scroll = all_lines.len().saturating_sub(viewport);
    let effective_scroll = scroll.min(max_scroll);
    let visible: Vec<Line<'static>> = all_lines
        .iter()
        .skip(effective_scroll)
        .take(viewport)
        .cloned()
        .collect();

    let borders = if show_borders {
        Borders::ALL
    } else {
        Borders::NONE
    };
    let block = Block::default()
        .borders(borders)
        .border_style(theme::border());

    let paragraph = Paragraph::new(Text::from(visible)).block(block);
    f.render_widget(paragraph, area);
}

#[cfg(test)]
pub(crate) fn detail_lines_for_session(state: &TuiState, session_id: &str) -> Vec<Line<'static>> {
    detail_lines_for_session_with_width(state, session_id, 80)
}

pub(crate) fn detail_lines_for_session_with_width(
    state: &TuiState,
    session_id: &str,
    max_width: u16,
) -> Vec<Line<'static>> {
    let Some(line_idx) = state.sub_agents.line_by_session.get(session_id).copied() else {
        return vec![Line::from(Span::styled(
            format!("Sub-agent {session_id} is no longer available."),
            theme::muted(),
        ))];
    };
    let Some(line) = state
        .lines
        .get(line_idx)
        .filter(|line| line.kind == MsgKind::SubAgent)
    else {
        return vec![Line::from(Span::styled(
            format!("Sub-agent {session_id} has no detail line."),
            theme::muted(),
        ))];
    };
    let detail = line.sub_detail.as_ref();
    let thinking = detail.map(|d| d.thinking.as_str()).unwrap_or("");
    let text = detail.map(|d| d.text.as_str()).unwrap_or("");
    let mut all_lines: Vec<Line<'static>> = Vec::new();
    all_lines.push(Line::from(Span::styled(
        line.text.clone(),
        theme::sub_agent(),
    )));
    all_lines.push(Line::from(""));

    if !thinking.is_empty() {
        all_lines.push(Line::from(Span::styled("── Thinking ──", theme::muted())));
        for raw in thinking.split('\n') {
            all_lines.push(Line::from(Span::styled(raw.to_string(), theme::muted())));
        }
        all_lines.push(Line::from(""));
    }

    if !text.is_empty() {
        all_lines.push(Line::from(Span::styled(
            "── Text ──",
            theme::primary_bold(),
        )));
        render_md_with_tables_with_width(&mut all_lines, text, max_width);
    }

    if all_lines.is_empty() {
        all_lines.push(Line::from(Span::styled("(no output)", theme::muted())));
    }
    all_lines
}

pub(super) fn render_detail_bar(f: &mut Frame, area: Rect) {
    let text = Span::styled(" Esc: Back │ ↑↓ PgUp/PgDn: Scroll ", theme::info());
    f.render_widget(Paragraph::new(Line::from(text)), area);
}

pub(crate) fn detail_viewport_height(area_height: u16, show_borders: bool) -> usize {
    let border_rows = if show_borders { 2 } else { 0 };
    area_height.saturating_sub(border_rows) as usize
}
