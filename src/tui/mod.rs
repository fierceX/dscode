//! TUI module for dscode using ratatui.

use crate::agent::orchestrator::OrchCmd;
use crate::ui::{Display, StatsSnapshot};
use crate::util::truncate_str;
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
use std::sync::mpsc;
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

// ─── State ─────────────────────────────────────────────────

#[derive(Clone)]
pub(crate) struct MsgLine {
    pub text: String,
    pub kind: MsgKind,
    pub collapsed: bool,
    pub cached_lines: Option<Vec<Line<'static>>>,
    pub cached_collapsed: bool,
}

impl MsgLine {
    fn new(text: String, kind: MsgKind, collapsed: bool) -> Self {
        MsgLine { text, kind, collapsed, cached_lines: None, cached_collapsed: collapsed }
    }
    fn cache_valid(&self) -> bool {
        self.cached_lines.is_some() && self.cached_collapsed == self.collapsed
    }
}

impl Default for MsgLine {
    fn default() -> Self {
        MsgLine { text: String::new(), kind: MsgKind::Text, collapsed: false, cached_lines: None, cached_collapsed: false }
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
            }
            TuiSignal::Text(c) => {
                if !self.stream_line.is_empty() && self.stream_kind != MsgKind::StreamText {
                    self.stream_line.push('\n');
                    self.save_stream();
                }
                self.stream_kind = MsgKind::StreamText;
                self.streaming = true;
                self.stream_line.push_str(c);
            }
            TuiSignal::Stop | TuiSignal::Retry => {
                self.finalize_stream();
            }
            TuiSignal::ToolCall(name, summary) => {
                self.finalize_stream();
                let text = if summary.is_empty() { format!("[tool] {name}") } else { format!("[tool] {summary}") };
                self.lines.push(MsgLine {
                    text,
                    kind: MsgKind::ToolCall,
                    collapsed: false,
                    cached_lines: None,
                    cached_collapsed: false,
                });
            }
            TuiSignal::Info(n) => {
                self.finalize_stream();
                self.lines.push(MsgLine { text: n.clone(), kind: MsgKind::Info, collapsed: false, cached_lines: None, cached_collapsed: false });
            }
            TuiSignal::ToolResult(_c) => {
                self.finalize_stream();
            }
            TuiSignal::Error(m) => {
                self.finalize_stream();
                self.lines.push(MsgLine {
                    text: format!("Error: {m}"),
                    kind: MsgKind::Error,
                    collapsed: false,
                    cached_lines: None,
                    cached_collapsed: false,
                });
            }
            TuiSignal::TitleUpdate(m, s) => { self.model = m.clone(); self.stats = s.clone(); }
            TuiSignal::SubAgentStatus(l) => {
                self.lines.push(MsgLine { text: l.clone(), kind: MsgKind::SubAgent, collapsed: false, cached_lines: None, cached_collapsed: false });
            }
            TuiSignal::Shutdown => {}
        }
    }

    fn add_help(&mut self) {
        self.lines.push(MsgLine { text: "Commands:".into(), kind: MsgKind::Info, collapsed: false, cached_lines: None, cached_collapsed: false });
        self.lines.push(MsgLine { text: "  /flash          Switch to flash tier".into(), kind: MsgKind::Text, collapsed: false, cached_lines: None, cached_collapsed: false });
        self.lines.push(MsgLine { text: "  /pro            Switch to pro tier".into(), kind: MsgKind::Text, collapsed: false, cached_lines: None, cached_collapsed: false });
        self.lines.push(MsgLine { text: "  /compact        Force context compaction".into(), kind: MsgKind::Text, collapsed: false, cached_lines: None, cached_collapsed: false });
        self.lines.push(MsgLine { text: "  /skills         List available skills".into(), kind: MsgKind::Text, collapsed: false, cached_lines: None, cached_collapsed: false });
        self.lines.push(MsgLine { text: "  /exit  /quit    Exit TUI".into(), kind: MsgKind::Text, collapsed: false, cached_lines: None, cached_collapsed: false });
    }

    fn show_skills(&mut self) {
        self.lines.push(MsgLine { text: "=== Built-in Skills ===".into(), kind: MsgKind::Info, collapsed: false, cached_lines: None, cached_collapsed: false });
        for skill in crate::assets::embedded_skills::all() {
            self.lines.push(MsgLine { text: format!("  {} — {}", skill.name, skill.description), kind: MsgKind::Text, collapsed: false, cached_lines: None, cached_collapsed: false });
        }
        self.lines.push(MsgLine {
            text: "Use --skill NAME or Skill(name) to load.".into(),
            kind: MsgKind::Info,
            collapsed: false,
            cached_lines: None,
            cached_collapsed: false,
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
        MsgKind::ToolResult => Style::default(),
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

    let mut flush_buf = |lines: &mut Vec<MsgLine>, buf: &mut String, kind: &mut Option<MsgKind>| {
        if !buf.is_empty() {
            let k = kind.take().unwrap_or(MsgKind::Text);
            lines.push(MsgLine { text: std::mem::take(buf), kind: k, collapsed: k == MsgKind::StreamThinking, cached_lines: None, cached_collapsed: k == MsgKind::StreamThinking });
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
                    lines.push(MsgLine { text: format!("> {preview}"), kind: MsgKind::Info, collapsed: false, cached_lines: None, cached_collapsed: false });
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
                lines.push(MsgLine { text, kind: MsgKind::ToolCall, collapsed: false, cached_lines: None, cached_collapsed: false });
            }
            "tool_result" => {
                flush_buf(&mut lines, &mut buf, &mut buf_kind);
            }
            "error" => {
                flush_buf(&mut lines, &mut buf, &mut buf_kind);
                let msg = evt.get("message").and_then(serde_json::Value::as_str).unwrap_or("");
                lines.push(MsgLine { text: format!("Error: {msg}"), kind: MsgKind::Error, collapsed: false, cached_lines: None, cached_collapsed: false });
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
) -> anyhow::Result<()> {
    let mut terminal = ratatui::init();
    crossterm::execute!(std::io::stdout(), crossterm::event::EnableMouseCapture)?;
    struct RestoreGuard;
    impl Drop for RestoreGuard {
        fn drop(&mut self) {
            let _ = crossterm::execute!(std::io::stdout(), crossterm::event::DisableMouseCapture);
            ratatui::restore();
        }
    }
    let _guard = RestoreGuard;
    tui_main_loop(&mut terminal, sig_rx, orch_tx, events_path)
}

fn tui_main_loop(
    terminal: &mut ratatui::DefaultTerminal,
    sig_rx: mpsc::Receiver<TuiSignal>,
    orch_tx: tokio::sync::mpsc::UnboundedSender<OrchCmd>,
    events_path: &Path,
) -> anyhow::Result<()> {
    let mut state = TuiState::default();
    state.lines = load_session(events_path);
    let mut sig_rx = sig_rx;

    loop {
        if state.dirty {
            terminal.draw(|f| render(f, &mut state))?;
            state.dirty = false;
        }

        if drain_signals(&mut sig_rx, &mut state) {
            state.dirty = true;
        }
        if state.quit {
            break;
        }

        if crossterm::event::poll(Duration::from_millis(100)).unwrap_or(false) {
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
    match (key.modifiers, key.code) {
        (KeyModifiers::CONTROL, KeyCode::Char('c')) => { state.quit = true; return true; }
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
    state.lines.push(MsgLine { text: format!("> {input}"), kind: MsgKind::Info, collapsed: false, cached_lines: None, cached_collapsed: false });
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
                MouseEventKind::ScrollUp => scroll_by(state, -(SCROLL_STEP as i16)),
                MouseEventKind::ScrollDown => scroll_by(state, SCROLL_STEP as i16),
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
                            }
                        }
                    }
                }
                _ => {}
            }
        }
        Event::Resize(..) => {}
        _ => {}
    }
    false
}

// ─── Render ────────────────────────────────────────────────

fn render(f: &mut Frame, state: &mut TuiState) {
    let area = f.area();
    if area.height < 5 || area.width < 20 { return; }

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(1),
            Constraint::Length(3),
            Constraint::Length(1),
        ])
        .split(area);

    state.content_y = chunks[0].y;
    render_content(f, chunks[0], state);
    render_input(f, chunks[1], state);

    let cursor_x = (chunks[1].x + 3 + unicode_width::UnicodeWidthStr::width(&state.input_buf[..state.input_cursor]) as u16)
        .min(chunks[1].right().saturating_sub(2));
    let cursor_y = chunks[1].y + 1;
    f.set_cursor_position((cursor_x, cursor_y));

    render_status(f, chunks[2], state);
}

fn render_content(f: &mut Frame, area: Rect, state: &mut TuiState) {
    let inner_w = area.width.saturating_sub(2).max(1);
    let width_changed = state.cached_width != inner_w;
    state.cached_width = inner_w;

    let mut need_rebuild = width_changed || state.cached_all.is_none() || state.streaming;
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
        if state.streaming && !state.stream_line.is_empty() {
            let mut seg: Vec<Line<'static>> = Vec::new();
            push_msg(&mut seg, &state.stream_line, state.stream_kind);
            all_lines.extend(wrap_lines_word(&seg, inner_w));
        }
        state.cached_all = Some(all_lines);
    }

    let all_wrapped = state.cached_all.as_ref().unwrap();
    let viewport = area.height.saturating_sub(2) as usize;
    let max_scroll = all_wrapped.len().saturating_sub(viewport) as u16;
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

    let visible: Vec<Line<'static>> = all_wrapped
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
                        lines.push(Line::from(Span::raw(raw.to_string())));
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
        _ => {
            let base = style_for_kind(kind);
            for raw in text.split('\n') {
                lines.push(Line::from(Span::styled(raw.to_string(), base)));
            }
        }
    }
}

fn render_input(f: &mut Frame, area: Rect, state: &TuiState) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::DarkGray));

    let inner = block.inner(area);
    f.render_widget(block, area);

    let prompt = Span::styled("> ", Style::default().fg(Color::Green).add_modifier(Modifier::BOLD));
    let input = Span::raw(&state.input_buf);
    let line = Line::from(vec![prompt, input]);
    f.render_widget(Paragraph::new(line), inner);
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
    use std::collections::BTreeMap;
    let input = evt.get("input").cloned().unwrap_or(serde_json::Value::Null);
    let mut fields = BTreeMap::new();
    if let Some(obj) = input.as_object() {
        for (k, v) in obj {
            match v {
                serde_json::Value::String(s) => { fields.insert(k.clone(), s.clone()); }
                _ => { fields.insert(k.clone(), v.to_string()); }
            }
        }
    }
    crate::session::store::build_tool_call_summary(name, &fields)
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
    fn render_prompt(&self) {}
    fn render_clear_line(&self) {}
}

// ─── Utilities ─────────────────────────────────────────────

fn fmt_k(n: u64) -> String {
    if n < 1000 { return n.to_string(); }
    if n >= 1_000_000 {
        format!("{}.{:02}M", n / 1_000_000, (n % 1_000_000) / 10_000)
    } else {
        format!("{}.{}K", n / 1000, (n % 1000) / 100)
    }
}
