use crate::agent::prefix::PrefixManager;
use crate::agent::turn::TurnEffect;
use crate::context::AgentSharedContext;
use crate::llm::client::LlmModelTarget;
use crate::tools::runner::ToolRunResult;
use std::sync::Arc;

pub struct PlanActionHandler {
    ctx: Arc<AgentSharedContext>,
}

impl PlanActionHandler {
    pub fn new(ctx: Arc<AgentSharedContext>) -> Self {
        Self { ctx }
    }

    pub async fn handle(
        &self,
        result: &mut ToolRunResult,
        effects: &mut Vec<TurnEffect>,
        prefix: &PrefixManager,
        target: LlmModelTarget<'_>,
    ) {
        match result.tool_name.as_str() {
            "PlanClear" => {
                let _ = self
                    .ctx
                    .compaction
                    .evaluate_and_compact("plan_clear", 0, target)
                    .await;
                let _ = tokio::fs::write(&self.ctx.plan_path, "").await;
                result.content = "Plan cleared.".to_string();
                effects.push(TurnEffect::PlanCleared);
                prefix.invalidate();
            }
            "PlanConfirm" => {
                let confirmed = match tokio::fs::read(&self.ctx.plan_draft_path).await {
                    Ok(data) if !data.is_empty() => {
                        let _ = self
                            .ctx
                            .compaction
                            .evaluate_and_compact("plan_confirm", 0, target)
                            .await;
                        let _ = tokio::fs::write(&self.ctx.plan_path, &data).await;
                        let _ = tokio::fs::write(&self.ctx.plan_draft_path, "").await;
                        result.content = "Plan confirmed and locked in.".to_string();
                        true
                    }
                    _ => {
                        result.content = "Error: no plan draft found to confirm.".to_string();
                        false
                    }
                };
                if confirmed {
                    effects.push(TurnEffect::PlanConfirmed);
                    prefix.invalidate();
                }
            }
            _ => {}
        }
    }
}
