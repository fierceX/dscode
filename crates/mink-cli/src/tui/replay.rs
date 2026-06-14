use crate::session::store::first_line;
use crate::tui::state::{MsgKind, MsgLine};
use crate::util::truncate_str;
use std::collections::VecDeque;
use std::io::BufRead;
use std::path::Path;

const REPLAY_TURNS: usize = 10;

pub(crate) fn load_session(events_path: &Path) -> Vec<MsgLine> {
    let mut lines: Vec<MsgLine> = Vec::new();
    if !events_path.exists() {
        return lines;
    }
    let file = match std::fs::File::open(events_path) {
        Ok(file) => file,
        Err(_) => return lines,
    };
    let events = load_recent_turn_events(file);
    if events.is_empty() {
        return lines;
    }

    build_lines_from_events(&events, &mut lines);
    lines
}

fn load_recent_turn_events(file: std::fs::File) -> Vec<serde_json::Value> {
    let mut turns: VecDeque<Vec<serde_json::Value>> = VecDeque::new();
    let mut current: Vec<serde_json::Value> = Vec::new();
    let mut seen_turn = false;
    for line in std::io::BufReader::new(file).lines() {
        let Ok(line) = line else {
            continue;
        };
        if line.trim().is_empty() {
            continue;
        }
        let Ok(evt) = serde_json::from_str::<serde_json::Value>(&line) else {
            continue;
        };
        let t = evt
            .get("type")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("");
        if t == "user_input" || t == "user_message" {
            if seen_turn && !current.is_empty() {
                turns.push_back(std::mem::take(&mut current));
                while turns.len() > REPLAY_TURNS {
                    turns.pop_front();
                }
            }
            seen_turn = true;
        }
        if seen_turn {
            current.push(evt);
        }
    }
    if seen_turn && !current.is_empty() {
        turns.push_back(current);
        while turns.len() > REPLAY_TURNS {
            turns.pop_front();
        }
    }
    turns.into_iter().flatten().collect()
}

fn build_lines_from_events(events: &[serde_json::Value], lines: &mut Vec<MsgLine>) {
    let mut buf = String::new();
    let mut buf_kind: Option<MsgKind> = None;

    let flush_buf = |lines: &mut Vec<MsgLine>, buf: &mut String, kind: &mut Option<MsgKind>| {
        if !buf.is_empty() {
            let k = kind.take().unwrap_or(MsgKind::Text);
            lines.push(MsgLine::new(std::mem::take(buf), k));
        }
    };

    for evt in events {
        let t = evt
            .get("type")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("");
        let c = evt
            .get("content")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("");
        match t {
            "user_input" | "user_message" => {
                flush_buf(lines, &mut buf, &mut buf_kind);
                let preview = truncate_str(first_line(c), 77);
                if !preview.is_empty() {
                    lines.push(MsgLine::new(format!("> {preview}"), MsgKind::Info));
                }
            }
            "thinking" => {
                let target = MsgKind::StreamThinking;
                if buf_kind != Some(target) {
                    flush_buf(lines, &mut buf, &mut buf_kind);
                    buf_kind = Some(target);
                }
                buf.push_str(c);
            }
            "text" => {
                let target = MsgKind::StreamText;
                if buf_kind != Some(target) {
                    flush_buf(lines, &mut buf, &mut buf_kind);
                    buf_kind = Some(target);
                }
                buf.push_str(c);
            }
            "tool_call" => {
                flush_buf(lines, &mut buf, &mut buf_kind);
                let name = evt
                    .get("name")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("");
                let summary = build_replay_tool_summary(name, evt);
                let text = if summary.is_empty() {
                    format!("[tool] {name}")
                } else {
                    format!("[tool] {summary}")
                };
                lines.push(MsgLine::new(text, MsgKind::ToolCall));
            }
            "tool_result" => {
                flush_buf(lines, &mut buf, &mut buf_kind);
            }
            "error" => {
                flush_buf(lines, &mut buf, &mut buf_kind);
                let msg = evt
                    .get("message")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("");
                lines.push(MsgLine::new(format!("Error: {msg}"), MsgKind::Error));
            }
            _ => {}
        }
    }
    flush_buf(lines, &mut buf, &mut buf_kind);
}

fn build_replay_tool_summary(name: &str, evt: &serde_json::Value) -> String {
    crate::session::store::build_tool_summary_from_json(name, evt)
}
