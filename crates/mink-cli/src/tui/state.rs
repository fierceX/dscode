use crate::tui::file_picker::{FilePickerPolicy, FilePickerState};
use crate::tui::notify::{TaskNotification, TaskNotificationKind};
use crate::tui::sanitize::sanitize_tui_text;
use crate::ui::StatsSnapshot;
use crate::ui::{
    ArtifactDisplay, PlanDisplay, TodoChangeDisplay, TodoDisplay, TodoStatusDisplay,
    ToolPresentation, ToolResultKind,
};
use ratatui::text::Line;
use std::collections::{HashMap, HashSet};
use std::fmt::{Debug, Display as FmtDisplay, Formatter};
use std::path::PathBuf;
use std::time::Instant;

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub(crate) enum TranscriptKind {
    #[default]
    Text,
    Tool,
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
pub(crate) struct ArtifactDetail {
    pub id: String,
    pub content: String,
    pub truncated: bool,
}

#[derive(Clone)]
pub(crate) struct TranscriptItem {
    pub text: String,
    pub kind: TranscriptKind,
    pub collapsed: bool,
    pub collapse_policy: CollapsePolicy,
    pub collapse_overridden: bool,
    pub tool_name: Option<String>,
    pub tool_use_id: Option<String>,
    pub tool_summary: Option<String>,
    pub tool_success: Option<bool>,
    pub tool_exit_code: Option<i32>,
    pub tool_result_kind: Option<ToolResultKind>,
    pub presentation: Option<ToolPresentation>,
    pub artifacts: Vec<ArtifactDisplay>,
    pub sealed: bool,
    pub cached_lines: Option<Vec<Line<'static>>>,
    pub cached_collapsed: bool,
    pub cached_interactive: bool,
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

    fn for_kind(kind: TranscriptKind) -> Self {
        match kind {
            TranscriptKind::StreamThinking => CollapsePolicy::Always,
            TranscriptKind::Tool => CollapsePolicy::Auto {
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

impl TranscriptItem {
    pub(crate) fn cache_valid(&self, interactive: bool) -> bool {
        self.cached_lines.is_some()
            && self.cached_collapsed == self.collapsed
            && self.cached_interactive == interactive
    }

    pub(crate) fn new(text: String, kind: TranscriptKind) -> Self {
        let text = sanitize_tui_text(&text);
        let collapse_policy = CollapsePolicy::for_kind(kind);
        let collapsed = collapse_policy.initial_collapsed(&text);
        TranscriptItem {
            text,
            kind,
            collapsed,
            collapse_policy,
            collapse_overridden: false,
            tool_name: None,
            tool_use_id: None,
            tool_summary: None,
            tool_success: None,
            tool_exit_code: None,
            tool_result_kind: None,
            presentation: None,
            artifacts: Vec::new(),
            sealed: true,
            cached_lines: None,
            cached_collapsed: collapsed,
            cached_interactive: false,
            sub_detail: None,
        }
    }

    pub(crate) fn new_tool_result(tool_name: String, text: String) -> Self {
        let mut line = Self::new(text, TranscriptKind::Tool);
        line.tool_name = Some(tool_name);
        line
    }

    pub(crate) fn new_tool_call(
        tool_use_id: Option<String>,
        tool_name: String,
        summary: String,
    ) -> Self {
        let text = if summary.is_empty() {
            format!("[tool] {tool_name}")
        } else {
            format!("[tool] {summary}")
        };
        let mut item = Self::new(text, TranscriptKind::Tool);
        item.tool_use_id = tool_use_id;
        item.tool_name = Some(tool_name);
        item.tool_summary = Some(summary);
        item.sealed = false;
        item
    }

    pub(crate) fn is_collapsible(&self) -> bool {
        self.collapse_policy != CollapsePolicy::Never
    }

    pub(crate) fn with_sub_detail(mut self, sub_detail: Option<SubAgentDetail>) -> Self {
        self.sub_detail = sub_detail;
        self
    }

    pub(crate) fn invalidate_cache(&mut self) {
        self.cached_lines = None;
    }

    pub(crate) fn toggle_collapsed(&mut self) {
        self.collapsed = !self.collapsed;
        self.collapse_overridden = true;
        self.invalidate_cache();
    }
}

impl Default for TranscriptItem {
    fn default() -> Self {
        TranscriptItem {
            text: String::new(),
            kind: TranscriptKind::Text,
            collapsed: false,
            collapse_policy: CollapsePolicy::Never,
            collapse_overridden: false,
            tool_name: None,
            tool_use_id: None,
            tool_summary: None,
            tool_success: None,
            tool_exit_code: None,
            tool_result_kind: None,
            presentation: None,
            artifacts: Vec::new(),
            sealed: true,
            cached_lines: None,
            cached_collapsed: false,
            cached_interactive: false,
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
    pub click_map: Vec<ClickTarget>,
    pub content_y: u16,
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
    OpenPlan,
    OpenTodos,
    OpenArtifact { id: String },
    OpenSubAgent { session_id: String },
}

#[derive(Clone, Default)]
pub(crate) struct RenderCache {
    pub width: u16,
    pub history_lines: Option<Vec<Line<'static>>>,
    pub stream_width: u16,
    pub stream_kind: TranscriptKind,
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

#[derive(Clone, Default)]
pub(crate) struct InlineSurfaceState {
    pub committed: usize,
}

#[derive(Clone)]
pub(crate) struct TuiState {
    pub lines: Vec<TranscriptItem>,
    pub inline: InlineSurfaceState,
    pub stream_line: String,
    pub stream_kind: TranscriptKind,
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
    pub plan: Option<PlanDisplay>,
    pub todos: Option<TodoDisplay>,
    pub artifacts_dir: PathBuf,
    pub artifact_detail: Option<ArtifactDetail>,
    /// 中断当前任务（由 Ctrl+C 触发），None 表示无中断能力
    pub view: View,
    pub overlay: Option<ActiveOverlay>,
    pub file_picker_policy: FilePickerPolicy,
    pub(crate) task_notification_armed: bool,
    pub(crate) pending_task_notification: Option<TaskNotification>,
}

#[derive(Clone, Debug, Default)]
pub(crate) enum View {
    #[default]
    Main,
    SubAgentDetail {
        session_id: String,
        scroll: usize,
    },
    Plan {
        scroll: usize,
    },
    Todos {
        scroll: usize,
    },
    Artifact {
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
            inline: InlineSurfaceState::default(),
            stream_line: String::new(),
            stream_kind: TranscriptKind::default(),
            streaming: false,
            input: InputState::default(),
            model: "flash".into(),
            cwd_label: short_cwd_label(),
            stats: StatsSnapshot::default(),
            viewport: ViewportState {
                auto_scroll: true,
                ..Default::default()
            },
            dirty: true,
            stream_revision: 0,
            cache: RenderCache::default(),
            quit: false,
            last_interrupt: None,
            work_state: WorkState::Idle,
            sub_agents: SubAgentState::default(),
            plan: None,
            todos: None,
            artifacts_dir: PathBuf::new(),
            artifact_detail: None,
            view: View::Main,
            overlay: None,
            file_picker_policy: FilePickerPolicy::default(),
            task_notification_armed: false,
            pending_task_notification: None,
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

    pub(crate) fn push_line(&mut self, line: TranscriptItem) -> usize {
        let idx = self.lines.len();
        self.lines.push(line);
        self.invalidate_all_cache();
        idx
    }

    pub(crate) fn save_stream(&mut self) {
        let text = std::mem::take(&mut self.stream_line);
        if !text.is_empty() {
            self.push_line(TranscriptItem::new(text, self.stream_kind));
        }
        self.stream_revision = self.stream_revision.wrapping_add(1);
        self.invalidate_stream_cache();
        self.viewport.auto_scroll = true;
    }

    pub(crate) fn finalize_stream(&mut self) {
        self.save_stream();
        self.streaming = false;
    }

    pub(crate) fn promote_stable_stream_prefix(&mut self) {
        if self.stream_kind != TranscriptKind::StreamText || self.stream_line.is_empty() {
            return;
        }
        let end = stable_markdown_prefix_end(&self.stream_line);
        if end == 0 {
            return;
        }
        let remaining = self.stream_line.split_off(end);
        let stable = std::mem::replace(&mut self.stream_line, remaining);
        if !stable.is_empty() {
            self.push_line(TranscriptItem::new(stable, TranscriptKind::StreamText));
        }
        self.stream_revision = self.stream_revision.wrapping_add(1);
        self.invalidate_stream_cache();
        self.viewport.auto_scroll = true;
    }

    pub(crate) fn seal_incomplete_transcript(&mut self, reason: &str) {
        let mut changed = false;
        for item in self.lines.iter_mut().skip(self.inline.committed) {
            if item.sealed {
                continue;
            }
            item.sealed = true;
            if item.kind == TranscriptKind::Tool {
                item.tool_success = Some(false);
                if item.text.is_empty() || item.text.starts_with("[tool]") {
                    item.text = reason.to_string();
                }
            } else if !reason.is_empty() {
                item.text.push('\n');
                item.text.push_str(reason);
            }
            item.invalidate_cache();
            changed = true;
        }
        if changed {
            self.sub_agents.active_sessions.clear();
            self.invalidate_all_cache();
        }
    }

    pub(crate) fn apply_todo_presentation(&mut self, update: &TodoDisplay) {
        if update.changes.is_empty() {
            self.todos = Some(update.clone());
            return;
        }

        let mut current = self.todos.take().unwrap_or_else(|| TodoDisplay {
            revision: update.revision,
            counts: update.counts.clone(),
            items: Vec::new(),
            changes: Vec::new(),
        });
        for change in &update.changes {
            match change {
                TodoChangeDisplay::Added { item } => upsert_todo(&mut current.items, item.clone()),
                TodoChangeDisplay::Updated { id, content } => {
                    if let Some(item) = current.items.iter_mut().find(|item| item.id == *id) {
                        item.content.clone_from(content);
                    }
                }
                TodoChangeDisplay::Removed { id } => {
                    current.items.retain(|item| item.id != *id);
                }
                TodoChangeDisplay::Completed { id } => {
                    set_todo_status(&mut current.items, id, TodoStatusDisplay::Completed);
                }
                TodoChangeDisplay::Activated { id } => {
                    set_todo_status(&mut current.items, id, TodoStatusDisplay::InProgress);
                }
                TodoChangeDisplay::Paused { id } | TodoChangeDisplay::Reopened { id } => {
                    set_todo_status(&mut current.items, id, TodoStatusDisplay::Pending);
                }
            }
        }
        for item in &update.items {
            upsert_todo(&mut current.items, item.clone());
        }
        current.revision = update.revision;
        current.counts = update.counts.clone();
        current.changes.clone_from(&update.changes);
        self.todos = Some(current);
    }

    pub(crate) fn arm_task_notification(&mut self) {
        self.task_notification_armed = true;
        self.pending_task_notification = None;
    }

    pub(crate) fn finish_task_notification(&mut self, kind: TaskNotificationKind) {
        if !self.task_notification_armed {
            return;
        }
        self.task_notification_armed = false;
        self.pending_task_notification = Some(TaskNotification::new(kind, &self.model));
    }

    pub(crate) fn take_task_notification(&mut self) -> Option<TaskNotification> {
        self.pending_task_notification.take()
    }

    pub(crate) fn add_help(&mut self) {
        self.push_line(TranscriptItem::new(
            "Commands:".into(),
            TranscriptKind::Info,
        ));
        self.push_line(TranscriptItem::new(
            "  /flash          Switch to flash alias".into(),
            TranscriptKind::Text,
        ));
        self.push_line(TranscriptItem::new(
            "  /pro            Switch to pro alias".into(),
            TranscriptKind::Text,
        ));
        self.push_line(TranscriptItem::new(
            "  /model NAME     Switch to a model name or alias".into(),
            TranscriptKind::Text,
        ));
        self.push_line(TranscriptItem::new(
            "  /compact        Force context compaction".into(),
            TranscriptKind::Text,
        ));
        self.push_line(TranscriptItem::new(
            "  /skills         List available skills".into(),
            TranscriptKind::Text,
        ));
        self.push_line(TranscriptItem::new(
            "  /plan           Open current plan detail".into(),
            TranscriptKind::Text,
        ));
        self.push_line(TranscriptItem::new(
            "  /todos          Open current todo detail".into(),
            TranscriptKind::Text,
        ));
        self.push_line(TranscriptItem::new(
            "  /sub-agent ID   Open sub-agent detail".into(),
            TranscriptKind::Text,
        ));
        self.push_line(TranscriptItem::new(
            "  /artifact ID    Open a bounded artifact preview".into(),
            TranscriptKind::Text,
        ));
        self.push_line(TranscriptItem::new(
            "  Ctrl+C          Interrupt current task".into(),
            TranscriptKind::Text,
        ));
        self.push_line(TranscriptItem::new(
            "  Ctrl+C again    Exit TUI".into(),
            TranscriptKind::Text,
        ));
        self.push_line(TranscriptItem::new(
            "  Esc             Exit TUI".into(),
            TranscriptKind::Text,
        ));
        self.push_line(TranscriptItem::new(
            "  /exit  /quit    Exit TUI".into(),
            TranscriptKind::Text,
        ));
    }

    pub(crate) fn show_skills(&mut self) {
        self.push_line(TranscriptItem::new(
            "=== Skills ===".into(),
            TranscriptKind::Info,
        ));
        let cwd = std::env::current_dir().unwrap_or_default();
        let home = std::path::PathBuf::from(
            std::env::var("MINK_HOME")
                .or_else(|_| std::env::var("HOME"))
                .unwrap_or_else(|_| String::from(".")),
        );
        match crate::capabilities::CapabilitySnapshot::load_default(
            &cwd,
            &home,
            "skills-list",
            "skills-list",
            &[],
        ) {
            Ok(snapshot) => {
                for skill in &snapshot.skills.discoverable {
                    self.push_line(TranscriptItem::new(
                        format!(
                            "  {} [{}] - {}",
                            skill.skill.name,
                            tui_skill_source_label(skill),
                            skill.skill.description
                        ),
                        TranscriptKind::Text,
                    ));
                }
            }
            Err(e) => {
                self.push_line(TranscriptItem::new(
                    format!("Error loading skills: {e}"),
                    TranscriptKind::Error,
                ));
            }
        }
        self.push_line(TranscriptItem::new(
            "Use --skill NAME or Read skill://NAME to load.".into(),
            TranscriptKind::Info,
        ));
    }

    pub(crate) fn open_artifact(&mut self, id: &str) {
        const MAX_ARTIFACT_DETAIL_BYTES: usize = 256 * 1024;
        let manager = crate::session::artifacts::ArtifactManager::new(self.artifacts_dir.clone());
        match manager.read_text_prefix(id, MAX_ARTIFACT_DETAIL_BYTES) {
            Ok((content, truncated)) => {
                self.artifact_detail = Some(ArtifactDetail {
                    id: id.to_string(),
                    content,
                    truncated,
                });
                self.view = View::Artifact { scroll: 0 };
            }
            Err(error) => {
                self.push_line(TranscriptItem::new(
                    format!("Cannot open artifact://{id}: {error}"),
                    TranscriptKind::Error,
                ));
            }
        }
    }
}

fn stable_markdown_prefix_end(text: &str) -> usize {
    let mut in_fence = false;
    let mut offset = 0usize;
    let mut stable_end = 0usize;
    for line in text.split_inclusive('\n') {
        offset += line.len();
        let trimmed = line.trim();
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            in_fence = !in_fence;
            if !in_fence {
                stable_end = offset;
            }
            continue;
        }
        if !in_fence && trimmed.is_empty() {
            stable_end = offset;
        }
    }
    stable_end
}

fn upsert_todo(items: &mut Vec<crate::ui::TodoItemDisplay>, update: crate::ui::TodoItemDisplay) {
    if let Some(item) = items.iter_mut().find(|item| item.id == update.id) {
        *item = update;
    } else {
        items.push(update);
    }
}

fn set_todo_status(items: &mut [crate::ui::TodoItemDisplay], id: &str, status: TodoStatusDisplay) {
    if let Some(item) = items.iter_mut().find(|item| item.id == id) {
        item.status = status;
    }
}

fn tui_skill_source_label(skill: &crate::capabilities::LoadedSkill) -> &'static str {
    match skill.source.level {
        crate::capabilities::SourceLevel::Runtime => "runtime",
        crate::capabilities::SourceLevel::Project => "project",
        crate::capabilities::SourceLevel::User => "user",
        crate::capabilities::SourceLevel::BuiltIn => "built-in",
    }
}
