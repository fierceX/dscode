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
use std::sync::{Arc, Mutex};

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
    pub tool_timeout_secs: i32,
    pub tool_result_max_bytes: usize,
    pub file_write_max_bytes: usize,
    pub events_path: PathBuf,
    pub summary_path: PathBuf,
    pub plan_path: PathBuf,
    pub plan_draft_path: PathBuf,
    pub immutable_prefix: Mutex<Option<ImmutablePrefix>>,
}

impl AgentSharedContext {
    pub fn model(&self) -> &str { &self.config.model }
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
