use crate::agent::turn::TurnEffect;
use crate::tools::plan::PlanCommand;
use crate::tools::runner::ToolRunResult;

pub struct PlanActionHandler;

impl PlanActionHandler {
    pub fn handle(
        &self,
        result: &mut ToolRunResult,
        effects: &mut Vec<TurnEffect>,
    ) -> Option<&'static str> {
        let command = result.plan_command.take()?;

        match command {
            PlanCommand::SetDraft => {}
            PlanCommand::Confirm => {
                effects.push(TurnEffect::PlanConfirmed);
            }
            PlanCommand::Clear => {
                effects.push(TurnEffect::PlanCleared);
            }
        }
        command.compaction_trigger()
    }
}
