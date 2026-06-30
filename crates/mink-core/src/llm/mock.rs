use crate::protocol::Event;
use anyhow::Result;
use std::sync::Mutex;

use super::client::{LlmBackend, LlmRequest, LlmResponseStream};

pub struct MockLlmClient {
    pub model_name: String,
    canned_events: Mutex<std::vec::IntoIter<Vec<Result<Event>>>>,
}

impl MockLlmClient {
    pub fn new(model: &str, sequences: Vec<Vec<Result<Event>>>) -> Self {
        Self {
            model_name: model.to_string(),
            canned_events: Mutex::new(sequences.into_iter()),
        }
    }
}

#[async_trait::async_trait]
impl LlmBackend for MockLlmClient {
    fn name(&self) -> &str {
        "mock"
    }

    async fn stream(&self, _request: LlmRequest) -> Result<LlmResponseStream> {
        let mut iter = self.canned_events.lock().unwrap();
        let events = iter.next().unwrap_or_default();
        Ok(LlmResponseStream {
            events: Box::pin(futures::stream::iter(events)),
            attempt_count: 1,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::*;

    #[tokio::test]
    async fn mock_empty_sequence() {
        let client = MockLlmClient::new("test-model", vec![vec![]]);
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
        let client = MockLlmClient::new("m", vec![events]);
        assert_eq!(client.model_name, "m");
    }
}
