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
    t.record_usage(&UsageEvent {
        input_tokens: 100,
        output_tokens: 50,
        cache_read_input_tokens: 30,
        cache_creation_input_tokens: 10,
    })
    .await;
    t.record_usage(&UsageEvent {
        input_tokens: 200,
        output_tokens: 80,
        cache_read_input_tokens: 0,
        cache_creation_input_tokens: 0,
    })
    .await;
    let snap = t.snapshot().await;
    assert_eq!(snap.agent_request_count, 2);
    assert_eq!(snap.total_input_tokens, 300);
    assert_eq!(snap.total_output_tokens, 130);
    assert_eq!(snap.total_cache_read_tokens, 30);
    assert_eq!(snap.total_cache_creation_tokens, 10);
}

#[tokio::test]
async fn record_usage_ignores_negative_provider_tokens() {
    let (t, _) = temp_tracker().await;
    t.record_usage(&UsageEvent {
        input_tokens: -1,
        output_tokens: 50,
        cache_read_input_tokens: 0,
        cache_creation_input_tokens: 0,
    })
    .await;

    let snap = t.snapshot().await;
    assert_eq!(snap.agent_request_count, 0);
    assert_eq!(snap.total_input_tokens, 0);
    assert_eq!(snap.total_output_tokens, 0);
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

/// Old cost fields are accepted and ignored when sessions resume.
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
        "flash_cost_micros": 17,
        "pro_cost_micros": 29,
        "last_updated": "2025-06-01T00:00:00Z"
    });
    let stats: Stats =
        serde_json::from_value(old_json).expect("old format should deserialize without error");
    assert_eq!(stats.current_turn_count, 5);
    assert_eq!(stats.agent_request_count, 12);
    assert_eq!(stats.total_input_tokens, 50000);
    assert_eq!(stats.total_output_tokens, 8000);
}

/// Verifies that completely empty JSON object deserializes to all-defaults.
#[test]
fn empty_json_deserializes_to_defaults() {
    let stats: Stats = serde_json::from_str("{}").expect("empty JSON should deserialize");
    assert_eq!(stats.current_turn_count, 0);
    assert_eq!(stats.total_input_tokens, 0);
}

#[tokio::test]
async fn malformed_stats_are_reported_without_overwrite() {
    let dir = std::env::temp_dir().join(format!(
        "mink-stats-corrupt-{}-{}",
        std::process::id(),
        chrono_now_rfc3339().replace([':', '.'], "-")
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("stats.json");
    std::fs::write(&path, "{not-json").unwrap();
    let error = StatsTracker::load(&path).await.err().unwrap().to_string();
    assert!(error.contains("corrupt stats"), "{error}");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "{not-json");
    let _ = std::fs::remove_dir_all(dir);
}
