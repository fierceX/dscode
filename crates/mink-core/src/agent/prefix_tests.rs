use super::*;
use crate::session::prefix::ImmutablePrefix;

#[tokio::test]
async fn ensure_reuses_valid_cached_prefix() -> anyhow::Result<()> {
    let ctx = crate::regression::test_context_for_agent("prefix-cache-hit").await?;
    let manager = PrefixManager::new(ctx.clone());
    let (first_prompt, first_tools) = manager.ensure()?;
    let first_names: Vec<_> = first_tools
        .iter()
        .filter_map(|schema| schema.get("name").and_then(serde_json::Value::as_str))
        .collect();
    assert_eq!(
        first_names,
        ctx.tool_surface.names().collect::<Vec<_>>(),
        "request schemas must come from the resolved surface"
    );
    let cached_fingerprint = ctx
        .immutable_prefix
        .lock()
        .unwrap()
        .as_ref()
        .unwrap()
        .fingerprint()
        .to_string();

    let (second_prompt, second_tools) = manager.ensure()?;

    assert_eq!(first_prompt, second_prompt);
    assert_eq!(first_tools, second_tools);
    assert_eq!(
        ctx.immutable_prefix
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .fingerprint(),
        cached_fingerprint
    );
    Ok(())
}

#[tokio::test]
async fn ensure_logs_prefix_snapshot_once_and_rebuild_replaces_it() -> anyhow::Result<()> {
    let ctx = crate::regression::test_context_for_agent("prefix-snapshot-event").await?;
    let manager = PrefixManager::new(ctx.clone());
    let (system_prompt, tools) = manager.ensure()?;

    let snapshot = |events: &str| -> Vec<serde_json::Value> {
        events
            .lines()
            .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
            .filter(|evt| {
                evt.get("type").and_then(serde_json::Value::as_str) == Some("prefix_snapshot")
            })
            .collect()
    };

    ctx.flush_event_log().await?;
    let events = tokio::fs::read_to_string(&ctx.events_path).await?;
    let snapshots = snapshot(&events);
    assert_eq!(
        snapshots.len(),
        1,
        "first build writes exactly one snapshot"
    );
    let evt = &snapshots[0];
    assert_eq!(evt["version"], 1);
    assert_eq!(
        evt["fingerprint"].as_str().unwrap(),
        ctx.immutable_prefix
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .fingerprint()
    );
    assert_eq!(evt["system_prompt"].as_str().unwrap(), system_prompt);
    assert_eq!(evt["tools_json"], serde_json::Value::Array(tools.clone()));

    // Cache hit must not duplicate the snapshot.
    manager.ensure()?;
    ctx.flush_event_log().await?;
    let events = tokio::fs::read_to_string(&ctx.events_path).await?;
    assert_eq!(snapshot(&events).len(), 1);

    // Invalidation rebuild replaces the snapshot with the new fingerprint.
    manager.invalidate();
    manager.ensure()?;
    ctx.flush_event_log().await?;
    let events = tokio::fs::read_to_string(&ctx.events_path).await?;
    let snapshots = snapshot(&events);
    assert_eq!(snapshots.len(), 2, "rebuild appends one more snapshot");
    assert_eq!(
        snapshots[1]["fingerprint"].as_str().unwrap(),
        ctx.immutable_prefix
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .fingerprint()
    );
    Ok(())
}

#[tokio::test]
async fn ensure_drops_corrupt_cached_prefix_and_rebuilds() -> anyhow::Result<()> {
    let ctx = crate::regression::test_context_for_agent("prefix-cache-invalid").await?;
    *ctx.immutable_prefix.lock().unwrap() = Some(ImmutablePrefix::new_with_fingerprint(
        "stale".into(),
        vec![serde_json::json!({"name":"Bash"})],
        String::new(),
        "bad-fingerprint".into(),
    ));
    let manager = PrefixManager::new(ctx.clone());

    let (system_prompt, tools) = manager.ensure()?;

    assert_ne!(system_prompt, "stale");
    assert!(!tools.is_empty());
    assert!(
        ctx.immutable_prefix
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .verify_fingerprint()
    );
    Ok(())
}
