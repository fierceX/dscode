mod builder;
mod config;
pub(crate) mod context_build;
mod events;
mod extensions;
mod handle;
mod options;
mod sdk_adapter;
pub mod session;
mod tools;
pub(crate) use tools::RegisteredCustomTool;

pub use crate::capabilities::CapabilitySnapshot;
pub use crate::capabilities::{
    CapabilityExposure, LoadedSkill, RuntimeSkill, SkillCapability, SkillDiscoveryPolicy,
    SkillLoadContext, SkillProvider, SourceLevel, SourceMeta,
};
pub use crate::config::{
    EditMode, ModelResolver, OutputFormat, ResolvedModel, SandboxConfig, SandboxPythonConfig,
    SignalPolicy, ToolApprovalMode, ToolApprovalPolicy,
};
pub use crate::llm::client::{
    LlmBackend, LlmCancelToken, LlmErrorEvent, LlmEvent, LlmEventStream, LlmPurpose, LlmRequest,
    LlmRequestFailure, LlmResponseStream, LlmRetryEvent, LlmStopEvent, LlmTextEvent,
    LlmThinkingEvent, LlmToolCallEvent, LlmUsageEvent, OpenAiCompatibleBackend,
    OpenAiCompatibleOptions, TokenParamKind,
};
pub use crate::resources::ResourceHandler;
pub use crate::runtime::extensions::{PostInitContext, PostInitHook, PrefixSource};
/// 同目录临时文件 + rename 的原子替换（session 状态文件共用实现）。
pub use crate::session::atomic_file::atomic_replace;
pub use crate::session::paths::SessionLayout;
pub use crate::capabilities::model_capabilities::{
    ImageDetail, ImageInputCapability, OpenAiChatImageUrlLimits, TokenEstimator, WireProtocol,
};
pub use crate::config::{ResolvedConfig, vision_model_defaults};
pub use crate::tools::image::ImageFormat;
pub use crate::tools::vfs::VfsImage;
pub use crate::tools::metadata::{
    ApprovalTier, ToolBlocker, ToolFailureKind, ToolResultKind, ToolStatus,
};
pub use crate::tools::semantic_capabilities::{
    CapabilityAvailability, CapabilityUseScope, ProviderTier, ToolSemanticCapability,
};
pub use crate::tools::vfs::{
    ReadOnlyFileSystem, VfsGlobRequest, VfsGlobResult, VfsGrepEntry, VfsGrepRequest, VfsGrepResult,
    VfsReadRequest, VfsReadResult, VfsScope, format_virtual_glob, format_virtual_grep,
    normalize_virtual_file_path, normalize_virtual_root, select_virtual_lines, tool_line_count,
    validate_virtual_glob_request, validate_virtual_grep_request,
};
pub use crate::ui::{
    ArtifactDisplay, PlanDisplay, PlanTransitionDisplay, PresentedToolResultDisplay, StatsSnapshot,
    SubAgentStreamKind, SubAgentStreamSink, TodoChangeDisplay, TodoCountsDisplay, TodoDisplay,
    TodoItemDisplay, TodoStatusDisplay, ToolCallDisplay, ToolPresentation, ToolResultDisplay,
};
pub(crate) use builder::build_runtime;
pub use config::{SessionInfo, SessionPolicy};
pub(crate) use events::TurnEventEmitter;
pub use events::{AgentEvent, AgentEventKind, EventSink};
pub use handle::{
    AgentEventStream, AgentRuntime, AgentRuntimeHandle, CompactOutcome, RuntimeError,
    RuntimeResult, TurnId, TurnOutcome,
};
pub use options::{AgentOptions, ContextPolicy, GenerationOptions, ProviderOptions, ToolOptions};

/// Display trait 的唯一定义（此前在 mink-cli 逐字复制一份）。
pub use crate::ui::Display;

/// 运行时限值校验的唯一公共实现（CLI 与 SDK 请求层共用）。
#[allow(clippy::too_many_arguments)]
pub fn validate_runtime_limits(
    edit_fuzzy_threshold: f64,
    max_tokens: i32,
    max_turns: i32,
    context_compact_pct: u8,
    context_reserve_tokens: usize,
    context_compact_tail_tokens: usize,
    context_compact_max_output_tokens: i32,
    max_context_tokens: usize,
) -> anyhow::Result<()> {
    crate::config::validate_runtime_limits(
        edit_fuzzy_threshold,
        max_tokens,
        max_turns,
        context_compact_pct,
        context_reserve_tokens,
        context_compact_tail_tokens,
        context_compact_max_output_tokens,
        max_context_tokens,
    )
}

pub use sdk_adapter::{
    exit_code_from_turn, final_from_outcome, runtime_skills_from_sdk_request,
    skill_discovery_policy_from_sdk_request,
};
pub use tools::{
    AgentTool, ToolActivation, ToolCapabilityOffer, ToolDefinition, ToolError,
    ToolExecutionContext, ToolExecutionMode, ToolOutput,
};

pub use crate::agent::orchestrator::TurnStatus;

/// CLI-process bootstrap. Embedded applications should configure sandboxing at
/// their own process boundary instead.
pub fn reexec_in_sandbox(config: &SandboxConfig, exe: &std::path::Path, args: &[String]) {
    crate::sandbox::reexec_in_sandbox(config, exe, args);
}
