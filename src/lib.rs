// Core modules (unchanged)
pub mod assets;
pub mod config;
pub mod errors;
pub mod events;
pub mod prompt;
pub mod protocol;
pub mod safety;
pub mod sse;

// New async modules
pub mod cancel;
pub mod context;
pub mod llm;
pub mod session;

pub mod guard;
pub mod repair;
pub mod tools;

pub mod agent;
pub mod tui;
pub mod ui;
pub mod util;

#[cfg(test)]
pub mod regression;

#[cfg(test)]
pub mod test_mock;
