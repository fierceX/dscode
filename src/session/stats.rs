use crate::protocol::UsageEvent;
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::sync::RwLock;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Stats {
    pub current_turn_count: u64,
    pub agent_request_count: u64,
    pub compact_request_count: u64,
    pub sub_agent_request_count: u64,
    pub total_input_tokens: u64,
    pub total_output_tokens: u64,
    pub total_cache_read_tokens: u64,
    pub total_cache_creation_tokens: u64,
    pub current_context_tokens: u64,
    pub last_updated: String,
}

impl Default for Stats {
    fn default() -> Self {
        Self {
            current_turn_count: 0,
            agent_request_count: 0,
            compact_request_count: 0,
            sub_agent_request_count: 0,
            total_input_tokens: 0,
            total_output_tokens: 0,
            total_cache_read_tokens: 0,
            total_cache_creation_tokens: 0,
            current_context_tokens: 0,
            last_updated: String::new(),
        }
    }
}

/// StatsTracker provides thread-safe, batched stats persistence.
/// Read operations use RwLock (read); writes mark dirty and flush occurs once per turn.
pub struct StatsTracker {
    stats: RwLock<Stats>,
    path: std::path::PathBuf,
    dirty: AtomicBool,
}

impl StatsTracker {
    pub async fn load(path: &Path) -> Result<Arc<Self>> {
        let stats = if path.exists() {
            match tokio::fs::read_to_string(path).await {
                Ok(data) => serde_json::from_str(&data).unwrap_or_default(),
                Err(_) => Stats::default(),
            }
        } else {
            Stats::default()
        };
        Ok(Arc::new(Self {
            stats: RwLock::new(stats),
            path: path.to_path_buf(),
            dirty: AtomicBool::new(false),
        }))
    }

    pub async fn flush(&self) -> Result<()> {
        let stats = self.stats.read().await;
        let data = serde_json::to_string(&*stats)?;
        tokio::fs::write(&self.path, data + "\n").await?;
        self.dirty.store(false, Ordering::Release);
        Ok(())
    }

    pub async fn flush_if_dirty(&self) -> Result<()> {
        if self.dirty.load(Ordering::Acquire) {
            self.flush().await?;
        }
        Ok(())
    }

    pub async fn record_turn(&self) {
        let mut s = self.stats.write().await;
        s.current_turn_count += 1;
        s.last_updated = chrono_now_rfc3339();
        self.dirty.store(true, Ordering::Release);
    }

    pub async fn record_usage(&self, u: &UsageEvent) {
        let mut s = self.stats.write().await;
        s.agent_request_count += 1;
        s.total_input_tokens += u.input_tokens as u64;
        s.total_output_tokens += u.output_tokens as u64;
        s.total_cache_read_tokens += u.cache_read_input_tokens as u64;
        s.total_cache_creation_tokens += u.cache_creation_input_tokens as u64;
        s.current_context_tokens = (u.input_tokens
            + u.output_tokens
            + u.cache_read_input_tokens
            + u.cache_creation_input_tokens) as u64;
        s.last_updated = chrono_now_rfc3339();
        self.dirty.store(true, Ordering::Release);
    }

    pub async fn record_compact(&self, u: &UsageEvent) {
        let mut s = self.stats.write().await;
        s.compact_request_count += 1;
        s.total_input_tokens += u.input_tokens as u64;
        s.total_output_tokens += u.output_tokens as u64;
        s.total_cache_read_tokens += u.cache_read_input_tokens as u64;
        s.total_cache_creation_tokens += u.cache_creation_input_tokens as u64;
        s.last_updated = chrono_now_rfc3339();
        self.dirty.store(true, Ordering::Release);
    }

    pub async fn record_sub_agent(&self, request_count: u64, in_tokens: u64, out_tokens: u64, cache_read: u64, cache_creation: u64) {
        let mut s = self.stats.write().await;
        s.sub_agent_request_count += 1;
        s.agent_request_count += request_count;
        s.total_input_tokens += in_tokens;
        s.total_output_tokens += out_tokens;
        s.total_cache_read_tokens += cache_read;
        s.total_cache_creation_tokens += cache_creation;
        s.last_updated = chrono_now_rfc3339();
        self.dirty.store(true, Ordering::Release);
    }

    pub async fn recalculate_turn_count(&self, remaining_turns: u64) {
        let mut s = self.stats.write().await;
        s.current_turn_count = remaining_turns;
        s.last_updated = chrono_now_rfc3339();
        self.dirty.store(true, Ordering::Release);
    }

    pub async fn snapshot(&self) -> Stats {
        self.stats.read().await.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::UsageEvent;
    use std::sync::Arc;
    use tokio;

    async fn temp_tracker() -> (Arc<StatsTracker>, std::path::PathBuf) {
        use std::sync::atomic::{AtomicU64, Ordering};
        static CNT: AtomicU64 = AtomicU64::new(0);
        let n = CNT.fetch_add(1, Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!("stats-test-{}-{}", std::process::id(), n));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("stats.json");
        let tracker = StatsTracker::load(&path).await.unwrap();
        (tracker, path)
    }

    #[tokio::test]
    async fn record_turn_increments() {
        let (t, _) = temp_tracker().await;
        t.record_turn().await;
        t.record_turn().await;
        let snap = t.snapshot().await;
        assert_eq!(snap.current_turn_count, 2);
    }

    #[tokio::test]
    async fn record_usage_accumulates_tokens() {
        let (t, _) = temp_tracker().await;
        t.record_usage(&UsageEvent { input_tokens: 100, output_tokens: 50, cache_read_input_tokens: 30, cache_creation_input_tokens: 10 }).await;
        t.record_usage(&UsageEvent { input_tokens: 200, output_tokens: 80, cache_read_input_tokens: 0, cache_creation_input_tokens: 0 }).await;
        let snap = t.snapshot().await;
        assert_eq!(snap.agent_request_count, 2);
        assert_eq!(snap.total_input_tokens, 300);
        assert_eq!(snap.total_output_tokens, 130);
        assert_eq!(snap.total_cache_read_tokens, 30);
        assert_eq!(snap.total_cache_creation_tokens, 10);
    }

    #[tokio::test]
    async fn record_compact_increments_count() {
        let (t, _) = temp_tracker().await;
        t.record_compact(&UsageEvent { input_tokens: 500, output_tokens: 20, cache_read_input_tokens: 400, cache_creation_input_tokens: 0 }).await;
        let snap = t.snapshot().await;
        assert_eq!(snap.compact_request_count, 1);
        assert_eq!(snap.total_input_tokens, 500);
    }

    #[tokio::test]
    async fn record_sub_agent() {
        let (t, _) = temp_tracker().await;
        t.record_sub_agent(3, 1000, 500, 200, 100).await;
        let snap = t.snapshot().await;
        assert_eq!(snap.sub_agent_request_count, 1);
        assert_eq!(snap.agent_request_count, 3);
        assert_eq!(snap.total_input_tokens, 1000);
    }

    #[tokio::test]
    async fn dirty_flag_only_flushes_when_dirty() {
        let (t, path) = temp_tracker().await;
        t.flush().await.unwrap(); // reset dirty
        t.flush_if_dirty().await.unwrap(); // should be no-op
        let mtime1 = tokio::fs::metadata(&path).await.unwrap().modified().unwrap();
        t.record_turn().await;
        t.flush_if_dirty().await.unwrap();
        let mtime2 = tokio::fs::metadata(&path).await.unwrap().modified().unwrap();
        assert!(mtime2 > mtime1);
    }
}

fn chrono_now_rfc3339() -> String {
    let now = time::OffsetDateTime::now_utc();
    let fmt = time::format_description::well_known::Rfc3339;
    now.format(&fmt).unwrap_or_else(|_| String::new())
}
