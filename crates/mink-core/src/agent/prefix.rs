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

    pub fn invalidate(&self) {
        *self
            .ctx
            .immutable_prefix
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::prefix::ImmutablePrefix;

    #[tokio::test]
    async fn ensure_reuses_valid_cached_prefix() -> anyhow::Result<()> {
        let ctx = crate::regression::test_context_for_agent("prefix-cache-hit").await?;
        let manager = PrefixManager::new(ctx.clone());
        let (first_prompt, first_tools) = manager.ensure()?;
        let first_names: Vec<_> = first_tools
            .iter()
            .filter_map(|schema| schema.get("name").and_then(serde_json::Value::as_str))
            .collect();
        assert_eq!(
            first_names,
            ctx.tool_surface.names().collect::<Vec<_>>(),
            "request schemas must come from the resolved surface"
        );
        let cached_fingerprint = ctx
            .immutable_prefix
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .fingerprint()
            .to_string();

        let (second_prompt, second_tools) = manager.ensure()?;

        assert_eq!(first_prompt, second_prompt);
        assert_eq!(first_tools, second_tools);
        assert_eq!(
            ctx.immutable_prefix
                .lock()
                .unwrap()
                .as_ref()
                .unwrap()
                .fingerprint(),
            cached_fingerprint
        );
        Ok(())
    }

    #[tokio::test]
    async fn ensure_logs_prefix_snapshot_once_and_rebuild_replaces_it() -> anyhow::Result<()> {
        let ctx = crate::regression::test_context_for_agent("prefix-snapshot-event").await?;
        let manager = PrefixManager::new(ctx.clone());
        let (system_prompt, tools) = manager.ensure()?;

        let snapshot = |events: &str| -> Vec<serde_json::Value> {
            events
                .lines()
                .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
                .filter(|evt| {
                    evt.get("type").and_then(serde_json::Value::as_str) == Some("prefix_snapshot")
                })
                .collect()
        };

        let events = tokio::fs::read_to_string(&ctx.events_path).await?;
        let snapshots = snapshot(&events);
        assert_eq!(
            snapshots.len(),
            1,
            "first build writes exactly one snapshot"
        );
        let evt = &snapshots[0];
        assert_eq!(evt["version"], 1);
        assert_eq!(
            evt["fingerprint"].as_str().unwrap(),
            ctx.immutable_prefix
                .lock()
                .unwrap()
                .as_ref()
                .unwrap()
                .fingerprint()
        );
        assert_eq!(evt["system_prompt"].as_str().unwrap(), system_prompt);
        assert_eq!(evt["tools_json"], serde_json::Value::Array(tools.clone()));

        // Cache hit must not duplicate the snapshot.
        manager.ensure()?;
        let events = tokio::fs::read_to_string(&ctx.events_path).await?;
        assert_eq!(snapshot(&events).len(), 1);

        // Invalidation rebuild replaces the snapshot with the new fingerprint.
        manager.invalidate();
        manager.ensure()?;
        let events = tokio::fs::read_to_string(&ctx.events_path).await?;
        let snapshots = snapshot(&events);
        assert_eq!(snapshots.len(), 2, "rebuild appends one more snapshot");
        assert_eq!(
            snapshots[1]["fingerprint"].as_str().unwrap(),
            ctx.immutable_prefix
                .lock()
                .unwrap()
                .as_ref()
                .unwrap()
                .fingerprint()
        );
        Ok(())
    }

    #[tokio::test]
    async fn ensure_drops_corrupt_cached_prefix_and_rebuilds() -> anyhow::Result<()> {
        let ctx = crate::regression::test_context_for_agent("prefix-cache-invalid").await?;
        *ctx.immutable_prefix.lock().unwrap() = Some(ImmutablePrefix::new_with_fingerprint(
            "stale".into(),
            vec![serde_json::json!({"name":"Bash"})],
            String::new(),
            "bad-fingerprint".into(),
        ));
        let manager = PrefixManager::new(ctx.clone());

        let (system_prompt, tools) = manager.ensure()?;

        assert_ne!(system_prompt, "stale");
        assert!(!tools.is_empty());
        assert!(
            ctx.immutable_prefix
                .lock()
                .unwrap()
                .as_ref()
                .unwrap()
                .verify_fingerprint()
        );
        Ok(())
    }
}
