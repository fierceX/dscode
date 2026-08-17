use crate::config::TuiMode;
use crate::tui::notify::TaskNotificationKind;
use crate::tui::sanitize::sanitize_tui_text;
use crate::tui::state::{SubAgentDetail, TranscriptItem, TranscriptKind, TuiState, WorkState};
use crate::ui::{ArtifactDisplay, ToolPresentation, ToolResultKind};
use crate::ui::{StatsSnapshot, SubAgentStreamKind};
use std::sync::mpsc;

#[derive(Debug, Clone, Default)]
pub enum TuiSignal {
    Thinking(String),
    Text(String),
    ToolCall {
        tool_use_id: Option<String>,
        tool_name: String,
        summary: String,
    },
    ToolResult {
        tool_use_id: Option<String>,
        tool_name: String,
        content: String,
        success: bool,
        exit_code: Option<i32>,
        result_kind: ToolResultKind,
        presentation: Option<ToolPresentation>,
        artifacts: Vec<ArtifactDisplay>,
    },
    Error(String),
    Stop,
    Retry,
    Info(String),
    TitleUpdate(String, StatsSnapshot),
    SubAgentStatus {
        session_id: String,
        status: String,
        in_tokens: u64,
        out_tokens: u64,
    },
    SubAgentStream {
        session_id: String,
        kind: SubAgentStreamKind,
        content: String,
    },
    SubAgentOutput {
        session_id: String,
        status: String,
        thinking: String,
        text: String,
        in_tokens: u64,
        out_tokens: u64,
    },
    #[default]
    Shutdown,
}

impl TuiState {
    pub(crate) fn apply(&mut self, sig: &TuiSignal) {
        match sig {
            TuiSignal::Thinking(c) => {
                let c = sanitize_tui_text(c);
                if !self.stream_line.is_empty()
                    && self.stream_kind != TranscriptKind::StreamThinking
                {
                    self.stream_line.push('\n');
                    self.save_stream();
                }
                self.stream_kind = TranscriptKind::StreamThinking;
                self.streaming = true;
                self.stream_line.push_str(&c);
                self.stream_revision = self.stream_revision.wrapping_add(1);
                self.work_state = WorkState::StreamingThinking;
            }
            TuiSignal::Text(c) => {
                let c = sanitize_tui_text(c);
                if !self.stream_line.is_empty() && self.stream_kind != TranscriptKind::StreamText {
                    self.stream_line.push('\n');
                    self.save_stream();
                }
                self.stream_kind = TranscriptKind::StreamText;
                self.streaming = true;
                self.stream_line.push_str(&c);
                self.stream_revision = self.stream_revision.wrapping_add(1);
                self.work_state = WorkState::StreamingText;
            }
            TuiSignal::Stop => {
                self.finalize_stream();
                self.seal_incomplete_transcript("Result unavailable: turn stopped.");
                self.work_state = WorkState::Idle;
                self.finish_task_notification(TaskNotificationKind::Completed);
            }
            TuiSignal::Retry => {
                self.finalize_stream();
                self.work_state = WorkState::WaitingModel;
            }
            TuiSignal::ToolCall {
                tool_use_id,
                tool_name,
                summary,
            } => {
                self.finalize_stream();
                self.work_state = WorkState::RunningTool;
                self.push_line(TranscriptItem::new_tool_call(
                    tool_use_id.clone(),
                    tool_name.clone(),
                    summary.clone(),
                ));
            }
            TuiSignal::Info(n) => {
                self.finalize_stream();
                self.push_line(TranscriptItem::new(n.clone(), TranscriptKind::Info));
            }
            TuiSignal::ToolResult {
                tool_use_id,
                tool_name,
                content,
                success,
                exit_code,
                result_kind,
                presentation,
                artifacts,
            } => {
                self.finalize_stream();
                self.work_state = WorkState::WaitingModel;
                let existing = tool_use_id.as_ref().and_then(|id| {
                    self.lines
                        .get(self.inline.committed..)
                        .and_then(|items| {
                            items.iter().rposition(|item| {
                                !item.sealed && item.tool_use_id.as_ref() == Some(id)
                            })
                        })
                        .map(|idx| self.inline.committed + idx)
                });
                let idx = if let Some(idx) = existing {
                    idx
                } else {
                    self.push_line(TranscriptItem::new_tool_result(
                        tool_name.clone(),
                        String::new(),
                    ))
                };
                if let Some(item) = self.lines.get_mut(idx) {
                    item.text = content.clone();
                    item.tool_name = Some(tool_name.clone());
                    item.tool_use_id.clone_from(tool_use_id);
                    item.tool_success = Some(*success);
                    item.tool_exit_code = *exit_code;
                    item.tool_result_kind = Some(*result_kind);
                    item.presentation.clone_from(presentation);
                    item.artifacts.clone_from(artifacts);
                    item.sealed = true;
                    item.invalidate_cache();
                }
                match presentation {
                    Some(ToolPresentation::Plan(plan)) => self.plan = Some(plan.clone()),
                    Some(ToolPresentation::Todo(todos)) => self.apply_todo_presentation(todos),
                    None => {}
                }
                self.invalidate_all_cache();
            }
            TuiSignal::Error(m) => {
                self.finalize_stream();
                self.seal_incomplete_transcript("Result unavailable: turn failed.");
                self.work_state = WorkState::Error;
                self.push_line(TranscriptItem::new(
                    format!("Error: {m}"),
                    TranscriptKind::Error,
                ));
                self.finish_task_notification(TaskNotificationKind::Failed);
            }
            TuiSignal::TitleUpdate(m, s) => {
                self.model = m.clone();
                self.stats = s.clone();
            }
            TuiSignal::SubAgentStatus {
                session_id,
                status,
                in_tokens,
                out_tokens,
            } => {
                let title = format!(
                    "[sub-agent {}] {} (in={}, out={})",
                    session_id, status, in_tokens, out_tokens
                );
                let running = status == "launched" || status == "running";
                let terminal = matches!(
                    status.as_str(),
                    "ok" | "failed" | "timed_out" | "cancelled" | "channel_closed"
                );
                let sub_detail = if running {
                    Some(SubAgentDetail {
                        thinking: String::new(),
                        text: String::new(),
                    })
                } else {
                    None
                };
                if running {
                    self.sub_agents.active_sessions.insert(session_id.clone());
                    self.work_state = WorkState::RunningSubAgent;
                } else if terminal {
                    self.sub_agents.active_sessions.remove(session_id);
                    if self.sub_agents.active_sessions.is_empty() {
                        self.work_state = WorkState::WaitingModel;
                    }
                }
                if let Some(idx) = self.sub_agents.line_by_session.get(session_id).copied()
                    && let Some(line) = self.lines.get_mut(idx)
                {
                    line.text = title;
                    line.sealed = !running;
                    if running && line.sub_detail.is_none() {
                        line.sub_detail = sub_detail;
                    }
                    line.invalidate_cache();
                    self.invalidate_all_cache();
                } else {
                    let mut item = TranscriptItem::new(title, TranscriptKind::SubAgent)
                        .with_sub_detail(sub_detail);
                    item.sealed = !running;
                    let idx = self.push_line(item);
                    self.sub_agents
                        .line_by_session
                        .insert(session_id.clone(), idx);
                }
            }
            TuiSignal::SubAgentStream {
                session_id,
                kind,
                content,
            } => {
                if let Some(idx) = self.sub_agents.line_by_session.get(session_id).copied()
                    && let Some(line) = self.lines.get_mut(idx)
                    && let Some(ref mut detail) = line.sub_detail
                {
                    let content = sanitize_tui_text(content);
                    match kind {
                        SubAgentStreamKind::Thinking => detail.thinking.push_str(&content),
                        SubAgentStreamKind::Text => detail.text.push_str(&content),
                    }
                }
            }
            TuiSignal::SubAgentOutput {
                session_id,
                status,
                thinking,
                text,
                in_tokens,
                out_tokens,
            } => {
                self.finalize_stream();
                self.sub_agents.active_sessions.remove(session_id);
                self.work_state = if self.sub_agents.active_sessions.is_empty() {
                    WorkState::WaitingModel
                } else {
                    WorkState::RunningSubAgent
                };
                let title = format!(
                    "[sub-agent {}] {} (in={}, out={})",
                    session_id, status, in_tokens, out_tokens
                );
                let thinking = sanitize_tui_text(thinking);
                let text = sanitize_tui_text(text);
                let mut found = false;
                if let Some(idx) = self.sub_agents.line_by_session.get(session_id).copied()
                    && let Some(line) = self.lines.get_mut(idx)
                {
                    line.text = title.clone();
                    line.sealed = true;
                    if let Some(ref mut detail) = line.sub_detail {
                        detail.thinking = thinking.clone();
                        detail.text = text.clone();
                    }
                    line.invalidate_cache();
                    found = true;
                }
                if found {
                    self.invalidate_all_cache();
                }
                if !found {
                    let mut item = TranscriptItem::new(title, TranscriptKind::SubAgent)
                        .with_sub_detail(Some(SubAgentDetail {
                            thinking: thinking.clone(),
                            text: text.clone(),
                        }));
                    item.sealed = true;
                    let idx = self.push_line(item);
                    self.sub_agents
                        .line_by_session
                        .insert(session_id.clone(), idx);
                }
            }
            TuiSignal::Shutdown => {}
        }
    }
}

pub(crate) fn drain_signals(
    rx: &mut mpsc::Receiver<TuiSignal>,
    state: &mut TuiState,
    mode: TuiMode,
) -> bool {
    const MAX_SIGNALS_PER_TICK: usize = 512;
    let mut pending = Vec::new();
    for _ in 0..MAX_SIGNALS_PER_TICK {
        let sig = match rx.try_recv() {
            Ok(sig) => sig,
            Err(mpsc::TryRecvError::Empty | mpsc::TryRecvError::Disconnected) => break,
        };
        match (pending.last_mut(), sig) {
            (Some(TuiSignal::Thinking(existing)), TuiSignal::Thinking(next))
            | (Some(TuiSignal::Text(existing)), TuiSignal::Text(next)) => {
                existing.push_str(&next);
            }
            (_, sig) => pending.push(sig),
        }
    }

    let mut visible_change = false;
    for sig in pending {
        let visible = match &sig {
            TuiSignal::SubAgentStream { session_id, .. } => matches!(
                &state.view,
                crate::tui::state::View::SubAgentDetail {
                    session_id: visible,
                    ..
                } if visible == session_id
            ),
            _ => true,
        };
        state.apply(&sig);
        if mode == TuiMode::Inline {
            state.promote_stable_stream_prefix();
        }
        visible_change |= visible;
    }
    visible_change
}
