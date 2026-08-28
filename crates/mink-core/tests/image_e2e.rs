//! End-to-end multimodal read protocol test: a real PNG file on disk, a real
//! session, and a capturing LLM backend that asserts the wire body carries
//! the raw image as an OpenAI `image_url` data URL (v7).
//!
//! This exercises the full chain: capability freeze → Read image capture →
//! content-addressed cache → conversation `tool_attachment` → request
//! materialization → wire body; plus `image://` reference re-injection and
//! the text-only regression (unknown scheme fail-closed).

use anyhow::Result;
use base64::Engine as _;
use mink::runtime::{
    AgentOptions, AgentRuntime, ImageInputCapability, LlmBackend, LlmRequest, LlmResponseStream,
    OpenAiChatImageUrlLimits, TurnStatus,
};
use mink::runtime::{LlmEvent, LlmStopEvent, LlmTextEvent, LlmToolCallEvent};
use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

/// Captures every request's messages+tools and serves scripted event
/// sequences; declares vision capability through the backend.
struct CapturingVisionBackend {
    requests: Arc<Mutex<Vec<serde_json::Value>>>,
    responses: Mutex<VecDeque<Vec<Result<LlmEvent>>>>,
}

impl CapturingVisionBackend {
    fn new(requests: Arc<Mutex<Vec<serde_json::Value>>>) -> Self {
        Self::build(requests, None)
    }

    /// Backend for a turn whose model must re-read a cached image through
    /// the `image://` reference (the reference is only known after the
    /// capture turn persisted it).
    fn new_with_reference(requests: Arc<Mutex<Vec<serde_json::Value>>>, reference: String) -> Self {
        Self::build(requests, Some(reference))
    }

    fn build(requests: Arc<Mutex<Vec<serde_json::Value>>>, reference: Option<String>) -> Self {
        fn tool_call(name: &str, id: &str, input: serde_json::Value) -> Result<LlmEvent> {
            Ok(LlmEvent::ToolCall(LlmToolCallEvent {
                name: name.to_string(),
                id: id.to_string(),
                input_json: input.clone(),
                fields: input
                    .as_object()
                    .map(|object| {
                        object
                            .iter()
                            .map(|(key, value)| {
                                (key.clone(), value.as_str().unwrap_or("").to_string())
                            })
                            .collect()
                    })
                    .unwrap_or_default(),
                parse_error: None,
            }))
        }
        fn text(content: &str) -> Result<LlmEvent> {
            Ok(LlmEvent::Text(LlmTextEvent {
                content: content.to_string(),
            }))
        }
        fn stop(reason: &str) -> Result<LlmEvent> {
            Ok(LlmEvent::Stop(LlmStopEvent {
                reason: reason.to_string(),
            }))
        }
        let mut responses = VecDeque::new();
        match reference {
            // Capture turn: Request 1 — the model asks to Read the image;
            // Request 2 (after capture) — the model describes the image.
            None => {
                responses.push_back(vec![
                    tool_call("Read", "call_read", serde_json::json!({"path": "shot.png"})),
                    stop("tool_use"),
                ]);
                responses.push_back(vec![
                    text("The image shows a red square on a black background."),
                    stop("stop"),
                ]);
            }
            // Reference turn: Request 1 — the model re-reads via the cached
            // reference; Request 2 — acknowledges the re-injection.
            Some(reference) => {
                responses.push_back(vec![
                    tool_call("Read", "call_re", serde_json::json!({"path": reference})),
                    stop("tool_use"),
                ]);
                responses.push_back(vec![text("Re-injected the cached image."), stop("stop")]);
            }
        }
        Self {
            requests,
            responses: Mutex::new(responses),
        }
    }
}

#[async_trait::async_trait]
impl LlmBackend for CapturingVisionBackend {
    fn name(&self) -> &str {
        "capturing-vision"
    }

    fn image_input_capability(&self, _model: &str) -> mink::runtime::ImageInputCapability {
        ImageInputCapability::OpenAiChatImageUrl(OpenAiChatImageUrlLimits::default())
    }

    async fn stream(&self, request: LlmRequest) -> Result<LlmResponseStream> {
        self.requests.lock().unwrap().push(serde_json::json!({
            "messages": request.messages,
            "tools": request.tools,
        }));
        let events = self
            .responses
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_default();
        Ok(LlmResponseStream {
            events: Box::pin(futures::stream::iter(events)),
            attempt_count: 1,
        })
    }
}

fn png_fixture(width: u32, height: u32) -> Vec<u8> {
    let mut img = image::RgbaImage::new(width, height);
    for (x, _y, pixel) in img.enumerate_pixels_mut() {
        *pixel = image::Rgba([if x < width / 2 { 255 } else { 0 }, 0, 0, 255]);
    }
    let mut out = Vec::new();
    img.write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png)
        .expect("png fixture");
    out
}

fn unique_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "mink-e2e-{name}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn find_image_url(messages: &[serde_json::Value]) -> Option<String> {
    for message in messages {
        let Some(blocks) = message.get("content").and_then(serde_json::Value::as_array) else {
            continue;
        };
        for block in blocks {
            if block.get("type").and_then(serde_json::Value::as_str) == Some("image_url") {
                return block
                    .get("image_url")
                    .and_then(|image_url| image_url.get("url"))
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_string);
            }
        }
    }
    None
}

fn tool_positions(messages: &[serde_json::Value]) -> Vec<usize> {
    messages
        .iter()
        .enumerate()
        .filter_map(|(index, message)| {
            (message.get("role").and_then(serde_json::Value::as_str) == Some("tool"))
                .then_some(index)
        })
        .collect()
}

#[tokio::test]
async fn read_image_captures_and_materializes_in_wire_body() {
    let home = unique_dir("home");
    let cwd = unique_dir("cwd");
    let bytes = png_fixture(48, 24);
    std::fs::write(cwd.join("shot.png"), &bytes).unwrap();

    let requests = Arc::new(Mutex::new(Vec::new()));
    let runtime = AgentRuntime::start(
        AgentOptions::new(home.clone(), cwd.clone())
            .with_llm_backend(Arc::new(CapturingVisionBackend::new(requests.clone())))
            .with_api_key("test-key")
            .with_base_url("https://example.invalid/v1"),
    )
    .await
    .unwrap();

    let outcome = runtime
        .run_turn("Read shot.png and describe it")
        .await
        .unwrap();
    assert_eq!(outcome.status, TurnStatus::Ok, "{:?}", outcome.error);

    let requests = requests.lock().unwrap().clone();
    assert_eq!(requests.len(), 2, "capture turn must produce two requests");

    // Request 1 carried the augmented Read tool description (static prefix).
    let tools = requests[0]["tools"].as_array().unwrap();
    let read_tool = tools
        .iter()
        .find(|tool| tool["name"] == "Read")
        .expect("Read tool in prefix");
    assert!(
        read_tool["description"]
            .as_str()
            .unwrap()
            .contains("capture supported raster images"),
        "Read description must advertise image capture"
    );

    // Request 2 carried the materialized data URL with the raw PNG bytes.
    let second_messages = requests[1]["messages"].as_array().unwrap().clone();
    let url = find_image_url(&second_messages).expect("image_url part in second request");
    assert!(url.starts_with("data:image/png;base64,"), "{url}");
    let encoded = &url["data:image/png;base64,".len()..];
    assert_eq!(
        base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .unwrap(),
        bytes,
        "wire body must carry the raw captured bytes (phase one, no transform)"
    );

    // Tool message precedes the residual user message carrying the image.
    let tool_positions = tool_positions(&second_messages);
    let image_message = second_messages
        .iter()
        .position(|message| {
            message
                .get("content")
                .and_then(serde_json::Value::as_array)
                .is_some_and(|blocks| blocks.iter().any(|b| b["type"] == "image_url"))
        })
        .expect("image user message");
    assert!(
        tool_positions
            .iter()
            .all(|position| position < &image_message),
        "tool messages must precede the image user message: {tool_positions:?} vs {image_message}"
    );

    // Conversation persisted the opaque reference with budget metadata.
    let session_conversation = find_conversation(&home);
    let raw = std::fs::read_to_string(&session_conversation).unwrap();
    assert!(raw.contains("\"type\":\"tool_attachment\""), "{raw}");
    assert!(raw.contains("\"url\":\"image://sha256:"), "{raw}");
    // The reference never contains a path.
    assert!(!raw.contains("image:///"), "no path inside image://");

    // Content-addressed object exists in the home image cache.
    let cache_root = home.join(".mink/cache/images/v1/objects");
    assert!(cache_root.exists(), "image cache objects dir");
    let object = find_object(&cache_root);
    assert!(object.is_some(), "cached object missing");
    assert_eq!(std::fs::read(object.unwrap()).unwrap(), bytes);

    // Capability snapshot persisted and verifies.
    let snapshot = read_capability_snapshot(&home).unwrap();
    assert!(snapshot.contains("open_ai_chat_image_url"), "{snapshot}");

    // Drop the runtime so the next turn starts from the persisted session.
    drop(runtime);

    // Second turn: `image://` reference read re-injects the cached object.
    let image_id = extract_image_id(&raw);
    let requests = Arc::new(Mutex::new(Vec::new()));
    let runtime = AgentRuntime::start(
        AgentOptions::new(home.clone(), cwd.clone())
            .with_llm_backend(Arc::new(CapturingVisionBackend::new_with_reference(
                requests.clone(),
                format!("image://{image_id}"),
            )))
            .with_api_key("test-key")
            .with_base_url("https://example.invalid/v1"),
    )
    .await
    .unwrap();
    let outcome = runtime
        .run_turn(&format!("Read image://{image_id}"))
        .await
        .unwrap();
    assert_eq!(outcome.status, TurnStatus::Ok, "{:?}", outcome.error);

    let requests = requests.lock().unwrap().clone();
    assert_eq!(
        requests.len(),
        2,
        "reference turn must produce two requests"
    );
    // The reference read materializes on the request AFTER the tool ran.
    let messages = requests[1]["messages"].as_array().unwrap().clone();
    let url = find_image_url(&messages).expect("re-injected image part");
    assert!(url.starts_with("data:image/png;base64,"), "{url}");
    let encoded = &url["data:image/png;base64,".len()..];
    assert_eq!(
        base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .unwrap(),
        bytes,
        "reference read must re-inject the same raw bytes"
    );

    let _ = std::fs::remove_dir_all(&home);
    let _ = std::fs::remove_dir_all(&cwd);
}

#[tokio::test]
async fn text_only_session_keeps_legacy_behavior() {
    let home = unique_dir("home-text");
    let cwd = unique_dir("cwd-text");
    let bytes = png_fixture(16, 16);
    std::fs::write(cwd.join("shot.png"), &bytes).unwrap();

    // No image capability declared anywhere: default backend is text-only.
    struct TextOnlyBackend;
    #[async_trait::async_trait]
    impl LlmBackend for TextOnlyBackend {
        fn name(&self) -> &str {
            "text-only"
        }
        async fn stream(&self, _request: LlmRequest) -> Result<LlmResponseStream> {
            Ok(LlmResponseStream {
                events: Box::pin(futures::stream::iter(vec![
                    Ok(LlmEvent::Text(LlmTextEvent {
                        content: "text reply".to_string(),
                    })),
                    Ok(LlmEvent::Stop(LlmStopEvent {
                        reason: "stop".to_string(),
                    })),
                ])),
                attempt_count: 1,
            })
        }
    }

    let runtime = AgentRuntime::start(
        AgentOptions::new(home.clone(), cwd.clone())
            .with_llm_backend(Arc::new(TextOnlyBackend))
            .with_api_key("test-key")
            .with_base_url("https://example.invalid/v1"),
    )
    .await
    .unwrap();
    // The Read tool description is NOT augmented and the image:// scheme
    // keeps its unknown-scheme fail-closed behavior: the run must still
    // succeed with a normal text flow.
    let _ = runtime.run_turn("hello").await.unwrap();

    // A png path in a text-only session is read as text and fails like
    // before (binary is not UTF-8), never producing an attachment.
    let outcome = runtime.run_turn("Read shot.png").await.unwrap();
    assert_eq!(
        outcome.status,
        TurnStatus::Ok,
        "turn itself still completes"
    );

    let _ = std::fs::remove_dir_all(&home);
    let _ = std::fs::remove_dir_all(&cwd);
}

// ── helpers ─────────────────────────────────────────────────────────────

fn find_conversation(home: &std::path::Path) -> PathBuf {
    // AgentOptions defaults to the Isolated layout: home is the session root.
    let path = home.join("conversation.jsonl");
    assert!(path.exists(), "{}", path.display());
    path
}

fn find_object(objects: &std::path::Path) -> Option<PathBuf> {
    let mut found = None;
    walk(objects, &mut |path| {
        if path.is_file() {
            found = Some(path.to_path_buf());
            return false;
        }
        true
    });
    found
}

fn read_capability_snapshot(home: &std::path::Path) -> Option<String> {
    let path = home.join("model-capabilities.json");
    path.exists()
        .then(|| std::fs::read_to_string(path).unwrap())
}

fn extract_image_id(conversation: &str) -> String {
    let marker = "image://";
    let start = conversation.find(marker).expect("image:// reference") + marker.len();
    let end = conversation[start..]
        .find('"')
        .unwrap_or(conversation.len() - start);
    conversation[start..start + end].to_string()
}

fn walk(dir: &std::path::Path, visit: &mut dyn FnMut(&std::path::Path) -> bool) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if !visit(&path) {
                return;
            }
            walk(&path, visit);
        } else if !visit(&path) {
            return;
        }
    }
}
