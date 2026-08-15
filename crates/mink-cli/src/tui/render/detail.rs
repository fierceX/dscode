use crate::tui::markdown::{render_md_with_tables_with_width, wrap_lines_word};
use crate::tui::render::padded_content_area;
use crate::tui::state::{TranscriptKind, TuiState};
use crate::tui::theme;
use crate::ui::{PlanTransitionDisplay, TodoStatusDisplay};
use ratatui::{
    Frame,
    layout::Rect,
    text::{Line, Span, Text},
    widgets::Paragraph,
};

pub(super) fn render_detail_content(
    f: &mut Frame,
    area: Rect,
    session_id: &str,
    scroll: usize,
    state: &TuiState,
) {
    let content_area = padded_content_area(area);
    let inner_w = content_area.width.max(1);
    let all_lines = detail_lines_for_session_with_width(state, session_id, inner_w);

    let viewport = detail_viewport_height(area.height);
    let max_scroll = all_lines.len().saturating_sub(viewport);
    let effective_scroll = scroll.min(max_scroll);
    let visible: Vec<Line<'static>> = all_lines
        .iter()
        .skip(effective_scroll)
        .take(viewport)
        .cloned()
        .collect();

    let paragraph = Paragraph::new(Text::from(visible));
    f.render_widget(paragraph, content_area);
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
        .filter(|line| line.kind == TranscriptKind::SubAgent)
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

pub(super) fn render_plan_content(f: &mut Frame, area: Rect, scroll: usize, state: &TuiState) {
    let inner_w = padded_content_area(area).width.max(1);
    let mut lines = Vec::new();
    if let Some(plan) = state.plan.as_ref() {
        let status = match plan.transition {
            PlanTransitionDisplay::DraftSaved => "Plan draft · awaiting confirmation",
            PlanTransitionDisplay::DraftCancelled => "Plan draft cancelled",
            PlanTransitionDisplay::Confirmed => "Plan confirmed",
            PlanTransitionDisplay::Cleared => "No active plan",
        };
        lines.push(Line::from(Span::styled(status, theme::primary_bold())));
        lines.push(Line::default());
        if let Some(content) = plan.content.as_deref() {
            render_md_with_tables_with_width(&mut lines, content, inner_w);
        }
    } else {
        lines.push(Line::from(Span::styled(
            "No plan state is available in this session.",
            theme::muted(),
        )));
    }
    render_detail_lines(f, area, scroll, lines);
}

pub(super) fn render_todos_content(f: &mut Frame, area: Rect, scroll: usize, state: &TuiState) {
    let mut lines = Vec::new();
    if let Some(todos) = state.todos.as_ref() {
        lines.push(Line::from(Span::styled(
            format!(
                "Todos r{} · {} active · {} pending · {} completed",
                todos.revision,
                todos.counts.in_progress,
                todos.counts.pending,
                todos.counts.completed
            ),
            theme::primary_bold(),
        )));
        lines.push(Line::default());
        for item in &todos.items {
            let marker = match item.status {
                TodoStatusDisplay::Pending => "○",
                TodoStatusDisplay::InProgress => "◉",
                TodoStatusDisplay::Completed => "✓",
            };
            lines.push(Line::from(format!(
                "{marker} {}  {}",
                item.id, item.content
            )));
        }
    } else {
        lines.push(Line::from(Span::styled(
            "No todo state is available in this session.",
            theme::muted(),
        )));
    }
    render_detail_lines(f, area, scroll, lines);
}

pub(super) fn render_artifact_content(f: &mut Frame, area: Rect, scroll: usize, state: &TuiState) {
    let mut lines = Vec::new();
    if let Some(artifact) = state.artifact_detail.as_ref() {
        lines.push(Line::from(Span::styled(
            format!("artifact://{}", artifact.id),
            theme::primary_bold(),
        )));
        if artifact.truncated {
            lines.push(Line::from(Span::styled(
                "Showing the first 256 KiB. Use Read with a selector for later sections.",
                theme::info(),
            )));
        }
        lines.push(Line::default());
        for raw in artifact.content.lines() {
            lines.push(Line::from(raw.to_string()));
        }
    }
    render_detail_lines(f, area, scroll, lines);
}

fn render_detail_lines(f: &mut Frame, area: Rect, scroll: usize, lines: Vec<Line<'static>>) {
    let content_area = padded_content_area(area);
    let lines = wrap_lines_word(&lines, content_area.width.max(1));
    let viewport = content_area.height as usize;
    let max_scroll = lines.len().saturating_sub(viewport);
    let visible = lines
        .into_iter()
        .skip(scroll.min(max_scroll))
        .take(viewport)
        .collect::<Vec<_>>();
    f.render_widget(Paragraph::new(Text::from(visible)), content_area);
}

pub(crate) fn detail_viewport_height(area_height: u16) -> usize {
    padded_content_area(Rect {
        x: 0,
        y: 0,
        width: 1,
        height: area_height,
    })
    .height as usize
}

#[cfg(test)]
#[path = "detail_tests.rs"]
mod tests;
