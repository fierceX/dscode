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

    /// In prefab mode only, rebuild the complete prefix from the session's
    /// standard `prefix_snapshot` event in `events.jsonl` instead of compiling
    /// it from code.
    #[cfg(feature = "prefab")]
    fn prefab_prefix_from_session(&self) -> Option<(String, Vec<serde_json::Value>)> {
        if !self.ctx.prefab_mode {
            return None;
        }
        let text = std::fs::read_to_string(&self.ctx.events_path).ok()?;
        for line in text.lines() {
            let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
                continue;
            };
            if value.get("type").and_then(serde_json::Value::as_str) != Some("prefix_snapshot") {
                continue;
            }
            let system_prompt = value.get("system_prompt")?.as_str()?.to_string();
            let tools_json = value.get("tools_json")?.as_array()?.clone();
            if !system_prompt.contains("<system-conventions>") {
                return Some((system_prompt, tools_json));
            }
        }
        None
    }

    pub fn ensure(&self) -> Result<(String, Vec<serde_json::Value>)> {
        #[cfg(feature = "prefab")]
        if let Some(prefix) = self.prefab_prefix_from_session() {
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
