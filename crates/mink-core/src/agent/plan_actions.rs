use crate::agent::turn::TurnEffect;
use crate::tools::plan::PlanCommand;
use crate::tools::runner::ToolExecution;

pub struct PlanActionHandler;

impl PlanActionHandler {
    pub fn handle(
        &self,
        result: &mut ToolExecution,
        effects: &mut Vec<TurnEffect>,
    ) -> Option<&'static str> {
        let command = result.plan_command.take()?;

        match command {
            PlanCommand::SetDraft => {}
            PlanCommand::Confirm => {
                effects.push("Plan confirmed.");
            }
            PlanCommand::Clear => {
                effects.push("Plan cleared.");
            }
        }
        command.compaction_trigger()
    }
}
