use crate::agent::orchestrator::OrchCmd;
use crate::tui::command::{SlashCommand, parse_slash_command};
use crate::tui::file_picker::FilePickerState;
use crate::tui::sanitize::normalize_tui_input;
use crate::tui::state::{ActiveOverlay, ClickAction, MsgKind, MsgLine, TuiState, View, WorkState};
use crossterm::event::{Event, KeyCode, KeyModifiers, MouseEventKind};
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

const SCROLL_STEP: usize = 3;
const INTERRUPT_EXIT_WINDOW: Duration = Duration::from_secs(2);

fn scroll_by(state: &mut TuiState, delta: isize) {
    let base = if state.viewport.auto_scroll {
        state.viewport.max_scroll
    } else {
        state.viewport.scroll
    };
    state.viewport.auto_scroll = false;
    if delta < 0 {
        state.viewport.scroll = base.saturating_sub(delta.unsigned_abs());
    } else {
        state.viewport.scroll = base
            .saturating_add(delta as usize)
            .min(state.viewport.max_scroll);
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
    state.input.clamp_cursor();

    if key.modifiers.contains(KeyModifiers::CONTROL)
        && matches!(key.code, KeyCode::Char(c) if c.eq_ignore_ascii_case(&'c'))
    {
        return handle_ctrl_c(state);
    }

    if handle_overlay_key(&key, state) {
        return false;
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
            state.viewport.show_borders = !state.viewport.show_borders;
        }
        (KeyModifiers::NONE, KeyCode::Tab) => {
            state.overlay = Some(ActiveOverlay::FilePicker(FilePickerState::open(
                &state.input.buf,
                state.input.cursor,
                &state.file_picker_policy,
            )));
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
            state.input.cursor = 0;
        }
        (KeyModifiers::CONTROL, KeyCode::Char('e')) | (KeyModifiers::NONE, KeyCode::End) => {
            state.input.cursor = state.input.buf.len();
        }
        (KeyModifiers::CONTROL, KeyCode::Char('u')) => cursor_delete_before(state),
        (KeyModifiers::CONTROL, KeyCode::Char('k')) => cursor_delete_after(state),
        (KeyModifiers::CONTROL, KeyCode::Char('d')) => {
            if state.input.buf.is_empty() {
                state.quit = true;
                return true;
            }
            cursor_delete_char_after(state);
        }
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
        (KeyModifiers::CONTROL, KeyCode::Char('w')) | (KeyModifiers::ALT, KeyCode::Backspace) => {
            cursor_delete_word(state)
        }
        (KeyModifiers::NONE, KeyCode::PageUp) => {
            scroll_by(state, -((SCROLL_STEP * 5) as isize));
        }
        (KeyModifiers::NONE, KeyCode::PageDown) => {
            scroll_by(state, (SCROLL_STEP * 5) as isize);
        }
        (KeyModifiers::NONE, KeyCode::Up) => {
            if !state.input.history.is_empty()
                && (state.input.buf.is_empty() || state.input.history_idx.is_some())
            {
                if state.input.history_idx.is_none() {
                    state.input.draft_before_history = Some(state.input.buf.clone());
                }
                let idx = match state.input.history_idx {
                    None => state.input.history.len().saturating_sub(1),
                    Some(i) => i.saturating_sub(1),
                };
                state.input.buf = state.input.history[idx].clone();
                state.input.cursor = state.input.buf.len();
                state.input.history_idx = Some(idx);
            } else {
                scroll_by(state, -(SCROLL_STEP as isize));
            }
        }
        (KeyModifiers::NONE, KeyCode::Down) => {
            if let Some(idx) = state.input.history_idx {
                let next = idx + 1;
                if next >= state.input.history.len() {
                    state.input.buf = state.input.draft_before_history.take().unwrap_or_default();
                    state.input.cursor = state.input.buf.len();
                    state.input.history_idx = None;
                } else {
                    state.input.buf = state.input.history[next].clone();
                    state.input.cursor = state.input.buf.len();
                    state.input.history_idx = Some(next);
                }
            } else {
                scroll_by(state, SCROLL_STEP as isize);
            }
        }
        _ => {}
    }
    refresh_file_picker(state);
    false
}

fn handle_overlay_key(key: &crossterm::event::KeyEvent, state: &mut TuiState) -> bool {
    let Some(ActiveOverlay::FilePicker(picker)) = state.overlay.as_mut() else {
        return false;
    };
    match (key.modifiers, key.code) {
        (KeyModifiers::NONE, KeyCode::Esc) => {
            state.overlay = None;
            true
        }
        (KeyModifiers::NONE, KeyCode::Up) => {
            picker.move_selection(-1, 8);
            true
        }
        (KeyModifiers::NONE, KeyCode::Down) => {
            picker.move_selection(1, 8);
            true
        }
        (KeyModifiers::NONE, KeyCode::PageUp) => {
            picker.move_selection(-8, 8);
            true
        }
        (KeyModifiers::NONE, KeyCode::PageDown) => {
            picker.move_selection(8, 8);
            true
        }
        (KeyModifiers::NONE, KeyCode::Enter) | (KeyModifiers::NONE, KeyCode::Tab) => {
            accept_file_picker(state);
            true
        }
        _ => false,
    }
}

fn accept_file_picker(state: &mut TuiState) {
    let Some(ActiveOverlay::FilePicker(picker)) = state.overlay.take() else {
        return;
    };
    let Some(path) = picker.selected_path() else {
        return;
    };
    let start = picker.replace_start.min(state.input.buf.len());
    let end = picker.replace_end.min(state.input.buf.len());
    if start <= end
        && state.input.buf.is_char_boundary(start)
        && state.input.buf.is_char_boundary(end)
    {
        state.input.buf.replace_range(start..end, &path);
        state.input.cursor = start + path.len();
        if path.ends_with('/') {
            state.overlay = Some(ActiveOverlay::FilePicker(FilePickerState::open(
                &state.input.buf,
                state.input.cursor,
                &state.file_picker_policy,
            )));
        }
    }
}

fn refresh_file_picker(state: &mut TuiState) {
    if let Some(ActiveOverlay::FilePicker(picker)) = state.overlay.as_mut() {
        picker.refresh_with_policy(
            &state.input.buf,
            state.input.cursor,
            &state.file_picker_policy,
        );
    }
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

    if state.work_state.is_working()
        && let Some(ref interrupt) = state.interrupt
    {
        interrupt.store(true, Ordering::SeqCst);
        state.last_interrupt = Some(now);
        return false;
    }

    state.quit = true;
    true
}

fn insert_char(state: &mut TuiState, c: char) {
    state.input.clamp_cursor();
    state.input.buf.insert(state.input.cursor, c);
    state.input.cursor += c.len_utf8();
}

fn cursor_left(state: &mut TuiState) {
    if state.input.cursor > 0 {
        let mut pos = state.input.cursor - 1;
        while pos > 0 && !state.input.buf.is_char_boundary(pos) {
            pos -= 1;
        }
        state.input.cursor = pos;
    }
}

fn cursor_right(state: &mut TuiState) {
    if state.input.cursor < state.input.buf.len() {
        let mut pos = state.input.cursor + 1;
        while pos < state.input.buf.len() && !state.input.buf.is_char_boundary(pos) {
            pos += 1;
        }
        state.input.cursor = pos;
    }
}

fn cursor_word_left(state: &mut TuiState) {
    state.input.clamp_cursor();
    let mut pos = state.input.cursor;
    while pos > 0 {
        pos = prev_char_boundary(&state.input.buf, pos);
        let Some(ch) = char_at(&state.input.buf, pos) else {
            break;
        };
        if !ch.is_whitespace() {
            break;
        }
    }
    while pos > 0 {
        let prev = prev_char_boundary(&state.input.buf, pos);
        let Some(ch) = char_at(&state.input.buf, prev) else {
            break;
        };
        if ch.is_whitespace() {
            break;
        }
        pos = prev;
    }
    state.input.cursor = pos;
}

fn cursor_word_right(state: &mut TuiState) {
    state.input.clamp_cursor();
    let mut pos = state.input.cursor;
    while pos < state.input.buf.len() {
        let Some(ch) = char_at(&state.input.buf, pos) else {
            break;
        };
        if ch.is_whitespace() {
            break;
        }
        pos += ch.len_utf8();
    }
    while pos < state.input.buf.len() {
        let Some(ch) = char_at(&state.input.buf, pos) else {
            break;
        };
        if !ch.is_whitespace() {
            break;
        }
        pos += ch.len_utf8();
    }
    state.input.cursor = pos;
}

fn cursor_backspace(state: &mut TuiState) {
    state.input.clamp_cursor();
    if state.input.cursor > 0 {
        let mut pos = state.input.cursor - 1;
        while pos > 0 && !state.input.buf.is_char_boundary(pos) {
            pos -= 1;
        }
        state.input.buf.remove(pos);
        state.input.cursor = pos;
    }
}

fn cursor_delete_before(state: &mut TuiState) {
    state.input.clamp_cursor();
    state.input.buf.replace_range(..state.input.cursor, "");
    state.input.cursor = 0;
}

fn cursor_delete_after(state: &mut TuiState) {
    state.input.clamp_cursor();
    state.input.buf.replace_range(state.input.cursor.., "");
}

fn cursor_delete_char_after(state: &mut TuiState) {
    state.input.clamp_cursor();
    if state.input.cursor < state.input.buf.len() {
        let next = next_char_boundary(&state.input.buf, state.input.cursor);
        state.input.buf.replace_range(state.input.cursor..next, "");
    }
}

fn cursor_delete_word(state: &mut TuiState) {
    state.input.clamp_cursor();
    let end = state.input.cursor;
    let mut start = end;

    while start > 0 {
        let prev = prev_char_boundary(&state.input.buf, start);
        let Some(ch) = char_at(&state.input.buf, prev) else {
            break;
        };
        if !ch.is_whitespace() {
            break;
        }
        start = prev;
    }

    while start > 0 {
        let prev = prev_char_boundary(&state.input.buf, start);
        let Some(ch) = char_at(&state.input.buf, prev) else {
            break;
        };
        if ch.is_whitespace() {
            break;
        }
        start = prev;
    }

    state.input.buf.replace_range(start..end, "");
    state.input.cursor = start;
}

fn char_at(s: &str, pos: usize) -> Option<char> {
    s.get(pos..)?.chars().next()
}

fn prev_char_boundary(s: &str, pos: usize) -> usize {
    let mut prev = pos.saturating_sub(1);
    while prev > 0 && !s.is_char_boundary(prev) {
        prev -= 1;
    }
    prev
}

fn next_char_boundary(s: &str, pos: usize) -> usize {
    let mut next = (pos + 1).min(s.len());
    while next < s.len() && !s.is_char_boundary(next) {
        next += 1;
    }
    next
}

fn handle_enter(
    state: &mut TuiState,
    orch_tx: &tokio::sync::mpsc::UnboundedSender<OrchCmd>,
) -> bool {
    state.input.clamp_cursor();
    let input = std::mem::take(&mut state.input.buf);
    state.input.cursor = 0;
    if input.is_empty() {
        return false;
    }
    state.input.history.push(input.clone());
    state.input.history_idx = None;
    state.push_line(MsgLine::new(format!("> {input}"), MsgKind::Info));
    match parse_slash_command(&input) {
        Ok(Some(command)) => match command {
            SlashCommand::Flash => {
                let _ = orch_tx.send(OrchCmd::SetModel("flash".into()));
                state.model = "flash".into();
            }
            SlashCommand::Pro => {
                let _ = orch_tx.send(OrchCmd::SetModel("pro".into()));
                state.model = "pro".into();
            }
            SlashCommand::Compact => {
                let (done_tx, _) = tokio::sync::oneshot::channel();
                if orch_tx.send(OrchCmd::Compact { done: done_tx }).is_ok() {
                    state.arm_task_notification();
                    state.work_state = WorkState::Compacting;
                } else {
                    state.push_line(MsgLine::new(
                        "Failed to send compact command.".into(),
                        MsgKind::Error,
                    ));
                }
            }
            SlashCommand::Help => state.add_help(),
            SlashCommand::Skills => state.show_skills(),
            SlashCommand::Quit => {
                state.quit = true;
                return true;
            }
        },
        Ok(None) => {
            let (done_tx, _) = tokio::sync::oneshot::channel();
            if orch_tx
                .send(OrchCmd::UserInput {
                    input,
                    done: done_tx,
                })
                .is_ok()
            {
                state.arm_task_notification();
                state.work_state = WorkState::WaitingModel;
            } else {
                state.push_line(MsgLine::new(
                    "Failed to send user input.".into(),
                    MsgKind::Error,
                ));
            }
        }
        Err(_) => {
            state.push_line(MsgLine::new(
                "Unknown command. Prefix with a space to send it as text.".into(),
                MsgKind::Info,
            ));
        }
    }
    state.viewport.auto_scroll = true;
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
                if state.viewport.click_map.is_empty() {
                    return false;
                }
                let Some(row) = content_row_for_mouse(state, mouse.row) else {
                    return false;
                };
                let mut hit: Option<(usize, ClickAction)> = None;
                for target in &state.viewport.click_map {
                    if (target.start_row..=target.end_row).contains(&row) {
                        hit = Some((target.line_idx, target.action.clone()));
                        break;
                    }
                }
                match hit {
                    Some((idx, ClickAction::ToggleCollapse)) => {
                        if let Some(msg) = state.lines.get_mut(idx) {
                            msg.toggle_collapsed();
                            state.invalidate_all_cache();
                        }
                    }
                    Some((_, ClickAction::OpenSubAgentDetail { session_id })) => {
                        state.view = View::SubAgentDetail {
                            session_id,
                            scroll: 0,
                        };
                    }
                    None => {}
                }
            }
            _ => {}
        },
        Event::Resize(..) => {}
        Event::Paste(content) => {
            state.input.clamp_cursor();
            let to_insert = normalize_tui_input(&content);
            if !to_insert.is_empty() {
                state.input.buf.insert_str(state.input.cursor, &to_insert);
                state.input.cursor += to_insert.len();
            }
            refresh_file_picker(state);
        }
        _ => {}
    }
    false
}

fn content_row_for_mouse(state: &TuiState, mouse_row: u16) -> Option<usize> {
    if state.viewport.show_borders {
        (mouse_row > state.viewport.content_y)
            .then(|| usize::from(mouse_row - state.viewport.content_y - 1))
    } else {
        (mouse_row >= state.viewport.content_y)
            .then(|| usize::from(mouse_row - state.viewport.content_y))
    }
}
