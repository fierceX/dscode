use super::*;
use crate::agent::prefix::PrefixManager;
use serde_json::json;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

#[tokio::test]
async fn maybe_compact_skips_after_already_compacted_this_turn() -> anyhow::Result<()> {
    let ctx = crate::regression::test_context_for_agent("compactor-already").await?;
    let prefix = PrefixManager::new(ctx.clone());
    let mut compactor = TurnCompactor::new(ctx, prefix);
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
            LlmModelTarget::new("test-model", None),
        )
        .await?;

    assert!(!did_compact);
    Ok(())
}

#[tokio::test]
#[ignore = "requires local loopback sockets"]
async fn maybe_compact_success_refreshes_context_and_prefix() -> anyhow::Result<()> {
    let (api_url, _server) = start_summary_server("Task focus: compacted\nLatest request: test\nProgress: done\nTool evidence: none\nReflections: none").await?;
    let ctx = crate::regression::test_context_for_agent_with_config("compactor-success", |cfg| {
        cfg.base_url = api_url.clone();
        // 与其余 compaction 测试一致：热尾部目标压到 1，使小对话也能
        // 通过"节省 ≥10%"门控（默认 256K 尾部对微型对话是恒等压缩）。
        cfg.context_compact_tail_tokens = 1;
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
    let mut compactor = TurnCompactor::new(ctx.clone(), prefix);
    let mut messages = ctx.store.lines().await?;
    let mut system_prompt = String::new();
    let mut tools = Vec::new();

    let did_compact = compactor
        .maybe_compact(
            "manual",
            &mut messages,
            &mut system_prompt,
            &mut tools,
            LlmModelTarget::new("test-model", None),
        )
        .await?;

    assert!(did_compact);
    assert!(compactor.compacted_this_turn());
    assert_eq!(messages, ctx.compaction.active_messages().await?);
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
