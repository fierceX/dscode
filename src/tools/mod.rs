pub mod bash;
pub mod file;
pub mod python;
pub mod runner;
pub mod search;
pub mod web;

/// Returns true if the tool modifies external state (filesystem, shell environment).
/// Used by StormBreaker: mutating calls clear the read-only storm window
/// so a post-edit verify-read isn't flagged as a repeat.
pub fn is_tool_mutating(name: &str) -> bool {
    runner::tool_registry()
        .into_iter()
        .any(|tool| tool.name() == name && tool.mutating())
}

/// Returns true if the tool should be exempt from storm-breaker suppression.
/// State-inspection / external-data tools that shouldn't trip repeat-loop guards.
pub fn is_storm_exempt(name: &str) -> bool {
    runner::tool_registry()
        .into_iter()
        .any(|tool| tool.name() == name && tool.storm_exempt())
}
