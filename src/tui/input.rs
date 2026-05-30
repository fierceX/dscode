use crate::agent::orchestrator::OrchCmd;
use crate::tui::state::{MsgKind, MsgLine, TuiState, View, WorkState};
use crossterm::event::{Event, KeyCode, KeyModifiers, MouseEventKind};
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

const SCROLL_STEP: usize = 3;
const INTERRUPT_EXIT_WINDOW: Duration = Duration::from_secs(2);

fn scroll_by(state: &mut TuiState, delta: isize) {
    let base = if state.auto_scroll {
        state.max_scroll
    } else {
        state.scroll
    };
    state.auto_scroll = false;
    if delta < 0 {
        state.scroll = base.saturating_sub(delta.unsigned_abs());
    } else {
        state.scroll = base.saturating_add(delta as usize).min(state.max_scroll);
    }
}

fn handle_key(
    key: crossterm::event::KeyEvent,
    state: &mut TuiState,
    orch_tx: &tokio::sync::mpsc::UnboundedSender<OrchCmd>,
) -> bool {
    if key.kind == crossterm::event::KeyEventKind::Release {
        return false;
    }

    if key.modifiers.contains(KeyModifiers::CONTROL)
        && matches!(key.code, KeyCode::Char(c) if c.eq_ignore_ascii_case(&'c'))
    {
        return handle_ctrl_c(state);
    }

    if matches!(state.view, View::SubAgentDetail { .. }) {
        match (key.modifiers, key.code) {
            (KeyModifiers::NONE, KeyCode::Esc) => {
                state.view = View::Main;
                return false;
            }
            (KeyModifiers::NONE, KeyCode::PageUp) => {
                if let View::SubAgentDetail { scroll, .. } = &mut state.view {
                    *scroll = scroll.saturating_sub(10);
                }
                return false;
            }
            (KeyModifiers::NONE, KeyCode::PageDown) => {
                if let View::SubAgentDetail { scroll, .. } = &mut state.view {
                    *scroll = scroll.saturating_add(10);
                }
                return false;
            }
            (KeyModifiers::NONE, KeyCode::Up) => {
                if let View::SubAgentDetail { scroll, .. } = &mut state.view {
                    *scroll = scroll.saturating_sub(1);
                }
                return false;
            }
            (KeyModifiers::NONE, KeyCode::Down) => {
                if let View::SubAgentDetail { scroll, .. } = &mut state.view {
                    *scroll = scroll.saturating_add(1);
                }
                return false;
            }
            _ => return false,
        }
    }

    match (key.modifiers, key.code) {
        (KeyModifiers::CONTROL, KeyCode::Char('t')) => {
            state.show_borders = !state.show_borders;
        }
        (KeyModifiers::NONE, KeyCode::Esc) => {
            state.quit = true;
            return true;
        }
        (mods, KeyCode::Char(c)) if is_text_modifier(mods) => {
            if !c.is_control() {
                insert_char(state, c);
            }
        }
        (KeyModifiers::SHIFT, KeyCode::Enter) => insert_char(state, '\n'),
        (KeyModifiers::CONTROL, KeyCode::Char('a')) | (KeyModifiers::NONE, KeyCode::Home) => {
            state.input_cursor = 0;
        }
        (KeyModifiers::CONTROL, KeyCode::Char('e')) | (KeyModifiers::NONE, KeyCode::End) => {
            state.input_cursor = state.input_buf.len();
        }
        (KeyModifiers::CONTROL, KeyCode::Char('u')) => cursor_delete_before(state),
        (KeyModifiers::CONTROL, KeyCode::Char('k')) => cursor_delete_after(state),
        (KeyModifiers::ALT, KeyCode::Left) | (KeyModifiers::ALT, KeyCode::Char('b')) => {
            cursor_word_left(state);
        }
        (KeyModifiers::ALT, KeyCode::Right) | (KeyModifiers::ALT, KeyCode::Char('f')) => {
            cursor_word_right(state);
        }
        (KeyModifiers::NONE, KeyCode::Left) => cursor_left(state),
        (KeyModifiers::NONE, KeyCode::Right) => cursor_right(state),
        (KeyModifiers::NONE, KeyCode::Enter) => return handle_enter(state, orch_tx),
        (KeyModifiers::NONE, KeyCode::Backspace) => cursor_backspace(state),
        (KeyModifiers::CONTROL, KeyCode::Char('w')) => cursor_delete_word(state),
        (KeyModifiers::NONE, KeyCode::PageUp) => {
            scroll_by(state, -((SCROLL_STEP * 5) as isize));
        }
        (KeyModifiers::NONE, KeyCode::PageDown) => {
            scroll_by(state, (SCROLL_STEP * 5) as isize);
        }
        (KeyModifiers::NONE, KeyCode::Up) => {
            if !state.input_history.is_empty() && state.input_buf.is_empty() {
                let idx = match state.history_idx {
                    None => state.input_history.len().saturating_sub(1),
                    Some(i) => i.saturating_sub(1),
                };
                state.input_buf = state.input_history[idx].clone();
                state.input_cursor = state.input_buf.len();
                state.history_idx = Some(idx);
            } else {
                scroll_by(state, -(SCROLL_STEP as isize));
            }
        }
        (KeyModifiers::NONE, KeyCode::Down) => {
            if let Some(idx) = state.history_idx {
                let next = idx + 1;
                if next >= state.input_history.len() {
                    state.input_buf.clear();
                    state.input_cursor = 0;
                    state.history_idx = None;
                } else {
                    state.input_buf = state.input_history[next].clone();
                    state.input_cursor = state.input_buf.len();
                    state.history_idx = Some(next);
                }
            } else {
                scroll_by(state, SCROLL_STEP as isize);
            }
        }
        _ => {}
    }
    false
}

fn is_text_modifier(modifiers: KeyModifiers) -> bool {
    modifiers == KeyModifiers::NONE || modifiers == KeyModifiers::SHIFT
}

pub(crate) fn handle_ctrl_c(state: &mut TuiState) -> bool {
    let now = Instant::now();
    if state
        .last_interrupt
        .is_some_and(|at| now.duration_since(at) <= INTERRUPT_EXIT_WINDOW)
    {
        state.quit = true;
        return true;
    }

    if state.work_state.is_working() {
        if let Some(ref interrupt) = state.interrupt {
            interrupt.store(true, Ordering::SeqCst);
            state.last_interrupt = Some(now);
            return false;
        }
    }

    state.quit = true;
    true
}

fn insert_char(state: &mut TuiState, c: char) {
    state.input_buf.insert(state.input_cursor, c);
    state.input_cursor += c.len_utf8();
}

fn cursor_left(state: &mut TuiState) {
    if state.input_cursor > 0 {
        let mut pos = state.input_cursor - 1;
        while pos > 0 && !state.input_buf.is_char_boundary(pos) {
            pos -= 1;
        }
        state.input_cursor = pos;
    }
}

fn cursor_right(state: &mut TuiState) {
    if state.input_cursor < state.input_buf.len() {
        let mut pos = state.input_cursor + 1;
        while pos < state.input_buf.len() && !state.input_buf.is_char_boundary(pos) {
            pos += 1;
        }
        state.input_cursor = pos;
    }
}

fn cursor_word_left(state: &mut TuiState) {
    let mut pos = state.input_cursor;
    while pos > 0 {
        pos = prev_char_boundary(&state.input_buf, pos);
        let ch = state.input_buf[pos..].chars().next().unwrap();
        if !ch.is_whitespace() {
            break;
        }
    }
    while pos > 0 {
        let prev = prev_char_boundary(&state.input_buf, pos);
        let ch = state.input_buf[prev..].chars().next().unwrap();
        if ch.is_whitespace() {
            break;
        }
        pos = prev;
    }
    state.input_cursor = pos;
}

fn cursor_word_right(state: &mut TuiState) {
    let mut pos = state.input_cursor;
    while pos < state.input_buf.len() {
        let ch = state.input_buf[pos..].chars().next().unwrap();
        if ch.is_whitespace() {
            break;
        }
        pos += ch.len_utf8();
    }
    while pos < state.input_buf.len() {
        let ch = state.input_buf[pos..].chars().next().unwrap();
        if !ch.is_whitespace() {
            break;
        }
        pos += ch.len_utf8();
    }
    state.input_cursor = pos;
}

fn cursor_backspace(state: &mut TuiState) {
    if state.input_cursor > 0 {
        let mut pos = state.input_cursor - 1;
        while pos > 0 && !state.input_buf.is_char_boundary(pos) {
            pos -= 1;
        }
        state.input_buf.remove(pos);
        state.input_cursor = pos;
    }
}

fn cursor_delete_before(state: &mut TuiState) {
    state.input_buf.replace_range(..state.input_cursor, "");
    state.input_cursor = 0;
}

fn cursor_delete_after(state: &mut TuiState) {
    state.input_buf.replace_range(state.input_cursor.., "");
}

fn cursor_delete_word(state: &mut TuiState) {
    let prefix: String = state.input_buf[..state.input_cursor].to_string();
    if let Some(pos) = prefix.trim_end().rfind(' ') {
        state.input_buf.replace_range(pos..state.input_cursor, "");
        state.input_cursor = pos;
    } else {
        state.input_buf.replace_range(..state.input_cursor, "");
        state.input_cursor = 0;
    }
}

fn prev_char_boundary(s: &str, pos: usize) -> usize {
    let mut prev = pos.saturating_sub(1);
    while prev > 0 && !s.is_char_boundary(prev) {
        prev -= 1;
    }
    prev
}

fn handle_enter(
    state: &mut TuiState,
    orch_tx: &tokio::sync::mpsc::UnboundedSender<OrchCmd>,
) -> bool {
    let input = std::mem::take(&mut state.input_buf);
    state.input_cursor = 0;
    if input.is_empty() {
        return false;
    }
    state.input_history.push(input.clone());
    state.history_idx = None;
    state.push_line(MsgLine::new(format!("> {input}"), MsgKind::Info));
    if input.starts_with('/') {
        match input.as_str() {
            "/flash" => {
                let _ = orch_tx.send(OrchCmd::SetModel("flash".into()));
                state.model = "flash".into();
            }
            "/pro" => {
                let _ = orch_tx.send(OrchCmd::SetModel("pro".into()));
                state.model = "pro".into();
            }
            "/compact" => {
                let (done_tx, _) = tokio::sync::oneshot::channel();
                let _ = orch_tx.send(OrchCmd::Compact { done: done_tx });
                state.work_state = WorkState::Compacting;
            }
            "/help" => state.add_help(),
            "/skills" => state.show_skills(),
            "/exit" | "/quit" | "/q" => {
                state.quit = true;
                return true;
            }
            _ => {
                state.push_line(MsgLine::new(
                    "Unknown command. Prefix with a space to send it as text.".into(),
                    MsgKind::Info,
                ));
            }
        }
    } else {
        let (done_tx, _) = tokio::sync::oneshot::channel();
        let _ = orch_tx.send(OrchCmd::UserInput {
            input,
            done: done_tx,
        });
        state.work_state = WorkState::WaitingModel;
    }
    state.auto_scroll = true;
    false
}

pub(crate) fn handle_event(
    ev: Event,
    state: &mut TuiState,
    orch_tx: &tokio::sync::mpsc::UnboundedSender<OrchCmd>,
) -> bool {
    match ev {
        Event::Key(key) => return handle_key(key, state, orch_tx),
        Event::Mouse(mouse) => match mouse.kind {
            MouseEventKind::ScrollUp => {
                if let View::SubAgentDetail { scroll, .. } = &mut state.view {
                    *scroll = scroll.saturating_sub(3);
                } else {
                    scroll_by(state, -(SCROLL_STEP as isize));
                }
            }
            MouseEventKind::ScrollDown => {
                if let View::SubAgentDetail { scroll, .. } = &mut state.view {
                    *scroll = scroll.saturating_add(3);
                } else {
                    scroll_by(state, SCROLL_STEP as isize);
                }
            }
            MouseEventKind::Down(_) => {
                if state.click_map.is_empty() {
                    return false;
                }
                let abs_row =
                    usize::from(mouse.row.saturating_sub(state.content_y).saturating_sub(1))
                        + state.effective_scroll;
                let mut hit: Option<usize> = None;
                for (idx, start, end) in &state.click_map {
                    if (*start..=*end).contains(&abs_row) {
                        hit = Some(*idx);
                        break;
                    }
                }
                if let Some(idx) = hit
                    && let Some(msg) = state.lines.get_mut(idx)
                {
                    if matches!(msg.kind, MsgKind::StreamThinking) {
                        msg.collapsed = !msg.collapsed;
                    } else if msg.kind == MsgKind::SubAgent && msg.sub_detail.is_some() {
                        state.view = View::SubAgentDetail {
                            line_idx: idx,
                            scroll: 0,
                        };
                    }
                }
            }
            _ => {}
        },
        Event::Resize(..) => {}
        Event::Paste(content) => {
            let to_insert: String = content
                .chars()
                .filter(|&ch| !ch.is_control() || ch == '\n' || ch == '\t')
                .collect();
            if !to_insert.is_empty() {
                state.input_buf.insert_str(state.input_cursor, &to_insert);
                state.input_cursor += to_insert.len();
            }
        }
        _ => {}
    }
    false
}
