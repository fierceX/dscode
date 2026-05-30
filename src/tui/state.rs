use crate::ui::StatsSnapshot;
use ratatui::text::Line;
use std::collections::{HashMap, HashSet};
use std::fmt::{Debug, Display as FmtDisplay, Formatter};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Instant;

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub(crate) enum MsgKind {
    #[default]
    Text,
    ToolCall,
    ToolResult,
    Error,
    Info,
    SubAgent,
    StreamThinking,
    StreamText,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub(crate) enum WorkState {
    #[default]
    Idle,
    WaitingModel,
    StreamingThinking,
    StreamingText,
    RunningTool,
    RunningSubAgent,
    Compacting,
    Error,
}

impl WorkState {
    pub(crate) fn is_working(self) -> bool {
        !matches!(self, WorkState::Idle | WorkState::Error)
    }

    pub(crate) fn label(self) -> &'static str {
        match self {
            WorkState::Idle => "idle",
            WorkState::WaitingModel => "waiting",
            WorkState::StreamingThinking => "thinking",
            WorkState::StreamingText => "generating",
            WorkState::RunningTool => "tool",
            WorkState::RunningSubAgent => "sub-agent",
            WorkState::Compacting => "compacting",
            WorkState::Error => "error",
        }
    }
}

#[derive(Clone)]
pub(crate) struct SubAgentDetail {
    pub thinking: String,
    pub text: String,
}

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
    pub(crate) fn cache_valid(&self) -> bool {
        self.cached_lines.is_some() && self.cached_collapsed == self.collapsed
    }

    pub(crate) fn new(text: String, kind: MsgKind) -> Self {
        let collapsed = kind == MsgKind::StreamThinking;
        MsgLine {
            text,
            kind,
            collapsed,
            cached_lines: None,
            cached_collapsed: collapsed,
            sub_detail: None,
        }
    }

    pub(crate) fn with_sub_detail(mut self, sub_detail: Option<SubAgentDetail>) -> Self {
        self.sub_detail = sub_detail;
        self
    }

    pub(crate) fn invalidate_cache(&mut self) {
        self.cached_lines = None;
    }
}

impl Default for MsgLine {
    fn default() -> Self {
        MsgLine {
            text: String::new(),
            kind: MsgKind::Text,
            collapsed: false,
            cached_lines: None,
            cached_collapsed: false,
            sub_detail: None,
        }
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
    pub input_scroll_row: usize,
    pub input_history: Vec<String>,
    pub history_idx: Option<usize>,
    pub model: String,
    pub stats: StatsSnapshot,
    pub scroll: usize,
    pub auto_scroll: bool,
    pub max_scroll: usize,
    pub show_borders: bool,
    pub click_map: Vec<(usize, usize, usize)>,
    pub content_y: u16,
    pub effective_scroll: usize,
    pub dirty: bool,
    pub cached_width: u16,
    pub cached_all: Option<Vec<Line<'static>>>,
    pub stream_revision: u64,
    pub cached_stream_width: u16,
    pub cached_stream_kind: MsgKind,
    pub cached_stream_revision: u64,
    pub cached_stream_lines: Option<Vec<Line<'static>>>,
    pub quit: bool,
    pub last_interrupt: Option<Instant>,
    pub work_state: WorkState,
    pub active_sub_agent_sessions: HashSet<String>,
    pub sub_agent_lines: HashMap<String, usize>,
    /// 中断当前任务（由 Ctrl+C 触发），None 表示无中断能力
    pub interrupt: Option<Arc<AtomicBool>>,
    pub view: View,
}

#[derive(Clone, Debug, Default)]
pub(crate) enum View {
    #[default]
    Main,
    SubAgentDetail {
        line_idx: usize,
        scroll: usize,
    },
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
            input_scroll_row: 0,
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
            stream_revision: 0,
            cached_stream_width: 0,
            cached_stream_kind: MsgKind::default(),
            cached_stream_revision: 0,
            cached_stream_lines: None,
            quit: false,
            last_interrupt: None,
            work_state: WorkState::Idle,
            active_sub_agent_sessions: HashSet::new(),
            sub_agent_lines: HashMap::new(),
            interrupt: None,
            view: View::Main,
        }
    }
}

impl Debug for TuiState {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "TuiState[lines={}, stream={}, input={}]",
            self.lines.len(),
            self.stream_line.len(),
            self.input_buf.len()
        )
    }
}

impl FmtDisplay for TuiState {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        Debug::fmt(self, f)
    }
}

impl TuiState {
    pub(crate) fn invalidate_all_cache(&mut self) {
        self.cached_all = None;
    }

    pub(crate) fn invalidate_stream_cache(&mut self) {
        self.cached_stream_lines = None;
    }

    pub(crate) fn push_line(&mut self, line: MsgLine) -> usize {
        let idx = self.lines.len();
        self.lines.push(line);
        self.invalidate_all_cache();
        idx
    }

    pub(crate) fn save_stream(&mut self) {
        let text = std::mem::take(&mut self.stream_line);
        if !text.is_empty() {
            self.push_line(MsgLine::new(text, self.stream_kind));
        }
        self.stream_revision = self.stream_revision.wrapping_add(1);
        self.invalidate_stream_cache();
        self.auto_scroll = true;
    }

    pub(crate) fn finalize_stream(&mut self) {
        self.save_stream();
        self.streaming = false;
    }

    pub(crate) fn add_help(&mut self) {
        self.push_line(MsgLine::new("Commands:".into(), MsgKind::Info));
        self.push_line(MsgLine::new(
            "  /flash          Switch to flash tier".into(),
            MsgKind::Text,
        ));
        self.push_line(MsgLine::new(
            "  /pro            Switch to pro tier".into(),
            MsgKind::Text,
        ));
        self.push_line(MsgLine::new(
            "  /compact        Force context compaction".into(),
            MsgKind::Text,
        ));
        self.push_line(MsgLine::new(
            "  /skills         List available skills".into(),
            MsgKind::Text,
        ));
        self.push_line(MsgLine::new(
            "  Ctrl+C          Interrupt current task".into(),
            MsgKind::Text,
        ));
        self.push_line(MsgLine::new(
            "  Ctrl+C again    Exit TUI".into(),
            MsgKind::Text,
        ));
        self.push_line(MsgLine::new(
            "  Esc             Exit TUI".into(),
            MsgKind::Text,
        ));
        self.push_line(MsgLine::new(
            "  /exit  /quit    Exit TUI".into(),
            MsgKind::Text,
        ));
    }

    pub(crate) fn show_skills(&mut self) {
        self.push_line(MsgLine::new(
            "=== Built-in Skills ===".into(),
            MsgKind::Info,
        ));
        for skill in crate::assets::embedded_skills::all() {
            self.push_line(MsgLine::new(
                format!("  {} — {}", skill.name, skill.description),
                MsgKind::Text,
            ));
        }
        self.push_line(MsgLine::new(
            "Use --skill NAME or Skill(name) to load.".into(),
            MsgKind::Info,
        ));
    }
}
