use crate::tui::markdown::truncate_visual;
use crate::tui::state::TuiState;
use crate::ui::StatsSnapshot;
use crate::util::fmt_k;
use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Style},
    text::{Line, Span},
    widgets::Paragraph,
};

pub(super) fn render_status(f: &mut Frame, area: Rect, state: &TuiState) {
    let status = build_status_line(state, area.width);
    let line = Line::from(Span::styled(status, Style::default().fg(Color::Cyan)));
    f.render_widget(Paragraph::new(line), area);
}

pub(crate) fn build_status_line(state: &TuiState, width: u16) -> String {
    let s = &state.stats;
    let b = if s.belief > 0.0 {
        format!(" B:{:.2}", s.belief)
    } else {
        String::new()
    };
    let ti = s.total_input_tokens + s.total_cache_read_tokens;
    let work = state.work_state.label();
    let status = if width >= 100 {
        format!(
            " {}{b} T:{} R:{} I:{}({}) O:{} C:{}({}) {} [{}]",
            state.model,
            StatsSnapshot::fmt_num(s.current_turn_count),
            StatsSnapshot::fmt_num(s.agent_request_count),
            fmt_k(ti),
            s.cache_pct(),
            fmt_k(s.total_output_tokens),
            fmt_k(s.current_context_tokens),
            s.ctx_pct(),
            s.format_cost(),
            work,
        )
    } else if width >= 64 {
        format!(
            " {}{b} C:{}({}) I:{} O:{} {} [{}]",
            state.model,
            fmt_k(s.current_context_tokens),
            s.ctx_pct(),
            fmt_k(ti),
            fmt_k(s.total_output_tokens),
            s.format_cost(),
            work,
        )
    } else {
        format!(
            " {}{b} C:{} {} [{}]",
            state.model,
            s.ctx_pct(),
            s.format_cost(),
            work,
        )
    };

    truncate_visual(&status, width as usize)
}
