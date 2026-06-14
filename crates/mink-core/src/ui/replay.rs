use serde_json::Value;
use std::io::Write;
use std::path::Path;

use crate::session::store::first_line;
use crate::util::truncate_str;

/// Replay last N turns from events.jsonl synchronously to stdout/stderr.
pub fn replay_last_turns(events_path: &Path) {
    if !events_path.exists() {
        return;
    }

    let data = match std::fs::read_to_string(events_path) {
        Ok(d) => d,
        Err(_) => return,
    };

    let events: Vec<Value> = data
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect();

    if events.is_empty() {
        return;
    }

    let mut turn_starts: Vec<usize> = Vec::new();
    for (i, evt) in events.iter().enumerate() {
        let t = evt.get("type").and_then(Value::as_str).unwrap_or("");
        if t == "user_input" || t == "user_message" {
            turn_starts.push(i);
        }
    }

    if turn_starts.is_empty() {
        return;
    }

    let keep = turn_starts.len().saturating_sub(10);
    let start_idx = if keep < turn_starts.len() {
        turn_starts[keep]
    } else {
        0
    };
    let had_turns = turn_starts.len().saturating_sub(keep) > 0;

    let mut stdout = std::io::stdout();
    let mut stderr = std::io::stderr();
    let mut last_char = '\n';
    let mut prev_was_thinking = false;

    for evt in &events[start_idx..] {
        let evt_type = evt.get("type").and_then(Value::as_str).unwrap_or("");

        match evt_type {
            "session_start" | "usage" | "stop" | "retry" => continue,
            "user_input" | "user_message" => {
                flush_newline(&mut stdout, &mut last_char, prev_was_thinking);
                prev_was_thinking = false;
                let content = evt.get("content").and_then(Value::as_str).unwrap_or("");
                if !content.is_empty() {
                    let preview = truncate_str(first_line(content), 77);
                    let _ = writeln!(stderr, "\x1b[32m> {preview}\x1b[0m");
                }
                last_char = '\n';
            }
            "thinking" => {
                let content = evt.get("content").and_then(Value::as_str).unwrap_or("");
                let _ = write!(stdout, "\x1b[90m{content}\x1b[0m");
                let _ = stdout.flush();
                if let Some(c) = content.chars().last() {
                    last_char = c;
                }
                prev_was_thinking = true;
            }
            "text" => {
                if prev_was_thinking && last_char != '\n' {
                    let _ = writeln!(stdout);
                    last_char = '\n';
                }
                let content = evt.get("content").and_then(Value::as_str).unwrap_or("");
                let _ = write!(stdout, "{content}");
                let _ = stdout.flush();
                if let Some(c) = content.chars().last() {
                    last_char = c;
                }
                prev_was_thinking = false;
            }
            "tool_call" => {
                flush_newline(&mut stdout, &mut last_char, prev_was_thinking);
                prev_was_thinking = false;
                let name = evt.get("name").and_then(Value::as_str).unwrap_or("");
                let summary = build_replay_tool_summary(name, evt);
                let _ = writeln!(stdout, "\x1b[33m[tool] {}\x1b[0m", summary);
                last_char = '\n';
            }
            "tool_result" => {
                flush_newline(&mut stdout, &mut last_char, prev_was_thinking);
                prev_was_thinking = false;
                let name = evt.get("name").and_then(Value::as_str).unwrap_or("");
                let content = evt.get("content").and_then(Value::as_str).unwrap_or("");
                let preview = if name == "Edit" || name == "Read" || name == "Write" {
                    first_line(content).to_string()
                } else {
                    truncate_str(content, 200)
                };
                if !preview.is_empty() {
                    let _ = writeln!(stdout, "{preview}");
                    last_char = '\n';
                }
            }
            "error" => {
                flush_newline(&mut stdout, &mut last_char, prev_was_thinking);
                prev_was_thinking = false;
                let msg = evt.get("message").and_then(Value::as_str).unwrap_or("");
                let _ = writeln!(stderr, "\x1b[31mError: {msg}\x1b[0m");
                last_char = '\n';
            }
            "assistant_message" => {
                flush_newline(&mut stdout, &mut last_char, prev_was_thinking);
                prev_was_thinking = false;
                let text = evt.get("text").and_then(Value::as_str).unwrap_or("");
                if !text.is_empty() {
                    let _ = write!(stdout, "{text}");
                    let _ = stdout.flush();
                }
                if let Some(tool_calls) = evt.get("tool_calls").and_then(Value::as_array) {
                    for tc in tool_calls {
                        let name = tc.get("name").and_then(Value::as_str).unwrap_or("");
                        let summary = build_legacy_tool_summary(name, tc);
                        let _ = writeln!(stdout, "\x1b[33m[tool] {}\x1b[0m", summary);
                    }
                }
                last_char = '\n';
            }
            _ => {}
        }
    }

    flush_newline(&mut stdout, &mut last_char, prev_was_thinking);
    if had_turns {
        let _ = writeln!(stdout);
    }
}

fn flush_newline(stdout: &mut std::io::Stdout, last_char: &mut char, prev_was_thinking: bool) {
    if prev_was_thinking && *last_char != '\n' {
        let _ = writeln!(stdout);
        *last_char = '\n';
    }
}

fn build_replay_tool_summary(name: &str, evt: &Value) -> String {
    crate::session::store::build_tool_summary_from_json(name, evt)
}

fn build_legacy_tool_summary(name: &str, tc: &Value) -> String {
    crate::session::store::build_tool_summary_from_json(name, tc)
}
