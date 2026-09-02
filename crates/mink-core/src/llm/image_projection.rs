//! Request-time image projection: resolve `tool_attachment` blocks into
//! OpenAI `image_url` data-URL parts (v7 §9.2/§9.5), plus single-consumption
//! lifecycle projection (§7.3).
//!
//! History references are plain text. Each reference is expanded into a data
//! URL exactly ONCE — on the first request after its capture (it sits after
//! the last assistant message). Once the model has seen it (a later
//! assistant message exists), the reference projects as a deterministic
//! text citation instead of being re-base64'd on every request; the model
//! can re-attach it with `Read image://...`. Failures follow the v7
//! contract: a fresh (unconsumed) attachment fails the current request; a
//! historical attachment degrades to a model-visible `[image unavailable]`
//! text block with per-id deduplicated warnings.

use anyhow::Result;
use base64::Engine as _;
use serde_json::Value;
use std::collections::HashSet;

pub(crate) const UNAVAILABLE_PREFIX: &str = "[image unavailable: ";

/// Deterministic text citation for an already-consumed attachment (the
/// model saw the image once; the pixel payload is not re-sent).
pub(crate) fn previous_image_text(block: &Value) -> String {
    let url = block
        .get("url")
        .and_then(Value::as_str)
        .unwrap_or("image://?");
    let width = block.get("width").and_then(Value::as_u64).unwrap_or(0);
    let height = block.get("height").and_then(Value::as_u64).unwrap_or(0);
    let format = block
        .get("format")
        .and_then(Value::as_str)
        .unwrap_or("image");
    format!(
        "[Previously attached image: {url} ({width}x{height} {format}). Use Read with this image reference if visual inspection is needed again.]"
    )
}

/// Single-consumption request projection (v7 §7.3): attachments after the
/// LAST assistant message have not been seen by the model yet — they stay as
/// `tool_attachment` and materialize on this request. Attachments before it
/// were already consumed (their pixel payload reached the model once) and
/// become a deterministic text citation; they are never re-expanded, never
/// budgeted as images, and never removed from history.
///
/// Order-based, so a crash between tool-result persistence and the next LLM
/// request still re-sends the capture (it remains after the last assistant
/// message). A compaction cut that drops the assistant message also counts
/// the surviving reference as unconsumed (safe direction: at most re-sent
/// once).
pub(crate) fn project_consumed_attachments(messages: &[Value]) -> Vec<Value> {
    if !messages
        .iter()
        .any(|message| message.get("role").and_then(Value::as_str) == Some("assistant"))
    {
        // No assistant message yet: every capture is unconsumed.
        return messages.to_vec();
    }
    let last_assistant = messages
        .iter()
        .rposition(|message| message.get("role").and_then(Value::as_str) == Some("assistant"))
        .expect("checked above");
    let mut projected = messages.to_vec();
    for message in projected.iter_mut().take(last_assistant) {
        let Some(blocks) = message.get_mut("content").and_then(Value::as_array_mut) else {
            continue;
        };
        for block in blocks.iter_mut() {
            if block.get("type").and_then(Value::as_str) != Some("tool_attachment") {
                continue;
            }
            let text = previous_image_text(block);
            *block = serde_json::json!({"type": "text", "text": text});
        }
    }
    projected
}

/// Materialize every `tool_attachment` block in the request messages.
///
/// - Success: block becomes `{"type":"image_url","image_url":{"url":"data:<mime>;base64,..."}}`
///   with the raw cached bytes, after digest verification (via the cache read)
///   and a header-metadata cross-check against the persisted block fields.
/// - Fresh failure (`image_id ∈ this_turn_ids`): `Err` — the Read that
///   claimed the image succeeded must not lie to the model.
/// - Historical failure: block degrades to a text placeholder; the warning is
///   emitted once per image id (deduplicated across the request).
/// - Capability disabled (defensive, unreachable by construction): text
///   placeholder `[image unavailable: capability disabled]`.
pub(crate) fn materialize_images_with(
    messages: &mut [Value],
    cache: &crate::session::image_cache::ImageCache,
    display: &dyn crate::ui::Display,
    limits: Option<&crate::capabilities::model_capabilities::OpenAiChatImageUrlLimits>,
    this_turn_ids: &HashSet<String>,
    warned: &mut HashSet<String>,
) -> Result<()> {
    let Some(limits) = limits else {
        // Defensive: capability-disabled sessions never produce
        // tool_attachment blocks, but an imported conversation could carry
        // them (v7 §8.4).
        for message in messages.iter_mut() {
            degrade_attachments(message, &mut |_| {
                Ok(Some("capability disabled".to_string()))
            })?;
        }
        return Ok(());
    };
    // Single-consumption: `project_request_messages` already turned every
    // CONSUMED reference into a text citation, so the remaining
    // `tool_attachment` blocks here are exactly the unconsumed batch for
    // this request. The defensive counters below enforce the per-request
    // budget on that batch; crossing it degrades historical attachments (or
    // fails the request for this-turn ones) instead of rewriting history.
    // Defensive re-check of the tool-layer quota on hand-imported or corrupt
    // conversations (review): count/byte limits are enforced before any
    // base64 allocation, and dimension/pixel limits inside materialize_one.
    let mut counters = MaterializeCounters::default();
    for message in messages.iter_mut() {
        degrade_attachments(message, &mut |block| {
            let url = match block.get("url").and_then(Value::as_str) {
                Some(url) => url.to_string(),
                None => return Ok(None),
            };
            let Some(id) = url.strip_prefix("image://") else {
                return Ok(None);
            };
            let id = id.to_string();
            if !crate::session::image_cache::validate_image_id(&id) {
                return Ok(Some(format!("invalid reference {url}")));
            }
            match materialize_one(&id, block, cache, limits, &mut counters) {
                Ok(()) => Ok(None),
                Err(error) => {
                    if this_turn_ids.contains(&id) {
                        // Fresh attachment: surface as a request failure.
                        return Err(anyhow::anyhow!(
                            "failed to materialize this-turn image {url}: {error}"
                        ));
                    }
                    if warned.insert(id.clone()) {
                        display.render_info(&format!(
                            "Image attachment unavailable: image://{id} ({error})"
                        ));
                    }
                    Ok(Some(format!("{id}: {error}")))
                }
            }
        })?;
    }
    Ok(())
}

/// Replace every persisted `tool_attachment` block with a deterministic text
/// marker. Summary requests never materialize or resend image pixels: cache-
/// aligned summaries may retain Agent tool schemas, but the payload of an
/// attachment is only ever a text citation (v7 §10.3).
pub(crate) fn degrade_images_for_summary(messages: &mut [Value]) {
    for message in messages.iter_mut() {
        let _ = degrade_attachments(message, &mut |block| {
            let url = block.get("url").and_then(Value::as_str).unwrap_or("?");
            let format = block.get("format").and_then(Value::as_str).unwrap_or("?");
            let width = block.get("width").and_then(Value::as_u64).unwrap_or(0);
            let height = block.get("height").and_then(Value::as_u64).unwrap_or(0);
            *block = serde_json::json!({
                "type": "text",
                "text": format!("[image {format} {width}x{height}: {url}]")
            });
            Ok(None)
        });
    }
}

/// Visit every message content array and rewrite `tool_attachment` blocks.
/// `rewrite` may materialize the block in place; it returns `Ok(None)` on
/// success, `Ok(Some(reason))` to degrade the block into a text placeholder,
/// or `Err` to fail the whole request.
fn degrade_attachments(
    message: &mut Value,
    rewrite: &mut dyn FnMut(&mut Value) -> Result<Option<String>>,
) -> Result<()> {
    let Some(blocks) = message.get_mut("content").and_then(Value::as_array_mut) else {
        return Ok(());
    };
    for block in blocks.iter_mut() {
        if block.get("type").and_then(Value::as_str) != Some("tool_attachment") {
            continue;
        }
        let reason = match rewrite(block)? {
            None => continue,
            Some(reason) => reason,
        };
        let text = format!("{UNAVAILABLE_PREFIX}image://{reason}]");
        *block = serde_json::json!({"type": "text", "text": text});
    }
    Ok(())
}

/// Running totals of attachments materialized so far in this request; used
/// to enforce the per-request limits defensively before any base64 work.
#[derive(Default)]
struct MaterializeCounters {
    images: usize,
    bytes: u64,
}

fn materialize_one(
    id: &str,
    block: &mut Value,
    cache: &crate::session::image_cache::ImageCache,
    limits: &crate::capabilities::model_capabilities::OpenAiChatImageUrlLimits,
    counters: &mut MaterializeCounters,
) -> Result<()> {
    // Persisted metadata is mandatory: every field must exist, have the
    // right type, and match the re-probed object exactly. A hand-imported or
    // corrupt block that omits fields fails closed instead of silently
    // sending an image with unknown budget facts (review fix #5).
    let declared_format = block
        .get("format")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("missing or invalid format field"))?;
    let declared_width = block
        .get("width")
        .and_then(Value::as_u64)
        .ok_or_else(|| anyhow::anyhow!("missing or invalid width field"))?;
    let declared_height = block
        .get("height")
        .and_then(Value::as_u64)
        .ok_or_else(|| anyhow::anyhow!("missing or invalid height field"))?;
    let declared_bytes = block
        .get("bytes")
        .and_then(Value::as_u64)
        .ok_or_else(|| anyhow::anyhow!("missing or invalid bytes field"))?;
    let bytes = cache
        .read_bounded(id, limits.max_image_bytes)?
        .ok_or_else(|| anyhow::anyhow!("object missing"))?;
    let info = crate::tools::image::probe(&bytes)
        .ok_or_else(|| anyhow::anyhow!("stored bytes are not a readable image"))?;
    // Cross-check persisted metadata against the stored object (v7 §8.1):
    // never send a picture that contradicts the conversation facts.
    let expected_format = match declared_format {
        "png" => crate::tools::image::ImageFormat::Png,
        "jpg" | "jpeg" => crate::tools::image::ImageFormat::Jpeg,
        "gif" => crate::tools::image::ImageFormat::Gif,
        "webp" => crate::tools::image::ImageFormat::Webp,
        other => anyhow::bail!("unknown declared format {other:?}"),
    };
    if expected_format != info.format {
        anyhow::bail!(
            "format mismatch: declared {declared_format:?}, stored {:?}",
            info.format
        );
    }
    if declared_width != u64::from(info.width) {
        anyhow::bail!(
            "width mismatch: declared {declared_width}, stored {}",
            info.width
        );
    }
    if declared_height != u64::from(info.height) {
        anyhow::bail!(
            "height mismatch: declared {declared_height}, stored {}",
            info.height
        );
    }
    if declared_bytes != bytes.len() as u64 {
        anyhow::bail!(
            "byte mismatch: declared {declared_bytes}, stored {}",
            bytes.len()
        );
    }
    if !limits.allowed_mime.contains(&info.format) {
        anyhow::bail!("mime {} not allowed by the current model", info.mime());
    }
    // Defensive per-request quota (review): a hand-imported conversation can
    // bypass the tool-layer reservation, so enforce count/byte limits before
    // any base64 allocation.
    if counters.images >= limits.max_images_per_request {
        anyhow::bail!(
            "exceeds max_images_per_request ({}); the first {} attachments already materialized",
            limits.max_images_per_request,
            counters.images
        );
    }
    if counters.bytes.saturating_add(bytes.len() as u64) > limits.max_image_bytes_per_request {
        anyhow::bail!(
            "exceeds max_image_bytes_per_request ({} bytes)",
            limits.max_image_bytes_per_request
        );
    }
    // Dimension / pixel limits are re-checked here too (review): the tool
    // layer enforced them at capture time, but imported blocks may carry
    // different facts.
    if info.width > limits.max_dimension || info.height > limits.max_dimension {
        anyhow::bail!(
            "image side {}x{} exceeds the {}px per-side limit",
            info.width,
            info.height,
            limits.max_dimension
        );
    }
    let pixels = crate::tools::image::checked_pixel_count(info.width, info.height)
        .ok_or_else(|| anyhow::anyhow!("image dimensions overflow"))?;
    if pixels > limits.max_pixels {
        anyhow::bail!(
            "image exceeds the {}px decoded-size limit",
            limits.max_pixels
        );
    }
    counters.images = counters.images.saturating_add(1);
    counters.bytes = counters.bytes.saturating_add(bytes.len() as u64);
    let encoded = base64::engine::general_purpose::STANDARD.encode(&bytes);
    let url = format!("data:{};base64,{encoded}", info.mime());
    let detail = match limits.detail {
        crate::capabilities::model_capabilities::ImageDetail::Low => "low",
        crate::capabilities::model_capabilities::ImageDetail::High => "high",
    };
    // Rebuild a fresh image_url block: never carry over unknown fields from
    // the persisted attachment.
    *block = serde_json::json!({
        "type": "image_url",
        "image_url": {"url": url, "detail": detail},
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capabilities::model_capabilities::OpenAiChatImageUrlLimits;
    use crate::session::image_cache::ImageCache;
    use serde_json::json;

    struct RecordingDisplay {
        infos: std::sync::Mutex<Vec<String>>,
    }

    impl crate::ui::Display for RecordingDisplay {
        fn render_thinking(&self, _content: &str) {}
        fn render_text(&self, _content: &str) {}
        fn render_tool_call(&self, _call: &crate::ui::ToolCallDisplay<'_>) {}
        fn render_tool_result(&self, _result: &crate::ui::PresentedToolResultDisplay<'_>) {}
        fn render_stop(&self, _reason: &str) {}
        fn render_signal(&self, _kind: &str, _severity: f64, _message: &str) {}
        fn render_error(&self, _message: &str) {}
        fn render_retry(&self) {}
        fn render_info(&self, msg: &str) {
            self.infos.lock().unwrap().push(msg.to_string());
        }
        fn render_title_update(&self, _model: &str, _stats: &crate::ui::StatsSnapshot) {}
        fn render_sub_agent_status(
            &self,
            _session_id: &str,
            _status: &str,
            _in_tokens: u64,
            _out_tokens: u64,
        ) {
        }
        fn render_sub_agent_output(
            &self,
            _session_id: &str,
            _status: &str,
            _thinking: &str,
            _text: &str,
            _in_tokens: u64,
            _out_tokens: u64,
        ) {
        }
        fn render_prompt(&self) {}
        fn render_clear_line(&self) {}
    }

    fn png_fixture(width: u32, height: u32) -> Vec<u8> {
        let img = image::RgbaImage::new(width, height);
        let mut out = Vec::new();
        img.write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png)
            .expect("png fixture");
        out
    }

    fn limits() -> OpenAiChatImageUrlLimits {
        OpenAiChatImageUrlLimits::default()
    }

    fn attachment_block(id: &str, bytes: &[u8]) -> Value {
        let info = crate::tools::image::probe(bytes).expect("fixture probe");
        json!({
            "type": "tool_attachment",
            "tool_use_id": "call_1",
            "url": format!("image://{id}"),
            "format": "png",
            "width": info.width,
            "height": info.height,
            "bytes": bytes.len(),
        })
    }

    #[test]
    fn materializes_data_url_from_cache() {
        let home = std::env::temp_dir().join(format!(
            "mink-proj-test-{}",
            std::thread::current().name().unwrap_or("t")
        ));
        let cache = ImageCache::new(&home);
        let bytes = png_fixture(32, 16);
        let id = cache.commit(&bytes).unwrap();
        let display = RecordingDisplay {
            infos: std::sync::Mutex::new(Vec::new()),
        };
        let mut messages = vec![json!({"role": "user", "content": [
            attachment_block(&id, &bytes),
        ]})];
        materialize_images_with(
            &mut messages,
            &cache,
            &display,
            Some(&limits()),
            &HashSet::new(),
            &mut HashSet::new(),
        )
        .unwrap();
        let block = &messages[0]["content"][0];
        assert_eq!(block["type"], "image_url");
        assert_eq!(block["image_url"]["detail"], "high");
        let url = block["image_url"]["url"].as_str().unwrap();
        assert!(url.starts_with("data:image/png;base64,"), "{url}");
        // The raw bytes are sent verbatim (phase one, no transform).
        let encoded = &url["data:image/png;base64,".len()..];
        assert_eq!(
            base64::engine::general_purpose::STANDARD
                .decode(encoded)
                .unwrap(),
            bytes
        );
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn historical_missing_object_degrades_with_warning_dedup() {
        let home = std::env::temp_dir().join(format!(
            "mink-proj-missing-{}",
            std::thread::current().name().unwrap_or("t")
        ));
        let cache = ImageCache::new(&home);
        let display = RecordingDisplay {
            infos: std::sync::Mutex::new(Vec::new()),
        };
        let id = "sha256:".to_string() + &"ab".repeat(32);
        let mut messages = vec![json!({"role": "user", "content": [
            json!({"type": "tool_attachment", "tool_use_id": "c", "url": format!("image://{id}"), "format": "png", "width": 1, "height": 1, "bytes": 1}),
            json!({"type": "tool_attachment", "tool_use_id": "c2", "url": format!("image://{id}"), "format": "png", "width": 1, "height": 1, "bytes": 1}),
        ]})];
        materialize_images_with(
            &mut messages,
            &cache,
            &display,
            Some(&limits()),
            &HashSet::new(),
            &mut HashSet::new(),
        )
        .unwrap();
        for block in messages[0]["content"].as_array().unwrap() {
            assert_eq!(block["type"], "text");
            assert!(
                block["text"]
                    .as_str()
                    .unwrap()
                    .contains("[image unavailable: image://")
            );
        }
        // Deduplicated: one warning per id, not per block.
        assert_eq!(display.infos.lock().unwrap().len(), 1);
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn fresh_attachment_failure_fails_the_request() {
        let home = std::env::temp_dir().join(format!(
            "mink-proj-fresh-{}",
            std::thread::current().name().unwrap_or("t")
        ));
        let cache = ImageCache::new(&home);
        let display = RecordingDisplay {
            infos: std::sync::Mutex::new(Vec::new()),
        };
        let id = "sha256:".to_string() + &"cd".repeat(32);
        let mut messages = vec![json!({"role": "user", "content": [
            json!({"type": "tool_attachment", "tool_use_id": "c", "url": format!("image://{id}"), "format": "png", "width": 1, "height": 1, "bytes": 1}),
        ]})];
        let err = materialize_images_with(
            &mut messages,
            &cache,
            &display,
            Some(&limits()),
            &HashSet::from([id.clone()]),
            &mut HashSet::new(),
        )
        .unwrap_err();
        assert!(err.to_string().contains("this-turn image"), "{err}");
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn metadata_mismatch_is_rejected() {
        let home = std::env::temp_dir().join(format!(
            "mink-proj-mismatch-{}",
            std::thread::current().name().unwrap_or("t")
        ));
        let cache = ImageCache::new(&home);
        let bytes = png_fixture(32, 16);
        let id = cache.commit(&bytes).unwrap();
        let display = RecordingDisplay {
            infos: std::sync::Mutex::new(Vec::new()),
        };
        let mut messages = vec![json!({"role": "user", "content": [
            json!({"type": "tool_attachment", "tool_use_id": "c", "url": format!("image://{id}"), "format": "png", "width": 999, "height": 1, "bytes": 1}),
        ]})];
        materialize_images_with(
            &mut messages,
            &cache,
            &display,
            Some(&limits()),
            &HashSet::new(),
            &mut HashSet::new(),
        )
        .unwrap();
        assert_eq!(messages[0]["content"][0]["type"], "text");
        assert!(
            messages[0]["content"][0]["text"]
                .as_str()
                .unwrap()
                .contains("width mismatch")
        );
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn capability_disabled_degrades_imported_attachments() {
        let home = std::env::temp_dir().join(format!(
            "mink-proj-disabled-{}",
            std::thread::current().name().unwrap_or("t")
        ));
        let cache = ImageCache::new(&home);
        let display = RecordingDisplay {
            infos: std::sync::Mutex::new(Vec::new()),
        };
        let mut messages = vec![json!({"role": "user", "content": [
            json!({"type": "tool_attachment", "tool_use_id": "c", "url": "image://sha256:aaaa", "format": "png", "width": 1, "height": 1, "bytes": 1}),
        ]})];
        materialize_images_with(
            &mut messages,
            &cache,
            &display,
            None,
            &HashSet::new(),
            &mut HashSet::new(),
        )
        .unwrap();
        assert!(
            messages[0]["content"][0]["text"]
                .as_str()
                .unwrap()
                .contains("capability disabled")
        );
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn wire_body_contains_materialized_image_part() {
        use crate::llm::transport::build_openai_body;
        let home = std::env::temp_dir().join(format!(
            "mink-proj-wire-{}",
            std::thread::current().name().unwrap_or("t")
        ));
        let cache = ImageCache::new(&home);
        let bytes = png_fixture(32, 16);
        let id = cache.commit(&bytes).unwrap();
        let display = RecordingDisplay {
            infos: std::sync::Mutex::new(Vec::new()),
        };
        let mut messages = vec![json!({"role": "user", "content": [
            json!({"type": "tool_result", "tool_use_id": "call_1", "content": "Image: 32x16 image/png ..."}),
            json!({"type": "text", "text": "Image for call_1: shot.png"}),
            attachment_block(&id, &bytes),
        ]})];
        materialize_images_with(
            &mut messages,
            &cache,
            &display,
            Some(&limits()),
            &HashSet::new(),
            &mut HashSet::new(),
        )
        .unwrap();
        let body = build_openai_body("vision-model", &messages, &[], "", 100).unwrap();
        let text = String::from_utf8_lossy(&body);
        assert!(text.contains("data:image/png;base64,"), "{text}");
        // Tool message precedes the residual user message carrying the image.
        let tool_pos = text.find("\"role\":\"tool\"").unwrap();
        let image_pos = text.find("image_url").unwrap();
        assert!(
            tool_pos < image_pos,
            "tool message must precede the image part"
        );
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn consumed_attachments_become_text_references() {
        let messages = vec![
            json!({"role": "user", "content": [
                json!({"type": "tool_attachment", "tool_use_id": "a", "url": "image://sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", "format": "png", "width": 1024, "height": 768, "bytes": 100}),
            ]}),
            json!({"role": "assistant", "content": [json!({"type": "text", "text": "I see a chart."})]}),
            json!({"role": "user", "content": [
                json!({"type": "tool_attachment", "tool_use_id": "b", "url": "image://sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb", "format": "png", "width": 64, "height": 32, "bytes": 200}),
            ]}),
        ];
        let projected = crate::llm::image_projection::project_consumed_attachments(&messages);
        // Oldest reference (before the last assistant message) is consumed.
        let first = &projected[0]["content"][0];
        assert_eq!(first["type"], "text");
        let text = first["text"].as_str().unwrap();
        assert!(
            text.contains("[Previously attached image: image://sha256:aaaaaaaa"),
            "{text}"
        );
        assert!(text.contains("1024x768 png"), "{text}");
        assert!(text.contains("Read with this image reference"), "{text}");
        // The reference after the last assistant message is unconsumed.
        assert_eq!(projected[2]["content"][0]["type"], "tool_attachment");
    }

    #[test]
    fn no_assistant_message_keeps_all_attachments() {
        let messages = vec![json!({"role": "user", "content": [
            json!({"type": "tool_attachment", "tool_use_id": "a", "url": "image://sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", "format": "png", "width": 1, "height": 1, "bytes": 1}),
        ]})];
        let projected = crate::llm::image_projection::project_consumed_attachments(&messages);
        assert_eq!(projected[0]["content"][0]["type"], "tool_attachment");
    }

    #[test]
    fn consumed_attachments_are_never_materialized() {
        // A consumed reference uses an id that does NOT exist in the cache:
        // projection turns it into a text citation, so materialization never
        // touches it (no unavailable degrade, no error). The unconsumed
        // reference still materializes.
        let home = std::env::temp_dir().join(format!(
            "mink-proj-consume-{}",
            std::thread::current().name().unwrap_or("t")
        ));
        let cache = ImageCache::new(&home);
        let (fresh_id, fresh_bytes) = {
            let b = png_fixture(16, 16);
            (cache.commit(&b).unwrap(), b)
        };
        let missing = "sha256:".to_string() + &"aa".repeat(32);
        let messages = vec![
            json!({"role": "user", "content": [
                json!({"type": "tool_attachment", "tool_use_id": "c", "url": format!("image://{missing}"), "format": "png", "width": 1, "height": 1, "bytes": 1}),
            ]}),
            json!({"role": "assistant", "content": [json!({"type": "text", "text": "seen"})]}),
            json!({"role": "user", "content": [attachment_block(&fresh_id, &fresh_bytes)]}),
        ];
        let mut projected = crate::llm::image_projection::project_consumed_attachments(&messages);
        let display = RecordingDisplay {
            infos: std::sync::Mutex::new(Vec::new()),
        };
        materialize_images_with(
            &mut projected,
            &cache,
            &display,
            Some(&limits()),
            &HashSet::new(),
            &mut HashSet::new(),
        )
        .unwrap();
        assert_eq!(projected[0]["content"][0]["type"], "text");
        assert!(
            projected[0]["content"][0]["text"]
                .as_str()
                .unwrap()
                .contains("[Previously attached image"),
            "{}",
            projected[0]
        );
        assert_eq!(projected[2]["content"][0]["type"], "image_url");
        let _ = std::fs::remove_dir_all(&home);
    }
}

#[cfg(test)]
mod strict_metadata_tests {
    use super::*;
    use crate::capabilities::model_capabilities::OpenAiChatImageUrlLimits;
    use crate::session::image_cache::ImageCache;
    use serde_json::json;

    struct SilentDisplay;
    impl crate::ui::Display for SilentDisplay {
        fn render_thinking(&self, _c: &str) {}
        fn render_text(&self, _c: &str) {}
        fn render_tool_call(&self, _c: &crate::ui::ToolCallDisplay<'_>) {}
        fn render_tool_result(&self, _r: &crate::ui::PresentedToolResultDisplay<'_>) {}
        fn render_stop(&self, _r: &str) {}
        fn render_signal(&self, _k: &str, _s: f64, _m: &str) {}
        fn render_error(&self, _m: &str) {}
        fn render_retry(&self) {}
        fn render_info(&self, _m: &str) {}
        fn render_title_update(&self, _m: &str, _s: &crate::ui::StatsSnapshot) {}
        fn render_sub_agent_status(&self, _s: &str, _st: &str, _i: u64, _o: u64) {}
        fn render_sub_agent_output(
            &self,
            _s: &str,
            _st: &str,
            _t: &str,
            _x: &str,
            _i: u64,
            _o: u64,
        ) {
        }
        fn render_prompt(&self) {}
        fn render_clear_line(&self) {}
    }

    fn png_fixture() -> Vec<u8> {
        let img = image::RgbaImage::new(16, 16);
        let mut out = Vec::new();
        img.write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png)
            .expect("png");
        out
    }

    #[test]
    fn missing_metadata_fields_fail_closed_and_never_send() {
        let home = std::env::temp_dir().join(format!(
            "mink-strict-{}",
            std::thread::current().name().unwrap_or("t")
        ));
        let cache = ImageCache::new(&home);
        let bytes = png_fixture();
        let id = cache.commit(&bytes).unwrap();
        // Hand-imported block that omits `bytes` (review fix #5).
        let mut messages = vec![json!({"role": "user", "content": [
            json!({"type": "tool_attachment", "tool_use_id": "c", "url": format!("image://{id}"), "format": "png", "width": 16, "height": 16}),
        ]})];
        materialize_images_with(
            &mut messages,
            &cache,
            &SilentDisplay,
            Some(&OpenAiChatImageUrlLimits::default()),
            &HashSet::new(),
            &mut HashSet::new(),
        )
        .unwrap();
        // Historical attachment degrades to a text placeholder; it must NOT
        // become an image_url with an unknown byte count.
        assert_eq!(messages[0]["content"][0]["type"], "text");
        assert!(
            messages[0]["content"][0]["text"]
                .as_str()
                .unwrap()
                .contains("missing or invalid bytes"),
            "{}",
            messages[0]["content"][0]
        );
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn unknown_fields_are_not_carried_into_image_url_block() {
        let home = std::env::temp_dir().join(format!(
            "mink-strict2-{}",
            std::thread::current().name().unwrap_or("t")
        ));
        let cache = ImageCache::new(&home);
        let bytes = png_fixture();
        let id = cache.commit(&bytes).unwrap();
        let mut messages = vec![json!({"role": "user", "content": [
            json!({"type": "tool_attachment", "tool_use_id": "c", "url": format!("image://{id}"), "format": "png", "width": 16, "height": 16, "bytes": bytes.len(), "sneaky": "field"}),
        ]})];
        materialize_images_with(
            &mut messages,
            &cache,
            &SilentDisplay,
            Some(&OpenAiChatImageUrlLimits::default()),
            &HashSet::new(),
            &mut HashSet::new(),
        )
        .unwrap();
        let block = &messages[0]["content"][0];
        assert_eq!(block["type"], "image_url");
        assert!(
            block.get("sneaky").is_none(),
            "unknown fields must be dropped"
        );
        assert!(block.get("tool_use_id").is_none());
        assert!(block["image_url"].get("detail").is_some());
        let _ = std::fs::remove_dir_all(&home);
    }
}

#[cfg(test)]
mod defensive_quota_tests {
    use super::*;
    use crate::capabilities::model_capabilities::OpenAiChatImageUrlLimits;
    use crate::session::image_cache::ImageCache;
    use serde_json::json;

    struct QuietDisplay;
    impl crate::ui::Display for QuietDisplay {
        fn render_thinking(&self, _c: &str) {}
        fn render_text(&self, _c: &str) {}
        fn render_tool_call(&self, _c: &crate::ui::ToolCallDisplay<'_>) {}
        fn render_tool_result(&self, _r: &crate::ui::PresentedToolResultDisplay<'_>) {}
        fn render_stop(&self, _r: &str) {}
        fn render_signal(&self, _k: &str, _s: f64, _m: &str) {}
        fn render_error(&self, _m: &str) {}
        fn render_retry(&self) {}
        fn render_info(&self, _m: &str) {}
        fn render_title_update(&self, _m: &str, _s: &crate::ui::StatsSnapshot) {}
        fn render_sub_agent_status(&self, _s: &str, _st: &str, _i: u64, _o: u64) {}
        fn render_sub_agent_output(
            &self,
            _s: &str,
            _st: &str,
            _t: &str,
            _x: &str,
            _i: u64,
            _o: u64,
        ) {
        }
        fn render_prompt(&self) {}
        fn render_clear_line(&self) {}
    }

    fn block(id: &str, bytes: &[u8]) -> serde_json::Value {
        let info = crate::tools::image::probe(bytes).expect("probe");
        json!({"type": "tool_attachment", "tool_use_id": "c", "url": format!("image://{id}"), "format": "png", "width": info.width, "height": info.height, "bytes": bytes.len()})
    }

    #[test]
    fn per_request_image_count_is_enforced_defensively() {
        let home = std::env::temp_dir().join(format!(
            "mink-def-quota-{}",
            std::thread::current().name().unwrap_or("t")
        ));
        let cache = ImageCache::new(&home);
        let bytes = {
            let img = image::RgbaImage::new(8, 8);
            let mut out = Vec::new();
            img.write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png)
                .unwrap();
            out
        };
        let limits = OpenAiChatImageUrlLimits {
            max_images_per_request: 2,
            ..Default::default()
        };
        let ids: Vec<String> = (0..3).map(|_| cache.commit(&bytes).unwrap()).collect();
        let mut messages = vec![json!({"role": "user", "content": [
            block(&ids[0], &bytes),
            block(&ids[1], &bytes),
            block(&ids[2], &bytes),
        ]})];
        materialize_images_with(
            &mut messages,
            &cache,
            &QuietDisplay,
            Some(&limits),
            &HashSet::new(),
            &mut HashSet::new(),
        )
        .unwrap();
        let blocks = messages[0]["content"].as_array().unwrap();
        let image_blocks = blocks.iter().filter(|b| b["type"] == "image_url").count();
        assert_eq!(image_blocks, 2, "third attachment must degrade, not send");
        assert!(
            blocks.iter().any(|b| b["type"] == "text"
                && b["text"]
                    .as_str()
                    .unwrap()
                    .contains("max_images_per_request")),
            "third block must carry the defensive limit reason"
        );
        let _ = std::fs::remove_dir_all(&home);
    }
}
