use crate::cancel::CancellationToken;
use crate::config::Config;
use crate::context::{AgentSharedContext, ToolConfig};
use crate::session::compaction::CompactionEngine;
use crate::session::paths::{self, SessionLayout};
use crate::session::usage::UsageJournal;
use crate::tools::snapshot::FileSnapshotStore;
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

    let compaction = Arc::new(CompactionEngine::new_with_usage(
        store.clone(),
        paths.summary.clone(),
        paths.plan.clone(),
        paths.plan_draft.clone(),
        params.cwd.clone(),
        params.home.clone(),
        config.skills.clone(),
        params.api_url.clone(),
        &config,
        stats.clone(),
        usage.clone(),
        params.session_id.clone(),
        params.display.clone(),
        params.cancel.clone(),
    ));

    let ctx = Arc::new(AgentSharedContext {
        config: config.clone(),
        cwd: params.cwd,
        home: params.home,
        session_layout: params.session_layout,
        api_url: params.api_url,
        store,
        artifacts,
        snapshots: Arc::new(Mutex::new(FileSnapshotStore::default())),
        stats,
        usage,
        compaction,
        cancel: params.cancel,
        display: params.display,
        sub_stream_tx: params.sub_stream_tx,
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
