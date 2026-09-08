use crate::cli::RuntimeCmd;
use crate::config::TuiMode;
use crate::tui::command::{SlashCommand, parse_slash_command};
use crate::tui::file_picker::FilePickerState;
use crate::tui::sanitize::normalize_tui_input;
use crate::tui::state::{
    ActiveOverlay, ClickAction, PendingImage, TranscriptItem, TranscriptKind, TuiState, TuiUiEvent,
    View, WorkState, display_user_input, submitted_user_input,
};
use crossterm::event::{Event, KeyCode, KeyModifiers, MouseEventKind};
use std::time::{Duration, Instant};

const SCROLL_STEP: usize = 3;
const INTERRUPT_EXIT_WINDOW: Duration = Duration::from_secs(2);
/// A clipboard read that never reports back (hung `osascript`) must not
/// disable paste for the rest of the session: after this window a new read is
/// allowed to start.
const CLIPBOARD_RETRY_WINDOW: Duration = Duration::from_secs(10);

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
    orch_tx: &tokio::sync::mpsc::UnboundedSender<RuntimeCmd>,
) -> bool {
    if key.kind == crossterm::event::KeyEventKind::Release {
        return false;
    }
    state.input.clamp_cursor();

    if key.modifiers.contains(KeyModifiers::CONTROL)
        && matches!(key.code, KeyCode::Char(c) if c.eq_ignore_ascii_case(&'c'))
    {
        return handle_ctrl_c(state, orch_tx);
    }

    if handle_overlay_key(&key, state) {
        return false;
    }

    if !matches!(state.view, View::Main) {
        match (key.modifiers, key.code) {
            (KeyModifiers::NONE, KeyCode::Esc) => {
                state.view = View::Main;
                return false;
            }
            (KeyModifiers::NONE, KeyCode::PageUp) => {
                if let Some(scroll) = view_scroll_mut(&mut state.view) {
                    *scroll = scroll.saturating_sub(10);
                }
                return false;
            }
            (KeyModifiers::NONE, KeyCode::PageDown) => {
                if let Some(scroll) = view_scroll_mut(&mut state.view) {
                    *scroll = scroll.saturating_add(10);
                }
                return false;
            }
            (KeyModifiers::NONE, KeyCode::Up) => {
                if let Some(scroll) = view_scroll_mut(&mut state.view) {
                    *scroll = scroll.saturating_sub(1);
                }
                return false;
            }
            (KeyModifiers::NONE, KeyCode::Down) => {
                if let Some(scroll) = view_scroll_mut(&mut state.view) {
                    *scroll = scroll.saturating_add(1);
                }
                return false;
            }
            _ => return false,
        }
    }

    match (key.modifiers, key.code) {
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
        (KeyModifiers::SHIFT, KeyCode::Enter)
        | (KeyModifiers::ALT, KeyCode::Enter)
        | (KeyModifiers::CONTROL, KeyCode::Char('j')) => {
            // The file picker owns these keys while it is open: a newline in a
            // path query is meaningless and only blanks the candidate list.
            if state.overlay.is_none() {
                insert_char(state, '\n');
            }
        }
        (KeyModifiers::CONTROL, KeyCode::Char('a')) | (KeyModifiers::NONE, KeyCode::Home) => {
            state.input.cursor = 0;
        }
        (KeyModifiers::CONTROL, KeyCode::Char('e')) | (KeyModifiers::NONE, KeyCode::End) => {
            state.input.cursor = state.input.buf.len();
        }
        (KeyModifiers::CONTROL, KeyCode::Char('u')) => cursor_delete_before(state),
        (KeyModifiers::CONTROL, KeyCode::Char('k')) => cursor_delete_after(state),
        (mods, KeyCode::Char('v'))
            if mods.contains(KeyModifiers::CONTROL) || mods.contains(KeyModifiers::SUPER) =>
        {
            // Queuing an image behind an open overlay gives no visible
            // feedback and would garble the picker; require a closed overlay.
            if state.overlay.is_none() {
                request_clipboard_image(state)
            }
        }
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
        (KeyModifiers::NONE, KeyCode::Backspace) => {
            if state.input.buf.is_empty() {
                // An empty input turns Backspace into "remove the last queued
                // clipboard image".
                state.input.pending_images.pop();
            } else {
                cursor_backspace(state);
            }
        }
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

fn view_scroll_mut(view: &mut View) -> Option<&mut usize> {
    match view {
        View::SubAgentDetail { scroll, .. }
        | View::Plan { scroll }
        | View::Todos { scroll }
        | View::Artifact { scroll } => Some(scroll),
        View::Main => None,
    }
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
        (KeyModifiers::NONE, KeyCode::Enter) => {
            accept_file_picker(state, false);
            true
        }
        (KeyModifiers::NONE, KeyCode::Tab) => {
            accept_file_picker(state, true);
            true
        }
        _ => false,
    }
}

fn accept_file_picker(state: &mut TuiState, keep_open_for_dirs: bool) {
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
        if keep_open_for_dirs && path.ends_with('/') {
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

pub(crate) fn handle_ctrl_c(
    state: &mut TuiState,
    orch_tx: &tokio::sync::mpsc::UnboundedSender<RuntimeCmd>,
) -> bool {
    let now = Instant::now();
    if state
        .last_interrupt
        .is_some_and(|at| now.duration_since(at) <= INTERRUPT_EXIT_WINDOW)
    {
        state.quit = true;
        return true;
    }

    if state.work_state.is_working() {
        let _ = orch_tx.send(RuntimeCmd::Interrupt);
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

/// Ctrl+V: read the system clipboard on a worker thread (the platform reader
/// spawns a subprocess, so it must not block the event loop) and stage the
/// image under the session attachment directory.
fn request_clipboard_image(state: &mut TuiState) {
    let Some(limits) = state.image_input.clone() else {
        state.push_line(TranscriptItem::new(
            "Clipboard image paste is unavailable: this session has no image input capability."
                .into(),
            TranscriptKind::Info,
        ));
        return;
    };
    if state
        .clipboard_started
        .is_some_and(|started| started.elapsed() < CLIPBOARD_RETRY_WINDOW)
    {
        return;
    }
    let Some(ui_tx) = state.ui_tx.clone() else {
        return;
    };
    if state.attachments_dir.as_os_str().is_empty() {
        state.push_line(TranscriptItem::new(
            "Clipboard image paste is unavailable: the session attachment directory is unknown."
                .into(),
            TranscriptKind::Error,
        ));
        return;
    }
    state.clipboard_started = Some(Instant::now());
    let dir = state.attachments_dir.clone();
    let reader = state.clipboard_reader.clone();
    std::thread::spawn(move || {
        let staged = match reader {
            Some(reader) => reader(&dir, &limits),
            None => crate::tui::clipboard::read_clipboard_png(&dir, &limits),
        }
        .and_then(|png| {
            let path = crate::tui::attachments::AttachmentStore::new(dir.clone())
                .commit_png(&png.bytes)?;
            // The marker quotes the path; a quote or control character inside
            // it could not be represented unambiguously for the model.
            let display = path.to_string_lossy();
            if display.contains('"') || display.chars().any(char::is_control) {
                anyhow::bail!(
                    "attachment path cannot be represented in the message marker: {display}"
                );
            }
            Ok(PendingImage {
                path,
                width: png.width,
                height: png.height,
                bytes: png.bytes.len(),
            })
        });
        let event = match staged {
            Ok(image) => TuiUiEvent::ImageCaptured(image),
            Err(error) => TuiUiEvent::ClipboardFailed(format!("{error:#}")),
        };
        let _ = ui_tx.send(event);
    });
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
    orch_tx: &tokio::sync::mpsc::UnboundedSender<RuntimeCmd>,
) -> bool {
    state.input.clamp_cursor();
    let typed = std::mem::take(&mut state.input.buf);
    let images = std::mem::take(&mut state.input.pending_images);
    state.input.cursor = 0;
    if typed.is_empty() && images.is_empty() {
        return false;
    }
    if !typed.is_empty() {
        state.input.history.push(typed.clone());
    }
    state.input.history_idx = None;
    // 提交新输入前先封口上一轮未结束的流式内容，保证用户输入始终显示在
    // 已展示内容之后，避免被后续到达的 finalize 插入到错误位置。
    state.finalize_stream();
    state.push_line(TranscriptItem::new(
        format!("> {}", display_user_input(&typed, &images)),
        TranscriptKind::Info,
    ));
    match parse_slash_command(&typed) {
        Ok(Some(command)) => {
            if !images.is_empty() {
                // Slash commands are local UI/runtime actions, not model
                // turns: keep the images queued for the next real message.
                state.input.pending_images = images;
                state.push_line(TranscriptItem::new(
                    "Queued image(s) were not attached to a slash command; they stay queued for the next message."
                        .into(),
                    TranscriptKind::Info,
                ));
            }
            match command {
                SlashCommand::Flash => {
                    let _ = orch_tx.send(RuntimeCmd::SetModel("flash".into()));
                }
                SlashCommand::Pro => {
                    let _ = orch_tx.send(RuntimeCmd::SetModel("pro".into()));
                }
                SlashCommand::Model(model) => {
                    let _ = orch_tx.send(RuntimeCmd::SetModel(model));
                }
                SlashCommand::Compact => {
                    if orch_tx.send(RuntimeCmd::Compact).is_ok() {
                        state.arm_task_notification();
                        state.work_state = WorkState::Compacting;
                    } else {
                        state.push_line(TranscriptItem::new(
                            "Failed to send compact command.".into(),
                            TranscriptKind::Error,
                        ));
                    }
                }
                SlashCommand::Help => state.add_help(),
                SlashCommand::Skills => state.show_skills(),
                SlashCommand::Plan => state.view = View::Plan { scroll: 0 },
                SlashCommand::Todos => state.view = View::Todos { scroll: 0 },
                SlashCommand::SubAgent(session_id) => {
                    state.view = View::SubAgentDetail {
                        session_id,
                        scroll: 0,
                    }
                }
                SlashCommand::Artifact(id) => state.open_artifact(&id),
                SlashCommand::Quit => {
                    state.quit = true;
                    return true;
                }
            }
        }
        Ok(None) => {
            let input = submitted_user_input(&typed, &images);
            if orch_tx.send(RuntimeCmd::Run { input, done: None }).is_ok() {
                state.arm_task_notification();
                state.work_state = WorkState::WaitingModel;
            } else {
                // The runtime channel is closed: put the text and the queued
                // images back so a failed send never silently discards them.
                state.input.buf = typed;
                state.input.cursor = state.input.buf.len();
                state.input.pending_images = images;
                state.push_line(TranscriptItem::new(
                    "Failed to send user input.".into(),
                    TranscriptKind::Error,
                ));
            }
        }
        Err(_) => {
            if !images.is_empty() {
                state.input.pending_images = images;
            }
            state.push_line(TranscriptItem::new(
                "Unknown command. Prefix with a space to send it as text.".into(),
                TranscriptKind::Info,
            ));
        }
    }
    state.viewport.auto_scroll = true;
    false
}

#[cfg(test)]
pub(crate) fn handle_event(
    ev: Event,
    state: &mut TuiState,
    orch_tx: &tokio::sync::mpsc::UnboundedSender<RuntimeCmd>,
) -> bool {
    handle_event_for_mode(ev, state, orch_tx, TuiMode::Full)
}

pub(crate) fn handle_event_for_mode(
    ev: Event,
    state: &mut TuiState,
    orch_tx: &tokio::sync::mpsc::UnboundedSender<RuntimeCmd>,
    mode: TuiMode,
) -> bool {
    match ev {
        Event::Key(key) => return handle_key(key, state, orch_tx),
        Event::Mouse(mouse) => {
            if let Some(scroll) = view_scroll_mut(&mut state.view) {
                match mouse.kind {
                    MouseEventKind::ScrollUp => *scroll = scroll.saturating_sub(3),
                    MouseEventKind::ScrollDown => *scroll = scroll.saturating_add(3),
                    _ => {}
                }
            } else if mode == TuiMode::Full {
                match mouse.kind {
                    MouseEventKind::ScrollUp => scroll_by(state, -(SCROLL_STEP as isize)),
                    MouseEventKind::ScrollDown => scroll_by(state, SCROLL_STEP as isize),
                    MouseEventKind::Down(_) => handle_full_click(state, mouse.row),
                    _ => {}
                }
            }
        }
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

fn handle_full_click(state: &mut TuiState, mouse_row: u16) {
    if mouse_row < state.viewport.content_y {
        return;
    }
    let row = usize::from(mouse_row - state.viewport.content_y);
    let action = state
        .viewport
        .click_map
        .iter()
        .find(|target| (target.start_row..=target.end_row).contains(&row))
        .map(|target| (target.line_idx, target.action.clone()));
    match action {
        Some((idx, ClickAction::ToggleCollapse)) => {
            if let Some(item) = state.lines.get_mut(idx) {
                item.toggle_collapsed();
                state.invalidate_all_cache();
            }
        }
        Some((_, ClickAction::OpenPlan)) => state.view = View::Plan { scroll: 0 },
        Some((_, ClickAction::OpenTodos)) => state.view = View::Todos { scroll: 0 },
        Some((_, ClickAction::OpenArtifact { id })) => state.open_artifact(&id),
        Some((_, ClickAction::OpenSubAgent { session_id })) => {
            state.view = View::SubAgentDetail {
                session_id,
                scroll: 0,
            };
        }
        None => {}
    }
}
