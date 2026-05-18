//! 会话初始化辅助函数。
//! 被 main.rs 和 sub_executor.rs 共用，消除重复代码。

use crate::session::store::ConversationStore;
use crate::session::stats::StatsTracker;
use crate::session::paths::{paths_for, ensure_dir};
use std::sync::Arc;

/// 初始化会话的基础设施：目录、文件、store、stats。
/// 返回创建好的 store 和 stats。
pub async fn init_session_base(home: &std::path::Path, cwd: &std::path::Path, session_id: &str) -> anyhow::Result<(Arc<ConversationStore>, Arc<StatsTracker>)> {
    let paths = paths_for(home, cwd, session_id);
    ensure_dir(&paths.session_dir).await?;

    for f in [&paths.conversation, &paths.events, &paths.summary, &paths.plan, &paths.plan_draft] {
        if !f.exists() {
            let _ = tokio::fs::File::create(f).await;
        }
    }

    let store = Arc::new(ConversationStore::new(paths.conversation.clone()));
    store.ensure().await?;

    let stats = StatsTracker::load(&paths.stats).await?;
    if !paths.stats.exists() || tokio::fs::metadata(&paths.stats).await.map_or(0, |m| m.len()) == 0 {
        let initial = r#"{"current_turn_count":0,"agent_request_count":0,"compact_request_count":0,"sub_agent_request_count":0,"total_input_tokens":0,"total_output_tokens":0,"total_cache_read_tokens":0,"total_cache_creation_tokens":0,"current_context_tokens":0,"last_updated":""}"#;
        let _ = tokio::fs::write(&paths.stats, format!("{initial}\n")).await;
    }

    Ok((store, stats))
}
