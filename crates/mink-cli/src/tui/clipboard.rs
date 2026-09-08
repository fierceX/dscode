//! Clipboard image capture for the TUI paste flow.
//!
//! Terminals never deliver image bytes through bracketed paste, so the TUI
//! reads the system clipboard itself. macOS extracts the pasteboard's PNG
//! representation through `osascript`; other platforms fail closed with a
//! clear message. Extracted bytes are validated against the frozen session
//! image limits before they are ever staged on disk.

use anyhow::{Context, Result, bail};
use std::path::Path;
use std::process::Command;

use mink::runtime::OpenAiChatImageUrlLimits;

/// A validated clipboard image, ready to be staged as an attachment.
#[derive(Debug)]
pub(crate) struct ClipboardPng {
    pub bytes: Vec<u8>,
    pub width: u32,
    pub height: u32,
}

/// Command seam so tests can drive the macOS extraction without a real
/// clipboard.
pub(crate) trait CommandRunner: Send + Sync {
    fn run(&self, program: &str, args: &[String]) -> Result<Vec<u8>>;
}

pub(crate) struct SystemRunner;

impl CommandRunner for SystemRunner {
    fn run(&self, program: &str, args: &[String]) -> Result<Vec<u8>> {
        let output = Command::new(program)
            .args(args)
            .output()
            .with_context(|| format!("failed to run {program}"))?;
        if !output.status.success() {
            bail!(
                "{program} failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        Ok(output.stdout)
    }
}

/// Read the clipboard as PNG and validate it against `limits`.
pub(crate) fn read_clipboard_png(
    staging_dir: &Path,
    limits: &OpenAiChatImageUrlLimits,
) -> Result<ClipboardPng> {
    read_clipboard_png_with(&SystemRunner, staging_dir, limits)
}

pub(crate) fn read_clipboard_png_with(
    runner: &dyn CommandRunner,
    staging_dir: &Path,
    limits: &OpenAiChatImageUrlLimits,
) -> Result<ClipboardPng> {
    #[cfg(target_os = "macos")]
    {
        let bytes = extract_macos_png(runner, staging_dir, limits.max_image_bytes)?;
        validate_png(&bytes, limits)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (runner, staging_dir, limits);
        bail!("clipboard image paste is not supported on this platform yet (macOS only)")
    }
}

/// macOS: write the pasteboard's `PNGf` representation to a staging file and
/// read it back. The staging file is always removed; the caller publishes the
/// validated bytes through the content-addressed attachment store. The read is
/// bounded by `max_bytes` so an oversized clipboard image fails the size check
/// instead of allocating unbounded memory.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn extract_macos_png(
    runner: &dyn CommandRunner,
    staging_dir: &Path,
    max_bytes: u64,
) -> Result<Vec<u8>> {
    let info = runner.run(
        "osascript",
        &["-e".to_string(), "clipboard info".to_string()],
    )?;
    let info = String::from_utf8_lossy(&info).trim().to_string();
    if !info.contains("PNGf") {
        bail!(
            "clipboard has no PNG image (types: {})",
            if info.is_empty() { "<none>" } else { &info }
        );
    }
    std::fs::create_dir_all(staging_dir).with_context(|| {
        format!(
            "cannot create clipboard staging directory {}",
            staging_dir.display()
        )
    })?;
    // The staging PNG is written by osascript with the process umask; restrict
    // the directory before any bytes land in it.
    crate::tui::attachments::restrict_private_dir(staging_dir)?;
    let staging = staging_dir.join(format!(
        ".clipboard-{}-{}.png",
        std::process::id(),
        staging_tail()
    ));
    let script = format!(
        "set theFile to (open for access POSIX file \"{}\" with write permission)\n\
         write (the clipboard as «class PNGf») to theFile\n\
         close access theFile",
        escape_applescript_string(&staging.to_string_lossy())
    );
    let written = runner.run("osascript", &["-e".to_string(), script]);
    let bytes = read_bounded(&staging, max_bytes);
    let _ = std::fs::remove_file(&staging);
    written.context("failed to extract the clipboard image")?;
    bytes.context("osascript did not write a clipboard image")
}

/// Read at most `max_bytes + 1` bytes: enough for `validate_png` to report the
/// real size error without buffering an arbitrarily large clipboard image.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn read_bounded(path: &Path, max_bytes: u64) -> std::io::Result<Vec<u8>> {
    use std::io::Read as _;
    let mut file = std::fs::File::open(path)?;
    let mut bytes = Vec::with_capacity((max_bytes as usize).min(64 * 1024));
    file.by_ref()
        .take(max_bytes.saturating_add(1))
        .read_to_end(&mut bytes)?;
    Ok(bytes)
}

#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn escape_applescript_string(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}

/// Structural PNG validation plus the session's image limits (byte, side,
/// pixel and MIME caps). The authoritative capture-time check still runs in
/// `Read`; this only keeps obviously unusable pastes off disk.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(crate) fn validate_png(
    bytes: &[u8],
    limits: &OpenAiChatImageUrlLimits,
) -> Result<ClipboardPng> {
    const SIGNATURE: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
    // Size first: an oversized clipboard image is read bounded by
    // `extract_macos_png`, so a truncated buffer must still report the real
    // limit instead of a bogus structural error.
    if bytes.len() as u64 > limits.max_image_bytes {
        bail!(
            "clipboard image is {} bytes, over the {} byte limit",
            bytes.len(),
            limits.max_image_bytes
        );
    }
    if bytes.len() < 33 || bytes[..8] != SIGNATURE[..] {
        bail!("clipboard data is not a PNG image");
    }
    if bytes[8..12] != 13u32.to_be_bytes() || bytes[12..16] != b"IHDR"[..] {
        bail!("clipboard PNG has an unsupported header");
    }
    let width = u32::from_be_bytes(bytes[16..20].try_into().expect("4 bytes"));
    let height = u32::from_be_bytes(bytes[20..24].try_into().expect("4 bytes"));
    if width == 0 || height == 0 {
        bail!("clipboard PNG has invalid dimensions {width}x{height}");
    }
    if !limits
        .allowed_mime
        .contains(&mink::runtime::ImageFormat::Png)
    {
        bail!("the current model does not accept PNG images");
    }
    if width > limits.max_dimension || height > limits.max_dimension {
        bail!(
            "clipboard image {width}x{height} exceeds the {}px per-side limit",
            limits.max_dimension
        );
    }
    let pixels = u64::from(width).saturating_mul(u64::from(height));
    if pixels > limits.max_pixels {
        bail!(
            "clipboard image exceeds the {}px decoded-size limit",
            limits.max_pixels
        );
    }
    Ok(ClipboardPng {
        bytes: bytes.to_vec(),
        width,
        height,
    })
}

#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn staging_tail() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos().to_string())
        .unwrap_or_else(|_| "0".to_string())
}

#[cfg(test)]
#[path = "clipboard_tests.rs"]
mod tests;
