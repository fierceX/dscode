//! 会话初始化辅助函数。
//! 被 main.rs 和 sub_executor.rs 共用，消除重复代码。

use crate::session::artifacts::ArtifactManager;
use crate::session::paths::ensure_dir;
use crate::session::stats::StatsTracker;
use crate::session::store::ConversationStore;
use std::sync::Arc;

pub async fn init_session_base_at(
    paths: &crate::session::paths::Paths,
) -> anyhow::Result<(
    Arc<ConversationStore>,
    Arc<StatsTracker>,
    Arc<ArtifactManager>,
)> {
    ensure_dir(&paths.session_dir).await?;
    ensure_dir(&paths.artifacts).await?;

    for f in [&paths.conversation, &paths.events, &paths.summary] {
        if !f.exists() {
            tokio::fs::File::create(f).await?;
        }
    }

    let store = Arc::new(ConversationStore::new(paths.conversation.clone()));
    store.ensure().await?;
    store.repair_dangling_tool_uses().await?;

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
        let stats_path = paths.stats.clone();
        tokio::task::spawn_blocking(move || {
            crate::session::atomic_file::atomic_replace(
                &stats_path,
                format!("{initial}\n").as_bytes(),
            )
        })
        .await??;
    }

    Ok((store, stats, artifacts))
}
