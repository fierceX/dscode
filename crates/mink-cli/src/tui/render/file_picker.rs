use crate::tui::markdown::truncate_visual;
use crate::tui::state::{ActiveOverlay, TuiState};
use crate::tui::theme;
use ratatui::{
    Frame,
    layout::Rect,
    style::Modifier,
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
};

const MAX_PICKER_ROWS: usize = 8;

pub(super) fn render_file_picker(f: &mut Frame, input_area: Rect, state: &mut TuiState) {
    let Some(ActiveOverlay::FilePicker(picker)) = state.overlay.as_mut() else {
        return;
    };
    let visible_rows = picker.items.len().clamp(1, MAX_PICKER_ROWS);
    picker.clamp_scroll(visible_rows);

    let height = (visible_rows + 2) as u16;
    let y = input_area.y.saturating_sub(height);
    let width = input_area.width.saturating_sub(2).max(20);
    let x = input_area.x.saturating_add(1);
    let area = Rect {
        x,
        y,
        width: width.min(input_area.width),
        height,
    };
    if area.height < 3 || area.width < 10 {
        return;
    }

    let inner_w = area.width.saturating_sub(2) as usize;
    let mut lines = Vec::new();
    if picker.items.is_empty() {
        lines.push(Line::from(Span::styled(
            "No matching files",
            theme::muted(),
        )));
    } else {
        for (row, item) in picker
            .items
            .iter()
            .enumerate()
            .skip(picker.scroll)
            .take(visible_rows)
        {
            let selected = row == picker.selected;
            let marker = if selected { ">" } else { " " };
            let suffix = if item.is_dir && !item.path.ends_with('/') {
                "/"
            } else {
                ""
            };
            let label = truncate_visual(&format!("{marker} {}{suffix}", item.path), inner_w);
            let style = if selected {
                theme::primary_bold().add_modifier(Modifier::REVERSED)
            } else if item.is_dir {
                theme::info()
            } else {
                theme::text()
            };
            lines.push(Line::from(Span::styled(label, style)));
        }
    }

    let title = truncate_visual(&format!(" files: {} ", picker.query), inner_w);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme::border())
        .title(Span::styled(title, theme::muted()));
    f.render_widget(Paragraph::new(lines).block(block), area);
}
