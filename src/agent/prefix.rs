use crate::context::AgentSharedContext;
use crate::session::prefix::ImmutablePrefix;
use anyhow::Result;
use std::sync::Arc;

pub struct PrefixManager {
    ctx: Arc<AgentSharedContext>,
}

impl PrefixManager {
    pub fn new(ctx: Arc<AgentSharedContext>) -> Self {
        Self { ctx }
    }

    pub fn ensure(&self) -> Result<(String, Vec<serde_json::Value>)> {
        loop {
            let mut guard = self
                .ctx
                .immutable_prefix
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            if let Some(ref prefix) = *guard {
                if prefix.verify_fingerprint() {
                    return Ok((
                        prefix.system_prompt().to_string(),
                        prefix.tools_json().to_vec(),
                    ));
                }
                *guard = None;
                drop(guard);
                continue;
            }

            let system_prompt = crate::prompt::Builder {
                cwd: self.ctx.cwd.clone(),
                home: self.ctx.home.clone(),
                skills: self.ctx.config.skills.clone(),
                summary_file: self.ctx.summary_path.clone(),
                plan_file: self.ctx.plan_path.clone(),
                plan_draft_file: self.ctx.plan_draft_path.clone(),
            }
            .build_system_prompt()?;
            let tools_json =
                serde_json::from_str::<Vec<serde_json::Value>>(crate::assets::TOOLS_JSON)
                    .unwrap_or_default();
            *guard = Some(ImmutablePrefix::new(
                system_prompt.clone(),
                tools_json.clone(),
            ));
            return Ok((system_prompt, tools_json));
        }
    }

    pub fn invalidate(&self) {
        *self
            .ctx
            .immutable_prefix
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = None;
    }
}
