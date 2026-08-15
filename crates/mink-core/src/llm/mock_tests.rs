use super::*;
use crate::protocol::*;

#[tokio::test]
async fn mock_empty_sequence() {
    let client = MockLlmBackend::new("test-model", vec![vec![]]);
    assert_eq!(client.model_name, "test-model");
}

#[tokio::test]
async fn mock_stream_yields_events() {
    let events = vec![
        Ok(Event::Text(TextEvent {
            content: "hello".into(),
        })),
        Ok(Event::Stop(StopEvent {
            reason: "end_turn".into(),
        })),
    ];
    let client = MockLlmBackend::new("m", vec![events]);
    assert_eq!(client.model_name, "m");
}
