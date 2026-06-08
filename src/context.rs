use crate::cancel::CancellationToken;
use crate::config::{Config, OutputFormat, ToolApprovalMode, ToolApprovalPolicy, ToolDisableFlags};
use crate::session::artifacts::ArtifactManager;
use crate::session::compaction::CompactionEngine;
use crate::session::prefix::ImmutablePrefix;
use crate::session::stats::StatsTracker;
use crate::session::store::ConversationStore;
use crate::tools::snapshot::FileSnapshotStore;
use crate::ui::Display;
use serde_json::Value;
use std::collections::BTreeMap;
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use crate::config::SandboxPythonConfig;

#[derive(Clone)]
pub struct ToolConfig {
    pub tool_timeout_secs: i32,
    pub sub_agent_timeout_secs: i32,
    pub tool_result_max_bytes: usize,
    pub file_write_max_bytes: usize,
    /// 工具禁用开关（运行时覆盖）
    pub tool_disable: ToolDisableFlags,
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
            tool_disable: cfg.tool_disable.clone(),
            tool_approval_mode: cfg.tool_approval_mode,
            tool_approval: cfg.tool_approval.clone(),
            sandbox_python: cfg.sandbox_python.clone(),
        }
    }
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
    pub tool_config: ToolConfig,
    pub interrupt: Arc<AtomicBool>,
}

impl From<&AgentSharedContext> for ToolContext {
    fn from(ctx: &AgentSharedContext) -> Self {
        Self {
            cwd: ctx.cwd.clone(),
            home: ctx.home.clone(),
            store: ctx.store.clone(),
            artifacts: ctx.artifacts.clone(),
            snapshots: ctx.snapshots.clone(),
            tool_config: ctx.tool_config.clone(),
            interrupt: ctx.interrupt.clone(),
        }
    }
}

/// AgentSharedContext holds all shared resources accessible by every component.
pub struct AgentSharedContext {
    pub config: Config,
    pub cwd: PathBuf,
    pub home: PathBuf,
    pub api_url: String,
    pub store: Arc<ConversationStore>,
    pub artifacts: Arc<ArtifactManager>,
    pub snapshots: Arc<Mutex<FileSnapshotStore>>,
    pub stats: Arc<StatsTracker>,
    pub compaction: Arc<CompactionEngine>,
    pub cancel: CancellationToken,
    pub display: Arc<dyn Display>,
    /// TUI-only: mpsc sender for sub-agent streaming. Set in main.rs for TUI mode.
    pub sub_stream_tx: Option<Arc<dyn std::any::Any + Send + Sync>>,
    pub tool_config: ToolConfig,
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
    pub fn model(&self) -> &str {
        crate::config::resolve_model_name(&self.config.model)
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
        if self.config.output_format == OutputFormat::StreamJson
            && let Err(e) = writeln!(std::io::stdout(), "{line}")
        {
            self.warn_event_log_once(&format!("failed to write stream-json event: {e}"));
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
