use crate::tui::render::content::render_content;
use crate::tui::render::detail::{render_detail_bar, render_detail_content};
use crate::tui::render::input::render_input;
use crate::tui::render::status::render_status;
use crate::tui::state::{TuiState, View};
use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout},
};

mod content;
mod detail;
mod input;
mod status;

#[cfg(test)]
pub(crate) use content::{
    build_visible_click_map, collapsed_summary, content_viewport_height, visible_lines,
};
#[cfg(test)]
pub(crate) use detail::{detail_lines_for_session, detail_viewport_height};
pub(crate) use input::{clamp_input_scroll, split_at_visual_width};
#[cfg(test)]
pub(crate) use status::build_status_line;

pub(crate) fn render(f: &mut Frame, state: &mut TuiState) {
    let area = f.area();
    if area.height < 5 || area.width < 20 {
        return;
    }

    let view = state.view.clone();
    match &view {
        View::Main => {
            state.input.clamp_cursor();
            let inner_w = area.width.saturating_sub(2).max(1) as usize;
            let vis_lines = split_at_visual_width(&state.input.buf, inner_w);
            let cursor = state.input.clamped_cursor();
            let lines_before = split_at_visual_width(&state.input.buf[..cursor], inner_w);
            let cursor_row = lines_before.len().saturating_sub(1);
            let content_lines = vis_lines.len().clamp(1, 5);
            state.input.scroll_row = clamp_input_scroll(
                vis_lines.len(),
                cursor_row,
                content_lines,
                state.input.scroll_row,
            );
            let visible_input_lines: Vec<String> = vis_lines
                .iter()
                .skip(state.input.scroll_row)
                .take(content_lines)
                .cloned()
                .collect();
            let input_height = content_lines + 2;

            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Min(1),
                    Constraint::Length(input_height as u16),
                    Constraint::Length(1),
                ])
                .split(area);

            state.viewport.content_y = chunks[0].y;
            render_content(f, chunks[0], state);
            render_input(f, chunks[1], &visible_input_lines);

            let row = cursor_row.saturating_sub(state.input.scroll_row);
            let col = lines_before.last().map_or(0, |line| {
                unicode_width::UnicodeWidthStr::width(line.as_str())
            });
            let cursor_x = (chunks[1].x + 1 + col as u16).min(chunks[1].right().saturating_sub(2));
            let cursor_y = (chunks[1].y + 1 + row as u16).min(chunks[1].bottom().saturating_sub(2));
            f.set_cursor_position((cursor_x, cursor_y));

            render_status(f, chunks[2], state);
        }
        View::SubAgentDetail { session_id, scroll } => {
            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Min(1), Constraint::Length(1)])
                .split(area);
            render_detail_content(
                f,
                chunks[0],
                session_id,
                *scroll,
                state.viewport.show_borders,
                state,
            );
            render_detail_bar(f, chunks[1]);
        }
    }
}
