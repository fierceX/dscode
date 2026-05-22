//! TUI module for dscode using ratatui.

use crate::agent::orchestrator::OrchCmd;
use crate::ui::{Display, StatsSnapshot};
use crate::util::{fmt_k, truncate_str};
use crossterm::event::{Event, KeyCode, KeyModifiers, MouseEventKind};
use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, Borders, Paragraph},
    Frame,
};
use std::fmt::{Debug, Display as FmtDisplay, Formatter};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::Arc;
use std::time::Duration;

// ─── Signals ───────────────────────────────────────────────

#[derive(Debug, Clone)]
pub enum TuiSignal {
    Thinking(String),
    Text(String),
    ToolCall(String, String),
    ToolResult(String),
    Error(String),
    Stop,
    Retry,
    Info(String),
    TitleUpdate(String, StatsSnapshot),
    SubAgentStatus(String),
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
    Shutdown,
}

impl Default for TuiSignal {
    fn default() -> Self { Self::Shutdown }
}

// ─── Message kinds for styling ─────────────────────────────

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum MsgKind {
    Text,
    ToolCall,
    ToolResult,
    Error,
    Info,
    SubAgent,
    StreamThinking,
    StreamText,
}

impl Default for MsgKind {
    fn default() -> Self { MsgKind::Text }
}

// ─── SubAgent detail data ─────────────────────────────────

#[derive(Clone)]
pub(crate) struct SubAgentDetail {
    pub thinking: String,
    pub text: String,
}

#[derive(Clone, Copy, Debug)]
pub enum SubAgentStreamKind {
    Thinking,
    Text,
}

// ─── State ─────────────────────────────────────────────────

#[derive(Clone)]
pub(crate) struct MsgLine {
    pub text: String,
    pub kind: MsgKind,
    pub collapsed: bool,
    pub cached_lines: Option<Vec<Line<'static>>>,
    pub cached_collapsed: bool,
    pub sub_detail: Option<SubAgentDetail>,
}

impl MsgLine {
    fn cache_valid(&self) -> bool {
        self.cached_lines.is_some() && self.cached_collapsed == self.collapsed
    }
}

impl Default for MsgLine {
    fn default() -> Self {
        MsgLine { text: String::new(), kind: MsgKind::Text, collapsed: false, cached_lines: None, cached_collapsed: false, sub_detail: None }
    }
}

#[derive(Clone)]
pub(crate) struct TuiState {
    pub lines: Vec<MsgLine>,
    pub stream_line: String,
    pub stream_kind: MsgKind,
    pub streaming: bool,
    pub input_buf: String,
    pub input_cursor: usize,
    pub input_history: Vec<String>,
    pub history_idx: Option<usize>,
    pub model: String,
    pub stats: StatsSnapshot,
    pub scroll: u16,
    pub auto_scroll: bool,
    pub max_scroll: u16,
    pub show_borders: bool,
    pub click_map: Vec<(usize, u16, u16)>,
    pub content_y: u16,
    pub effective_scroll: u16,
    pub dirty: bool,
    pub cached_width: u16,
    pub cached_all: Option<Vec<Line<'static>>>,
    pub quit: bool,
    /// 系统忙（等待 API / 工具执行），非 idle
    pub busy: bool,
    /// 中断当前任务（由 Ctrl+C 触发），None 表示无中断能力
    pub interrupt: Option<Arc<AtomicBool>>,
    pub view: View,
}

// ─── View navigation ─────────────────────────────────────

#[derive(Clone, Debug)]
pub(crate) enum View {
    Main,
    SubAgentDetail {
        line_idx: usize,
        scroll: u16,
    },
}

impl Default for View {
    fn default() -> Self { View::Main }
}

impl Default for TuiState {
    fn default() -> Self {
        Self {
            lines: Vec::new(),
            stream_line: String::new(),
            stream_kind: MsgKind::default(),
            streaming: false,
            input_buf: String::new(),
            input_cursor: 0,
            input_history: Vec::new(),
            history_idx: None,
            model: "flash".into(),
            stats: StatsSnapshot::default(),
            scroll: 0,
            auto_scroll: true,
            max_scroll: 0,
            show_borders: true,
            click_map: Vec::new(),
            content_y: 0,
            effective_scroll: 0,
            dirty: true,
            cached_width: 0,
            cached_all: None,
            quit: false,
            busy: false,
            interrupt: None,
            view: View::Main,
        }
    }
}

impl Debug for TuiState {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "TuiState[lines={}, stream={}, input={}]",
            self.lines.len(), self.stream_line.len(), self.input_buf.len())
    }
}

impl FmtDisplay for TuiState {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        Debug::fmt(self, f)
    }
}

impl TuiState {
    fn save_stream(&mut self) {
        let text = std::mem::take(&mut self.stream_line);
        if !text.is_empty() {
            self.lines.push(MsgLine {
                text,
                kind: self.stream_kind,
                collapsed: self.stream_kind == MsgKind::StreamThinking,
                cached_lines: None,
                cached_collapsed: self.stream_kind == MsgKind::StreamThinking,
                sub_detail: None,
            });
        }
        self.auto_scroll = true;
    }

    fn finalize_stream(&mut self) {
        self.save_stream();
        self.streaming = false;
    }

    fn apply(&mut self, sig: &TuiSignal) {
        match sig {
            TuiSignal::Thinking(c) => {
                if !self.stream_line.is_empty() && self.stream_kind != MsgKind::StreamThinking {
                    self.stream_line.push('\n');
                    self.save_stream();
                }
                self.stream_kind = MsgKind::StreamThinking;
                self.streaming = true;
                self.stream_line.push_str(c);
                self.busy = false; // 开始流式输出，不再"waiting"
            }
            TuiSignal::Text(c) => {
                if !self.stream_line.is_empty() && self.stream_kind != MsgKind::StreamText {
                    self.stream_line.push('\n');
                    self.save_stream();
                }
                self.stream_kind = MsgKind::StreamText;
                self.streaming = true;
                self.stream_line.push_str(c);
                self.busy = false; // 开始流式输出，不再"waiting"
            }
            TuiSignal::Stop => {
                self.finalize_stream();
                self.busy = false; // 本轮结束，进入 idle
            }
            TuiSignal::Retry => {
                self.finalize_stream();
            }
            TuiSignal::ToolCall(name, summary) => {
                self.finalize_stream();
                self.busy = true; // 工具已发出，等待执行结果
                let text = if summary.is_empty() { format!("[tool] {name}") } else { format!("[tool] {summary}") };
                self.lines.push(MsgLine {
                    text,
                    kind: MsgKind::ToolCall,
                    collapsed: false,
                    cached_lines: None,
                    cached_collapsed: false,
                    sub_detail: None,
                });
            }
            TuiSignal::Info(n) => {
                self.finalize_stream();
                self.lines.push(MsgLine { text: n.clone(), kind: MsgKind::Info, collapsed: false, cached_lines: None, cached_collapsed: false, sub_detail: None });
            }
            TuiSignal::ToolResult(c) => {
                self.finalize_stream();
                if !c.is_empty() {
                    self.lines.push(MsgLine {
                        text: c.clone(),
                        kind: MsgKind::ToolResult,
                        collapsed: false,
                        cached_lines: None,
                        cached_collapsed: false,
                        sub_detail: None,
                    });
                }
            }
            TuiSignal::Error(m) => {
                self.finalize_stream();
                self.busy = false; // 错误终止当前任务
                self.lines.push(MsgLine {
                    text: format!("Error: {m}"),
                    kind: MsgKind::Error,
                    collapsed: false,
                    cached_lines: None,
                    cached_collapsed: false,
                    sub_detail: None,
                });
            }
            TuiSignal::TitleUpdate(m, s) => { self.model = m.clone(); self.stats = s.clone(); }
            TuiSignal::SubAgentStatus(l) => {
                let launched = l.contains("launched");
                let sub_detail = if launched {
                    Some(SubAgentDetail { thinking: String::new(), text: String::new() })
                } else {
                    None
                };
                self.busy = launched; // launched → wait for sub-agent result
                self.lines.push(MsgLine { text: l.clone(), kind: MsgKind::SubAgent, collapsed: false, cached_lines: None, cached_collapsed: false, sub_detail });
            }
            TuiSignal::SubAgentStream { session_id, kind, content } => {
                // 追加到匹配 session_id 的 SubAgent 行的 detail 中
                for line in self.lines.iter_mut().rev() {
                    if line.kind == MsgKind::SubAgent && line.text.contains(session_id.as_str()) {
                        if let Some(ref mut detail) = line.sub_detail {
                            match kind {
                                SubAgentStreamKind::Thinking => detail.thinking.push_str(&content),
                                SubAgentStreamKind::Text => detail.text.push_str(&content),
                            }
                        }
                        break;
                    }
                }
            }
            TuiSignal::SubAgentOutput { session_id, status, thinking, text, in_tokens, out_tokens } => {
                self.finalize_stream();
                let title = format!("[sub-agent {}] {} (in={}, out={})",
                    session_id, status, in_tokens, out_tokens);
                // 更新已有的 launched 行（而非创建新行）
                let mut found = false;
                for line in self.lines.iter_mut().rev() {
                    if line.kind == MsgKind::SubAgent && line.text.contains(session_id.as_str()) {
                        line.text = title.clone();
                        if let Some(ref mut detail) = line.sub_detail {
                            detail.thinking = thinking.clone();
                            detail.text = text.clone();
                        }
                        found = true;
                        break;
                    }
                }
                if !found {
                    // 回退：launched 行未找到时新建
                    self.lines.push(MsgLine {
                        text: title,
                        kind: MsgKind::SubAgent,
                        collapsed: false,
                        cached_lines: None,
                        cached_collapsed: false,
                        sub_detail: Some(SubAgentDetail { thinking: thinking.clone(), text: text.clone() }),
                    });
                }
            }
            TuiSignal::Shutdown => {}
        }
    }

    fn add_help(&mut self) {
        self.lines.push(MsgLine { text: "Commands:".into(), kind: MsgKind::Info, collapsed: false, cached_lines: None, cached_collapsed: false, sub_detail: None });
        self.lines.push(MsgLine { text: "  /flash          Switch to flash tier".into(), kind: MsgKind::Text, collapsed: false, cached_lines: None, cached_collapsed: false, sub_detail: None });
        self.lines.push(MsgLine { text: "  /pro            Switch to pro tier".into(), kind: MsgKind::Text, collapsed: false, cached_lines: None, cached_collapsed: false, sub_detail: None });
        self.lines.push(MsgLine { text: "  /compact        Force context compaction".into(), kind: MsgKind::Text, collapsed: false, cached_lines: None, cached_collapsed: false, sub_detail: None });
        self.lines.push(MsgLine { text: "  /skills         List available skills".into(), kind: MsgKind::Text, collapsed: false, cached_lines: None, cached_collapsed: false, sub_detail: None });
        self.lines.push(MsgLine { text: "  Ctrl+C          Interrupt current task".into(), kind: MsgKind::Text, collapsed: false, cached_lines: None, cached_collapsed: false, sub_detail: None });
        self.lines.push(MsgLine { text: "  Ctrl+C again    Exit TUI".into(), kind: MsgKind::Text, collapsed: false, cached_lines: None, cached_collapsed: false, sub_detail: None });
        self.lines.push(MsgLine { text: "  Esc             Exit TUI".into(), kind: MsgKind::Text, collapsed: false, cached_lines: None, cached_collapsed: false, sub_detail: None });
        self.lines.push(MsgLine { text: "  /exit  /quit    Exit TUI".into(), kind: MsgKind::Text, collapsed: false, cached_lines: None, cached_collapsed: false, sub_detail: None });
    }

    fn show_skills(&mut self) {
        self.lines.push(MsgLine { text: "=== Built-in Skills ===".into(), kind: MsgKind::Info, collapsed: false, cached_lines: None, cached_collapsed: false, sub_detail: None });
        for skill in crate::assets::embedded_skills::all() {
            self.lines.push(MsgLine { text: format!("  {} — {}", skill.name, skill.description), kind: MsgKind::Text, collapsed: false, cached_lines: None, cached_collapsed: false, sub_detail: None });
        }
        self.lines.push(MsgLine {
            text: "Use --skill NAME or Skill(name) to load.".into(),
            kind: MsgKind::Info,
            collapsed: false,
            cached_lines: None,
            cached_collapsed: false,
            sub_detail: None,
        });
    }
}

// ─── Section styling ───────────────────────────────────────

fn style_for_kind(kind: MsgKind) -> Style {
    match kind {
        MsgKind::StreamThinking => Style::default().fg(Color::Rgb(139, 139, 139)),
        MsgKind::Text | MsgKind::StreamText => Style::default(),
        MsgKind::ToolCall => Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
        MsgKind::Error => Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        MsgKind::Info => Style::default().fg(Color::Yellow),
        MsgKind::SubAgent => Style::default().fg(Color::Magenta),
        MsgKind::ToolResult => Style::default().fg(Color::Rgb(100, 100, 100)),
    }
}

// ─── Session replay ────────────────────────────────────────

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
        let t = evt.get("type").and_then(serde_json::Value::as_str).unwrap_or("");
        if t == "user_input" || t == "user_message" {
            turn_starts.push(i);
        }
    }
    if turn_starts.is_empty() {
        return lines;
    }
    let keep = turn_starts.len().saturating_sub(10);
    let start_idx = if keep < turn_starts.len() { turn_starts[keep] } else { 0 };

    let mut buf = String::new();
    let mut buf_kind: Option<MsgKind> = None;

    let flush_buf = |lines: &mut Vec<MsgLine>, buf: &mut String, kind: &mut Option<MsgKind>| {
        if !buf.is_empty() {
            let k = kind.take().unwrap_or(MsgKind::Text);
            lines.push(MsgLine { text: std::mem::take(buf), kind: k, collapsed: k == MsgKind::StreamThinking, cached_lines: None, cached_collapsed: k == MsgKind::StreamThinking, sub_detail: None });
        }
    };

    for evt in &events[start_idx..] {
        let t = evt.get("type").and_then(serde_json::Value::as_str).unwrap_or("");
        let c = evt.get("content").and_then(serde_json::Value::as_str).unwrap_or("");
        match t {
            "user_input" | "user_message" => {
                flush_buf(&mut lines, &mut buf, &mut buf_kind);
                let preview = truncate_str(crate::session::store::first_line(c), 77);
                if !preview.is_empty() {
                    lines.push(MsgLine { text: format!("> {preview}"), kind: MsgKind::Info, collapsed: false, cached_lines: None, cached_collapsed: false, sub_detail: None });
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
                let name = evt.get("name").and_then(serde_json::Value::as_str).unwrap_or("");
                let summary = build_replay_tool_summary(name, evt);
                let text = if summary.is_empty() { format!("[tool] {name}") } else { format!("[tool] {summary}") };
                lines.push(MsgLine { text, kind: MsgKind::ToolCall, collapsed: false, cached_lines: None, cached_collapsed: false, sub_detail: None });
            }
            "tool_result" => {
                flush_buf(&mut lines, &mut buf, &mut buf_kind);
            }
            "error" => {
                flush_buf(&mut lines, &mut buf, &mut buf_kind);
                let msg = evt.get("message").and_then(serde_json::Value::as_str).unwrap_or("");
                lines.push(MsgLine { text: format!("Error: {msg}"), kind: MsgKind::Error, collapsed: false, cached_lines: None, cached_collapsed: false, sub_detail: None });
            }
            _ => {}
        }
    }
    flush_buf(&mut lines, &mut buf, &mut buf_kind);
    lines
}

// ─── TUI runner ────────────────────────────────────────────

pub fn run_tui(
    sig_rx: mpsc::Receiver<TuiSignal>,
    orch_tx: tokio::sync::mpsc::UnboundedSender<OrchCmd>,
    events_path: &Path,
    interrupt: Option<Arc<AtomicBool>>,
) -> anyhow::Result<()> {
    let mut terminal = ratatui::init();
    crossterm::execute!(std::io::stdout(),
        crossterm::event::EnableMouseCapture,
        crossterm::event::EnableBracketedPaste,
    )?;
    struct RestoreGuard;
    impl Drop for RestoreGuard {
        fn drop(&mut self) {
            let _ = crossterm::execute!(std::io::stdout(),
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
    let mut state = TuiState::default();
    state.lines = load_session(events_path);
    state.interrupt = interrupt;
    let mut sig_rx = sig_rx;

    loop {
        if drain_signals(&mut sig_rx, &mut state) {
            state.dirty = true;
        }
        if state.quit {
            break;
        }

        // render BEFORE event poll — ensures click_map is fresh
        if state.dirty {
            terminal.draw(|f| render(f, &mut state))?;
            state.dirty = false;
        }

        // 流式时用更短的 poll 间隔提升实时性
        let poll_timeout = if state.streaming {
            Duration::from_millis(16)
        } else {
            Duration::from_millis(100)
        };
        if crossterm::event::poll(poll_timeout).unwrap_or(false) {
            let mut should_quit = false;
            loop {
                match crossterm::event::read() {
                    Ok(ev) => {
                        if handle_event(ev, &mut state, &orch_tx) { should_quit = true; break; }
                    }
                    Err(_) => break,
                }
                if !crossterm::event::poll(Duration::ZERO).unwrap_or(false) { break; }
            }
            state.dirty = true;
            if should_quit { break; }
        }
    }
    Ok(())
}

fn drain_signals(rx: &mut mpsc::Receiver<TuiSignal>, state: &mut TuiState) -> bool {
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

const SCROLL_STEP: u16 = 3;

fn scroll_by(state: &mut TuiState, delta: i16) {
    let base = if state.auto_scroll { state.max_scroll } else { state.scroll };
    state.auto_scroll = false;
    if delta < 0 {
        state.scroll = base.saturating_sub(delta.unsigned_abs());
    } else {
        state.scroll = base.saturating_add(delta as u16).min(state.max_scroll);
    }
}

fn handle_key(
    key: crossterm::event::KeyEvent,
    state: &mut TuiState,
    orch_tx: &tokio::sync::mpsc::UnboundedSender<OrchCmd>,
) -> bool {
    if key.kind == crossterm::event::KeyEventKind::Release {
        return false;
    }

    // 详情视图专用按键
    if matches!(state.view, View::SubAgentDetail { .. }) {
        match (key.modifiers, key.code) {
            (KeyModifiers::NONE, KeyCode::Esc) => {
                state.view = View::Main;
                return false;
            }
            (KeyModifiers::NONE, KeyCode::PageUp) => {
                if let View::SubAgentDetail { scroll, .. } = &mut state.view {
                    *scroll = scroll.saturating_sub(10);
                }
                return false;
            }
            (KeyModifiers::NONE, KeyCode::PageDown) => {
                if let View::SubAgentDetail { scroll, .. } = &mut state.view {
                    *scroll = scroll.saturating_add(10);
                }
                return false;
            }
            (KeyModifiers::NONE, KeyCode::Up) => {
                if let View::SubAgentDetail { scroll, .. } = &mut state.view {
                    *scroll = scroll.saturating_sub(1);
                }
                return false;
            }
            (KeyModifiers::NONE, KeyCode::Down) => {
                if let View::SubAgentDetail { scroll, .. } = &mut state.view {
                    *scroll = scroll.saturating_add(1);
                }
                return false;
            }
            _ => { return false; }  // 忽略详情视图中的其他按键
        }
    }

    match (key.modifiers, key.code) {
        (KeyModifiers::CONTROL, KeyCode::Char('c')) => {
            if let Some(ref interrupt) = state.interrupt {
                interrupt.store(true, Ordering::SeqCst);
                return false; // 中断当前任务，不退出
            } else {
                state.quit = true;
                return true;
            }
        }
        (KeyModifiers::CONTROL, KeyCode::Char('t')) => { state.show_borders = !state.show_borders; }
        (KeyModifiers::NONE, KeyCode::Esc) => { state.quit = true; return true; }
        (KeyModifiers::NONE, KeyCode::Char(c)) => {
            if !c.is_control() {
                state.input_buf.insert(state.input_cursor, c);
                state.input_cursor += c.len_utf8();
            }
        }
        (KeyModifiers::NONE, KeyCode::Left) => { cursor_left(state); }
        (KeyModifiers::NONE, KeyCode::Right) => { cursor_right(state); }
        (KeyModifiers::NONE, KeyCode::Enter) => { return handle_enter(state, orch_tx); }
        (KeyModifiers::NONE, KeyCode::Backspace) => cursor_backspace(state),
        (KeyModifiers::CONTROL, KeyCode::Char('w')) => cursor_delete_word(state),
        (KeyModifiers::NONE, KeyCode::PageUp) => { scroll_by(state, -((SCROLL_STEP * 5) as i16)); }
        (KeyModifiers::NONE, KeyCode::PageDown) => { scroll_by(state, (SCROLL_STEP * 5) as i16); }
        (KeyModifiers::NONE, KeyCode::Up) => {
            if !state.input_history.is_empty() && state.input_buf.is_empty() {
                let idx = match state.history_idx {
                    None => state.input_history.len().saturating_sub(1),
                    Some(i) => i.saturating_sub(1),
                };
                state.input_buf = state.input_history[idx].clone();
                state.input_cursor = state.input_buf.len();
                state.history_idx = Some(idx);
            } else {
                scroll_by(state, -(SCROLL_STEP as i16));
            }
        }
        (KeyModifiers::NONE, KeyCode::Down) => {
            if let Some(idx) = state.history_idx {
                let next = idx + 1;
                if next >= state.input_history.len() {
                    state.input_buf.clear();
                    state.input_cursor = 0;
                    state.history_idx = None;
                } else {
                    state.input_buf = state.input_history[next].clone();
                    state.input_cursor = state.input_buf.len();
                    state.history_idx = Some(next);
                }
            } else {
                scroll_by(state, SCROLL_STEP as i16);
            }
        }
        _ => {}
    }
    false
}

fn cursor_left(state: &mut TuiState) {
    if state.input_cursor > 0 {
        let mut pos = state.input_cursor - 1;
        while pos > 0 && !state.input_buf.is_char_boundary(pos) { pos -= 1; }
        state.input_cursor = pos;
    }
}

fn cursor_right(state: &mut TuiState) {
    if state.input_cursor < state.input_buf.len() {
        let mut pos = state.input_cursor + 1;
        while pos < state.input_buf.len() && !state.input_buf.is_char_boundary(pos) { pos += 1; }
        state.input_cursor = pos;
    }
}

fn cursor_backspace(state: &mut TuiState) {
    if state.input_cursor > 0 {
        let mut pos = state.input_cursor - 1;
        while pos > 0 && !state.input_buf.is_char_boundary(pos) { pos -= 1; }
        state.input_buf.remove(pos);
        state.input_cursor = pos;
    }
}

fn cursor_delete_word(state: &mut TuiState) {
    let prefix: String = state.input_buf[..state.input_cursor].to_string();
    if let Some(pos) = prefix.trim_end().rfind(' ') {
        state.input_buf.replace_range(pos..state.input_cursor, "");
        state.input_cursor = pos;
    } else {
        state.input_buf.replace_range(..state.input_cursor, "");
        state.input_cursor = 0;
    }
}

fn handle_enter(
    state: &mut TuiState,
    orch_tx: &tokio::sync::mpsc::UnboundedSender<OrchCmd>,
) -> bool {
    let input = std::mem::take(&mut state.input_buf);
    state.input_cursor = 0;
    if input.is_empty() { return false; }
    state.input_history.push(input.clone());
    state.history_idx = None;
    state.lines.push(MsgLine { text: format!("> {input}"), kind: MsgKind::Info, collapsed: false, cached_lines: None, cached_collapsed: false, sub_detail: None });
    if input.starts_with('/') {
        match input.as_str() {
            "/flash" => { let _ = orch_tx.send(OrchCmd::SetModel("flash".into())); state.model = "flash".into(); }
            "/pro" => { let _ = orch_tx.send(OrchCmd::SetModel("pro".into())); state.model = "pro".into(); }
            "/compact" => {
                let (done_tx, _) = tokio::sync::oneshot::channel();
                let _ = orch_tx.send(OrchCmd::Compact { done: done_tx });
            }
            "/help" => { state.add_help(); }
            "/skills" => { state.show_skills(); }
            "/exit" | "/quit" | "/q" => { state.quit = true; return true; }
            _ => {
                let (done_tx, _) = tokio::sync::oneshot::channel();
                let _ = orch_tx.send(OrchCmd::UserInput { input, done: done_tx });
            }
        }
    } else {
        let (done_tx, _) = tokio::sync::oneshot::channel();
        let _ = orch_tx.send(OrchCmd::UserInput { input, done: done_tx });
    }
    state.auto_scroll = true;
    false
}

fn handle_event(
    ev: Event,
    state: &mut TuiState,
    orch_tx: &tokio::sync::mpsc::UnboundedSender<OrchCmd>,
) -> bool {
    match ev {
        Event::Key(key) => return handle_key(key, state, orch_tx),
        Event::Mouse(mouse) => {
            match mouse.kind {
                MouseEventKind::ScrollUp => {
                    if let View::SubAgentDetail { scroll, .. } = &mut state.view {
                        *scroll = scroll.saturating_sub(3);
                    } else {
                        scroll_by(state, -(SCROLL_STEP as i16));
                    }
                }
                MouseEventKind::ScrollDown => {
                    if let View::SubAgentDetail { scroll, .. } = &mut state.view {
                        *scroll = scroll.saturating_add(3);
                    } else {
                        scroll_by(state, SCROLL_STEP as i16);
                    }
                }
                MouseEventKind::Down(_) => {
                    if state.click_map.is_empty() { return false; }
                    let abs_row = mouse.row.saturating_sub(state.content_y).saturating_sub(1) + state.effective_scroll;
                    let mut best: Option<(usize, u16)> = None;
                    for (idx, start, end) in &state.click_map {
                        let dist = if abs_row < *start {
                            *start - abs_row
                        } else if abs_row > *end {
                            abs_row - *end
                        } else {
                            0
                        };
                        if best.map_or(true, |(_, d)| dist < d) {
                            best = Some((*idx, dist));
                        }
                    }
                    if let Some((idx, _)) = best {
                        if let Some(msg) = state.lines.get_mut(idx) {
                            if matches!(msg.kind, MsgKind::StreamThinking) {
                                msg.collapsed = !msg.collapsed;
                            } else if msg.kind == MsgKind::SubAgent && msg.sub_detail.is_some() {
                                // 进入子代理详情视图（按索引引用，自动反映实时更新）
                                state.view = View::SubAgentDetail {
                                    line_idx: idx,
                                    scroll: 0,
                                };
                            }
                        }
                    }
                }
                _ => {}
            }
        }
        Event::Resize(..) => {}
        Event::Paste(content) => {
            // 批量过滤并插入，避免单字符 insert() 的 O(n²)
            let to_insert: String = content.chars()
                .filter(|&ch| !ch.is_control() || ch == '\n' || ch == '\t')
                .collect();
            if !to_insert.is_empty() {
                state.input_buf.insert_str(state.input_cursor, &to_insert);
                state.input_cursor += to_insert.len();
            }
        }
        _ => {}
    }
    false
}

// ─── Render ────────────────────────────────────────────────

fn render(f: &mut Frame, state: &mut TuiState) {
    let area = f.area();
    if area.height < 5 || area.width < 20 { return; }

    let view = state.view.clone();
    match &view {
        View::Main => {
            // 输入框内边宽度（去除 borders）
            let inner_w = area.width.saturating_sub(2).max(1) as usize;
            // 用 split_at_visual_width 直接计算实际行数，与渲染完全一致
            let vis_lines = split_at_visual_width(&state.input_buf, inner_w);
            let content_lines = vis_lines.len().min(5).max(1);
            let input_height = content_lines + 2; // +2 for borders

            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Min(1),
                    Constraint::Length(input_height as u16),
                    Constraint::Length(1),
                ])
                .split(area);

            state.content_y = chunks[0].y;
            render_content(f, chunks[0], state);
            render_input(f, chunks[1], state, &vis_lines);

            // 光标位置：用 split_at_visual_width 切分光标前的文本，
            // 行 = 行数-1, 列 = 最后一行视觉宽度
            let lines_before = split_at_visual_width(&state.input_buf[..state.input_cursor], inner_w);
            let row = lines_before.len().saturating_sub(1);
            let col = if lines_before.is_empty() {
                0
            } else {
                unicode_width::UnicodeWidthStr::width(lines_before.last().unwrap().as_str())
            };
            let cursor_x = (chunks[1].x + 1 + col as u16)
                .min(chunks[1].right().saturating_sub(2));
            let cursor_y = (chunks[1].y + 1 + row as u16)
                .min(chunks[1].bottom().saturating_sub(2));
            f.set_cursor_position((cursor_x, cursor_y));

            render_status(f, chunks[2], state);
        }
        View::SubAgentDetail { line_idx, scroll } => {
            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Min(1),
                    Constraint::Length(1),
                ])
                .split(area);
            render_detail_content(f, chunks[0], *line_idx, *scroll, state.show_borders, state);
            render_detail_bar(f, chunks[1]);
        }
    }
}

fn render_detail_content(
    f: &mut Frame,
    area: Rect,
    line_idx: usize,
    scroll: u16,
    show_borders: bool,
    state: &TuiState,
) {
    let (title, thinking, text) = match state.lines.get(line_idx) {
        Some(line) if line.kind == MsgKind::SubAgent => {
            let detail = line.sub_detail.as_ref();
            let thinking = detail.map(|d| d.thinking.as_str()).unwrap_or("");
            let text = detail.map(|d| d.text.as_str()).unwrap_or("");
            (line.text.as_str(), thinking, text)
        }
        _ => return,
    };

    let mut all_lines: Vec<Line<'static>> = Vec::new();

    // Title
    all_lines.push(Line::from(Span::styled(
        title.to_string(),
        Style::default().fg(Color::Magenta).add_modifier(Modifier::BOLD),
    )));
    all_lines.push(Line::from(""));

    // Thinking section
    if !thinking.is_empty() {
        all_lines.push(Line::from(Span::styled(
            "── Thinking ──",
            Style::default().fg(Color::Rgb(139, 139, 139)),
        )));
        for raw in thinking.split('\n') {
            all_lines.push(Line::from(Span::styled(
                raw.to_string(),
                Style::default().fg(Color::Rgb(139, 139, 139)),
            )));
        }
        all_lines.push(Line::from(""));
    }

    // Text section
    if !text.is_empty() {
        all_lines.push(Line::from(Span::styled(
            "── Text ──",
            Style::default().fg(Color::White),
        )));
        render_md_with_tables(&mut all_lines, text);
    }

    if all_lines.is_empty() {
        all_lines.push(Line::from(Span::styled(
            "(no output)",
            Style::default().fg(Color::DarkGray),
        )));
    }

    let viewport = area.height.saturating_sub(2) as usize;
    let max_scroll = all_lines.len().saturating_sub(viewport) as u16;
    let effective_scroll = scroll.min(max_scroll);

    let visible: Vec<Line<'static>> = all_lines
        .iter()
        .skip(effective_scroll as usize)
        .take(viewport)
        .cloned()
        .collect();

    let borders = if show_borders { Borders::ALL } else { Borders::NONE };
    let block = Block::default()
        .borders(borders)
        .border_style(Style::default().fg(Color::DarkGray));

    let paragraph = Paragraph::new(Text::from(visible)).block(block);
    f.render_widget(paragraph, area);
}

fn render_detail_bar(f: &mut Frame, area: Rect) {
    let text = Span::styled(
        " Esc: Back │ ↑↓ PgUp/PgDn: Scroll ",
        Style::default().fg(Color::Yellow),
    );
    f.render_widget(Paragraph::new(Line::from(text)), area);
}

fn render_content(f: &mut Frame, area: Rect, state: &mut TuiState) {
    let inner_w = area.width.saturating_sub(2).max(1);
    let width_changed = state.cached_width != inner_w;
    state.cached_width = inner_w;

    let mut need_rebuild = width_changed || state.cached_all.is_none();
    let collapsible = |k: MsgKind| matches!(k, MsgKind::StreamThinking);

    for msg in state.lines.iter_mut() {
        if width_changed || !msg.cache_valid() {
            let mut seg = Vec::new();
            if msg.collapsed {
                let first = msg.text.lines().next().unwrap_or("");
                let max_w = (inner_w as usize).saturating_sub(4).max(1);
                let mut snippet = String::new();
                let mut dw = 0usize;
                let mut cut = false;
                for ch in first.chars() {
                    let cw = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
                    if dw + cw > max_w { cut = true; break; }
                    snippet.push(ch);
                    dw += cw;
                }
                let suffix = if cut { "…" } else { "" };
                push_msg(&mut seg, &format!("► {snippet}{suffix}"), msg.kind);
            } else if collapsible(msg.kind) {
                push_msg(&mut seg, &format!("▼ {}", msg.text), msg.kind);
            } else {
                push_msg(&mut seg, &msg.text, msg.kind);
            }
            msg.cached_lines = Some(wrap_lines_word(&seg, inner_w));
            msg.cached_collapsed = msg.collapsed;
            need_rebuild = true;
        }
    }

    if need_rebuild {
        // 重建历史消息缓存（不包含 stream_line）
        let mut all_lines: Vec<Line<'static>> = Vec::new();
        state.click_map.clear();
        let mut current_row = 0u16;
        for (idx, msg) in state.lines.iter().enumerate() {
            let cached = msg.cached_lines.as_ref().unwrap();
            let phys = cached.len() as u16;
            state.click_map.push((idx, current_row, current_row + phys.saturating_sub(1)));
            current_row += phys;
            all_lines.extend(cached.clone());
        }
        state.cached_all = Some(all_lines);
    }

    // 每帧从缓存 + 当前 stream_line 构建完整显示列表
    // stream_line 不进入 cached_all，避免流式逐字符触发全量重建
    let mut display_lines = state.cached_all.as_ref().cloned().unwrap_or_default();
    if state.streaming && !state.stream_line.is_empty() {
        let mut seg: Vec<Line<'static>> = Vec::new();
        push_msg(&mut seg, &state.stream_line, state.stream_kind);
        display_lines.extend(wrap_lines_word(&seg, inner_w));
    }

    let viewport = area.height.saturating_sub(2) as usize;
    let max_scroll = display_lines.len().saturating_sub(viewport) as u16;
    state.max_scroll = max_scroll;

    let scroll = if state.auto_scroll {
        max_scroll
    } else {
        state.scroll.min(max_scroll)
    };
    state.effective_scroll = scroll;

    let border_color = if state.streaming { Color::Cyan } else { Color::DarkGray };
    let borders = if state.show_borders { Borders::ALL } else { Borders::NONE };
    let mut block = Block::default()
        .borders(borders)
        .border_style(Style::default().fg(border_color));

    let mut title_parts: Vec<Span<'static>> = Vec::new();
    if scroll > 0 {
        title_parts.push(Span::styled(" ↥ ", Style::default().fg(Color::Yellow)));
    }
    if scroll < max_scroll {
        title_parts.push(Span::styled(" ↧ ", Style::default().fg(Color::Yellow)));
    }
    if !title_parts.is_empty() {
        block = block.title_bottom(Line::from(title_parts));
    }

    let visible: Vec<Line<'static>> = display_lines
        .iter()
        .skip(scroll as usize)
        .take(viewport)
        .cloned()
        .collect();

    let paragraph = Paragraph::new(Text::from(visible))
        .block(block);

    f.render_widget(paragraph, area);
}

fn wrap_lines_word(lines: &[Line<'static>], max_w: u16) -> Vec<Line<'static>> {
    let mw = max_w.max(1) as usize;
    let mut out = Vec::new();
    for line in lines {
        let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        let style = line.spans.first().map(|s| s.style).unwrap_or_default();

        if text.is_empty() {
            out.push(Line::from(""));
            continue;
        }

        let mut cur = String::new();
        let mut cur_w = 0usize;

        for word in text.split_inclusive(' ') {
            let word_w = unicode_width::UnicodeWidthStr::width(word);
            if cur_w == 0 && word_w > mw {
                for ch in word.chars() {
                    let ch_w = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
                    if cur_w + ch_w > mw && !cur.is_empty() {
                        out.push(Line::from(Span::styled(std::mem::take(&mut cur), style)));
                        cur_w = 0;
                    }
                    cur.push(ch);
                    cur_w += ch_w;
                }
            } else if cur_w + word_w > mw {
                out.push(Line::from(Span::styled(std::mem::take(&mut cur), style)));
                let trimmed = word.trim_start_matches(' ');
                cur_w = unicode_width::UnicodeWidthStr::width(trimmed);
                cur.push_str(trimmed);
            } else {
                cur.push_str(word);
                cur_w += word_w;
            }
        }
        if !cur.is_empty() {
            out.push(Line::from(Span::styled(cur, style)));
        }
    }
    out
}

fn push_md(lines: &mut Vec<Line<'static>>, text: &str) {
    if text.is_empty() {
        return;
    }
    let md: Text<'_> = tui_markdown::from_str(text);
    for line in md.lines {
        let spans: Vec<Span<'static>> = line
            .spans
            .into_iter()
            .map(|s| {
                let mut style = s.style;
                if style.fg.is_none() {
                    style = style.fg(Color::Reset);
                }
                Span::styled(s.content.to_string(), style)
            })
            .collect();
        lines.push(Line::from(spans));
    }
}

fn push_msg(lines: &mut Vec<Line<'static>>, text: &str, kind: MsgKind) {
    if text.is_empty() {
        return;
    }
    match kind {
        MsgKind::Text | MsgKind::StreamText => {
            render_md_with_tables(lines, text);
        }
        MsgKind::ToolResult => {
            // 检查是否为 unified diff（包含 ---/+++ 或 @@ 行首）
            let is_diff = text.lines().take(3).any(|l| {
                let t = l.trim();
                t.starts_with("--- ") || t.starts_with("+++ ") || t.starts_with("@@")
            });
            if is_diff {
                render_diff(lines, text);
            } else {
                let base = style_for_kind(kind);
                for raw in text.split('\n') {
                    lines.push(Line::from(Span::styled(raw.to_string(), base)));
                }
            }
        }
        _ => {
            let base = style_for_kind(kind);
            for raw in text.split('\n') {
                lines.push(Line::from(Span::styled(raw.to_string(), base)));
            }
        }
    }
}

/// 渲染 unified diff：---/+++ 黄色，- 行红色，+ 行绿色，@@ 青色
/// 自动剥离内容中的 ANSI 转义码（Edit 工具输出包含颜色代码）。
fn render_diff(lines: &mut Vec<Line<'static>>, text: &str) {
    let gray = Style::default().fg(Color::Rgb(100, 100, 100));
    let red = Style::default().fg(Color::Rgb(255, 100, 100));
    let green = Style::default().fg(Color::Rgb(100, 200, 100));
    let cyan = Style::default().fg(Color::Cyan);
    let yellow = Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD);

    for raw in text.split('\n') {
        let clean = strip_ansi(raw);
        let trimmed = clean.trim();
        let style = if trimmed.starts_with("--- ") || trimmed.starts_with("+++ ") {
            yellow
        } else if trimmed.starts_with("@@") {
            cyan
        } else if clean.starts_with('-') && !clean.starts_with("---") {
            red
        } else if clean.starts_with('+') && !clean.starts_with("+++") {
            green
        } else {
            gray
        };
        lines.push(Line::from(Span::styled(clean, style)));
    }
}

/// 剥离 ANSI 转义序列（如 `\x1b[31m`、`\x1b[0m` 等）。
fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_escape = false;
    for ch in s.chars() {
        if in_escape {
            if ch == 'm' {
                in_escape = false;
            }
        } else if ch == '\x1b' {
            in_escape = true;
        } else {
            out.push(ch);
        }
    }
    out
}

/// Render markdown with manual table handling: extract table rows and render them raw
/// (bypass tui_markdown which doesn't support tables well).
fn render_md_with_tables(lines: &mut Vec<Line<'static>>, text: &str) {
    if text.contains("|---") || text.contains("| --") {
        let mut md_buf = String::new();
        for raw in text.split('\n') {
            let trimmed = raw.trim();
            let is_table = trimmed.starts_with('|') || trimmed.contains("---");
            if is_table {
                if !md_buf.is_empty() {
                    push_md(lines, &md_buf);
                    md_buf.clear();
                }
                // 表行追加空行，防止 tui_markdown 将相邻内容误解析为表格
                lines.push(Line::from(Span::raw(format!("{raw}\n"))));
            } else {
                if !md_buf.is_empty() {
                    md_buf.push('\n');
                }
                md_buf.push_str(raw);
            }
        }
        if !md_buf.is_empty() {
            push_md(lines, &md_buf);
        }
    } else {
        push_md(lines, text);
    }
}

fn render_input(f: &mut Frame, area: Rect, _state: &TuiState, vis_lines: &[String]) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::DarkGray));

    let inner = block.inner(area);
    f.render_widget(block, area);

    // 用预先计算好的 vis_lines 直接渲染，避免重复 split
    let text = Text::from(vis_lines.join("\n"));
    f.render_widget(Paragraph::new(text), inner);
}

/// 按视觉宽度切分字符串，CJK 字符宽度 2，ASCII 宽度 1。
/// 与光标计算中的 `visual_width / inner_w` 完全一致。
fn split_at_visual_width(s: &str, max_width: usize) -> Vec<String> {
    if s.is_empty() {
        return vec![String::new()];
    }
    let mut lines: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut cur_w = 0usize;
    for ch in s.chars() {
        let cw = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
        if cur_w + cw > max_width && !cur.is_empty() {
            lines.push(cur);
            cur = String::new();
            cur_w = 0;
        }
        cur.push(ch);
        cur_w += cw;
    }
    if !cur.is_empty() {
        lines.push(cur);
    }
    lines
}

fn render_status(f: &mut Frame, area: Rect, state: &TuiState) {
    let s = &state.stats;
    let b = if s.belief > 0.0 { format!(" B:{:.2}", s.belief) } else { String::new() };
    let ti = s.total_input_tokens + s.total_cache_read_tokens;
    let work = if state.streaming {
        match state.stream_kind {
            MsgKind::StreamThinking => "thinking",
            MsgKind::StreamText => "generating",
            _ => "working",
        }
    } else if state.busy {
        "waiting"
    } else {
        "idle"
    };
    let status = format!(
        " {}{b} T:{} R:{} I:{}({}) O:{} C:{}({}) {} [{}]",
        state.model,
        StatsSnapshot::fmt_num(s.current_turn_count),
        StatsSnapshot::fmt_num(s.agent_request_count),
        fmt_k(ti), s.cache_pct(),
        fmt_k(s.total_output_tokens),
        fmt_k(s.current_context_tokens), s.ctx_pct(),
        s.format_cost(),
        work,
    );
    let line = Line::from(Span::styled(status, Style::default().fg(Color::Cyan)));
    f.render_widget(Paragraph::new(line), area);
}

fn build_replay_tool_summary(name: &str, evt: &serde_json::Value) -> String {
    crate::session::store::build_tool_summary_from_json(name, evt)
}

// ─── TuiDisplay (agent side) ───────────────────────────────

pub struct TuiDisplay {
    tx: mpsc::Sender<TuiSignal>,
}

impl TuiDisplay {
    pub fn new(tx: mpsc::Sender<TuiSignal>) -> Self { Self { tx } }
}

impl Display for TuiDisplay {
    fn render_thinking(&self, c: &str) { let _ = self.tx.send(TuiSignal::Thinking(c.into())); }
    fn render_text(&self, c: &str) { let _ = self.tx.send(TuiSignal::Text(c.into())); }
    fn render_tool_call(&self, n: &str, s: &str) { let _ = self.tx.send(TuiSignal::ToolCall(n.into(), s.into())); }
    fn render_tool_result(&self, _: &str, c: &str) { let _ = self.tx.send(TuiSignal::ToolResult(c.into())); }
    fn render_stop(&self) { let _ = self.tx.send(TuiSignal::Stop); }
    fn render_error(&self, m: &str) { let _ = self.tx.send(TuiSignal::Error(m.into())); }
    fn render_retry(&self) { let _ = self.tx.send(TuiSignal::Retry); }
    fn render_info(&self, m: &str) { let _ = self.tx.send(TuiSignal::Info(m.into())); }
    fn render_title_update(&self, m: &str, s: &StatsSnapshot) {
        let _ = self.tx.send(TuiSignal::TitleUpdate(m.into(), s.clone()));
    }
    fn render_sub_agent_status(&self, sid: &str, st: &str, it: u64, ot: u64) {
        let l = if st == "ok" || st == "launched" || st == "running" {
            format!("[sub-agent {}] {} (in={}, out={})", sid, st, it, ot)
        } else {
            format!("[sub-agent {}] failed", sid)
        };
        let _ = self.tx.send(TuiSignal::SubAgentStatus(l));
    }
    fn render_sub_agent_output(&self, sid: &str, st: &str, thinking: &str, text: &str, it: u64, ot: u64) {
        let _ = self.tx.send(TuiSignal::SubAgentOutput {
            session_id: sid.into(),
            status: st.into(),
            thinking: thinking.into(),
            text: text.into(),
            in_tokens: it,
            out_tokens: ot,
        });
    }
    fn render_prompt(&self) {}
    fn render_clear_line(&self) {}
}

// ─── Utilities ─────────────────────────────────────────────

