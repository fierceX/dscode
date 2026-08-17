use crate::cancel::CancellationToken;
use crate::capabilities::CapabilitySnapshot;
use crate::config::{
    EditMode, OutputFormat, ResolvedConfig as Config, ToolApprovalMode, ToolApprovalPolicy,
};
use crate::llm::client::LlmBackend;
use crate::resources::ResourceRouter;
use crate::session::artifacts::ArtifactManager;
use crate::session::compaction::CompactionEngine;
use crate::session::event_log::EventLogWriter;
use crate::session::paths::SessionLayout;
use crate::session::plan::PlanStore;
use crate::session::prefix::ImmutablePrefix;
use crate::session::stats::StatsTracker;
use crate::session::store::ConversationStore;
use crate::session::todo::TodoStore;
use crate::session::usage::{UsageJournal, UsageKind, UsageScope};
use crate::tools::semantic_capabilities::ResolvedToolCapabilities;
use crate::tools::snapshot::FileSnapshotStore;
use crate::tools::surface::{AgentRole, ModelToolSurface, ToolResolutionContext};
use crate::tools::vfs::{ReadOnlyFileSystem, VfsScope};
use crate::ui::{Display, SubAgentStreamSink};
use serde_json::Value;
use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::config::SandboxPythonConfig;

#[derive(Clone)]
pub struct ToolConfig {
    pub tool_timeout_secs: i32,
    /// 单次 Bash/Python/自定义工具调用的超时上限（默认 600 秒）。
    pub tool_timeout_max_secs: i32,
    pub sub_agent_timeout_secs: i32,
    pub tool_result_max_bytes: usize,
    pub file_write_max_bytes: usize,
    pub edit_mode: EditMode,
    pub edit_fuzzy_match: bool,
    pub edit_fuzzy_threshold: f64,
    pub edit_enforce_seen_lines: bool,
    pub max_search_files: usize,
    pub max_search_results: usize,
    /// 工具选择：`None` 使用默认工具集；`Some(vec![])` 不启用任何工具。
    pub enabled_tools: Option<Vec<String>>,
    pub tool_approval_mode: ToolApprovalMode,
    pub tool_approval: BTreeMap<String, ToolApprovalPolicy>,
    /// CPython WASI 沙箱工具配置
    pub sandbox_python: SandboxPythonConfig,
}

impl ToolConfig {
    pub fn from_config(cfg: &crate::config::ResolvedConfig) -> Self {
        Self {
            tool_timeout_secs: cfg.tool_timeout_secs,
            tool_timeout_max_secs: cfg.tool_timeout_max_secs,
            sub_agent_timeout_secs: cfg.sub_agent_timeout_secs,
            tool_result_max_bytes: cfg.tool_result_max_bytes,
            file_write_max_bytes: cfg.file_write_max_bytes,
            edit_mode: cfg.edit_mode,
            edit_fuzzy_match: cfg.edit_fuzzy_match,
            edit_fuzzy_threshold: cfg.edit_fuzzy_threshold,
            edit_enforce_seen_lines: cfg.edit_enforce_seen_lines,
            max_search_files: cfg.max_search_files,
            max_search_results: cfg.max_search_results,
            enabled_tools: cfg.enabled_tools.clone(),
            tool_approval_mode: cfg.tool_approval_mode,
            tool_approval: cfg.tool_approval.clone(),
            sandbox_python: cfg.sandbox_python.clone(),
        }
    }
}

pub(crate) fn resolve_tool_runtime(
    config: &ToolConfig,
    is_sub_agent: bool,
    read_only_fs_present: bool,
    custom_tools: &[crate::runtime::RegisteredCustomTool],
) -> anyhow::Result<(
    ToolResolutionContext,
    Arc<ModelToolSurface>,
    Arc<ResolvedToolCapabilities>,
)> {
    let resolution = ToolResolutionContext::from_runtime(
        if is_sub_agent {
            AgentRole::SubAgent
        } else {
            AgentRole::Primary
        },
        config,
        read_only_fs_present,
    );
    let surface = Arc::new(
        crate::tools::surface::ModelToolSurface::resolve_with_custom(
            crate::tools::catalog::ToolCatalog::builtin()?,
            config,
            &resolution,
            custom_tools,
        )?,
    );
    let capabilities = Arc::new(
        crate::tools::semantic_capabilities::ToolCapabilityRegistry::builtin()
            .resolve_with_custom(&surface, &resolution, custom_tools)?,
    );
    Ok((resolution, surface, capabilities))
}

/// ToolContext — 工具层只需要这些字段，不依赖 LLM、cancel、compaction 等。
/// 从 `AgentSharedContext` 通过 `From` trait 创建。
#[derive(Clone)]
pub struct ToolContext {
    pub cwd: PathBuf,
    pub home: PathBuf,
    pub store: Arc<ConversationStore>,
    pub artifacts: Arc<ArtifactManager>,
    pub read_memo: Arc<Mutex<crate::tools::read_memo::ReadMemo>>,
    pub memo_epoch: Arc<AtomicU64>,
    pub memo_mutation: Arc<AtomicU64>,
    pub snapshots: Arc<Mutex<FileSnapshotStore>>,
    pub plan_store: Arc<PlanStore>,
    pub todo_store: Arc<TodoStore>,
    pub tool_config: ToolConfig,
    pub interrupt: Arc<AtomicBool>,
    pub read_only_fs: Option<Arc<dyn ReadOnlyFileSystem>>,
    pub vfs_scope: VfsScope,
    pub resource_router: Arc<ResourceRouter>,
    pub capability_snapshot: Arc<CapabilitySnapshot>,
    pub tool_resolution_context: ToolResolutionContext,
    pub tool_surface: Arc<ModelToolSurface>,
    pub tool_capabilities: Arc<ResolvedToolCapabilities>,
    pub(crate) custom_tools: Arc<Vec<crate::runtime::RegisteredCustomTool>>,
}

impl From<&AgentSharedContext> for ToolContext {
    fn from(ctx: &AgentSharedContext) -> Self {
        Self {
            cwd: ctx.cwd.clone(),
            home: ctx.home.clone(),
            store: ctx.store.clone(),
            artifacts: ctx.artifacts.clone(),
            read_memo: ctx.read_memo.clone(),
            memo_epoch: ctx.memo_epoch.clone(),
            memo_mutation: ctx.memo_mutation.clone(),
            snapshots: ctx.snapshots.clone(),
            plan_store: Arc::new(PlanStore::new(
                ctx.plan_path.clone(),
                ctx.plan_draft_path.clone(),
            )),
            todo_store: ctx.todo_store.clone(),
            tool_config: ctx.tool_config.clone(),
            interrupt: ctx.interrupt.clone(),
            read_only_fs: ctx.read_only_fs.clone(),
            vfs_scope: ctx.vfs_scope.clone(),
            resource_router: ctx.resource_router.clone(),
            capability_snapshot: ctx.capability_snapshot.clone(),
            tool_resolution_context: ctx.tool_resolution_context,
            tool_surface: ctx.tool_surface.clone(),
            tool_capabilities: ctx.tool_capabilities.clone(),
            custom_tools: ctx.custom_tools.clone(),
        }
    }
}

impl ToolContext {
    /// When the file is byte-identical and an earlier read covers the requested
    /// range, return a short behavioral "reuse that content" response. This is
    /// engine-internal: it never references token budgets or memo mechanics.
    pub fn memo_hit(
        &self,
        path: &Path,
        raw: bool,
        start_line: Option<usize>,
        end_line: Option<usize>,
    ) -> Option<String> {
        let meta = std::fs::metadata(path).ok()?;
        let mtime = meta.modified().ok()?;
        let epoch = self.memo_epoch.load(Ordering::SeqCst);
        let mutation = self.memo_mutation.load(Ordering::SeqCst);
        let hit = self
            .read_memo
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .hit(
                path,
                meta.len(),
                mtime,
                raw,
                epoch,
                mutation,
                start_line,
                end_line,
            );
        if !hit {
            return None;
        }
        let display = display_tool_path(&self.cwd, path);
        let tag = self
            .snapshots
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .latest_tag(path);
        let mut out = format!(
            "Read({display}): unchanged, no edits since. Reuse that content. If the file changed or you need a different range, read again with a selector."
        );
        if let Some(tag) = tag {
            out.push_str(&format!(
                " Current snapshot: [{display}#{tag}]. Use that header for edits."
            ));
        }
        Some(out)
    }

    /// Record a local read so a later identical request can be short-circuited.
    pub fn memo_record(
        &self,
        path: &Path,
        raw: bool,
        start_line: Option<usize>,
        end_line: Option<usize>,
    ) {
        let Ok(meta) = std::fs::metadata(path) else {
            return;
        };
        let Ok(mtime) = meta.modified() else {
            return;
        };
        let epoch = self.memo_epoch.load(Ordering::SeqCst);
        let mutation = self.memo_mutation.load(Ordering::SeqCst);
        self.read_memo
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .record(
                path,
                meta.len(),
                mtime,
                raw,
                start_line,
                end_line,
                epoch,
                mutation,
            );
    }

    /// Invalidate every memo of this agent after a successful Write/Edit.
    pub fn bump_mutation(&self) {
        self.memo_mutation.fetch_add(1, Ordering::SeqCst);
    }
}

fn display_tool_path(cwd: &Path, path: &Path) -> String {
    let relative = path.strip_prefix(cwd).unwrap_or(path);
    relative
        .to_string_lossy()
        .replace(std::path::MAIN_SEPARATOR, "/")
}

/// AgentSharedContext holds all shared resources accessible by every component.
pub struct AgentSharedContext {
    pub config: Config,
    pub cwd: PathBuf,
    pub home: PathBuf,
    pub session_layout: SessionLayout,
    pub api_url: String,
    pub llm_backend: Arc<dyn LlmBackend>,
    pub store: Arc<ConversationStore>,
    pub artifacts: Arc<ArtifactManager>,
    pub todo_store: Arc<TodoStore>,
    pub read_memo: Arc<Mutex<crate::tools::read_memo::ReadMemo>>,
    pub memo_epoch: Arc<AtomicU64>,
    pub memo_mutation: Arc<AtomicU64>,
    pub snapshots: Arc<Mutex<FileSnapshotStore>>,
    pub stats: Arc<StatsTracker>,
    pub usage: Arc<UsageJournal>,
    pub compaction: Arc<CompactionEngine>,
    pub cancel: CancellationToken,
    pub display: Arc<dyn Display>,
    /// Optional sink for live sub-agent streaming. TUI sets this to a channel-backed sink.
    pub sub_stream_tx: Option<Arc<dyn SubAgentStreamSink>>,
    pub read_only_fs: Option<Arc<dyn ReadOnlyFileSystem>>,
    pub vfs_scope: VfsScope,
    pub resource_router: Arc<ResourceRouter>,
    pub capability_snapshot: Arc<CapabilitySnapshot>,
    pub tool_config: ToolConfig,
    pub tool_resolution_context: ToolResolutionContext,
    pub tool_surface: Arc<ModelToolSurface>,
    pub tool_capabilities: Arc<ResolvedToolCapabilities>,
    pub(crate) custom_tools: Arc<Vec<crate::runtime::RegisteredCustomTool>>,
    pub events_path: PathBuf,
    pub summary_path: PathBuf,
    pub plan_path: PathBuf,
    pub plan_draft_path: PathBuf,
    pub immutable_prefix: Mutex<Option<ImmutablePrefix>>,
    /// 是否为子代理上下文。为 true 时禁止递归调用 SubAgent。
    pub is_sub_agent: bool,
    /// 用于中断当前任务的原子标志。每轮开始时重置为 false。
    pub interrupt: Arc<AtomicBool>,
    /// Avoid repeated stderr warnings if event logging starts failing.
    pub event_log_warned: AtomicBool,
    /// Optional serialized writer for production runtimes. Hand-built test
    /// contexts must inject a writer; there is intentionally no synchronous
    /// fallback, so unset writers discard (with one warning) instead of
    /// silently writing through a different path.
    pub(crate) event_log_writer: Option<EventLogWriter>,
    /// Per-context stream-json flush throttle. Deliberately not process-global:
    /// multiple embedded runtimes must not share one flush clock.
    pub(crate) stream_flush_last: Mutex<Option<Instant>>,
}

impl AgentSharedContext {
    pub(crate) fn todo_read_provider(&self) -> Option<&str> {
        use crate::tools::semantic_capabilities::ToolSemanticCapability::TodoInspect;
        self.tool_capabilities
            .primary_provider(TodoInspect)
            .map(|provider| provider.tool.as_str())
    }

    pub(crate) fn todo_advance_provider(&self) -> Option<&str> {
        use crate::tools::semantic_capabilities::ToolSemanticCapability::TodoProgressTransition;
        self.tool_capabilities
            .primary_provider(TodoProgressTransition)
            .map(|provider| provider.tool.as_str())
    }

    pub fn model(&self) -> String {
        crate::config::model_resolver(&self.config)
            .resolve(&self.config.model)
            .actual
    }
    pub fn max_turns(&self) -> i32 {
        self.config.max_turns
    }
    pub fn max_tokens(&self) -> i32 {
        self.config.max_tokens
    }
    pub fn api_key(&self) -> &str {
        &self.config.api_key
    }
    pub fn verbose(&self) -> bool {
        self.config.verbose
    }
    pub fn interactive(&self) -> bool {
        self.config.interactive
    }

    pub fn usage_scope(&self, kind: UsageKind) -> UsageScope {
        self.usage.scope(kind, self.config.session_id.clone())
    }

    /// Append a typed event to events.jsonl. In stream-json mode, also emit
    /// to stdout. Persistence requires a configured `EventLogWriter`; callers
    /// that build a context by hand (tests, embedded builders) inject one.
    pub fn log_event(&self, event: crate::events::EventLog) {
        let value = match serde_json::to_value(event) {
            Ok(value) => value,
            Err(e) => {
                self.warn_event_log_once(&format!("failed to serialize event: {e}"));
                return;
            }
        };
        let line = match serde_json::to_string(&value) {
            Ok(s) => s,
            Err(e) => {
                self.warn_event_log_once(&format!("failed to serialize event: {e}"));
                return;
            }
        };
        if self.config.log_events {
            if let Some(writer) = &self.event_log_writer {
                writer.send(line.clone());
            } else {
                self.warn_event_log_once("event log writer is not configured; event discarded");
            }
        }
        if self.config.output_format == OutputFormat::StreamJson {
            let mut stdout = std::io::stdout().lock();
            let write_result = writeln!(stdout, "{line}");
            let flush_result = if write_result.is_ok() && self.should_flush_stream_event(&value) {
                stdout.flush()
            } else {
                Ok(())
            };
            if let Err(e) = write_result.and(flush_result) {
                self.warn_event_log_once(&format!("failed to write stream-json event: {e}"));
            }
        }
    }

    pub(crate) async fn flush_event_log(&self) -> std::io::Result<()> {
        if let Some(writer) = &self.event_log_writer {
            writer.flush().await?;
        }
        Ok(())
    }

    fn warn_event_log_once(&self, message: &str) {
        if !self.event_log_warned.swap(true, Ordering::SeqCst) {
            let _ = writeln!(std::io::stderr(), "[mink] Warning: {message}");
        }
    }

    fn should_flush_stream_event(&self, value: &Value) -> bool {
        const FLUSH_INTERVAL: Duration = Duration::from_millis(80);

        let event_type = value.get("type").and_then(Value::as_str).unwrap_or("");
        let force = matches!(
            event_type,
            "final"
                | "tool_call"
                | "tool_result"
                | "stop"
                | "error"
                | "retry"
                | "turn_error"
                | "llm_wait"
        );

        let now = Instant::now();
        let mut last = self
            .stream_flush_last
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let due = match *last {
            Some(prev) => now.duration_since(prev) >= FLUSH_INTERVAL,
            None => true,
        };
        let stream_delta = matches!(event_type, "text");
        if force || (stream_delta && due) {
            *last = Some(now);
            true
        } else {
            false
        }
    }
}
