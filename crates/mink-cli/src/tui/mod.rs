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

use crate::agent::orchestrator::OrchCmd;
use crate::config::SandboxConfig;
use file_picker::FilePickerPolicy;
use input::handle_event;
use notify::send_task_notification;
use render::render;
use replay::load_session;
use signal::drain_signals;
use state::{TuiState, short_cwd_label};
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
    initial_model: &str,
    sandbox: &SandboxConfig,
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
    tui_main_loop(
        &mut terminal,
        sig_rx,
        orch_tx,
        events_path,
        interrupt,
        initial_model,
        sandbox,
    )
}

fn tui_main_loop(
    terminal: &mut ratatui::DefaultTerminal,
    sig_rx: mpsc::Receiver<TuiSignal>,
    orch_tx: tokio::sync::mpsc::UnboundedSender<OrchCmd>,
    events_path: &Path,
    interrupt: Option<Arc<AtomicBool>>,
    initial_model: &str,
    sandbox: &SandboxConfig,
) -> anyhow::Result<()> {
    let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
    let mut state = TuiState {
        lines: load_session(events_path),
        cwd_label: short_cwd_label(),
        model: initial_model.to_string(),
        interrupt,
        file_picker_policy: FilePickerPolicy::from_sandbox(cwd, sandbox),
        ..Default::default()
    };
    let mut sig_rx = sig_rx;

    loop {
        if drain_signals(&mut sig_rx, &mut state) {
            state.dirty = true;
        }
        if let Some(notification) = state.take_task_notification() {
            send_task_notification(&notification);
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
    use crate::tui::command::{SlashCommand, parse_slash_command};
    use crate::tui::file_picker::{FilePickCandidate, FilePickerPolicy, FilePickerState};
    use crate::tui::input::{handle_ctrl_c, handle_event};
    use crate::tui::markdown::{
        InlineNode, MdBlock, TableAlign, TableRows, normalize_markdown_input, parse_blocks,
        push_msg, push_msg_with_tool, push_msg_with_width, render_table, strip_ansi,
        wrap_lines_word,
    };
    use crate::tui::notify::TaskNotificationKind;
    use crate::tui::render::{
        build_status_line, build_status_spans, build_visible_click_map, collapsed_summary,
        content_viewport_height, detail_lines_for_session, detail_viewport_height,
        split_at_visual_width, visible_lines,
    };
    use crate::tui::state::{
        ActiveOverlay, ClickAction, ClickTarget, CollapsePolicy, MsgKind, MsgLine, SubAgentDetail,
        TuiState, View, WorkState,
    };
    use crate::ui::{Display, ToolResultDisplay};
    use crossterm::event::{
        Event, KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
    };
    use ratatui::{
        Terminal,
        backend::TestBackend,
        style::{Color, Style},
        text::{Line, Span},
    };
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicBool, Ordering};

    #[test]
    fn tui_display_detail_uses_full_tool_result_content() {
        let (tx, rx) = std::sync::mpsc::channel();
        let display = TuiDisplay::new(tx);

        display.render_tool_result_detail(&ToolResultDisplay {
            tool_name: "Bash",
            content_preview: "short preview\n",
            content: "full output\nwith more detail",
            tool_use_id: Some("toolu_1"),
            exit_code: Some(0),
        });

        match rx.recv().unwrap() {
            TuiSignal::ToolResult { tool_name, content } => {
                assert_eq!(tool_name, "Bash");
                assert_eq!(content, "full output\nwith more detail");
            }
            other => panic!("unexpected signal: {other:?}"),
        }
    }

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
    fn strip_ansi_removes_common_non_sgr_sequences_without_swallowing_text() {
        assert_eq!(strip_ansi("a\x1b[Kb\x1b[?25lc"), "abc");
        assert_eq!(strip_ansi("x\x1b]0;title\x07y"), "xy");
        assert_eq!(strip_ansi("x\x1b]8;;https://e.test\x1b\\link"), "xlink");
    }

    #[test]
    fn markdown_normalize_cleans_control_sequences_and_line_endings() {
        assert_eq!(
            normalize_markdown_input("a\r\n\t\x1b[31mred\x1b[0m\x07\rb", false),
            "a\n    red\nb"
        );
    }

    #[test]
    fn load_session_replays_recent_turns() {
        let path = std::env::temp_dir().join(format!(
            "mink_tui_events_{}_{}.jsonl",
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
        state.cache.history_lines = Some(vec![Line::from("stale")]);

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
        assert!(state.cache.history_lines.is_none());
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
        state.cache.history_lines = Some(vec![Line::from("stale")]);

        state.apply(&TuiSignal::SubAgentStatus {
            session_id: "sub_1".into(),
            status: "running".into(),
            in_tokens: 3,
            out_tokens: 4,
        });

        assert_eq!(state.lines.len(), 1);
        assert_eq!(state.sub_agents.active_sessions.len(), 1);
        assert!(state.lines[0].text.contains("running"));
        assert!(state.lines[0].cached_lines.is_none());
        assert!(state.cache.history_lines.is_none());
    }

    #[test]
    fn sub_agent_terminal_status_clears_active_state() {
        let mut state = TuiState::default();
        state.apply(&TuiSignal::SubAgentStatus {
            session_id: "sub_1".into(),
            status: "launched".into(),
            in_tokens: 0,
            out_tokens: 0,
        });

        state.apply(&TuiSignal::SubAgentStatus {
            session_id: "sub_1".into(),
            status: "timed_out".into(),
            in_tokens: 0,
            out_tokens: 0,
        });

        assert!(state.sub_agents.active_sessions.is_empty());
        assert_eq!(state.work_state, WorkState::WaitingModel);
        assert!(state.lines[0].text.contains("timed_out"));
    }

    #[test]
    fn user_task_stop_emits_completion_notification() {
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let mut state = TuiState::default();
        state.model = "pro".into();
        state.input.buf = "do the task".into();
        state.input.cursor = state.input.buf.len();

        assert!(!handle_event(
            Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
            &mut state,
            &tx,
        ));
        assert!(state.take_task_notification().is_none());

        state.apply(&TuiSignal::Stop);

        let notification = state.take_task_notification().unwrap();
        assert_eq!(notification.kind, TaskNotificationKind::Completed);
        assert_eq!(notification.title, "mink 任务完成");
        assert!(notification.body.contains("pro"));
        assert!(state.take_task_notification().is_none());
    }

    #[test]
    fn user_task_error_emits_failure_notification() {
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let mut state = TuiState::default();
        state.input.buf = "do the task".into();
        state.input.cursor = state.input.buf.len();

        assert!(!handle_event(
            Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
            &mut state,
            &tx,
        ));

        state.apply(&TuiSignal::Error("network timeout".into()));

        let notification = state.take_task_notification().unwrap();
        assert_eq!(notification.kind, TaskNotificationKind::Failed);
        assert_eq!(notification.title, "mink 任务失败");
    }

    #[test]
    fn local_help_does_not_emit_completion_notification() {
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let mut state = TuiState::default();
        state.input.buf = "/help".into();
        state.input.cursor = state.input.buf.len();

        assert!(!handle_event(
            Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
            &mut state,
            &tx,
        ));

        state.apply(&TuiSignal::Stop);

        assert!(state.take_task_notification().is_none());
    }

    #[test]
    fn compact_stop_emits_completion_notification() {
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let mut state = TuiState::default();
        state.input.buf = "/compact".into();
        state.input.cursor = state.input.buf.len();

        assert!(!handle_event(
            Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
            &mut state,
            &tx,
        ));
        assert_eq!(state.work_state, WorkState::Compacting);

        state.apply(&TuiSignal::Stop);

        let notification = state.take_task_notification().unwrap();
        assert_eq!(notification.kind, TaskNotificationKind::Completed);
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

        assert!(state.sub_agents.active_sessions.is_empty());
        assert_eq!(state.work_state, WorkState::WaitingModel);
        assert_eq!(state.lines.len(), 1);
    }

    #[test]
    fn unknown_slash_command_does_not_reach_orchestrator() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let mut state = TuiState::default();
        state.input.buf = "/unknown".into();
        state.input.cursor = "/unknown".len();

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
    fn content_viewports_leave_light_padding_when_space_allows() {
        assert_eq!(content_viewport_height(10), 9);
        assert_eq!(content_viewport_height(2), 1);
        assert_eq!(content_viewport_height(1), 1);
        assert_eq!(detail_viewport_height(10), 9);
        assert_eq!(detail_viewport_height(2), 1);
        assert_eq!(detail_viewport_height(1), 1);
    }

    #[test]
    fn slash_command_parser_classifies_known_unknown_and_text() {
        assert_eq!(
            parse_slash_command("/flash").unwrap(),
            Some(SlashCommand::Flash)
        );
        assert_eq!(parse_slash_command("/q").unwrap(), Some(SlashCommand::Quit));
        assert_eq!(parse_slash_command(" /flash").unwrap(), None);
        assert!(parse_slash_command("/unknown").is_err());
    }

    #[test]
    fn wrap_lines_word_preserves_span_styles_across_wraps() {
        let red = Style::default().fg(Color::Red);
        let green = Style::default().fg(Color::Green);
        let lines = vec![Line::from(vec![
            Span::styled("abc", red),
            Span::styled("def", green),
        ])];

        let wrapped = wrap_lines_word(&lines, 4);

        assert_eq!(wrapped.len(), 2);
        assert_eq!(wrapped[0].spans[0].style, red);
        assert_eq!(wrapped[0].spans[1].style, green);
        assert_eq!(wrapped[1].spans[0].style, green);
        assert_eq!(line_text(&wrapped[0]), "abcd");
        assert_eq!(line_text(&wrapped[1]), "ef");
    }

    #[test]
    fn markdown_renderer_handles_heading_list_and_inline_code() {
        let mut lines = Vec::new();

        push_msg(&mut lines, "# Title\n- item `code`", MsgKind::Text);

        assert_eq!(line_text(&lines[0]), "Title");
        assert_eq!(line_text(&lines[1]), "- item code");
        assert!(
            lines[0].spans[0]
                .style
                .add_modifier
                .contains(ratatui::style::Modifier::BOLD)
        );
        assert_eq!(lines[1].spans[0].style.fg, Some(Color::Yellow));
        assert_eq!(lines[1].spans[3].content.as_ref(), "code");
        assert_eq!(lines[1].spans[3].style.fg, Some(Color::Yellow));
    }

    #[test]
    fn markdown_parser_builds_block_ir_for_core_blocks() {
        let blocks =
            parse_blocks("# Title\n\n> quote\n- item `code`\n```rust\nfn main() {}\n```\nplain");

        assert!(matches!(
            &blocks[0],
            MdBlock::Heading {
                level: 1,
                content
            } if content == &vec![InlineNode::Text("Title".into())]
        ));
        assert!(matches!(blocks[1], MdBlock::Blank));
        assert!(matches!(
            &blocks[2],
            MdBlock::BlockQuote(content)
                if content == &vec![InlineNode::Text("quote".into())]
        ));
        assert!(matches!(
            &blocks[3],
            MdBlock::ListItem { marker, content }
                if marker == "-" && content == &vec![
                    InlineNode::Text("item ".into()),
                    InlineNode::Code("code".into())
                ]
        ));
        assert!(matches!(
            &blocks[4],
            MdBlock::CodeBlock { lang, lines }
                if lang.as_deref() == Some("rust") && lines == &vec!["fn main() {}".to_string()]
        ));
        assert!(matches!(
            &blocks[5],
            MdBlock::Paragraph(content)
                if content == &vec![InlineNode::Text("plain".into())]
        ));
    }

    #[test]
    fn markdown_parser_keeps_unclosed_code_fence_as_code_block() {
        let blocks = parse_blocks("```text\nopen\nstill open");

        assert_eq!(blocks.len(), 1);
        assert!(matches!(
            &blocks[0],
            MdBlock::CodeBlock { lang, lines }
                if lang.as_deref() == Some("text")
                    && lines == &vec!["open".to_string(), "still open".to_string()]
        ));
    }

    #[test]
    fn markdown_renderer_aligns_pipe_tables() {
        let mut lines = Vec::new();

        push_msg(
            &mut lines,
            "| Name | Count | Note |\n| :--- | ---: | :---: |\n| 中 | 2 | ok |\n| long-name | 10 | yes |",
            MsgKind::Text,
        );

        assert_eq!(line_text(&lines[0]), "Name      │ Count │ Note");
        assert_eq!(line_text(&lines[1]), "──────────┼───────┼─────");
        assert_eq!(line_text(&lines[2]), "中        │     2 │  ok ");
        assert_eq!(line_text(&lines[3]), "long-name │    10 │ yes ");
        assert!(
            lines[0].spans[0]
                .style
                .add_modifier
                .contains(ratatui::style::Modifier::BOLD)
        );
    }

    #[test]
    fn markdown_renderer_keeps_escaped_pipe_inside_table_cell() {
        let mut lines = Vec::new();

        push_msg(
            &mut lines,
            "| Pattern | Meaning |\n| --- | --- |\n| `a\\|b` | escaped pipe |",
            MsgKind::Text,
        );

        assert_eq!(line_text(&lines[0]), "Pattern │ Meaning     ");
        assert_eq!(line_text(&lines[2]), "`a|b`   │ escaped pipe");
    }

    #[test]
    fn markdown_table_wraps_long_cells_without_omitting_content() {
        let mut lines = Vec::new();

        push_msg_with_width(
            &mut lines,
            "| Key | Description |\n| --- | --- |\n| row | abcdefghijklmnopqrstuvwxyz0123456789 |",
            MsgKind::Text,
            24,
            None,
        );

        let rendered = lines.iter().map(line_text).collect::<Vec<_>>().join("\n");
        assert!(!rendered.contains('…'));
        assert!(rendered.contains("abcdefghijklmnopqr"));
        assert!(rendered.contains("stuvwxyz0123456789"));
    }

    #[test]
    fn markdown_table_uses_available_width_before_wrapping_cells() {
        let mut lines = Vec::new();

        push_msg_with_width(
            &mut lines,
            "| Key | Description |\n| --- | --- |\n| row | abcdefghijklmnopqrstuvwxyz0123456789 |",
            MsgKind::Text,
            80,
            None,
        );

        assert_eq!(lines.len(), 3);
        assert!(line_text(&lines[2]).contains("abcdefghijklmnopqrstuvwxyz0123456789"));
    }

    #[test]
    fn markdown_table_renderer_falls_back_for_invalid_table_rows() {
        let mut lines = Vec::new();
        let table = TableRows {
            header: vec!["A".into(), "B".into()],
            alignments: vec![TableAlign::Left],
            rows: vec![vec!["1".into(), "2".into()], vec!["too-short".into()]],
        };

        render_table(&mut lines, &table, Style::default(), 80);

        assert_eq!(line_text(&lines[0]), "A | B");
        assert_eq!(line_text(&lines[1]), "1 | 2");
        assert_eq!(line_text(&lines[2]), "too-short");
    }

    #[test]
    fn markdown_renderer_styles_strong_emphasis_and_links() {
        let mut lines = Vec::new();

        push_msg(
            &mut lines,
            "Use **bold** and *em* plus [docs](https://example.com)",
            MsgKind::Text,
        );

        assert_eq!(
            line_text(&lines[0]),
            "Use bold and em plus docs (https://example.com)"
        );
        assert!(
            lines[0].spans[1]
                .style
                .add_modifier
                .contains(ratatui::style::Modifier::BOLD)
        );
        assert!(
            lines[0].spans[3]
                .style
                .add_modifier
                .contains(ratatui::style::Modifier::ITALIC)
        );
        assert_eq!(lines[0].spans[5].style.fg, Some(Color::Blue));
        assert!(
            lines[0].spans[5]
                .style
                .add_modifier
                .contains(ratatui::style::Modifier::UNDERLINED)
        );
        assert_eq!(lines[0].spans[6].style.fg, Some(Color::DarkGray));
    }

    #[test]
    fn markdown_renderer_keeps_blockquote_inline_styles() {
        let mut lines = Vec::new();

        push_msg(
            &mut lines,
            "> quoted **bold** and [docs](https://example.com/a_(b))",
            MsgKind::Text,
        );

        assert_eq!(
            line_text(&lines[0]),
            "| quoted bold and docs (https://example.com/a_(b))"
        );
        assert_eq!(lines[0].spans[0].style.fg, Some(Color::DarkGray));
        assert!(
            lines[0].spans[2]
                .style
                .add_modifier
                .contains(ratatui::style::Modifier::BOLD)
        );
        assert_eq!(lines[0].spans[4].style.fg, Some(Color::Blue));
        assert!(
            lines[0].spans[4]
                .style
                .add_modifier
                .contains(ratatui::style::Modifier::UNDERLINED)
        );
    }

    #[test]
    fn colored_tool_diff_is_detected_after_normalization() {
        let mut lines = Vec::new();

        push_msg_with_tool(
            &mut lines,
            "\x1b[31m--- a/file\x1b[0m\n\x1b[32m+++ b/file\x1b[0m\n@@ -1 +1 @@\n-old\n+new",
            MsgKind::ToolResult,
            "Edit",
        );

        assert_eq!(line_text(&lines[0]), "--- a/file");
        assert_eq!(line_text(&lines[1]), "+++ b/file");
        assert_eq!(lines[0].spans[0].style.fg, Some(Color::Yellow));
        assert_eq!(lines[2].spans[0].style.fg, Some(Color::Cyan));
        assert_eq!(lines[3].spans[0].style.fg, Some(Color::Red));
        assert_eq!(lines[4].spans[0].style.fg, Some(Color::Green));
    }

    #[test]
    fn malformed_table_separator_falls_back_to_plain_lines() {
        let mut lines = Vec::new();

        push_msg(
            &mut lines,
            "| A | B |\n| nope | --- |\nplain",
            MsgKind::Text,
        );

        assert_eq!(line_text(&lines[0]), "| A | B |");
        assert_eq!(line_text(&lines[1]), "| nope | --- |");
        assert_eq!(line_text(&lines[2]), "plain");
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
        state.viewport.click_map = vec![ClickTarget {
            line_idx: 0,
            start_row: 5,
            end_row: 5,
            action: ClickAction::ToggleCollapse,
        }];
        state.viewport.content_y = 0;
        state.viewport.effective_scroll = 0;

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
    fn mouse_click_uses_area_top_as_content_row() {
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let mut state = TuiState::default();
        state
            .lines
            .push(MsgLine::new("thinking".into(), MsgKind::StreamThinking));
        state.lines[0].collapsed = true;
        state.viewport.click_map = vec![ClickTarget {
            line_idx: 0,
            start_row: 0,
            end_row: 0,
            action: ClickAction::ToggleCollapse,
        }];
        state.viewport.content_y = 0;

        assert!(!handle_event(
            Event::Mouse(MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: 0,
                row: 0,
                modifiers: KeyModifiers::NONE,
            }),
            &mut state,
            &tx,
        ));
        assert!(!state.lines[0].collapsed);
    }

    #[test]
    fn long_tool_result_defaults_collapsed_and_can_be_clicked_open() {
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let mut state = TuiState::default();
        let text = (0..25)
            .map(|i| format!("line {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        state.lines.push(MsgLine::new(text, MsgKind::ToolResult));
        state.lines[0].cached_lines = Some(vec![Line::from("stale")]);
        state.cache.history_lines = Some(vec![Line::from("stale")]);
        state.viewport.click_map = vec![ClickTarget {
            line_idx: 0,
            start_row: 0,
            end_row: 0,
            action: ClickAction::ToggleCollapse,
        }];

        assert!(state.lines[0].collapsed);
        assert_eq!(
            state.lines[0].collapse_policy,
            CollapsePolicy::Auto {
                threshold_lines: 20
            }
        );
        assert!(!handle_event(
            Event::Mouse(MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: 0,
                row: 0,
                modifiers: KeyModifiers::NONE,
            }),
            &mut state,
            &tx,
        ));
        assert!(!state.lines[0].collapsed);
        assert!(state.lines[0].cached_lines.is_none());
        assert!(state.cache.history_lines.is_none());
    }

    #[test]
    fn short_tool_result_does_not_default_to_collapsed() {
        let line = MsgLine::new("short\noutput".into(), MsgKind::ToolResult);

        assert!(!line.collapsed);
        assert_eq!(
            line.collapse_policy,
            CollapsePolicy::Auto {
                threshold_lines: 20
            }
        );
    }

    #[test]
    fn collapsed_summary_includes_tool_result_line_count() {
        let text = (0..25)
            .map(|i| format!("line {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let line = MsgLine::new_tool_result("Bash".into(), text);

        let summary = collapsed_summary(&line, 80);

        assert!(summary.contains("Bash: 25 lines"));
        assert!(summary.contains("bytes"));
        assert!(summary.contains("line 0"));
    }

    #[test]
    fn collapsed_summary_marks_truncated_tool_result() {
        let line = MsgLine::new_tool_result(
            "Read".into(),
            "first\n\n[... truncated: showing first/last portions of 10000 bytes ...]\nlast".into(),
        );

        let summary = collapsed_summary(&line, 120);

        assert!(summary.contains("Read: 4 lines"));
        assert!(summary.contains("truncated"));
        assert!(summary.contains("first"));
    }

    #[test]
    fn tool_result_signal_preserves_tool_name_for_summaries() {
        let mut state = TuiState::default();

        state.apply(&TuiSignal::ToolResult {
            tool_name: "Edit".into(),
            content: "changed file".into(),
        });

        assert_eq!(state.lines.len(), 1);
        assert_eq!(state.lines[0].tool_name.as_deref(), Some("Edit"));
        assert_eq!(state.lines[0].kind, MsgKind::ToolResult);
    }

    #[test]
    fn long_wrapped_tool_result_auto_collapses_until_user_overrides() {
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let mut state = TuiState::default();
        state
            .lines
            .push(MsgLine::new_tool_result("Bash".into(), "x".repeat(1000)));
        assert!(state.lines[0].collapsed);

        let backend = TestBackend::new(40, 30);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|f| render(f, &mut state)).unwrap();

        assert!(state.lines[0].collapsed);
        assert!(!state.lines[0].collapse_overridden);
        assert!(!state.viewport.click_map.is_empty());

        assert!(!handle_event(
            Event::Mouse(MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: 0,
                row: state.viewport.content_y,
                modifiers: KeyModifiers::NONE,
            }),
            &mut state,
            &tx,
        ));
        assert!(!state.lines[0].collapsed);
        assert!(state.lines[0].collapse_overridden);

        terminal.draw(|f| render(f, &mut state)).unwrap();

        assert!(!state.lines[0].collapsed);
    }

    #[test]
    fn visible_click_map_keeps_only_viewport_relative_targets() {
        let mut state = TuiState::default();
        state
            .lines
            .push(MsgLine::new("hidden".into(), MsgKind::StreamThinking));
        state
            .lines
            .push(MsgLine::new("visible".into(), MsgKind::StreamThinking));
        state
            .lines
            .push(MsgLine::new("below".into(), MsgKind::StreamThinking));
        state.lines[0].cached_lines = Some(vec![Line::from("h0"), Line::from("h1")]);
        state.lines[1].cached_lines = Some(vec![Line::from("v0"), Line::from("v1")]);
        state.lines[2].cached_lines = Some(vec![Line::from("b0")]);

        let targets = build_visible_click_map(&state, 2, 2);

        assert_eq!(targets.len(), 1);
        assert_eq!(targets[0].line_idx, 1);
        assert_eq!(targets[0].start_row, 0);
        assert_eq!(targets[0].end_row, 1);
        assert_eq!(targets[0].action, ClickAction::ToggleCollapse);
    }

    #[test]
    fn status_line_adapts_to_terminal_width() {
        let mut state = TuiState {
            work_state: WorkState::StreamingText,
            cwd_label: "mink-new".into(),
            ..Default::default()
        };
        state.stats.current_turn_count = 12;
        state.stats.agent_request_count = 18;
        state.stats.total_input_tokens = 14_000;
        state.stats.total_cache_read_tokens = 2_000;
        state.stats.total_output_tokens = 3_000;
        state.stats.current_context_tokens = 58_000;
        state.stats.max_context_tokens = 80_000;
        state.stats.belief = 0.75;

        let narrow = build_status_line(&state, 40);
        let medium = build_status_line(&state, 80);
        let wide = build_status_line(&state, 120);

        assert!(unicode_width::UnicodeWidthStr::width(narrow.as_str()) <= 40);
        assert!(unicode_width::UnicodeWidthStr::width(medium.as_str()) <= 80);
        assert!(unicode_width::UnicodeWidthStr::width(wide.as_str()) <= 120);
        assert!(!narrow.contains(" T:"));
        assert!(!narrow.contains("@mink-new"));
        assert!(medium.contains(" I:"));
        assert!(medium.contains("@mink-new"));
        assert!(wide.contains(" T:12"));
        assert!(wide.contains(" R:18"));
    }

    #[test]
    fn status_spans_style_path_and_work_state() {
        let mut state = TuiState {
            model: "deepseek-chat".into(),
            cwd_label: "mink-new".into(),
            work_state: WorkState::RunningTool,
            ..Default::default()
        };
        state.stats.belief = 0.75;

        let line = build_status_spans(&state, 120);

        assert_eq!(line_text(&line), build_status_line(&state, 120));
        assert_eq!(line.spans[1].style.fg, Some(Color::Blue));
        assert!(
            line.spans[1]
                .style
                .add_modifier
                .contains(ratatui::style::Modifier::BOLD)
        );
        let path_span = line
            .spans
            .iter()
            .find(|span| span.content.as_ref() == "@mink-new")
            .unwrap();
        assert_eq!(path_span.style.fg, Some(Color::DarkGray));
        let work_span = line.spans.last().unwrap();
        assert_eq!(work_span.content.as_ref(), "[tool]");
        assert_eq!(work_span.style.fg, Some(Color::Yellow));
        assert!(
            work_span
                .style
                .add_modifier
                .contains(ratatui::style::Modifier::BOLD)
        );
    }

    #[test]
    fn status_spans_color_error_state_as_error() {
        let state = TuiState {
            work_state: WorkState::Error,
            cwd_label: "mink-new".into(),
            ..Default::default()
        };

        let line = build_status_spans(&state, 120);
        let work_span = line.spans.last().unwrap();

        assert_eq!(work_span.content.as_ref(), "[error]");
        assert_eq!(work_span.style.fg, Some(Color::Red));
        assert!(
            work_span
                .style
                .add_modifier
                .contains(ratatui::style::Modifier::BOLD)
        );
    }

    #[test]
    fn mouse_click_on_sub_agent_opens_detail_by_session_id() {
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let mut state = TuiState::default();
        state.lines.push(
            MsgLine::new("sub".into(), MsgKind::SubAgent).with_sub_detail(Some(SubAgentDetail {
                thinking: String::new(),
                text: String::new(),
            })),
        );
        state.sub_agents.line_by_session.insert("sub_1".into(), 0);
        state.viewport.click_map = vec![ClickTarget {
            line_idx: 0,
            start_row: 0,
            end_row: 0,
            action: ClickAction::OpenSubAgentDetail {
                session_id: "sub_1".into(),
            },
        }];
        state.viewport.content_y = 0;
        state.viewport.effective_scroll = 0;

        assert!(!handle_event(
            Event::Mouse(MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: 0,
                row: 0,
                modifiers: KeyModifiers::NONE,
            }),
            &mut state,
            &tx,
        ));

        assert!(matches!(
            state.view,
            View::SubAgentDetail {
                ref session_id,
                scroll: 0
            } if session_id == "sub_1"
        ));
    }

    #[test]
    fn sub_agent_detail_tracks_session_after_output_update() {
        let mut state = TuiState::default();
        state.apply(&TuiSignal::SubAgentStatus {
            session_id: "sub_1".into(),
            status: "launched".into(),
            in_tokens: 0,
            out_tokens: 0,
        });
        state.view = View::SubAgentDetail {
            session_id: "sub_1".into(),
            scroll: 0,
        };

        state.apply(&TuiSignal::SubAgentOutput {
            session_id: "sub_1".into(),
            status: "ok".into(),
            thinking: "child thinking".into(),
            text: "child text".into(),
            in_tokens: 12,
            out_tokens: 34,
        });

        assert!(matches!(
            state.view,
            View::SubAgentDetail {
                ref session_id,
                scroll: 0
            } if session_id == "sub_1"
        ));
        let detail = detail_lines_for_session(&state, "sub_1");
        let text: Vec<String> = detail.iter().map(line_text).collect();
        assert!(text.iter().any(|line| line.contains("ok")));
        assert!(text.iter().any(|line| line == "child thinking"));
        assert!(text.iter().any(|line| line == "child text"));
    }

    #[test]
    fn sub_agent_detail_missing_session_renders_fallback() {
        let state = TuiState::default();

        let detail = detail_lines_for_session(&state, "missing");

        assert_eq!(detail.len(), 1);
        assert!(line_text(&detail[0]).contains("missing"));
    }

    #[test]
    fn visible_lines_only_clones_requested_viewport_across_history_and_stream() {
        let history = vec![Line::from("h0"), Line::from("h1"), Line::from("h2")];
        let stream = vec![Line::from("s0"), Line::from("s1")];

        let lines = visible_lines(&history, &stream, 2, 3);

        let text: Vec<String> = lines.iter().map(line_text).collect();
        assert_eq!(text, vec!["h2", "s0", "s1"]);
    }

    #[test]
    fn input_supports_readline_shortcuts_and_multiline_insert() {
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let mut state = TuiState::default();
        state.input.buf = "hello world".into();
        state.input.cursor = state.input.buf.len();

        assert!(!handle_event(
            Event::Key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::CONTROL)),
            &mut state,
            &tx,
        ));
        assert_eq!(state.input.cursor, 0);

        assert!(!handle_event(
            Event::Key(KeyEvent::new(KeyCode::Char('e'), KeyModifiers::CONTROL)),
            &mut state,
            &tx,
        ));
        assert_eq!(state.input.cursor, state.input.buf.len());

        assert!(!handle_event(
            Event::Key(KeyEvent::new(KeyCode::Left, KeyModifiers::ALT)),
            &mut state,
            &tx,
        ));
        assert_eq!(state.input.cursor, "hello ".len());

        assert!(!handle_event(
            Event::Key(KeyEvent::new(KeyCode::Char('k'), KeyModifiers::CONTROL)),
            &mut state,
            &tx,
        ));
        assert_eq!(state.input.buf, "hello ");

        assert!(!handle_event(
            Event::Key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL)),
            &mut state,
            &tx,
        ));
        assert_eq!(state.input.buf, "");
        assert_eq!(state.input.cursor, 0);

        assert!(!handle_event(
            Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT)),
            &mut state,
            &tx,
        ));
        assert_eq!(state.input.buf, "\n");
        assert_eq!(state.input.cursor, 1);

        assert!(!handle_event(
            Event::Key(KeyEvent::new(KeyCode::Char('A'), KeyModifiers::SHIFT)),
            &mut state,
            &tx,
        ));
        assert_eq!(state.input.buf, "\nA");
        assert_eq!(state.input.cursor, 2);
    }

    #[test]
    fn paste_normalizes_control_text_before_inserting() {
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let mut state = TuiState::default();

        assert!(!handle_event(
            Event::Paste("a\r\n\tb\x07c".into()),
            &mut state,
            &tx,
        ));

        assert_eq!(state.input.buf, "a\n    bc");
        assert_eq!(state.input.cursor, state.input.buf.len());
    }

    #[test]
    fn tab_opens_file_picker_overlay() {
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let mut state = TuiState::default();
        state.input.buf = "src/tui/in".into();
        state.input.cursor = state.input.buf.len();

        assert!(!handle_event(
            Event::Key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE)),
            &mut state,
            &tx,
        ));

        assert!(matches!(state.overlay, Some(ActiveOverlay::FilePicker(_))));
    }

    #[test]
    fn file_picker_accept_replaces_current_path_token() {
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let mut state = TuiState::default();
        state.input.buf = "Read @src/tui/in".into();
        state.input.cursor = state.input.buf.len();
        state.overlay = Some(ActiveOverlay::FilePicker(
            FilePickerState::open_with_candidates(
                &state.input.buf,
                state.input.cursor,
                vec![
                    FilePickCandidate {
                        path: "src/tui/input.rs".into(),
                        is_dir: false,
                    },
                    FilePickCandidate {
                        path: "src/tui/render/input.rs".into(),
                        is_dir: false,
                    },
                ],
            ),
        ));

        assert!(!handle_event(
            Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
            &mut state,
            &tx,
        ));

        assert_eq!(state.input.buf, "Read @src/tui/input.rs");
        assert_eq!(state.input.cursor, state.input.buf.len());
        assert!(state.overlay.is_none());
    }

    #[test]
    fn file_picker_accept_replaces_entire_current_path_token() {
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let mut state = TuiState::default();
        state.input.buf = "Read src/tui/in_suffix".into();
        state.input.cursor = "Read src/tui/in".len();
        state.overlay = Some(ActiveOverlay::FilePicker(
            FilePickerState::open_with_candidates(
                &state.input.buf,
                state.input.cursor,
                vec![FilePickCandidate {
                    path: "src/tui/input.rs".into(),
                    is_dir: false,
                }],
            ),
        ));

        assert!(!handle_event(
            Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
            &mut state,
            &tx,
        ));

        assert_eq!(state.input.buf, "Read src/tui/input.rs");
        assert_eq!(state.input.cursor, state.input.buf.len());
    }

    #[test]
    fn file_picker_accept_preserves_read_selector_suffix() {
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let mut state = TuiState::default();
        state.input.buf = "Read src/tui/in:10-20".into();
        state.input.cursor = "Read src/tui/in".len();
        state.overlay = Some(ActiveOverlay::FilePicker(
            FilePickerState::open_with_candidates(
                &state.input.buf,
                state.input.cursor,
                vec![FilePickCandidate {
                    path: "src/tui/input.rs".into(),
                    is_dir: false,
                }],
            ),
        ));

        assert!(!handle_event(
            Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
            &mut state,
            &tx,
        ));

        assert_eq!(state.input.buf, "Read src/tui/input.rs:10-20");
        assert_eq!(state.input.cursor, "Read src/tui/input.rs".len());
    }

    #[test]
    fn file_picker_accept_preserves_selector_when_cursor_is_after_selector() {
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let mut state = TuiState::default();
        state.input.buf = "Read src/tui/in:raw".into();
        state.input.cursor = state.input.buf.len();
        state.overlay = Some(ActiveOverlay::FilePicker(
            FilePickerState::open_with_candidates(
                &state.input.buf,
                state.input.cursor,
                vec![FilePickCandidate {
                    path: "src/tui/input.rs".into(),
                    is_dir: false,
                }],
            ),
        ));

        assert!(!handle_event(
            Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
            &mut state,
            &tx,
        ));

        assert_eq!(state.input.buf, "Read src/tui/input.rs:raw");
        assert_eq!(state.input.cursor, "Read src/tui/input.rs".len());
    }

    #[test]
    fn file_picker_accept_preserves_raw_range_selector() {
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let mut state = TuiState::default();
        state.input.buf = "Read src/tui/in:raw:10-20".into();
        state.input.cursor = "Read src/tui/in".len();
        state.overlay = Some(ActiveOverlay::FilePicker(
            FilePickerState::open_with_candidates(
                &state.input.buf,
                state.input.cursor,
                vec![FilePickCandidate {
                    path: "src/tui/input.rs".into(),
                    is_dir: false,
                }],
            ),
        ));

        assert!(!handle_event(
            Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
            &mut state,
            &tx,
        ));

        assert_eq!(state.input.buf, "Read src/tui/input.rs:raw:10-20");
        assert_eq!(state.input.cursor, "Read src/tui/input.rs".len());
    }

    #[test]
    fn file_picker_accept_does_not_treat_colon_filename_as_selector() {
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let mut state = TuiState::default();
        state.input.buf = "Read notes/report:2026.md".into();
        state.input.cursor = "Read notes/report".len();
        state.overlay = Some(ActiveOverlay::FilePicker(
            FilePickerState::open_with_candidates(
                &state.input.buf,
                state.input.cursor,
                vec![FilePickCandidate {
                    path: "notes/report-final.md".into(),
                    is_dir: false,
                }],
            ),
        ));

        assert!(!handle_event(
            Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
            &mut state,
            &tx,
        ));

        assert_eq!(state.input.buf, "Read notes/report-final.md");
        assert_eq!(state.input.cursor, state.input.buf.len());
    }

    #[test]
    fn file_picker_accept_does_not_treat_alphanumeric_suffix_as_selector() {
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let mut state = TuiState::default();
        state.input.buf = "Read src/tui/in:10abc".into();
        state.input.cursor = "Read src/tui/in".len();
        state.overlay = Some(ActiveOverlay::FilePicker(
            FilePickerState::open_with_candidates(
                &state.input.buf,
                state.input.cursor,
                vec![FilePickCandidate {
                    path: "src/tui/input.rs".into(),
                    is_dir: false,
                }],
            ),
        ));

        assert!(!handle_event(
            Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
            &mut state,
            &tx,
        ));

        assert_eq!(state.input.buf, "Read src/tui/input.rs");
        assert_eq!(state.input.cursor, state.input.buf.len());
    }

    #[test]
    fn file_picker_accept_does_not_treat_zero_line_as_selector() {
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let mut state = TuiState::default();
        state.input.buf = "Read src/tui/in:0".into();
        state.input.cursor = "Read src/tui/in".len();
        state.overlay = Some(ActiveOverlay::FilePicker(
            FilePickerState::open_with_candidates(
                &state.input.buf,
                state.input.cursor,
                vec![FilePickCandidate {
                    path: "src/tui/input.rs".into(),
                    is_dir: false,
                }],
            ),
        ));

        assert!(!handle_event(
            Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
            &mut state,
            &tx,
        ));

        assert_eq!(state.input.buf, "Read src/tui/input.rs");
        assert_eq!(state.input.cursor, state.input.buf.len());
    }

    #[test]
    fn file_picker_parent_prefix_scans_parent_directory() {
        let root = unique_test_dir("parent-scan");
        let cwd = root.join("workspace");
        let shared = root.join("shared");
        std::fs::create_dir_all(&cwd).unwrap();
        std::fs::create_dir_all(&shared).unwrap();
        std::fs::write(shared.join("lib.rs"), "fn main() {}\n").unwrap();
        let policy = FilePickerPolicy::restricted_for_tests(cwd, vec![root.clone()], 3);

        let picker = FilePickerState::open("../shared/l", "../shared/l".len(), &policy);

        assert!(
            picker
                .items
                .iter()
                .any(|item| item.path == "../shared/lib.rs")
        );
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn file_picker_parent_prefix_respects_depth_limit() {
        let root = unique_test_dir("parent-depth");
        let cwd = root.join("workspace");
        std::fs::create_dir_all(&cwd).unwrap();
        let policy = FilePickerPolicy::restricted_for_tests(cwd, vec![root.clone()], 1);

        let picker = FilePickerState::open("../../", "../../".len(), &policy);

        assert!(picker.items.is_empty());
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn file_picker_parent_prefix_respects_restricted_roots() {
        let root = unique_test_dir("parent-restricted");
        let cwd = root.join("workspace");
        let shared = root.join("shared");
        std::fs::create_dir_all(&cwd).unwrap();
        std::fs::create_dir_all(&shared).unwrap();
        std::fs::write(shared.join("lib.rs"), "fn main() {}\n").unwrap();
        let policy = FilePickerPolicy::restricted_for_tests(cwd.clone(), vec![cwd], 3);

        let picker = FilePickerState::open("../shared/l", "../shared/l".len(), &policy);

        assert!(picker.items.is_empty());
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn file_picker_refresh_rescans_when_parent_root_changes() {
        let root = unique_test_dir("parent-refresh");
        let cwd = root.join("workspace");
        let shared = root.join("shared");
        std::fs::create_dir_all(&cwd).unwrap();
        std::fs::create_dir_all(&shared).unwrap();
        std::fs::write(shared.join("lib.rs"), "fn main() {}\n").unwrap();
        let policy = FilePickerPolicy::restricted_for_tests(cwd, vec![root.clone()], 3);
        let mut picker = FilePickerState::open("", 0, &policy);

        picker.refresh_with_policy("../shared/l", "../shared/l".len(), &policy);

        assert!(
            picker
                .items
                .iter()
                .any(|item| item.path == "../shared/lib.rs")
        );
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn file_picker_parent_prefix_allows_sandbox_sibling_root() {
        let root = unique_test_dir("parent-allowed-sibling");
        let cwd = root.join("workspace");
        let shared = root.join("shared");
        let other = root.join("other");
        std::fs::create_dir_all(&cwd).unwrap();
        std::fs::create_dir_all(&shared).unwrap();
        std::fs::create_dir_all(&other).unwrap();
        std::fs::write(shared.join("lib.rs"), "fn main() {}\n").unwrap();
        std::fs::write(other.join("secret.rs"), "fn main() {}\n").unwrap();
        let policy = FilePickerPolicy::restricted_for_tests(cwd, vec![shared.clone()], 3);

        let picker = FilePickerState::open("../", "../".len(), &policy);

        assert!(picker.items.iter().any(|item| item.path == "../shared/"));
        assert!(!picker.items.iter().any(|item| item.path == "../other/"));
        std::fs::remove_dir_all(root).ok();
    }

    fn unique_test_dir(name: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("mink-tui-{name}-{}-{nanos}", std::process::id()))
    }

    #[test]
    fn input_history_can_walk_multiple_entries_and_restore_draft() {
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let mut state = TuiState::default();
        state.input.history = vec!["first".into(), "second".into()];

        for (key, expected) in [
            (KeyCode::Up, "second"),
            (KeyCode::Up, "first"),
            (KeyCode::Down, "second"),
            (KeyCode::Down, ""),
        ] {
            assert!(!handle_event(
                Event::Key(KeyEvent::new(key, KeyModifiers::NONE)),
                &mut state,
                &tx,
            ));
            assert_eq!(state.input.buf, expected);
            assert_eq!(state.input.cursor, state.input.buf.len());
        }
    }

    #[test]
    fn input_ctrl_d_deletes_next_char_or_exits_when_empty() {
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let mut state = TuiState::default();
        state.input.buf = "a中🙂b".into();
        state.input.cursor = "a".len();

        assert!(!handle_event(
            Event::Key(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL)),
            &mut state,
            &tx,
        ));
        assert_eq!(state.input.buf, "a🙂b");
        assert_eq!(state.input.cursor, "a".len());

        state.input.buf.clear();
        state.input.cursor = 0;
        assert!(handle_event(
            Event::Key(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL)),
            &mut state,
            &tx,
        ));
        assert!(state.quit);
    }

    #[test]
    fn input_clamps_invalid_cursor_before_editing() {
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let mut state = TuiState::default();
        state.input.buf = "a中b".into();
        state.input.cursor = 2;

        assert!(!handle_event(
            Event::Key(KeyEvent::new(KeyCode::Char('X'), KeyModifiers::NONE)),
            &mut state,
            &tx,
        ));
        assert_eq!(state.input.buf, "aX中b");
        assert_eq!(state.input.cursor, "aX".len());

        state.input.cursor = usize::MAX;
        assert!(!handle_event(
            Event::Key(KeyEvent::new(KeyCode::Char('k'), KeyModifiers::CONTROL)),
            &mut state,
            &tx,
        ));
        assert_eq!(state.input.buf, "aX中b");
        assert_eq!(state.input.cursor, state.input.buf.len());
    }

    #[test]
    fn render_clamps_invalid_cursor_and_repairs_missing_cache() {
        let mut state = TuiState::default();
        state.input.buf = "a中b".into();
        state.input.cursor = 2;
        state
            .lines
            .push(MsgLine::new("first".into(), MsgKind::Text));
        state
            .lines
            .push(MsgLine::new("second".into(), MsgKind::Text));
        state.cache.width = 78;
        state.cache.history_lines = Some(vec![Line::from("stale")]);
        state.lines[0].cached_lines = Some(vec![Line::from("first")]);
        state.lines[0].cached_collapsed = state.lines[0].collapsed;

        let backend = TestBackend::new(80, 20);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|f| render(f, &mut state)).unwrap();

        assert_eq!(state.input.cursor, "a".len());
        assert!(state.lines[1].cached_lines.is_some());
        assert!(
            state
                .cache
                .history_lines
                .as_ref()
                .is_some_and(|lines| lines.len() >= 2)
        );
    }

    #[test]
    fn input_alt_backspace_deletes_previous_utf8_word() {
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let mut state = TuiState::default();
        state.input.buf = "hello 世界".into();
        state.input.cursor = state.input.buf.len();

        assert!(!handle_event(
            Event::Key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::ALT)),
            &mut state,
            &tx,
        ));
        assert_eq!(state.input.buf, "hello ");
        assert_eq!(state.input.cursor, "hello ".len());

        assert!(!handle_event(
            Event::Key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::ALT)),
            &mut state,
            &tx,
        ));
        assert_eq!(state.input.buf, "");
        assert_eq!(state.input.cursor, 0);
    }

    fn line_text(line: &Line<'static>) -> String {
        line.spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect::<String>()
    }
}
