use crate::context::AgentSharedContext;
use crate::session::prefix::ImmutablePrefix;
use anyhow::Result;
use std::sync::Arc;

/// Tool names that map to disable flags.
const TOOL_DISABLE_MAP: &[(&str, fn(&crate::config::ToolDisableFlags) -> bool)] = &[
    ("Bash", |f| f.disable_bash),
    ("Python", |f| f.disable_python),
    ("WebSearch", |f| f.disable_web),
    ("WebFetch", |f| f.disable_web),
    ("SubAgent", |f| f.disable_sub_agent),
];

/// Remove tool definitions from the JSON list that are disabled by config.
fn filter_disabled_tools(
    tools: Vec<serde_json::Value>,
    flags: &crate::config::ToolDisableFlags,
) -> Vec<serde_json::Value> {
    tools
        .into_iter()
        .filter(|tool| {
            let name = tool.get("name").and_then(|v| v.as_str()).unwrap_or("");
            !TOOL_DISABLE_MAP
                .iter()
                .any(|(n, check)| *n == name && check(flags))
        })
        .collect()
}

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
                mission_file: self.ctx.config.mission_file.clone(),
            }
            .build_system_prompt()?;
            let tools_json =
                serde_json::from_str::<Vec<serde_json::Value>>(crate::assets::TOOLS_JSON)
                    .unwrap_or_default();

            // Filter out disabled tools at the source — the LLM won't even
            // see them as available functions.
            let tools_json = filter_disabled_tools(tools_json, &self.ctx.config.tool_disable);
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
