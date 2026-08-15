use crate::config::{ToolApprovalMode, ToolApprovalPolicy};
use crate::context::ToolConfig;
use crate::tools::metadata::{ApprovalTier, ToolMetadata};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolAuthorizationDeniedReason {
    ExplicitDeny,
    PromptUnavailable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolAuthorization {
    Allowed,
    Denied {
        reason: ToolAuthorizationDeniedReason,
    },
}

pub fn authorize_tool(metadata: &ToolMetadata, config: &ToolConfig) -> ToolAuthorization {
    match config.tool_approval.get(metadata.name.as_ref()).copied() {
        Some(ToolApprovalPolicy::Allow) => return ToolAuthorization::Allowed,
        Some(ToolApprovalPolicy::Deny) => {
            return ToolAuthorization::Denied {
                reason: ToolAuthorizationDeniedReason::ExplicitDeny,
            };
        }
        Some(ToolApprovalPolicy::Prompt) => {
            return ToolAuthorization::Denied {
                reason: ToolAuthorizationDeniedReason::PromptUnavailable,
            };
        }
        None => {}
    }

    let allowed = match config.tool_approval_mode {
        ToolApprovalMode::Yolo => true,
        ToolApprovalMode::Write => {
            matches!(metadata.approval, ApprovalTier::Read | ApprovalTier::Write)
        }
        ToolApprovalMode::AlwaysAsk => matches!(metadata.approval, ApprovalTier::Read),
    };
    if allowed {
        ToolAuthorization::Allowed
    } else {
        ToolAuthorization::Denied {
            reason: ToolAuthorizationDeniedReason::PromptUnavailable,
        }
    }
}

#[cfg(test)]
pub fn denied_message(metadata: &ToolMetadata, reason: ToolAuthorizationDeniedReason) -> String {
    match reason {
        ToolAuthorizationDeniedReason::ExplicitDeny => {
            format!("Tool '{}' blocked by approval policy: deny", metadata.name)
        }
        ToolAuthorizationDeniedReason::PromptUnavailable => format!(
            "Tool '{}' requires approval, but interactive approval prompts are not implemented.",
            metadata.name
        ),
    }
}

#[cfg(test)]
#[path = "approval_tests.rs"]
mod tests;
