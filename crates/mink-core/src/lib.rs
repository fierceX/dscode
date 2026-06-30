//! Public library entry points for embedding mink.
//!
//! Stable embedding code should prefer `mink::runtime`, `mink::config`,
//! `mink::sandbox`, `mink::sdk_protocol`, or `mink::prelude`. Other public
//! modules are kept visible for the existing binaries and integration tests,
//! but are internal implementation details and may change as the library API
//! is tightened.

// Stable and semi-stable public modules.
pub mod config;
pub mod runtime;
pub mod sandbox;
pub mod sdk_protocol;
pub mod ui;

/// Common imports for embedded Rust services.
///
/// This facade is the intended stable surface for services that embed mink as
/// a library. It deliberately re-exports runtime types only; lower-level agent,
/// tool, session, and LLM modules remain implementation details shared by the
/// CLI binaries and the library runtime.
pub mod prelude {
    pub use crate::runtime::{
        AgentEvent, AgentEventStream, AgentOptions, AgentRuntime, CapabilityExposure, EventSink,
        LlmBackend, LlmCancelToken, LlmErrorEvent, LlmEvent, LlmEventStream, LlmPurpose,
        LlmRequest, LlmRequestFailure, LlmResponseStream, LlmRetryEvent, LlmStopEvent,
        LlmTextEvent, LlmThinkingEvent, LlmToolCallEvent, LlmUsageEvent, LoadedSkill,
        OpenAiCompatibleBackend, OpenAiCompatibleOptions, ReadOnlyFileSystem, ResourceHandler,
        RuntimeSkill, SessionInfo, SessionLayout, SessionPolicy, SkillCapability,
        SkillDiscoveryPolicy, SkillLoadContext, SkillProvider, SourceLevel, SourceMeta,
        TokenParamKind, TurnOutcome, TurnStatus, VfsGlobRequest, VfsGlobResult, VfsGrepEntry,
        VfsGrepRequest, VfsGrepResult, VfsReadRequest, VfsReadResult, VfsScope, VirtualFile,
    };
    pub use crate::session::usage::{
        TokenUsage, UsageKind, UsageRecord, UsageStatus, UsageSummary,
    };
}

// Internal modules kept public during the library transition.
#[doc(hidden)]
pub mod assets;
#[doc(hidden)]
pub mod errors;
#[doc(hidden)]
pub mod events;
#[doc(hidden)]
pub mod prompt;
#[doc(hidden)]
pub mod protocol;
#[doc(hidden)]
pub mod safety;
#[doc(hidden)]
pub mod sse;

#[doc(hidden)]
pub mod cancel;
#[doc(hidden)]
pub mod capabilities;
#[doc(hidden)]
pub mod context;
#[doc(hidden)]
pub mod llm;
#[doc(hidden)]
pub mod session;

#[doc(hidden)]
pub mod guard;
#[doc(hidden)]
pub mod repair;
#[doc(hidden)]
pub mod resources;
#[doc(hidden)]
pub mod tools;

#[doc(hidden)]
pub mod agent;
#[doc(hidden)]
pub mod util;

#[cfg(test)]
pub mod regression;

#[cfg(test)]
pub mod test_mock;
