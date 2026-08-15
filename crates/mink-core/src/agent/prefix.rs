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
                skill_snapshot: Arc::new(self.ctx.capability_snapshot.skills.clone()),
                context_file_snapshot: Arc::new(self.ctx.capability_snapshot.context_files.clone()),
                rule_snapshot: Arc::new(self.ctx.capability_snapshot.rules.clone()),
                mission_file: self.ctx.config.mission_file.clone(),
                mission_content: self.ctx.config.mission_content.clone(),
                tool_surface: self.ctx.tool_surface.clone(),
                tool_capabilities: self.ctx.tool_capabilities.clone(),
                edit_mode: self.ctx.tool_config.edit_mode,
                edit_fuzzy_match: self.ctx.tool_config.edit_fuzzy_match,
                edit_fuzzy_threshold: self.ctx.tool_config.edit_fuzzy_threshold,
                edit_enforce_seen_lines: self.ctx.tool_config.edit_enforce_seen_lines,
                signal_policy: self.ctx.config.signal_policy,
            }
            .build_system_prompt()?;
            let tools_json = self.ctx.tool_surface.schemas();
            let workflows = crate::prompt::workflows::PromptWorkflowResolver::builtin()
                .resolve(&self.ctx.tool_capabilities)?;
            self.ctx.log_event(serde_json::json!({
                "type": "prompt_workflow_resolution",
                "active_workflows": workflows.ordered().iter().map(|spec| spec.id).collect::<Vec<_>>(),
                "workflow_fingerprint": workflows.fingerprint(),
            }));
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
            self.ctx
                .log_typed_event(crate::events::EventLog::PrefixSnapshot {
                    version: 1,
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
