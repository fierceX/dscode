mod builder;
mod config;
pub(crate) mod context_build;
mod events;
mod handle;
pub mod llm;
mod options;
mod sdk_adapter;

pub use crate::capabilities::{
    CapabilityExposure, LoadedSkill, RuntimeSkill, SkillCapability, SkillDiscoveryPolicy,
    SkillLoadContext, SkillProvider, SourceLevel, SourceMeta,
};
pub use crate::resources::ResourceHandler;
pub use crate::session::paths::SessionLayout;
pub use crate::tools::vfs::{
    ReadOnlyFileSystem, VfsGlobRequest, VfsGlobResult, VfsGrepEntry, VfsGrepRequest, VfsGrepResult,
    VfsReadRequest, VfsReadResult, VfsScope, VirtualFile, collect_virtual_glob,
    collect_virtual_grep, format_virtual_glob, format_virtual_grep, normalize_virtual_file_path,
    normalize_virtual_root, select_virtual_lines, tool_line_count, try_collect_virtual_glob,
    try_collect_virtual_grep, validate_virtual_glob_request, validate_virtual_grep_request,
};
/// Build a full mink runtime from configuration.
///
/// This is the library entry point used by the `mink` and `mink-core` binaries.
/// It initializes the same session store, artifacts, compaction engine, tool
/// configuration, cancellation token, and orchestrator used by the CLI entry
/// points. Library users should treat it as an embedded form of mink itself,
/// not as a separate implementation of agent behavior.
pub use builder::build_runtime;
pub use config::{AgentRuntimeConfig, SessionInfo, SessionPolicy};
pub use events::{AgentEvent, EventSink};
pub use handle::{AgentEventStream, AgentRuntime, TurnOutcome};
pub use llm::{
    BackendLlmClient, LlmBackend, LlmCancelToken, LlmErrorEvent, LlmEvent, LlmEventStream,
    LlmPurpose, LlmRequest, LlmRequestFailure, LlmResponseStream, LlmRetryEvent, LlmStopEvent,
    LlmTextEvent, LlmThinkingEvent, LlmToolCallEvent, LlmUsageEvent, OpenAiCompatibleBackend,
    OpenAiCompatibleOptions, TokenParamKind,
};
pub use options::AgentOptions;
pub use sdk_adapter::{
    apply_sdk_request_options, exit_code_from_turn, final_from_outcome,
    runtime_skills_from_sdk_request, sdk_status_from_turn, skill_discovery_policy_from_sdk_request,
};

pub use crate::agent::orchestrator::TurnStatus;
