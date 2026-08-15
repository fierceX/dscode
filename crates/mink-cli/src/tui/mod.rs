//! TUI module for mink using ratatui.

mod command;
mod display;
mod file_picker;
mod input;
mod markdown;
mod notify;
mod render;
mod replay;
mod sanitize;
mod signal;
mod state;
mod theme;

pub use display::{TuiDisplay, TuiSubAgentStreamSink};
pub use signal::TuiSignal;

use crate::cli::RuntimeCmd;
use crate::config::{SandboxConfig, TuiMode};
use file_picker::FilePickerPolicy;
use input::handle_event_for_mode;
use notify::send_task_notification;
use render::render;
use replay::load_session;
use signal::drain_signals;
use state::{TuiState, short_cwd_label};
use std::sync::mpsc;
use std::time::Duration;

pub fn run_tui(
    mode: TuiMode,
    sig_rx: mpsc::Receiver<TuiSignal>,
    orch_tx: tokio::sync::mpsc::UnboundedSender<RuntimeCmd>,
    session: &crate::runtime::SessionInfo,
    initial_model: &str,
    sandbox: &SandboxConfig,
) -> anyhow::Result<()> {
    match mode {
        TuiMode::Full => run_full_tui(sig_rx, orch_tx, session, initial_model, sandbox),
        TuiMode::Inline => run_inline_tui(sig_rx, orch_tx, session, initial_model, sandbox),
        TuiMode::Off => anyhow::bail!("TUI mode is disabled"),
    }
}

fn run_full_tui(
    sig_rx: mpsc::Receiver<TuiSignal>,
    orch_tx: tokio::sync::mpsc::UnboundedSender<RuntimeCmd>,
    session: &crate::runtime::SessionInfo,
    initial_model: &str,
    sandbox: &SandboxConfig,
) -> anyhow::Result<()> {
    let mut terminal = ratatui::init();
    crossterm::execute!(
        std::io::stdout(),
        crossterm::event::EnableMouseCapture,
        crossterm::event::EnableBracketedPaste,
    )?;
    struct FullRestoreGuard;
    impl Drop for FullRestoreGuard {
        fn drop(&mut self) {
            let _ = crossterm::execute!(
                std::io::stdout(),
                crossterm::event::DisableBracketedPaste,
                crossterm::event::DisableMouseCapture,
            );
            ratatui::restore();
        }
    }
    let _guard = FullRestoreGuard;
    tui_main_loop(
        &mut terminal,
        sig_rx,
        orch_tx,
        TuiLoopConfig {
            mode: TuiMode::Full,
            session,
            initial_model,
            sandbox,
        },
    )
}

fn run_inline_tui(
    sig_rx: mpsc::Receiver<TuiSignal>,
    orch_tx: tokio::sync::mpsc::UnboundedSender<RuntimeCmd>,
    session: &crate::runtime::SessionInfo,
    initial_model: &str,
    sandbox: &SandboxConfig,
) -> anyhow::Result<()> {
    let inline_height = preferred_inline_height();
    // ratatui 的 Inline viewport 初始化依赖光标位置查询（DSR `\x1b[6n`）。
    // 部分终端（dumb terminal、某些 SSH/multiplexer 环境）不响应该查询，
    // 直接 `init_with_options` 会在 `try_init_with_options` 失败时 panic。
    // 这里改用可失败的初始化，失败时降级为全屏 TUI，保证交互可用。
    let (mut terminal, effective_mode) =
        match ratatui::try_init_with_options(ratatui::TerminalOptions {
            viewport: ratatui::Viewport::Inline(inline_height),
        }) {
            Ok(terminal) => (terminal, TuiMode::Inline),
            Err(error) => {
                eprintln!("Inline TUI unavailable ({error}); falling back to fullscreen TUI.");
                crossterm::execute!(std::io::stdout(), crossterm::event::EnableMouseCapture,)?;
                (ratatui::init(), TuiMode::Full)
            }
        };
    crossterm::execute!(std::io::stdout(), crossterm::event::EnableBracketedPaste,)?;
    struct RestoreGuard;
    impl Drop for RestoreGuard {
        fn drop(&mut self) {
            let _ = crossterm::execute!(
                std::io::stdout(),
                crossterm::event::DisableBracketedPaste,
                crossterm::event::DisableMouseCapture,
                crossterm::terminal::LeaveAlternateScreen,
            );
            ratatui::restore();
        }
    }
    let _guard = RestoreGuard;
    tui_main_loop(
        &mut terminal,
        sig_rx,
        orch_tx,
        TuiLoopConfig {
            mode: effective_mode,
            session,
            initial_model,
            sandbox,
        },
    )
}

struct TuiLoopConfig<'a> {
    mode: TuiMode,
    session: &'a crate::runtime::SessionInfo,
    initial_model: &'a str,
    sandbox: &'a SandboxConfig,
}

fn tui_main_loop(
    terminal: &mut ratatui::DefaultTerminal,
    sig_rx: mpsc::Receiver<TuiSignal>,
    orch_tx: tokio::sync::mpsc::UnboundedSender<RuntimeCmd>,
    config: TuiLoopConfig<'_>,
) -> anyhow::Result<()> {
    let TuiLoopConfig {
        mode,
        session,
        initial_model,
        sandbox,
    } = config;
    let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
    let mut state = TuiState {
        lines: load_session(&session.events_path),
        cwd_label: short_cwd_label(),
        artifacts_dir: session.artifacts_dir.clone(),
        model: initial_model.to_string(),
        file_picker_policy: FilePickerPolicy::from_sandbox(cwd, sandbox),
        ..Default::default()
    };
    load_persisted_state(session, &mut state);
    let mut sig_rx = sig_rx;
    let mut saved_inline_terminal = None;

    loop {
        if drain_signals(&mut sig_rx, &mut state, mode) {
            state.dirty = true;
        }
        if mode == TuiMode::Inline {
            sync_inline_terminal_mode(terminal, &state, &mut saved_inline_terminal)?;
            if matches!(state.view, state::View::Main) && commit_ready(terminal, &mut state)? {
                state.dirty = true;
            }
        }
        if let Some(notification) = state.take_task_notification() {
            send_task_notification(&notification);
        }
        if state.quit {
            break;
        }

        if state.dirty {
            terminal.draw(|f| render(f, &mut state, mode))?;
            state.dirty = false;
        }

        let poll_timeout = if state.streaming {
            Duration::from_millis(16)
        } else {
            Duration::from_millis(100)
        };
        if crossterm::event::poll(poll_timeout).unwrap_or(false) {
            let mut should_quit = false;
            while let Ok(ev) = crossterm::event::read() {
                if handle_event_for_mode(ev, &mut state, &orch_tx, mode) {
                    should_quit = true;
                    break;
                }
                if !crossterm::event::poll(Duration::ZERO).unwrap_or(false) {
                    break;
                }
            }
            state.dirty = true;
            if should_quit {
                break;
            }
        }
    }
    Ok(())
}

fn preferred_inline_height() -> u16 {
    crossterm::terminal::size()
        .map(|(_, height)| height.saturating_sub(4).clamp(8, 12))
        .unwrap_or(10)
}

fn sync_inline_terminal_mode(
    terminal: &mut ratatui::DefaultTerminal,
    state: &TuiState,
    saved_inline_terminal: &mut Option<ratatui::DefaultTerminal>,
) -> anyhow::Result<()> {
    use crossterm::terminal::{EnterAlternateScreen, LeaveAlternateScreen};
    use ratatui::backend::CrosstermBackend;

    let wants_detail = !matches!(state.view, state::View::Main);
    if wants_detail == saved_inline_terminal.is_some() {
        return Ok(());
    }
    if wants_detail {
        crossterm::execute!(
            std::io::stdout(),
            EnterAlternateScreen,
            crossterm::event::EnableMouseCapture,
        )?;
        let detail_terminal = ratatui::Terminal::new(CrosstermBackend::new(std::io::stdout()))?;
        *saved_inline_terminal = Some(std::mem::replace(terminal, detail_terminal));
    } else {
        crossterm::execute!(
            std::io::stdout(),
            crossterm::event::DisableMouseCapture,
            LeaveAlternateScreen,
        )?;
        let inline_terminal = saved_inline_terminal
            .take()
            .ok_or_else(|| anyhow::anyhow!("inline terminal state is unavailable"))?;
        *terminal = inline_terminal;
    }
    terminal.clear()?;
    Ok(())
}

fn load_persisted_state(session: &crate::runtime::SessionInfo, state: &mut TuiState) {
    use crate::session::todo::{TodoSnapshot, TodoStatus};
    use crate::ui::{
        PlanDisplay, PlanTransitionDisplay, TodoCountsDisplay, TodoDisplay, TodoItemDisplay,
        TodoStatusDisplay,
    };

    let plan = std::fs::read_to_string(&session.plan_draft_path)
        .ok()
        .filter(|content| !content.trim().is_empty())
        .map(|content| PlanDisplay {
            transition: PlanTransitionDisplay::DraftSaved,
            content: Some(content),
        })
        .or_else(|| {
            std::fs::read_to_string(&session.plan_path)
                .ok()
                .filter(|content| !content.trim().is_empty())
                .map(|content| PlanDisplay {
                    transition: PlanTransitionDisplay::Confirmed,
                    content: Some(content),
                })
        });
    state.plan = plan;

    let Ok(bytes) = std::fs::read(&session.todos_path) else {
        state.todos = None;
        return;
    };
    let Ok(snapshot) = serde_json::from_slice::<TodoSnapshot>(&bytes) else {
        state.todos = None;
        return;
    };
    let (pending, in_progress, completed) =
        snapshot
            .items
            .iter()
            .fold((0, 0, 0), |counts, item| match item.status {
                TodoStatus::Pending => (counts.0 + 1, counts.1, counts.2),
                TodoStatus::InProgress => (counts.0, counts.1 + 1, counts.2),
                TodoStatus::Completed => (counts.0, counts.1, counts.2 + 1),
            });
    state.todos = Some(TodoDisplay {
        revision: snapshot.revision,
        counts: TodoCountsDisplay {
            pending,
            in_progress,
            completed,
        },
        items: snapshot
            .items
            .into_iter()
            .map(|item| TodoItemDisplay {
                id: item.id,
                content: item.content,
                status: match item.status {
                    TodoStatus::Pending => TodoStatusDisplay::Pending,
                    TodoStatus::InProgress => TodoStatusDisplay::InProgress,
                    TodoStatus::Completed => TodoStatusDisplay::Completed,
                },
            })
            .collect(),
        changes: Vec::new(),
    });
}

fn commit_ready<B: ratatui::backend::Backend>(
    terminal: &mut ratatui::Terminal<B>,
    state: &mut TuiState,
) -> Result<bool, B::Error> {
    use ratatui::text::Text;
    use ratatui::widgets::{Paragraph, Widget};

    let start = state.inline.committed;
    let end = committable_prefix_end(state);
    if end == start {
        return Ok(false);
    }

    let width = terminal.size()?.width.saturating_sub(2).max(1);
    for item in &state.lines[start..end] {
        let lines = render::transcript_item_lines(item, width);
        for chunk in lines.chunks(4096) {
            let owned = chunk.to_vec();
            terminal.insert_before(owned.len() as u16, move |buf| {
                Paragraph::new(Text::from(owned)).render(buf.area, buf);
            })?;
        }
    }
    state.inline.committed = end;
    state.invalidate_all_cache();
    Ok(true)
}

fn sealed_prefix_end(state: &TuiState) -> usize {
    let mut end = state.inline.committed;
    while state.lines.get(end).is_some_and(|item| item.sealed) {
        end += 1;
    }
    end
}

fn committable_prefix_end(state: &TuiState) -> usize {
    let end = sealed_prefix_end(state);
    if !state.work_state.is_working() && end == state.lines.len() && end > state.inline.committed {
        end - 1
    } else {
        end
    }
}

#[cfg(test)]
#[path = "mod_tests.rs"]
mod tests;
