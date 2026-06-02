use crate::protocol::UsageEvent;
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::sync::RwLock;

/// Accumulated session statistics: token usage, cost, turn counts.
/// Persisted to stats.json and read back on session resume.
#[derive(Default, Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
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
    /// Per-tier accumulated cost in micro-dollars (1/1,000,000 USD).
    pub flash_cost_micros: u64,
    pub pro_cost_micros: u64,
    pub last_updated: String,
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

    /// Record usage with a specific model tier for per-tier cost tracking.
    pub async fn record_usage_with_tier(&self, u: &UsageEvent, tier: crate::config::ModelTier) {
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

        // Per-tier cost accumulation
        let input_cost = (u.input_tokens as f64) * tier.price_input_per_m() / 1_000_000.0;
        let output_cost = (u.output_tokens as f64) * tier.price_output_per_m() / 1_000_000.0;
        let cache_read_cost =
            (u.cache_read_input_tokens as f64) * tier.price_cache_read_per_m() / 1_000_000.0;
        let cache_creation_cost =
            (u.cache_creation_input_tokens as f64) * tier.price_input_per_m() / 1_000_000.0;
        let delta_micros = ((input_cost + output_cost + cache_read_cost + cache_creation_cost)
            * 1_000_000.0) as u64;

        match tier {
            crate::config::ModelTier::Flash => s.flash_cost_micros += delta_micros,
            crate::config::ModelTier::Pro => s.pro_cost_micros += delta_micros,
        }

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

    pub async fn record_sub_agent(
        &self,
        request_count: u64,
        in_tokens: u64,
        out_tokens: u64,
        cache_read: u64,
        cache_creation: u64,
    ) {
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

pub(crate) fn chrono_now_rfc3339() -> String {
    let now = time::OffsetDateTime::now_utc();
    let fmt = time::format_description::well_known::Rfc3339;
    now.format(&fmt).unwrap_or_else(|_| String::new())
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
        t.record_usage_with_tier(
            &UsageEvent {
                input_tokens: 100,
                output_tokens: 50,
                cache_read_input_tokens: 30,
                cache_creation_input_tokens: 10,
            },
            crate::config::ModelTier::Flash,
        )
        .await;
        t.record_usage_with_tier(
            &UsageEvent {
                input_tokens: 200,
                output_tokens: 80,
                cache_read_input_tokens: 0,
                cache_creation_input_tokens: 0,
            },
            crate::config::ModelTier::Pro,
        )
        .await;
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
        t.record_compact(&UsageEvent {
            input_tokens: 500,
            output_tokens: 20,
            cache_read_input_tokens: 400,
            cache_creation_input_tokens: 0,
        })
        .await;
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
        let mtime1 = tokio::fs::metadata(&path)
            .await
            .unwrap()
            .modified()
            .unwrap();
        t.record_turn().await;
        t.flush_if_dirty().await.unwrap();
        let mtime2 = tokio::fs::metadata(&path)
            .await
            .unwrap()
            .modified()
            .unwrap();
        assert!(mtime2 > mtime1);
    }

    /// Verifies that old-format stats.json (without flash_cost_micros/pro_cost_micros)
    /// deserializes correctly and preserves existing accumulated fields.
    #[test]
    fn old_format_json_preserves_existing_fields() {
        let old_json = serde_json::json!({
            "current_turn_count": 5,
            "agent_request_count": 12,
            "compact_request_count": 2,
            "sub_agent_request_count": 1,
            "total_input_tokens": 50000,
            "total_output_tokens": 8000,
            "total_cache_read_tokens": 30000,
            "total_cache_creation_tokens": 5000,
            "current_context_tokens": 6000,
            "last_updated": "2025-06-01T00:00:00Z"
            // flash_cost_micros and pro_cost_micros missing — old format
        });
        let stats: Stats =
            serde_json::from_value(old_json).expect("old format should deserialize without error");
        assert_eq!(stats.current_turn_count, 5);
        assert_eq!(stats.agent_request_count, 12);
        assert_eq!(stats.total_input_tokens, 50000);
        assert_eq!(stats.total_output_tokens, 8000);
        // Missing fields default to 0
        assert_eq!(stats.flash_cost_micros, 0);
        assert_eq!(stats.pro_cost_micros, 0);
    }

    /// Verifies that completely empty JSON object deserializes to all-defaults.
    #[test]
    fn empty_json_deserializes_to_defaults() {
        let stats: Stats = serde_json::from_str("{}").expect("empty JSON should deserialize");
        assert_eq!(stats.current_turn_count, 0);
        assert_eq!(stats.total_input_tokens, 0);
        assert_eq!(stats.flash_cost_micros, 0);
        assert_eq!(stats.pro_cost_micros, 0);
    }
}
