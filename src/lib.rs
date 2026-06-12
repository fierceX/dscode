//! Public library entry points for embedding mink.
//!
//! Stable embedding code should prefer `mink::runtime`, `mink::config`,
//! `mink::sandbox`, and `mink::sdk_protocol`. Other public modules are kept
//! visible for the existing binaries and integration tests, but are internal
//! implementation details and may change as the library API is tightened.

// Stable and semi-stable public modules.
pub mod config;
pub mod runtime;
pub mod sandbox;
pub mod sdk_protocol;
pub mod ui;

// Internal modules kept public during the library transition.
#[doc(hidden)]
pub mod assets;
#[doc(hidden)]
pub mod cli;
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
pub mod context;
#[doc(hidden)]
pub mod llm;
#[doc(hidden)]
pub mod session;
#[doc(hidden)]
pub mod skills;

#[doc(hidden)]
pub mod guard;
#[doc(hidden)]
pub mod repair;
#[doc(hidden)]
pub mod tools;

#[doc(hidden)]
pub mod agent;
#[cfg(feature = "tui")]
#[doc(hidden)]
pub mod tui;
#[doc(hidden)]
pub mod util;

#[cfg(test)]
pub mod regression;

#[cfg(test)]
pub mod test_mock;
