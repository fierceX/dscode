use crate::capabilities::{CapabilityExposure, RuntimeSkill, SkillDiscoveryPolicy, SkillProvider};
use crate::config::{
    EditMode, OutputFormat, ResolvedConfig as Config, SandboxConfig, SandboxPythonConfig,
    ToolApprovalMode, ToolApprovalPolicy,
};
use crate::llm::client::{LlmBackend, TokenParamKind};
use crate::resources::ResourceHandler;
use crate::runtime::config::{
    AgentRuntimeConfig, first_prompt_from_config, session_policy_from_config,
};
use crate::runtime::extensions::{PostInitHook, PrefixSource};
use crate::runtime::{EventSink, SessionPolicy};
use crate::session::paths::SessionLayout;
use crate::tools::vfs::ReadOnlyFileSystem;
use crate::ui::SubAgentStreamSink;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

#[derive(Debug, Clone)]
pub struct ProviderOptions {
    pub model: String,
    pub model_aliases: BTreeMap<String, String>,
    pub api_key: String,
    pub base_url: String,
    pub reasoning_effort: Option<String>,
    pub include_usage: bool,
    pub token_param: TokenParamKind,
    pub tool_choice: Option<serde_json::Value>,
    pub extra_body: BTreeMap<String, serde_json::Value>,
}

#[derive(Debug, Clone)]
pub struct GenerationOptions {
    pub max_tokens: i32,
    pub max_turns: i32,
    pub first_event_timeout_secs: i32,
    pub idle_timeout_secs: i32,
    pub wait_heartbeat_secs: i32,
}

#[derive(Debug, Clone)]
pub struct ContextPolicy {
    pub max_context_tokens: usize,
    pub compact_pct: u8,
    pub reserve_tokens: usize,
    pub compact_tail_tokens: usize,
    pub compact_max_output_tokens: i32,
    pub compact_input_reduction: bool,
    pub plan_projection_tail: bool,
}

#[derive(Debug, Clone)]
pub struct ToolOptions {
    pub timeout_secs: i32,
    /// 单次 Bash/Python/自定义工具调用的超时上限（默认 600 秒）。
    pub timeout_max_secs: i32,
    pub sub_agent_timeout_secs: i32,
    pub result_max_bytes: usize,
    pub file_write_max_bytes: usize,
    pub edit_mode: EditMode,
    pub edit_fuzzy_match: bool,
    pub edit_fuzzy_threshold: f64,
    pub edit_enforce_seen_lines: bool,
    pub max_search_files: usize,
    pub max_search_results: usize,
    pub enabled_tools: Option<Vec<String>>,
    pub approval_mode: ToolApprovalMode,
    pub approval: BTreeMap<String, ToolApprovalPolicy>,
}

impl Default for ProviderOptions {
    fn default() -> Self {
        let config = Config::default();
        Self {
            model: config.model,
            model_aliases: config.model_aliases,
            api_key: config.api_key,
            base_url: config.base_url,
            reasoning_effort: config.openai_reasoning_effort,
            include_usage: config.openai_include_usage,
            token_param: config.openai_token_param,
            tool_choice: config.openai_tool_choice,
            extra_body: config.openai_extra_body,
        }
    }
}

impl Default for GenerationOptions {
    fn default() -> Self {
        let config = Config::default();
        Self {
            max_tokens: config.max_tokens,
            max_turns: config.max_turns,
            first_event_timeout_secs: config.llm_first_event_timeout_secs,
            idle_timeout_secs: config.llm_idle_timeout_secs,
            wait_heartbeat_secs: config.llm_wait_heartbeat_secs,
        }
    }
}

impl Default for ContextPolicy {
    fn default() -> Self {
        let config = Config::default();
        Self {
            max_context_tokens: config.max_context_tokens,
            compact_pct: config.context_compact_pct,
            reserve_tokens: config.context_reserve_tokens,
            compact_tail_tokens: config.context_compact_tail_tokens,
            compact_max_output_tokens: config.context_compact_max_output_tokens,
            compact_input_reduction: config.context_compact_input_reduction,
            plan_projection_tail: config.plan_projection_tail,
        }
    }
}

impl Default for ToolOptions {
    fn default() -> Self {
        let config = Config::default();
        Self {
            timeout_secs: config.tool_timeout_secs,
            timeout_max_secs: config.tool_timeout_max_secs,
            sub_agent_timeout_secs: config.sub_agent_timeout_secs,
            result_max_bytes: config.tool_result_max_bytes,
            file_write_max_bytes: config.file_write_max_bytes,
            edit_mode: config.edit_mode,
            edit_fuzzy_match: config.edit_fuzzy_match,
            edit_fuzzy_threshold: config.edit_fuzzy_threshold,
            edit_enforce_seen_lines: config.edit_enforce_seen_lines,
            max_search_files: config.max_search_files,
            max_search_results: config.max_search_results,
            enabled_tools: config.enabled_tools,
            approval_mode: config.tool_approval_mode,
            approval: config.tool_approval,
        }
    }
}

/// Ergonomic builder for embedding mink from Rust.
///
/// This is the single public configuration entry point for [`AgentRuntime`](crate::runtime::AgentRuntime).
/// Runtime policy is applied through grouped option values and typed extension methods.
pub struct AgentOptions {
    config: Config,
    home: PathBuf,
    cwd: PathBuf,
    session: SessionPolicy,
    session_layout: SessionLayout,
    session_overridden: bool,
    first_prompt: Option<String>,
    first_prompt_overridden: bool,
    event_sink: Option<Arc<dyn EventSink>>,
    sub_stream_tx: Option<Arc<dyn SubAgentStreamSink>>,
    read_only_fs: Option<Arc<dyn ReadOnlyFileSystem>>,
    resource_handlers: Vec<Arc<dyn ResourceHandler>>,
    skill_providers: Vec<Arc<dyn SkillProvider>>,
    runtime_skills: Vec<RuntimeSkill>,
    skill_discovery_policy: SkillDiscoveryPolicy,
    llm_backend: Option<Arc<dyn LlmBackend>>,
    resource_session_id: Option<String>,
    custom_tools: Vec<Arc<dyn crate::runtime::AgentTool>>,
    prefix_source: Option<Arc<dyn PrefixSource>>,
    post_init_hook: Option<Arc<dyn PostInitHook>>,
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
    /// Provider defaults, config files, and environment merging are not applied here.
    pub fn new(home: impl Into<PathBuf>, cwd: impl Into<PathBuf>) -> Self {
        let config = Config::default();
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
            prefix_source: None,
            post_init_hook: None,
        }
    }

    pub fn with_provider_options(mut self, options: ProviderOptions) -> Self {
        self.config.model = options.model;
        self.config.model_aliases = options.model_aliases;
        self.config.api_key = options.api_key;
        self.config.base_url = options.base_url;
        self.config.openai_reasoning_effort = options.reasoning_effort;
        self.config.openai_include_usage = options.include_usage;
        self.config.openai_token_param = options.token_param;
        self.config.openai_tool_choice = options.tool_choice;
        self.config.openai_extra_body = options.extra_body;
        self
    }

    pub fn with_generation_options(mut self, options: GenerationOptions) -> Self {
        self.config.max_tokens = options.max_tokens;
        self.config.max_turns = options.max_turns;
        self.config.llm_first_event_timeout_secs = options.first_event_timeout_secs;
        self.config.llm_idle_timeout_secs = options.idle_timeout_secs;
        self.config.llm_wait_heartbeat_secs = options.wait_heartbeat_secs;
        self
    }

    pub fn with_context_policy(mut self, policy: ContextPolicy) -> Self {
        self.config.max_context_tokens = policy.max_context_tokens;
        self.config.context_compact_pct = policy.compact_pct;
        self.config.context_reserve_tokens = policy.reserve_tokens;
        self.config.context_compact_tail_tokens = policy.compact_tail_tokens;
        self.config.context_compact_max_output_tokens = policy.compact_max_output_tokens;
        self.config.context_compact_input_reduction = policy.compact_input_reduction;
        self.config.plan_projection_tail = policy.plan_projection_tail;
        self
    }

    pub fn with_tool_options(mut self, options: ToolOptions) -> Self {
        self.config.tool_timeout_secs = options.timeout_secs;
        self.config.tool_timeout_max_secs = options.timeout_max_secs;
        self.config.sub_agent_timeout_secs = options.sub_agent_timeout_secs;
        self.config.tool_result_max_bytes = options.result_max_bytes;
        self.config.file_write_max_bytes = options.file_write_max_bytes;
        self.config.edit_mode = options.edit_mode;
        self.config.edit_fuzzy_match = options.edit_fuzzy_match;
        self.config.edit_fuzzy_threshold = options.edit_fuzzy_threshold;
        self.config.edit_enforce_seen_lines = options.edit_enforce_seen_lines;
        self.config.max_search_files = options.max_search_files;
        self.config.max_search_results = options.max_search_results;
        self.config.enabled_tools = options.enabled_tools;
        self.config.tool_approval_mode = options.approval_mode;
        self.config.tool_approval = options.approval;
        self
    }

    pub fn with_signal_policy(mut self, policy: crate::config::SignalPolicy) -> Self {
        self.config.signal_policy = policy;
        self
    }

    pub fn with_interactive(mut self, interactive: bool) -> Self {
        self.config.interactive = interactive;
        self
    }

    pub fn with_model_alias(mut self, alias: impl Into<String>, model: impl Into<String>) -> Self {
        self.config.model_aliases.insert(alias.into(), model.into());
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

    /// Declare the image-input capability explicitly (v7 §3.1).
    ///
    /// Priority is explicit config > backend declaration > Unsupported.
    /// Without this, image reading stays disabled even for vision-capable
    /// endpoints unless the backend declares it.
    pub fn with_image_input(
        mut self,
        capability: crate::capabilities::model_capabilities::ImageInputCapability,
    ) -> Self {
        self.config.image_input = Some(capability);
        self
    }

    /// Replace the backend-declared vision model list (v7 §3.1). An empty
    /// list disables image capture for every model; the default built-in list
    /// is `deepseek-v4-flash-vision-exp`.
    pub fn with_vision_models(mut self, models: Vec<String>) -> Self {
        self.config.vision_models = models;
        self
    }

    /// The currently configured vision model list (built-in defaults unless
    /// replaced via `with_vision_models`). Used by CLI layers that construct
    /// their own backend (e.g. `--router`) so user configuration is honored.
    pub fn vision_models(&self) -> &[String] {
        &self.config.vision_models
    }

    pub fn with_openai_reasoning_effort(mut self, effort: impl Into<String>) -> Self {
        let effort = effort.into();
        self.config.openai_reasoning_effort = Some(effort);
        self
    }

    pub fn with_openai_include_usage(mut self, include_usage: bool) -> Self {
        self.config.openai_include_usage = include_usage;
        self
    }

    pub fn with_openai_token_param(mut self, token_param: TokenParamKind) -> Self {
        self.config.openai_token_param = token_param;
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

    pub fn with_tool(mut self, tool: Arc<dyn crate::runtime::AgentTool>) -> Self {
        self.custom_tools.push(tool);
        self
    }

    /// Supply an alternative immutable prefix (system prompt + tool schemas)
    /// consulted before the compiled prompt on every prefix build.
    pub fn with_prefix_source(mut self, source: Arc<dyn PrefixSource>) -> Self {
        self.prefix_source = Some(source);
        self
    }

    /// Run a hook once after the session context is built, before the first
    /// LLM request, with a read-only view of the resolved prompt/tools and an
    /// event appender (see [`PostInitContext`]).
    pub fn with_post_init_hook(mut self, hook: Arc<dyn PostInitHook>) -> Self {
        self.post_init_hook = Some(hook);
        self
    }

    pub fn with_tools<I>(mut self, tools: I) -> Self
    where
        I: IntoIterator<Item = Arc<dyn crate::runtime::AgentTool>>,
    {
        self.custom_tools.extend(tools);
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
        self.config.context_compact_pct = pct;
        self
    }

    pub fn with_context_reserve_tokens(mut self, tokens: usize) -> Self {
        self.config.context_reserve_tokens = tokens;
        self
    }

    pub fn with_context_compact_tail_tokens(mut self, tokens: usize) -> Self {
        self.config.context_compact_tail_tokens = tokens;
        self
    }

    pub fn with_context_compact_max_output_tokens(mut self, tokens: i32) -> Self {
        self.config.context_compact_max_output_tokens = tokens;
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

    /// 设置单次 Bash/Python/自定义工具调用的超时上限（默认 600 秒，至少 5 秒）。
    pub fn with_tool_timeout_max_secs(mut self, secs: i32) -> Self {
        self.config.tool_timeout_max_secs = secs;
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

    pub fn with_tool_approval_mode(mut self, mode: ToolApprovalMode) -> Self {
        self.config.tool_approval_mode = mode;
        self
    }

    pub(crate) fn into_runtime_config(mut self) -> AgentRuntimeConfig {
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
            event_sink: self.event_sink,
            sub_stream_tx: self.sub_stream_tx,
            read_only_fs: self.read_only_fs,
            resource_handlers: self.resource_handlers,
            skill_providers: self.skill_providers,
            runtime_skills: self.runtime_skills,
            skill_discovery_policy: self.skill_discovery_policy,
            llm_backend: self.llm_backend,
            resource_session_id: self.resource_session_id,
            custom_tools: self.custom_tools,
            prefix_source: self.prefix_source,
            post_init_hook: self.post_init_hook,
        }
    }
}

#[cfg(test)]
#[path = "options_tests.rs"]
mod tests;
