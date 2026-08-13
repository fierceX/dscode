mod builder;
mod config;
pub(crate) mod context_build;
mod events;
mod handle;
mod options;
mod sdk_adapter;
mod tools;
pub(crate) use tools::RegisteredCustomTool;

pub use crate::capabilities::{
    CapabilityExposure, LoadedSkill, RuntimeSkill, SkillCapability, SkillDiscoveryPolicy,
    SkillLoadContext, SkillProvider, SourceLevel, SourceMeta,
};
pub use crate::config::EditMode;
pub use crate::llm::client::{
    BackendLlmClient, LlmBackend, LlmCancelToken, LlmErrorEvent, LlmEvent, LlmEventStream,
    LlmPurpose, LlmRequest, LlmRequestFailure, LlmResponseStream, LlmRetryEvent, LlmStopEvent,
    LlmTextEvent, LlmThinkingEvent, LlmToolCallEvent, LlmUsageEvent, OpenAiCompatibleBackend,
    OpenAiCompatibleOptions, TokenParamKind,
};
pub use crate::resources::ResourceHandler;
pub use crate::session::paths::SessionLayout;
pub use crate::tools::metadata::ApprovalTier;
pub use crate::tools::semantic_capabilities::{
    CapabilityAvailability, CapabilityUseScope, ProviderTier, ToolSemanticCapability,
};
pub use crate::tools::vfs::{
    ReadOnlyFileSystem, VfsGlobRequest, VfsGlobResult, VfsGrepEntry, VfsGrepRequest, VfsGrepResult,
    VfsReadRequest, VfsReadResult, VfsScope, format_virtual_glob, format_virtual_grep,
    normalize_virtual_file_path, normalize_virtual_root, select_virtual_lines, tool_line_count,
    validate_virtual_glob_request, validate_virtual_grep_request,
};
pub(crate) use builder::build_runtime;
pub use config::{SessionInfo, SessionPolicy};
#[doc(hidden)]
pub use events::TurnEventEmitter;
pub use events::{AgentEvent, AgentEventKind, EventSink};
pub use handle::{
    AgentEventStream, AgentRuntime, AgentRuntimeHandle, CompactOutcome, RuntimeError,
    RuntimeResult, TurnId, TurnOutcome,
};
pub use options::AgentOptions;
pub use sdk_adapter::{
    apply_sdk_request_options, exit_code_from_turn, final_from_outcome,
    runtime_skills_from_sdk_request, sdk_status_from_turn, skill_discovery_policy_from_sdk_request,
};
pub use tools::{
    AgentTool, ToolActivation, ToolCapabilityOffer, ToolDefinition, ToolError,
    ToolExecutionContext, ToolExecutionMode, ToolOutput,
};

pub use crate::agent::orchestrator::TurnStatus;
