use crate::session::store::first_line;
use crate::tui::signal::TuiSignal;
use crate::tui::state::{TranscriptItem, TranscriptKind, TuiState};
use crate::ui::{ArtifactDisplay, ToolPresentation, ToolResultKind};
use crate::util::truncate_str;
use std::collections::VecDeque;
use std::io::BufRead;
use std::path::Path;

const REPLAY_TURNS: usize = 10;

pub(crate) fn load_session(events_path: &Path) -> Vec<TranscriptItem> {
    if !events_path.exists() {
        return Vec::new();
    }
    let file = match std::fs::File::open(events_path) {
        Ok(file) => file,
        Err(_) => return Vec::new(),
    };
    let events = load_recent_turn_events(file);
    if events.is_empty() {
        return Vec::new();
    }

    build_lines_from_events(&events)
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

fn build_lines_from_events(events: &[serde_json::Value]) -> Vec<TranscriptItem> {
    let mut state = TuiState::default();
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
                state.finalize_stream();
                let preview = truncate_str(first_line(c), 77);
                if !preview.is_empty() {
                    state.push_line(TranscriptItem::new(
                        format!("> {preview}"),
                        TranscriptKind::Info,
                    ));
                }
            }
            "thinking" => state.apply(&TuiSignal::Thinking(c.to_string())),
            "text" => state.apply(&TuiSignal::Text(c.to_string())),
            "tool_call" => {
                let name = evt
                    .get("name")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("");
                let summary = build_replay_tool_summary(name, evt);
                state.apply(&TuiSignal::ToolCall {
                    tool_use_id: evt
                        .get("id")
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_owned),
                    tool_name: name.to_string(),
                    summary,
                });
            }
            "tool_result" => {
                let tool_name = evt
                    .get("name")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("");
                let presentation = evt
                    .get("presentation")
                    .cloned()
                    .and_then(|value| serde_json::from_value::<ToolPresentation>(value).ok());
                let artifacts = evt
                    .get("artifacts")
                    .cloned()
                    .and_then(|value| serde_json::from_value::<Vec<ArtifactDisplay>>(value).ok())
                    .unwrap_or_default();
                let result_kind = evt
                    .get("result_kind")
                    .cloned()
                    .and_then(|value| serde_json::from_value::<ToolResultKind>(value).ok())
                    .unwrap_or(ToolResultKind::Text);
                state.apply(&TuiSignal::ToolResult {
                    tool_use_id: evt
                        .get("tool_use_id")
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_owned),
                    tool_name: tool_name.to_string(),
                    content: c.to_string(),
                    success: evt
                        .get("success")
                        .and_then(serde_json::Value::as_bool)
                        .unwrap_or(true),
                    exit_code: evt
                        .get("exit_code")
                        .and_then(serde_json::Value::as_i64)
                        .and_then(|value| i32::try_from(value).ok()),
                    result_kind,
                    presentation,
                    artifacts,
                });
            }
            "error" => {
                let msg = evt
                    .get("message")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("");
                state.apply(&TuiSignal::Error(msg.to_string()));
            }
            _ => {}
        }
    }
    state.finalize_stream();
    for item in &mut state.lines {
        if !item.sealed {
            item.sealed = true;
            if item.kind == TranscriptKind::Tool {
                item.tool_success = Some(false);
                if item.text.starts_with("[tool]") {
                    item.text
                        .push_str("\nResult unavailable in the persisted event log.");
                }
            }
        }
    }
    state.lines
}

fn build_replay_tool_summary(name: &str, evt: &serde_json::Value) -> String {
    crate::session::store::build_tool_summary_from_json(name, evt)
}
