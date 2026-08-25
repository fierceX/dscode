//! Session-scoped model capabilities: resolved once at session init, frozen,
//! persisted to `model-capabilities.json`, and used to gate model switching,
//! startup validation, and prefix construction (v7 design §3).

use crate::config::{ResolvedConfig, model_resolver};
use crate::llm::client::LlmBackend;
use crate::tools::image::ImageFormat;
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::Path;
use std::path::PathBuf;

pub const MODEL_CAPABILITIES_VERSION: u32 = 1;
pub const MODEL_CAPABILITIES_FILE: &str = "model-capabilities.json";

// v7 §3.1 defaults (internal constants, not config-exposed).
pub const DEFAULT_MAX_IMAGES_PER_REQUEST: usize = 4;
pub const DEFAULT_MAX_IMAGE_BYTES_PER_REQUEST: u64 = 16 * 1024 * 1024;
pub const DEFAULT_MAX_IMAGE_BYTES: u64 = 16 * 1024 * 1024;
pub const DEFAULT_MAX_DIMENSION: u32 = 16_384;
pub const DEFAULT_MAX_PIXELS: u64 = 16_000_000;
/// Hard cap for the final HTTP request body (v7 §9.4).
pub const MAX_REQUEST_BODY_BYTES: u64 = 32 * 1024 * 1024;

/// OpenAI `image_url` token estimation (review fix): 85 base tokens per
/// image plus 170 tokens per 512px tile under the standard sequential
/// detail scaling — first cap the long edge at 2000, then cap the short
/// edge at 768 (each stage re-proportions the other axis). No artificial
/// tile cap: preflight must never underestimate, so the estimator is
/// conservative by construction.
pub fn estimate_image_tokens(width: u32, height: u32, detail: ImageDetail) -> u64 {
    let (long, short) = if width >= height {
        (width, height)
    } else {
        (height, width)
    };
    // Stage 1: long edge <= 2000.
    let (long, short) = if long > 2000 {
        let ratio = 2000.0 / f64::from(long);
        (
            2000,
            (f64::from(short) * ratio).max(1.0) as u32,
        )
    } else {
        (long, short)
    };
    // Stage 2: short edge <= 768 (after stage 1 re-proportioning).
    let (long, short) = if short > 768 {
        let ratio = 768.0 / f64::from(short);
        (
            (f64::from(long) * ratio) as u32,
            768,
        )
    } else {
        (long, short)
    };
    let tiles = (u64::from(long).saturating_add(511) / 512)
        .saturating_mul(u64::from(short).saturating_add(511) / 512);
    match detail {
        ImageDetail::Low => 85,
        ImageDetail::High => 85 + 170 * tiles,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ImageDetail {
    Low,
    High,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WireProtocol {
    OpenAiChatImageUrl,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TokenEstimator {
    OpenAiTile,
}

/// Limits for the OpenAI chat/completions `image_url` input protocol.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpenAiChatImageUrlLimits {
    pub detail: ImageDetail,
    pub wire_protocol: WireProtocol,
    pub token_estimator: TokenEstimator,
    /// Model-allowed output MIME set (double-layer MIME check, §4.3).
    pub allowed_mime: Vec<ImageFormat>,
    pub max_images_per_request: usize,
    pub max_image_bytes_per_request: u64,
    pub max_image_bytes: u64,
    pub max_dimension: u32,
    pub max_pixels: u64,
}

impl OpenAiChatImageUrlLimits {
    /// Canonicalize `allowed_mime` (sort + dedup, reject empty) so the same
    /// MIME set always yields the same fingerprint regardless of source order
    /// (review fix). Callers must invoke this before fingerprinting.
    pub fn normalize(&mut self) -> Result<()> {
        self.allowed_mime.sort();
        self.allowed_mime.dedup();
        if self.allowed_mime.is_empty() {
            anyhow::bail!("image capability requires a non-empty allowed_mime set");
        }
        Ok(())
    }
}

impl Default for OpenAiChatImageUrlLimits {
    fn default() -> Self {
        Self {
            detail: ImageDetail::High,
            wire_protocol: WireProtocol::OpenAiChatImageUrl,
            token_estimator: TokenEstimator::OpenAiTile,
            allowed_mime: ImageFormat::ALL.to_vec(),
            max_images_per_request: DEFAULT_MAX_IMAGES_PER_REQUEST,
            max_image_bytes_per_request: DEFAULT_MAX_IMAGE_BYTES_PER_REQUEST,
            max_image_bytes: DEFAULT_MAX_IMAGE_BYTES,
            max_dimension: DEFAULT_MAX_DIMENSION,
            max_pixels: DEFAULT_MAX_PIXELS,
        }
    }
}

/// Image input capability of one model route.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ImageInputCapability {
    Unsupported,
    OpenAiChatImageUrl(OpenAiChatImageUrlLimits),
}

impl ImageInputCapability {
    pub fn supported(&self) -> bool {
        !matches!(self, ImageInputCapability::Unsupported)
    }

    pub fn limits(&self) -> Option<&OpenAiChatImageUrlLimits> {
        match self {
            ImageInputCapability::OpenAiChatImageUrl(limits) => Some(limits),
            ImageInputCapability::Unsupported => None,
        }
    }

    /// Capability fingerprint over every compatibility-relevant field.
    /// Deliberately excludes the model name so equally capable models can
    /// switch freely (v7 §3.1).
    pub fn fingerprint(&self) -> String {
        let canonical = serde_json::to_vec(self).unwrap_or_else(|_| b"{}".to_vec());
        let mut hasher = Sha256::new();
        hasher.update(b"mink-image-input-capability-v1\0");
        hasher.update(&canonical);
        crate::capabilities::fingerprint::hex_lower(hasher.finalize())
    }
}

/// Frozen per-session capability snapshot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionModelCapabilities {
    pub version: u32,
    /// Model that created the session (informational; never part of the fingerprint).
    pub initial_model: String,
    pub image_input: ImageInputCapability,
    pub capability_fingerprint: String,
}

impl SessionModelCapabilities {
    /// Text-only snapshot used by tests and legacy fallback.
    pub fn unsupported(initial_model: impl Into<String>) -> Self {
        let image_input = ImageInputCapability::Unsupported;
        let mut caps = Self {
            version: MODEL_CAPABILITIES_VERSION,
            initial_model: initial_model.into(),
            image_input,
            capability_fingerprint: String::new(),
        };
        caps.capability_fingerprint = caps.image_input.fingerprint();
        caps
    }

    /// Resolve capabilities for one model through the single entry point used
    /// by session init, recovery checks, and `/model` switching.
    pub fn resolve(model: &str, config: &ResolvedConfig, backend: &dyn LlmBackend) -> Self {
        let resolved = model_resolver(config).resolve(model);
        let mut image_input = config
            .image_input
            .clone()
            .unwrap_or_else(|| backend.image_input_capability(&resolved.actual));
        // Canonicalize the MIME set before fingerprinting so order/duplicates
        // never change the session fingerprint; an empty set is a
        // configuration error that fails closed to text-only (review fix).
        if let ImageInputCapability::OpenAiChatImageUrl(limits) = &mut image_input
            && limits.normalize().is_err()
        {
            image_input = ImageInputCapability::Unsupported;
        }
        let mut caps = Self {
            version: MODEL_CAPABILITIES_VERSION,
            initial_model: resolved.actual.clone(),
            image_input,
            capability_fingerprint: String::new(),
        };
        caps.capability_fingerprint = caps.image_input.fingerprint();
        caps
    }

    /// Compatibility predicate (v7 §3.3): an Unsupported session accepts any
    /// model but stays text-only; an image-capable session requires an exact
    /// fingerprint match in the MVP.
    pub fn is_compatible_with(&self, candidate: &Self) -> bool {
        match self.image_input {
            ImageInputCapability::Unsupported => true,
            _ => self.capability_fingerprint == candidate.capability_fingerprint,
        }
    }

    /// Recompute the fingerprint and verify the persisted field matches.
    /// Recovery never trusts the persisted fingerprint blindly (v7 §3.2).
    pub fn verify_fingerprint(&self) -> bool {
        self.image_input.fingerprint() == self.capability_fingerprint
    }
}

pub fn capabilities_path(session_dir: &Path) -> PathBuf {
    session_dir.join(MODEL_CAPABILITIES_FILE)
}

/// Load a persisted snapshot. `None` when the file does not exist.
pub fn load_capabilities(path: &Path) -> Result<Option<SessionModelCapabilities>> {
    let raw = match std::fs::read_to_string(path) {
        Ok(raw) => raw,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let caps: SessionModelCapabilities = serde_json::from_str(&raw).with_context(|| {
        format!(
            "invalid model capabilities snapshot at {}",
            path.display()
        )
    })?;
    if !caps.verify_fingerprint() {
        bail!(
            "model capabilities snapshot fingerprint mismatch at {}",
            path.display()
        );
    }
    Ok(Some(caps))
}

/// Atomically persist a snapshot (same-directory temp file + rename).
pub fn save_capabilities(path: &Path, caps: &SessionModelCapabilities) -> Result<()> {
    let raw = serde_json::to_string_pretty(caps)?;
    crate::session::atomic_file::atomic_replace(path, raw.as_bytes())
}

/// Crash-boundary classification for a session without a snapshot (v7 §3.2):
/// a directory with an empty conversation and no `prefix_snapshot` event is
/// an interrupted initialization and may be re-resolved; anything else is a
/// legacy session and freezes to `Unsupported`.
pub enum SnapshotAbsence {
    /// Interrupted init: resolve and persist fresh capabilities.
    Uninitialized,
    /// Legacy session: freeze Unsupported.
    Legacy,
}

pub fn classify_snapshot_absence(
    conversation_path: &Path,
    events_path: &Path,
) -> Result<SnapshotAbsence> {
    let conversation_has_messages = has_nonempty_jsonl(conversation_path)?;
    if conversation_has_messages {
        return Ok(SnapshotAbsence::Legacy);
    }
    let events_has_prefix = match std::fs::read_to_string(events_path) {
        Ok(raw) => raw.contains("prefix_snapshot"),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
        Err(error) => return Err(error.into()),
    };
    Ok(if events_has_prefix {
        SnapshotAbsence::Legacy
    } else {
        SnapshotAbsence::Uninitialized
    })
}

fn has_nonempty_jsonl(path: &Path) -> Result<bool> {
    match std::fs::read_to_string(path) {
        Ok(raw) => Ok(raw
            .lines()
            .any(|line| !line.trim().is_empty())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base_config() -> ResolvedConfig {
        let mut config = ResolvedConfig::default();
        config.model = "vision-model".to_string();
        config
    }

    struct TextBackend;
    #[async_trait::async_trait]
    impl LlmBackend for TextBackend {
        fn name(&self) -> &str {
            "text-backend"
        }
        async fn stream(
            &self,
            _request: crate::llm::client::LlmRequest,
        ) -> anyhow::Result<crate::llm::client::LlmResponseStream> {
            unreachable!()
        }
    }

    #[test]
    fn backend_default_is_unsupported() {
        let caps = SessionModelCapabilities::resolve("any-model", &base_config(), &TextBackend);
        assert_eq!(caps.image_input, ImageInputCapability::Unsupported);
        assert!(!caps.image_input.supported());
    }

    #[test]
    fn explicit_config_wins_over_backend() {
        let mut config = base_config();
        config.image_input = Some(ImageInputCapability::OpenAiChatImageUrl(
            OpenAiChatImageUrlLimits::default(),
        ));
        let caps = SessionModelCapabilities::resolve("any-model", &config, &TextBackend);
        assert!(caps.image_input.supported());
    }

    #[test]
    fn fingerprint_excludes_model_name() {
        let mut config_a = base_config();
        config_a.model = "model-a".to_string();
        config_a.image_input = Some(ImageInputCapability::OpenAiChatImageUrl(
            OpenAiChatImageUrlLimits::default(),
        ));
        let mut config_b = base_config();
        config_b.model = "model-b".to_string();
        config_b.image_input = Some(ImageInputCapability::OpenAiChatImageUrl(
            OpenAiChatImageUrlLimits::default(),
        ));
        let a = SessionModelCapabilities::resolve("model-a", &config_a, &TextBackend);
        let b = SessionModelCapabilities::resolve("model-b", &config_b, &TextBackend);
        assert_eq!(a.capability_fingerprint, b.capability_fingerprint);
        assert!(a.is_compatible_with(&b));
    }

    #[test]
    fn different_limits_change_fingerprint_and_compatibility() {
        let mut config_a = base_config();
        config_a.image_input = Some(ImageInputCapability::OpenAiChatImageUrl(
            OpenAiChatImageUrlLimits::default(),
        ));
        let mut config_b = base_config();
        let mut limits = OpenAiChatImageUrlLimits::default();
        limits.max_images_per_request = 1;
        config_b.image_input = Some(ImageInputCapability::OpenAiChatImageUrl(limits));
        let a = SessionModelCapabilities::resolve("m", &config_a, &TextBackend);
        let b = SessionModelCapabilities::resolve("m", &config_b, &TextBackend);
        assert_ne!(a.capability_fingerprint, b.capability_fingerprint);
        assert!(!a.is_compatible_with(&b));
    }

    #[test]
    fn unsupported_session_accepts_any_model() {
        let legacy = SessionModelCapabilities::unsupported("text-model");
        let vision = SessionModelCapabilities::resolve(
            "vision-model",
            &{
                let mut config = base_config();
                config.image_input = Some(ImageInputCapability::OpenAiChatImageUrl(
                    OpenAiChatImageUrlLimits::default(),
                ));
                config
            },
            &TextBackend,
        );
        assert!(legacy.is_compatible_with(&vision));
    }

    #[test]
    fn snapshot_roundtrip_and_fingerprint_verification() {
        let dir = std::env::temp_dir().join(format!("mink-caps-test-{}", std::process::id()));
        let path = dir.join(MODEL_CAPABILITIES_FILE);
        let caps = SessionModelCapabilities::resolve(
            "vision-model",
            &{
                let mut config = base_config();
                config.image_input = Some(ImageInputCapability::OpenAiChatImageUrl(
                    OpenAiChatImageUrlLimits::default(),
                ));
                config
            },
            &TextBackend,
        );
        save_capabilities(&path, &caps).unwrap();
        let loaded = load_capabilities(&path).unwrap().expect("snapshot");
        assert_eq!(loaded, caps);
        assert!(loaded.verify_fingerprint());
        // Tamper with the fingerprint: recovery must fail closed.
        let mut tampered = caps.clone();
        tampered.capability_fingerprint = "deadbeef".to_string();
        save_capabilities(&path, &tampered).unwrap();
        assert!(load_capabilities(&path).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn absence_classification() {
        let dir = std::env::temp_dir().join(format!("mink-absence-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let conversation = dir.join("conversation.jsonl");
        let events = dir.join("events.jsonl");

        // Empty conversation, no events: uninitialized.
        std::fs::write(&conversation, "").unwrap();
        assert!(matches!(
            classify_snapshot_absence(&conversation, &events).unwrap(),
            SnapshotAbsence::Uninitialized
        ));

        // Non-empty conversation: legacy.
        std::fs::write(&conversation, "{\"role\":\"user\",\"content\":\"hi\"}\n").unwrap();
        assert!(matches!(
            classify_snapshot_absence(&conversation, &events).unwrap(),
            SnapshotAbsence::Legacy
        ));

        // Empty conversation but a prefix_snapshot event: legacy.
        std::fs::write(&conversation, "").unwrap();
        std::fs::write(&events, "{\"type\":\"prefix_snapshot\",\"fingerprint\":\"x\"}\n").unwrap();
        assert!(matches!(
            classify_snapshot_absence(&conversation, &events).unwrap(),
            SnapshotAbsence::Legacy
        ));

        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[cfg(test)]
mod backend_declaration_tests {
    use super::*;
    use crate::config::ResolvedConfig;

    #[test]
    fn default_backend_declares_vision_for_builtin_vision_model() {
        let config = ResolvedConfig::default();
        let backend = crate::llm::client::OpenAiCompatibleBackend::from_config(&config);
        assert!(
            backend
                .image_input_capability("deepseek-v4-flash-vision-exp")
                .limits()
                .is_some(),
            "built-in vision model must resolve image capability by default"
        );
        assert!(
            backend.image_input_capability("deepseek-v4-pro").limits().is_none(),
            "non-vision models stay text-only"
        );
    }
}

#[cfg(test)]
mod token_estimation_tests {
    use super::*;

    #[test]
    fn tile_estimation_matches_openai_semantics() {
        // 1024x768 high: 2x2 tiles -> 85 + 170*4 = 765.
        assert_eq!(
            estimate_image_tokens(1024, 768, ImageDetail::High),
            765
        );
        // low detail is a flat 85 regardless of size.
        assert_eq!(estimate_image_tokens(4000, 3000, ImageDetail::Low), 85);
        // Sequential two-stage scaling (review fix): 4000x3000 -> stage 1
        // long edge 2000 -> 2000x1500 -> stage 2 short edge 768 -> 1024x768
        // -> 2x2 tiles = 765.
        assert_eq!(
            estimate_image_tokens(4000, 3000, ImageDetail::High),
            765
        );
        // Wide image without the 4-tile cap (review fix): 2000x1000 -> stage 2
        // short edge 768 -> 1536x768 -> 3x2 tiles = 6 -> 85 + 170*6 = 1105.
        assert_eq!(estimate_image_tokens(2000, 1000, ImageDetail::High), 1105);
        // Small image: 1 tile -> 85 + 170 = 255.
        assert_eq!(estimate_image_tokens(512, 512, ImageDetail::High), 255);
    }
}
