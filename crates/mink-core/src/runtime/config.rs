use crate::capabilities::{RuntimeSkill, SkillDiscoveryPolicy, SkillProvider};
use crate::config::ResolvedConfig as Config;
use crate::llm::client::LlmBackend;
use crate::resources::ResourceHandler;
use crate::runtime::EventSink;
#[cfg(feature = "prefab")]
use crate::runtime::prefab::PrefabTemplate;
use crate::session::paths::Paths;
use crate::session::paths::SessionLayout;
use crate::tools::vfs::ReadOnlyFileSystem;
use crate::ui::SubAgentStreamSink;
use std::path::PathBuf;
use std::sync::Arc;

/// Fully resolved, runtime-only configuration assembled from grouped options.
///
/// CLI parsing and configuration-source precedence live in `mink-cli`; core
/// receives only the resolved values needed to build a runtime.
pub(crate) struct AgentRuntimeConfig {
    pub config: Config,
    pub home: PathBuf,
    pub cwd: PathBuf,
    pub session: SessionPolicy,
    pub session_layout: SessionLayout,
    pub first_prompt: Option<String>,
    pub event_sink: Option<Arc<dyn EventSink>>,
    pub sub_stream_tx: Option<Arc<dyn SubAgentStreamSink>>,
    pub read_only_fs: Option<Arc<dyn ReadOnlyFileSystem>>,
    pub resource_handlers: Vec<Arc<dyn ResourceHandler>>,
    pub skill_providers: Vec<Arc<dyn SkillProvider>>,
    pub runtime_skills: Vec<RuntimeSkill>,
    pub skill_discovery_policy: SkillDiscoveryPolicy,
    pub llm_backend: Option<Arc<dyn LlmBackend>>,
    /// Knowledge-base scope used by the read-only VFS. Defaults to the
    /// resolved runtime session id.
    pub resource_session_id: Option<String>,
    pub custom_tools: Vec<Arc<dyn crate::runtime::AgentTool>>,
    #[cfg(feature = "prefab")]
    pub prefab_enabled: bool,
    #[cfg(feature = "prefab")]
    pub prefab_template: Option<PrefabTemplate>,
}

#[cfg(test)]
impl AgentRuntimeConfig {
    /// Create runtime configuration from an existing mink config and explicit
    /// home/current working directories.
    ///
    /// `config.session_id` and `config.continue_session` are converted into a
    /// [`SessionPolicy`] here. The original config is still passed through to
    /// the runtime after session resolution, with `config.session_id` rewritten
    /// to the concrete session id.
    pub fn from_config(config: Config, home: PathBuf, cwd: PathBuf) -> Self {
        let session = session_policy_from_config(&config);
        let first_prompt = first_prompt_from_config(&config);
        Self {
            config,
            home,
            cwd,
            session,
            session_layout: SessionLayout::ProjectScoped,
            first_prompt,
            event_sink: None,
            sub_stream_tx: None,
            read_only_fs: None,
            resource_handlers: Vec::new(),
            skill_providers: Vec::new(),
            runtime_skills: Vec::new(),
            skill_discovery_policy: SkillDiscoveryPolicy::Defaults,
            llm_backend: None,
            resource_session_id: None,
            custom_tools: Vec::new(),
            #[cfg(feature = "prefab")]
            prefab_enabled: false,
            #[cfg(feature = "prefab")]
            prefab_template: None,
        }
    }

    pub fn with_event_sink(mut self, event_sink: Arc<dyn EventSink>) -> Self {
        self.event_sink = Some(event_sink);
        self
    }

    pub fn with_llm_backend(mut self, backend: Arc<dyn LlmBackend>) -> Self {
        self.llm_backend = Some(backend);
        self
    }

    pub fn with_session_layout(mut self, layout: SessionLayout) -> Self {
        self.session_layout = layout;
        self
    }
}

pub(crate) fn session_policy_from_config(config: &Config) -> SessionPolicy {
    if !config.session_id.trim().is_empty() {
        SessionPolicy::UseOrCreate(config.session_id.trim().to_string())
    } else if config.continue_session {
        SessionPolicy::ContinueLatest
    } else {
        SessionPolicy::New
    }
}

pub(crate) fn first_prompt_from_config(config: &Config) -> Option<String> {
    (!config.prompt.trim().is_empty()).then(|| config.prompt.clone())
}

/// Session selection policy for embedded callers.
///
/// `UseOrCreate` resolves an existing reference when possible. In the
/// historical project-scoped CLI layout, a missing reference creates a fresh
/// timestamped session and stores the reference as its alias. In `Direct` and
/// `HomeScoped` layouts, a missing reference becomes the sanitized concrete
/// session directory so services can address sessions predictably. In
/// `Isolated`, the supplied home is already the session directory; the resolved
/// id is still written to metadata and events, but no child directory is
/// appended. `Resume` is stricter and fails if the reference does not exist.
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
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
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
    pub usage_path: PathBuf,
    pub plan_path: PathBuf,
    pub plan_draft_path: PathBuf,
    pub todos_path: PathBuf,
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
            usage_path: paths.usage.clone(),
            plan_path: paths.plan.clone(),
            plan_draft_path: paths.plan_draft.clone(),
            todos_path: paths.todos.clone(),
        }
    }
}

#[cfg(test)]
#[path = "config_tests.rs"]
mod tests;
