use crate::cancel::CancellationToken;
use crate::config::{Config, OutputFormat};
use crate::session::store::ConversationStore;
use crate::session::stats::StatsTracker;
use crate::session::compaction::CompactionEngine;
use crate::session::prefix::ImmutablePrefix;
use crate::ui::Display;
use serde_json::Value;
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};

/// Tool-level configuration extracted from Config, embedded in AgentSharedContext.
#[derive(Clone)]
pub struct ToolConfig {
    pub tool_timeout_secs: i32,
    pub sub_agent_timeout_secs: i32,
    pub tool_result_max_bytes: usize,
    pub file_write_max_bytes: usize,
}

impl ToolConfig {
    pub fn from_config(cfg: &crate::config::Config) -> Self {
        Self {
            tool_timeout_secs: cfg.tool_timeout_secs,
            sub_agent_timeout_secs: cfg.sub_agent_timeout_secs,
            tool_result_max_bytes: cfg.tool_result_max_bytes,
            file_write_max_bytes: cfg.file_write_max_bytes,
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
    pub tool_config: ToolConfig,
}

impl From<&AgentSharedContext> for ToolContext {
    fn from(ctx: &AgentSharedContext) -> Self {
        Self {
            cwd: ctx.cwd.clone(),
            home: ctx.home.clone(),
            store: ctx.store.clone(),
            tool_config: ctx.tool_config.clone(),
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
}

impl AgentSharedContext {
    pub fn model(&self) -> &str { crate::config::resolve_model_name(&self.config.model) }
    pub fn max_turns(&self) -> i32 { self.config.max_turns }
    pub fn max_tokens(&self) -> i32 { self.config.max_tokens }
    pub fn api_key(&self) -> &str { &self.config.api_key }
    pub fn verbose(&self) -> bool { self.config.verbose }
    pub fn interactive(&self) -> bool { self.config.interactive }

    /// Append a JSON line to events.jsonl. In stream-json mode, also emit to stdout.
    pub fn log_event(&self, value: Value) {
        if !self.config.log_events { return; }
        let line = match serde_json::to_string(&value) {
            Ok(s) => s,
            Err(_) => return,
        };
        if let Ok(mut file) = std::fs::OpenOptions::new()
            .create(true).append(true).open(&self.events_path)
        {
            let _ = writeln!(file, "{line}");
        }
        if self.config.output_format == OutputFormat::StreamJson {
            let _ = writeln!(std::io::stdout(), "{line}");
        }
    }
}
