use crate::context::ToolContext;
use crate::tools::metadata::{ApprovalTier, ToolMetadata, ToolResultKind};
use crate::tools::runner::{ToolExec, ToolOutcome};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlanCommand {
    SetDraft,
    Confirm,
    Clear,
}

impl PlanCommand {
    pub fn compaction_trigger(self) -> Option<&'static str> {
        match self {
            Self::SetDraft => None,
            Self::Confirm => Some("plan_confirm"),
            Self::Clear => Some("plan_clear"),
        }
    }
}

pub struct PlanDraftTool;
pub struct PlanConfirmTool;
pub struct PlanClearTool;

impl ToolExec for PlanDraftTool {
    fn metadata(&self) -> ToolMetadata {
        ToolMetadata::new(
            "PlanDraft",
            "Create, replace, or cancel the current plan draft.",
            ApprovalTier::Write,
            ToolResultKind::Control,
        )
        .mutating()
        .storm_exempt()
        .internal()
    }

    fn execute(&self, input: &serde_json::Value, ctx: &ToolContext) -> anyhow::Result<ToolOutcome> {
        #[derive(serde::Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Args {
            content: String,
        }
        let args: Args = serde_json::from_value(input.clone())?;
        let cancelled = args.content.is_empty();
        ctx.plan_store
            .set_draft(&args.content, ctx.tool_config.file_write_max_bytes)?;
        Ok(ToolOutcome::plan(
            PlanCommand::SetDraft,
            if cancelled {
                "Plan draft cancelled."
            } else {
                "Plan draft saved."
            },
        ))
    }
}

impl ToolExec for PlanConfirmTool {
    fn metadata(&self) -> ToolMetadata {
        ToolMetadata::new(
            "PlanConfirm",
            "Atomically confirm the current plan draft.",
            ApprovalTier::Write,
            ToolResultKind::Control,
        )
        .mutating()
        .storm_exempt()
        .internal()
    }

    fn execute(
        &self,
        _input: &serde_json::Value,
        ctx: &ToolContext,
    ) -> anyhow::Result<ToolOutcome> {
        ctx.plan_store.confirm()?;
        Ok(ToolOutcome::plan(
            PlanCommand::Confirm,
            "Plan confirmed and locked in.",
        ))
    }
}

impl ToolExec for PlanClearTool {
    fn metadata(&self) -> ToolMetadata {
        ToolMetadata::new(
            "PlanClear",
            "Atomically clear the current confirmed plan.",
            ApprovalTier::Write,
            ToolResultKind::Control,
        )
        .mutating()
        .storm_exempt()
        .internal()
    }

    fn execute(
        &self,
        _input: &serde_json::Value,
        ctx: &ToolContext,
    ) -> anyhow::Result<ToolOutcome> {
        ctx.plan_store.clear()?;
        Ok(ToolOutcome::plan(PlanCommand::Clear, "Plan cleared."))
    }
}
