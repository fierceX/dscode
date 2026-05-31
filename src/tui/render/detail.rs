use crate::tui::markdown::render_md_with_tables;
use crate::tui::state::{MsgKind, TuiState};
use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Modifier, Style},
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
    let all_lines = detail_lines_for_session(state, session_id);

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
        .border_style(Style::default().fg(Color::DarkGray));

    let paragraph = Paragraph::new(Text::from(visible)).block(block);
    f.render_widget(paragraph, area);
}

pub(crate) fn detail_lines_for_session(state: &TuiState, session_id: &str) -> Vec<Line<'static>> {
    let Some(line_idx) = state.sub_agents.line_by_session.get(session_id).copied() else {
        return vec![Line::from(Span::styled(
            format!("Sub-agent {session_id} is no longer available."),
            Style::default().fg(Color::DarkGray),
        ))];
    };
    let Some(line) = state
        .lines
        .get(line_idx)
        .filter(|line| line.kind == MsgKind::SubAgent)
    else {
        return vec![Line::from(Span::styled(
            format!("Sub-agent {session_id} has no detail line."),
            Style::default().fg(Color::DarkGray),
        ))];
    };
    let detail = line.sub_detail.as_ref();
    let thinking = detail.map(|d| d.thinking.as_str()).unwrap_or("");
    let text = detail.map(|d| d.text.as_str()).unwrap_or("");
    let mut all_lines: Vec<Line<'static>> = Vec::new();
    all_lines.push(Line::from(Span::styled(
        line.text.clone(),
        Style::default()
            .fg(Color::Magenta)
            .add_modifier(Modifier::BOLD),
    )));
    all_lines.push(Line::from(""));

    if !thinking.is_empty() {
        all_lines.push(Line::from(Span::styled(
            "── Thinking ──",
            Style::default().fg(Color::Rgb(139, 139, 139)),
        )));
        for raw in thinking.split('\n') {
            all_lines.push(Line::from(Span::styled(
                raw.to_string(),
                Style::default().fg(Color::Rgb(139, 139, 139)),
            )));
        }
        all_lines.push(Line::from(""));
    }

    if !text.is_empty() {
        all_lines.push(Line::from(Span::styled(
            "── Text ──",
            Style::default().fg(Color::White),
        )));
        render_md_with_tables(&mut all_lines, text);
    }

    if all_lines.is_empty() {
        all_lines.push(Line::from(Span::styled(
            "(no output)",
            Style::default().fg(Color::DarkGray),
        )));
    }
    all_lines
}

pub(super) fn render_detail_bar(f: &mut Frame, area: Rect) {
    let text = Span::styled(
        " Esc: Back │ ↑↓ PgUp/PgDn: Scroll ",
        Style::default().fg(Color::Yellow),
    );
    f.render_widget(Paragraph::new(Line::from(text)), area);
}

pub(crate) fn detail_viewport_height(area_height: u16, show_borders: bool) -> usize {
    let border_rows = if show_borders { 2 } else { 0 };
    area_height.saturating_sub(border_rows) as usize
}
