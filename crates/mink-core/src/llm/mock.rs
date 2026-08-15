use crate::protocol::Event;
use anyhow::Result;
use std::sync::Mutex;

use super::client::{LlmBackend, LlmRequest, LlmResponseStream};

pub struct MockLlmBackend {
    pub model_name: String,
    canned_events: Mutex<std::vec::IntoIter<Vec<Result<Event>>>>,
}

impl MockLlmBackend {
    pub fn new(model: &str, sequences: Vec<Vec<Result<Event>>>) -> Self {
        Self {
            model_name: model.to_string(),
            canned_events: Mutex::new(sequences.into_iter()),
        }
    }
}

#[async_trait::async_trait]
impl LlmBackend for MockLlmBackend {
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
#[path = "mock_tests.rs"]
mod tests;
