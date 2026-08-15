use crate::protocol::UsageEvent;
use crate::session::usage::TokenUsage;
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::sync::RwLock;

/// Accumulated session statistics: token usage, context, and turn counts.
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
            let data = tokio::fs::read_to_string(path).await.map_err(|error| {
                anyhow::anyhow!("failed to read stats {}: {error}", path.display())
            })?;
            if data.trim().is_empty() {
                Stats::default()
            } else {
                serde_json::from_str(&data)
                    .map_err(|error| anyhow::anyhow!("corrupt stats {}: {error}", path.display()))?
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
        let path = self.path.clone();
        tokio::task::spawn_blocking(move || {
            crate::session::atomic_file::atomic_replace(&path, format!("{data}\n").as_bytes())
        })
        .await??;
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
        s.current_turn_count = s.current_turn_count.saturating_add(1);
        s.last_updated = chrono_now_rfc3339();
        self.dirty.store(true, Ordering::Release);
    }

    pub async fn record_usage(&self, u: &UsageEvent) {
        let tokens = match TokenUsage::from_provider(u) {
            Ok(tokens) => tokens,
            Err(error) => {
                eprintln!("[mink] Warning: ignoring invalid provider usage: {error}");
                return;
            }
        };
        let mut s = self.stats.write().await;
        s.agent_request_count = s.agent_request_count.saturating_add(1);
        s.total_input_tokens = s.total_input_tokens.saturating_add(tokens.input_tokens);
        s.total_output_tokens = s.total_output_tokens.saturating_add(tokens.output_tokens);
        s.total_cache_read_tokens = s
            .total_cache_read_tokens
            .saturating_add(tokens.cache_read_tokens);
        s.total_cache_creation_tokens = s
            .total_cache_creation_tokens
            .saturating_add(tokens.cache_creation_tokens);
        s.current_context_tokens = usage_token_total(&tokens);

        s.last_updated = chrono_now_rfc3339();
        self.dirty.store(true, Ordering::Release);
    }

    pub async fn record_compact(&self, u: &UsageEvent) {
        let tokens = match TokenUsage::from_provider(u) {
            Ok(tokens) => tokens,
            Err(error) => {
                eprintln!("[mink] Warning: ignoring invalid compaction usage: {error}");
                return;
            }
        };
        let mut s = self.stats.write().await;
        s.compact_request_count = s.compact_request_count.saturating_add(1);
        s.total_input_tokens = s.total_input_tokens.saturating_add(tokens.input_tokens);
        s.total_output_tokens = s.total_output_tokens.saturating_add(tokens.output_tokens);
        s.total_cache_read_tokens = s
            .total_cache_read_tokens
            .saturating_add(tokens.cache_read_tokens);
        s.total_cache_creation_tokens = s
            .total_cache_creation_tokens
            .saturating_add(tokens.cache_creation_tokens);
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
        s.sub_agent_request_count = s.sub_agent_request_count.saturating_add(1);
        s.agent_request_count = s.agent_request_count.saturating_add(request_count);
        s.total_input_tokens = s.total_input_tokens.saturating_add(in_tokens);
        s.total_output_tokens = s.total_output_tokens.saturating_add(out_tokens);
        s.total_cache_read_tokens = s.total_cache_read_tokens.saturating_add(cache_read);
        s.total_cache_creation_tokens =
            s.total_cache_creation_tokens.saturating_add(cache_creation);
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

fn usage_token_total(tokens: &TokenUsage) -> u64 {
    tokens
        .input_tokens
        .saturating_add(tokens.output_tokens)
        .saturating_add(tokens.cache_read_tokens)
        .saturating_add(tokens.cache_creation_tokens)
}

#[cfg(test)]
#[path = "stats_tests.rs"]
mod tests;
