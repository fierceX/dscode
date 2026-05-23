use crate::agent::prefix::PrefixManager;
use crate::context::AgentSharedContext;
use anyhow::Result;
use std::sync::Arc;

pub struct TurnCompactor {
    ctx: Arc<AgentSharedContext>,
    compacted_this_turn: bool,
}

impl TurnCompactor {
    pub fn new(ctx: Arc<AgentSharedContext>) -> Self {
        Self {
            ctx,
            compacted_this_turn: false,
        }
    }

    pub fn reset(&mut self) {
        self.compacted_this_turn = false;
    }

    pub fn compacted_this_turn(&self) -> bool {
        self.compacted_this_turn
    }

    pub async fn maybe_compact(
        &mut self,
        trigger: &str,
        messages: &mut Vec<serde_json::Value>,
        system_prompt: &mut String,
        tools_json: &mut Vec<serde_json::Value>,
        prefix: &PrefixManager,
    ) -> Result<bool> {
        if self.compacted_this_turn {
            return Ok(false);
        }
        let stats = self.ctx.stats.snapshot().await;
        let compacted = self
            .ctx
            .compaction
            .evaluate_and_compact(trigger, stats.current_context_tokens as usize)
            .await;
        if let Ok((did_compact, _)) = compacted
            && did_compact
        {
            self.compacted_this_turn = true;
            prefix.invalidate();
            *messages = self.ctx.store.lines().await?;
            (*system_prompt, *tools_json) = prefix.ensure()?;
            return Ok(true);
        }
        Ok(false)
    }
}
