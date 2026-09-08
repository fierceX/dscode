#[cfg(test)]
use crate::tui::markdown::truncate_visual;
use crate::tui::state::TuiState;
use crate::tui::theme;
use crate::ui::PlanTransitionDisplay;
use crate::ui::StatsSnapshot;
use crate::util::fmt_k;
use ratatui::{
    Frame,
    layout::Rect,
    style::Style,
    text::{Line, Span},
    widgets::Paragraph,
};

pub(super) fn render_status(f: &mut Frame, area: Rect, state: &TuiState) {
    let line = build_status_spans(state, area.width);
    f.render_widget(Paragraph::new(line), area);
}

pub(crate) fn build_status_spans(state: &TuiState, width: u16) -> Line<'static> {
    let items = visible_status_items(state, width);
    let mut spans = Vec::new();
    for item in items {
        spans.push(Span::raw(" "));
        spans.push(Span::styled(item.text, item.style));
    }
    Line::from(spans)
}

#[cfg(test)]
pub(crate) fn build_status_line(state: &TuiState, width: u16) -> String {
    let mut out = String::new();
    for item in visible_status_items(state, width) {
        out.push(' ');
        out.push_str(&item.text);
    }
    truncate_visual(&out, width as usize)
}

#[derive(Clone)]
struct StatusItem {
    text: String,
    style: Style,
    priority: u8,
}

fn visible_status_items(state: &TuiState, width: u16) -> Vec<StatusItem> {
    let mut items = build_status_items(state);
    while status_width(&items) > width as usize {
        let Some((idx, _)) = items
            .iter()
            .enumerate()
            .max_by_key(|(_, item)| item.priority)
        else {
            break;
        };
        if items[idx].priority == 0 {
            break;
        }
        items.remove(idx);
    }
    items
}

fn build_status_items(state: &TuiState) -> Vec<StatusItem> {
    let s = &state.stats;
    let ti = s.total_input_tokens + s.total_cache_read_tokens;
    let mut items = vec![StatusItem {
        text: state.model.clone(),
        style: theme::primary_bold(),
        priority: 0,
    }];
    if !state.cwd_label.is_empty() {
        items.push(StatusItem {
            text: format!("@{}", state.cwd_label),
            style: theme::muted(),
            priority: 4,
        });
    }
    if s.belief > 0.0 {
        items.push(StatusItem {
            text: format!("B:{:.2}", s.belief),
            style: theme::muted(),
            priority: 7,
        });
    }
    if let Some(plan) = state.plan.as_ref() {
        let label = match plan.transition {
            PlanTransitionDisplay::DraftSaved => "plan:draft",
            PlanTransitionDisplay::Confirmed => "plan:confirmed",
            PlanTransitionDisplay::DraftCancelled | PlanTransitionDisplay::Cleared => "plan:none",
        };
        items.push(StatusItem {
            text: label.to_string(),
            style: theme::info(),
            priority: 6,
        });
    }
    if let Some(todos) = state.todos.as_ref()
        && todos.counts.in_progress + todos.counts.pending > 0
    {
        items.push(StatusItem {
            text: format!("todo:{}/{}", todos.counts.in_progress, todos.counts.pending),
            style: theme::info(),
            priority: 6,
        });
    }
    items.push(StatusItem {
        text: format!("T:{}", StatsSnapshot::fmt_num(s.current_turn_count)),
        style: theme::muted(),
        priority: 8,
    });
    items.push(StatusItem {
        text: format!("R:{}", StatsSnapshot::fmt_num(s.agent_request_count)),
        style: theme::muted(),
        priority: 8,
    });
    items.push(StatusItem {
        text: format!("I:{}({})", fmt_k(ti), s.cache_pct()),
        style: theme::muted(),
        priority: 5,
    });
    items.push(StatusItem {
        text: format!("O:{}", fmt_k(s.total_output_tokens)),
        style: theme::muted(),
        priority: 5,
    });
    items.push(StatusItem {
        text: format!("C:{}({})", fmt_k(s.current_context_tokens), s.ctx_pct()),
        style: theme::muted(),
        priority: 2,
    });
    // 流式等待心跳的精简状态标签（如 `·30s`），渲染宽度不足时优先被裁剪。
    if let Some(status) = state.stream_status.as_deref() {
        items.push(StatusItem {
            text: status.to_string(),
            style: theme::info(),
            priority: 9,
        });
    }
    items.push(StatusItem {
        text: format!("[{}]", state.work_state.label()),
        style: theme::work_state(state.work_state),
        priority: 1,
    });
    items
}

fn status_width(items: &[StatusItem]) -> usize {
    items
        .iter()
        .map(|item| 1 + unicode_width::UnicodeWidthStr::width(item.text.as_str()))
        .sum()
}
