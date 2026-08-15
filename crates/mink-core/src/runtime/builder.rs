use crate::agent::orchestrator::new_orchestrator;
use crate::cancel::CancellationToken;
use crate::config::api_url;
use crate::llm::client::OpenAiCompatibleBackend;
use crate::runtime::config::{AgentRuntimeConfig, SessionInfo, SessionPolicy};
use crate::runtime::context_build::{AgentContextBuild, build_agent_context};
use crate::runtime::events::{EventDispatcher, EventDisplay};
use crate::runtime::handle::AgentRuntime;
use crate::session::metadata::{SessionSeed, sanitize_alias};
use crate::session::paths;
use anyhow::{Result, bail};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

pub(crate) async fn build_runtime(config: AgentRuntimeConfig) -> Result<AgentRuntime> {
    let AgentRuntimeConfig {
        mut config,
        home,
        cwd,
        session,
        session_layout,
        first_prompt,
        event_sink,
        sub_stream_tx,
        read_only_fs,
        resource_handlers,
        skill_providers,
        runtime_skills,
        skill_discovery_policy,
        llm_backend,
        resource_session_id,
        custom_tools,
    } = config;

    crate::config::validate_runtime_config(&config)?;
    crate::tools::catalog::validate_tool_config(&crate::context::ToolConfig::from_config(&config))?;
    let custom_tools = crate::runtime::tools::freeze_custom_tools(custom_tools);
    crate::tools::catalog::validate_custom_tools(&custom_tools)?;

    let (sid, session_ref, session_alias, resolved_paths) =
        resolve_session(&home, &cwd, session, session_layout).await?;
    if !resolved_paths.metadata.exists()
        && resolved_paths.session_dir.exists()
        && (session_layout != paths::SessionLayout::Isolated
            || crate::session::metadata::session_dir_has_data(&resolved_paths.session_dir).await?)
    {
        bail!(
            "session metadata missing for existing session {}",
            resolved_paths.metadata.display()
        );
    }
    config.session_id = sid.clone();

    let cancel = CancellationToken::new();
    let event_dispatcher = event_sink.map(EventDispatcher::new);
    let event_display = Arc::new(EventDisplay::new(event_dispatcher));
    let display: Arc<dyn crate::ui::Display> = event_display.clone();
    let interrupt = Arc::new(AtomicBool::new(false));
    let api_url_str = api_url(&config);
    let llm_backend =
        llm_backend.unwrap_or_else(|| Arc::new(OpenAiCompatibleBackend::from_config(&config)));
    let resource_session_id = resource_session_id.unwrap_or_else(|| sid.clone());
    let built = build_agent_context(AgentContextBuild {
        config: config.clone(),
        home: home.clone(),
        cwd: cwd.clone(),
        session_id: sid.clone(),
        session_layout,
        resolved_paths: Some(resolved_paths),
        api_url: api_url_str.clone(),
        display: display.clone(),
        sub_stream_tx,
        cancel: cancel.clone(),
        interrupt: interrupt.clone(),
        is_sub_agent: false,
        usage_journal: None,
        read_only_fs,
        resource_session_id,
        resource_handlers,
        skill_providers,
        runtime_skills,
        skill_discovery_policy,
        llm_backend,
        resource_router: None,
        capability_snapshot: None,
        custom_tools,
    })
    .await?;
    let ctx = built.ctx;
    let spaths = built.paths;
    let new_session = built.is_new;

    ctx.compaction.validate_startup().await?;

    crate::session::metadata::ensure_metadata(
        &spaths,
        &cwd,
        SessionSeed {
            alias: session_alias,
            title: first_prompt
                .as_deref()
                .and_then(crate::session::metadata::title_from_prompt),
            first_prompt,
        },
    )
    .await?;

    let (orchestrator, cmd_tx) = new_orchestrator(ctx.clone());
    let orch_display = display.clone();
    let orch_handle = tokio::spawn(async move {
        let result = orchestrator.run().await;
        if let Err(e) = &result {
            orch_display.render_error(&format!("Orchestrator: {e}"));
        }
        result
    });

    if new_session {
        ctx.log_event(serde_json::json!({"type":"session_start","session_id":sid}));
    }
    if let Err(error) = ctx.flush_event_log().await {
        ctx.display
            .render_error(&format!("Event log flush failed: {error}"));
    }

    let session_info = SessionInfo::new(sid, session_ref, new_session, home, cwd, &spaths);
    let handle = crate::runtime::AgentRuntimeHandle {
        cmd_tx,
        session: session_info,
        event_display: event_display.clone(),
        turn_gate: crate::runtime::handle::new_turn_gate(ctx.interrupt.clone()),
        turn_counter: Arc::new(std::sync::atomic::AtomicU64::new(0)),
    };
    Ok(AgentRuntime {
        ctx,
        orch_handle,
        event_display,
        handle,
    })
}

async fn resolve_session(
    home: &std::path::Path,
    cwd: &std::path::Path,
    policy: SessionPolicy,
    layout: paths::SessionLayout,
) -> Result<(String, String, Option<String>, paths::Paths)> {
    match policy {
        SessionPolicy::New => {
            let sid = paths::chrono_session_id();
            let resolved = paths::paths_for_layout(home, cwd, &sid, layout);
            Ok((sid.clone(), sid, None, resolved))
        }
        SessionPolicy::ContinueLatest => {
            let records =
                crate::session::metadata::list_sessions_with_layout(home, cwd, layout).await?;
            if let Some(record) = records.into_iter().max_by_key(|record| record.modified) {
                let sid = record.id;
                let resolved = paths::Paths::from_session_dir(&sid, record.path);
                Ok((sid.clone(), sid, None, resolved))
            } else {
                let sid = paths::chrono_session_id();
                let resolved = paths::paths_for_layout(home, cwd, &sid, layout);
                Ok((sid.clone(), sid, None, resolved))
            }
        }
        SessionPolicy::Resume(reference) => {
            let trimmed = reference.trim();
            if trimmed.is_empty() {
                bail!("invalid empty session reference");
            }
            if let Some(record) = crate::session::metadata::resolve_session_record_with_layout(
                home, cwd, trimmed, layout,
            )
            .await?
            {
                let resolved = record.id;
                let selected = paths::Paths::from_session_dir(&resolved, record.path);
                Ok((resolved, trimmed.to_string(), None, selected))
            } else {
                bail!("session not found: {trimmed}");
            }
        }
        SessionPolicy::UseOrCreate(reference) => {
            let trimmed = reference.trim();
            if trimmed.is_empty() {
                let sid = paths::chrono_session_id();
                let resolved = paths::paths_for_layout(home, cwd, &sid, layout);
                return Ok((sid.clone(), sid, None, resolved));
            }
            if let Some(record) = crate::session::metadata::resolve_session_record_with_layout(
                home, cwd, trimmed, layout,
            )
            .await?
            {
                let resolved = record.id;
                let selected = paths::Paths::from_session_dir(&resolved, record.path);
                Ok((resolved, trimmed.to_string(), None, selected))
            } else {
                let alias = sanitize_alias(trimmed);
                let Some(alias) = alias else {
                    bail!("invalid session name: {trimmed}");
                };
                let sid = if layout == paths::SessionLayout::ProjectScoped {
                    paths::chrono_session_id()
                } else {
                    alias.clone()
                };
                let resolved = paths::paths_for_layout(home, cwd, &sid, layout);
                Ok((sid, trimmed.to_string(), Some(alias), resolved))
            }
        }
    }
}

#[cfg(test)]
#[path = "builder_tests.rs"]
mod tests;
