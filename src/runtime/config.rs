use crate::config::Config;
use crate::runtime::EventSink;
use crate::session::paths::Paths;
use crate::ui::{Display, StatsSnapshot, SubAgentStreamSink};
#[cfg(test)]
use crate::llm::client::LlmClient;
use std::path::PathBuf;
use std::sync::Arc;

/// Configuration required to embed mink as a Rust runtime.
///
/// The contained [`Config`] is the same configuration type consumed by the
/// CLI. The runtime builder derives `ToolConfig`, session paths, API URL,
/// compaction state, and the orchestrator from this value so that embedded
/// callers and the `mink`/`mink-core` binaries share the same execution logic.
pub struct AgentRuntimeConfig {
    pub config: Config,
    pub home: PathBuf,
    pub cwd: PathBuf,
    pub session: SessionPolicy,
    pub first_prompt: Option<String>,
    pub display: Option<Arc<dyn Display>>,
    pub event_sink: Option<Arc<dyn EventSink>>,
    pub sub_stream_tx: Option<Arc<dyn SubAgentStreamSink>>,
    /// Test-only: inject a mock LLM client so integration tests can exercise
    /// `run_turn()` end-to-end without live API calls.
    #[cfg(test)]
    pub(crate) llm_override: Option<Arc<dyn LlmClient>>,
}

impl AgentRuntimeConfig {
    /// Create runtime configuration from an existing mink config and explicit
    /// home/current working directories.
    ///
    /// `config.session_id` and `config.continue_session` are converted into a
    /// [`SessionPolicy`] here. The original config is still passed through to
    /// the runtime after session resolution, with `config.session_id` rewritten
    /// to the concrete session id.
    pub fn from_config(config: Config, home: PathBuf, cwd: PathBuf) -> Self {
        let session = if !config.session_id.trim().is_empty() {
            SessionPolicy::UseOrCreate(config.session_id.trim().to_string())
        } else if config.continue_session {
            SessionPolicy::ContinueLatest
        } else {
            SessionPolicy::New
        };
        let first_prompt = (!config.prompt.trim().is_empty()).then(|| config.prompt.clone());
        Self {
            config,
            home,
            cwd,
            session,
            first_prompt,
            display: None,
            event_sink: None,
            sub_stream_tx: None,
            #[cfg(test)]
            llm_override: None,
        }
    }

    pub fn with_display(mut self, display: Arc<dyn Display>) -> Self {
        self.display = Some(display);
        self
    }

    pub fn with_event_sink(mut self, event_sink: Arc<dyn EventSink>) -> Self {
        self.event_sink = Some(event_sink);
        self
    }

    pub fn with_sub_stream_tx(mut self, sub_stream_tx: Arc<dyn SubAgentStreamSink>) -> Self {
        self.sub_stream_tx = Some(sub_stream_tx);
        self
    }

    pub fn with_first_prompt(mut self, first_prompt: Option<String>) -> Self {
        self.first_prompt = first_prompt;
        self
    }
}

/// Session selection policy for embedded callers.
///
/// `UseOrCreate` matches the historical `--session NAME` behavior: resolve an
/// existing reference when possible, otherwise create a new session with that
/// alias. `Resume` is stricter and fails if the reference does not exist.
#[derive(Debug, Clone)]
pub enum SessionPolicy {
    New,
    Resume(String),
    ContinueLatest,
    UseOrCreate(String),
}

/// Concrete session paths and identity selected by the runtime builder.
///
/// These paths are the same paths emitted by the SDK protocol and used by the
/// existing binaries for event logs, conversation JSONL, artifacts, summaries,
/// and plan files.
#[derive(Debug, Clone)]
pub struct SessionInfo {
    pub session_id: String,
    pub session_ref: String,
    pub is_new: bool,
    pub home: PathBuf,
    pub cwd: PathBuf,
    pub events_path: PathBuf,
    pub conversation_path: PathBuf,
    pub artifacts_dir: PathBuf,
    pub summary_path: PathBuf,
    pub plan_path: PathBuf,
    pub plan_draft_path: PathBuf,
}

impl SessionInfo {
    pub(crate) fn new(
        session_id: String,
        session_ref: String,
        is_new: bool,
        home: PathBuf,
        cwd: PathBuf,
        paths: &Paths,
    ) -> Self {
        Self {
            session_id,
            session_ref,
            is_new,
            home,
            cwd,
            events_path: paths.events.clone(),
            conversation_path: paths.conversation.clone(),
            artifacts_dir: paths.artifacts.clone(),
            summary_path: paths.summary.clone(),
            plan_path: paths.plan.clone(),
            plan_draft_path: paths.plan_draft.clone(),
        }
    }
}

#[derive(Default)]
pub(crate) struct NoopDisplay;

impl Display for NoopDisplay {
    fn render_thinking(&self, _content: &str) {}
    fn render_text(&self, _content: &str) {}
    fn render_tool_call(&self, _name: &str, _summary: &str) {}
    fn render_tool_result(&self, _tool_name: &str, _content_preview: &str) {}
    fn render_stop(&self) {}
    fn render_error(&self, _message: &str) {}
    fn render_retry(&self) {}
    fn render_info(&self, _msg: &str) {}
    fn render_title_update(&self, _model: &str, _stats: &StatsSnapshot) {}
    fn render_sub_agent_status(
        &self,
        _session_id: &str,
        _status: &str,
        _in_tokens: u64,
        _out_tokens: u64,
    ) {
    }
    fn render_prompt(&self) {}
    fn render_clear_line(&self) {}
}
