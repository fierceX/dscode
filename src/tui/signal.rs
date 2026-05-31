use crate::tui::state::{MsgKind, MsgLine, SubAgentDetail, TuiState, WorkState};
use crate::ui::StatsSnapshot;
use std::sync::mpsc;

#[derive(Debug, Clone, Default)]
pub enum TuiSignal {
    Thinking(String),
    Text(String),
    ToolCall(String, String),
    ToolResult {
        tool_name: String,
        content: String,
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

#[derive(Clone, Copy, Debug)]
pub enum SubAgentStreamKind {
    Thinking,
    Text,
}

impl TuiState {
    pub(crate) fn apply(&mut self, sig: &TuiSignal) {
        match sig {
            TuiSignal::Thinking(c) => {
                if !self.stream_line.is_empty() && self.stream_kind != MsgKind::StreamThinking {
                    self.stream_line.push('\n');
                    self.save_stream();
                }
                self.stream_kind = MsgKind::StreamThinking;
                self.streaming = true;
                self.stream_line.push_str(c);
                self.stream_revision = self.stream_revision.wrapping_add(1);
                self.work_state = WorkState::StreamingThinking;
            }
            TuiSignal::Text(c) => {
                if !self.stream_line.is_empty() && self.stream_kind != MsgKind::StreamText {
                    self.stream_line.push('\n');
                    self.save_stream();
                }
                self.stream_kind = MsgKind::StreamText;
                self.streaming = true;
                self.stream_line.push_str(c);
                self.stream_revision = self.stream_revision.wrapping_add(1);
                self.work_state = WorkState::StreamingText;
            }
            TuiSignal::Stop => {
                self.finalize_stream();
                self.work_state = WorkState::Idle;
            }
            TuiSignal::Retry => {
                self.finalize_stream();
                self.work_state = WorkState::WaitingModel;
            }
            TuiSignal::ToolCall(name, summary) => {
                self.finalize_stream();
                self.work_state = WorkState::RunningTool;
                let text = if summary.is_empty() {
                    format!("[tool] {name}")
                } else {
                    format!("[tool] {summary}")
                };
                self.push_line(MsgLine::new(text, MsgKind::ToolCall));
            }
            TuiSignal::Info(n) => {
                self.finalize_stream();
                self.push_line(MsgLine::new(n.clone(), MsgKind::Info));
            }
            TuiSignal::ToolResult { tool_name, content } => {
                self.finalize_stream();
                self.work_state = WorkState::WaitingModel;
                if !content.is_empty() {
                    self.push_line(MsgLine::new_tool_result(tool_name.clone(), content.clone()));
                }
            }
            TuiSignal::Error(m) => {
                self.finalize_stream();
                self.work_state = WorkState::Error;
                self.push_line(MsgLine::new(format!("Error: {m}"), MsgKind::Error));
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
                let launched = status == "launched" || status == "running";
                let sub_detail = if launched {
                    Some(SubAgentDetail {
                        thinking: String::new(),
                        text: String::new(),
                    })
                } else {
                    None
                };
                if launched {
                    self.sub_agents.active_sessions.insert(session_id.clone());
                    self.work_state = WorkState::RunningSubAgent;
                }
                if let Some(idx) = self.sub_agents.line_by_session.get(session_id).copied()
                    && let Some(line) = self.lines.get_mut(idx)
                {
                    line.text = title;
                    if launched && line.sub_detail.is_none() {
                        line.sub_detail = sub_detail;
                    }
                    line.invalidate_cache();
                    self.invalidate_all_cache();
                } else {
                    let idx = self.push_line(
                        MsgLine::new(title, MsgKind::SubAgent).with_sub_detail(sub_detail),
                    );
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
                let mut changed = false;
                if let Some(idx) = self.sub_agents.line_by_session.get(session_id).copied()
                    && let Some(line) = self.lines.get_mut(idx)
                    && let Some(ref mut detail) = line.sub_detail
                {
                    match kind {
                        SubAgentStreamKind::Thinking => detail.thinking.push_str(content),
                        SubAgentStreamKind::Text => detail.text.push_str(content),
                    }
                    line.invalidate_cache();
                    changed = true;
                }
                if changed {
                    self.invalidate_all_cache();
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
                let mut found = false;
                if let Some(idx) = self.sub_agents.line_by_session.get(session_id).copied()
                    && let Some(line) = self.lines.get_mut(idx)
                {
                    line.text = title.clone();
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
                    let idx =
                        self.push_line(MsgLine::new(title, MsgKind::SubAgent).with_sub_detail(
                            Some(SubAgentDetail {
                                thinking: thinking.clone(),
                                text: text.clone(),
                            }),
                        ));
                    self.sub_agents
                        .line_by_session
                        .insert(session_id.clone(), idx);
                }
            }
            TuiSignal::Shutdown => {}
        }
    }
}

pub(crate) fn drain_signals(rx: &mut mpsc::Receiver<TuiSignal>, state: &mut TuiState) -> bool {
    let mut drained = false;
    loop {
        match rx.try_recv() {
            Ok(sig) => {
                state.apply(&sig);
                drained = true;
            }
            Err(mpsc::TryRecvError::Empty) => break,
            Err(mpsc::TryRecvError::Disconnected) => break,
        }
    }
    drained
}
