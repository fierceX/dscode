use crate::agent::turn::TurnEffect;
use crate::tools::plan::PlanCommand;
use crate::tools::runner::ToolExecution;

pub struct PlanActionHandler;

impl PlanActionHandler {
    pub fn handle(
        &self,
        result: &mut ToolExecution,
        effects: &mut Vec<TurnEffect>,
    ) -> Option<PlanCommand> {
        let command = result.plan_command.take()?;

        match command {
            PlanCommand::SetDraft => return None,
            PlanCommand::Confirm => {
                effects.push("Plan confirmed.");
            }
            PlanCommand::Clear => {
                effects.push("Plan cleared.");
            }
        }
        Some(command)
    }
}
