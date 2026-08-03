use crate::capabilities::{CapabilityExposure, RuntimeSkill, SkillDiscoveryPolicy, SkillProvider};
use crate::config::{
    Config, EditMode, OpenAiTokenParamConfig, OutputFormat, SandboxConfig, SandboxPythonConfig,
    ToolApprovalMode, ToolApprovalPolicy,
};
use crate::llm::client::{LlmBackend, TokenParamKind};
use crate::resources::ResourceHandler;
use crate::runtime::config::{first_prompt_from_config, session_policy_from_config};
use crate::runtime::{AgentRuntimeConfig, EventSink, SessionPolicy};
use crate::session::paths::SessionLayout;
use crate::tools::vfs::ReadOnlyFileSystem;
use crate::ui::{Display, SubAgentStreamSink};
use anyhow::Result;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

/// Ergonomic builder for embedding mink from Rust.
///
/// `AgentOptions` is a convenience layer only. It owns a complete [`Config`]
/// and converts losslessly into [`AgentRuntimeConfig`], so callers can either
/// use the typed setters below or mutate the underlying config directly through
/// [`AgentOptions::config_mut`]. This keeps the public Rust API compact without
/// creating a second, reduced configuration model.
pub struct AgentOptions {
    config: Config,
    home: PathBuf,
    cwd: PathBuf,
    session: SessionPolicy,
    session_layout: SessionLayout,
    session_overridden: bool,
    first_prompt: Option<String>,
    first_prompt_overridden: bool,
    display: Option<Arc<dyn Display>>,
    event_sink: Option<Arc<dyn EventSink>>,
    sub_stream_tx: Option<Arc<dyn SubAgentStreamSink>>,
    read_only_fs: Option<Arc<dyn ReadOnlyFileSystem>>,
    resource_handlers: Vec<Arc<dyn ResourceHandler>>,
    skill_providers: Vec<Arc<dyn SkillProvider>>,
    runtime_skills: Vec<RuntimeSkill>,
    skill_discovery_policy: SkillDiscoveryPolicy,
    llm_backend: Option<Arc<dyn LlmBackend>>,
    resource_session_id: Option<String>,
}

impl AgentOptions {
    /// Create options from default mink configuration and explicit home/cwd.
    ///
    /// `AgentOptions` defaults to [`SessionLayout::Isolated`]: the supplied
    /// `home` is treated as the concrete session directory. Use
    /// [`AgentOptions::with_direct_sessions`] when `home` is a shared root that
    /// should contain one child directory per `session_id`, or
    /// [`AgentOptions::with_home_scoped_sessions`] /
    /// [`AgentOptions::with_project_scoped_sessions`] for SDK/CLI-compatible
    /// layouts.
    ///
    /// Provider defaults, config files, and environment merging are not applied
    /// here. CLI callers should keep using the CLI adapter; embedded callers
    /// that want those effects can call the existing config helpers before
    /// constructing options with [`AgentOptions::from_config`].
    pub fn new(home: impl Into<PathBuf>, cwd: impl Into<PathBuf>) -> Self {
        Self::from_config(Config::default(), home, cwd)
    }

    /// Create options from a complete mink [`Config`].
    pub fn from_config(config: Config, home: impl Into<PathBuf>, cwd: impl Into<PathBuf>) -> Self {
        let first_prompt = first_prompt_from_config(&config);
        let session = session_policy_from_config(&config);
        Self {
            config,
            home: home.into(),
            cwd: cwd.into(),
            session,
            session_layout: SessionLayout::Isolated,
            session_overridden: false,
            first_prompt,
            first_prompt_overridden: false,
            display: None,
            event_sink: None,
            sub_stream_tx: None,
            read_only_fs: None,
            resource_handlers: Vec::new(),
            skill_providers: Vec::new(),
            runtime_skills: Vec::new(),
            skill_discovery_policy: SkillDiscoveryPolicy::Defaults,
            llm_backend: None,
            resource_session_id: None,
        }
    }

    /// Inspect the complete underlying mink config.
    pub fn config(&self) -> &Config {
        &self.config
    }

    /// Mutate any config field that does not have a dedicated convenience
    /// setter. This is the supported escape hatch for full Config coverage.
    pub fn config_mut(&mut self) -> &mut Config {
        &mut self.config
    }

    pub fn with_config(mut self, config: Config) -> Self {
        if !self.first_prompt_overridden {
            self.first_prompt = first_prompt_from_config(&config);
        }
        if !self.session_overridden {
            self.session = session_policy_from_config(&config);
        }
        self.config = config;
        self
    }

    pub fn with_home(mut self, home: impl Into<PathBuf>) -> Self {
        self.home = home.into();
        self
    }

    pub fn with_cwd(mut self, cwd: impl Into<PathBuf>) -> Self {
        self.cwd = cwd.into();
        self
    }

    pub fn with_session(mut self, session: SessionPolicy) -> Self {
        self.session = session;
        self.session_overridden = true;
        self
    }

    pub fn with_session_layout(mut self, layout: SessionLayout) -> Self {
        self.session_layout = layout;
        self
    }

    pub fn with_project_scoped_sessions(self) -> Self {
        self.with_session_layout(SessionLayout::ProjectScoped)
    }

    /// Store sessions under `home/.mink/sessions/<session_id>`.
    pub fn with_home_scoped_sessions(self) -> Self {
        self.with_session_layout(SessionLayout::HomeScoped)
    }

    /// Store sessions under `home/<session_id>`.
    pub fn with_direct_sessions(self) -> Self {
        self.with_session_layout(SessionLayout::Direct)
    }

    /// Treat `home` itself as the current session directory.
    pub fn with_isolated_sessions(self) -> Self {
        self.with_session_layout(SessionLayout::Isolated)
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

    pub fn with_llm_backend(mut self, backend: Arc<dyn LlmBackend>) -> Self {
        self.llm_backend = Some(backend);
        self
    }

    pub fn with_openai_reasoning_effort(mut self, effort: impl Into<String>) -> Self {
        let effort = effort.into();
        self.config.openai_reasoning_effort = Some(effort);
        self
    }

    pub fn without_openai_reasoning_effort(mut self) -> Self {
        self.config.openai_reasoning_effort = None;
        self
    }

    pub fn with_openai_include_usage(mut self, include_usage: bool) -> Self {
        self.config.openai_include_usage = include_usage;
        self
    }

    pub fn with_openai_token_param(mut self, token_param: TokenParamKind) -> Self {
        self.config.openai_token_param = match token_param {
            TokenParamKind::MaxTokens => OpenAiTokenParamConfig::MaxTokens,
            TokenParamKind::MaxCompletionTokens => OpenAiTokenParamConfig::MaxCompletionTokens,
        };
        self
    }

    pub fn with_openai_tool_choice(mut self, tool_choice: impl Into<serde_json::Value>) -> Self {
        self.config.openai_tool_choice = Some(tool_choice.into());
        self
    }

    pub fn with_openai_extra_body(
        mut self,
        extra_body: BTreeMap<String, serde_json::Value>,
    ) -> Self {
        self.config.openai_extra_body = extra_body;
        self
    }

    /// Replace local Read/Glob/Grep access with a synchronous read-only VFS.
    pub fn with_read_only_file_system(mut self, fs: Arc<dyn ReadOnlyFileSystem>) -> Self {
        self.read_only_fs = Some(fs);
        self
    }

    pub fn with_resource_handler(mut self, handler: Arc<dyn ResourceHandler>) -> Self {
        self.resource_handlers.push(handler);
        self
    }

    pub fn with_skill_provider(mut self, provider: Arc<dyn SkillProvider>) -> Self {
        self.skill_providers.push(provider);
        self
    }

    pub fn with_runtime_skill(mut self, skill: RuntimeSkill) -> Self {
        self.runtime_skills.push(skill);
        self
    }

    pub fn with_runtime_skill_content(
        self,
        name: impl Into<String>,
        description: impl Into<String>,
        content: impl Into<String>,
    ) -> Self {
        self.with_runtime_skill(RuntimeSkill {
            name: name.into(),
            description: description.into(),
            content: content.into(),
            exposure: CapabilityExposure::ModelDiscoverable,
            revision: None,
        })
    }

    pub fn with_skill_discovery_policy(mut self, policy: SkillDiscoveryPolicy) -> Self {
        self.skill_discovery_policy = policy;
        self
    }

    /// Override the knowledge-base scope passed to the read-only VFS.
    ///
    /// When omitted, the resolved runtime session id is used. Child agents
    /// inherit this value while retaining their own agent session id.
    pub fn with_resource_session_id(mut self, session_id: impl Into<String>) -> Self {
        self.resource_session_id = Some(session_id.into());
        self
    }

    /// Set the metadata first prompt without changing turn execution input.
    pub fn with_first_prompt(mut self, first_prompt: impl Into<String>) -> Self {
        self.first_prompt = Some(first_prompt.into());
        self.first_prompt_overridden = true;
        self
    }

    pub fn with_model(mut self, model: impl Into<String>) -> Self {
        self.config.model = model.into();
        self
    }

    pub fn with_api_key(mut self, api_key: impl Into<String>) -> Self {
        self.config.api_key = api_key.into();
        self
    }

    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.config.base_url = base_url.into();
        self
    }

    pub fn with_max_tokens(mut self, max_tokens: i32) -> Self {
        self.config.max_tokens = max_tokens;
        self
    }

    pub fn with_max_turns(mut self, max_turns: i32) -> Self {
        self.config.max_turns = max_turns;
        self
    }

    pub fn with_max_context_tokens(mut self, max_context_tokens: usize) -> Self {
        self.config.max_context_tokens = max_context_tokens;
        self
    }

    pub fn with_context_compact_pct(mut self, pct: u8) -> Self {
        self.config.context_compact_pct = pct.clamp(1, 100);
        self
    }

    pub fn with_context_reserve_tokens(mut self, tokens: usize) -> Self {
        self.config.context_reserve_tokens = tokens.max(1);
        self
    }

    pub fn with_context_compact_tail_tokens(mut self, tokens: usize) -> Self {
        self.config.context_compact_tail_tokens = tokens.max(1);
        self
    }

    pub fn with_context_compact_max_output_tokens(mut self, tokens: i32) -> Self {
        self.config.context_compact_max_output_tokens = tokens.max(1);
        self
    }

    pub fn with_context_compact_input_reduction(mut self, enabled: bool) -> Self {
        self.config.context_compact_input_reduction = enabled;
        self
    }

    pub fn with_tool_timeout_secs(mut self, secs: i32) -> Self {
        self.config.tool_timeout_secs = secs;
        self
    }

    pub fn with_sub_agent_timeout_secs(mut self, secs: i32) -> Self {
        self.config.sub_agent_timeout_secs = secs;
        self
    }

    pub fn with_llm_timeouts(
        mut self,
        first_event_secs: i32,
        idle_secs: i32,
        wait_heartbeat_secs: i32,
    ) -> Self {
        self.config.llm_first_event_timeout_secs = first_event_secs;
        self.config.llm_idle_timeout_secs = idle_secs;
        self.config.llm_wait_heartbeat_secs = wait_heartbeat_secs;
        self
    }

    pub fn with_tool_result_max_bytes(mut self, bytes: usize) -> Self {
        self.config.tool_result_max_bytes = bytes;
        self
    }

    pub fn with_file_write_max_bytes(mut self, bytes: usize) -> Self {
        self.config.file_write_max_bytes = bytes;
        self
    }

    pub fn with_edit_mode(mut self, mode: EditMode) -> Self {
        self.config.edit_mode = mode;
        self
    }

    pub fn with_edit_fuzzy_match(mut self, enabled: bool) -> Self {
        self.config.edit_fuzzy_match = enabled;
        self
    }

    pub fn with_edit_fuzzy_threshold(mut self, threshold: f64) -> Self {
        self.config.edit_fuzzy_threshold = threshold;
        self
    }

    pub fn with_edit_enforce_seen_lines(mut self, enabled: bool) -> Self {
        self.config.edit_enforce_seen_lines = enabled;
        self
    }

    pub fn with_search_limits(mut self, max_files: usize, max_results: usize) -> Self {
        self.config.max_search_files = max_files;
        self.config.max_search_results = max_results;
        self
    }

    pub fn with_output_format(mut self, output_format: OutputFormat) -> Self {
        self.config.output_format = output_format;
        self
    }

    pub fn with_verbose(mut self, verbose: bool) -> Self {
        self.config.verbose = verbose;
        self
    }

    pub fn with_log_events(mut self, log_events: bool) -> Self {
        self.config.log_events = log_events;
        self
    }

    pub fn with_selected_skills<I, S>(mut self, skills: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.config.skills = skills.into_iter().map(Into::into).collect();
        self
    }

    pub fn with_skills<I, S>(self, skills: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.with_selected_skills(skills)
    }

    pub fn with_mission_file(mut self, mission_file: impl Into<PathBuf>) -> Self {
        self.config.mission_file = Some(mission_file.into());
        self
    }

    pub fn with_mission_content(mut self, mission_content: impl Into<String>) -> Self {
        self.config.mission_content = Some(mission_content.into());
        self
    }

    /// Set sandbox configuration on the embedded runtime config.
    ///
    /// This does not sandbox the current process. Full process isolation still
    /// requires an executable boundary: call `sandbox::reexec_in_sandbox()` from
    /// a CLI/SDK process or a private hidden worker before starting the runtime.
    pub fn with_sandbox(mut self, sandbox: SandboxConfig) -> Self {
        self.config.sandbox = sandbox;
        self
    }

    pub fn with_sandbox_python(mut self, sandbox_python: SandboxPythonConfig) -> Self {
        self.config.sandbox_python = sandbox_python;
        self
    }

    /// Restrict execution to exactly the provided tools.
    ///
    /// Passing an empty list disables all tools. `None` uses the catalog's
    /// default tool set; explicitly listing `PythonSandbox` opts into it.
    pub fn with_enabled_tools(mut self, tools: impl Into<Vec<String>>) -> Self {
        self.config.enabled_tools = Some(tools.into());
        self
    }

    pub fn with_default_tools(mut self) -> Self {
        self.config.enabled_tools = None;
        self
    }

    pub fn with_tool_approval_mode(mut self, mode: ToolApprovalMode) -> Self {
        self.config.tool_approval_mode = mode;
        self
    }

    pub fn with_tool_approval(mut self, approval: BTreeMap<String, ToolApprovalPolicy>) -> Self {
        self.config.tool_approval = approval;
        self
    }

    pub fn with_tool_approval_policy(
        mut self,
        tool_name: impl Into<String>,
        policy: ToolApprovalPolicy,
    ) -> Self {
        self.config.tool_approval.insert(tool_name.into(), policy);
        self
    }

    pub fn into_runtime_config(mut self) -> AgentRuntimeConfig {
        if !self.session_overridden {
            self.session = session_policy_from_config(&self.config);
        }
        if !self.first_prompt_overridden {
            self.first_prompt = first_prompt_from_config(&self.config);
        }
        AgentRuntimeConfig {
            config: self.config,
            home: self.home,
            cwd: self.cwd,
            session: self.session,
            session_layout: self.session_layout,
            first_prompt: self.first_prompt,
            display: self.display,
            event_sink: self.event_sink,
            sub_stream_tx: self.sub_stream_tx,
            read_only_fs: self.read_only_fs,
            resource_handlers: self.resource_handlers,
            skill_providers: self.skill_providers,
            runtime_skills: self.runtime_skills,
            skill_discovery_policy: self.skill_discovery_policy,
            llm_backend: self.llm_backend,
            resource_session_id: self.resource_session_id,
        }
    }
}

impl TryFrom<AgentOptions> for AgentRuntimeConfig {
    type Error = anyhow::Error;

    fn try_from(options: AgentOptions) -> Result<Self> {
        Ok(options.into_runtime_config())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn options_convert_to_runtime_config_without_losing_config_fields() {
        let runtime_config = AgentOptions::new("/tmp/mink-home", "/tmp/project")
            .with_model("pro")
            .with_api_key("test-key")
            .with_base_url("https://example.invalid/v1")
            .with_session(SessionPolicy::UseOrCreate("work".to_string()))
            .with_max_tokens(123)
            .with_max_turns(7)
            .with_max_context_tokens(456)
            .with_context_compact_pct(70)
            .with_context_reserve_tokens(71)
            .with_context_compact_tail_tokens(72)
            .with_context_compact_max_output_tokens(73)
            .with_context_compact_input_reduction(true)
            .with_tool_timeout_secs(10)
            .with_sub_agent_timeout_secs(11)
            .with_llm_timeouts(12, 13, 14)
            .with_tool_result_max_bytes(15)
            .with_file_write_max_bytes(16)
            .with_search_limits(17, 18)
            .with_output_format(OutputFormat::StreamJson)
            .with_verbose(true)
            .with_log_events(false)
            .with_openai_reasoning_effort("high")
            .with_openai_include_usage(false)
            .with_openai_token_param(TokenParamKind::MaxCompletionTokens)
            .with_openai_tool_choice("auto")
            .with_openai_extra_body(BTreeMap::from([(
                "enable_thinking".to_string(),
                serde_json::json!(true),
            )]))
            .with_runtime_skill_content("runtime-rust", "Runtime Rust", "runtime body")
            .with_skill_discovery_policy(SkillDiscoveryPolicy::RuntimeOnly)
            .with_selected_skills(["runtime-rust"])
            .with_mission_content("mission")
            .with_enabled_tools(vec!["Read".to_string(), "Bash".to_string()])
            .with_tool_approval_mode(ToolApprovalMode::Write)
            .with_tool_approval_policy("Bash", ToolApprovalPolicy::Prompt)
            .with_first_prompt("hello")
            .into_runtime_config();

        assert_eq!(runtime_config.home, PathBuf::from("/tmp/mink-home"));
        assert_eq!(runtime_config.cwd, PathBuf::from("/tmp/project"));
        assert_eq!(runtime_config.session_layout, SessionLayout::Isolated);
        assert_eq!(
            runtime_config.skill_discovery_policy,
            SkillDiscoveryPolicy::RuntimeOnly
        );
        assert_eq!(runtime_config.runtime_skills.len(), 1);
        assert_eq!(runtime_config.runtime_skills[0].name, "runtime-rust");
        assert!(matches!(
            runtime_config.session,
            SessionPolicy::UseOrCreate(ref value) if value == "work"
        ));
        assert_eq!(runtime_config.first_prompt.as_deref(), Some("hello"));

        let cfg = runtime_config.config;
        assert_eq!(cfg.model, "pro");
        assert_eq!(cfg.api_key, "test-key");
        assert_eq!(cfg.base_url, "https://example.invalid/v1");
        assert_eq!(cfg.max_tokens, 123);
        assert_eq!(cfg.max_turns, 7);
        assert_eq!(cfg.max_context_tokens, 456);
        assert_eq!(cfg.context_compact_pct, 70);
        assert_eq!(cfg.context_reserve_tokens, 71);
        assert_eq!(cfg.context_compact_tail_tokens, 72);
        assert_eq!(cfg.context_compact_max_output_tokens, 73);
        assert!(cfg.context_compact_input_reduction);
        assert_eq!(cfg.tool_timeout_secs, 10);
        assert_eq!(cfg.sub_agent_timeout_secs, 11);
        assert_eq!(cfg.llm_first_event_timeout_secs, 12);
        assert_eq!(cfg.llm_idle_timeout_secs, 13);
        assert_eq!(cfg.llm_wait_heartbeat_secs, 14);
        assert_eq!(cfg.tool_result_max_bytes, 15);
        assert_eq!(cfg.file_write_max_bytes, 16);
        assert_eq!(cfg.max_search_files, 17);
        assert_eq!(cfg.max_search_results, 18);
        assert_eq!(cfg.output_format, OutputFormat::StreamJson);
        assert!(cfg.verbose);
        assert!(!cfg.log_events);
        assert_eq!(cfg.openai_reasoning_effort.as_deref(), Some("high"));
        assert!(!cfg.openai_include_usage);
        assert_eq!(
            cfg.openai_token_param,
            OpenAiTokenParamConfig::MaxCompletionTokens
        );
        assert_eq!(cfg.openai_tool_choice, Some(serde_json::json!("auto")));
        assert_eq!(
            cfg.openai_extra_body["enable_thinking"],
            serde_json::json!(true)
        );
        assert_eq!(cfg.skills, vec!["runtime-rust"]);
        assert_eq!(cfg.mission_content.as_deref(), Some("mission"));
        assert_eq!(
            cfg.enabled_tools,
            Some(vec!["Read".to_string(), "Bash".to_string()])
        );
        assert_eq!(cfg.tool_approval_mode, ToolApprovalMode::Write);
        assert_eq!(cfg.tool_approval["Bash"], ToolApprovalPolicy::Prompt);
    }

    #[test]
    fn options_with_skills_remains_selected_skills_alias() {
        let runtime_config = AgentOptions::new("/tmp/mink-home", "/tmp/project")
            .with_skills(["rust", "debugging"])
            .into_runtime_config();

        assert_eq!(runtime_config.config.skills, vec!["rust", "debugging"]);
    }

    #[test]
    fn options_config_mut_is_lossless_escape_hatch() {
        let mut options = AgentOptions::new("/tmp/mink-home", "/tmp/project");
        options.config_mut().prompt = "from config mut".to_string();
        options.config_mut().agent_jsonl = true;
        options.config_mut().session_id = "via-config-mut".to_string();
        let runtime_config = options.into_runtime_config();

        assert_eq!(runtime_config.config.prompt, "from config mut");
        assert_eq!(
            runtime_config.first_prompt.as_deref(),
            Some("from config mut")
        );
        assert!(runtime_config.config.agent_jsonl);
        assert!(matches!(
            runtime_config.session,
            SessionPolicy::UseOrCreate(ref value) if value == "via-config-mut"
        ));
    }

    #[test]
    fn options_can_disable_openai_reasoning_effort() {
        let runtime_config = AgentOptions::new("/tmp/mink-home", "/tmp/project")
            .without_openai_reasoning_effort()
            .into_runtime_config();

        assert_eq!(runtime_config.config.openai_reasoning_effort, None);
    }

    #[test]
    fn options_explicit_first_prompt_overrides_config_prompt() {
        let mut options =
            AgentOptions::new("/tmp/mink-home", "/tmp/project").with_first_prompt("metadata only");
        options.config_mut().prompt = "turn prompt".to_string();
        let runtime_config = options.into_runtime_config();

        assert_eq!(runtime_config.config.prompt, "turn prompt");
        assert_eq!(
            runtime_config.first_prompt.as_deref(),
            Some("metadata only")
        );
    }

    #[test]
    fn options_from_config_preserves_session_selection_semantics() {
        let cfg = Config {
            session_id: "existing-or-alias".to_string(),
            ..Config::default()
        };
        let runtime_config =
            AgentOptions::from_config(cfg, "/tmp/mink-home", "/tmp/project").into_runtime_config();
        assert!(matches!(
            runtime_config.session,
            SessionPolicy::UseOrCreate(ref value) if value == "existing-or-alias"
        ));

        let cfg = Config {
            continue_session: true,
            ..Config::default()
        };
        let runtime_config =
            AgentOptions::from_config(cfg, "/tmp/mink-home", "/tmp/project").into_runtime_config();
        assert!(matches!(
            runtime_config.session,
            SessionPolicy::ContinueLatest
        ));
    }

    #[test]
    fn options_explicit_session_overrides_config_session_fields() {
        let cfg = Config {
            session_id: "from-config".to_string(),
            ..Config::default()
        };
        let runtime_config = AgentOptions::from_config(cfg, "/tmp/mink-home", "/tmp/project")
            .with_session(SessionPolicy::New)
            .into_runtime_config();

        assert!(matches!(runtime_config.session, SessionPolicy::New));
    }

    #[test]
    fn options_can_override_session_layout() {
        let runtime_config = AgentOptions::new("/tmp/mink-home", "/tmp/project")
            .with_home_scoped_sessions()
            .into_runtime_config();
        assert_eq!(runtime_config.session_layout, SessionLayout::HomeScoped);

        let runtime_config = AgentOptions::new("/tmp/mink-home", "/tmp/project")
            .with_project_scoped_sessions()
            .into_runtime_config();
        assert_eq!(runtime_config.session_layout, SessionLayout::ProjectScoped);

        let runtime_config = AgentOptions::new("/tmp/mink-home", "/tmp/project")
            .with_direct_sessions()
            .into_runtime_config();
        assert_eq!(runtime_config.session_layout, SessionLayout::Direct);
    }
}
