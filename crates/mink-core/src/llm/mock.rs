use crate::context::AgentSharedContext;
use crate::protocol::Event;
use anyhow::Result;
use std::sync::Mutex;

use super::client::LlmClient;
use super::client::MeteredStream;
use crate::session::usage::UsageKind;

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
impl LlmClient for MockLlmClient {
    fn model(&self) -> &str {
        &self.model_name
    }

    async fn stream(
        &self,
        ctx: &AgentSharedContext,
        _messages_json: &[serde_json::Value],
        _tools_json: &[serde_json::Value],
        _system_prompt: &str,
    ) -> Result<Box<dyn futures::Stream<Item = Result<Event>> + Unpin + Send>> {
        let mut iter = self.canned_events.lock().unwrap();
        let events = iter.next().unwrap_or_default();
        let capture = ctx.usage.capture(
            ctx.usage_scope(if ctx.is_sub_agent {
                UsageKind::SubAgent
            } else {
                UsageKind::Agent
            }),
            self.model_name.clone(),
        );
        Ok(Box::new(MeteredStream::new(
            futures::stream::iter(events),
            capture,
            1,
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::*;

    #[tokio::test]
    async fn mock_empty_sequence() {
        let client = MockLlmClient::new("test-model", vec![vec![]]);
        assert_eq!(client.model(), "test-model");
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
        assert_eq!(client.model(), "m");
    }
}
