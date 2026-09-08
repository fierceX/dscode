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

/// One clipboard image staged for the next user message. The bytes live in
/// `<session_dir>/attachments/<sha256>.png`; the message only carries the path.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PendingImage {
    pub path: PathBuf,
    pub width: u32,
    pub height: u32,
    pub bytes: usize,
}

impl PendingImage {
    /// Stable marker appended to the submitted user text. The model is asked
    /// to `Read` the absolute path; the capture itself still runs through the
    /// v7 image pipeline.
    pub(crate) fn marker(&self) -> String {
        format!(
            "[Attached image: \"{}\" - Read it to view.]",
            self.path.display()
        )
    }

    pub(crate) fn chip(&self, index: usize) -> String {
        format!(
            "[image #{index} {}x{} {}]",
            self.width,
            self.height,
            format_bytes(self.bytes)
        )
    }
}

pub(crate) fn format_bytes(bytes: usize) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = 1024.0 * 1024.0;
    let value = bytes as f64;
    if value >= MB {
        scaled(value / MB, "MB")
    } else if value >= KB {
        scaled(value / KB, "KB")
    } else {
        format!("{bytes}B")
    }
}

/// At most one decimal: `220KB` instead of `220.0KB`, `1.5KB` keeps its digit.
fn scaled(value: f64, unit: &str) -> String {
    let rounded = (value * 10.0).round() / 10.0;
    if (rounded - rounded.trunc()).abs() < f64::EPSILON {
        format!("{rounded:.0}{unit}")
    } else {
        format!("{rounded:.1}{unit}")
    }
}

/// Result of a background clipboard read, delivered back to the TUI loop.
#[derive(Debug)]
pub(crate) enum TuiUiEvent {
    ImageCaptured(PendingImage),
    ClipboardFailed(String),
}

/// Injectable clipboard reader (tests); production uses the platform impl.
pub(crate) type ClipboardReader = std::sync::Arc<
    dyn Fn(
            &std::path::Path,
            &crate::runtime::OpenAiChatImageUrlLimits,
        ) -> anyhow::Result<crate::tui::clipboard::ClipboardPng>
        + Send
        + Sync,
>;

/// Submitted text for the runtime: the typed text plus one marker per image.
pub(crate) fn submitted_user_input(typed: &str, images: &[PendingImage]) -> String {
    if images.is_empty() {
        return typed.to_string();
    }
    let markers = images
        .iter()
        .map(PendingImage::marker)
        .collect::<Vec<_>>()
        .join("\n");
    if typed.is_empty() {
        markers
    } else {
        format!("{typed}\n\n{markers}")
    }
}

/// Transcript echo for one user message: images collapse to `[image #N]`.
pub(crate) fn display_user_input(typed: &str, images: &[PendingImage]) -> String {
    let prefix = (1..=images.len())
        .map(|index| format!("[image #{index}]"))
        .collect::<Vec<_>>()
        .join(" ");
    match (typed.is_empty(), prefix.is_empty()) {
        (true, _) => prefix,
        (false, true) => typed.to_string(),
        (false, false) => format!("{prefix} {typed}"),
    }
}

/// Compact paste markers in replayed user input back to `[image]` so a resumed
/// session does not echo absolute attachment paths.
pub(crate) fn compact_user_input_for_display(text: &str) -> String {
    text.split('\n')
        .map(|line| {
            if line.starts_with("[Attached image: \"") && line.ends_with("\" - Read it to view.]") {
                "[image]"
            } else {
                line
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[derive(Clone, Default)]
pub(crate) struct InputState {
    pub buf: String,
    pub cursor: usize,
    pub scroll_row: usize,
    pub history: Vec<String>,
    pub history_idx: Option<usize>,
    pub draft_before_history: Option<String>,
    /// Clipboard images queued for the next submitted message.
    pub pending_images: Vec<PendingImage>,
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
    /// Paste staging directory (`<session_dir>/attachments`).
    pub attachments_dir: PathBuf,
    /// Frozen session image limits; `None` disables clipboard image paste.
    pub image_input: Option<crate::runtime::OpenAiChatImageUrlLimits>,
    /// Result channel of background clipboard reads (absent in tests that do
    /// not exercise the async path).
    pub ui_tx: Option<std::sync::mpsc::Sender<TuiUiEvent>>,
    /// When the in-flight clipboard read started. A read that never reports
    /// back (hung `osascript`) must not disable paste for the whole session.
    pub clipboard_started: Option<Instant>,
    /// Injectable clipboard reader; `None` uses the platform implementation.
    pub clipboard_reader: Option<ClipboardReader>,
    pub artifact_detail: Option<ArtifactDetail>,
    /// 流式期间收到的 Info 信号（如 llm_wait_heartbeat）。
    /// 不打断进行中的 markdown 流：先缓冲，待流结束时落为独立条目。
    pub pending_infos: Vec<String>,
    /// 流式期间的等待状态标签（心跳精简后，如 `·30s`），仅在状态栏瞬时展示。
    pub stream_status: Option<String>,
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
            attachments_dir: PathBuf::new(),
            image_input: None,
            ui_tx: None,
            clipboard_started: None,
            clipboard_reader: None,
            artifact_detail: None,
            pending_infos: Vec::new(),
            stream_status: None,
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
        // 流式期间缓冲的 Info（非心跳告警等）在流结束后落盘：此时文本已完整，
        // 不会被切成两段重新按 markdown 解析（围栏上下文不再丢失）。
        self.stream_status = None;
        let pending = std::mem::take(&mut self.pending_infos);
        for info in pending {
            self.push_line(TranscriptItem::new(info, TranscriptKind::Info));
        }
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

    /// Apply one background clipboard result. Failed reads are reported once;
    /// a captured image only grows the pending list (the chip row is the
    /// visible feedback).
    pub(crate) fn apply_ui_event(&mut self, event: TuiUiEvent) {
        self.clipboard_started = None;
        match event {
            TuiUiEvent::ImageCaptured(image) => {
                // The same bytes stage to the same content-addressed path: a
                // duplicate paste would make the model receive one picture
                // twice (double vision tokens) for no benefit.
                if self
                    .input
                    .pending_images
                    .iter()
                    .any(|pending| pending.path == image.path)
                {
                    self.push_line(TranscriptItem::new(
                        format!("Image already queued: {}", image.path.display()),
                        TranscriptKind::Info,
                    ));
                } else {
                    self.input.pending_images.push(image);
                }
            }
            TuiUiEvent::ClipboardFailed(message) => {
                self.push_line(TranscriptItem::new(
                    format!("Clipboard image unavailable: {message}"),
                    TranscriptKind::Error,
                ));
            }
        }
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
        for (index, line) in crate::local::COMMON_COMMAND_HELP
            .iter()
            .chain(crate::local::TUI_EXTRA_HELP)
            .enumerate()
        {
            self.push_line(TranscriptItem::new(
                line.to_string(),
                if index == 0 {
                    TranscriptKind::Info
                } else {
                    TranscriptKind::Text
                },
            ));
        }
    }

    pub(crate) fn show_skills(&mut self) {
        self.push_line(TranscriptItem::new(
            "=== Skills ===".into(),
            TranscriptKind::Info,
        ));
        match crate::local::discoverable_skill_lines() {
            Ok(lines) => {
                for line in lines {
                    self.push_line(TranscriptItem::new(line, TranscriptKind::Text));
                }
            }
            Err(error) => {
                self.push_line(TranscriptItem::new(
                    format!("Error loading skills: {error}"),
                    TranscriptKind::Error,
                ));
            }
        }
        self.push_line(TranscriptItem::new(
            crate::local::SKILL_LOAD_HINT.into(),
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

/// 识别 LLM 等待心跳消息并提取精简状态标签（如 `·30s`）。
/// 消息格式由 mink-core 统一（`mink::runtime::llm_wait_heartbeat_message`）；
/// 非心跳消息返回 None（按普通告警处理）。
pub(crate) fn heartbeat_status_label(msg: &str) -> Option<String> {
    let elapsed = mink::runtime::parse_llm_wait_heartbeat_elapsed(msg)?;
    Some(format!("·{elapsed}s"))
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
