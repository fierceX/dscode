use crate::tui::markdown::{push_msg, render_md_with_tables, wrap_lines_word};
use crate::tui::state::{MsgKind, TuiState, View};
use crate::ui::StatsSnapshot;
use crate::util::fmt_k;
use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, Borders, Paragraph},
};

pub(crate) fn render(f: &mut Frame, state: &mut TuiState) {
    let area = f.area();
    if area.height < 5 || area.width < 20 {
        return;
    }

    let view = state.view.clone();
    match &view {
        View::Main => {
            let inner_w = area.width.saturating_sub(2).max(1) as usize;
            let vis_lines = split_at_visual_width(&state.input_buf, inner_w);
            let lines_before =
                split_at_visual_width(&state.input_buf[..state.input_cursor], inner_w);
            let cursor_row = lines_before.len().saturating_sub(1);
            let content_lines = vis_lines.len().clamp(1, 5);
            state.input_scroll_row = clamp_input_scroll(
                vis_lines.len(),
                cursor_row,
                content_lines,
                state.input_scroll_row,
            );
            let visible_input_lines: Vec<String> = vis_lines
                .iter()
                .skip(state.input_scroll_row)
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

            state.content_y = chunks[0].y;
            render_content(f, chunks[0], state);
            render_input(f, chunks[1], &visible_input_lines);

            let row = cursor_row.saturating_sub(state.input_scroll_row);
            let col = if lines_before.is_empty() {
                0
            } else {
                unicode_width::UnicodeWidthStr::width(lines_before.last().unwrap().as_str())
            };
            let cursor_x = (chunks[1].x + 1 + col as u16).min(chunks[1].right().saturating_sub(2));
            let cursor_y = (chunks[1].y + 1 + row as u16).min(chunks[1].bottom().saturating_sub(2));
            f.set_cursor_position((cursor_x, cursor_y));

            render_status(f, chunks[2], state);
        }
        View::SubAgentDetail { line_idx, scroll } => {
            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Min(1), Constraint::Length(1)])
                .split(area);
            render_detail_content(f, chunks[0], *line_idx, *scroll, state.show_borders, state);
            render_detail_bar(f, chunks[1]);
        }
    }
}

fn render_detail_content(
    f: &mut Frame,
    area: Rect,
    line_idx: usize,
    scroll: usize,
    show_borders: bool,
    state: &TuiState,
) {
    let (title, thinking, text) = match state.lines.get(line_idx) {
        Some(line) if line.kind == MsgKind::SubAgent => {
            let detail = line.sub_detail.as_ref();
            let thinking = detail.map(|d| d.thinking.as_str()).unwrap_or("");
            let text = detail.map(|d| d.text.as_str()).unwrap_or("");
            (line.text.as_str(), thinking, text)
        }
        _ => return,
    };

    let mut all_lines: Vec<Line<'static>> = Vec::new();
    all_lines.push(Line::from(Span::styled(
        title.to_string(),
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

    let viewport = area.height.saturating_sub(2) as usize;
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

fn render_detail_bar(f: &mut Frame, area: Rect) {
    let text = Span::styled(
        " Esc: Back │ ↑↓ PgUp/PgDn: Scroll ",
        Style::default().fg(Color::Yellow),
    );
    f.render_widget(Paragraph::new(Line::from(text)), area);
}

fn render_content(f: &mut Frame, area: Rect, state: &mut TuiState) {
    let inner_w = area.width.saturating_sub(2).max(1);
    let width_changed = state.cached_width != inner_w;
    state.cached_width = inner_w;

    let mut need_rebuild = width_changed || state.cached_all.is_none();
    let collapsible = |k: MsgKind| matches!(k, MsgKind::StreamThinking);

    for msg in state.lines.iter_mut() {
        if width_changed || !msg.cache_valid() {
            let mut seg = Vec::new();
            if msg.collapsed {
                let first = msg.text.lines().next().unwrap_or("");
                let max_w = (inner_w as usize).saturating_sub(4).max(1);
                let mut snippet = String::new();
                let mut dw = 0usize;
                let mut cut = false;
                for ch in first.chars() {
                    let cw = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
                    if dw + cw > max_w {
                        cut = true;
                        break;
                    }
                    snippet.push(ch);
                    dw += cw;
                }
                let suffix = if cut { "…" } else { "" };
                push_msg(&mut seg, &format!("► {snippet}{suffix}"), msg.kind);
            } else if collapsible(msg.kind) {
                push_msg(&mut seg, &format!("▼ {}", msg.text), msg.kind);
            } else {
                push_msg(&mut seg, &msg.text, msg.kind);
            }
            msg.cached_lines = Some(wrap_lines_word(&seg, inner_w));
            msg.cached_collapsed = msg.collapsed;
            need_rebuild = true;
        }
    }

    if need_rebuild {
        let mut all_lines: Vec<Line<'static>> = Vec::new();
        state.click_map.clear();
        let mut current_row = 0usize;
        for (idx, msg) in state.lines.iter().enumerate() {
            let cached = msg.cached_lines.as_ref().unwrap();
            let phys = cached.len();
            state
                .click_map
                .push((idx, current_row, current_row + phys.saturating_sub(1)));
            current_row += phys;
            all_lines.extend(cached.clone());
        }
        state.cached_all = Some(all_lines);
    }

    ensure_stream_cache(state, inner_w);
    let history_len = state.cached_all.as_ref().map_or(0, Vec::len);
    let stream_len = state.cached_stream_lines.as_ref().map_or(0, Vec::len);
    let total_len = history_len + stream_len;
    let viewport = area.height.saturating_sub(2) as usize;
    let max_scroll = total_len.saturating_sub(viewport);
    state.max_scroll = max_scroll;

    let scroll = if state.auto_scroll {
        max_scroll
    } else {
        state.scroll.min(max_scroll)
    };
    state.effective_scroll = scroll;

    let border_color = if state.streaming {
        Color::Cyan
    } else {
        Color::DarkGray
    };
    let borders = if state.show_borders {
        Borders::ALL
    } else {
        Borders::NONE
    };
    let mut block = Block::default()
        .borders(borders)
        .border_style(Style::default().fg(border_color));

    let mut title_parts: Vec<Span<'static>> = Vec::new();
    if scroll > 0 {
        title_parts.push(Span::styled(" ↥ ", Style::default().fg(Color::Yellow)));
    }
    if scroll < max_scroll {
        title_parts.push(Span::styled(" ↧ ", Style::default().fg(Color::Yellow)));
    }
    if !title_parts.is_empty() {
        block = block.title_bottom(Line::from(title_parts));
    }

    let visible = visible_lines(
        state.cached_all.as_deref().unwrap_or(&[]),
        state.cached_stream_lines.as_deref().unwrap_or(&[]),
        scroll,
        viewport,
    );

    let paragraph = Paragraph::new(Text::from(visible)).block(block);
    f.render_widget(paragraph, area);
}

fn ensure_stream_cache(state: &mut TuiState, inner_w: u16) {
    if !state.streaming || state.stream_line.is_empty() {
        state.invalidate_stream_cache();
        return;
    }
    let cache_valid = state.cached_stream_lines.is_some()
        && state.cached_stream_width == inner_w
        && state.cached_stream_kind == state.stream_kind
        && state.cached_stream_revision == state.stream_revision;
    if cache_valid {
        return;
    }

    let mut seg: Vec<Line<'static>> = Vec::new();
    push_msg(&mut seg, &state.stream_line, state.stream_kind);
    state.cached_stream_lines = Some(wrap_lines_word(&seg, inner_w));
    state.cached_stream_width = inner_w;
    state.cached_stream_kind = state.stream_kind;
    state.cached_stream_revision = state.stream_revision;
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

fn render_input(f: &mut Frame, area: Rect, vis_lines: &[String]) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::DarkGray));

    let inner = block.inner(area);
    f.render_widget(block, area);
    let text = Text::from(vis_lines.join("\n"));
    f.render_widget(Paragraph::new(text), inner);
}

pub(crate) fn split_at_visual_width(s: &str, max_width: usize) -> Vec<String> {
    if s.is_empty() {
        return vec![String::new()];
    }
    let max_width = max_width.max(1);
    let mut lines: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut cur_w = 0usize;
    for ch in s.chars() {
        if ch == '\n' {
            lines.push(std::mem::take(&mut cur));
            cur_w = 0;
            continue;
        }
        let cw = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
        if cur_w + cw > max_width && !cur.is_empty() {
            lines.push(cur);
            cur = String::new();
            cur_w = 0;
        }
        cur.push(ch);
        cur_w += cw;
    }
    if !cur.is_empty() || s.ends_with('\n') {
        lines.push(cur);
    }
    lines
}

pub(crate) fn clamp_input_scroll(
    total_rows: usize,
    cursor_row: usize,
    visible_rows: usize,
    current_scroll: usize,
) -> usize {
    if total_rows == 0 || visible_rows == 0 {
        return 0;
    }

    let max_scroll = total_rows.saturating_sub(visible_rows);
    let scroll = current_scroll.min(max_scroll);
    if cursor_row < scroll {
        cursor_row
    } else if cursor_row >= scroll + visible_rows {
        cursor_row + 1 - visible_rows
    } else {
        scroll
    }
}

fn render_status(f: &mut Frame, area: Rect, state: &TuiState) {
    let s = &state.stats;
    let b = if s.belief > 0.0 {
        format!(" B:{:.2}", s.belief)
    } else {
        String::new()
    };
    let ti = s.total_input_tokens + s.total_cache_read_tokens;
    let work = state.work_state.label();
    let status = format!(
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
    );
    let line = Line::from(Span::styled(status, Style::default().fg(Color::Cyan)));
    f.render_widget(Paragraph::new(line), area);
}
