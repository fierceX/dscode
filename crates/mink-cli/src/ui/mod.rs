#[cfg(test)]
pub use mink::runtime::ToolResultDisplay;
#[cfg(feature = "tui")]
pub use mink::runtime::{
    ArtifactDisplay, PlanDisplay, PlanTransitionDisplay, SubAgentStreamKind, TodoChangeDisplay,
    TodoCountsDisplay, TodoDisplay, TodoItemDisplay, TodoStatusDisplay, ToolPresentation,
    ToolResultKind,
};
pub use mink::runtime::{
    PresentedToolResultDisplay, StatsSnapshot, SubAgentStreamSink, ToolCallDisplay,
};

// Display trait 的唯一实现在 mink-core（此前逐字复制两份，需手工同步）。
pub use mink::runtime::Display;

pub mod engine;
pub mod replay;
