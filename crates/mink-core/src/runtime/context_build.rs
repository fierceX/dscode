use crate::cancel::CancellationToken;
use crate::capabilities::{
    CapabilitySnapshot, RuntimeSkill, SkillDiscoveryPolicy, SkillProvider,
    skill_providers_for_policy,
};
use crate::config::Config;
use crate::context::{AgentSharedContext, ToolConfig};
use crate::llm::client::LlmBackend;
use crate::resources::{ResourceHandler, ResourceRouter};
use crate::session::compaction::CompactionEngine;
use crate::session::paths::{self, SessionLayout};
use crate::session::usage::UsageJournal;
use crate::tools::snapshot::FileSnapshotStore;
use crate::tools::vfs::{ReadOnlyFileSystem, VfsScope};
use crate::ui::{Display, SubAgentStreamSink};
use anyhow::Result;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};

pub(crate) struct AgentContextBuild {
    pub config: Config,
    pub home: PathBuf,
    pub cwd: PathBuf,
    pub session_id: String,
    pub session_layout: SessionLayout,
    pub api_url: String,
    pub display: Arc<dyn Display>,
    pub sub_stream_tx: Option<Arc<dyn SubAgentStreamSink>>,
    pub cancel: CancellationToken,
    pub interrupt: Arc<AtomicBool>,
    pub is_sub_agent: bool,
    pub usage_journal: Option<Arc<UsageJournal>>,
    pub read_only_fs: Option<Arc<dyn ReadOnlyFileSystem>>,
    pub resource_session_id: String,
    pub resource_handlers: Vec<Arc<dyn ResourceHandler>>,
    pub skill_providers: Vec<Arc<dyn SkillProvider>>,
    pub runtime_skills: Vec<RuntimeSkill>,
    pub skill_discovery_policy: SkillDiscoveryPolicy,
    pub llm_backend: Arc<dyn LlmBackend>,
    pub resource_router: Option<Arc<ResourceRouter>>,
    pub capability_snapshot: Option<Arc<CapabilitySnapshot>>,
}

pub(crate) struct BuiltAgentContext {
    pub ctx: Arc<AgentSharedContext>,
    pub paths: paths::Paths,
    pub is_new: bool,
}

pub(crate) async fn build_agent_context(params: AgentContextBuild) -> Result<BuiltAgentContext> {
    let mut config = params.config.clone();
    config.session_id = params.session_id.clone();
    let paths = paths::paths_for_layout(
        &params.home,
        &params.cwd,
        &params.session_id,
        params.session_layout,
    );
    let is_new = !paths.events.exists();

    let (store, stats, artifacts) = crate::session::init::init_session_base_with_layout(
        &params.home,
        &params.cwd,
        &params.session_id,
        params.session_layout,
    )
    .await?;
    let usage = params
        .usage_journal
        .unwrap_or_else(|| UsageJournal::new(paths.usage.clone()));

    let vfs_scope = VfsScope {
        resource_session_id: params.resource_session_id,
        agent_session_id: params.session_id.clone(),
    };
    let capability_snapshot = if let Some(snapshot) = params.capability_snapshot {
        snapshot
    } else {
        let providers = skill_providers_for_policy(
            params.skill_discovery_policy,
            &params.runtime_skills,
            &params.skill_providers,
        );
        Arc::new(CapabilitySnapshot::load_from_skill_providers(
            &providers,
            &params.cwd,
            &params.home,
            &params.session_id,
            &vfs_scope.resource_session_id,
            &config.skills,
        )?)
    };
    let resource_router = if let Some(router) = params.resource_router {
        router
    } else {
        let mut router = ResourceRouter::with_builtin_handlers();
        for handler in params.resource_handlers {
            router.register(handler, false)?;
        }
        Arc::new(router)
    };
    let compaction = Arc::new(CompactionEngine::new_with_usage(
        store.clone(),
        paths.summary.clone(),
        paths.plan.clone(),
        paths.plan_draft.clone(),
        params.cwd.clone(),
        params.home.clone(),
        capability_snapshot.clone(),
        params.api_url.clone(),
        &config,
        stats.clone(),
        usage.clone(),
        params.session_id.clone(),
        params.display.clone(),
        params.cancel.clone(),
        params.llm_backend.clone(),
    ));

    let ctx = Arc::new(AgentSharedContext {
        config: config.clone(),
        cwd: params.cwd,
        home: params.home,
        session_layout: params.session_layout,
        api_url: params.api_url,
        llm_backend: params.llm_backend,
        store,
        artifacts,
        snapshots: Arc::new(Mutex::new(FileSnapshotStore::default())),
        stats,
        usage,
        compaction,
        cancel: params.cancel,
        display: params.display,
        sub_stream_tx: params.sub_stream_tx,
        read_only_fs: params.read_only_fs,
        vfs_scope,
        resource_router,
        capability_snapshot,
        tool_config: ToolConfig::from_config(&config),
        events_path: paths.events.clone(),
        summary_path: paths.summary.clone(),
        plan_path: paths.plan.clone(),
        plan_draft_path: paths.plan_draft.clone(),
        immutable_prefix: Mutex::new(None),
        is_sub_agent: params.is_sub_agent,
        interrupt: params.interrupt,
        event_log_warned: AtomicBool::new(false),
    });

    Ok(BuiltAgentContext { ctx, paths, is_new })
}
