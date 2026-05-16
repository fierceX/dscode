pub mod scavenge;
pub mod flatten;

pub use scavenge::{
    scavenge_tool_calls, scavenge_combined, repair_truncated_json,
    ToolCallInfo, TruncationResult,
};
