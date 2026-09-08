use super::*;
use crate::tui::command::{SlashCommand, parse_slash_command};
use crate::tui::file_picker::{FilePickCandidate, FilePickerPolicy, FilePickerState};
use crate::tui::input::{handle_ctrl_c, handle_event};
use crate::tui::markdown::{
    InlineNode, MdBlock, TableAlign, TableRows, normalize_markdown_input, parse_blocks, push_msg,
    push_msg_with_tool, push_msg_with_width, render_table, strip_ansi, wrap_lines_word,
};
use crate::tui::notify::TaskNotificationKind;
use crate::tui::render::{
    build_status_line, build_status_spans, collapsed_summary, content_viewport_height,
    detail_lines_for_session, detail_viewport_height, split_at_visual_width, visible_lines,
};
use crate::tui::state::{
    ActiveOverlay, CollapsePolicy, TranscriptItem, TranscriptKind, TuiState, View, WorkState,
};
use crate::ui::{Display, ToolResultDisplay, ToolResultKind};
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

#[test]
fn tui_display_detail_uses_full_tool_result_content() {
    let (tx, rx) = std::sync::mpsc::channel();
    let display = TuiDisplay::new(tx);

    display.render_tool_result(&crate::ui::PresentedToolResultDisplay {
        base: ToolResultDisplay {
            tool_name: "Bash",
            content_preview: "short preview\n",
            content: "full output\nwith more detail",
            tool_use_id: Some("toolu_1"),
            exit_code: Some(0),
        },
        status: crate::runtime::ToolStatus::Succeeded,
        result_kind: crate::ui::ToolResultKind::Text,
        presentation: None,
        artifacts: &[],
    });

    match rx.recv().unwrap() {
        TuiSignal::ToolResult {
            tool_name, content, ..
        } => {
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
        serde_json::json!({"type":"tool_call","id":"call-1","name":"Read","input":{"file_path":"src/tui/mod.rs"}}),
        serde_json::json!({
            "type":"tool_result",
            "tool_use_id":"call-1",
            "name":"Read",
            "content":"file contents",
            "success":true,
            "result_kind":"text"
        }),
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
            .any(|line| line.kind == TranscriptKind::StreamThinking)
    );
    assert!(
        lines
            .iter()
            .any(|line| line.kind == TranscriptKind::StreamText)
    );
    let tool = lines
        .iter()
        .find(|line| line.kind == TranscriptKind::Tool)
        .unwrap();
    assert!(tool.sealed);
    assert_eq!(tool.tool_success, Some(true));
    assert!(tool.text.contains("file contents"));
}

#[cfg(feature = "prefab")]
#[test]
fn seeded_prefab_session_events_are_replayable_in_tui() {
    use mink_prefab::{PrefabSeed, PrefabSeedOptions, seed_session};

    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("mink_tui_prefab_{unique}"));
    std::fs::create_dir_all(&dir).unwrap();

    let seed = PrefabSeed::builtin().unwrap();
    let options = PrefabSeedOptions {
        session_id: "tui-prefab".to_string(),
        title: None,
        cwd: dir.clone(),
        agents_md: None,
        skill_result_code: None,
        skill_result_document: None,
        skill_result_list: None,
        system_reminder_agents: None,
        skill_catalog_reminder: None,
        instruction_hint: None,
        full_system_prompt: None,
    };
    seed_session(&dir, &seed.template, &options).unwrap();

    let lines = load_session(&dir.join("events.jsonl"));
    assert!(
        lines
            .iter()
            .any(|line| line.text.contains("Read the workspace-root AGENTS.md"))
    );
    assert!(lines.iter().any(|line| line.text.contains("Ready.")));

    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn apply_switches_stream_kinds_and_collapses_thinking() {
    let mut state = TuiState::default();

    state.apply(&TuiSignal::Thinking("think".into()));
    state.apply(&TuiSignal::Text("answer".into()));

    assert_eq!(state.lines.len(), 1);
    assert_eq!(state.lines[0].kind, TranscriptKind::StreamThinking);
    assert!(state.lines[0].collapsed);
    assert_eq!(state.stream_kind, TranscriptKind::StreamText);
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
    let mut state = TuiState {
        model: "pro".into(),
        ..TuiState::default()
    };
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
fn compact_error_then_stop_is_visible_and_restores_input() {
    let mut state = TuiState {
        work_state: WorkState::Compacting,
        ..TuiState::default()
    };
    state.apply(&TuiSignal::Error(
        "Compact failed: compaction interrupted".into(),
    ));
    state.apply(&TuiSignal::Stop);

    assert_eq!(state.work_state, WorkState::Idle);
    assert!(state.lines.iter().any(|line| {
        line.kind == TranscriptKind::Error && line.text.contains("compaction interrupted")
    }));
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
    assert!(state.lines.iter().any(|line| {
        line.kind == TranscriptKind::Info && line.text.contains("Unknown command")
    }));
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
    assert_eq!(
        parse_slash_command("/model qwen3-coder-plus").unwrap(),
        Some(SlashCommand::Model("qwen3-coder-plus".into()))
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

    push_msg(&mut lines, "# Title\n- item `code`", TranscriptKind::Text);

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
        TranscriptKind::Text,
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
        TranscriptKind::Text,
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
        TranscriptKind::Text,
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
        TranscriptKind::Text,
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
        TranscriptKind::Text,
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
        TranscriptKind::Text,
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
        TranscriptKind::Tool,
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
        TranscriptKind::Text,
    );

    assert_eq!(line_text(&lines[0]), "| A | B |");
    assert_eq!(line_text(&lines[1]), "| nope | --- |");
    assert_eq!(line_text(&lines[2]), "plain");
}

#[test]
fn ctrl_c_interrupts_working_state_then_exits_on_second_press() {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let mut state = TuiState {
        streaming: true,
        work_state: WorkState::StreamingText,
        ..Default::default()
    };

    assert!(!handle_ctrl_c(&mut state, &tx));
    assert!(matches!(
        rx.try_recv(),
        Ok(crate::cli::RuntimeCmd::Interrupt)
    ));
    assert!(!state.quit);

    assert!(handle_ctrl_c(&mut state, &tx));
    assert!(state.quit);
}

#[test]
fn ctrl_shift_c_interrupts_working_state() {
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    let mut state = TuiState {
        work_state: WorkState::WaitingModel,
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
    assert!(!state.quit);
}

#[test]
fn ctrl_c_exits_immediately_when_idle() {
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    let mut state = TuiState::default();

    assert!(handle_ctrl_c(&mut state, &tx));
    assert!(state.quit);
}

#[test]
fn short_tool_result_does_not_default_to_collapsed() {
    let line = TranscriptItem::new("short\noutput".into(), TranscriptKind::Tool);

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
    let line = TranscriptItem::new_tool_result("Bash".into(), text);

    let summary = collapsed_summary(&line, 80);

    assert!(summary.contains("Bash: 25 lines"));
    assert!(summary.contains("bytes"));
    assert!(summary.contains("line 0"));
}

#[test]
fn collapsed_summary_marks_truncated_tool_result() {
    let line = TranscriptItem::new_tool_result(
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
        tool_use_id: None,
        tool_name: "Edit".into(),
        content: "changed file".into(),
        success: true,
        exit_code: None,
        result_kind: crate::ui::ToolResultKind::Edit,
        presentation: None,
        artifacts: Vec::new(),
    });

    assert_eq!(state.lines.len(), 1);
    assert_eq!(state.lines[0].tool_name.as_deref(), Some("Edit"));
    assert_eq!(state.lines[0].kind, TranscriptKind::Tool);
}

#[test]
fn structured_tool_result_updates_the_running_transcript_item() {
    let mut state = TuiState::default();
    state.apply(&TuiSignal::ToolCall {
        tool_use_id: Some("call-1".into()),
        tool_name: "PlanDraft".into(),
        summary: "PlanDraft".into(),
    });
    assert_eq!(state.lines.len(), 1);
    assert!(!state.lines[0].sealed);

    let presentation = crate::ui::ToolPresentation::Plan(crate::ui::PlanDisplay {
        transition: crate::ui::PlanTransitionDisplay::DraftSaved,
        content: Some("1. inspect\n2. implement".into()),
    });
    state.apply(&TuiSignal::ToolResult {
        tool_use_id: Some("call-1".into()),
        tool_name: "PlanDraft".into(),
        content: "Plan draft saved.".into(),
        success: true,
        exit_code: None,
        result_kind: crate::ui::ToolResultKind::Control,
        presentation: Some(presentation),
        artifacts: Vec::new(),
    });

    assert_eq!(state.lines.len(), 1);
    assert!(state.lines[0].sealed);
    assert_eq!(state.lines[0].tool_success, Some(true));
    assert!(state.plan.is_some());
}

#[test]
fn tool_result_with_mismatched_id_does_not_merge_by_tool_name() {
    use crate::ui::{TodoChangeDisplay, TodoCountsDisplay, TodoDisplay, ToolPresentation};

    let mut state = TuiState::default();
    state.apply(&TuiSignal::ToolCall {
        tool_use_id: Some("call-1".into()),
        tool_name: "TodoAdvance".into(),
        summary: "TodoAdvance(2 transitions @r1)".into(),
    });
    let presentation = ToolPresentation::Todo(TodoDisplay {
        revision: 2,
        counts: TodoCountsDisplay {
            pending: 2,
            in_progress: 2,
            completed: 0,
        },
        items: Vec::new(),
        changes: vec![TodoChangeDisplay::Activated { id: "T0001".into() }],
    });
    state.apply(&TuiSignal::ToolResult {
        tool_use_id: Some("provider-call-1".into()),
        tool_name: "TodoAdvance".into(),
        content: "<todo-event>raw protocol</todo-event>".into(),
        success: true,
        exit_code: None,
        result_kind: ToolResultKind::Control,
        presentation: Some(presentation),
        artifacts: Vec::new(),
    });

    assert_eq!(state.lines.len(), 2);
    assert!(!state.lines[0].sealed);
    assert_eq!(state.lines[0].tool_use_id.as_deref(), Some("call-1"));
    assert!(state.lines[1].sealed);
    assert_eq!(
        state.lines[1].tool_use_id.as_deref(),
        Some("provider-call-1")
    );
    let rendered = render::transcript_item_lines(&state.lines[1], 80)
        .iter()
        .map(line_text)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(rendered.contains("Todos r2"));
    assert!(rendered.contains("activated T0001"));
    assert!(!rendered.contains("<todo-event>"));
    assert_eq!(state.todos.as_ref().unwrap().revision, 2);
}

#[test]
fn reused_tool_id_updates_the_latest_unsealed_call() {
    let mut state = TuiState::default();
    state.apply(&TuiSignal::ToolCall {
        tool_use_id: Some("call-1".into()),
        tool_name: "Read".into(),
        summary: "Read(old)".into(),
    });
    state.apply(&TuiSignal::ToolResult {
        tool_use_id: Some("call-1".into()),
        tool_name: "Read".into(),
        content: "old result".into(),
        success: true,
        exit_code: None,
        result_kind: ToolResultKind::FileRead,
        presentation: None,
        artifacts: Vec::new(),
    });
    state.inline.committed = 1;
    state.apply(&TuiSignal::ToolCall {
        tool_use_id: Some("call-1".into()),
        tool_name: "Read".into(),
        summary: "Read(new)".into(),
    });
    state.apply(&TuiSignal::ToolResult {
        tool_use_id: Some("call-1".into()),
        tool_name: "Read".into(),
        content: "new result".into(),
        success: true,
        exit_code: None,
        result_kind: ToolResultKind::FileRead,
        presentation: None,
        artifacts: Vec::new(),
    });

    assert_eq!(state.lines.len(), 2);
    assert_eq!(state.lines[0].text, "old result");
    assert_eq!(state.lines[1].text, "new result");
    assert!(state.lines[1].sealed);
}

#[test]
fn collapsed_artifact_card_keeps_the_artifact_id_visible() {
    let mut item = TranscriptItem::new_tool_result("Bash".into(), "large output".into());
    item.collapsed = true;
    item.artifacts = vec![crate::ui::ArtifactDisplay {
        id: "bash-0001".into(),
        tool: "Bash".into(),
        bytes: 100_000,
        description: "full tool output".into(),
    }];

    let rendered = render::transcript_item_lines(&item, 80)
        .iter()
        .map(line_text)
        .collect::<Vec<_>>()
        .join("\n");

    assert!(rendered.contains("artifact://bash-0001"));
}

#[test]
fn todo_delta_preserves_items_not_mentioned_by_the_update() {
    use crate::ui::{
        TodoChangeDisplay, TodoCountsDisplay, TodoDisplay, TodoItemDisplay, TodoStatusDisplay,
    };

    let mut state = TuiState {
        todos: Some(TodoDisplay {
            revision: 1,
            counts: TodoCountsDisplay {
                pending: 2,
                in_progress: 0,
                completed: 0,
            },
            items: vec![
                TodoItemDisplay {
                    id: "todo-1".into(),
                    content: "first".into(),
                    status: TodoStatusDisplay::Pending,
                },
                TodoItemDisplay {
                    id: "todo-2".into(),
                    content: "second".into(),
                    status: TodoStatusDisplay::Pending,
                },
            ],
            changes: Vec::new(),
        }),
        ..Default::default()
    };

    state.apply_todo_presentation(&TodoDisplay {
        revision: 2,
        counts: TodoCountsDisplay {
            pending: 1,
            in_progress: 1,
            completed: 0,
        },
        items: vec![TodoItemDisplay {
            id: "todo-1".into(),
            content: "first".into(),
            status: TodoStatusDisplay::InProgress,
        }],
        changes: vec![TodoChangeDisplay::Activated {
            id: "todo-1".into(),
        }],
    });

    let todos = state.todos.unwrap();
    assert_eq!(todos.items.len(), 2);
    assert_eq!(todos.items[0].status, TodoStatusDisplay::InProgress);
    assert_eq!(todos.items[1].content, "second");
    assert_eq!(todos.revision, 2);
}

#[test]
fn shared_tool_card_renderer_colors_tools_by_result_kind() {
    let mut item = TranscriptItem::new_tool_result("Edit".into(), "updated file".into());
    item.tool_success = Some(true);
    item.tool_result_kind = Some(ToolResultKind::Edit);

    let lines = render::transcript_item_lines(&item, 80);

    assert_eq!(lines.len(), 2);
    assert_eq!(lines[0].spans[0].style.fg, Some(Color::Green));
    assert_eq!(lines[0].spans[1].content.as_ref(), "Edit");
    assert_eq!(lines[0].spans[1].style.fg, Some(Color::Yellow));
}

#[test]
fn inline_tool_projection_auto_collapses_without_expand_marker() {
    let mut item = TranscriptItem::new_tool_result(
        "Bash".into(),
        (0..30)
            .map(|line| format!("output {line}"))
            .collect::<Vec<_>>()
            .join("\n"),
    );
    item.tool_success = Some(true);
    item.tool_result_kind = Some(ToolResultKind::Command);

    let lines = render::transcript_item_lines(&item, 80);

    assert_eq!(lines.len(), 1);
    assert!(!line_text(&lines[0]).starts_with('▶'));
    assert!(line_text(&lines[0]).contains("Bash"));
}

#[test]
fn full_tui_restores_mouse_toggle_for_collapsible_cards() {
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    let mut item = TranscriptItem::new_tool_result(
        "Bash".into(),
        (0..30)
            .map(|line| format!("output {line}"))
            .collect::<Vec<_>>()
            .join("\n"),
    );
    item.tool_success = Some(true);
    item.tool_result_kind = Some(ToolResultKind::Command);
    let mut state = TuiState::default();
    state.push_line(item);
    let backend = TestBackend::new(80, 20);
    let mut terminal = Terminal::new(backend).unwrap();

    terminal
        .draw(|frame| render(frame, &mut state, TuiMode::Full))
        .unwrap();
    assert!(state.lines[0].collapsed);
    assert!(!state.viewport.click_map.is_empty());
    let row = state.viewport.content_y + state.viewport.click_map[0].start_row as u16;

    handle_event(
        Event::Mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 1,
            row,
            modifiers: KeyModifiers::NONE,
        }),
        &mut state,
        &tx,
    );

    assert!(!state.lines[0].collapsed);
    assert!(state.lines[0].collapse_overridden);
}

#[test]
fn inline_stream_promotes_complete_markdown_blocks_before_stop() {
    let mut state = TuiState::default();
    state.apply(&TuiSignal::Text(
        "first paragraph\n\nsecond paragraph".into(),
    ));

    state.promote_stable_stream_prefix();

    assert_eq!(state.lines.len(), 1);
    assert_eq!(state.lines[0].text, "first paragraph\n\n");
    assert_eq!(state.stream_line, "second paragraph");
    assert!(state.lines[0].sealed);
}

#[test]
fn info_heartbeat_during_streaming_does_not_split_markdown_fence() {
    // 心跳在未闭合的代码围栏中间到达：文本不得被切段，否则下半段丢失围栏
    // 上下文，围栏内的注释行会被当成 markdown 重新解析。
    let text = r#"**核心思路**：示例说明。

```
data/demo/material/
├── _library/                          # 类型库（不参与查找）
│   ├── types.json                     # 入口键全集：key → type
│   │                                  #    {"a":"A",
│   │                                  #     "b":"B", …}
│   ├── sources/示例素材-0000/          # 原始原件归档（只存一份）
│   └── 校验报告-<日期>.md               # 每类型校验
│
├── <entry>_<branch>/                   # 每个业务入口（自包含）
│   ├── type.json                      # {"type":"A","type_version":"2025-01-01"}
│   ├── framework.json                 # 章节框架 → 结构化章节树
│   ├── shared.md                      # 跨源固定模板块
│   ├── sources/
│   │   ├── <分支ID>/                    # 分支 = 素材版本（01.01/02.02、A/B…）
│   │   │   ├── index.md               # 来源 / 适用说明
│   │   │   ├── 工艺工况.md             # 内容提炼（匹配判断用）
│   │   │   └── 原文.md                 # 抽取正文
│   │   └── …
│   └── output/                        # 占位（可无）
│
└── （不再有旧式手摆目录）
```

## 5. 后续

继续讨论。
"#;
    let split = text.find("01.01/02.02").unwrap();
    let (head, tail) = text.split_at(split);

    let mut state = TuiState::default();
    state.apply(&TuiSignal::Text(head.into()));
    state.apply(&TuiSignal::Info(mink::runtime::llm_wait_heartbeat_message(
        30, 0,
    )));
    // 心跳不打断流：尚未落入 transcript，状态栏标签就位。
    assert!(state.streaming);
    assert_eq!(state.lines.len(), 0);
    assert_eq!(state.stream_status.as_deref(), Some("·30s"));
    state.apply(&TuiSignal::Text(tail.into()));
    // 新内容到达后等待标签清除。
    assert_eq!(state.stream_status, None);
    state.apply(&TuiSignal::Stop);

    // 最终 transcript：完整文本一条，心跳不落盘（瞬态状态）。
    assert_eq!(state.lines.len(), 1);
    assert_eq!(state.lines[0].text, text);
    assert!(!state.streaming);

    // Render the final transcript the same way the TUI does.
    let mut rendered = String::new();
    for item in &state.lines {
        for line in crate::tui::render::transcript_item_lines(item, 104) {
            rendered.push_str(&line_text(&line));
            rendered.push('\n');
        }
    }

    // 跨心跳边界的那一行必须完整：被切段时两半会渲染成不同行。
    assert!(
        rendered.contains("# 分支 = 素材版本（01.01/02.02、A/B…）"),
        "fence line split at heartbeat; rendered:\n{rendered}"
    );
    assert!(
        rendered.contains("├── _library/                          # 类型库（不参与查找）"),
        "tree line before heartbeat lost; rendered:\n{rendered}"
    );
    assert!(
        rendered.contains("└── 原文.md                 # 抽取正文"),
        "tree line after heartbeat lost; rendered:\n{rendered}"
    );
    // 围栏闭合后的内容按 markdown 渲染（标题不再被代码块吞掉）。
    assert!(
        rendered.contains("5. 后续"),
        "content after fence missing; rendered:\n{rendered}"
    );
    assert!(
        !rendered.contains("Waiting for model response"),
        "heartbeat should not be persisted into transcript; rendered:\n{rendered}"
    );
}

#[test]
fn heartbeat_status_label_extracts_elapsed_seconds() {
    assert_eq!(
        crate::tui::state::heartbeat_status_label(&mink::runtime::llm_wait_heartbeat_message(
            30, 0
        ))
        .as_deref(),
        Some("·30s")
    );
    assert_eq!(
        crate::tui::state::heartbeat_status_label(&mink::runtime::llm_wait_heartbeat_message(
            60, 45
        ))
        .as_deref(),
        Some("·60s")
    );
    assert_eq!(
        crate::tui::state::heartbeat_status_label("Retrying (1/3)..."),
        None
    );
    assert_eq!(crate::tui::state::heartbeat_status_label("其他信息"), None);
}

#[test]
fn info_during_streaming_non_heartbeat_is_deferred_until_stream_ends() {
    let mut state = TuiState::default();
    state.apply(&TuiSignal::Text("first".into()));
    state.apply(&TuiSignal::Info("Retrying (1/3)...".into()));
    assert!(state.streaming);
    assert_eq!(state.lines.len(), 0);
    assert_eq!(
        state.pending_infos.as_slice(),
        &["Retrying (1/3)...".to_string()]
    );
    state.apply(&TuiSignal::Text("second".into()));
    state.apply(&TuiSignal::Stop);
    assert_eq!(state.lines.len(), 2);
    assert_eq!(state.lines[0].kind, TranscriptKind::StreamText);
    assert_eq!(state.lines[1].kind, TranscriptKind::Info);
    assert_eq!(state.lines[1].text, "Retrying (1/3)...");
}

#[test]
fn status_bar_shows_transient_wait_label() {
    let mut state = TuiState {
        cwd_label: String::new(),
        ..Default::default()
    };
    let line = build_status_line(&state, 80);
    assert!(!line.contains("·"));
    // 首事件等待阶段（未流式）的心跳同样只进状态栏，不进 transcript。
    state.apply(&TuiSignal::Info(mink::runtime::llm_wait_heartbeat_message(
        30, 0,
    )));
    assert_eq!(state.lines.len(), 0);
    assert_eq!(state.stream_status.as_deref(), Some("·30s"));
    // 流式阶段心跳替换更新标签。
    state.apply(&TuiSignal::Text("hello".into()));
    assert_eq!(state.stream_status, None);
    state.apply(&TuiSignal::Info(mink::runtime::llm_wait_heartbeat_message(
        60, 45,
    )));
    assert_eq!(state.stream_status.as_deref(), Some("·60s"));
    let line = build_status_line(&state, 80);
    assert!(line.contains("·60s"));
    assert!(line.contains("[generating]"));
    state.apply(&TuiSignal::Stop);
    assert_eq!(state.stream_status, None);
}

#[test]
fn markdown_trailing_newline_does_not_create_an_extra_blank_row() {
    let mut lines = Vec::new();

    push_msg(&mut lines, "hello\n", TranscriptKind::Text);

    assert_eq!(lines.len(), 1);
    assert_eq!(line_text(&lines[0]), "hello");
}

#[test]
fn committed_boundary_stops_at_the_first_live_item() {
    let mut state = TuiState::default();
    state.push_line(TranscriptItem::new("sealed".into(), TranscriptKind::Text));
    state.push_line(TranscriptItem::new_tool_call(
        Some("call-1".into()),
        "Bash".into(),
        "Bash(test)".into(),
    ));
    state.push_line(TranscriptItem::new("later".into(), TranscriptKind::Text));

    assert_eq!(sealed_prefix_end(&state), 1);
    state.lines[1].sealed = true;
    assert_eq!(sealed_prefix_end(&state), 3);
    state.inline.committed = 2;
    assert_eq!(sealed_prefix_end(&state), 3);
}

#[test]
fn terminal_signal_seals_orphan_tool_calls() {
    let mut state = TuiState::default();
    state.apply(&TuiSignal::ToolCall {
        tool_use_id: Some("call-1".into()),
        tool_name: "Bash".into(),
        summary: "Bash(test)".into(),
    });

    state.apply(&TuiSignal::Error("connection failed".into()));

    assert!(state.lines[0].sealed);
    assert_eq!(state.lines[0].tool_success, Some(false));
    assert!(state.lines[0].text.contains("turn failed"));
    assert_eq!(sealed_prefix_end(&state), state.lines.len());
}

#[test]
fn commit_ready_moves_only_the_sealed_prefix_into_native_history() {
    let backend = TestBackend::new(60, 12);
    let mut terminal = Terminal::with_options(
        backend,
        ratatui::TerminalOptions {
            viewport: ratatui::Viewport::Inline(4),
        },
    )
    .unwrap();
    let mut state = TuiState::default();
    state.push_line(TranscriptItem::new(
        "completed answer".into(),
        TranscriptKind::Text,
    ));
    state.push_line(TranscriptItem::new_tool_call(
        Some("call-1".into()),
        "Bash".into(),
        "Bash(test)".into(),
    ));

    assert!(commit_ready(&mut terminal, &mut state).unwrap());
    assert_eq!(state.inline.committed, 1);
    assert!(!commit_ready(&mut terminal, &mut state).unwrap());

    state.lines[1].sealed = true;
    state.work_state = WorkState::WaitingModel;
    assert!(commit_ready(&mut terminal, &mut state).unwrap());
    assert_eq!(state.inline.committed, 2);
}

#[test]
fn inline_keeps_the_final_item_in_the_viewport_until_new_work_starts() {
    let mut state = TuiState::default();
    state.push_line(TranscriptItem::new(
        "final answer".into(),
        TranscriptKind::StreamText,
    ));

    assert_eq!(committable_prefix_end(&state), 0);

    state.work_state = WorkState::WaitingModel;
    assert_eq!(committable_prefix_end(&state), 1);
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
fn file_picker_enter_accepts_directory_and_closes_overlay() {
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    let mut state = TuiState::default();
    state.input.buf = "Read do".into();
    state.input.cursor = state.input.buf.len();
    state.overlay = Some(ActiveOverlay::FilePicker(
        FilePickerState::open_with_candidates(
            &state.input.buf,
            state.input.cursor,
            vec![FilePickCandidate {
                path: "docs".into(),
                is_dir: true,
            }],
        ),
    ));

    assert!(!handle_event(
        Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
        &mut state,
        &tx,
    ));

    assert_eq!(state.input.buf, "Read docs/");
    assert_eq!(state.input.cursor, state.input.buf.len());
    assert!(state.overlay.is_none());
}

#[test]
fn file_picker_tab_enters_directory_and_keeps_overlay_open() {
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    let mut state = TuiState::default();
    state.input.buf = "Read do".into();
    state.input.cursor = state.input.buf.len();
    state.overlay = Some(ActiveOverlay::FilePicker(
        FilePickerState::open_with_candidates(
            &state.input.buf,
            state.input.cursor,
            vec![FilePickCandidate {
                path: "docs".into(),
                is_dir: true,
            }],
        ),
    ));

    assert!(!handle_event(
        Event::Key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE)),
        &mut state,
        &tx,
    ));

    assert_eq!(state.input.buf, "Read docs/");
    assert_eq!(state.input.cursor, state.input.buf.len());
    assert!(matches!(state.overlay, Some(ActiveOverlay::FilePicker(_))));
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
        .push(TranscriptItem::new("first".into(), TranscriptKind::Text));
    state
        .lines
        .push(TranscriptItem::new("second".into(), TranscriptKind::Text));
    state.cache.width = 78;
    state.cache.history_lines = Some(vec![Line::from("stale")]);
    state.lines[0].cached_lines = Some(vec![Line::from("first")]);
    state.lines[0].cached_collapsed = state.lines[0].collapsed;

    let backend = TestBackend::new(80, 20);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal
        .draw(|f| render(f, &mut state, TuiMode::Full))
        .unwrap();

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

#[test]
fn small_viewport_input_box_keeps_cursor_inside_and_border_intact() {
    // Regression: on an 8-row viewport (inline minimum) a 5-line input
    // must not squeeze the input block — the last line was clipped and
    // the cursor landed on the bottom border.
    let mut state = TuiState::default();
    state.input.buf = "line1\nline2\nline3\nline4\nline5".into();
    state.input.cursor = state.input.buf.len();

    use ratatui::backend::Backend;
    let backend = TestBackend::new(80, 8);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal
        .draw(|f| render(f, &mut state, TuiMode::Full))
        .unwrap();

    let buf = terminal.backend().buffer();
    // Layout for 8 rows: content row0, input block rows1..6, status row7.
    assert_eq!(buf.cell((0, 1)).unwrap().symbol(), "┌");
    assert_eq!(buf.cell((0, 6)).unwrap().symbol(), "└");
    // The last input line must be visible inside the box (row5), not
    // clipped or overwriting the bottom border (row6).
    let last_row: String = (1..6)
        .map(|x| buf.cell((x, 5)).unwrap().symbol().to_string())
        .collect();
    assert_eq!(last_row, "line5");
    assert!(matches!(buf.cell((1, 6)).unwrap().symbol(), "─" | "━"));
    let cur = terminal.backend_mut().get_cursor_position().unwrap();
    // Cursor must sit on the last inner row (5), never on the border (6).
    assert_eq!(cur.y, 5);
}

#[test]
fn new_user_input_is_appended_after_the_open_stream() {
    // Regression: submitting a new message while the previous turn's
    // stream is still open must not let the late finalize (Stop) insert
    // the old answer after the new user input.
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    let mut state = TuiState::default();

    state.apply(&TuiSignal::Text("previous answer".into()));
    assert!(state.streaming);

    state.input.buf = "next question".into();
    state.input.cursor = state.input.buf.len();
    assert!(!handle_event(
        Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
        &mut state,
        &tx,
    ));

    assert!(!state.streaming);
    let texts: Vec<&str> = state.lines.iter().map(|item| item.text.as_str()).collect();
    assert_eq!(
        texts,
        vec!["previous answer", "> next question"],
        "the user input echo must follow the already displayed stream content"
    );

    // A late Stop must not move the previous answer behind the new input.
    state.apply(&TuiSignal::Stop);
    let texts: Vec<&str> = state.lines.iter().map(|item| item.text.as_str()).collect();
    assert_eq!(texts, vec!["previous answer", "> next question"]);
    assert_eq!(state.work_state, WorkState::Idle);
}

fn pending_image(name: &str) -> crate::tui::state::PendingImage {
    crate::tui::state::PendingImage {
        path: PathBuf::from(format!("/tmp/{name}.png")),
        width: 12,
        height: 8,
        bytes: 3456,
    }
}

fn fake_clipboard_reader() -> crate::tui::state::ClipboardReader {
    std::sync::Arc::new(
        |_: &std::path::Path,
         _: &mink::runtime::OpenAiChatImageUrlLimits|
         -> anyhow::Result<crate::tui::clipboard::ClipboardPng> {
            Ok(crate::tui::clipboard::ClipboardPng {
                bytes: b"fake-png".to_vec(),
                width: 12,
                height: 8,
            })
        },
    )
}

fn session_info(dir: &std::path::Path) -> mink::runtime::SessionInfo {
    mink::runtime::SessionInfo {
        session_id: "s".into(),
        session_ref: "s".into(),
        is_new: true,
        home: dir.to_path_buf(),
        cwd: dir.to_path_buf(),
        events_path: dir.join("events.jsonl"),
        conversation_path: dir.join("conversation.jsonl"),
        artifacts_dir: dir.join("artifacts"),
        summary_path: dir.join("summary.md"),
        usage_path: dir.join("usage.jsonl"),
        plan_path: dir.join("plan.md"),
        plan_draft_path: dir.join("plan.draft"),
        todos_path: dir.join("todos.json"),
    }
}

#[test]
fn ctrl_v_stages_clipboard_image_and_queues_it() {
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    let (ui_tx, ui_rx) = std::sync::mpsc::channel();
    let dir = unique_test_dir("clipboard-stage");
    let mut state = TuiState {
        image_input: Some(mink::runtime::OpenAiChatImageUrlLimits::default()),
        attachments_dir: dir.clone(),
        ui_tx: Some(ui_tx),
        clipboard_reader: Some(fake_clipboard_reader()),
        ..Default::default()
    };

    assert!(!handle_event(
        Event::Key(KeyEvent::new(KeyCode::Char('v'), KeyModifiers::CONTROL)),
        &mut state,
        &tx,
    ));
    assert!(state.clipboard_started.is_some());

    let event = ui_rx
        .recv_timeout(std::time::Duration::from_secs(10))
        .expect("clipboard worker must report back");
    state.apply_ui_event(event);

    assert!(state.clipboard_started.is_none());
    assert_eq!(state.input.pending_images.len(), 1);
    let image = &state.input.pending_images[0];
    assert!(image.path.starts_with(&dir), "{:?}", image.path);
    assert_eq!((image.width, image.height, image.bytes), (12, 8, 8));
    assert_eq!(std::fs::read(&image.path).unwrap(), b"fake-png");
    std::fs::remove_dir_all(dir).ok();
}

#[test]
fn super_v_also_requests_clipboard_image() {
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    let (ui_tx, ui_rx) = std::sync::mpsc::channel();
    let dir = unique_test_dir("clipboard-super");
    let mut state = TuiState {
        image_input: Some(mink::runtime::OpenAiChatImageUrlLimits::default()),
        attachments_dir: dir.clone(),
        ui_tx: Some(ui_tx),
        clipboard_reader: Some(fake_clipboard_reader()),
        ..Default::default()
    };

    assert!(!handle_event(
        Event::Key(KeyEvent::new(KeyCode::Char('v'), KeyModifiers::SUPER)),
        &mut state,
        &tx,
    ));

    let event = ui_rx
        .recv_timeout(std::time::Duration::from_secs(10))
        .expect("Super+V must trigger the clipboard worker");
    state.apply_ui_event(event);

    assert_eq!(state.input.pending_images.len(), 1);
    std::fs::remove_dir_all(dir).ok();
}

#[test]
fn stale_clipboard_read_allows_a_retry() {
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    let (ui_tx, ui_rx) = std::sync::mpsc::channel();
    let dir = unique_test_dir("clipboard-stale");
    let mut state = TuiState {
        image_input: Some(mink::runtime::OpenAiChatImageUrlLimits::default()),
        attachments_dir: dir.clone(),
        ui_tx: Some(ui_tx),
        clipboard_reader: Some(fake_clipboard_reader()),
        // A read that never reported back (hung osascript) must not disable
        // paste for the rest of the session.
        clipboard_started: Some(std::time::Instant::now() - std::time::Duration::from_secs(60)),
        ..Default::default()
    };

    assert!(!handle_event(
        Event::Key(KeyEvent::new(KeyCode::Char('v'), KeyModifiers::CONTROL)),
        &mut state,
        &tx,
    ));

    let event = ui_rx
        .recv_timeout(std::time::Duration::from_secs(10))
        .expect("a stale in-flight read must not block a retry");
    state.apply_ui_event(event);
    assert_eq!(state.input.pending_images.len(), 1);
    std::fs::remove_dir_all(dir).ok();
}

#[test]
fn ctrl_v_without_image_capability_reports_and_queues_nothing() {
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    let (ui_tx, _ui_rx) = std::sync::mpsc::channel();
    let mut state = TuiState {
        image_input: None,
        attachments_dir: unique_test_dir("clipboard-disabled"),
        ui_tx: Some(ui_tx),
        clipboard_reader: Some(fake_clipboard_reader()),
        ..Default::default()
    };

    assert!(!handle_event(
        Event::Key(KeyEvent::new(KeyCode::Char('v'), KeyModifiers::CONTROL)),
        &mut state,
        &tx,
    ));

    assert!(state.clipboard_started.is_none());
    assert!(state.input.pending_images.is_empty());
    let last = state.lines.last().unwrap();
    assert_eq!(last.kind, TranscriptKind::Info);
    assert!(
        last.text.contains("no image input capability"),
        "{}",
        last.text
    );
}

#[test]
fn backspace_on_empty_input_drops_last_queued_image() {
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    let mut state = TuiState::default();
    state.input.pending_images = vec![pending_image("a"), pending_image("b")];

    assert!(!handle_event(
        Event::Key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE)),
        &mut state,
        &tx,
    ));
    assert_eq!(state.input.pending_images.len(), 1);
    assert_eq!(
        state.input.pending_images[0].path,
        PathBuf::from("/tmp/a.png")
    );

    assert!(!handle_event(
        Event::Key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE)),
        &mut state,
        &tx,
    ));
    assert!(state.input.pending_images.is_empty());
}

#[test]
fn enter_expands_image_markers_and_keeps_history_clean() {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let mut state = TuiState::default();
    state.input.buf = "look at this".into();
    state.input.cursor = state.input.buf.len();
    state.input.pending_images = vec![pending_image("a")];

    assert!(!handle_event(
        Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
        &mut state,
        &tx,
    ));

    let command = rx.try_recv().expect("run command");
    let crate::cli::RuntimeCmd::Run { input, .. } = command else {
        panic!("expected a Run command");
    };
    assert_eq!(
        input,
        "look at this\n\n[Attached image: \"/tmp/a.png\" - Read it to view.]"
    );
    assert_eq!(state.input.history, vec!["look at this".to_string()]);
    assert!(state.input.pending_images.is_empty());
    assert_eq!(
        state.lines.last().unwrap().text,
        "> [image #1] look at this"
    );
}

#[test]
fn enter_with_only_images_submits_marker_text() {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let mut state = TuiState::default();
    state.input.pending_images = vec![pending_image("a")];

    assert!(!handle_event(
        Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
        &mut state,
        &tx,
    ));

    let command = rx.try_recv().expect("run command");
    let crate::cli::RuntimeCmd::Run { input, .. } = command else {
        panic!("expected a Run command");
    };
    assert_eq!(input, "[Attached image: \"/tmp/a.png\" - Read it to view.]");
    assert!(state.input.history.is_empty());
    assert_eq!(state.lines.last().unwrap().text, "> [image #1]");
}

#[test]
fn slash_command_keeps_pending_images_queued() {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let mut state = TuiState::default();
    state.input.buf = "/plan".into();
    state.input.cursor = state.input.buf.len();
    state.input.pending_images = vec![pending_image("a")];

    assert!(!handle_event(
        Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
        &mut state,
        &tx,
    ));

    assert!(
        rx.try_recv().is_err(),
        "slash commands never reach the runtime"
    );
    assert!(matches!(state.view, View::Plan { .. }));
    assert_eq!(state.input.pending_images.len(), 1);
    assert!(
        state
            .lines
            .iter()
            .any(|line| line.text.contains("stay queued")),
        "queue notice missing"
    );
}

#[test]
fn failed_send_keeps_text_and_queued_images() {
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<crate::cli::RuntimeCmd>();
    drop(rx);
    let mut state = TuiState::default();
    state.input.buf = "look at this".into();
    state.input.cursor = state.input.buf.len();
    state.input.pending_images = vec![pending_image("a")];

    assert!(!handle_event(
        Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
        &mut state,
        &tx,
    ));

    assert_eq!(state.input.buf, "look at this");
    assert_eq!(state.input.cursor, "look at this".len());
    assert_eq!(state.input.pending_images.len(), 1);
    assert!(
        state
            .lines
            .iter()
            .any(|line| line.kind == TranscriptKind::Error
                && line.text.contains("Failed to send user input")),
        "a failed send must be reported"
    );
}

#[test]
fn unrepresentable_attachment_path_fails_closed() {
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    let (ui_tx, ui_rx) = std::sync::mpsc::channel();
    let root = unique_test_dir("clipboard-quote");
    let mut state = TuiState {
        image_input: Some(mink::runtime::OpenAiChatImageUrlLimits::default()),
        attachments_dir: root.join("bad\"dir"),
        ui_tx: Some(ui_tx),
        clipboard_reader: Some(fake_clipboard_reader()),
        ..Default::default()
    };

    assert!(!handle_event(
        Event::Key(KeyEvent::new(KeyCode::Char('v'), KeyModifiers::CONTROL)),
        &mut state,
        &tx,
    ));
    let event = ui_rx
        .recv_timeout(std::time::Duration::from_secs(10))
        .expect("clipboard worker must report back");
    state.apply_ui_event(event);

    assert!(state.input.pending_images.is_empty());
    assert!(
        state
            .lines
            .iter()
            .any(|line| line.kind == TranscriptKind::Error
                && line.text.contains("cannot be represented")),
        "unrepresentable paths must fail closed"
    );
    std::fs::remove_dir_all(root).ok();
}

#[test]
fn chip_lines_truncate_to_requested_rows() {
    let images = vec![pending_image("a"), pending_image("b"), pending_image("c")];

    let lines = render::chip_lines(&images, 12, 2);

    assert_eq!(lines.len(), 2);
    assert!(lines[0].contains("[image #1"), "{}", lines[0]);
    assert!(lines[1].ends_with('…'), "{}", lines[1]);
    assert!(
        lines
            .iter()
            .all(|line| unicode_width::UnicodeWidthStr::width(line.as_str()) <= 12),
        "chips must stay within the requested width: {lines:?}"
    );
    assert!(render::chip_lines(&images, 80, 0).is_empty());
    assert!(render::chip_lines(&[], 80, 2).is_empty());
}

#[test]
fn load_image_limits_reads_frozen_snapshot() {
    let dir = unique_test_dir("capabilities");
    std::fs::create_dir_all(&dir).unwrap();
    let info = session_info(&dir);

    assert!(
        load_image_limits(&info).is_none(),
        "missing snapshot fails closed"
    );

    let supported = mink::runtime::ImageInputCapability::OpenAiChatImageUrl(Default::default());
    std::fs::write(
        dir.join("model-capabilities.json"),
        serde_json::json!({"image_input": supported}).to_string(),
    )
    .unwrap();
    assert!(load_image_limits(&info).is_some());

    let unsupported = mink::runtime::ImageInputCapability::Unsupported;
    std::fs::write(
        dir.join("model-capabilities.json"),
        serde_json::json!({"image_input": unsupported}).to_string(),
    )
    .unwrap();
    assert!(
        load_image_limits(&info).is_none(),
        "text-only snapshot fails closed"
    );

    std::fs::remove_dir_all(dir).ok();
}

#[test]
fn render_draws_queued_image_chips_above_the_input() {
    let mut state = TuiState::default();
    state
        .lines
        .push(TranscriptItem::new("hello".into(), TranscriptKind::Text));
    state.input.pending_images = vec![pending_image("a")];
    let backend = TestBackend::new(80, 20);
    let mut terminal = Terminal::new(backend).unwrap();

    terminal
        .draw(|f| render(f, &mut state, TuiMode::Full))
        .unwrap();

    let buf = terminal.backend().buffer();
    let row_text = |y: u16| -> String {
        (0..80)
            .map(|x| buf.cell((x, y)).unwrap().symbol().to_string())
            .collect()
    };
    let chip_row = (0..20)
        .find(|y| row_text(*y).contains("[image #1 12x8 3.4KB]"))
        .expect("chip row must be rendered");
    assert!(
        row_text(chip_row + 1).starts_with('┌'),
        "input box must stay directly below the chips"
    );
}

#[test]
fn format_bytes_keeps_one_decimal_only_when_needed() {
    let format = crate::tui::state::format_bytes;

    assert_eq!(format(512), "512B");
    assert_eq!(format(1024), "1KB");
    assert_eq!(format(1536), "1.5KB");
    assert_eq!(format(220 * 1024), "220KB");
    assert_eq!(format(1024 * 1024), "1MB");
    assert_eq!(format(1024 * 1024 * 3 / 2), "1.5MB");
}

#[test]
fn duplicate_paste_is_not_queued_twice() {
    let mut state = TuiState::default();

    state.apply_ui_event(crate::tui::state::TuiUiEvent::ImageCaptured(pending_image(
        "a",
    )));
    state.apply_ui_event(crate::tui::state::TuiUiEvent::ImageCaptured(pending_image(
        "a",
    )));

    assert_eq!(state.input.pending_images.len(), 1);
    let notice = state
        .lines
        .iter()
        .find(|line| line.text.contains("already queued"))
        .expect("duplicate paste must be reported");
    // The notice must not echo the absolute attachment path.
    assert!(!notice.text.contains("/tmp/"), "{}", notice.text);
}

#[test]
fn ctrl_v_and_newline_keys_are_ignored_while_the_file_picker_is_open() {
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    let (ui_tx, ui_rx) = std::sync::mpsc::channel();
    let dir = unique_test_dir("clipboard-overlay");
    let mut state = TuiState {
        image_input: Some(mink::runtime::OpenAiChatImageUrlLimits::default()),
        attachments_dir: dir.clone(),
        ui_tx: Some(ui_tx),
        clipboard_reader: Some(fake_clipboard_reader()),
        overlay: Some(ActiveOverlay::FilePicker(
            FilePickerState::open_with_candidates("", 0, Vec::new()),
        )),
        ..Default::default()
    };

    for (code, modifiers) in [
        (KeyCode::Char('v'), KeyModifiers::CONTROL),
        (KeyCode::Enter, KeyModifiers::SHIFT),
        (KeyCode::Enter, KeyModifiers::ALT),
        (KeyCode::Char('j'), KeyModifiers::CONTROL),
    ] {
        assert!(!handle_event(
            Event::Key(KeyEvent::new(code, modifiers)),
            &mut state,
            &tx,
        ));
    }

    assert!(
        state.clipboard_started.is_none(),
        "Ctrl+V must not start a clipboard read behind the overlay"
    );
    assert!(ui_rx.try_recv().is_err());
    assert!(state.input.buf.is_empty(), "newline keys must be inert");
    std::fs::remove_dir_all(dir).ok();
}

#[test]
fn chips_are_hidden_while_the_file_picker_overlay_is_open() {
    let mut state = TuiState::default();
    state.input.pending_images = vec![pending_image("a")];
    state.overlay = Some(ActiveOverlay::FilePicker(
        FilePickerState::open_with_candidates("", 0, Vec::new()),
    ));
    let backend = TestBackend::new(100, 40);
    let mut terminal = Terminal::new(backend).unwrap();

    terminal
        .draw(|f| render(f, &mut state, TuiMode::Full))
        .unwrap();

    let buf = terminal.backend().buffer();
    let rows: Vec<String> = (0..40)
        .map(|y| {
            (0..100)
                .map(|x| buf.cell((x, y)).unwrap().symbol().to_string())
                .collect()
        })
        .collect();
    assert!(
        rows.iter().all(|row| !row.contains("[image #1")),
        "chips must not be drawn under the picker overlay: {rows:?}"
    );
    assert!(
        rows.iter().any(|row| row.contains("files:")),
        "the picker must still be rendered"
    );
}

#[test]
fn compact_user_input_replaces_only_marker_lines() {
    use crate::tui::state::compact_user_input_for_display as compact;

    assert_eq!(
        compact("[Attached image: \"/tmp/a.png\" - Read it to view.]"),
        "[image]"
    );
    assert_eq!(
        compact("hello\n[Attached image: \"/tmp/a.png\" - Read it to view.]"),
        "hello\n[image]"
    );
    assert_eq!(compact("plain text"), "plain text");
}

#[test]
fn replay_compacts_paste_markers_in_user_input() {
    let path = std::env::temp_dir().join(format!(
        "mink_tui_paste_replay_{}_{}.jsonl",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let events = [
        serde_json::json!({
            "type": "user_input",
            "content": "[Attached image: \"/tmp/a.png\" - Read it to view.]"
        }),
        serde_json::json!({"type": "text", "content": "answer"}),
    ];
    let data = events
        .iter()
        .map(serde_json::Value::to_string)
        .collect::<Vec<_>>()
        .join("\n");
    std::fs::write(&path, data).unwrap();

    let lines = load_session(&path);
    let _ = std::fs::remove_file(path);

    assert!(
        lines.iter().any(|line| line.text == "> [image]"),
        "replayed paste markers must not echo absolute paths: {:?}",
        lines
            .iter()
            .map(|line| line.text.as_str())
            .collect::<Vec<_>>()
    );
}

#[test]
fn shift_alt_enter_and_ctrl_j_insert_newlines() {
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    let mut state = TuiState::default();
    state.input.buf = "ab".into();
    state.input.cursor = 1;

    for (code, modifiers) in [
        (KeyCode::Enter, KeyModifiers::SHIFT),
        (KeyCode::Enter, KeyModifiers::ALT),
        (KeyCode::Char('j'), KeyModifiers::CONTROL),
    ] {
        assert!(!handle_event(
            Event::Key(KeyEvent::new(code, modifiers)),
            &mut state,
            &tx,
        ));
    }

    assert_eq!(state.input.buf, "a\n\n\nb");
    assert_eq!(state.input.cursor, "a\n\n\n".len());
}

#[test]
fn keyboard_enhancement_gate_respects_env_and_term() {
    assert!(keyboard_enhancement_allowed(None, Some("xterm-256color")));
    assert!(keyboard_enhancement_allowed(
        Some("on"),
        Some("xterm-256color")
    ));
    assert!(!keyboard_enhancement_allowed(
        Some("off"),
        Some("xterm-256color")
    ));
    assert!(!keyboard_enhancement_allowed(Some(" 0 "), None));
    assert!(!keyboard_enhancement_allowed(Some("false"), None));
    assert!(!keyboard_enhancement_allowed(Some("no"), None));
    // Case-insensitive, like every other MINK_* parser.
    assert!(!keyboard_enhancement_allowed(Some("OFF"), None));
    assert!(!keyboard_enhancement_allowed(Some(" False "), None));
    assert!(!keyboard_enhancement_allowed(Some("No"), None));
    assert!(!keyboard_enhancement_allowed(None, Some("dumb")));
    assert!(!keyboard_enhancement_allowed(None, Some("")));
    // TERM unset is not a reason to skip the probe: the probe itself decides.
    assert!(keyboard_enhancement_allowed(None, None));
}

#[test]
fn disable_keyboard_enhancement_pops_at_most_once() {
    KEYBOARD_ENHANCED.store(false, Ordering::SeqCst);
    // Never pushed: must not emit a stray pop sequence.
    disable_keyboard_enhancement();
    assert!(!KEYBOARD_ENHANCED.load(Ordering::SeqCst));

    KEYBOARD_ENHANCED.store(true, Ordering::SeqCst);
    disable_keyboard_enhancement();
    assert!(!KEYBOARD_ENHANCED.load(Ordering::SeqCst));
    // Idempotent: the restore guard and the panic hook may both call it.
    disable_keyboard_enhancement();
    assert!(!KEYBOARD_ENHANCED.load(Ordering::SeqCst));
}

fn line_text(line: &Line<'static>) -> String {
    line.spans
        .iter()
        .map(|span| span.content.as_ref())
        .collect::<String>()
}

#[cfg(feature = "prefab")]
#[tokio::test]
async fn resumed_prefab_session_is_visible_in_tui() {
    use mink::runtime::SessionPolicy;

    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let home = std::env::temp_dir().join(format!("mink_tui_prefab_resume_home_{unique}"));
    let cwd = std::env::temp_dir().join(format!("mink_tui_prefab_resume_cwd_{unique}"));
    std::fs::create_dir_all(&cwd).unwrap();

    let create_options = mink::runtime::AgentOptions::new(home.clone(), cwd.clone())
        .with_project_scoped_sessions()
        .with_prefix_source(std::sync::Arc::new(
            mink_prefab::adapter::PrefabPrefixSource,
        ))
        .with_post_init_hook(std::sync::Arc::new(
            mink_prefab::adapter::PrefabRestructureHook::builtin().unwrap(),
        ))
        .with_api_key("test-key")
        .with_base_url("https://example.invalid/v1");
    let created = mink::runtime::AgentRuntime::start(create_options)
        .await
        .unwrap();
    let original = created.session_info().clone();
    assert!(original.is_new);
    created.shutdown().await.unwrap();

    let resume_options = mink::runtime::AgentOptions::new(home.clone(), cwd.clone())
        .with_project_scoped_sessions()
        .with_session(SessionPolicy::ContinueLatest)
        .with_api_key("test-key")
        .with_base_url("https://example.invalid/v1");
    let resumed = mink::runtime::AgentRuntime::start(resume_options)
        .await
        .unwrap();
    let info = resumed.session_info().clone();
    assert!(!info.is_new);
    assert_eq!(info.session_id, original.session_id);
    resumed.shutdown().await.unwrap();

    let lines = load_session(&info.events_path);
    assert!(!lines.is_empty());
    assert!(
        lines
            .iter()
            .any(|line| line.text.contains("Read the workspace-root AGENTS.md"))
    );
    assert!(lines.iter().any(|line| line.text.contains("Ready.")));

    std::fs::remove_dir_all(&home).unwrap();
    std::fs::remove_dir_all(&cwd).unwrap();
}
