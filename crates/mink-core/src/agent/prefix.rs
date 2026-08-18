use crate::context::AgentSharedContext;
use crate::session::prefix::ImmutablePrefix;
use anyhow::Result;
use std::sync::Arc;

#[derive(Clone)]
pub struct PrefixManager {
    ctx: Arc<AgentSharedContext>,
}

impl PrefixManager {
    pub fn new(ctx: Arc<AgentSharedContext>) -> Self {
        Self { ctx }
    }

    pub fn ensure(&self) -> Result<(String, Vec<serde_json::Value>)> {
        if let Some(source) = &self.ctx.prefix_source
            && let Some(prefix) = source.prefix(&self.ctx.events_path)
        {
            return Ok(prefix);
        }

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

            let system_prompt = self.ctx.build_system_prompt()?;
            let tools_json = self.ctx.tool_surface.schemas();
            let workflows = crate::prompt::workflows::PromptWorkflowResolver::builtin()
                .resolve(&self.ctx.tool_capabilities)?;
            self.ctx
                .log_event(crate::events::EventLog::PromptWorkflowResolution {
                    active_workflows: workflows
                        .ordered()
                        .iter()
                        .map(|spec| spec.id.to_string())
                        .collect(),
                    workflow_fingerprint: workflows.fingerprint().to_string(),
                });
            let dependency_fingerprint = format!(
                "mink-prefix-dependencies-v2\0{}\0{}\0{}\0{}",
                self.ctx.capability_snapshot.dependency_fingerprint,
                self.ctx.tool_surface.fingerprint(),
                self.ctx.tool_capabilities.fingerprint(),
                workflows.fingerprint(),
            );
            let prefix = ImmutablePrefix::new(
                system_prompt.clone(),
                tools_json.clone(),
                dependency_fingerprint.clone(),
            );
            self.ctx.log_event(crate::events::EventLog::PrefixSnapshot {
                version: Some(1),
                fingerprint: prefix.fingerprint().to_string(),
                dependency_fingerprint: dependency_fingerprint.clone(),
                system_prompt: system_prompt.clone(),
                tools_json: tools_json.clone(),
            });
            *guard = Some(prefix);
            return Ok((system_prompt, tools_json));
        }
    }

    #[cfg(test)]
    pub fn invalidate(&self) {
        *self
            .ctx
            .immutable_prefix
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = None;
    }
}

#[cfg(test)]
#[path = "prefix_tests.rs"]
mod tests;
