use crate::agent::prefix::PrefixManager;
use crate::context::AgentSharedContext;
use anyhow::Result;
use std::sync::Arc;

pub struct TurnCompactor {
    ctx: Arc<AgentSharedContext>,
    compacted_this_turn: bool,
}

impl TurnCompactor {
    pub fn new(ctx: Arc<AgentSharedContext>) -> Self {
        Self {
            ctx,
            compacted_this_turn: false,
        }
    }

    pub fn reset(&mut self) {
        self.compacted_this_turn = false;
    }

    pub fn compacted_this_turn(&self) -> bool {
        self.compacted_this_turn
    }

    pub async fn maybe_compact(
        &mut self,
        trigger: &str,
        messages: &mut Vec<serde_json::Value>,
        system_prompt: &mut String,
        tools_json: &mut Vec<serde_json::Value>,
        prefix: &PrefixManager,
    ) -> Result<bool> {
        if self.compacted_this_turn {
            return Ok(false);
        }
        let stats = self.ctx.stats.snapshot().await;
        let compacted = self
            .ctx
            .compaction
            .evaluate_and_compact(trigger, stats.current_context_tokens as usize)
            .await;
        if let Ok((did_compact, _)) = compacted
            && did_compact
        {
            self.compacted_this_turn = true;
            prefix.invalidate();
            *messages = self.ctx.store.lines().await?;
            (*system_prompt, *tools_json) = prefix.ensure()?;
            return Ok(true);
        }
        Ok(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::prefix::PrefixManager;
    use serde_json::json;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    #[tokio::test]
    async fn maybe_compact_skips_after_already_compacted_this_turn() -> anyhow::Result<()> {
        let ctx = crate::regression::test_context_for_agent("compactor-already").await?;
        let prefix = PrefixManager::new(ctx.clone());
        let mut compactor = TurnCompactor::new(ctx);
        compactor.compacted_this_turn = true;
        let mut messages = Vec::new();
        let mut system_prompt = String::new();
        let mut tools = Vec::new();

        let did_compact = compactor
            .maybe_compact(
                "manual",
                &mut messages,
                &mut system_prompt,
                &mut tools,
                &prefix,
            )
            .await?;

        assert!(!did_compact);
        Ok(())
    }

    #[tokio::test]
    #[ignore = "requires local loopback sockets"]
    async fn maybe_compact_success_refreshes_context_and_prefix() -> anyhow::Result<()> {
        let (api_url, _server) = start_summary_server("Task focus: compacted\nLatest request: test\nProgress: done\nTool evidence: none\nReflections: none").await?;
        let ctx =
            crate::regression::test_context_for_agent_with_config("compactor-success", |cfg| {
                cfg.base_url = api_url.clone();
            })
            .await?;
        for idx in 0..3 {
            ctx.store.add_user(&format!("user {idx}")).await?;
            ctx.store
                .add_assistant(&format!("assistant {idx}"), "", &[])
                .await?;
        }
        let prefix = PrefixManager::new(ctx.clone());
        let (_old_prompt, _old_tools) = prefix.ensure()?;
        let mut compactor = TurnCompactor::new(ctx.clone());
        let mut messages = ctx.store.lines().await?;
        let mut system_prompt = String::new();
        let mut tools = Vec::new();

        let did_compact = compactor
            .maybe_compact(
                "manual",
                &mut messages,
                &mut system_prompt,
                &mut tools,
                &prefix,
            )
            .await?;

        assert!(did_compact);
        assert!(compactor.compacted_this_turn());
        assert_eq!(messages, ctx.store.lines().await?);
        assert!(!system_prompt.is_empty());
        assert!(!tools.is_empty());
        Ok(())
    }

    async fn start_summary_server(
        summary_text: &str,
    ) -> anyhow::Result<(String, tokio::task::JoinHandle<()>)> {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let addr = listener.local_addr()?;
        let summary_text = summary_text.to_string();
        let handle = tokio::spawn(async move {
            let Ok((mut socket, _)) = listener.accept().await else {
                return;
            };
            let mut buf = Vec::new();
            let mut chunk = [0u8; 1024];
            while let Ok(n) = socket.read(&mut chunk).await {
                if n == 0 {
                    return;
                }
                buf.extend_from_slice(&chunk[..n]);
                if buf.windows(4).any(|w| w == b"\r\n\r\n") {
                    break;
                }
            }
            let body = format!(
                "data: {}\n\ndata: {}\n\ndata: [DONE]\n\n",
                json!({"choices":[{"delta":{"content":summary_text}}]}),
                json!({"choices":[{"finish_reason":"stop","delta":{}}],"usage":{"prompt_tokens":4,"completion_tokens":2}})
            );
            let response = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            let _ = socket.write_all(response.as_bytes()).await;
        });
        Ok((format!("http://{addr}/chat/completions"), handle))
    }
}
