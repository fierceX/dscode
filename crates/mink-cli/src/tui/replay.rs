use crate::replay::ReplayEventKind;
use crate::session::store::first_line;
use crate::tui::signal::TuiSignal;
use crate::tui::state::{TranscriptItem, TranscriptKind, TuiState};
use crate::ui::{ArtifactDisplay, ToolPresentation, ToolResultKind};
use crate::util::truncate_display;
use std::collections::VecDeque;
use std::io::BufRead;
use std::path::Path;

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
        if crate::replay::classify_event(&evt) == ReplayEventKind::UserInput {
            if seen_turn && !current.is_empty() {
                turns.push_back(std::mem::take(&mut current));
                while turns.len() > crate::replay::REPLAY_TURNS {
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
        while turns.len() > crate::replay::REPLAY_TURNS {
            turns.pop_front();
        }
    }
    turns.into_iter().flatten().collect()
}

fn build_lines_from_events(events: &[serde_json::Value]) -> Vec<TranscriptItem> {
    let mut state = TuiState::default();
    for evt in events {
        let kind = crate::replay::classify_event(evt);
        let c = crate::replay::event_content(evt);
        match kind {
            // 前缀快照只用于离线重建，重放不渲染。
            ReplayEventKind::PrefixSnapshot | ReplayEventKind::Ignored => {}
            ReplayEventKind::UserInput => {
                state.finalize_stream();
                let preview = truncate_display(first_line(c), 77);
                if !preview.is_empty() {
                    state.push_line(TranscriptItem::new(
                        format!("> {preview}"),
                        TranscriptKind::Info,
                    ));
                }
            }
            ReplayEventKind::Thinking => state.apply(&TuiSignal::Thinking(c.to_string())),
            ReplayEventKind::Text => state.apply(&TuiSignal::Text(c.to_string())),
            ReplayEventKind::ToolCall => {
                let name = crate::replay::event_name(evt);
                let summary = crate::replay::build_tool_summary(name, evt);
                state.apply(&TuiSignal::ToolCall {
                    tool_use_id: evt
                        .get("id")
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_owned),
                    tool_name: name.to_string(),
                    summary,
                });
            }
            ReplayEventKind::ToolResult => {
                let tool_name = crate::replay::event_name(evt);
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
                        .get("status")
                        .and_then(|status| status.get("state"))
                        .and_then(serde_json::Value::as_str)
                        .is_none_or(|state| state == "succeeded"),
                    exit_code: evt
                        .get("exit_code")
                        .and_then(serde_json::Value::as_i64)
                        .and_then(|value| i32::try_from(value).ok()),
                    result_kind,
                    presentation,
                    artifacts,
                });
            }
            ReplayEventKind::Error => {
                let msg = crate::replay::event_message(evt);
                state.apply(&TuiSignal::Error(msg.to_string()));
            }
            ReplayEventKind::AssistantMessage | ReplayEventKind::Unknown => {}
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
