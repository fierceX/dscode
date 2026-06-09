use crate::tui::file_picker::{FilePickerPolicy, FilePickerState};
use crate::tui::sanitize::sanitize_tui_text;
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
    pub collapse_policy: CollapsePolicy,
    pub collapse_overridden: bool,
    pub tool_name: Option<String>,
    pub cached_lines: Option<Vec<Line<'static>>>,
    pub cached_collapsed: bool,
    pub sub_detail: Option<SubAgentDetail>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum CollapsePolicy {
    Never,
    Always,
    Auto { threshold_lines: usize },
}

impl CollapsePolicy {
    const LARGE_AUTO_COLLAPSE_BYTES: usize = 4096;
    const LARGE_AUTO_COLLAPSE_LINE_WIDTH: usize = 240;

    fn for_kind(kind: MsgKind) -> Self {
        match kind {
            MsgKind::StreamThinking => CollapsePolicy::Always,
            MsgKind::ToolResult => CollapsePolicy::Auto {
                threshold_lines: 20,
            },
            _ => CollapsePolicy::Never,
        }
    }

    fn initial_collapsed(self, text: &str) -> bool {
        match self {
            CollapsePolicy::Never => false,
            CollapsePolicy::Always => true,
            CollapsePolicy::Auto { threshold_lines } => {
                text.lines().count() > threshold_lines
                    || text.len() > Self::LARGE_AUTO_COLLAPSE_BYTES
                    || text.lines().any(|line| {
                        unicode_width::UnicodeWidthStr::width(line)
                            > Self::LARGE_AUTO_COLLAPSE_LINE_WIDTH
                    })
            }
        }
    }

    pub(crate) fn should_collapse_rendered(self, rendered_lines: usize) -> bool {
        match self {
            CollapsePolicy::Auto { threshold_lines } => rendered_lines > threshold_lines,
            CollapsePolicy::Always => true,
            CollapsePolicy::Never => false,
        }
    }
}

impl MsgLine {
    pub(crate) fn cache_valid(&self) -> bool {
        self.cached_lines.is_some() && self.cached_collapsed == self.collapsed
    }

    pub(crate) fn new(text: String, kind: MsgKind) -> Self {
        let text = sanitize_tui_text(&text);
        let collapse_policy = CollapsePolicy::for_kind(kind);
        let collapsed = collapse_policy.initial_collapsed(&text);
        MsgLine {
            text,
            kind,
            collapsed,
            collapse_policy,
            collapse_overridden: false,
            tool_name: None,
            cached_lines: None,
            cached_collapsed: collapsed,
            sub_detail: None,
        }
    }

    pub(crate) fn new_tool_result(tool_name: String, text: String) -> Self {
        let mut line = Self::new(text, MsgKind::ToolResult);
        line.tool_name = Some(tool_name);
        line
    }

    pub(crate) fn is_collapsible(&self) -> bool {
        self.collapse_policy != CollapsePolicy::Never
    }

    pub(crate) fn with_sub_detail(mut self, sub_detail: Option<SubAgentDetail>) -> Self {
        self.sub_detail = sub_detail;
        self
    }

    pub(crate) fn toggle_collapsed(&mut self) {
        self.collapsed = !self.collapsed;
        self.collapse_overridden = true;
        self.invalidate_cache();
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
            collapse_policy: CollapsePolicy::Never,
            collapse_overridden: false,
            tool_name: None,
            cached_lines: None,
            cached_collapsed: false,
            sub_detail: None,
        }
    }
}

#[derive(Clone, Default)]
pub(crate) struct InputState {
    pub buf: String,
    pub cursor: usize,
    pub scroll_row: usize,
    pub history: Vec<String>,
    pub history_idx: Option<usize>,
    pub draft_before_history: Option<String>,
}

impl InputState {
    pub(crate) fn clamped_cursor(&self) -> usize {
        clamp_char_boundary(&self.buf, self.cursor)
    }

    pub(crate) fn clamp_cursor(&mut self) {
        self.cursor = self.clamped_cursor();
    }
}

pub(crate) fn clamp_char_boundary(s: &str, pos: usize) -> usize {
    let mut pos = pos.min(s.len());
    while pos > 0 && !s.is_char_boundary(pos) {
        pos -= 1;
    }
    pos
}

#[derive(Clone, Default)]
pub(crate) struct ViewportState {
    pub scroll: usize,
    pub auto_scroll: bool,
    pub max_scroll: usize,
    pub show_borders: bool,
    pub click_map: Vec<ClickTarget>,
    pub content_y: u16,
    pub effective_scroll: usize,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub(crate) struct ClickTarget {
    pub line_idx: usize,
    pub start_row: usize,
    pub end_row: usize,
    pub action: ClickAction,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub(crate) enum ClickAction {
    ToggleCollapse,
    OpenSubAgentDetail { session_id: String },
}

#[derive(Clone, Default)]
pub(crate) struct RenderCache {
    pub width: u16,
    pub history_lines: Option<Vec<Line<'static>>>,
    pub stream_width: u16,
    pub stream_kind: MsgKind,
    pub stream_revision: u64,
    pub stream_lines: Option<Vec<Line<'static>>>,
}

#[derive(Clone, Default)]
pub(crate) struct SubAgentState {
    pub active_sessions: HashSet<String>,
    pub line_by_session: HashMap<String, usize>,
}

impl SubAgentState {
    pub(crate) fn session_for_line(&self, line_idx: usize) -> Option<&str> {
        self.line_by_session
            .iter()
            .find_map(|(session_id, &idx)| (idx == line_idx).then_some(session_id.as_str()))
    }
}

#[derive(Clone)]
pub(crate) struct TuiState {
    pub lines: Vec<MsgLine>,
    pub stream_line: String,
    pub stream_kind: MsgKind,
    pub streaming: bool,
    pub input: InputState,
    pub model: String,
    pub cwd_label: String,
    pub stats: StatsSnapshot,
    pub viewport: ViewportState,
    pub dirty: bool,
    pub stream_revision: u64,
    pub cache: RenderCache,
    pub quit: bool,
    pub last_interrupt: Option<Instant>,
    pub work_state: WorkState,
    pub sub_agents: SubAgentState,
    /// 中断当前任务（由 Ctrl+C 触发），None 表示无中断能力
    pub interrupt: Option<Arc<AtomicBool>>,
    pub view: View,
    pub overlay: Option<ActiveOverlay>,
    pub file_picker_policy: FilePickerPolicy,
}

#[derive(Clone, Debug, Default)]
pub(crate) enum View {
    #[default]
    Main,
    SubAgentDetail {
        session_id: String,
        scroll: usize,
    },
}

#[derive(Clone, Debug)]
pub(crate) enum ActiveOverlay {
    FilePicker(FilePickerState),
}

impl Default for TuiState {
    fn default() -> Self {
        Self {
            lines: Vec::new(),
            stream_line: String::new(),
            stream_kind: MsgKind::default(),
            streaming: false,
            input: InputState::default(),
            model: "flash".into(),
            cwd_label: short_cwd_label(),
            stats: StatsSnapshot::default(),
            viewport: ViewportState {
                auto_scroll: true,
                show_borders: true,
                ..Default::default()
            },
            dirty: true,
            stream_revision: 0,
            cache: RenderCache::default(),
            quit: false,
            last_interrupt: None,
            work_state: WorkState::Idle,
            sub_agents: SubAgentState::default(),
            interrupt: None,
            view: View::Main,
            overlay: None,
            file_picker_policy: FilePickerPolicy::default(),
        }
    }
}

pub(crate) fn short_cwd_label() -> String {
    let Some(cwd) = std::env::current_dir().ok() else {
        return "?".into();
    };

    if let Some(home) = std::env::var_os("HOME").map(std::path::PathBuf::from)
        && cwd == home
    {
        return "~".into();
    }

    cwd.file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .unwrap_or("/")
        .to_string()
}

impl Debug for TuiState {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "TuiState[lines={}, stream={}, input={}]",
            self.lines.len(),
            self.stream_line.len(),
            self.input.buf.len()
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
        self.cache.history_lines = None;
    }

    pub(crate) fn invalidate_stream_cache(&mut self) {
        self.cache.stream_lines = None;
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
        self.viewport.auto_scroll = true;
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
        self.push_line(MsgLine::new("=== Skills ===".into(), MsgKind::Info));
        let cwd = std::env::current_dir().unwrap_or_default();
        let home = std::path::PathBuf::from(
            std::env::var("MINK_HOME")
                .or_else(|_| std::env::var("HOME"))
                .unwrap_or_else(|_| String::from(".")),
        );
        for skill in crate::skills::list_available_skills(&cwd, &home) {
            let source = match skill.source {
                crate::skills::SkillSource::BuiltIn => "built-in",
                crate::skills::SkillSource::FileSystem => "local",
            };
            self.push_line(MsgLine::new(
                format!("  {} [{}] - {}", skill.name, source, skill.description),
                MsgKind::Text,
            ));
        }
        self.push_line(MsgLine::new(
            "Use --skill NAME or Read skill://NAME to load.".into(),
            MsgKind::Info,
        ));
    }
}
