//! 会话初始化辅助函数。
//! 被 main.rs 和 sub_executor.rs 共用，消除重复代码。

use crate::session::artifacts::ArtifactManager;
use crate::session::paths::{SessionLayout, ensure_dir, paths_for_layout};
use crate::session::stats::StatsTracker;
use crate::session::store::ConversationStore;
use std::sync::Arc;

/// 初始化会话的基础设施：目录、文件、store、stats。
/// 返回创建好的 store 和 stats。
pub async fn init_session_base(
    home: &std::path::Path,
    cwd: &std::path::Path,
    session_id: &str,
) -> anyhow::Result<(
    Arc<ConversationStore>,
    Arc<StatsTracker>,
    Arc<ArtifactManager>,
)> {
    init_session_base_with_layout(home, cwd, session_id, SessionLayout::ProjectScoped).await
}

/// Initialize session files with an explicit filesystem layout.
///
/// This is the layout-aware variant used by embedded runtimes. Keeping the
/// setup here prevents the store, event log, stats, and artifacts from
/// accidentally being initialized under different session roots.
pub async fn init_session_base_with_layout(
    home: &std::path::Path,
    cwd: &std::path::Path,
    session_id: &str,
    layout: SessionLayout,
) -> anyhow::Result<(
    Arc<ConversationStore>,
    Arc<StatsTracker>,
    Arc<ArtifactManager>,
)> {
    let paths = paths_for_layout(home, cwd, session_id, layout);
    ensure_dir(&paths.session_dir).await?;
    ensure_dir(&paths.artifacts).await?;

    for f in [&paths.conversation, &paths.events, &paths.summary] {
        if !f.exists() {
            let _ = tokio::fs::File::create(f).await;
        }
    }

    let store = Arc::new(ConversationStore::new(paths.conversation.clone()));
    store.ensure().await?;

    let stats = StatsTracker::load(&paths.stats).await?;
    let artifacts = Arc::new(ArtifactManager::new(paths.artifacts.clone()));
    artifacts.ensure()?;
    if !paths.stats.exists()
        || tokio::fs::metadata(&paths.stats)
            .await
            .map_or(0, |m| m.len())
            == 0
    {
        let initial = r#"{"current_turn_count":0,"agent_request_count":0,"compact_request_count":0,"sub_agent_request_count":0,"total_input_tokens":0,"total_output_tokens":0,"total_cache_read_tokens":0,"total_cache_creation_tokens":0,"current_context_tokens":0,"last_updated":""}"#;
        let _ = tokio::fs::write(&paths.stats, format!("{initial}\n")).await;
    }

    Ok((store, stats, artifacts))
}
