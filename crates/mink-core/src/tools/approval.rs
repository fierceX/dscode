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
mod tests {
    use super::*;
    use crate::config::{ResolvedConfig as Config, ToolApprovalMode, ToolApprovalPolicy};
    use crate::context::ToolConfig;
    use crate::tools::metadata::{ApprovalTier, ToolMetadata, ToolResultKind};

    fn metadata(tier: ApprovalTier) -> ToolMetadata {
        ToolMetadata::new("Example", "test", tier, ToolResultKind::Text)
    }

    #[test]
    fn authorization_table_is_exact() {
        let mut config = ToolConfig::from_config(&Config::default());
        for tier in [ApprovalTier::Read, ApprovalTier::Write, ApprovalTier::Exec] {
            assert_eq!(
                authorize_tool(&metadata(tier), &config),
                ToolAuthorization::Allowed
            );
        }
        config.tool_approval_mode = ToolApprovalMode::Write;
        assert_eq!(
            authorize_tool(&metadata(ApprovalTier::Read), &config),
            ToolAuthorization::Allowed
        );
        assert_eq!(
            authorize_tool(&metadata(ApprovalTier::Write), &config),
            ToolAuthorization::Allowed
        );
        assert!(matches!(
            authorize_tool(&metadata(ApprovalTier::Exec), &config),
            ToolAuthorization::Denied { .. }
        ));
        config.tool_approval_mode = ToolApprovalMode::AlwaysAsk;
        assert_eq!(
            authorize_tool(&metadata(ApprovalTier::Read), &config),
            ToolAuthorization::Allowed
        );
        assert!(matches!(
            authorize_tool(&metadata(ApprovalTier::Write), &config),
            ToolAuthorization::Denied { .. }
        ));

        config
            .tool_approval
            .insert("Example".into(), ToolApprovalPolicy::Allow);
        assert_eq!(
            authorize_tool(&metadata(ApprovalTier::Exec), &config),
            ToolAuthorization::Allowed
        );
        config
            .tool_approval
            .insert("Example".into(), ToolApprovalPolicy::Deny);
        assert_eq!(
            authorize_tool(&metadata(ApprovalTier::Read), &config),
            ToolAuthorization::Denied {
                reason: ToolAuthorizationDeniedReason::ExplicitDeny
            }
        );
        config
            .tool_approval
            .insert("Example".into(), ToolApprovalPolicy::Prompt);
        assert_eq!(
            authorize_tool(&metadata(ApprovalTier::Read), &config),
            ToolAuthorization::Denied {
                reason: ToolAuthorizationDeniedReason::PromptUnavailable
            }
        );
    }
}
