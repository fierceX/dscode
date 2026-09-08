//! Public library entry points for embedding mink.
//!
//! The public surface is deliberately limited to [`runtime`], [`prelude`], and
//! [`sdk_protocol`].

pub mod runtime;
pub mod sdk_protocol;

/// Common imports for embedded Rust services.
///
/// This facade is the intended stable surface for services that embed mink as
/// a library. It deliberately re-exports runtime types only; lower-level agent,
/// tool, session, and LLM modules remain implementation details shared by the
/// CLI binaries and the library runtime.
pub mod prelude {
    pub use crate::runtime::session::{
        TokenUsage, UsageKind, UsageRecord, UsageStatus, UsageSummary,
    };
    pub use crate::runtime::{
        AgentEvent, AgentEventKind, AgentEventStream, AgentOptions, AgentRuntime,
        AgentRuntimeHandle, AgentTool, ApprovalTier, CapabilityExposure, CompactOutcome,
        ContextPolicy, EditMode, EventSink, GenerationOptions, LlmBackend, LlmCacheProjection,
        LlmCancelToken, LlmErrorEvent, LlmEvent, LlmEventStream, LlmPurpose, LlmRequest,
        LlmRequestFailure, LlmResponseStream, LlmRetryEvent, LlmStopEvent, LlmTextEvent,
        LlmThinkingEvent, LlmToolCallEvent, LlmUsageEvent, LoadedSkill, ModelResolver,
        OpenAiCompatibleBackend, OpenAiCompatibleOptions, OutputFormat, PostInitContext,
        PostInitHook, PrefixSource, ProviderOptions, ReadOnlyFileSystem, ResolvedModel,
        ResourceHandler, RuntimeError, RuntimeSkill, SandboxConfig, SandboxPythonConfig,
        SessionInfo, SessionLayout, SessionPolicy, SignalPolicy, SkillCapability,
        SkillDiscoveryPolicy, SkillLoadContext, SkillProvider, SourceLevel, SourceMeta,
        TokenParamKind, ToolActivation, ToolApprovalMode, ToolApprovalPolicy, ToolCapabilityOffer,
        ToolDefinition, ToolError, ToolExecutionContext, ToolExecutionMode, ToolOptions,
        ToolOutput, ToolSemanticCapability, TurnId, TurnOutcome, TurnStatus, VfsGlobRequest,
        VfsGlobResult, VfsGrepEntry, VfsGrepRequest, VfsGrepResult, VfsReadRequest, VfsReadResult,
        VfsScope,
    };
}

mod agent;
mod assets;
mod cancel;
mod capabilities;
#[allow(dead_code)]
mod config;
mod context;
mod errors;
mod events;
mod guard;
mod llm;
mod prompt;
mod protocol;
mod repair;
mod resources;
mod safety;
mod sandbox;
mod session;
mod sse;
mod tools;
pub mod ui;

#[cfg(test)]
mod regression;
