//! TUI module for dscode using ratatui.

mod display;
mod input;
mod markdown;
mod render;
mod replay;
mod signal;
mod state;

pub use display::TuiDisplay;
pub use signal::{SubAgentStreamKind, TuiSignal};

use crate::agent::orchestrator::OrchCmd;
use input::handle_event;
use render::render;
use replay::load_session;
use signal::drain_signals;
use state::TuiState;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::mpsc;
use std::time::Duration;

pub fn run_tui(
    sig_rx: mpsc::Receiver<TuiSignal>,
    orch_tx: tokio::sync::mpsc::UnboundedSender<OrchCmd>,
    events_path: &Path,
    interrupt: Option<Arc<AtomicBool>>,
) -> anyhow::Result<()> {
    let mut terminal = ratatui::init();
    crossterm::execute!(
        std::io::stdout(),
        crossterm::event::EnableMouseCapture,
        crossterm::event::EnableBracketedPaste,
    )?;
    struct RestoreGuard;
    impl Drop for RestoreGuard {
        fn drop(&mut self) {
            let _ = crossterm::execute!(
                std::io::stdout(),
                crossterm::event::DisableBracketedPaste,
                crossterm::event::DisableMouseCapture,
            );
            ratatui::restore();
        }
    }
    let _guard = RestoreGuard;
    tui_main_loop(&mut terminal, sig_rx, orch_tx, events_path, interrupt)
}

fn tui_main_loop(
    terminal: &mut ratatui::DefaultTerminal,
    sig_rx: mpsc::Receiver<TuiSignal>,
    orch_tx: tokio::sync::mpsc::UnboundedSender<OrchCmd>,
    events_path: &Path,
    interrupt: Option<Arc<AtomicBool>>,
) -> anyhow::Result<()> {
    let mut state = TuiState {
        lines: load_session(events_path),
        interrupt,
        ..Default::default()
    };
    let mut sig_rx = sig_rx;

    loop {
        if drain_signals(&mut sig_rx, &mut state) {
            state.dirty = true;
        }
        if state.quit {
            break;
        }

        if state.dirty {
            terminal.draw(|f| render(f, &mut state))?;
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
                if handle_event(ev, &mut state, &orch_tx) {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::input::{handle_ctrl_c, handle_event};
    use crate::tui::markdown::strip_ansi;
    use crate::tui::render::{split_at_visual_width, visible_lines};
    use crate::tui::state::{MsgKind, MsgLine, TuiState, WorkState};
    use crossterm::event::{
        Event, KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
    };
    use ratatui::text::Line;
    use std::sync::atomic::{AtomicBool, Ordering};

    #[test]
    fn split_at_visual_width_respects_cjk_width() {
        assert_eq!(split_at_visual_width("ab中c", 3), vec!["ab", "中c"]);
        assert_eq!(split_at_visual_width("", 3), vec![""]);
    }

    #[test]
    fn split_at_visual_width_preserves_explicit_newlines() {
        assert_eq!(split_at_visual_width("a\nb", 10), vec!["a", "b"]);
        assert_eq!(split_at_visual_width("a\n", 10), vec!["a", ""]);
        assert_eq!(split_at_visual_width("\n", 10), vec!["", ""]);
        assert_eq!(split_at_visual_width("ab\n中c", 3), vec!["ab", "中c"]);
    }

    #[test]
    fn strip_ansi_removes_basic_sgr_sequences() {
        assert_eq!(strip_ansi("\x1b[31mred\x1b[0m plain"), "red plain");
    }

    #[test]
    fn load_session_replays_recent_turns() {
        let path = std::env::temp_dir().join(format!(
            "dscode_tui_events_{}_{}.jsonl",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let events = [
            serde_json::json!({"type":"user_input","content":"hello\nsecond line"}),
            serde_json::json!({"type":"thinking","content":"thinking"}),
            serde_json::json!({"type":"text","content":"answer"}),
            serde_json::json!({"type":"tool_call","name":"Read","input":{"file_path":"src/tui/mod.rs"}}),
        ];
        let data = events
            .iter()
            .map(serde_json::Value::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        std::fs::write(&path, data).unwrap();

        let lines = load_session(&path);
        let _ = std::fs::remove_file(path);

        assert!(lines.iter().any(|line| line.text == "> hello"));
        assert!(
            lines
                .iter()
                .any(|line| line.kind == MsgKind::StreamThinking)
        );
        assert!(lines.iter().any(|line| line.kind == MsgKind::StreamText));
        assert!(lines.iter().any(|line| line.kind == MsgKind::ToolCall));
    }

    #[test]
    fn apply_switches_stream_kinds_and_collapses_thinking() {
        let mut state = TuiState::default();

        state.apply(&TuiSignal::Thinking("think".into()));
        state.apply(&TuiSignal::Text("answer".into()));

        assert_eq!(state.lines.len(), 1);
        assert_eq!(state.lines[0].kind, MsgKind::StreamThinking);
        assert!(state.lines[0].collapsed);
        assert_eq!(state.stream_kind, MsgKind::StreamText);
        assert_eq!(state.stream_line, "answer");
        assert!(state.streaming);
        assert_eq!(state.work_state, WorkState::StreamingText);
    }

    #[test]
    fn sub_agent_output_invalidates_updated_line_cache() {
        let mut state = TuiState::default();
        state.apply(&TuiSignal::SubAgentStatus {
            session_id: "sub_1".into(),
            status: "launched".into(),
            in_tokens: 0,
            out_tokens: 0,
        });
        state.lines[0].cached_lines = Some(vec![Line::from("stale")]);
        state.cached_all = Some(vec![Line::from("stale")]);

        state.apply(&TuiSignal::SubAgentOutput {
            session_id: "sub_1".into(),
            status: "ok".into(),
            thinking: "child thinking".into(),
            text: "child text".into(),
            in_tokens: 12,
            out_tokens: 34,
        });

        assert!(state.lines[0].text.contains("ok"));
        assert!(state.lines[0].cached_lines.is_none());
        assert!(state.cached_all.is_none());
        let detail = state.lines[0].sub_detail.as_ref().unwrap();
        assert_eq!(detail.thinking, "child thinking");
        assert_eq!(detail.text, "child text");
    }

    #[test]
    fn sub_agent_status_updates_existing_line() {
        let mut state = TuiState::default();
        state.apply(&TuiSignal::SubAgentStatus {
            session_id: "sub_1".into(),
            status: "launched".into(),
            in_tokens: 0,
            out_tokens: 0,
        });
        state.lines[0].cached_lines = Some(vec![Line::from("stale")]);
        state.cached_all = Some(vec![Line::from("stale")]);

        state.apply(&TuiSignal::SubAgentStatus {
            session_id: "sub_1".into(),
            status: "running".into(),
            in_tokens: 3,
            out_tokens: 4,
        });

        assert_eq!(state.lines.len(), 1);
        assert_eq!(state.active_sub_agent_sessions.len(), 1);
        assert!(state.lines[0].text.contains("running"));
        assert!(state.lines[0].cached_lines.is_none());
        assert!(state.cached_all.is_none());
    }

    #[test]
    fn duplicate_sub_agent_output_does_not_drift_active_state() {
        let mut state = TuiState::default();
        state.apply(&TuiSignal::SubAgentStatus {
            session_id: "sub_1".into(),
            status: "launched".into(),
            in_tokens: 0,
            out_tokens: 0,
        });

        let output = TuiSignal::SubAgentOutput {
            session_id: "sub_1".into(),
            status: "ok".into(),
            thinking: String::new(),
            text: "done".into(),
            in_tokens: 1,
            out_tokens: 2,
        };
        state.apply(&output);
        state.apply(&output);

        assert!(state.active_sub_agent_sessions.is_empty());
        assert_eq!(state.work_state, WorkState::WaitingModel);
        assert_eq!(state.lines.len(), 1);
    }

    #[test]
    fn unknown_slash_command_does_not_reach_orchestrator() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let mut state = TuiState {
            input_buf: "/unknown".into(),
            input_cursor: "/unknown".len(),
            ..Default::default()
        };

        let key = crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Enter,
            crossterm::event::KeyModifiers::NONE,
        );

        assert!(!handle_event(
            crossterm::event::Event::Key(key),
            &mut state,
            &tx
        ));
        assert!(rx.try_recv().is_err());
        assert_eq!(state.work_state, WorkState::Idle);
        assert!(
            state.lines.iter().any(|line| {
                line.kind == MsgKind::Info && line.text.contains("Unknown command")
            })
        );
    }

    #[test]
    fn input_scroll_keeps_cursor_visible() {
        assert_eq!(render::clamp_input_scroll(8, 7, 5, 0), 3);
        assert_eq!(render::clamp_input_scroll(8, 1, 5, 3), 1);
        assert_eq!(render::clamp_input_scroll(8, 4, 5, 2), 2);
        assert_eq!(render::clamp_input_scroll(3, 2, 5, 9), 0);
    }

    #[test]
    fn ctrl_c_interrupts_working_state_then_exits_on_second_press() {
        let interrupt = Arc::new(AtomicBool::new(false));
        let mut state = TuiState {
            streaming: true,
            work_state: WorkState::StreamingText,
            interrupt: Some(interrupt.clone()),
            ..Default::default()
        };

        assert!(!handle_ctrl_c(&mut state));
        assert!(interrupt.load(Ordering::SeqCst));
        assert!(!state.quit);

        assert!(handle_ctrl_c(&mut state));
        assert!(state.quit);
    }

    #[test]
    fn ctrl_shift_c_interrupts_working_state() {
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let interrupt = Arc::new(AtomicBool::new(false));
        let mut state = TuiState {
            work_state: WorkState::WaitingModel,
            interrupt: Some(interrupt.clone()),
            ..Default::default()
        };

        assert!(!handle_event(
            Event::Key(KeyEvent::new(
                KeyCode::Char('C'),
                KeyModifiers::CONTROL | KeyModifiers::SHIFT,
            )),
            &mut state,
            &tx,
        ));
        assert!(interrupt.load(Ordering::SeqCst));
        assert!(!state.quit);
    }

    #[test]
    fn ctrl_c_exits_immediately_when_idle() {
        let mut state = TuiState {
            interrupt: Some(Arc::new(AtomicBool::new(false))),
            ..Default::default()
        };

        assert!(handle_ctrl_c(&mut state));
        assert!(state.quit);
    }

    #[test]
    fn mouse_click_does_not_toggle_nearest_line_when_outside_hit_range() {
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let mut state = TuiState::default();
        state
            .lines
            .push(MsgLine::new("thinking".into(), MsgKind::StreamThinking));
        state.lines[0].collapsed = true;
        state.click_map = vec![(0, 5, 5)];
        state.content_y = 0;
        state.effective_scroll = 0;

        let ev = Event::Mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 0,
            row: 1,
            modifiers: KeyModifiers::NONE,
        });

        assert!(!handle_event(ev, &mut state, &tx));
        assert!(state.lines[0].collapsed);
    }

    #[test]
    fn visible_lines_only_clones_requested_viewport_across_history_and_stream() {
        let history = vec![Line::from("h0"), Line::from("h1"), Line::from("h2")];
        let stream = vec![Line::from("s0"), Line::from("s1")];

        let lines = visible_lines(&history, &stream, 2, 3);

        let text: Vec<String> = lines
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect::<String>()
            })
            .collect();
        assert_eq!(text, vec!["h2", "s0", "s1"]);
    }

    #[test]
    fn input_supports_readline_shortcuts_and_multiline_insert() {
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let mut state = TuiState::default();
        state.input_buf = "hello world".into();
        state.input_cursor = state.input_buf.len();

        assert!(!handle_event(
            Event::Key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::CONTROL)),
            &mut state,
            &tx,
        ));
        assert_eq!(state.input_cursor, 0);

        assert!(!handle_event(
            Event::Key(KeyEvent::new(KeyCode::Char('e'), KeyModifiers::CONTROL)),
            &mut state,
            &tx,
        ));
        assert_eq!(state.input_cursor, state.input_buf.len());

        assert!(!handle_event(
            Event::Key(KeyEvent::new(KeyCode::Left, KeyModifiers::ALT)),
            &mut state,
            &tx,
        ));
        assert_eq!(state.input_cursor, "hello ".len());

        assert!(!handle_event(
            Event::Key(KeyEvent::new(KeyCode::Char('k'), KeyModifiers::CONTROL)),
            &mut state,
            &tx,
        ));
        assert_eq!(state.input_buf, "hello ");

        assert!(!handle_event(
            Event::Key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL)),
            &mut state,
            &tx,
        ));
        assert_eq!(state.input_buf, "");
        assert_eq!(state.input_cursor, 0);

        assert!(!handle_event(
            Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT)),
            &mut state,
            &tx,
        ));
        assert_eq!(state.input_buf, "\n");
        assert_eq!(state.input_cursor, 1);

        assert!(!handle_event(
            Event::Key(KeyEvent::new(KeyCode::Char('A'), KeyModifiers::SHIFT)),
            &mut state,
            &tx,
        ));
        assert_eq!(state.input_buf, "\nA");
        assert_eq!(state.input_cursor, 2);
    }
}
