use crate::agent::orchestrator::new_orchestrator;
use crate::cancel::CancellationToken;
use crate::config::api_url;
use crate::llm::client::OpenAiCompatibleBackend;
use crate::runtime::config::{AgentRuntimeConfig, SessionInfo, SessionPolicy};
use crate::runtime::context_build::{AgentContextBuild, build_agent_context};
use crate::runtime::events::EventDisplay;
use crate::runtime::handle::AgentRuntime;
use crate::session::metadata::{SessionSeed, sanitize_alias};
use crate::session::paths;
use anyhow::{Result, bail};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

pub async fn build_runtime(config: AgentRuntimeConfig) -> Result<AgentRuntime> {
    let AgentRuntimeConfig {
        mut config,
        home,
        cwd,
        session,
        session_layout,
        first_prompt,
        display,
        event_sink,
        sub_stream_tx,
        read_only_fs,
        resource_handlers,
        skill_providers,
        runtime_skills,
        skill_discovery_policy,
        llm_backend,
        resource_session_id,
    } = config;

    crate::config::validate_runtime_config(&config)?;
    crate::tools::catalog::validate_tool_config(&crate::context::ToolConfig::from_config(&config))?;

    let (sid, session_ref, session_alias) =
        resolve_session(&home, &cwd, session, session_layout).await?;
    config.session_id = sid.clone();

    let cancel = CancellationToken::new();
    let event_display = Arc::new(EventDisplay::new(event_sink.clone(), display));
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
    })
    .await?;
    let ctx = built.ctx;
    let spaths = built.paths;
    let new_session = built.is_new;

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
        if let Err(e) = orchestrator.run().await {
            orch_display.render_error(&format!("Orchestrator: {e}"));
        }
    });

    if new_session {
        ctx.log_event(serde_json::json!({"type":"session_start","session_id":sid}));
    }

    let session_info = SessionInfo::new(sid, session_ref, new_session, home, cwd, &spaths);
    Ok(AgentRuntime {
        ctx,
        cmd_tx,
        orch_handle,
        session: session_info,
        event_sink,
        event_display,
        stream_in_progress: Arc::new(AtomicBool::new(false)),
    })
}

async fn resolve_session(
    home: &std::path::Path,
    cwd: &std::path::Path,
    policy: SessionPolicy,
    layout: paths::SessionLayout,
) -> Result<(String, String, Option<String>)> {
    match policy {
        SessionPolicy::New => {
            let sid = paths::chrono_session_id();
            Ok((sid.clone(), sid, None))
        }
        SessionPolicy::ContinueLatest => {
            let sid = paths::continue_session_with_layout(home, cwd, layout)
                .await
                .unwrap_or_default();
            if sid.is_empty() {
                let sid = paths::chrono_session_id();
                Ok((sid.clone(), sid, None))
            } else {
                Ok((sid.clone(), sid, None))
            }
        }
        SessionPolicy::Resume(reference) => {
            let trimmed = reference.trim();
            if trimmed.is_empty() {
                bail!("invalid empty session reference");
            }
            if let Some(resolved) = crate::session::metadata::resolve_session_reference_with_layout(
                home, cwd, trimmed, layout,
            )
            .await?
            {
                Ok((resolved, trimmed.to_string(), None))
            } else {
                bail!("session not found: {trimmed}");
            }
        }
        SessionPolicy::UseOrCreate(reference) => {
            let trimmed = reference.trim();
            if trimmed.is_empty() {
                let sid = paths::chrono_session_id();
                return Ok((sid.clone(), sid, None));
            }
            if let Some(resolved) = crate::session::metadata::resolve_session_reference_with_layout(
                home, cwd, trimmed, layout,
            )
            .await?
            {
                Ok((resolved, trimmed.to_string(), None))
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
                Ok((sid, trimmed.to_string(), Some(alias)))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capabilities::skills::{LoadContext, LoadedSkill, SkillCapability, SkillProvider};
    use crate::capabilities::{CapabilityExposure, SourceLevel, SourceMeta};
    use crate::config::Config;
    use crate::resources::{
        Resource, ResourceContentType, ResourceHandler, ResourceMetadata, ResourceRequest,
    };
    use crate::runtime::{AgentEvent, AgentOptions, EventSink, SkillDiscoveryPolicy};
    use crate::runtime::{
        runtime_skills_from_sdk_request, skill_discovery_policy_from_sdk_request,
    };
    use std::sync::Mutex;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn unique_temp_dir(name: &str) -> std::path::PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        std::env::temp_dir().join(format!("mink-runtime-{name}-{nanos}"))
    }

    fn read_skill_resource(url: &str, ctx: &crate::context::ToolContext) -> anyhow::Result<String> {
        let selection = crate::resources::split_read_path_selection(url)?;
        ctx.resource_router
            .resolve(&selection, ctx)
            .map(|resource| resource.content)
    }

    #[tokio::test]
    async fn build_runtime_initializes_session_paths() {
        let home = unique_temp_dir("home");
        let cwd = unique_temp_dir("cwd");
        tokio::fs::create_dir_all(&cwd).await.unwrap();
        let cfg = Config {
            log_events: true,
            ..Config::default()
        };
        let runtime = build_runtime(AgentRuntimeConfig::from_config(
            cfg,
            home.clone(),
            cwd.clone(),
        ))
        .await
        .unwrap();
        let session = runtime.session_info().clone();

        assert!(!session.session_id.is_empty());
        assert!(session.is_new);
        assert_eq!(session.home, home);
        assert_eq!(session.cwd, cwd);
        assert!(session.events_path.exists());
        assert_eq!(
            session.todos_path.parent(),
            session.events_path.parent(),
            "todo state must live in the resolved session directory"
        );
        assert!(
            std::fs::read_to_string(&session.events_path)
                .unwrap()
                .contains("\"session_start\"")
        );

        runtime.shutdown().await.unwrap();
        let _ = tokio::fs::remove_dir_all(session.home).await;
        let _ = tokio::fs::remove_dir_all(session.cwd).await;
    }

    #[tokio::test]
    async fn build_runtime_rejects_unusable_context_budget_before_session_creation() {
        let home = unique_temp_dir("invalid-context-home");
        let cwd = unique_temp_dir("invalid-context-cwd");
        let cfg = Config {
            max_context_tokens: 64_000,
            ..Config::default()
        };

        let error = build_runtime(AgentRuntimeConfig::from_config(cfg, home.clone(), cwd))
            .await
            .err()
            .expect("invalid context budget must fail")
            .to_string();

        assert!(error.contains("context_reserve_tokens"), "{error}");
        assert!(!home.exists());
    }

    #[tokio::test]
    async fn build_runtime_rejects_unknown_tool_before_session_creation() {
        let home = unique_temp_dir("invalid-tool-home");
        let cwd = unique_temp_dir("invalid-tool-cwd");
        let cfg = Config {
            enabled_tools: Some(vec!["NoSuchTool".into()]),
            ..Config::default()
        };
        let error = build_runtime(AgentRuntimeConfig::from_config(cfg, home.clone(), cwd))
            .await
            .err()
            .expect("unknown tool must fail")
            .to_string();
        assert!(error.contains("unknown tool 'NoSuchTool'"), "{error}");
        assert!(!home.exists());
    }

    #[tokio::test]
    async fn build_runtime_reuses_project_scoped_session_by_alias() {
        let home = unique_temp_dir("alias-home");
        let cwd = unique_temp_dir("alias-cwd");
        tokio::fs::create_dir_all(&cwd).await.unwrap();
        let cfg = Config {
            session_id: "feature x".into(),
            ..Config::default()
        };

        let first = build_runtime(AgentRuntimeConfig::from_config(
            cfg.clone(),
            home.clone(),
            cwd.clone(),
        ))
        .await
        .unwrap();
        let first_session = first.session_info().clone();
        first.shutdown().await.unwrap();

        let metadata: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(first_session.events_path.with_file_name("session.json"))
                .unwrap(),
        )
        .unwrap();
        assert_eq!(metadata["alias"], "feature-x");

        let second = build_runtime(AgentRuntimeConfig::from_config(
            cfg,
            home.clone(),
            cwd.clone(),
        ))
        .await
        .unwrap();
        let second_session = second.session_info().clone();

        assert_eq!(second_session.session_id, first_session.session_id);
        assert!(!second_session.is_new);

        second.shutdown().await.unwrap();
        let _ = tokio::fs::remove_dir_all(home).await;
        let _ = tokio::fs::remove_dir_all(cwd).await;
    }

    #[tokio::test]
    async fn build_runtime_sets_vfs_resource_and_agent_sessions() {
        let home = unique_temp_dir("vfs-scope-home");
        let cwd = unique_temp_dir("vfs-scope-cwd");
        tokio::fs::create_dir_all(&cwd).await.unwrap();
        let mut runtime_config =
            AgentRuntimeConfig::from_config(Config::default(), home.clone(), cwd.clone());
        runtime_config.resource_session_id = Some("tenant-knowledge-7".into());

        let runtime = build_runtime(runtime_config).await.unwrap();
        assert_eq!(
            runtime.ctx.vfs_scope.resource_session_id,
            "tenant-knowledge-7"
        );
        assert_eq!(
            runtime.ctx.vfs_scope.agent_session_id,
            runtime.session_info().session_id
        );

        runtime.shutdown().await.unwrap();
        let _ = tokio::fs::remove_dir_all(home).await;
        let _ = tokio::fs::remove_dir_all(cwd).await;
    }

    #[tokio::test]
    async fn build_runtime_respects_direct_session_layout() {
        let home = unique_temp_dir("direct-home");
        let cwd = unique_temp_dir("direct-cwd");
        tokio::fs::create_dir_all(&cwd).await.unwrap();
        let cfg = Config {
            session_id: "service-session".into(),
            ..Config::default()
        };
        let runtime_config = AgentRuntimeConfig::from_config(cfg, home.clone(), cwd.clone())
            .with_session_layout(paths::SessionLayout::Direct);

        let runtime = build_runtime(runtime_config).await.unwrap();
        let session = runtime.session_info().clone();

        assert_eq!(session.session_id, "service-session");
        assert_eq!(
            session.conversation_path,
            home.join("service-session/conversation.jsonl")
        );
        assert!(
            !session
                .conversation_path
                .starts_with(home.join(".mink/projects"))
        );

        runtime.shutdown().await.unwrap();
        let _ = tokio::fs::remove_dir_all(home).await;
        let _ = tokio::fs::remove_dir_all(cwd).await;
    }

    #[tokio::test]
    async fn build_runtime_respects_home_scoped_session_layout() {
        let home = unique_temp_dir("home-scoped-home");
        let cwd = unique_temp_dir("home-scoped-cwd");
        tokio::fs::create_dir_all(&cwd).await.unwrap();
        let cfg = Config {
            session_id: "sdk-session".into(),
            ..Config::default()
        };
        let runtime_config = AgentRuntimeConfig::from_config(cfg, home.clone(), cwd.clone())
            .with_session_layout(paths::SessionLayout::HomeScoped);

        let runtime = build_runtime(runtime_config).await.unwrap();
        let session = runtime.session_info().clone();

        assert_eq!(session.session_id, "sdk-session");
        assert_eq!(
            session.conversation_path,
            home.join(".mink/sessions/sdk-session/conversation.jsonl")
        );
        assert!(session.conversation_path.exists());

        runtime.shutdown().await.unwrap();
        let _ = tokio::fs::remove_dir_all(home).await;
        let _ = tokio::fs::remove_dir_all(cwd).await;
    }

    #[tokio::test]
    async fn build_runtime_respects_isolated_session_layout() {
        let home = unique_temp_dir("isolated-home");
        let cwd = unique_temp_dir("isolated-cwd");
        tokio::fs::create_dir_all(&cwd).await.unwrap();
        let runtime_config =
            AgentRuntimeConfig::from_config(Config::default(), home.clone(), cwd.clone())
                .with_session_layout(paths::SessionLayout::Isolated);

        let runtime = build_runtime(runtime_config).await.unwrap();
        let session = runtime.session_info().clone();

        assert!(!session.session_id.is_empty());
        assert_eq!(session.conversation_path, home.join("conversation.jsonl"));
        assert_eq!(session.events_path, home.join("events.jsonl"));
        assert!(
            !session
                .conversation_path
                .starts_with(home.join(&session.session_id))
        );

        let metadata: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(home.join("session.json")).unwrap())
                .unwrap();
        assert_eq!(metadata["id"], session.session_id);

        runtime.shutdown().await.unwrap();
        let _ = tokio::fs::remove_dir_all(home).await;
        let _ = tokio::fs::remove_dir_all(cwd).await;
    }

    #[tokio::test]
    async fn runtime_skill_can_be_read() {
        let home = unique_temp_dir("runtime-skill-read-home");
        let cwd = unique_temp_dir("runtime-skill-read-cwd");
        tokio::fs::create_dir_all(&cwd).await.unwrap();
        let runtime_config = AgentOptions::new(&home, &cwd)
            .with_runtime_skill_content("runtime-guide", "Runtime guide", "Runtime body")
            .into_runtime_config();

        let runtime = build_runtime(runtime_config).await.unwrap();
        let tool_ctx = crate::context::ToolContext::from(&*runtime.ctx);
        let content = read_skill_resource("skill://runtime-guide", &tool_ctx).unwrap();

        assert!(content.contains("# skill://runtime-guide"));
        assert!(content.contains("Runtime body"));

        runtime.shutdown().await.unwrap();
        let _ = tokio::fs::remove_dir_all(home).await;
        let _ = tokio::fs::remove_dir_all(cwd).await;
    }

    #[tokio::test]
    async fn runtime_skill_can_be_selected() {
        let home = unique_temp_dir("runtime-skill-selected-home");
        let cwd = unique_temp_dir("runtime-skill-selected-cwd");
        tokio::fs::create_dir_all(&cwd).await.unwrap();
        let runtime_config = AgentOptions::new(&home, &cwd)
            .with_runtime_skill_content("runtime-guide", "Runtime guide", "Runtime body")
            .with_selected_skills(["runtime-guide"])
            .into_runtime_config();

        let runtime = build_runtime(runtime_config).await.unwrap();

        assert_eq!(runtime.ctx.capability_snapshot.skills.selected.len(), 1);
        assert_eq!(
            runtime.ctx.capability_snapshot.skills.selected[0].info.name,
            "runtime-guide"
        );

        runtime.shutdown().await.unwrap();
        let _ = tokio::fs::remove_dir_all(home).await;
        let _ = tokio::fs::remove_dir_all(cwd).await;
    }

    #[tokio::test]
    async fn sdk_inline_skill_can_be_selected_and_read() {
        let home = unique_temp_dir("sdk-inline-skill-home");
        let cwd = unique_temp_dir("sdk-inline-skill-cwd");
        tokio::fs::create_dir_all(&cwd).await.unwrap();
        let req = crate::sdk_protocol::parse_agent_jsonl_request(
            r#"{
                "prompt":"hi",
                "options":{
                    "skills":["company-policy"],
                    "inline_skills":[{
                        "name":"company-policy",
                        "description":"Company policy",
                        "content":"private policy",
                        "exposure":"model_addressable"
                    }],
                    "skill_discovery_policy":"runtime_only"
                }
            }"#,
        )
        .unwrap();
        crate::sdk_protocol::validate_sdk_request(&req).unwrap();
        let mut runtime_config = AgentOptions::new(&home, &cwd).into_runtime_config();
        runtime_config.config.skills = req.options.skills.clone().unwrap();
        runtime_config.runtime_skills = runtime_skills_from_sdk_request(&req);
        runtime_config.skill_discovery_policy =
            skill_discovery_policy_from_sdk_request(&req).unwrap();

        let runtime = build_runtime(runtime_config).await.unwrap();
        let tool_ctx = crate::context::ToolContext::from(&*runtime.ctx);
        let content = read_skill_resource("skill://company-policy", &tool_ctx).unwrap();

        assert!(content.contains("private policy"));
        assert_eq!(runtime.ctx.capability_snapshot.skills.selected.len(), 1);
        assert!(
            !runtime
                .ctx
                .capability_snapshot
                .skills
                .discoverable
                .iter()
                .any(|skill| skill.skill.name == "company-policy")
        );
        assert!(
            !runtime
                .ctx
                .capability_snapshot
                .skills
                .by_name
                .contains_key("debugging")
        );

        runtime.shutdown().await.unwrap();
        let _ = tokio::fs::remove_dir_all(home).await;
        let _ = tokio::fs::remove_dir_all(cwd).await;
    }

    #[tokio::test]
    async fn runtime_only_policy_does_not_load_filesystem_or_builtin() {
        let home = unique_temp_dir("runtime-only-home");
        let cwd = unique_temp_dir("runtime-only-cwd");
        tokio::fs::create_dir_all(cwd.join("skills/local-only"))
            .await
            .unwrap();
        tokio::fs::write(
            cwd.join("skills/local-only/SKILL.md"),
            "---\ndescription: \"Local only\"\n---\n\nlocal body",
        )
        .await
        .unwrap();
        let runtime_config = AgentOptions::new(&home, &cwd)
            .with_runtime_skill_content("runtime-only", "Runtime only", "runtime body")
            .with_skill_discovery_policy(SkillDiscoveryPolicy::RuntimeOnly)
            .into_runtime_config();

        let runtime = build_runtime(runtime_config).await.unwrap();
        let snapshot = &runtime.ctx.capability_snapshot.skills;

        assert!(snapshot.by_name.contains_key("runtime-only"));
        assert!(!snapshot.by_name.contains_key("local-only"));
        assert!(!snapshot.by_name.contains_key("debugging"));

        runtime.shutdown().await.unwrap();
        let _ = tokio::fs::remove_dir_all(home).await;
        let _ = tokio::fs::remove_dir_all(cwd).await;
    }

    struct RuntimeTestSkillProvider;

    impl SkillProvider for RuntimeTestSkillProvider {
        fn id(&self) -> &'static str {
            "runtime-test-skills"
        }

        fn display_name(&self) -> &'static str {
            "runtime test skills"
        }

        fn priority(&self) -> i32 {
            250
        }

        fn load_skills(&self, _ctx: &LoadContext<'_>) -> Result<Vec<LoadedSkill>> {
            Ok(vec![LoadedSkill {
                skill: SkillCapability {
                    name: "provider-guide".to_string(),
                    description: "Provider guide".to_string(),
                    content: "provider body".to_string(),
                    base_dir: "<provider>".to_string(),
                    disable_model_invocation: false,
                },
                source: SourceMeta {
                    provider_id: self.id().to_string(),
                    provider_name: self.display_name().to_string(),
                    level: SourceLevel::Runtime,
                    source_path: None,
                    display_label: Some("provider".to_string()),
                },
                exposure: CapabilityExposure::ModelDiscoverable,
                revision: "provider-rev-1".to_string(),
            }])
        }
    }

    #[tokio::test]
    async fn explicit_skill_provider_can_be_read() {
        let home = unique_temp_dir("explicit-provider-home");
        let cwd = unique_temp_dir("explicit-provider-cwd");
        tokio::fs::create_dir_all(&cwd).await.unwrap();
        let runtime_config = AgentOptions::new(&home, &cwd)
            .with_skill_discovery_policy(SkillDiscoveryPolicy::ExplicitOnly)
            .with_skill_provider(Arc::new(RuntimeTestSkillProvider))
            .into_runtime_config();

        let runtime = build_runtime(runtime_config).await.unwrap();
        let tool_ctx = crate::context::ToolContext::from(&*runtime.ctx);
        let content = read_skill_resource("skill://provider-guide", &tool_ctx).unwrap();

        assert!(content.contains("provider body"));
        assert!(
            !runtime
                .ctx
                .capability_snapshot
                .skills
                .by_name
                .contains_key("debugging")
        );

        runtime.shutdown().await.unwrap();
        let _ = tokio::fs::remove_dir_all(home).await;
        let _ = tokio::fs::remove_dir_all(cwd).await;
    }

    struct KbResourceHandler;

    impl ResourceHandler for KbResourceHandler {
        fn scheme(&self) -> &'static str {
            "kb"
        }

        fn resolve(
            &self,
            req: &ResourceRequest,
            _ctx: &crate::context::ToolContext,
        ) -> Result<Resource> {
            Ok(Resource {
                canonical_url: req.resource_url.clone(),
                content: format!("kb authority={} path={}", req.authority, req.path),
                content_type: ResourceContentType::PlainText,
                immutable: Some(true),
                metadata: ResourceMetadata::default(),
            })
        }
    }

    #[tokio::test]
    async fn custom_resource_handler_is_registered() {
        let home = unique_temp_dir("resource-handler-home");
        let cwd = unique_temp_dir("resource-handler-cwd");
        tokio::fs::create_dir_all(&cwd).await.unwrap();
        let runtime_config = AgentOptions::new(&home, &cwd)
            .with_resource_handler(Arc::new(KbResourceHandler))
            .into_runtime_config();

        let runtime = build_runtime(runtime_config).await.unwrap();
        let tool_ctx = crate::context::ToolContext::from(&*runtime.ctx);
        let selection = crate::resources::split_read_path_selection("kb://tenant/doc").unwrap();
        let resource = runtime
            .ctx
            .resource_router
            .resolve(&selection, &tool_ctx)
            .unwrap();

        assert_eq!(resource.content, "kb authority=tenant path=doc");

        runtime.shutdown().await.unwrap();
        let _ = tokio::fs::remove_dir_all(home).await;
        let _ = tokio::fs::remove_dir_all(cwd).await;
    }

    #[tokio::test]
    async fn defaults_policy_keeps_existing_skill_behavior() {
        let home = unique_temp_dir("defaults-policy-home");
        let cwd = unique_temp_dir("defaults-policy-cwd");
        tokio::fs::create_dir_all(&cwd).await.unwrap();
        let runtime = build_runtime(AgentOptions::new(&home, &cwd).into_runtime_config())
            .await
            .unwrap();

        assert!(
            runtime
                .ctx
                .capability_snapshot
                .skills
                .by_name
                .contains_key("debugging")
        );

        runtime.shutdown().await.unwrap();
        let _ = tokio::fs::remove_dir_all(home).await;
        let _ = tokio::fs::remove_dir_all(cwd).await;
    }

    #[tokio::test]
    async fn multiple_runtimes_do_not_share_skill_snapshot() {
        let home_a = unique_temp_dir("runtime-a-home");
        let cwd_a = unique_temp_dir("runtime-a-cwd");
        let home_b = unique_temp_dir("runtime-b-home");
        let cwd_b = unique_temp_dir("runtime-b-cwd");
        tokio::fs::create_dir_all(&cwd_a).await.unwrap();
        tokio::fs::create_dir_all(&cwd_b).await.unwrap();
        let runtime_a = build_runtime(
            AgentOptions::new(&home_a, &cwd_a)
                .with_runtime_skill_content("runtime-a", "Runtime A", "body a")
                .into_runtime_config(),
        )
        .await
        .unwrap();
        let runtime_b = build_runtime(
            AgentOptions::new(&home_b, &cwd_b)
                .with_runtime_skill_content("runtime-b", "Runtime B", "body b")
                .into_runtime_config(),
        )
        .await
        .unwrap();

        assert!(
            runtime_a
                .ctx
                .capability_snapshot
                .skills
                .by_name
                .contains_key("runtime-a")
        );
        assert!(
            !runtime_a
                .ctx
                .capability_snapshot
                .skills
                .by_name
                .contains_key("runtime-b")
        );
        assert!(
            runtime_b
                .ctx
                .capability_snapshot
                .skills
                .by_name
                .contains_key("runtime-b")
        );
        assert!(!Arc::ptr_eq(
            &runtime_a.ctx.capability_snapshot,
            &runtime_b.ctx.capability_snapshot
        ));

        runtime_a.shutdown().await.unwrap();
        runtime_b.shutdown().await.unwrap();
        let _ = tokio::fs::remove_dir_all(home_a).await;
        let _ = tokio::fs::remove_dir_all(cwd_a).await;
        let _ = tokio::fs::remove_dir_all(home_b).await;
        let _ = tokio::fs::remove_dir_all(cwd_b).await;
    }

    #[derive(Default)]
    struct RecordingSink {
        events: Mutex<Vec<AgentEvent>>,
    }

    impl EventSink for RecordingSink {
        fn on_event(&self, event: AgentEvent) {
            self.events.lock().unwrap().push(event);
        }
    }

    #[tokio::test]
    async fn runtime_event_sink_observes_existing_display_path() {
        let home = unique_temp_dir("event-home");
        let cwd = unique_temp_dir("event-cwd");
        tokio::fs::create_dir_all(&cwd).await.unwrap();
        let sink = Arc::new(RecordingSink::default());
        let cfg = Config {
            log_events: true,
            ..Config::default()
        };
        let runtime = build_runtime(
            AgentRuntimeConfig::from_config(cfg, home.clone(), cwd.clone())
                .with_event_sink(sink.clone()),
        )
        .await
        .unwrap();

        runtime.compact().await.unwrap();
        runtime.shutdown().await.unwrap();

        {
            let events = sink.events.lock().unwrap();
            assert!(events.iter().any(|event| matches!(
                event,
                AgentEvent::Info { message } if message == "Compressing..."
            )));
            assert!(
                events
                    .iter()
                    .any(|event| matches!(event, AgentEvent::Stop { .. }))
            );
        }

        let _ = tokio::fs::remove_dir_all(home).await;
        let _ = tokio::fs::remove_dir_all(cwd).await;
    }

    // ── Mock LLM runtime integration tests ──────────────────────────

    /// Build a minimal mock LLM that returns Text + Stop for each call.
    fn mock_llm_hello() -> crate::llm::mock::MockLlmClient {
        use crate::protocol::{Event, StopEvent, TextEvent};
        crate::llm::mock::MockLlmClient::new(
            "flash",
            vec![
                vec![
                    Ok(Event::Text(TextEvent {
                        content: "Hello, world!".into(),
                    })),
                    Ok(Event::Stop(StopEvent {
                        reason: "end_turn".into(),
                    })),
                ],
                vec![
                    Ok(Event::Text(TextEvent {
                        content: "second".into(),
                    })),
                    Ok(Event::Stop(StopEvent {
                        reason: "end_turn".into(),
                    })),
                ],
                vec![
                    Ok(Event::Text(TextEvent {
                        content: "third".into(),
                    })),
                    Ok(Event::Stop(StopEvent {
                        reason: "end_turn".into(),
                    })),
                ],
            ],
        )
    }

    fn mock_llm_with_usage() -> crate::llm::mock::MockLlmClient {
        use crate::protocol::{Event, StopEvent, TextEvent, UsageEvent};
        crate::llm::mock::MockLlmClient::new(
            "flash",
            vec![vec![
                Ok(Event::Text(TextEvent {
                    content: "measured".into(),
                })),
                Ok(Event::Usage(UsageEvent {
                    input_tokens: 100,
                    output_tokens: 20,
                    cache_read_input_tokens: 40,
                    cache_creation_input_tokens: 0,
                })),
                Ok(Event::Stop(StopEvent {
                    reason: "end_turn".into(),
                })),
            ]],
        )
    }

    fn runtime_config_with_mock(
        home: &std::path::Path,
        cwd: &std::path::Path,
        mock: crate::llm::mock::MockLlmClient,
    ) -> AgentRuntimeConfig {
        let cfg = Config {
            model: "flash".into(),
            api_key: "test-key".into(),
            base_url: "https://example.invalid/v1".into(),
            max_context_tokens: 1_000_000,
            log_events: true,
            ..Config::default()
        };
        let mut rt_config =
            AgentRuntimeConfig::from_config(cfg, home.to_path_buf(), cwd.to_path_buf());
        rt_config.llm_backend = Some(Arc::new(mock));
        rt_config
    }

    #[tokio::test]
    async fn run_turn_with_mock_llm_returns_ok_outcome() {
        let home = unique_temp_dir("mock-hello-home");
        let cwd = unique_temp_dir("mock-hello-cwd");
        tokio::fs::create_dir_all(&cwd).await.unwrap();

        let runtime = build_runtime(runtime_config_with_mock(&home, &cwd, mock_llm_hello()))
            .await
            .unwrap();

        let outcome = runtime.run_turn("say hello").await.unwrap();
        assert_eq!(outcome.status, crate::agent::orchestrator::TurnStatus::Ok);
        assert_eq!(outcome.tool_call_count, 0);
        assert_eq!(outcome.tool_error_count, 0);
        assert!(outcome.error.is_none());

        runtime.shutdown().await.unwrap();
        let _ = tokio::fs::remove_dir_all(home).await;
        let _ = tokio::fs::remove_dir_all(cwd).await;
    }

    type SeenModels = Arc<Mutex<Vec<(String, Option<String>)>>>;

    struct RecordingBackend {
        seen: SeenModels,
    }

    #[async_trait::async_trait]
    impl crate::runtime::LlmBackend for RecordingBackend {
        fn name(&self) -> &str {
            "recording"
        }

        async fn stream(
            &self,
            request: crate::runtime::LlmRequest,
        ) -> anyhow::Result<crate::runtime::LlmResponseStream> {
            self.seen
                .lock()
                .unwrap()
                .push((request.model, request.model_alias));
            Ok(crate::runtime::LlmResponseStream {
                events: Box::pin(futures::stream::iter(vec![
                    Ok(crate::runtime::LlmEvent::Text(
                        crate::runtime::LlmTextEvent {
                            content: "ok".into(),
                        },
                    )),
                    Ok(crate::runtime::LlmEvent::Stop(
                        crate::runtime::LlmStopEvent {
                            reason: "end_turn".into(),
                        },
                    )),
                ])),
                attempt_count: 1,
            })
        }
    }

    #[tokio::test]
    async fn injected_backend_receives_resolved_model_alias() {
        let home = unique_temp_dir("backend-alias-home");
        let cwd = unique_temp_dir("backend-alias-cwd");
        tokio::fs::create_dir_all(&cwd).await.unwrap();

        let seen = Arc::new(Mutex::new(Vec::new()));
        let mut aliases = std::collections::BTreeMap::new();
        aliases.insert("flash".to_string(), "local-fast".to_string());
        let cfg = Config {
            model: "flash".into(),
            model_aliases: aliases,
            api_key: "test-key".into(),
            base_url: "https://example.invalid/v1".into(),
            max_context_tokens: 1_000_000,
            ..Config::default()
        };
        let runtime_config = AgentRuntimeConfig::from_config(cfg, home.clone(), cwd.clone())
            .with_llm_backend(Arc::new(RecordingBackend { seen: seen.clone() }));
        let runtime = build_runtime(runtime_config).await.unwrap();

        let outcome = runtime.run_turn("hi").await.unwrap();
        assert_eq!(outcome.status, crate::agent::orchestrator::TurnStatus::Ok);
        assert_eq!(
            seen.lock().unwrap().as_slice(),
            &[("local-fast".to_string(), Some("flash".to_string()))]
        );

        runtime.shutdown().await.unwrap();
        let _ = tokio::fs::remove_dir_all(home).await;
        let _ = tokio::fs::remove_dir_all(cwd).await;
    }

    #[tokio::test]
    async fn agent_options_default_model_resolves_to_flash_for_injected_backend() {
        let home = unique_temp_dir("backend-default-model-home");
        let cwd = unique_temp_dir("backend-default-model-cwd");
        tokio::fs::create_dir_all(&cwd).await.unwrap();

        let seen = Arc::new(Mutex::new(Vec::new()));
        let runtime_config = crate::runtime::AgentOptions::new(home.clone(), cwd.clone())
            .with_llm_backend(Arc::new(RecordingBackend { seen: seen.clone() }))
            .into_runtime_config();
        let runtime = build_runtime(runtime_config).await.unwrap();

        let outcome = runtime.run_turn("hi").await.unwrap();
        assert_eq!(outcome.status, crate::agent::orchestrator::TurnStatus::Ok);
        assert_eq!(
            seen.lock().unwrap().as_slice(),
            &[("deepseek-v4-flash".to_string(), Some("flash".to_string()))]
        );

        runtime.shutdown().await.unwrap();
        let _ = tokio::fs::remove_dir_all(home).await;
        let _ = tokio::fs::remove_dir_all(cwd).await;
    }

    #[tokio::test]
    async fn run_turn_returns_usage_records_from_the_session_journal() {
        let home = unique_temp_dir("mock-usage-home");
        let cwd = unique_temp_dir("mock-usage-cwd");
        tokio::fs::create_dir_all(&cwd).await.unwrap();
        let runtime = build_runtime(runtime_config_with_mock(&home, &cwd, mock_llm_with_usage()))
            .await
            .unwrap();

        let outcome = runtime.run_turn("measure").await.unwrap();
        assert!(!outcome.billing_turn_id.is_empty());
        assert_eq!(outcome.usage_records.len(), 1);
        assert_eq!(outcome.usage.request_count, 1);
        assert_eq!(outcome.usage.tokens.input_tokens, 100);
        assert_eq!(outcome.usage.tokens.cache_read_tokens, 40);
        assert_eq!(outcome.usage.tokens.output_tokens, 20);
        assert_eq!(outcome.usage.cost_nano_cny, 140_800);
        assert_eq!(
            outcome.usage_records[0].billing_turn_id,
            outcome.billing_turn_id
        );
        assert!(outcome.session.usage_path.exists());

        runtime.shutdown().await.unwrap();
        let _ = tokio::fs::remove_dir_all(home).await;
        let _ = tokio::fs::remove_dir_all(cwd).await;
    }

    #[tokio::test]
    async fn run_turn_with_mock_llm_emits_text_and_final_events() {
        let home = unique_temp_dir("mock-events-home");
        let cwd = unique_temp_dir("mock-events-cwd");
        tokio::fs::create_dir_all(&cwd).await.unwrap();
        let sink = Arc::new(RecordingSink::default());

        let mut rt_config = runtime_config_with_mock(&home, &cwd, mock_llm_hello());
        rt_config.event_sink = Some(sink.clone());

        let runtime = build_runtime(rt_config).await.unwrap();
        runtime.run_turn("say hello").await.unwrap();
        runtime.shutdown().await.unwrap();

        {
            let events = sink.events.lock().unwrap();
            assert!(
                events.iter().any(|event| matches!(
                    event,
                    AgentEvent::Text { content } if content == "Hello, world!"
                )),
                "expected Text event with greeting"
            );
            assert!(
                events.iter().any(|event| matches!(
                    event,
                    AgentEvent::Final { outcome } if outcome.status == crate::agent::orchestrator::TurnStatus::Ok
                )),
                "expected Final event with Ok status"
            );
        }

        let _ = tokio::fs::remove_dir_all(home).await;
        let _ = tokio::fs::remove_dir_all(cwd).await;
    }

    #[tokio::test]
    async fn stream_turn_without_event_sink_emits_text_and_final_events() {
        let home = unique_temp_dir("mock-stream-home");
        let cwd = unique_temp_dir("mock-stream-cwd");
        tokio::fs::create_dir_all(&cwd).await.unwrap();

        let runtime = build_runtime(runtime_config_with_mock(&home, &cwd, mock_llm_hello()))
            .await
            .unwrap();

        let mut stream = runtime.stream_turn("say hello");
        let mut saw_text = false;
        let mut saw_final = false;
        while let Some(event) =
            tokio::time::timeout(tokio::time::Duration::from_secs(2), stream.recv())
                .await
                .expect("stream event timed out")
        {
            match event {
                AgentEvent::Text { content } if content == "Hello, world!" => {
                    saw_text = true;
                }
                AgentEvent::Final { outcome } => {
                    assert_eq!(outcome.status, crate::agent::orchestrator::TurnStatus::Ok);
                    saw_final = true;
                    break;
                }
                _ => {}
            }
        }

        assert!(saw_text, "expected streaming Text event without EventSink");
        assert!(saw_final, "expected streaming Final event");
        let outcome = stream.outcome().await.unwrap();
        assert_eq!(outcome.status, crate::agent::orchestrator::TurnStatus::Ok);

        runtime.shutdown().await.unwrap();
        let _ = tokio::fs::remove_dir_all(home).await;
        let _ = tokio::fs::remove_dir_all(cwd).await;
    }

    #[tokio::test]
    async fn stream_outcome_succeeds_without_draining_events() {
        let home = unique_temp_dir("mock-stream-outcome-home");
        let cwd = unique_temp_dir("mock-stream-outcome-cwd");
        tokio::fs::create_dir_all(&cwd).await.unwrap();

        let runtime = build_runtime(runtime_config_with_mock(&home, &cwd, mock_llm_hello()))
            .await
            .unwrap();

        let stream = runtime.stream_turn("say hello");
        let outcome = stream.outcome().await.unwrap();
        assert_eq!(outcome.status, crate::agent::orchestrator::TurnStatus::Ok);
        assert!(outcome.text.contains("Hello, world!"));

        runtime.shutdown().await.unwrap();
        let _ = tokio::fs::remove_dir_all(home).await;
        let _ = tokio::fs::remove_dir_all(cwd).await;
    }

    #[tokio::test]
    async fn try_stream_turn_reports_concurrent_turn_as_error() {
        let home = unique_temp_dir("mock-stream-concurrent-home");
        let cwd = unique_temp_dir("mock-stream-concurrent-cwd");
        tokio::fs::create_dir_all(&cwd).await.unwrap();

        let runtime = build_runtime(runtime_config_with_mock(&home, &cwd, mock_llm_hello()))
            .await
            .unwrap();

        let stream = runtime.try_stream_turn("say hello").unwrap();
        let err = match runtime.try_stream_turn("second stream") {
            Ok(_) => panic!("concurrent stream should fail"),
            Err(err) => err,
        };
        assert!(err.to_string().contains("stream_turn already in progress"));

        let outcome = stream.outcome().await.unwrap();
        assert_eq!(outcome.status, crate::agent::orchestrator::TurnStatus::Ok);

        runtime.shutdown().await.unwrap();
        let _ = tokio::fs::remove_dir_all(home).await;
        let _ = tokio::fs::remove_dir_all(cwd).await;
    }

    /// Run three consecutive turns with a mock LLM to verify the runtime
    /// can handle successive completions without state corruption.
    #[tokio::test]
    async fn consecutive_turns_with_mock_llm_all_succeed() {
        let home = unique_temp_dir("mock-consecutive-home");
        let cwd = unique_temp_dir("mock-consecutive-cwd");
        tokio::fs::create_dir_all(&cwd).await.unwrap();

        let runtime = build_runtime(runtime_config_with_mock(&home, &cwd, mock_llm_hello()))
            .await
            .unwrap();

        for msg in ["first", "second", "third"] {
            let outcome = runtime.run_turn(msg).await.unwrap();
            assert_eq!(outcome.status, crate::agent::orchestrator::TurnStatus::Ok);
            assert!(outcome.error.is_none());
        }

        runtime.shutdown().await.unwrap();
        let _ = tokio::fs::remove_dir_all(home).await;
        let _ = tokio::fs::remove_dir_all(cwd).await;
    }

    // ── Interrupt test with blocking mock LLM ──────────────────────

    /// A mock LLM whose stream never yields, used to test interrupt.
    /// A mock LLM whose first `stream()` call returns a never-yielding
    /// stream (for testing interrupt), and subsequent calls return a normal
    /// Text+Stop (for testing recovery).
    struct InterruptTestMockLlmClient {
        calls: std::sync::Mutex<u32>,
    }

    #[async_trait::async_trait]
    impl crate::llm::client::LlmBackend for InterruptTestMockLlmClient {
        fn name(&self) -> &str {
            "interrupt-test"
        }
        async fn stream(
            &self,
            _request: crate::runtime::LlmRequest,
        ) -> anyhow::Result<crate::runtime::LlmResponseStream> {
            let mut c = self.calls.lock().unwrap();
            *c += 1;
            if *c == 1 {
                Ok(crate::runtime::LlmResponseStream {
                    events: Box::pin(futures::stream::pending()),
                    attempt_count: 1,
                })
            } else {
                use crate::protocol::{Event, StopEvent, TextEvent};
                Ok(crate::runtime::LlmResponseStream {
                    events: Box::pin(futures::stream::iter(vec![
                        Ok(Event::Text(TextEvent {
                            content: "recovered".into(),
                        })),
                        Ok(Event::Stop(StopEvent {
                            reason: "end_turn".into(),
                        })),
                    ])),
                    attempt_count: 1,
                })
            }
        }
    }

    fn runtime_config_with_blocking_mock(
        home: &std::path::Path,
        cwd: &std::path::Path,
    ) -> AgentRuntimeConfig {
        let cfg = Config {
            model: "flash".into(),
            api_key: "test-key".into(),
            base_url: "https://example.invalid/v1".into(),
            max_context_tokens: 1_000_000,
            log_events: true,
            ..Config::default()
        };
        let mut rt_config =
            AgentRuntimeConfig::from_config(cfg, home.to_path_buf(), cwd.to_path_buf());
        rt_config.llm_backend = Some(Arc::new(InterruptTestMockLlmClient {
            calls: std::sync::Mutex::new(0),
        }));
        rt_config
    }

    #[tokio::test]
    async fn interrupt_mid_turn_returns_interrupted_and_next_turn_works() {
        let home = unique_temp_dir("mock-int-home");
        let cwd = unique_temp_dir("mock-int-cwd");
        tokio::fs::create_dir_all(&cwd).await.unwrap();

        let runtime = build_runtime(runtime_config_with_blocking_mock(&home, &cwd))
            .await
            .unwrap();

        // Snapshot the shared interrupt flag before moving the runtime.
        let interrupt_flag = runtime.interrupt_flag();

        // Wrap runtime so the spawned task can take ownership and return it.
        let turn_rt = std::sync::Arc::new(tokio::sync::Mutex::new(Some(runtime)));
        let turn_rt_clone = turn_rt.clone();

        let handle = tokio::spawn(async move {
            let runtime = turn_rt_clone.lock().await.take().unwrap();
            let outcome = runtime.run_turn("blocking turn").await;
            (runtime, outcome)
        });

        // Let the orchestrator enter the LLM stream loop.
        tokio::time::sleep(tokio::time::Duration::from_millis(200)).await;

        // The orchestrator's turn executor polls this flag every 25 ms.
        interrupt_flag.store(true, std::sync::atomic::Ordering::SeqCst);

        let (runtime, outcome) = handle.await.unwrap();
        let outcome = outcome.unwrap();
        assert_eq!(
            outcome.status,
            crate::agent::orchestrator::TurnStatus::Interrupted
        );

        // Next turn must still run successfully.
        let outcome2 = runtime.run_turn("recovery turn").await.unwrap();
        assert_eq!(outcome2.status, crate::agent::orchestrator::TurnStatus::Ok);

        runtime.shutdown().await.unwrap();
        let _ = tokio::fs::remove_dir_all(home).await;
        let _ = tokio::fs::remove_dir_all(cwd).await;
    }

    // ── Mock LLM with tool-use test ───────────────────────────────

    /// Mock LLM that exercises the full tool-execution pipeline:
    /// first turn returns a Bash tool call, second turn returns Text+Stop.
    fn mock_llm_tool_use() -> crate::llm::mock::MockLlmClient {
        use crate::protocol::{Event, StopEvent, TextEvent, ToolCallEvent};
        use serde_json::json;
        let mut fields = std::collections::BTreeMap::new();
        fields.insert("command".into(), "echo hello".into());
        crate::llm::mock::MockLlmClient::new(
            "flash",
            vec![
                // First LLM call: request a Bash tool execution
                vec![
                    Ok(Event::ToolCall(ToolCallEvent {
                        name: "Bash".into(),
                        id: "call_bash_1".into(),
                        input_json: json!({"command": "echo hello"}),
                        fields,
                        order: vec!["command".into()],
                    })),
                    Ok(Event::Stop(StopEvent {
                        reason: "tool_use".into(),
                    })),
                ],
                // Second LLM call: text response after tool execution
                vec![
                    Ok(Event::Text(TextEvent {
                        content: "all done".into(),
                    })),
                    Ok(Event::Stop(StopEvent {
                        reason: "end_turn".into(),
                    })),
                ],
            ],
        )
    }

    #[tokio::test]
    async fn tool_use_turn_executes_tool_and_returns_outcome() {
        let home = unique_temp_dir("mock-tool-home");
        let cwd = unique_temp_dir("mock-tool-cwd");
        tokio::fs::create_dir_all(&cwd).await.unwrap();

        let runtime = build_runtime(runtime_config_with_mock(&home, &cwd, mock_llm_tool_use()))
            .await
            .unwrap();

        let outcome = runtime.run_turn("run echo hello").await.unwrap();
        assert_eq!(outcome.status, crate::agent::orchestrator::TurnStatus::Ok);
        // At least one tool was executed
        assert!(
            outcome.tool_call_count >= 1,
            "expected at least 1 tool call, got {}",
            outcome.tool_call_count
        );
        // The LLM response after tool execution
        assert!(outcome.text.contains("all done"));

        runtime.shutdown().await.unwrap();
        let _ = tokio::fs::remove_dir_all(home).await;
        let _ = tokio::fs::remove_dir_all(cwd).await;
    }

    #[tokio::test]
    async fn tool_use_turn_emits_tool_call_and_tool_result_events() {
        let home = unique_temp_dir("mock-tool-ev-home");
        let cwd = unique_temp_dir("mock-tool-ev-cwd");
        tokio::fs::create_dir_all(&cwd).await.unwrap();
        let sink = Arc::new(RecordingSink::default());

        let mut rt_config = runtime_config_with_mock(&home, &cwd, mock_llm_tool_use());
        rt_config.event_sink = Some(sink.clone());

        let runtime = build_runtime(rt_config).await.unwrap();
        runtime.run_turn("run echo hello").await.unwrap();
        runtime.shutdown().await.unwrap();

        let (has_tool_call, has_tool_result, has_final) = {
            let events = sink.events.lock().unwrap();
            (
                events
                    .iter()
                    .any(|e| matches!(e, AgentEvent::ToolCall { .. })),
                events
                    .iter()
                    .any(|e| matches!(e, AgentEvent::ToolResult { .. })),
                events.iter().any(|e| matches!(e, AgentEvent::Final { .. })),
            )
        };

        assert!(has_tool_call, "expected ToolCall event");
        assert!(has_tool_result, "expected ToolResult event");
        assert!(has_final, "expected Final event");

        let _ = tokio::fs::remove_dir_all(home).await;
        let _ = tokio::fs::remove_dir_all(cwd).await;
    }

    #[tokio::test]
    async fn stream_turn_without_event_sink_emits_tool_events() {
        let home = unique_temp_dir("mock-tool-stream-home");
        let cwd = unique_temp_dir("mock-tool-stream-cwd");
        tokio::fs::create_dir_all(&cwd).await.unwrap();

        let runtime = build_runtime(runtime_config_with_mock(&home, &cwd, mock_llm_tool_use()))
            .await
            .unwrap();

        let mut stream = runtime.stream_turn("run echo hello");
        let mut saw_tool_call = false;
        let mut saw_tool_result = false;
        while let Some(event) =
            tokio::time::timeout(tokio::time::Duration::from_secs(10), stream.recv())
                .await
                .expect("stream event timed out")
        {
            match event {
                AgentEvent::ToolCall { .. } => saw_tool_call = true,
                AgentEvent::ToolResult { .. } => saw_tool_result = true,
                AgentEvent::Final { .. } => break,
                _ => {}
            }
        }

        let outcome = stream.outcome().await.unwrap();
        assert_eq!(outcome.status, crate::agent::orchestrator::TurnStatus::Ok);
        assert!(saw_tool_call, "expected streaming ToolCall event");
        assert!(saw_tool_result, "expected streaming ToolResult event");

        runtime.shutdown().await.unwrap();
        let _ = tokio::fs::remove_dir_all(home).await;
        let _ = tokio::fs::remove_dir_all(cwd).await;
    }
}
