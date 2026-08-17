use super::*;
use crate::config::{ResolvedConfig as Config, ToolApprovalMode, ToolApprovalPolicy};
use crate::context::ToolConfig;
use crate::tools::metadata::{ApprovalTier, ToolMetadata, ToolResultKind};

fn metadata(tier: ApprovalTier) -> ToolMetadata {
    ToolMetadata::new("Example", tier, ToolResultKind::Text)
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
