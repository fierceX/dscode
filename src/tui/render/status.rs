use crate::tui::markdown::truncate_visual;
use crate::tui::state::TuiState;
use crate::tui::theme;
use crate::ui::StatsSnapshot;
use crate::util::fmt_k;
use ratatui::{
    Frame,
    layout::Rect,
    text::{Line, Span},
    widgets::Paragraph,
};

pub(super) fn render_status(f: &mut Frame, area: Rect, state: &TuiState) {
    let line = build_status_spans(state, area.width);
    f.render_widget(Paragraph::new(line), area);
}

pub(crate) fn build_status_spans(state: &TuiState, width: u16) -> Line<'static> {
    let status = build_status_line(state, width);
    let clipped = status.ends_with('…');
    let work = state.work_state.label();
    let work_marker = format!("[{work}]");

    let mut spans = Vec::new();
    let mut remaining = status.as_str();
    if let Some(prefix) = remaining.strip_prefix(' ') {
        spans.push(Span::raw(" "));
        remaining = prefix;
    }

    if let Some(rest) = remaining.strip_prefix(&state.model) {
        spans.push(Span::styled(state.model.clone(), theme::primary_bold()));
        remaining = rest;
    }

    if !clipped && !state.cwd_label.is_empty() {
        let path_marker = format!(" @{}", state.cwd_label);
        if let Some(rest) = remaining.strip_prefix(&path_marker) {
            spans.push(Span::styled(path_marker, theme::muted()));
            remaining = rest;
        }
    }

    if !clipped && let Some(before_work) = remaining.strip_suffix(&work_marker) {
        if !before_work.is_empty() {
            spans.push(Span::styled(before_work.to_string(), theme::muted()));
        }
        spans.push(Span::styled(
            work_marker,
            theme::work_state(state.work_state),
        ));
    } else if !remaining.is_empty() {
        spans.push(Span::styled(remaining.to_string(), theme::muted()));
    }

    Line::from(spans)
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
    let cwd = if state.cwd_label.is_empty() {
        String::new()
    } else {
        format!(" @{}", state.cwd_label)
    };
    let status = if width >= 100 {
        format!(
            " {}{cwd}{b} T:{} R:{} I:{}({}) O:{} C:{}({}) {} [{}]",
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
            " {}{cwd}{b} C:{}({}) I:{} O:{} {} [{}]",
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
