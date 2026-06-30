//! Example custom LLM backend for embedded Rust applications.
//!
//! The backend lives in the embedding application. `mink-core` only receives a
//! trait object through `AgentOptions::with_llm_backend`.

use anyhow::Result;
use futures::stream;
use mink::prelude::{
    AgentOptions, AgentRuntime, LlmBackend, LlmEvent, LlmEventStream, LlmRequest,
    LlmRequestFailure, LlmResponseStream, LlmStopEvent, LlmTextEvent, LlmUsageEvent,
};
use serde_json::Value;
use std::sync::Arc;

struct EchoBackend;

#[async_trait::async_trait]
impl LlmBackend for EchoBackend {
    fn name(&self) -> &str {
        "echo"
    }

    async fn stream(&self, request: LlmRequest) -> Result<LlmResponseStream> {
        if request.cancel.is_cancelled() {
            return Err(LlmRequestFailure {
                attempt_count: 0,
                error: anyhow::anyhow!("request was cancelled before dispatch"),
            }
            .into());
        }

        let last_user_message = last_user_text(&request.messages)
            .unwrap_or("<no user message>")
            .to_string();
        let alias = request.model_alias.as_deref().unwrap_or("<direct model>");
        let text = format!(
            "custom backend={} model={} alias={} received: {}",
            self.name(),
            request.model,
            alias,
            last_user_message
        );

        let events: LlmEventStream = Box::pin(stream::iter(vec![
            Ok(LlmEvent::Text(LlmTextEvent { content: text })),
            Ok(LlmEvent::Usage(LlmUsageEvent {
                input_tokens: 16,
                output_tokens: 12,
                cache_read_input_tokens: 0,
                cache_creation_input_tokens: 0,
            })),
            Ok(LlmEvent::Stop(LlmStopEvent {
                reason: "end_turn".to_string(),
            })),
        ]));

        Ok(LlmResponseStream {
            events,
            attempt_count: 1,
        })
    }
}

fn last_user_text(messages: &[Value]) -> Option<&str> {
    messages.iter().rev().find_map(|message| {
        (message.get("role")?.as_str()? == "user")
            .then(|| message.get("content")?.as_str())
            .flatten()
    })
}

#[tokio::main]
async fn main() -> Result<()> {
    let root = std::env::temp_dir().join("mink-custom-llm-example");
    let cwd = root.join("workspace");
    std::fs::create_dir_all(&cwd)?;

    let mut options = AgentOptions::new(root.join("session"), &cwd)
        .with_model("local")
        .with_enabled_tools(Vec::<String>::new())
        .with_llm_backend(Arc::new(EchoBackend));
    options
        .config_mut()
        .model_aliases
        .insert("local".to_string(), "private-model-v1".to_string());

    let runtime = AgentRuntime::start_with_options(options).await?;
    let outcome = runtime
        .run_turn("Explain how custom LLM injection works.")
        .await?;

    println!("{}", outcome.text);
    println!(
        "usage: requests={}, input={}, output={}, cost_nano_cny={}",
        outcome.usage.request_count,
        outcome.usage.tokens.input_tokens,
        outcome.usage.tokens.output_tokens,
        outcome.usage.cost_nano_cny
    );

    runtime.shutdown().await?;
    Ok(())
}
