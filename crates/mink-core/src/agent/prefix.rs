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
                plan_file: self.ctx.plan_path.clone(),
                plan_draft_file: self.ctx.plan_draft_path.clone(),
                mission_file: self.ctx.config.mission_file.clone(),
                mission_content: self.ctx.config.mission_content.clone(),
            }
            .build_system_prompt()?;
            let tools_json =
                serde_json::from_str::<Vec<serde_json::Value>>(crate::assets::TOOLS_JSON)
                    .unwrap_or_default();
            let available_tools: std::collections::BTreeSet<&'static str> =
                crate::tools::runner::tool_registry()
                    .iter()
                    .map(|tool| tool.metadata().name)
                    .collect();
            let tools_json = tools_json
                .into_iter()
                .filter(|tool| {
                    tool.get("name")
                        .and_then(serde_json::Value::as_str)
                        .is_some_and(|name| available_tools.contains(name))
                })
                .collect();

            // Filter tools: combine disable flags + enabled whitelist at config layer
            let tools_json = self.ctx.tool_config.filter_tools_json(tools_json);
            *guard = Some(ImmutablePrefix::new(
                system_prompt.clone(),
                tools_json.clone(),
                self.ctx.capability_snapshot.dependency_fingerprint.clone(),
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::prefix::ImmutablePrefix;

    #[tokio::test]
    async fn ensure_reuses_valid_cached_prefix() -> anyhow::Result<()> {
        let ctx = crate::regression::test_context_for_agent("prefix-cache-hit").await?;
        let manager = PrefixManager::new(ctx.clone());
        let (first_prompt, first_tools) = manager.ensure()?;
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
