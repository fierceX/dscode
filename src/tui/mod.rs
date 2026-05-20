//! TUI module for dscode using ratatui.

use crate::agent::orchestrator::OrchCmd;
use crate::ui::{Display, StatsSnapshot};
use crate::util::truncate_str;
use crossterm::event::{Event, KeyCode, KeyModifiers, MouseEventKind};
use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, Borders, Paragraph, Wrap},
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
    ToolCall(String),
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

#[derive(Clone, Default)]
pub(crate) struct MsgLine {
    text: String,
    kind: MsgKind,
}

#[derive(Clone)]
pub(crate) struct TuiState {
    pub lines: Vec<MsgLine>,
    pub stream_line: String,
    pub stream_kind: MsgKind,
    pub streaming: bool,
    pub input_buf: String,
    pub input_history: Vec<String>,
    pub history_idx: Option<usize>,
    pub model: String,
    pub stats: StatsSnapshot,
    pub scroll: u16,
    pub auto_scroll: bool,
    pub max_scroll: u16,
    pub show_borders: bool,
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
            input_history: Vec::new(),
            history_idx: None,
            model: String::new(),
            stats: StatsSnapshot::default(),
            scroll: 0,
            auto_scroll: true,
            max_scroll: 0,
            show_borders: true,
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
            self.lines.push(MsgLine { text, kind: self.stream_kind });
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
            TuiSignal::ToolCall(n) | TuiSignal::Info(n) => {
                self.finalize_stream();
                self.lines.push(MsgLine { text: n.clone(), kind: MsgKind::ToolCall });
            }
            TuiSignal::ToolResult(c) => {
                self.finalize_stream();
                for l in c.split('\n') {
                    if !l.is_empty() {
                        self.lines.push(MsgLine { text: l.to_string(), kind: MsgKind::ToolResult });
                    }
                }
            }
            TuiSignal::Error(m) => {
                self.finalize_stream();
                self.lines.push(MsgLine { text: format!("Error: {m}"), kind: MsgKind::Error });
            }
            TuiSignal::TitleUpdate(m, s) => { self.model = m.clone(); self.stats = s.clone(); }
            TuiSignal::SubAgentStatus(l) => {
                self.lines.push(MsgLine { text: l.clone(), kind: MsgKind::SubAgent });
            }
            TuiSignal::Shutdown => {}
        }
    }

    fn add_help(&mut self) {
        self.lines.push(MsgLine { text: "Commands:".into(), kind: MsgKind::Info });
        self.lines.push(MsgLine { text: "  /flash          Switch to flash tier".into(), kind: MsgKind::Text });
        self.lines.push(MsgLine { text: "  /pro            Switch to pro tier".into(), kind: MsgKind::Text });
        self.lines.push(MsgLine { text: "  /compact        Force context compaction".into(), kind: MsgKind::Text });
        self.lines.push(MsgLine { text: "  /skills         List available skills".into(), kind: MsgKind::Text });
        self.lines.push(MsgLine { text: "  /exit  /quit    Exit TUI".into(), kind: MsgKind::Text });
    }

    fn show_skills(&mut self) {
        self.lines.push(MsgLine { text: "=== Built-in Skills ===".into(), kind: MsgKind::Info });
        for skill in crate::assets::embedded_skills::all() {
            self.lines.push(MsgLine { text: format!("  {} — {}", skill.name, skill.description), kind: MsgKind::Text });
        }
        self.lines.push(MsgLine {
            text: "Use --skill NAME or Skill(name) to load.".into(),
            kind: MsgKind::Info,
        });
    }
}

// ─── Section styling ───────────────────────────────────────

fn style_for_kind(kind: MsgKind) -> Style {
    match kind {
        MsgKind::StreamThinking => Style::default().fg(Color::Gray).add_modifier(Modifier::DIM),
        MsgKind::Text | MsgKind::StreamText => Style::default(),
        MsgKind::ToolCall => Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
        MsgKind::ToolResult => Style::default().fg(Color::Green),
        MsgKind::Error => Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        MsgKind::Info => Style::default().fg(Color::Yellow),
        MsgKind::SubAgent => Style::default().fg(Color::Magenta),
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
            lines.push(MsgLine { text: std::mem::take(buf), kind: kind.take().unwrap_or(MsgKind::Text) });
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
                    lines.push(MsgLine { text: format!("> {preview}"), kind: MsgKind::Info });
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
                lines.push(MsgLine { text: format!("[tool] {name}"), kind: MsgKind::ToolCall });
            }
            "tool_result" => {
                flush_buf(&mut lines, &mut buf, &mut buf_kind);
                let preview = truncate_str(c, 200);
                if !preview.is_empty() {
                    lines.push(MsgLine { text: preview.to_string(), kind: MsgKind::ToolResult });
                }
            }
            "error" => {
                flush_buf(&mut lines, &mut buf, &mut buf_kind);
                let msg = evt.get("message").and_then(serde_json::Value::as_str).unwrap_or("");
                lines.push(MsgLine { text: format!("Error: {msg}"), kind: MsgKind::Error });
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
    crossterm::terminal::enable_raw_mode()?;
    let mut terminal = ratatui::init();
    let result = tui_main_loop(&mut terminal, sig_rx, orch_tx, events_path);
    ratatui::restore();
    result
}

fn tui_main_loop(
    terminal: &mut ratatui::DefaultTerminal,
    sig_rx: mpsc::Receiver<TuiSignal>,
    orch_tx: tokio::sync::mpsc::UnboundedSender<OrchCmd>,
    events_path: &Path,
) -> anyhow::Result<()> {
    let mut state = TuiState::default();
    state.lines = load_session(events_path);
    let sig_rx = std::cell::RefCell::new(sig_rx);

    loop {
        terminal.draw(|f| render(f, &mut state))?;

        if state.quit || drain_signals(&sig_rx, &mut state) {
            terminal.draw(|f| render(f, &mut state))?;
        }
        if state.quit {
            break;
        }

        if crossterm::event::poll(Duration::from_millis(50))? {
            if handle_event(crossterm::event::read()?, &mut state, &orch_tx) {
                break;
            }
        }
    }
    Ok(())
}

fn drain_signals(rx: &std::cell::RefCell<mpsc::Receiver<TuiSignal>>, state: &mut TuiState) -> bool {
    let rx = rx.borrow_mut();
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

fn apply_scroll(state: &mut TuiState, delta: i32) {
    let base = if state.auto_scroll { state.max_scroll } else { state.scroll };
    state.auto_scroll = false;
    let new = (base as i32 + delta).clamp(0, state.max_scroll as i32);
    state.scroll = new as u16;
}

fn handle_event(
    ev: Event,
    state: &mut TuiState,
    orch_tx: &tokio::sync::mpsc::UnboundedSender<OrchCmd>,
) -> bool {
    match ev {
        Event::Key(key) => {
            if key.kind == crossterm::event::KeyEventKind::Release {
                return false;
            }
            match (key.modifiers, key.code) {
                (KeyModifiers::CONTROL, KeyCode::Char('c')) => { state.quit = true; return true; }
                (KeyModifiers::CONTROL, KeyCode::Char('t')) => { state.show_borders = !state.show_borders; }
                (KeyModifiers::NONE, KeyCode::Esc) => { state.quit = true; return true; }
                (KeyModifiers::NONE, KeyCode::Char(c)) => {
                    if !c.is_control() {
                        state.input_buf.push(c);
                    }
                }
                (KeyModifiers::NONE, KeyCode::Enter) => {
                    let input = std::mem::take(&mut state.input_buf);
                    if input.is_empty() {
                        return false;
                    }
                    state.input_history.push(input.clone());
                    state.history_idx = None;
                    state.lines.push(MsgLine { text: format!("> {input}"), kind: MsgKind::Info });
                    if input.starts_with('/') {
                        match input.as_str() {
                            "/flash" => { let _ = orch_tx.send(OrchCmd::SetModel("flash".into())); }
                            "/pro" => { let _ = orch_tx.send(OrchCmd::SetModel("pro".into())); }
                            "/compact" => {
                                let (done_tx, _) = tokio::sync::oneshot::channel();
                                let _ = orch_tx.send(OrchCmd::Compact { done: done_tx });
                            }
                            "/help" => { state.add_help(); }
                            "/skills" => { state.show_skills(); }
                            "/exit" | "/quit" | "/q" => { state.quit = true; return true; }
                            _ => {
                                let (done_tx, _) = tokio::sync::oneshot::channel();
                                let _ = orch_tx.send(OrchCmd::UserInput {
                                    input, done: done_tx,
                                });
                            }
                        }
                    } else {
                        let (done_tx, _) = tokio::sync::oneshot::channel();
                        let _ = orch_tx.send(OrchCmd::UserInput { input, done: done_tx });
                    }
                    state.auto_scroll = true;
                }
                (KeyModifiers::NONE, KeyCode::Backspace) => {
                    state.input_buf.pop();
                }
                (KeyModifiers::NONE, KeyCode::PageUp) => { apply_scroll(state, -(SCROLL_STEP as i32 * 5)); }
                (KeyModifiers::NONE, KeyCode::PageDown) => { apply_scroll(state, SCROLL_STEP as i32 * 5); }
                (KeyModifiers::NONE, KeyCode::Up) => {
                    if !state.input_history.is_empty() && state.input_buf.is_empty() {
                        let idx = match state.history_idx {
                            None => state.input_history.len().saturating_sub(1),
                            Some(i) => i.saturating_sub(1),
                        };
                        state.input_buf = state.input_history[idx].clone();
                        state.history_idx = Some(idx);
                    } else {
                        apply_scroll(state, -(SCROLL_STEP as i32));
                    }
                }
                (KeyModifiers::NONE, KeyCode::Down) => {
                    if state.history_idx.is_some() {
                        let idx = state.history_idx.unwrap() + 1;
                        if idx >= state.input_history.len() {
                            state.input_buf.clear();
                            state.history_idx = None;
                        } else {
                            state.input_buf = state.input_history[idx].clone();
                            state.history_idx = Some(idx);
                        }
                    } else {
                        apply_scroll(state, SCROLL_STEP as i32);
                    }
                }
                _ => {}
            }
        }
        Event::Mouse(mouse) => {
            match mouse.kind {
                MouseEventKind::ScrollUp => apply_scroll(state, SCROLL_STEP as i32),
                MouseEventKind::ScrollDown => apply_scroll(state, -(SCROLL_STEP as i32)),
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

    render_content(f, chunks[0], state);
    render_input(f, chunks[1], state);

    let cursor_x = (chunks[1].x + 3 + unicode_width::UnicodeWidthStr::width(state.input_buf.as_str()) as u16)
        .min(chunks[1].right().saturating_sub(2));
    let cursor_y = chunks[1].y + 1;
    f.set_cursor_position((cursor_x, cursor_y));

    render_status(f, chunks[2], state);
}

fn render_content(f: &mut Frame, area: Rect, state: &mut TuiState) {
    let mut all_lines: Vec<Line<'static>> = Vec::new();

    for msg in &state.lines {
        push_msg(&mut all_lines, &msg.text, msg.kind);
    }
    if state.streaming && !state.stream_line.is_empty() {
        push_msg(&mut all_lines, &state.stream_line, state.stream_kind);
    }

    let inner_w = area.width.saturating_sub(2).max(1);
    let viewport = area.height.saturating_sub(2) as usize;
    let phys_est = est_physical_lines(&all_lines, inner_w);
    let max_scroll = phys_est.saturating_sub(viewport).min(u16::MAX as usize) as u16;
    state.max_scroll = max_scroll;

    let scroll = if state.auto_scroll {
        max_scroll
    } else {
        (state.scroll as i32).clamp(0, max_scroll as i32) as u16
    };

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

    let paragraph = Paragraph::new(Text::from(all_lines))
        .block(block)
        .wrap(Wrap { trim: false })
        .scroll((scroll, 0));

    f.render_widget(paragraph, area);
}

fn est_physical_lines(lines: &[Line<'static>], max_w: u16) -> usize {
    let mw = max_w.max(1) as usize;
    let mut total = 0usize;
    for line in lines {
        let mut row_w = 0usize;
        total += 1;
        for span in &line.spans {
            for ch in span.content.chars() {
                let ch_w = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
                if row_w + ch_w > mw {
                    total += 1;
                    row_w = 0;
                }
                row_w += ch_w;
            }
        }
    }
    total
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
        " {}{b} [{}] T:{} R:{} I:{}({}) O:{} C:{}({}) {}",
        state.model, work,
        StatsSnapshot::fmt_num(s.current_turn_count),
        StatsSnapshot::fmt_num(s.agent_request_count),
        fmt_k(ti), s.cache_pct(),
        fmt_k(s.total_output_tokens),
        fmt_k(s.current_context_tokens), s.ctx_pct(),
        s.format_cost(),
    );
    let line = Line::from(Span::styled(status, Style::default().fg(Color::Cyan)));
    f.render_widget(Paragraph::new(line), area);
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
    fn render_tool_call(&self, n: &str, _: &str) { let _ = self.tx.send(TuiSignal::ToolCall(n.into())); }
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
