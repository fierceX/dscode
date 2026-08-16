use super::{AgentEvent, AgentEventKind, EventDispatcher, EventSink};
use std::sync::Arc;

#[test]
fn shared_server_protocol_fixture_is_real_agent_event_json() {
    let fixture = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../mink-server/protocol-fixtures/agent-events.json"
    ));
    let expected: serde_json::Value = serde_json::from_str(fixture).unwrap();
    let events: Vec<AgentEvent> = serde_json::from_value(expected.clone()).unwrap();
    assert_eq!(serde_json::to_value(events).unwrap(), expected);
}

struct BlockingSink;

#[async_trait::async_trait]
impl EventSink for BlockingSink {
    async fn on_event(&self, _event: AgentEvent) -> Result<(), String> {
        std::future::pending().await
    }
}

fn info_event(sequence: u64) -> AgentEvent {
    AgentEvent {
        turn_id: None,
        sequence,
        kind: AgentEventKind::Info {
            message: sequence.to_string(),
        },
    }
}

#[tokio::test]
async fn observer_overflow_drops_newest_and_keeps_observer_alive() {
    let dispatcher = EventDispatcher::new(Arc::new(BlockingSink));
    for sequence in 0..2048 {
        dispatcher.dispatch(info_event(sequence));
    }

    dispatcher
        .shutdown_with_timeout(std::time::Duration::from_millis(1))
        .await
        .unwrap_err();
    assert!(
        dispatcher.dropped_events() > 0,
        "overflowing dispatch must record dropped events"
    );
}

struct FailingSink;

#[async_trait::async_trait]
impl EventSink for FailingSink {
    async fn on_event(&self, _event: AgentEvent) -> Result<(), String> {
        Err("observer failure".into())
    }
}

#[tokio::test]
async fn observer_failure_is_reported_at_shutdown() {
    let dispatcher = EventDispatcher::new(Arc::new(FailingSink));
    dispatcher.dispatch(info_event(1));
    tokio::task::yield_now().await;
    dispatcher.dispatch(info_event(2));
    let error = dispatcher.shutdown().await.unwrap_err();
    assert!(error.contains("observer failure"));
    assert!(!error.contains("stopped before runtime shutdown"));
}

#[tokio::test]
async fn observer_shutdown_timeout_aborts_and_reports_failure() {
    let dispatcher = EventDispatcher::new(Arc::new(BlockingSink));
    dispatcher.dispatch(info_event(1));
    tokio::task::yield_now().await;
    let error = dispatcher
        .shutdown_with_timeout(std::time::Duration::from_millis(10))
        .await
        .unwrap_err();
    assert!(error.contains("shutdown timed out"));
}
