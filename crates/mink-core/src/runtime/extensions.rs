//! Public extension points for embedders.
//!
//! These traits let a host application influence the runtime without touching
//! the agent loop:
//!
//! - [`PrefixSource`] replaces the compiled immutable prefix (system prompt +
//!   tool schemas) with values supplied by the host, e.g. a prompt restored
//!   from a session's `prefix_snapshot` event.
//! - [`PostInitHook`] runs once after the session context is built and before
//!   the first LLM request, with a read-only [`PostInitContext`] view of the
//!   resolved system prompt, tool schemas, capability fingerprints and an
//!   event appender — enough to restructure the session directory on disk.

use crate::capabilities::CapabilitySnapshot;
use crate::session::paths::Paths;
use serde_json::Value;
use std::path::Path;

/// Supplies an alternative immutable prefix (system prompt + tool schemas)
/// that replaces the compiled prompt document.
///
/// Consulted on every prefix build before falling back to compilation; a
/// `None` return keeps the compiled prefix. `events_path` is the session's
/// `events.jsonl`, the canonical place for hosts to persist a restored
/// prefix (see the `prefix_snapshot` event shape).
pub trait PrefixSource: Send + Sync {
    fn prefix(&self, events_path: &Path) -> Option<(String, Vec<Value>)>;
}

/// Runs once after the runtime session context is built and before the first
/// LLM request.
///
/// The hook receives a read-only [`PostInitContext`] with the resolved system
/// prompt, tool schemas, capability fingerprints and an event appender, so it
/// can restructure the session directory or record lifecycle events. Errors
/// propagate and fail runtime startup (fail closed).
pub trait PostInitHook: Send + Sync {
    fn run(&self, ctx: &PostInitContext<'_>) -> anyhow::Result<()>;
}

/// Read-only view handed to [`PostInitHook`].
pub struct PostInitContext<'a> {
    pub(crate) paths: &'a Paths,
    pub(crate) cwd: &'a Path,
    pub(crate) system_prompt: &'a str,
    pub(crate) tools: &'a [Value],
    pub(crate) capabilities: &'a CapabilitySnapshot,
    pub(crate) workflow_ids: &'a [String],
    pub(crate) workflow_fingerprint: &'a str,
    pub(crate) tool_surface_fingerprint: &'a str,
    pub(crate) tool_capabilities_fingerprint: &'a str,
    pub(crate) dependency_fingerprint: &'a str,
    /// `+ Sync` keeps `&dyn Fn` Send so the context can live across await
    /// points inside the runtime builder.
    pub(crate) log_event: &'a (dyn Fn(Value) -> anyhow::Result<()> + Sync),
}

impl PostInitContext<'_> {
    /// Concrete session paths (session dir, conversation, events, ...).
    pub fn session_paths(&self) -> &Paths {
        self.paths
    }

    /// Working directory of the agent.
    pub fn cwd(&self) -> &Path {
        self.cwd
    }

    /// The resolved system prompt the runtime will use for main-agent
    /// requests unless a [`PrefixSource`] overrides it.
    pub fn system_prompt(&self) -> &str {
        self.system_prompt
    }

    /// Tool schemas of the resolved model tool surface.
    pub fn tools(&self) -> &[Value] {
        self.tools
    }

    /// Capability snapshot (skills, context files, rules) of this session.
    pub fn capabilities(&self) -> &CapabilitySnapshot {
        self.capabilities
    }

    /// Active prompt workflow ids in declaration order.
    pub fn workflow_ids(&self) -> &[String] {
        self.workflow_ids
    }

    pub fn workflow_fingerprint(&self) -> &str {
        self.workflow_fingerprint
    }

    pub fn tool_surface_fingerprint(&self) -> &str {
        self.tool_surface_fingerprint
    }

    pub fn tool_capabilities_fingerprint(&self) -> &str {
        self.tool_capabilities_fingerprint
    }

    pub fn dependency_fingerprint(&self) -> &str {
        self.dependency_fingerprint
    }

    /// Append a raw JSON event line to the session's `events.jsonl` through
    /// the runtime's serialized event writer.
    pub fn log_event(&self, event: Value) -> anyhow::Result<()> {
        (self.log_event)(event)
    }
}
