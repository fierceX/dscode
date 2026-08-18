//! Optional runtime integration for Mink (`mink-integration` feature).
//!
//! The seeder core (`seed`/`template` modules) stays independent of
//! `mink-core`. This module is the thin adapter that plugs prefab into a
//! running Mink runtime through Mink's neutral extension points:
//!
//! - [`PrefabPrefixSource`] implements [`mink::runtime::PrefixSource`]:
//!   restores the prefab system prompt + tool schemas from the session's
//!   standard `prefix_snapshot` event instead of the compiled prompt.
//! - [`PrefabRestructureHook`] implements [`mink::runtime::PostInitHook`]:
//!   seeds/restructures a fresh session into the template conversation and
//!   records the prefix as standard `prefix_snapshot` lifecycle events.
//!
//! Hosts (CLI, examples, embedders) wire both through
//! [`install_template`] and never re-implement this logic.

use crate::{PrefabSeedOptions, PrefabTemplate, load_named, load_path, restructure_session};
use mink::runtime::{PostInitContext, PostInitHook, PrefixSource};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::path::Path;
use std::sync::Arc;

/// Resolve a prefab template from either a bundled name or a template
/// directory.
pub fn resolve_template(spec: &str) -> anyhow::Result<PrefabTemplate> {
    if let Ok(template) = load_named(spec) {
        return Ok(template);
    }
    let path = Path::new(spec);
    if path.exists() {
        return load_path(path);
    }
    anyhow::bail!(
        "unknown prefab template or missing template path: {spec} (expected a bundled name such as 'pro'/'flash' or a path to a template directory)"
    )
}

/// True when a session's `events.jsonl` already carries a Prefab special
/// `prefix_snapshot` event (one whose system prompt is not the standard
/// compiled prompt, i.e. does not contain `<system-conventions>`).
fn has_prefab_prefix_snapshot(events_path: &Path) -> bool {
    let Ok(text) = std::fs::read_to_string(events_path) else {
        return false;
    };
    for line in text.lines() {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        if value.get("type").and_then(serde_json::Value::as_str) != Some("prefix_snapshot") {
            continue;
        }
        let system_prompt = value
            .get("system_prompt")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("");
        if !system_prompt.contains("<system-conventions>") {
            return true;
        }
    }
    false
}

/// Supplies the Prefab prefix (system prompt + tool schemas) restored from
/// the session's standard `prefix_snapshot` event.
pub struct PrefabPrefixSource;

impl PrefixSource for PrefabPrefixSource {
    fn prefix(&self, events_path: &Path) -> Option<(String, Vec<Value>)> {
        let text = std::fs::read_to_string(events_path).ok()?;
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
}

/// Render a skill_search result in the same shape as the DSH prefab seeder.
/// Keeps prefab conversation placeholders populated with live workspace
/// skills instead of hard-coded "No skills match" defaults.
fn render_skill_search_result(query: &str, snapshot: &mink::runtime::CapabilitySnapshot) -> String {
    let query_lower = query.to_lowercase();
    let wanted: Vec<&str> = query_lower
        .split(|c: char| !c.is_ascii_alphanumeric() && c != '_' && c != '-')
        .filter(|s| !s.is_empty())
        .collect();
    let matches: Vec<_> = snapshot
        .skills
        .discoverable
        .iter()
        .filter(|skill| {
            if wanted.is_empty() {
                return true;
            }
            let haystack = format!(
                "{} {} {}",
                skill.skill.name.to_lowercase(),
                skill.skill.description.to_lowercase(),
                skill.skill.description.to_lowercase()
            );
            wanted.iter().all(|token| haystack.contains(token))
        })
        .take(20)
        .collect();
    if matches.is_empty() {
        return format!("No skills match \"{query}\". Use skill_search with other keywords.");
    }
    let lines = matches
        .iter()
        .map(|skill| {
            format!(
                "- {}: {}",
                skill.skill.name,
                skill.skill.description.split('\n').next().unwrap_or("")
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    format!(
        "Matching skills ({}):\n{lines}\n\nLoad one with skill_load (exact name).",
        matches.len()
    )
}

/// Post-initialization hook that restructures a fresh session into the
/// Prefab template conversation and records the special prefix as standard
/// `prefix_snapshot` lifecycle events. Idempotent: sessions that already
/// carry a Prefab `prefix_snapshot` event are left untouched.
pub struct PrefabRestructureHook {
    template: PrefabTemplate,
}

impl PrefabRestructureHook {
    pub fn new(template: PrefabTemplate) -> Self {
        Self { template }
    }

    /// Hook for the bundled default template.
    pub fn builtin() -> anyhow::Result<Self> {
        Ok(Self::new(crate::load_builtin()?))
    }
}

impl PostInitHook for PrefabRestructureHook {
    fn run(&self, ctx: &PostInitContext<'_>) -> anyhow::Result<()> {
        let events_path = ctx.session_paths().events.clone();
        if has_prefab_prefix_snapshot(&events_path) {
            return Ok(());
        }

        let agents_md = std::fs::read_to_string(ctx.cwd().join("AGENTS.md")).ok();
        let skill_list = ctx.capabilities().skills.format_discoverable_skills();
        let agents_content = agents_md.as_deref().unwrap_or(
            "No AGENTS.md instruction file is present; continue without additional file-based instructions.",
        );
        let options = PrefabSeedOptions {
            session_id: ctx.session_paths().session_id.clone(),
            title: None,
            cwd: ctx.cwd().to_path_buf(),
            agents_md: agents_md.clone(),
            skill_result_code: Some(render_skill_search_result("code", ctx.capabilities())),
            skill_result_document: Some(render_skill_search_result("document", ctx.capabilities())),
            skill_result_list: Some(skill_list.clone()),
            system_reminder_agents: Some(format!(
                "<system-reminder>\nThe following workspace instructions may be relevant to your work. Use them as guidance when applicable. More specific instructions take precedence over broader ones. They do not override system, developer, or direct user instructions.\n\nInstructions from: workspace AGENTS.md\n\n{agents_content}\n</system-reminder>"
            )),
            skill_catalog_reminder: Some(format!(
                "<system-reminder>\nA skill is a reusable set of task-specific instructions. The following skills are available in this session:\n\n<available_skills>\n{skill_list}</available_skills>\n</system-reminder>"
            )),
            instruction_hint: Some(format!(
                "Workspace instruction files exist: AGENTS.md (project root: {}). Do NOT assume their content. When a task touches this workspace, read the relevant instruction files first and follow them.",
                ctx.cwd().display()
            )),
            full_system_prompt: Some(ctx.system_prompt().to_string()),
        };
        restructure_session(&ctx.session_paths().session_dir, &self.template, &options)?;

        ctx.log_event(serde_json::json!({
            "type": "prompt_workflow_resolution",
            "active_workflows": ctx.workflow_ids(),
            "workflow_fingerprint": ctx.workflow_fingerprint(),
        }))?;

        let dependency_fingerprint = format!(
            "mink-prefab-prefix-dependencies-v1\0{}\0{}\0{}",
            ctx.dependency_fingerprint(),
            ctx.tool_surface_fingerprint(),
            ctx.tool_capabilities_fingerprint(),
        );
        let prefab_system_prompt = if self.template.meta.system_prompt.is_empty() {
            ctx.system_prompt().to_string()
        } else {
            self.template.meta.system_prompt.clone()
        };
        let mut hasher = Sha256::new();
        hasher.update(b"mink-prefab-prefix-v1\0");
        hasher.update(prefab_system_prompt.as_bytes());
        hasher.update(b"\0");
        hasher.update(serde_json::to_vec(ctx.tools()).unwrap_or_default());
        let fingerprint = hex_lower(&hasher.finalize());
        ctx.log_event(serde_json::json!({
            "type": "prefix_snapshot",
            "version": 1,
            "fingerprint": fingerprint,
            "dependency_fingerprint": dependency_fingerprint,
            "system_prompt": prefab_system_prompt,
            "tools_json": ctx.tools(),
        }))?;
        Ok(())
    }
}

fn hex_lower(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// Wire a resolved template into runtime options through the two extension
/// points: the prefix source restores the prefab prefix, the hook
/// seeds/restructures the session and records the `prefix_snapshot` event.
pub fn install_template(
    options: mink::runtime::AgentOptions,
    template: PrefabTemplate,
) -> mink::runtime::AgentOptions {
    options
        .with_prefix_source(Arc::new(PrefabPrefixSource))
        .with_post_init_hook(Arc::new(PrefabRestructureHook::new(template)))
}

/// Convenience for tests and embedders: wire the bundled default template.
pub fn install_builtin(
    options: mink::runtime::AgentOptions,
) -> anyhow::Result<mink::runtime::AgentOptions> {
    Ok(install_template(options, crate::load_builtin()?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};
    use std::time::{SystemTime, UNIX_EPOCH};

    struct TextBackend {
        seen: Arc<Mutex<Vec<String>>>,
    }

    #[async_trait::async_trait]
    impl mink::runtime::LlmBackend for TextBackend {
        fn name(&self) -> &str {
            "text"
        }

        async fn stream(
            &self,
            request: mink::runtime::LlmRequest,
        ) -> anyhow::Result<mink::runtime::LlmResponseStream> {
            self.seen.lock().unwrap().push(request.system_prompt);
            Ok(mink::runtime::LlmResponseStream {
                events: Box::pin(futures::stream::iter(vec![
                    Ok(mink::runtime::LlmEvent::Text(mink::runtime::LlmTextEvent {
                        content: "ok".into(),
                    })),
                    Ok(mink::runtime::LlmEvent::Stop(mink::runtime::LlmStopEvent {
                        reason: "end_turn".into(),
                    })),
                ])),
                attempt_count: 1,
            })
        }
    }

    fn unique_dir(name: &str) -> std::path::PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("mink-prefab-adapter-{name}-{nanos}"))
    }

    /// A normal (non-prefab) session resumed with the prefab adapter must not
    /// have its conversation rewritten: the hook's `restructure_session` guard
    /// sees existing data, and only the standard prefix_snapshot event is
    /// appended. Afterwards the prefix source serves the template prompt.
    #[tokio::test]
    async fn adapter_does_not_rewrite_existing_normal_conversation() {
        let home = unique_dir("normal-resume-home");
        let cwd = unique_dir("normal-resume-cwd");
        tokio::fs::create_dir_all(&cwd).await.unwrap();

        // 1) Create a normal session with one real turn.
        let seen = Arc::new(Mutex::new(Vec::new()));
        let base = || {
            mink::runtime::AgentOptions::new(home.clone(), cwd.clone())
                .with_project_scoped_sessions()
                .with_llm_backend(Arc::new(TextBackend { seen: seen.clone() }))
                .with_api_key("test-key")
                .with_base_url("https://example.invalid/v1")
        };
        let runtime = mink::runtime::AgentRuntime::start(base()).await.unwrap();
        let info = runtime.session_info().clone();
        runtime.run_turn("hello").await.unwrap();
        runtime.shutdown().await.unwrap();

        let conv_before = tokio::fs::read_to_string(&info.conversation_path)
            .await
            .unwrap();
        let events_before = tokio::fs::read_to_string(&info.events_path).await.unwrap();
        assert!(conv_before.contains("hello"));
        // A normal session writes a standard prefix_snapshot (compiled prompt
        // with <system-conventions>) during the first turn.
        let normal_snapshots: Vec<serde_json::Value> = events_before
            .lines()
            .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
            .filter(|v| {
                v.get("type").and_then(serde_json::Value::as_str) == Some("prefix_snapshot")
            })
            .collect();
        assert_eq!(normal_snapshots.len(), 1);
        assert!(
            normal_snapshots[0]["system_prompt"]
                .as_str()
                .unwrap()
                .contains("<system-conventions>"),
            "standard snapshot carries the compiled prompt"
        );

        // 2) Resume the same session with the prefab adapter installed.
        let resumed = mink::runtime::AgentRuntime::start(
            install_builtin(base().with_session(mink::runtime::SessionPolicy::ContinueLatest))
                .unwrap(),
        )
        .await
        .unwrap();
        let conv_after = tokio::fs::read_to_string(&info.conversation_path)
            .await
            .unwrap();
        let events_after = tokio::fs::read_to_string(&info.events_path).await.unwrap();
        assert_eq!(
            conv_after, conv_before,
            "conversation must not be rewritten"
        );
        let snapshots: Vec<serde_json::Value> = events_after
            .lines()
            .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
            .filter(|v| {
                v.get("type").and_then(serde_json::Value::as_str) == Some("prefix_snapshot")
            })
            .collect();
        assert_eq!(snapshots.len(), 2, "one standard + one prefab snapshot");
        let prompt = snapshots[1]["system_prompt"].as_str().unwrap();
        assert!(
            !prompt.contains("<system-conventions>"),
            "the appended snapshot carries the template prompt"
        );

        // 3) The prefix source serves the template prompt from the event.
        let prefix = PrefabPrefixSource.prefix(&info.events_path);
        let (source_prompt, _tools) = prefix.expect("prefix source must restore the prefab prompt");
        assert_eq!(source_prompt, prompt);

        resumed.shutdown().await.unwrap();
        let _ = tokio::fs::remove_dir_all(home).await;
        let _ = tokio::fs::remove_dir_all(cwd).await;
    }

    /// The hook is idempotent: a second start must not duplicate events.
    #[tokio::test]
    async fn adapter_is_idempotent_across_starts() {
        let home = unique_dir("idempotent-home");
        let cwd = unique_dir("idempotent-cwd");
        tokio::fs::create_dir_all(&cwd).await.unwrap();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let base = || {
            mink::runtime::AgentOptions::new(home.clone(), cwd.clone())
                .with_project_scoped_sessions()
                .with_llm_backend(Arc::new(TextBackend { seen: seen.clone() }))
                .with_api_key("test-key")
                .with_base_url("https://example.invalid/v1")
        };

        let first = mink::runtime::AgentRuntime::start(install_builtin(base()).unwrap())
            .await
            .unwrap();
        let info = first.session_info().clone();
        first.shutdown().await.unwrap();

        let second = mink::runtime::AgentRuntime::start(
            install_builtin(base().with_session(mink::runtime::SessionPolicy::ContinueLatest))
                .unwrap(),
        )
        .await
        .unwrap();
        let events = tokio::fs::read_to_string(&info.events_path).await.unwrap();
        let snapshots = events
            .lines()
            .filter(|l| l.contains("\"type\":\"prefix_snapshot\""))
            .count();
        assert_eq!(snapshots, 1, "no duplicate snapshot events across starts");
        second.shutdown().await.unwrap();
        let _ = tokio::fs::remove_dir_all(home).await;
        let _ = tokio::fs::remove_dir_all(cwd).await;
    }
}
