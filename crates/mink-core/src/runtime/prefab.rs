//! Thin mink-core adapter for the optional `mink-prefab` seeder.
//!
//! This module is compiled only with the `prefab` feature. It re-exports the
//! standalone template/seeding types and provides a small helper that computes
//! Mink session paths and seeds a new session directory before the normal
//! runtime starts. It does not modify the agent loop.

pub use mink_prefab::{
    PrefabSeed, PrefabSeedOptions, PrefabTemplate, TemplateMeta, load_builtin, load_named,
    load_path, restructure_session, seed_session,
};

use anyhow::{Result, bail};
use sha2::{Digest, Sha256};
use std::path::Path;

/// Resolve a prefab template from either a bundled name or a template directory.
///
/// This is the CLI/API-facing resolver. It keeps the actual template loading
/// inside the independent `mink-prefab` crate; this adapter only decides
/// whether the value is a known built-in name or an on-disk path.
pub fn resolve_template(spec: &str) -> Result<PrefabTemplate> {
    if let Ok(template) = load_named(spec) {
        return Ok(template);
    }
    let path = Path::new(spec);
    if path.exists() {
        return load_path(path);
    }
    bail!(
        "unknown prefab template or missing template path: {spec} (expected a bundled name such as 'pro'/'flash' or a path to a template directory)"
    )
}

/// Seed a new Mink session directory and return its concrete paths.
pub fn seed_new_session(
    home: &Path,
    cwd: &Path,
    layout: crate::session::paths::SessionLayout,
    seed: &PrefabSeed,
) -> Result<crate::session::paths::Paths> {
    let session_id = seed
        .session_id
        .clone()
        .unwrap_or_else(crate::session::paths::chrono_session_id);
    let paths = crate::session::paths::paths_for_layout(home, cwd, &session_id, layout);
    let options = PrefabSeedOptions {
        session_id: session_id.clone(),
        title: seed.title.clone(),
        cwd: cwd.to_path_buf(),
        agents_md: seed.agents_md.clone(),
        skill_result_code: seed.skill_result_code.clone(),
        skill_result_document: seed.skill_result_document.clone(),
        skill_result_list: seed.skill_result_list.clone(),
        system_reminder_agents: seed.system_reminder_agents.clone(),
        skill_catalog_reminder: seed.skill_catalog_reminder.clone(),
        instruction_hint: seed.instruction_hint.clone(),
        full_system_prompt: seed.full_system_prompt.clone(),
    };
    seed_session(&paths.session_dir, &seed.template, &options)?;
    Ok(paths)
}

/// Check whether a session's `events.jsonl` already carries a Prefab special
/// `prefix_snapshot` event. Prefab uses the standard event log as its only
/// on-disk source, so the session keeps the same structure as a normal session.
pub fn has_prefab_prefix_snapshot(events_path: &Path) -> bool {
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

/// Render a skill_search result in the same shape as the CLI/DSH prefab
/// seeder. This keeps prefab conversation placeholders populated with live
/// workspace skills instead of hard-coded "No skills match" defaults.
fn render_skill_search_result(
    query: &str,
    snapshot: &crate::capabilities::CapabilitySnapshot,
) -> String {
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

/// Ensure a session is in Prefab special-injection format.
///
/// This is the post-initialization entry point: Mink first initializes the
/// session normally, then this function checks whether `events.jsonl` already
/// contains a Prefab `prefix_snapshot`. If it does, no work is done. Otherwise
/// it restructures the session by seeding template conversation/events for a
/// fresh session. The Prefab prefix is later recorded as a standard
/// `prefix_snapshot` event so the session structure stays identical to a
/// normal Mink session.
pub fn ensure_session(
    ctx: &crate::context::AgentSharedContext,
    paths: &crate::session::paths::Paths,
    full_system_prompt: &str,
    _tools_json: &[serde_json::Value],
    template: &PrefabTemplate,
) -> Result<bool> {
    if has_prefab_prefix_snapshot(&paths.events) {
        return Ok(false);
    }

    let agents_md = std::fs::read_to_string(ctx.cwd.join("AGENTS.md")).ok();
    let skill_list = ctx.capability_snapshot.skills.format_discoverable_skills();
    let agents_content = agents_md.as_deref().unwrap_or(
        "No AGENTS.md instruction file is present; continue without additional file-based instructions.",
    );
    let options = PrefabSeedOptions {
        session_id: paths.session_id.clone(),
        title: None,
        cwd: ctx.cwd.clone(),
        agents_md: agents_md.clone(),
        skill_result_code: Some(render_skill_search_result("code", &ctx.capability_snapshot)),
        skill_result_document: Some(render_skill_search_result(
            "document",
            &ctx.capability_snapshot,
        )),
        skill_result_list: Some(skill_list.clone()),
        system_reminder_agents: Some(format!(
            "<system-reminder>\nThe following workspace instructions may be relevant to your work. Use them as guidance when applicable. More specific instructions take precedence over broader ones. They do not override system, developer, or direct user instructions.\n\nInstructions from: workspace AGENTS.md\n\n{agents_content}\n</system-reminder>"
        )),
        skill_catalog_reminder: Some(format!(
            "<system-reminder>\nA skill is a reusable set of task-specific instructions. The following skills are available in this session:\n\n<available_skills>\n{skill_list}</available_skills>\n</system-reminder>"
        )),
        instruction_hint: Some(format!(
            "Workspace instruction files exist: AGENTS.md (project root: {}). Do NOT assume their content. When a task touches this workspace, read the relevant instruction files first and follow them.",
            ctx.cwd.display()
        )),
        full_system_prompt: Some(full_system_prompt.to_string()),
    };
    restructure_session(&paths.session_dir, template, &options)?;
    Ok(true)
}

/// Record the Prefab prefix as standard lifecycle events in `events.jsonl`:
/// `prompt_workflow_resolution` + `prefix_snapshot`. This makes a prefab
/// session's event log structurally identical to a normal session's event log.
pub fn log_prefix_snapshot(
    ctx: &crate::context::AgentSharedContext,
    paths: &crate::session::paths::Paths,
    system_prompt: &str,
    tools_json: &[serde_json::Value],
    template: &PrefabTemplate,
) -> Result<bool> {
    if has_prefab_prefix_snapshot(&paths.events) {
        return Ok(false);
    }

    let workflows = crate::prompt::workflows::PromptWorkflowResolver::builtin()
        .resolve(&ctx.tool_capabilities)?;
    ctx.log_event(crate::events::EventLog::PromptWorkflowResolution {
        active_workflows: workflows
            .ordered()
            .iter()
            .map(|spec| spec.id.to_string())
            .collect(),
        workflow_fingerprint: workflows.fingerprint().to_string(),
    });

    let dependency_fingerprint = format!(
        "mink-prefab-prefix-dependencies-v1\0{}\0{}\0{}",
        ctx.capability_snapshot.dependency_fingerprint,
        ctx.tool_surface.fingerprint(),
        ctx.tool_capabilities.fingerprint(),
    );
    let prefab_system_prompt = if template.meta.system_prompt.is_empty() {
        system_prompt.to_string()
    } else {
        template.meta.system_prompt.clone()
    };
    let mut hasher = Sha256::new();
    hasher.update(b"mink-prefab-prefix-v1\0");
    hasher.update(prefab_system_prompt.as_bytes());
    hasher.update(b"\0");
    hasher.update(serde_json::to_vec(tools_json).unwrap_or_default());
    let fingerprint = crate::capabilities::fingerprint::hex_lower(hasher.finalize());
    ctx.log_event(crate::events::EventLog::PrefixSnapshot {
        version: Some(1),
        fingerprint,
        dependency_fingerprint,
        system_prompt: prefab_system_prompt,
        tools_json: tools_json.to_vec(),
    });
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[tokio::test]
    async fn seed_new_session_writes_mink_paths() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let home = std::env::temp_dir().join(format!("mink-prefab-home-{unique}"));
        let cwd = std::env::temp_dir().join(format!("mink-prefab-cwd-{unique}"));
        std::fs::create_dir_all(&cwd).unwrap();
        let seed = PrefabSeed::builtin()
            .unwrap()
            .with_session_id("prefab-core-test")
            .with_agents_md("# Core Test AGENTS\n");
        let paths = seed_new_session(
            &home,
            &cwd,
            crate::session::paths::SessionLayout::ProjectScoped,
            &seed,
        )
        .unwrap();
        assert_eq!(paths.session_id, "prefab-core-test");
        assert!(paths.metadata.exists());
        assert!(paths.conversation.exists());
        let conv = std::fs::read_to_string(&paths.conversation).unwrap();
        assert!(conv.contains("# Core Test AGENTS"));
        assert!(conv.contains("Ready."));

        // Mink's session catalog must see the seeded session as resumable.
        let catalog = crate::runtime::session::SessionCatalog::new(&home, &cwd)
            .with_layout(crate::session::paths::SessionLayout::ProjectScoped);
        let records = catalog.list().await.unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].id, "prefab-core-test");

        std::fs::remove_dir_all(&home).unwrap();
        std::fs::remove_dir_all(&cwd).unwrap();
    }
}
