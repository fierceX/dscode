use crate::cancel::CancellationToken;
use crate::capabilities::CapabilitySnapshot;
use crate::config::{Config, OutputFormat, ToolApprovalMode, ToolApprovalPolicy};
use crate::llm::client::LlmBackend;
use crate::resources::ResourceRouter;
use crate::session::artifacts::ArtifactManager;
use crate::session::compaction::CompactionEngine;
use crate::session::paths::SessionLayout;
use crate::session::plan::PlanStore;
use crate::session::prefix::ImmutablePrefix;
use crate::session::stats::StatsTracker;
use crate::session::store::ConversationStore;
use crate::session::usage::{UsageJournal, UsageKind, UsageScope};
use crate::tools::semantic_capabilities::ResolvedToolCapabilities;
use crate::tools::snapshot::FileSnapshotStore;
use crate::tools::surface::{AgentRole, ModelToolSurface, ToolResolutionContext};
use crate::tools::vfs::{ReadOnlyFileSystem, VfsScope};
use crate::ui::{Display, SubAgentStreamSink};
use serde_json::Value;
use std::collections::BTreeMap;
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use crate::config::SandboxPythonConfig;

#[derive(Clone)]
pub struct ToolConfig {
    pub tool_timeout_secs: i32,
    pub sub_agent_timeout_secs: i32,
    pub tool_result_max_bytes: usize,
    pub file_write_max_bytes: usize,
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
    pub fn from_config(cfg: &crate::config::Config) -> Self {
        Self {
            tool_timeout_secs: cfg.tool_timeout_secs,
            sub_agent_timeout_secs: cfg.sub_agent_timeout_secs,
            tool_result_max_bytes: cfg.tool_result_max_bytes,
            file_write_max_bytes: cfg.file_write_max_bytes,
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
    let surface = Arc::new(crate::tools::surface::ModelToolSurface::resolve(
        crate::tools::catalog::ToolCatalog::builtin()?,
        config,
        &resolution,
    )?);
    let capabilities = Arc::new(
        crate::tools::semantic_capabilities::ToolCapabilityRegistry::builtin()
            .resolve(&surface, &resolution)?,
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
    pub snapshots: Arc<Mutex<FileSnapshotStore>>,
    pub plan_store: Arc<PlanStore>,
    pub tool_config: ToolConfig,
    pub interrupt: Arc<AtomicBool>,
    pub read_only_fs: Option<Arc<dyn ReadOnlyFileSystem>>,
    pub vfs_scope: VfsScope,
    pub resource_router: Arc<ResourceRouter>,
    pub capability_snapshot: Arc<CapabilitySnapshot>,
    pub tool_resolution_context: ToolResolutionContext,
    pub tool_surface: Arc<ModelToolSurface>,
    pub tool_capabilities: Arc<ResolvedToolCapabilities>,
}

impl From<&AgentSharedContext> for ToolContext {
    fn from(ctx: &AgentSharedContext) -> Self {
        Self {
            cwd: ctx.cwd.clone(),
            home: ctx.home.clone(),
            store: ctx.store.clone(),
            artifacts: ctx.artifacts.clone(),
            snapshots: ctx.snapshots.clone(),
            plan_store: Arc::new(PlanStore::new(
                ctx.plan_path.clone(),
                ctx.plan_draft_path.clone(),
            )),
            tool_config: ctx.tool_config.clone(),
            interrupt: ctx.interrupt.clone(),
            read_only_fs: ctx.read_only_fs.clone(),
            vfs_scope: ctx.vfs_scope.clone(),
            resource_router: ctx.resource_router.clone(),
            capability_snapshot: ctx.capability_snapshot.clone(),
            tool_resolution_context: ctx.tool_resolution_context,
            tool_surface: ctx.tool_surface.clone(),
            tool_capabilities: ctx.tool_capabilities.clone(),
        }
    }
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
}

impl AgentSharedContext {
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

    /// Append a JSON line to events.jsonl. In stream-json mode, also emit to stdout.
    pub fn log_event(&self, value: Value) {
        let line = match serde_json::to_string(&value) {
            Ok(s) => s,
            Err(e) => {
                self.warn_event_log_once(&format!("failed to serialize event: {e}"));
                return;
            }
        };
        if self.config.log_events {
            match std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&self.events_path)
            {
                Ok(mut file) => {
                    if let Err(e) = writeln!(file, "{line}") {
                        self.warn_event_log_once(&format!(
                            "failed to write event log {}: {e}",
                            self.events_path.display()
                        ));
                    }
                }
                Err(e) => {
                    self.warn_event_log_once(&format!(
                        "failed to open event log {}: {e}",
                        self.events_path.display()
                    ));
                }
            }
        }
        if self.config.output_format == OutputFormat::StreamJson {
            let mut stdout = std::io::stdout().lock();
            let write_result = writeln!(stdout, "{line}");
            let flush_result = if write_result.is_ok() && should_flush_stream_event(&value) {
                stdout.flush()
            } else {
                Ok(())
            };
            if let Err(e) = write_result.and(flush_result) {
                self.warn_event_log_once(&format!("failed to write stream-json event: {e}"));
            }
        }
    }

    pub fn log_typed_event(&self, event: crate::events::EventLog) {
        if let Ok(value) = serde_json::to_value(event) {
            self.log_event(value);
        }
    }

    fn warn_event_log_once(&self, message: &str) {
        if !self.event_log_warned.swap(true, Ordering::SeqCst) {
            let _ = writeln!(std::io::stderr(), "[mink] Warning: {message}");
        }
    }
}

fn should_flush_stream_event(value: &Value) -> bool {
    const FLUSH_INTERVAL: Duration = Duration::from_millis(80);
    static LAST_FLUSH: OnceLock<Mutex<Option<Instant>>> = OnceLock::new();

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
    let lock = LAST_FLUSH.get_or_init(|| Mutex::new(None));
    let mut last = lock.lock().unwrap_or_else(|e| e.into_inner());
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
