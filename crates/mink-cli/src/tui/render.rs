use crate::config::TuiMode;
use crate::tui::render::content::{ContentMode, render_content};
use crate::tui::render::detail::{
    render_artifact_content, render_detail_bar, render_detail_content, render_plan_content,
    render_todos_content,
};
use crate::tui::render::file_picker::render_file_picker;
use crate::tui::render::input::render_input;
use crate::tui::render::status::render_status;
use crate::tui::state::{TuiState, View};
use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
};

mod content;
mod detail;
mod file_picker;
mod input;
mod status;

pub(crate) use content::transcript_item_lines;
#[cfg(test)]
pub(crate) use content::{collapsed_summary, content_viewport_height, visible_lines};
#[cfg(test)]
pub(crate) use detail::{detail_lines_for_session, detail_viewport_height};
pub(crate) use input::{clamp_input_scroll, split_at_visual_width};
#[cfg(test)]
pub(crate) use status::{build_status_line, build_status_spans};

pub(crate) fn render(f: &mut Frame, state: &mut TuiState, mode: TuiMode) {
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
            // 输入框正文最大行数受视口高度约束：正文行 + 2 行边框之外，
            // 布局还需要至少 1 行内容区和 1 行状态栏。主视图入口保证
            // `area.height >= 5`，因此小视口（inline 最小 8 行）下不会把
            // 输入框压缩到越界，导致最后一行被裁剪、光标落到边框上。
            let max_content_lines = usize::from(area.height).saturating_sub(4).clamp(1, 5);
            let content_lines = vis_lines.len().clamp(1, max_content_lines);
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

            render_content(
                f,
                chunks[0],
                state,
                if mode == TuiMode::Full {
                    ContentMode::Full
                } else {
                    ContentMode::Inline
                },
            );
            render_input(f, chunks[1], &visible_input_lines);
            render_file_picker(f, chunks[1], state);

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
            render_detail_content(f, chunks[0], session_id, *scroll, state);
            render_detail_bar(f, chunks[1]);
        }
        View::Plan { scroll } => {
            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Min(1), Constraint::Length(1)])
                .split(area);
            render_plan_content(f, chunks[0], *scroll, state);
            render_detail_bar(f, chunks[1]);
        }
        View::Todos { scroll } => {
            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Min(1), Constraint::Length(1)])
                .split(area);
            render_todos_content(f, chunks[0], *scroll, state);
            render_detail_bar(f, chunks[1]);
        }
        View::Artifact { scroll } => {
            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Min(1), Constraint::Length(1)])
                .split(area);
            render_artifact_content(f, chunks[0], *scroll, state);
            render_detail_bar(f, chunks[1]);
        }
    }
}

pub(super) fn padded_content_area(area: Rect) -> Rect {
    let horizontal = if area.width > 2 { 1 } else { 0 };
    let bottom = if area.height > 1 { 1 } else { 0 };
    Rect {
        x: area.x.saturating_add(horizontal),
        y: area.y,
        width: area.width.saturating_sub(horizontal * 2),
        height: area.height.saturating_sub(bottom),
    }
}
