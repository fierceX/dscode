use crate::session::store::first_line;
use crate::tui::state::{MsgKind, MsgLine};
use crate::util::truncate_str;
use std::path::Path;

pub(crate) fn load_session(events_path: &Path) -> Vec<MsgLine> {
    let mut lines: Vec<MsgLine> = Vec::new();
    if !events_path.exists() {
        return lines;
    }
    let data = match std::fs::read_to_string(events_path) {
        Ok(d) => d,
        Err(_) => return lines,
    };
    let events: Vec<serde_json::Value> = data
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect();
    if events.is_empty() {
        return lines;
    }
    let mut turn_starts: Vec<usize> = Vec::new();
    for (i, evt) in events.iter().enumerate() {
        let t = evt
            .get("type")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("");
        if t == "user_input" || t == "user_message" {
            turn_starts.push(i);
        }
    }
    if turn_starts.is_empty() {
        return lines;
    }
    let keep = turn_starts.len().saturating_sub(10);
    let start_idx = if keep < turn_starts.len() {
        turn_starts[keep]
    } else {
        0
    };

    let mut buf = String::new();
    let mut buf_kind: Option<MsgKind> = None;

    let flush_buf = |lines: &mut Vec<MsgLine>, buf: &mut String, kind: &mut Option<MsgKind>| {
        if !buf.is_empty() {
            let k = kind.take().unwrap_or(MsgKind::Text);
            lines.push(MsgLine::new(std::mem::take(buf), k));
        }
    };

    for evt in &events[start_idx..] {
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
                flush_buf(&mut lines, &mut buf, &mut buf_kind);
                let preview = truncate_str(first_line(c), 77);
                if !preview.is_empty() {
                    lines.push(MsgLine::new(format!("> {preview}"), MsgKind::Info));
                }
            }
            "thinking" => {
                let target = MsgKind::StreamThinking;
                if buf_kind != Some(target) {
                    flush_buf(&mut lines, &mut buf, &mut buf_kind);
                    buf_kind = Some(target);
                }
                buf.push_str(c);
            }
            "text" => {
                let target = MsgKind::StreamText;
                if buf_kind != Some(target) {
                    flush_buf(&mut lines, &mut buf, &mut buf_kind);
                    buf_kind = Some(target);
                }
                buf.push_str(c);
            }
            "tool_call" => {
                flush_buf(&mut lines, &mut buf, &mut buf_kind);
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
                flush_buf(&mut lines, &mut buf, &mut buf_kind);
            }
            "error" => {
                flush_buf(&mut lines, &mut buf, &mut buf_kind);
                let msg = evt
                    .get("message")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("");
                lines.push(MsgLine::new(format!("Error: {msg}"), MsgKind::Error));
            }
            _ => {}
        }
    }
    flush_buf(&mut lines, &mut buf, &mut buf_kind);
    lines
}

fn build_replay_tool_summary(name: &str, evt: &serde_json::Value) -> String {
    crate::session::store::build_tool_summary_from_json(name, evt)
}
