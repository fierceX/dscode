mod builder;
mod config;
pub(crate) mod context_build;
mod events;
mod handle;
mod options;
mod sdk_adapter;

pub use crate::session::paths::SessionLayout;
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
pub use options::AgentOptions;
pub use sdk_adapter::{
    apply_sdk_request_options, exit_code_from_turn, final_from_outcome, final_from_run_result,
    sdk_status_from_turn,
};

pub use crate::agent::orchestrator::TurnStatus;
