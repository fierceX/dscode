use super::*;
use crate::capabilities::skills::{LoadContext, LoadedSkill, SkillCapability, SkillProvider};
use crate::capabilities::{CapabilityExposure, SourceLevel, SourceMeta};
use crate::config::ResolvedConfig as Config;
use crate::resources::{
    ResourceHandler,
    router::{Resource, ResourceRequest},
};
use crate::runtime::{
    AgentEvent, AgentOptions, AgentTool, EventSink, SkillDiscoveryPolicy, ToolDefinition,
    ToolError, ToolExecutionContext, ToolExecutionMode, ToolOutput,
};
use crate::runtime::{runtime_skills_from_sdk_request, skill_discovery_policy_from_sdk_request};
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
    let selection = crate::resources::selector::split_read_path_selection(url)?;
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
        &std::fs::read_to_string(first_session.events_path.with_file_name("session.json")).unwrap(),
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

async fn write_session_metadata(dir: &std::path::Path, id: &str, cwd: &std::path::Path) {
    tokio::fs::create_dir_all(dir).await.unwrap();
    let metadata = crate::session::metadata::SessionMetadata {
        id: id.into(),
        alias: Some("legacy-alias".into()),
        title: None,
        created_at: "2026-01-01T00:00:00Z".into(),
        updated_at: "2026-01-01T00:00:00Z".into(),
        cwd: cwd.display().to_string(),
        parent: None,
        first_prompt: None,
        summary: None,
    };
    tokio::fs::write(
        dir.join("session.json"),
        serde_json::to_vec_pretty(&metadata).unwrap(),
    )
    .await
    .unwrap();
}

#[tokio::test]
async fn project_scoped_resume_reads_legacy_location_and_writes_in_place() {
    let home = unique_temp_dir("legacy-resume-home");
    let cwd = unique_temp_dir("legacy-resume-cwd");
    tokio::fs::create_dir_all(&cwd).await.unwrap();
    let id = "legacy-session";
    let old_dir = home
        .join(".mink/projects")
        .join(crate::session::paths::legacy_project_key(&cwd))
        .join(id);
    write_session_metadata(&old_dir, id, &cwd).await;

    let mut config = runtime_config_with_mock(&home, &cwd, mock_llm_hello());
    config.session = SessionPolicy::Resume(id.into());
    let runtime = build_runtime(config).await.unwrap();
    assert_eq!(
        runtime.session_info().conversation_path.parent(),
        Some(old_dir.as_path())
    );
    runtime.run_turn("continue legacy").await.unwrap();
    runtime.shutdown().await.unwrap();
    assert!(old_dir.join("conversation.jsonl").is_file());
    let new_dir = home
        .join(".mink/projects")
        .join(crate::session::paths::project_key(&cwd))
        .join(id);
    assert!(!new_dir.exists());

    let _ = tokio::fs::remove_dir_all(home).await;
    let _ = tokio::fs::remove_dir_all(cwd).await;
}

#[tokio::test]
async fn project_scoped_resume_fails_when_new_and_legacy_locations_collide() {
    let home = unique_temp_dir("legacy-ambiguous-home");
    let cwd = unique_temp_dir("legacy-ambiguous-cwd");
    tokio::fs::create_dir_all(&cwd).await.unwrap();
    let id = "duplicate-session";
    for key in [
        crate::session::paths::project_key(&cwd),
        crate::session::paths::legacy_project_key(&cwd),
    ] {
        write_session_metadata(&home.join(".mink/projects").join(key).join(id), id, &cwd).await;
    }

    let mut config = runtime_config_with_mock(&home, &cwd, mock_llm_hello());
    config.session = SessionPolicy::Resume(id.into());
    let error = build_runtime(config).await.err().unwrap().to_string();
    assert!(error.contains("ambiguous across"), "{error}");
    assert!(error.contains(&crate::session::paths::project_key(&cwd)));
    assert!(error.contains(&crate::session::paths::legacy_project_key(&cwd)));

    let _ = tokio::fs::remove_dir_all(home).await;
    let _ = tokio::fs::remove_dir_all(cwd).await;
}

#[tokio::test]
async fn continue_latest_propagates_corrupt_metadata() {
    let home = unique_temp_dir("continue-corrupt-home");
    let cwd = unique_temp_dir("continue-corrupt-cwd");
    tokio::fs::create_dir_all(&cwd).await.unwrap();
    let dir = home
        .join(".mink/projects")
        .join(crate::session::paths::project_key(&cwd))
        .join("broken");
    tokio::fs::create_dir_all(&dir).await.unwrap();
    tokio::fs::write(dir.join("session.json"), "{not-json")
        .await
        .unwrap();

    let mut config = runtime_config_with_mock(&home, &cwd, mock_llm_hello());
    config.session = SessionPolicy::ContinueLatest;
    let error = build_runtime(config).await.err().unwrap().to_string();
    assert!(error.contains("corrupt session metadata"), "{error}");
    assert_eq!(
        tokio::fs::read_to_string(dir.join("session.json"))
            .await
            .unwrap(),
        "{not-json"
    );

    let _ = tokio::fs::remove_dir_all(home).await;
    let _ = tokio::fs::remove_dir_all(cwd).await;
}

#[tokio::test]
async fn continue_latest_fails_on_duplicate_alias_across_layouts() {
    let home = unique_temp_dir("continue-alias-home");
    let cwd = unique_temp_dir("continue-alias-cwd");
    tokio::fs::create_dir_all(&cwd).await.unwrap();
    for (key, id) in [
        (crate::session::paths::project_key(&cwd), "new-session"),
        (
            crate::session::paths::legacy_project_key(&cwd),
            "old-session",
        ),
    ] {
        write_session_metadata(&home.join(".mink/projects").join(key).join(id), id, &cwd).await;
    }

    let mut config = runtime_config_with_mock(&home, &cwd, mock_llm_hello());
    config.session = SessionPolicy::ContinueLatest;
    let error = build_runtime(config).await.err().unwrap().to_string();
    assert!(error.contains("alias:legacy-alias"), "{error}");
    assert!(error.contains("ambiguous across"), "{error}");

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
        serde_json::from_str(&std::fs::read_to_string(home.join("session.json")).unwrap()).unwrap();
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
                    "tools":{
                        "skills":["company-policy"],
                        "inline_skills":[{
                            "name":"company-policy",
                            "description":"Company policy",
                            "content":"private policy",
                            "exposure":"model_addressable"
                        }],
                        "skill_discovery_policy":"runtime_only"
                    }
                }
            }"#,
    )
    .unwrap();
    crate::sdk_protocol::validate_sdk_request(&req).unwrap();
    let mut runtime_config = AgentOptions::new(&home, &cwd).into_runtime_config();
    runtime_config.config.skills = req.options.tools.skills.clone().unwrap();
    runtime_config.runtime_skills = runtime_skills_from_sdk_request(&req);
    runtime_config.skill_discovery_policy = skill_discovery_policy_from_sdk_request(&req).unwrap();

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
    let selection =
        crate::resources::selector::split_read_path_selection("kb://tenant/doc").unwrap();
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

#[async_trait::async_trait]
impl EventSink for RecordingSink {
    async fn on_event(&self, event: AgentEvent) -> Result<(), String> {
        self.events.lock().unwrap().push(event);
        Ok(())
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
                AgentEvent { kind: crate::runtime::AgentEventKind::Info { message }, .. } if message == "Compressing..."
            )));
        assert!(
            events
                .iter()
                .any(|event| matches!(event.kind, crate::runtime::AgentEventKind::Stop { .. }))
        );
    }

    let _ = tokio::fs::remove_dir_all(home).await;
    let _ = tokio::fs::remove_dir_all(cwd).await;
}

fn tool_call_event(
    name: &str,
    id: &str,
    input: serde_json::Value,
) -> crate::protocol::ToolCallEvent {
    let fields: std::collections::BTreeMap<String, String> = input
        .as_object()
        .map(|obj| {
            obj.iter()
                .map(|(key, value)| (key.clone(), value.to_string()))
                .collect()
        })
        .unwrap_or_default();
    crate::protocol::ToolCallEvent {
        name: name.into(),
        id: id.into(),
        input_json: input,
        fields,
        parse_error: None,
    }
}

#[tokio::test]
async fn tool_result_events_carry_presentation_and_artifacts() {
    use crate::protocol::{Event, StopEvent, TextEvent};
    use serde_json::json;
    let home = unique_temp_dir("b3-home");
    let cwd = unique_temp_dir("b3-cwd");
    tokio::fs::create_dir_all(&cwd).await.unwrap();
    let sink = Arc::new(RecordingSink::default());

    let mock = crate::llm::mock::MockLlmBackend::new(
        "flash",
        vec![
            vec![
                Ok(Event::ToolCall(tool_call_event(
                    "TodoWrite",
                    "call_todo",
                    json!({"base_revision":0,"add":[{"content":"first task"}]}),
                ))),
                Ok(Event::Stop(StopEvent {
                    reason: "tool_use".into(),
                })),
            ],
            vec![
                Ok(Event::Text(TextEvent {
                    content: "todo done".into(),
                })),
                Ok(Event::Stop(StopEvent {
                    reason: "end_turn".into(),
                })),
            ],
            vec![
                Ok(Event::ToolCall(tool_call_event(
                    "Bash",
                    "call_bash",
                    json!({"command":"yes x | head -c 150000"}),
                ))),
                Ok(Event::Stop(StopEvent {
                    reason: "tool_use".into(),
                })),
            ],
            vec![
                Ok(Event::Text(TextEvent {
                    content: "bash done".into(),
                })),
                Ok(Event::Stop(StopEvent {
                    reason: "end_turn".into(),
                })),
            ],
        ],
    );
    let runtime_config = runtime_config_with_mock(&home, &cwd, mock).with_event_sink(sink.clone());
    let runtime = build_runtime(runtime_config).await.unwrap();

    runtime.run_turn("add a todo").await.unwrap();
    runtime.run_turn("produce a big output").await.unwrap();
    runtime.shutdown().await.unwrap();

    {
        let events = sink.events.lock().unwrap();
        let todo_presentations: Vec<_> = events
            .iter()
            .filter_map(|event| match &event.kind {
                crate::runtime::AgentEventKind::ToolResult {
                    tool_name,
                    presentation,
                    ..
                } if tool_name == "TodoWrite" => Some(presentation.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(todo_presentations.len(), 1, "exactly one TodoWrite result");
        assert!(
            matches!(
                todo_presentations[0],
                Some(crate::ui::ToolPresentation::Todo(_))
            ),
            "TodoWrite ToolResult event must carry its structured presentation"
        );

        let bash_artifacts: Vec<_> = events
            .iter()
            .filter_map(|event| match &event.kind {
                crate::runtime::AgentEventKind::ToolResult {
                    tool_name,
                    artifacts,
                    ..
                } if tool_name == "Bash" => Some(artifacts.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(bash_artifacts.len(), 1, "exactly one Bash result");
        assert!(
            !bash_artifacts[0].is_empty(),
            "oversized Bash result must carry artifact references in the event"
        );
    }

    let _ = tokio::fs::remove_dir_all(home).await;
    let _ = tokio::fs::remove_dir_all(cwd).await;
}

// ── Mock LLM runtime integration tests ──────────────────────────

/// Build a minimal mock LLM that returns Text + Stop for each call.
fn mock_llm_hello() -> crate::llm::mock::MockLlmBackend {
    use crate::protocol::{Event, StopEvent, TextEvent};
    crate::llm::mock::MockLlmBackend::new(
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

fn mock_llm_with_usage() -> crate::llm::mock::MockLlmBackend {
    use crate::protocol::{Event, StopEvent, TextEvent, UsageEvent};
    crate::llm::mock::MockLlmBackend::new(
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
    mock: crate::llm::mock::MockLlmBackend,
) -> AgentRuntimeConfig {
    let cfg = Config {
        model: "flash".into(),
        api_key: "test-key".into(),
        base_url: "https://example.invalid/v1".into(),
        max_context_tokens: 1_000_000,
        log_events: true,
        ..Config::default()
    };
    let mut rt_config = AgentRuntimeConfig::from_config(cfg, home.to_path_buf(), cwd.to_path_buf());
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

type SeenMessages = Arc<Mutex<Vec<Vec<serde_json::Value>>>>;

struct MessageCaptureBackend {
    seen: SeenMessages,
}

#[async_trait::async_trait]
impl crate::runtime::LlmBackend for MessageCaptureBackend {
    fn name(&self) -> &str {
        "message-capture"
    }

    async fn stream(
        &self,
        request: crate::runtime::LlmRequest,
    ) -> anyhow::Result<crate::runtime::LlmResponseStream> {
        self.seen.lock().unwrap().push(request.messages);
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
async fn uncompressed_confirmed_plan_is_not_dynamically_projected() {
    let home = unique_temp_dir("plan-tail-home");
    let cwd = unique_temp_dir("plan-tail-cwd");
    tokio::fs::create_dir_all(&cwd).await.unwrap();

    let seen = Arc::new(Mutex::new(Vec::new()));
    let cfg = Config {
        model: "flash".into(),
        api_key: "test-key".into(),
        base_url: "https://example.invalid/v1".into(),
        max_context_tokens: 1_000_000,
        ..Config::default()
    };
    let runtime_config = AgentRuntimeConfig::from_config(cfg, home.clone(), cwd.clone())
        .with_llm_backend(Arc::new(MessageCaptureBackend { seen: seen.clone() }));
    let runtime = build_runtime(runtime_config).await.unwrap();

    // Confirm a plan via the same file path the turn executor projects from.
    tokio::fs::write(
        runtime.session_info().plan_path.clone(),
        "# Verified plan\n1. implement\n2. verify\n",
    )
    .await
    .unwrap();

    let outcome = runtime.run_turn("execute").await.unwrap();
    assert_eq!(outcome.status, crate::agent::orchestrator::TurnStatus::Ok);

    {
        let captured = seen.lock().unwrap();
        assert_eq!(captured.len(), 1);
        let messages = &captured[0];
        assert!(messages.iter().all(|message| {
            !message["content"]
                .as_str()
                .is_some_and(|content| content.contains("<current-plan>"))
        }));
    }

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
    assert_eq!(outcome.usage.cost.known_nano_cny, 140_800);
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
    let events_path = runtime.session_info().events_path.clone();
    runtime.run_turn("say hello").await.unwrap();

    // run_turn returning means the turn-final event must already be durable:
    // callers (server, SDK, replay, tests) may read events.jsonl immediately.
    let event_log = std::fs::read_to_string(&events_path).unwrap();
    assert!(
        event_log.contains(r#""type":"turn_final""#),
        "turn_final must be flushed before run_turn returns: {event_log}"
    );

    runtime.shutdown().await.unwrap();

    {
        let events = sink.events.lock().unwrap();
        assert!(
                events.iter().any(|event| matches!(
                    event,
                    AgentEvent { kind: crate::runtime::AgentEventKind::Text { content }, .. } if content == "Hello, world!"
                )),
                "expected Text event with greeting"
            );
        assert!(
                events.iter().any(|event| matches!(
                    event,
                    AgentEvent { kind: crate::runtime::AgentEventKind::Final { outcome }, .. } if outcome.status == crate::agent::orchestrator::TurnStatus::Ok
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

    let mut stream = runtime.stream_turn("say hello").unwrap();
    let mut saw_text = false;
    let mut saw_final = false;
    while let Some(event) = tokio::time::timeout(tokio::time::Duration::from_secs(2), stream.recv())
        .await
        .expect("stream event timed out")
    {
        match event.kind {
            crate::runtime::AgentEventKind::Text { content } if content == "Hello, world!" => {
                saw_text = true;
            }
            crate::runtime::AgentEventKind::Final { outcome } => {
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

    let stream = runtime.stream_turn("say hello").unwrap();
    let outcome = stream.outcome().await.unwrap();
    assert_eq!(outcome.status, crate::agent::orchestrator::TurnStatus::Ok);
    assert!(outcome.text.contains("Hello, world!"));

    runtime.shutdown().await.unwrap();
    let _ = tokio::fs::remove_dir_all(home).await;
    let _ = tokio::fs::remove_dir_all(cwd).await;
}

#[tokio::test]
async fn stream_turn_reports_concurrent_turn_as_busy() {
    let home = unique_temp_dir("mock-stream-concurrent-home");
    let cwd = unique_temp_dir("mock-stream-concurrent-cwd");
    tokio::fs::create_dir_all(&cwd).await.unwrap();

    let runtime = build_runtime(runtime_config_with_mock(&home, &cwd, mock_llm_hello()))
        .await
        .unwrap();

    let stream = runtime.stream_turn("say hello").unwrap();
    let active_turn_id = stream.turn_id().clone();
    let err = match runtime.stream_turn("second stream") {
        Ok(_) => panic!("concurrent stream should fail"),
        Err(err) => err,
    };
    assert!(matches!(
        err,
        crate::runtime::RuntimeError::Busy { active_turn_id: ref id } if id == &active_turn_id
    ));

    let outcome = stream.outcome().await.unwrap();
    assert_eq!(outcome.status, crate::agent::orchestrator::TurnStatus::Ok);

    runtime.shutdown().await.unwrap();
    let _ = tokio::fs::remove_dir_all(home).await;
    let _ = tokio::fs::remove_dir_all(cwd).await;
}

#[tokio::test]
async fn cloned_handles_share_the_turn_gate_and_reject_empty_models() {
    let home = unique_temp_dir("mock-handle-gate-home");
    let cwd = unique_temp_dir("mock-handle-gate-cwd");
    tokio::fs::create_dir_all(&cwd).await.unwrap();
    let runtime = build_runtime(runtime_config_with_mock(&home, &cwd, mock_llm_hello()))
        .await
        .unwrap();
    let first = runtime.handle();
    let second = runtime.handle();

    let stream = first.stream_turn("first").unwrap();
    assert!(matches!(
        second.stream_turn("second"),
        Err(crate::runtime::RuntimeError::Busy { .. })
    ));
    stream.outcome().await.unwrap();
    assert!(matches!(
        second.set_model("   ").await,
        Err(crate::runtime::RuntimeError::Command(message)) if message.contains("must not be empty")
    ));

    runtime.shutdown().await.unwrap();
    let _ = tokio::fs::remove_dir_all(home).await;
    let _ = tokio::fs::remove_dir_all(cwd).await;
}

#[tokio::test]
async fn concurrent_run_turns_share_the_same_nonblocking_gate() {
    let home = unique_temp_dir("mock-run-concurrent-home");
    let cwd = unique_temp_dir("mock-run-concurrent-cwd");
    tokio::fs::create_dir_all(&cwd).await.unwrap();
    let runtime = build_runtime(runtime_config_with_mock(&home, &cwd, mock_llm_hello()))
        .await
        .unwrap();

    let (first, second) = tokio::join!(runtime.run_turn("first"), runtime.run_turn("second"));
    let results = [first, second];
    assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
    assert_eq!(
        results
            .iter()
            .filter(|result| matches!(result, Err(crate::runtime::RuntimeError::Busy { .. })))
            .count(),
        1
    );

    runtime.shutdown().await.unwrap();
    let _ = tokio::fs::remove_dir_all(home).await;
    let _ = tokio::fs::remove_dir_all(cwd).await;
}

#[tokio::test]
async fn run_turn_is_busy_while_a_stream_owns_the_turn() {
    let home = unique_temp_dir("mock-run-stream-busy-home");
    let cwd = unique_temp_dir("mock-run-stream-busy-cwd");
    tokio::fs::create_dir_all(&cwd).await.unwrap();
    let runtime = build_runtime(runtime_config_with_mock(&home, &cwd, mock_llm_hello()))
        .await
        .unwrap();

    let stream = runtime.stream_turn("stream").unwrap();
    let active_turn_id = stream.turn_id().clone();
    assert!(matches!(
        runtime.run_turn("run").await,
        Err(crate::runtime::RuntimeError::Busy { active_turn_id: ref id }) if id == &active_turn_id
    ));
    stream.outcome().await.unwrap();

    runtime.shutdown().await.unwrap();
    let _ = tokio::fs::remove_dir_all(home).await;
    let _ = tokio::fs::remove_dir_all(cwd).await;
}

#[tokio::test]
async fn dropping_a_stream_cancels_then_releases_the_turn_gate() {
    let home = unique_temp_dir("mock-drop-stream-home");
    let cwd = unique_temp_dir("mock-drop-stream-cwd");
    tokio::fs::create_dir_all(&cwd).await.unwrap();
    let runtime = build_runtime(runtime_config_with_blocking_mock(&home, &cwd))
        .await
        .unwrap();

    drop(runtime.stream_turn("blocking").unwrap());
    let replacement = tokio::time::timeout(tokio::time::Duration::from_secs(2), async {
        loop {
            match runtime.stream_turn("replacement") {
                Ok(stream) => break stream,
                Err(crate::runtime::RuntimeError::Busy { .. }) => {
                    tokio::task::yield_now().await;
                }
                Err(error) => panic!("unexpected runtime error: {error}"),
            }
        }
    })
    .await
    .expect("dropped stream did not release its permit");
    let outcome = replacement.outcome().await.unwrap();
    assert_eq!(outcome.status, crate::agent::orchestrator::TurnStatus::Ok);

    runtime.shutdown().await.unwrap();
    let _ = tokio::fs::remove_dir_all(home).await;
    let _ = tokio::fs::remove_dir_all(cwd).await;
}

#[tokio::test]
async fn dropping_completed_old_stream_does_not_interrupt_new_turn() {
    let home = unique_temp_dir("mock-drop-completed-home");
    let cwd = unique_temp_dir("mock-drop-completed-cwd");
    tokio::fs::create_dir_all(&cwd).await.unwrap();
    let runtime = build_runtime(runtime_config_with_mock(&home, &cwd, mock_llm_hello()))
        .await
        .unwrap();

    let mut old = runtime.stream_turn("first").unwrap();
    while old.recv().await.is_some() {}
    let second = runtime.stream_turn("second").unwrap();
    drop(old);
    let outcome = second.outcome().await.unwrap();
    assert_eq!(outcome.status, crate::agent::orchestrator::TurnStatus::Ok);
    assert_eq!(outcome.text, "second");

    runtime.shutdown().await.unwrap();
    let _ = tokio::fs::remove_dir_all(home).await;
    let _ = tokio::fs::remove_dir_all(cwd).await;
}

#[tokio::test]
async fn shutdown_closes_gate_before_cloned_handles_can_submit_commands() {
    let home = unique_temp_dir("mock-shutdown-gate-home");
    let cwd = unique_temp_dir("mock-shutdown-gate-cwd");
    tokio::fs::create_dir_all(&cwd).await.unwrap();
    let runtime = build_runtime(runtime_config_with_mock(&home, &cwd, mock_llm_hello()))
        .await
        .unwrap();
    let handle = runtime.handle();

    runtime.shutdown().await.unwrap();
    assert!(matches!(
        handle.stream_turn("late"),
        Err(crate::runtime::RuntimeError::Closed)
    ));
    assert!(matches!(
        handle.compact().await,
        Err(crate::runtime::RuntimeError::Closed)
    ));
    assert!(matches!(
        handle.set_model("pro").await,
        Err(crate::runtime::RuntimeError::Closed)
    ));

    let _ = tokio::fs::remove_dir_all(home).await;
    let _ = tokio::fs::remove_dir_all(cwd).await;
}

#[tokio::test]
async fn runtime_start_rejects_compaction_boundary_beyond_history() {
    let home = unique_temp_dir("invalid-compaction-boundary-home");
    let cwd = unique_temp_dir("invalid-compaction-boundary-cwd");
    tokio::fs::create_dir_all(&cwd).await.unwrap();
    let first = build_runtime(runtime_config_with_mock(&home, &cwd, mock_llm_hello()))
        .await
        .unwrap();
    let session = first.session_info().clone();
    first.shutdown().await.unwrap();
    tokio::fs::write(
        session.summary_path.with_file_name("context-state.json"),
        r#"{"active_start":1,"summary":"bad"}"#,
    )
    .await
    .unwrap();

    let mut config = runtime_config_with_mock(&home, &cwd, mock_llm_hello());
    config.session = SessionPolicy::Resume(session.session_id.clone());
    let error = build_runtime(config).await.err().unwrap().to_string();
    assert!(error.contains("exceeds history length"), "{error}");

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

#[tokio::test]
async fn consecutive_turn_outcomes_keep_their_own_text() {
    let home = unique_temp_dir("mock-outcome-ownership-home");
    let cwd = unique_temp_dir("mock-outcome-ownership-cwd");
    tokio::fs::create_dir_all(&cwd).await.unwrap();
    let runtime = build_runtime(runtime_config_with_mock(&home, &cwd, mock_llm_hello()))
        .await
        .unwrap();

    let first = runtime.run_turn("first").await.unwrap();
    let second = runtime.run_turn("second").await.unwrap();
    assert_eq!(first.text, "Hello, world!");
    assert_eq!(second.text, "second");
    assert_ne!(first.turn_id, second.turn_id);

    runtime.shutdown().await.unwrap();
    let _ = tokio::fs::remove_dir_all(home).await;
    let _ = tokio::fs::remove_dir_all(cwd).await;
}

// ── Interrupt test with blocking mock LLM ──────────────────────

/// A mock LLM whose stream never yields, used to test interrupt.
/// A mock LLM whose first `stream()` call returns a never-yielding
/// stream (for testing interrupt), and subsequent calls return a normal
/// Text+Stop (for testing recovery).
struct InterruptTestMockLlmBackend {
    calls: std::sync::Mutex<u32>,
}

struct InterruptibleCompactionBackend;

#[async_trait::async_trait]
impl crate::llm::client::LlmBackend for InterruptibleCompactionBackend {
    fn name(&self) -> &str {
        "interruptible-compaction"
    }

    async fn stream(
        &self,
        request: crate::runtime::LlmRequest,
    ) -> anyhow::Result<crate::runtime::LlmResponseStream> {
        if matches!(request.purpose, crate::runtime::LlmPurpose::Compaction) {
            return Ok(crate::runtime::LlmResponseStream {
                events: Box::pin(futures::stream::pending()),
                attempt_count: 1,
            });
        }
        use crate::protocol::{Event, StopEvent, TextEvent};
        Ok(crate::runtime::LlmResponseStream {
            events: Box::pin(futures::stream::iter(vec![
                Ok(Event::Text(TextEvent {
                    content: "after compact interrupt".into(),
                })),
                Ok(Event::Stop(StopEvent {
                    reason: "end_turn".into(),
                })),
            ])),
            attempt_count: 1,
        })
    }
}

#[async_trait::async_trait]
impl crate::llm::client::LlmBackend for InterruptTestMockLlmBackend {
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
    let mut rt_config = AgentRuntimeConfig::from_config(cfg, home.to_path_buf(), cwd.to_path_buf());
    rt_config.llm_backend = Some(Arc::new(InterruptTestMockLlmBackend {
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

    let runtime_handle = runtime.handle();
    let turn_handle = runtime_handle.clone();
    let task = tokio::spawn(async move { turn_handle.run_turn("blocking turn").await });

    // Let the orchestrator enter the LLM stream loop.
    tokio::time::sleep(tokio::time::Duration::from_millis(200)).await;

    // The orchestrator's turn executor polls this flag every 25 ms.
    runtime_handle.interrupt_current_turn();

    let outcome = task.await.unwrap().unwrap();
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

#[tokio::test]
async fn interrupt_manual_compaction_releases_gate_for_next_turn() {
    let home = unique_temp_dir("compact-interrupt-home");
    let cwd = unique_temp_dir("compact-interrupt-cwd");
    tokio::fs::create_dir_all(&cwd).await.unwrap();
    let cfg = Config {
        model: "flash".into(),
        api_key: "test-key".into(),
        max_context_tokens: 64_000,
        context_reserve_tokens: 8_000,
        context_compact_tail_tokens: 1,
        ..Config::default()
    };
    let runtime = build_runtime(
        AgentRuntimeConfig::from_config(cfg, home.clone(), cwd.clone())
            .with_llm_backend(Arc::new(InterruptibleCompactionBackend)),
    )
    .await
    .unwrap();
    for index in 0..3 {
        runtime
            .ctx
            .store
            .add_user(&format!("request {index}: {}", "x".repeat(2_000)))
            .await
            .unwrap();
        runtime
            .ctx
            .store
            .add_assistant(&format!("progress {index}: {}", "y".repeat(2_000)), "", &[])
            .await
            .unwrap();
    }
    let handle = runtime.handle();
    let compact_handle = handle.clone();
    let compact = tokio::spawn(async move { compact_handle.compact().await });
    tokio::time::sleep(tokio::time::Duration::from_millis(40)).await;
    handle.interrupt_current_turn();
    let error = tokio::time::timeout(tokio::time::Duration::from_secs(2), compact)
        .await
        .unwrap()
        .unwrap()
        .unwrap_err()
        .to_string();
    assert!(error.contains("compaction interrupted"), "{error}");

    let outcome = runtime.run_turn("still works").await.unwrap();
    assert_eq!(outcome.status, crate::agent::orchestrator::TurnStatus::Ok);
    runtime.shutdown().await.unwrap();
    let _ = tokio::fs::remove_dir_all(home).await;
    let _ = tokio::fs::remove_dir_all(cwd).await;
}

#[tokio::test]
async fn explicit_stream_cancel_releases_the_turn_gate_after_outcome() {
    let home = unique_temp_dir("mock-cancel-stream-home");
    let cwd = unique_temp_dir("mock-cancel-stream-cwd");
    tokio::fs::create_dir_all(&cwd).await.unwrap();
    let runtime = build_runtime(runtime_config_with_blocking_mock(&home, &cwd))
        .await
        .unwrap();

    let stream = runtime.stream_turn("blocking").unwrap();
    stream.cancel();
    let cancelled = stream.outcome().await.unwrap();
    assert_eq!(
        cancelled.status,
        crate::agent::orchestrator::TurnStatus::Interrupted
    );
    let recovered = runtime.run_turn("recovery").await.unwrap();
    assert_eq!(recovered.status, crate::agent::orchestrator::TurnStatus::Ok);

    runtime.shutdown().await.unwrap();
    let _ = tokio::fs::remove_dir_all(home).await;
    let _ = tokio::fs::remove_dir_all(cwd).await;
}

// ── Mock LLM with tool-use test ───────────────────────────────

/// Mock LLM that exercises the full tool-execution pipeline:
/// first turn returns a Bash tool call, second turn returns Text+Stop.
fn mock_llm_tool_use() -> crate::llm::mock::MockLlmBackend {
    use crate::protocol::{Event, StopEvent, TextEvent, ToolCallEvent};
    use serde_json::json;
    let mut fields = std::collections::BTreeMap::new();
    fields.insert("command".into(), "echo hello".into());
    crate::llm::mock::MockLlmBackend::new(
        "flash",
        vec![
            // First LLM call: request a Bash tool execution
            vec![
                Ok(Event::ToolCall(ToolCallEvent {
                    name: "Bash".into(),
                    id: "call_bash_1".into(),
                    input_json: json!({"command": "echo hello"}),
                    fields,
                    parse_error: None,
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

struct AsyncEchoTool;

#[async_trait::async_trait]
impl AgentTool for AsyncEchoTool {
    fn definition(&self) -> ToolDefinition {
        let mut definition = ToolDefinition::new(
            "AsyncEcho",
            "Echo text asynchronously",
            serde_json::json!({
                "type": "object",
                "properties": {"text": {"type": "string"}},
                "required": ["text"],
                "additionalProperties": false
            }),
        );
        definition.execution = ToolExecutionMode::ParallelReadOnly;
        definition
    }

    async fn execute(
        &self,
        input: serde_json::Value,
        ctx: ToolExecutionContext,
    ) -> Result<ToolOutput, ToolError> {
        if ctx.is_cancelled() {
            return Err(ToolError::new("cancelled"));
        }
        Ok(ToolOutput::text(format!(
            "echo:{}@{}",
            input["text"].as_str().unwrap_or_default(),
            ctx.cwd().display()
        )))
    }
}

struct MutatingEchoTool;

#[async_trait::async_trait]
impl AgentTool for MutatingEchoTool {
    fn definition(&self) -> ToolDefinition {
        let mut definition = ToolDefinition::new(
            "MutatingEcho",
            "A mutating test tool",
            serde_json::json!({
                "type": "object",
                "properties": {},
                "additionalProperties": false
            }),
        );
        definition.execution = ToolExecutionMode::Sequential;
        definition.mutating = true;
        definition
    }

    async fn execute(
        &self,
        _input: serde_json::Value,
        _ctx: ToolExecutionContext,
    ) -> Result<ToolOutput, ToolError> {
        Ok(ToolOutput::text("mutated"))
    }
}

struct CancellableTool;

struct NonCooperativeTool;

#[async_trait::async_trait]
impl AgentTool for NonCooperativeTool {
    fn definition(&self) -> ToolDefinition {
        let mut definition = ToolDefinition::new(
            "NonCooperative",
            "Never completes or checks cancellation",
            serde_json::json!({"type":"object","properties":{},"additionalProperties":false}),
        );
        definition.execution = ToolExecutionMode::Sequential;
        definition
    }

    async fn execute(
        &self,
        _input: serde_json::Value,
        _ctx: ToolExecutionContext,
    ) -> Result<ToolOutput, ToolError> {
        futures::future::pending().await
    }
}

#[async_trait::async_trait]
impl AgentTool for CancellableTool {
    fn definition(&self) -> ToolDefinition {
        let mut definition = ToolDefinition::new(
            "Cancellable",
            "Wait until cancelled",
            serde_json::json!({
                "type": "object",
                "properties": {},
                "additionalProperties": false
            }),
        );
        definition.execution = ToolExecutionMode::Sequential;
        definition
    }

    async fn execute(
        &self,
        _input: serde_json::Value,
        ctx: ToolExecutionContext,
    ) -> Result<ToolOutput, ToolError> {
        while !ctx.is_cancelled() {
            tokio::time::sleep(tokio::time::Duration::from_millis(5)).await;
        }
        Err(ToolError::new("cancelled"))
    }
}

struct DriftingDefinitionTool {
    calls: Arc<std::sync::atomic::AtomicUsize>,
}

#[async_trait::async_trait]
impl AgentTool for DriftingDefinitionTool {
    fn definition(&self) -> ToolDefinition {
        let call = self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        ToolDefinition::new(
            if call == 0 {
                "FrozenTool"
            } else {
                "DriftedTool"
            },
            "Definition must be frozen at startup",
            serde_json::json!({
                "type": "object",
                "properties": {},
                "additionalProperties": false
            }),
        )
    }

    async fn execute(
        &self,
        _input: serde_json::Value,
        _ctx: ToolExecutionContext,
    ) -> Result<ToolOutput, ToolError> {
        Ok(ToolOutput::text("frozen"))
    }
}

fn mock_llm_single_tool(name: &str) -> crate::llm::mock::MockLlmBackend {
    use crate::protocol::{Event, StopEvent, TextEvent, ToolCallEvent};
    crate::llm::mock::MockLlmBackend::new(
        "flash",
        vec![
            vec![
                Ok(Event::ToolCall(ToolCallEvent {
                    name: name.into(),
                    id: format!("call_{}", name.to_ascii_lowercase()),
                    input_json: serde_json::json!({}),
                    fields: Default::default(),
                    parse_error: None,
                })),
                Ok(Event::Stop(StopEvent {
                    reason: "tool_use".into(),
                })),
            ],
            vec![
                Ok(Event::Text(TextEvent {
                    content: "done".into(),
                })),
                Ok(Event::Stop(StopEvent {
                    reason: "end_turn".into(),
                })),
            ],
        ],
    )
}

fn mock_llm_custom_tool_use() -> crate::llm::mock::MockLlmBackend {
    use crate::protocol::{Event, StopEvent, TextEvent, ToolCallEvent};
    let mut fields = std::collections::BTreeMap::new();
    fields.insert("text".into(), "hello".into());
    crate::llm::mock::MockLlmBackend::new(
        "flash",
        vec![
            vec![
                Ok(Event::ToolCall(ToolCallEvent {
                    name: "AsyncEcho".into(),
                    id: "call_echo_1".into(),
                    input_json: serde_json::json!({"text": "hello"}),
                    fields,
                    parse_error: None,
                })),
                Ok(Event::Stop(StopEvent {
                    reason: "tool_use".into(),
                })),
            ],
            vec![
                Ok(Event::Text(TextEvent {
                    content: "custom done".into(),
                })),
                Ok(Event::Stop(StopEvent {
                    reason: "end_turn".into(),
                })),
            ],
        ],
    )
}

#[tokio::test]
async fn registered_async_custom_tool_executes_through_the_core_pipeline() {
    let home = unique_temp_dir("custom-tool-home");
    let cwd = unique_temp_dir("custom-tool-cwd");
    tokio::fs::create_dir_all(&cwd).await.unwrap();
    let mut config = runtime_config_with_mock(&home, &cwd, mock_llm_custom_tool_use());
    config.custom_tools.push(Arc::new(AsyncEchoTool));
    let runtime = build_runtime(config).await.unwrap();

    let mut stream = runtime.stream_turn("echo").unwrap();
    let turn_id = stream.turn_id().clone();
    let mut sequences = Vec::new();
    let mut result = None;
    while let Some(event) = stream.recv().await {
        assert_eq!(event.turn_id.as_ref(), Some(&turn_id));
        sequences.push(event.sequence);
        if let crate::runtime::AgentEventKind::ToolResult {
            tool_use_id,
            tool_name,
            content,
            status,
            result_kind,
            ..
        } = event.kind
        {
            result = Some((tool_use_id, tool_name, content, status, result_kind));
        }
    }
    assert!(sequences.windows(2).all(|pair| pair[0] < pair[1]));
    let outcome = stream.outcome().await.unwrap();
    assert!(outcome.text.contains("custom done"));
    let (tool_use_id, tool_name, content, status, result_kind) = result.unwrap();
    assert_eq!(tool_use_id.as_deref(), Some("call_echo_1"));
    assert_eq!(tool_name, "AsyncEcho");
    assert!(content.contains("echo:hello@"));
    assert!(status.is_success());
    assert_eq!(result_kind, crate::tools::metadata::ToolResultKind::Text);

    runtime.shutdown().await.unwrap();
    let _ = tokio::fs::remove_dir_all(home).await;
    let _ = tokio::fs::remove_dir_all(cwd).await;
}

#[tokio::test]
async fn custom_tool_definition_is_evaluated_once_and_frozen() {
    let home = unique_temp_dir("custom-frozen-home");
    let cwd = unique_temp_dir("custom-frozen-cwd");
    tokio::fs::create_dir_all(&cwd).await.unwrap();
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let mut config = runtime_config_with_mock(&home, &cwd, mock_llm_single_tool("FrozenTool"));
    config.custom_tools.push(Arc::new(DriftingDefinitionTool {
        calls: calls.clone(),
    }));

    let runtime = build_runtime(config).await.unwrap();
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    let outcome = runtime.run_turn("use frozen definition").await.unwrap();
    assert_eq!(outcome.tool_call_count, 1);
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);

    runtime.shutdown().await.unwrap();
    let _ = tokio::fs::remove_dir_all(home).await;
    let _ = tokio::fs::remove_dir_all(cwd).await;
}

#[tokio::test]
async fn successful_mutating_custom_tool_invalidates_read_memos() {
    let home = unique_temp_dir("custom-mutating-home");
    let cwd = unique_temp_dir("custom-mutating-cwd");
    tokio::fs::create_dir_all(&cwd).await.unwrap();
    let mut config = runtime_config_with_mock(&home, &cwd, mock_llm_single_tool("MutatingEcho"));
    config.custom_tools.push(Arc::new(MutatingEchoTool));
    let runtime = build_runtime(config).await.unwrap();

    assert_eq!(
        runtime
            .ctx
            .memo_mutation
            .load(std::sync::atomic::Ordering::SeqCst),
        0
    );
    runtime.run_turn("mutate").await.unwrap();
    assert_eq!(
        runtime
            .ctx
            .memo_mutation
            .load(std::sync::atomic::Ordering::SeqCst),
        1
    );

    runtime.shutdown().await.unwrap();
    let _ = tokio::fs::remove_dir_all(home).await;
    let _ = tokio::fs::remove_dir_all(cwd).await;
}

#[tokio::test]
async fn async_custom_tool_observes_stream_cancellation() {
    let home = unique_temp_dir("custom-cancel-home");
    let cwd = unique_temp_dir("custom-cancel-cwd");
    tokio::fs::create_dir_all(&cwd).await.unwrap();
    let mut config = runtime_config_with_mock(&home, &cwd, mock_llm_single_tool("Cancellable"));
    config.custom_tools.push(Arc::new(CancellableTool));
    let runtime = build_runtime(config).await.unwrap();

    let stream = runtime.stream_turn("wait").unwrap();
    tokio::time::sleep(tokio::time::Duration::from_millis(20)).await;
    stream.cancel();
    let outcome = tokio::time::timeout(tokio::time::Duration::from_secs(2), stream.outcome())
        .await
        .expect("custom tool did not stop after cancellation")
        .unwrap();
    assert_eq!(
        outcome.status,
        crate::agent::orchestrator::TurnStatus::Interrupted
    );

    runtime.shutdown().await.unwrap();
    let _ = tokio::fs::remove_dir_all(home).await;
    let _ = tokio::fs::remove_dir_all(cwd).await;
}

#[tokio::test]
async fn non_cooperative_custom_tool_is_dropped_on_interrupt() {
    let home = unique_temp_dir("custom-non-cooperative-home");
    let cwd = unique_temp_dir("custom-non-cooperative-cwd");
    tokio::fs::create_dir_all(&cwd).await.unwrap();
    let mut config = runtime_config_with_mock(&home, &cwd, mock_llm_single_tool("NonCooperative"));
    config.custom_tools.push(Arc::new(NonCooperativeTool));
    let runtime = build_runtime(config).await.unwrap();

    let stream = runtime.stream_turn("wait").unwrap();
    tokio::time::sleep(tokio::time::Duration::from_millis(40)).await;
    stream.cancel();
    let outcome = tokio::time::timeout(tokio::time::Duration::from_secs(2), stream.outcome())
        .await
        .expect("non-cooperative custom tool was not dropped")
        .unwrap();
    assert_eq!(
        outcome.status,
        crate::agent::orchestrator::TurnStatus::Interrupted
    );

    runtime.shutdown().await.unwrap();
    let _ = tokio::fs::remove_dir_all(home).await;
    let _ = tokio::fs::remove_dir_all(cwd).await;
}

#[tokio::test]
async fn custom_tool_timeout_is_local_and_next_turn_still_runs() {
    let home = unique_temp_dir("custom-timeout-home");
    let cwd = unique_temp_dir("custom-timeout-cwd");
    tokio::fs::create_dir_all(&cwd).await.unwrap();
    let mut config = runtime_config_with_mock(
        &home,
        &cwd,
        crate::llm::mock::MockLlmBackend::new(
            "flash",
            vec![
                vec![
                    Ok(crate::protocol::Event::ToolCall(
                        crate::protocol::ToolCallEvent {
                            name: "NonCooperative".into(),
                            id: "call-timeout".into(),
                            input_json: serde_json::json!({}),
                            fields: Default::default(),
                            parse_error: None,
                        },
                    )),
                    Ok(crate::protocol::Event::Stop(crate::protocol::StopEvent {
                        reason: "tool_use".into(),
                    })),
                ],
                vec![
                    Ok(crate::protocol::Event::Text(crate::protocol::TextEvent {
                        content: "recovered from tool timeout".into(),
                    })),
                    Ok(crate::protocol::Event::Stop(crate::protocol::StopEvent {
                        reason: "end_turn".into(),
                    })),
                ],
                vec![
                    Ok(crate::protocol::Event::Text(crate::protocol::TextEvent {
                        content: "next turn".into(),
                    })),
                    Ok(crate::protocol::Event::Stop(crate::protocol::StopEvent {
                        reason: "end_turn".into(),
                    })),
                ],
            ],
        ),
    );
    config.config.tool_timeout_secs = 5;
    config.custom_tools.push(Arc::new(NonCooperativeTool));
    let runtime = build_runtime(config).await.unwrap();

    let mut stream = runtime.stream_turn("timeout tool").unwrap();
    let mut timeout_result = None;
    while let Some(event) = stream.recv().await {
        if let crate::runtime::AgentEventKind::ToolResult {
            content, status, ..
        } = event.kind
        {
            timeout_result = Some((content, status));
        }
    }
    let timed_out = stream.outcome().await.unwrap();
    assert_eq!(timed_out.status, crate::agent::orchestrator::TurnStatus::Ok);
    let (content, status) = timeout_result.expect("timeout tool result");
    assert!(!status.is_success());
    assert!(content.contains("timed out after 5s"), "{content}");
    let next = runtime.run_turn("next").await.unwrap();
    assert_eq!(next.status, crate::agent::orchestrator::TurnStatus::Ok);
    assert_eq!(next.text, "next turn");

    runtime.shutdown().await.unwrap();
    let _ = tokio::fs::remove_dir_all(home).await;
    let _ = tokio::fs::remove_dir_all(cwd).await;
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
                .any(|e| matches!(e.kind, crate::runtime::AgentEventKind::ToolCall { .. })),
            events
                .iter()
                .any(|e| matches!(e.kind, crate::runtime::AgentEventKind::ToolResult { .. })),
            events
                .iter()
                .any(|e| matches!(e.kind, crate::runtime::AgentEventKind::Final { .. })),
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

    let mut stream = runtime.stream_turn("run echo hello").unwrap();
    let mut saw_tool_call = false;
    let mut saw_tool_result = false;
    while let Some(event) =
        tokio::time::timeout(tokio::time::Duration::from_secs(10), stream.recv())
            .await
            .expect("stream event timed out")
    {
        match event.kind {
            crate::runtime::AgentEventKind::ToolCall { .. } => saw_tool_call = true,
            crate::runtime::AgentEventKind::ToolResult { .. } => saw_tool_result = true,
            crate::runtime::AgentEventKind::Final { .. } => break,
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

type SeenSystemPrompts = Arc<Mutex<Vec<String>>>;

struct SystemPromptCaptureBackend {
    seen: SeenSystemPrompts,
}

#[async_trait::async_trait]
impl crate::runtime::LlmBackend for SystemPromptCaptureBackend {
    fn name(&self) -> &str {
        "system-prompt-capture"
    }

    async fn stream(
        &self,
        request: crate::runtime::LlmRequest,
    ) -> anyhow::Result<crate::runtime::LlmResponseStream> {
        self.seen.lock().unwrap().push(request.system_prompt);
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

struct StaticPrefixSource {
    system_prompt: String,
}

impl crate::runtime::PrefixSource for StaticPrefixSource {
    fn prefix(&self, _events_path: &std::path::Path) -> Option<(String, Vec<serde_json::Value>)> {
        Some((self.system_prompt.clone(), Vec::new()))
    }
}

#[tokio::test]
async fn prefix_source_overrides_compiled_prefix() {
    let home = unique_temp_dir("prefix-source-home");
    let cwd = unique_temp_dir("prefix-source-cwd");
    tokio::fs::create_dir_all(&cwd).await.unwrap();

    let seen = Arc::new(Mutex::new(Vec::new()));
    let runtime = crate::runtime::AgentRuntime::start(
        crate::runtime::AgentOptions::new(home.clone(), cwd.clone())
            .with_llm_backend(Arc::new(SystemPromptCaptureBackend { seen: seen.clone() }))
            .with_api_key("test-key")
            .with_base_url("https://example.invalid/v1")
            .with_prefix_source(Arc::new(StaticPrefixSource {
                system_prompt: "CUSTOM-PREFIX-SYSTEM-PROMPT".into(),
            })),
    )
    .await
    .unwrap();

    let outcome = runtime.run_turn("hi").await.unwrap();
    assert_eq!(outcome.status, crate::agent::orchestrator::TurnStatus::Ok);
    // Second turn in the same process must still consult the source (the
    // compiled prefix cache must never shadow a configured source).
    let outcome = runtime.run_turn("hi again").await.unwrap();
    assert_eq!(outcome.status, crate::agent::orchestrator::TurnStatus::Ok);
    let captured = seen.lock().unwrap().clone();
    assert_eq!(
        captured.len(),
        2,
        "both requests must consult the prefix source"
    );
    assert_eq!(captured[0], "CUSTOM-PREFIX-SYSTEM-PROMPT");
    assert_eq!(captured[1], "CUSTOM-PREFIX-SYSTEM-PROMPT");

    runtime.shutdown().await.unwrap();
    let _ = tokio::fs::remove_dir_all(home).await;
    let _ = tokio::fs::remove_dir_all(cwd).await;
}

type SeenHookViews = Arc<Mutex<Option<(String, usize, usize, String, String, String, String)>>>;

struct RecordingPostInitHook {
    seen: SeenHookViews,
}

impl crate::runtime::PostInitHook for RecordingPostInitHook {
    fn run(&self, ctx: &crate::runtime::PostInitContext<'_>) -> anyhow::Result<()> {
        *self.seen.lock().unwrap() = Some((
            ctx.system_prompt().to_string(),
            ctx.tools().len(),
            ctx.workflow_ids().len(),
            ctx.workflow_fingerprint().to_string(),
            ctx.tool_surface_fingerprint().to_string(),
            ctx.tool_capabilities_fingerprint().to_string(),
            ctx.dependency_fingerprint().to_string(),
        ));
        assert!(!ctx.cwd().as_os_str().is_empty());
        assert!(!ctx.capabilities().skills.discoverable.is_empty());
        ctx.log_event(serde_json::json!({ "type": "hook_test_event" }))?;
        Ok(())
    }
}

#[tokio::test]
async fn post_init_hook_receives_resolved_view_and_logs_events() {
    let home = unique_temp_dir("post-init-hook-home");
    let cwd = unique_temp_dir("post-init-hook-cwd");
    tokio::fs::create_dir_all(&cwd).await.unwrap();

    let seen_hook = Arc::new(Mutex::new(None));
    let runtime = crate::runtime::AgentRuntime::start(
        crate::runtime::AgentOptions::new(home.clone(), cwd.clone())
            .with_llm_backend(Arc::new(MessageCaptureBackend {
                seen: Arc::new(Mutex::new(Vec::new())),
            }))
            .with_api_key("test-key")
            .with_base_url("https://example.invalid/v1")
            .with_post_init_hook(Arc::new(RecordingPostInitHook {
                seen: seen_hook.clone(),
            })),
    )
    .await
    .unwrap();

    let info = runtime.session_info().clone();
    let view = seen_hook.lock().unwrap().clone().expect("hook ran");
    assert!(
        view.0.contains("system-conventions"),
        "resolved system prompt must carry the core section"
    );
    assert!(view.1 > 0, "tool schemas must be visible to the hook");
    assert!(view.2 > 0, "workflow ids must be visible to the hook");
    assert!(!view.3.is_empty());
    assert!(!view.4.is_empty());
    assert!(!view.5.is_empty());
    assert!(!view.6.is_empty());

    let events_text = tokio::fs::read_to_string(&info.events_path).await.unwrap();
    assert!(
        events_text.contains("\"type\":\"hook_test_event\""),
        "hook event must be persisted"
    );

    let outcome = runtime.run_turn("hi").await.unwrap();
    assert_eq!(outcome.status, crate::agent::orchestrator::TurnStatus::Ok);

    runtime.shutdown().await.unwrap();
    let _ = tokio::fs::remove_dir_all(home).await;
    let _ = tokio::fs::remove_dir_all(cwd).await;
}

struct RewritingHook;

impl crate::runtime::PostInitHook for RewritingHook {
    fn run(&self, ctx: &crate::runtime::PostInitContext<'_>) -> anyhow::Result<()> {
        // Simulate a host that rewrites session files before the first turn
        // (prefab-style restructuring): append a user line to the
        // conversation and record a prefix_snapshot event.
        let mut conversation =
            std::fs::read_to_string(&ctx.session_paths().conversation).unwrap_or_default();
        conversation.push_str("{\"role\":\"user\",\"content\":\"HOOK-INJECTED-LINE\"}\n");
        std::fs::write(&ctx.session_paths().conversation, conversation)?;
        ctx.log_event(serde_json::json!({
            "type": "prefix_snapshot",
            "version": 1,
            "fingerprint": "hook-test",
            "dependency_fingerprint": "hook-test",
            "system_prompt": "HOOK-PROMPT",
            "tools_json": [],
        }))?;
        Ok(())
    }
}

#[tokio::test]
async fn post_init_hook_rewrites_are_visible_to_first_turn() {
    let home = unique_temp_dir("hook-rewrite-home");
    let cwd = unique_temp_dir("hook-rewrite-cwd");
    tokio::fs::create_dir_all(&cwd).await.unwrap();

    let seen = Arc::new(Mutex::new(Vec::new()));
    let runtime = crate::runtime::AgentRuntime::start(
        crate::runtime::AgentOptions::new(home.clone(), cwd.clone())
            .with_llm_backend(Arc::new(MessageCaptureBackend { seen: seen.clone() }))
            .with_api_key("test-key")
            .with_base_url("https://example.invalid/v1")
            .with_post_init_hook(Arc::new(RewritingHook)),
    )
    .await
    .unwrap();

    let outcome = runtime.run_turn("hi").await.unwrap();
    assert_eq!(outcome.status, crate::agent::orchestrator::TurnStatus::Ok);
    let captured = seen.lock().unwrap().clone();
    assert_eq!(captured.len(), 1);
    let messages = &captured[0];
    assert!(
        messages.iter().any(|m| m
            .get("content")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|c| c.contains("HOOK-INJECTED-LINE"))),
        "the hook's conversation rewrite must be visible to the first turn \
         (conversation store cache must not shadow disk rewrites)"
    );

    runtime.shutdown().await.unwrap();
    let _ = tokio::fs::remove_dir_all(home).await;
    let _ = tokio::fs::remove_dir_all(cwd).await;
}
