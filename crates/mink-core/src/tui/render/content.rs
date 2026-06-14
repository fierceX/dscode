use crate::tui::markdown::{push_msg_with_width, truncate_visual, wrap_lines_word};
use crate::tui::state::{ClickAction, ClickTarget, MsgKind, MsgLine, TuiState};
use crate::tui::theme;
use ratatui::{
    Frame,
    layout::Rect,
    text::{Line, Span, Text},
    widgets::{Block, Borders, Paragraph},
};

pub(super) fn render_content(f: &mut Frame, area: Rect, state: &mut TuiState) {
    let inner_w = area.width.saturating_sub(2).max(1);
    let width_changed = state.cache.width != inner_w;
    state.cache.width = inner_w;

    let mut need_rebuild = width_changed || state.cache.history_lines.is_none();
    for msg in state.lines.iter_mut() {
        if width_changed || !msg.cache_valid() {
            rebuild_message_cache(msg, inner_w);
            need_rebuild = true;
        }
    }

    if need_rebuild {
        let mut all_lines: Vec<Line<'static>> = Vec::new();
        for msg in state.lines.iter_mut() {
            if msg.cached_lines.is_none() {
                rebuild_message_cache(msg, inner_w);
            }
            if let Some(cached) = msg.cached_lines.as_ref() {
                all_lines.extend(cached.clone());
            }
        }
        state.cache.history_lines = Some(all_lines);
    }

    ensure_stream_cache(state, inner_w);
    let history_len = state.cache.history_lines.as_ref().map_or(0, Vec::len);
    let stream_len = state.cache.stream_lines.as_ref().map_or(0, Vec::len);
    let total_len = history_len + stream_len;
    let viewport = content_viewport_height(area.height, state.viewport.show_borders);
    let max_scroll = total_len.saturating_sub(viewport);
    state.viewport.max_scroll = max_scroll;

    let scroll = if state.viewport.auto_scroll {
        max_scroll
    } else {
        state.viewport.scroll.min(max_scroll)
    };
    state.viewport.effective_scroll = scroll;
    state.viewport.click_map = build_visible_click_map(state, scroll, viewport);

    let border_style = if state.streaming {
        theme::streaming_border()
    } else {
        theme::border()
    };
    let borders = if state.viewport.show_borders {
        Borders::ALL
    } else {
        Borders::NONE
    };
    let mut block = Block::default().borders(borders).border_style(border_style);

    let mut title_parts: Vec<Span<'static>> = Vec::new();
    if scroll > 0 {
        title_parts.push(Span::styled(" ↥ ", theme::info()));
    }
    if scroll < max_scroll {
        title_parts.push(Span::styled(" ↧ ", theme::info()));
    }
    if !title_parts.is_empty() {
        block = block.title_bottom(Line::from(title_parts));
    }

    let visible = visible_lines(
        state.cache.history_lines.as_deref().unwrap_or(&[]),
        state.cache.stream_lines.as_deref().unwrap_or(&[]),
        scroll,
        viewport,
    );

    let paragraph = Paragraph::new(Text::from(visible)).block(block);
    f.render_widget(paragraph, area);
}

fn rebuild_message_cache(msg: &mut MsgLine, inner_w: u16) {
    let mut seg = build_message_segments(msg, inner_w);
    let mut wrapped = wrap_lines_word(&seg, inner_w);
    if !msg.collapsed
        && !msg.collapse_overridden
        && msg.collapse_policy.should_collapse_rendered(wrapped.len())
    {
        msg.collapsed = true;
        seg = build_message_segments(msg, inner_w);
        wrapped = wrap_lines_word(&seg, inner_w);
    }
    msg.cached_lines = Some(wrapped);
    msg.cached_collapsed = msg.collapsed;
}

fn build_message_segments(msg: &MsgLine, inner_w: u16) -> Vec<Line<'static>> {
    let mut seg = Vec::new();
    if msg.collapsed {
        let max_w = (inner_w as usize).saturating_sub(4).max(1);
        push_msg_with_width(&mut seg, &collapsed_summary(msg, max_w), msg.kind, inner_w);
    } else if msg.is_collapsible() {
        push_msg_with_width(&mut seg, &format!("▼ {}", msg.text), msg.kind, inner_w);
    } else {
        push_msg_with_width(&mut seg, &msg.text, msg.kind, inner_w);
    }
    seg
}

fn click_action_for_message(
    state: &TuiState,
    line_idx: usize,
    msg: &MsgLine,
) -> Option<ClickAction> {
    match msg.kind {
        _ if msg.is_collapsible() => Some(ClickAction::ToggleCollapse),
        MsgKind::SubAgent => state
            .sub_agents
            .session_for_line(line_idx)
            .map(|session_id| ClickAction::OpenSubAgentDetail {
                session_id: session_id.to_string(),
            }),
        _ => None,
    }
}

pub(crate) fn build_visible_click_map(
    state: &TuiState,
    scroll: usize,
    viewport: usize,
) -> Vec<ClickTarget> {
    if viewport == 0 {
        return Vec::new();
    }

    let visible_start = scroll;
    let visible_end = scroll.saturating_add(viewport).saturating_sub(1);
    let mut out = Vec::new();
    let mut current_row = 0usize;

    for (idx, msg) in state.lines.iter().enumerate() {
        let Some(cached) = msg.cached_lines.as_ref() else {
            continue;
        };
        let phys = cached.len();
        if phys == 0 {
            continue;
        }

        let start = current_row;
        let end = current_row + phys.saturating_sub(1);
        current_row += phys;

        if end < visible_start || start > visible_end {
            continue;
        }

        if let Some(action) = click_action_for_message(state, idx, msg) {
            out.push(ClickTarget {
                line_idx: idx,
                start_row: start.saturating_sub(scroll),
                end_row: end.min(visible_end).saturating_sub(scroll),
                action,
            });
        }
    }

    out
}

pub(crate) fn collapsed_summary(msg: &MsgLine, max_w: usize) -> String {
    let line_count = msg.text.lines().count();
    let first = msg
        .text
        .lines()
        .find(|line| !line.trim().is_empty())
        .unwrap_or("")
        .trim();
    let prefix = match msg.kind {
        MsgKind::ToolResult => tool_result_summary_prefix(msg, line_count),
        MsgKind::StreamThinking => "► thinking | ".to_string(),
        _ => "► ".to_string(),
    };
    let available = max_w.saturating_sub(unicode_width::UnicodeWidthStr::width(prefix.as_str()));
    format!("{prefix}{}", truncate_visual(first, available))
}

fn tool_result_summary_prefix(msg: &MsgLine, line_count: usize) -> String {
    let tool = msg.tool_name.as_deref().unwrap_or("tool");
    let bytes = msg.text.len();
    let truncated = if msg.text.contains("[... truncated:") {
        ", truncated"
    } else {
        ""
    };
    format!("► {tool}: {line_count} lines, {bytes} bytes{truncated} | ")
}

fn ensure_stream_cache(state: &mut TuiState, inner_w: u16) {
    if !state.streaming || state.stream_line.is_empty() {
        state.invalidate_stream_cache();
        return;
    }
    let cache_valid = state.cache.stream_lines.is_some()
        && state.cache.stream_width == inner_w
        && state.cache.stream_kind == state.stream_kind
        && state.cache.stream_revision == state.stream_revision;
    if cache_valid {
        return;
    }

    let mut seg: Vec<Line<'static>> = Vec::new();
    push_msg_with_width(&mut seg, &state.stream_line, state.stream_kind, inner_w);
    state.cache.stream_lines = Some(wrap_lines_word(&seg, inner_w));
    state.cache.stream_width = inner_w;
    state.cache.stream_kind = state.stream_kind;
    state.cache.stream_revision = state.stream_revision;
}

pub(crate) fn visible_lines(
    history: &[Line<'static>],
    stream: &[Line<'static>],
    scroll: usize,
    viewport: usize,
) -> Vec<Line<'static>> {
    let mut visible = Vec::with_capacity(viewport.min(history.len() + stream.len()));
    if viewport == 0 {
        return visible;
    }

    if scroll < history.len() {
        let take = viewport.min(history.len() - scroll);
        visible.extend(history.iter().skip(scroll).take(take).cloned());
    }

    if visible.len() < viewport {
        let stream_skip = scroll.saturating_sub(history.len());
        let remaining = viewport - visible.len();
        visible.extend(stream.iter().skip(stream_skip).take(remaining).cloned());
    }

    visible
}

pub(crate) fn content_viewport_height(area_height: u16, show_borders: bool) -> usize {
    let border_rows = if show_borders { 2 } else { 0 };
    area_height.saturating_sub(border_rows) as usize
}
