use super::*;
use mink::runtime::ImageFormat;
use std::path::PathBuf;
use std::sync::Mutex;

fn unique_dir(name: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    std::env::temp_dir().join(format!(
        "mink-clipboard-{name}-{}-{nanos}",
        std::process::id()
    ))
}

fn png_fixture(width: u32, height: u32, len: usize) -> Vec<u8> {
    let mut bytes = vec![0u8; len.max(33)];
    bytes[..8].copy_from_slice(&[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a]);
    bytes[8..12].copy_from_slice(&13u32.to_be_bytes());
    bytes[12..16].copy_from_slice(b"IHDR");
    bytes[16..20].copy_from_slice(&width.to_be_bytes());
    bytes[20..24].copy_from_slice(&height.to_be_bytes());
    bytes
}

#[test]
fn validate_png_accepts_wellformed_header() {
    let image = validate_png(
        &png_fixture(1440, 900, 64),
        &OpenAiChatImageUrlLimits::default(),
    )
    .unwrap();

    assert_eq!((image.width, image.height), (1440, 900));
    assert_eq!(image.bytes.len(), 64);
}

#[test]
fn validate_png_rejects_non_png_bytes() {
    let error = validate_png(b"not an image at all", &OpenAiChatImageUrlLimits::default())
        .unwrap_err()
        .to_string();
    assert!(error.contains("not a PNG image"), "{error}");
}

#[test]
fn validate_png_rejects_invalid_dimensions() {
    let error = validate_png(
        &png_fixture(0, 10, 64),
        &OpenAiChatImageUrlLimits::default(),
    )
    .unwrap_err()
    .to_string();
    assert!(error.contains("invalid dimensions"), "{error}");
}

#[test]
fn validate_png_enforces_byte_and_side_limits() {
    let bytes = png_fixture(64, 64, 64);
    let byte_limited = OpenAiChatImageUrlLimits {
        max_image_bytes: 32,
        ..Default::default()
    };
    let error = validate_png(&bytes, &byte_limited).unwrap_err().to_string();
    assert!(error.contains("over the 32 byte limit"), "{error}");

    let side_limited = OpenAiChatImageUrlLimits {
        max_dimension: 32,
        ..Default::default()
    };
    let error = validate_png(&bytes, &side_limited).unwrap_err().to_string();
    assert!(error.contains("per-side limit"), "{error}");
}

#[test]
fn validate_png_rejects_disallowed_mime() {
    let limits = OpenAiChatImageUrlLimits {
        allowed_mime: vec![ImageFormat::Jpeg],
        ..Default::default()
    };
    let error = validate_png(&png_fixture(16, 16, 64), &limits)
        .unwrap_err()
        .to_string();
    assert!(error.contains("does not accept PNG"), "{error}");
}

struct FakeRunner {
    info: String,
    image: Option<Vec<u8>>,
    fail_write: bool,
    written_to: Mutex<Option<PathBuf>>,
}

impl CommandRunner for FakeRunner {
    fn run(&self, program: &str, args: &[String]) -> Result<Vec<u8>> {
        assert_eq!(program, "osascript");
        let script = args.last().expect("script argument");
        if script == "clipboard info" {
            return Ok(self.info.clone().into_bytes());
        }
        if self.fail_write {
            bail!("osascript exploded");
        }
        let path = script
            .split("POSIX file \"")
            .nth(1)
            .and_then(|rest| rest.split('"').next())
            .expect("staging path in script");
        std::fs::write(path, self.image.clone().expect("fake image"))?;
        *self.written_to.lock().unwrap() = Some(PathBuf::from(path));
        Ok(Vec::new())
    }
}

fn fake_runner(info: &str, image: Option<Vec<u8>>, fail_write: bool) -> FakeRunner {
    FakeRunner {
        info: info.to_string(),
        image,
        fail_write,
        written_to: Mutex::new(None),
    }
}

fn max_bytes() -> u64 {
    OpenAiChatImageUrlLimits::default().max_image_bytes
}

#[test]
fn extract_macos_png_reports_missing_png_clipboard() {
    let dir = unique_dir("no-png");
    let runner = fake_runner("«class utf8», 18, string, 18", None, false);

    let error = extract_macos_png(&runner, &dir, max_bytes())
        .unwrap_err()
        .to_string();

    assert!(error.contains("no PNG image"), "{error}");
    assert!(!dir.exists(), "no staging directory should be created");
}

#[test]
fn extract_macos_png_reads_and_removes_staging_file() {
    let dir = unique_dir("roundtrip");
    let bytes = png_fixture(32, 16, 64);
    let runner = fake_runner("«class PNGf», 64", Some(bytes.clone()), false);

    let extracted = extract_macos_png(&runner, &dir, max_bytes()).unwrap();

    assert_eq!(extracted, bytes);
    let staging = runner.written_to.lock().unwrap().clone().unwrap();
    assert!(!staging.exists(), "staging file must be removed");
    std::fs::remove_dir_all(dir).ok();
}

#[test]
fn extract_macos_png_surfaces_runner_failure() {
    let dir = unique_dir("failure");
    let runner = fake_runner("«class PNGf», 64", None, true);

    let error = extract_macos_png(&runner, &dir, max_bytes())
        .unwrap_err()
        .to_string();

    assert!(
        error.contains("failed to extract the clipboard image"),
        "{error}"
    );
    std::fs::remove_dir_all(dir).ok();
}

#[test]
fn extract_macos_png_bounds_the_staging_read() {
    let dir = unique_dir("bounded");
    let runner = fake_runner("«class PNGf», 4096", Some(png_fixture(64, 64, 4096)), false);

    let extracted = extract_macos_png(&runner, &dir, 64).unwrap();

    assert_eq!(extracted.len(), 65, "read must stop at max_bytes + 1");
    let limits = OpenAiChatImageUrlLimits {
        max_image_bytes: 64,
        ..Default::default()
    };
    let error = validate_png(&extracted, &limits).unwrap_err().to_string();
    assert!(error.contains("over the 64 byte limit"), "{error}");
    std::fs::remove_dir_all(dir).ok();
}

/// Manual smoke test against the real pasteboard. Ignored so normal test runs
/// never shell out; run with:
/// `cargo test -p mink-cli --features tui -- --ignored --nocapture clipboard_smoke`
#[cfg(target_os = "macos")]
#[test]
#[ignore = "reads the real system clipboard"]
fn clipboard_smoke_reads_real_pasteboard() {
    let dir = unique_dir("smoke");
    let limits = OpenAiChatImageUrlLimits::default();

    match read_clipboard_png(&dir, &limits) {
        Ok(image) => {
            assert!(image.bytes.starts_with(&[0x89, b'P', b'N', b'G']));
            assert!(image.width > 0 && image.height > 0);
            let staging_empty = std::fs::read_dir(&dir)
                .map(|mut entries| entries.next().is_none())
                .unwrap_or(false);
            assert!(
                staging_empty,
                "staging file must be removed after extraction"
            );
            // Same chain Ctrl+V runs: content-addressed staging + message marker.
            let path = crate::tui::attachments::AttachmentStore::new(dir.clone())
                .commit_png(&image.bytes)
                .expect("commit clipboard image");
            let staged = std::fs::read(&path).expect("read staged attachment");
            assert_eq!(staged, image.bytes);
            // Dedup hit on a real file: identical bytes reuse the object after
            // content verification.
            let again = crate::tui::attachments::AttachmentStore::new(dir.clone())
                .commit_png(&image.bytes)
                .expect("dedup commit");
            assert_eq!(path, again, "identical bytes must reuse one object");
            let marker = crate::tui::state::PendingImage {
                path: path.clone(),
                width: image.width,
                height: image.height,
                bytes: image.bytes.len(),
            }
            .marker();
            eprintln!(
                "clipboard PNG: {}x{} ({} bytes); staging dir empty: {staging_empty}",
                image.width,
                image.height,
                image.bytes.len()
            );
            eprintln!("attachment: {}", path.display());
            eprintln!("marker: {marker}");
        }
        Err(error) => {
            let message = error.to_string();
            eprintln!("clipboard read error: {message}");
            assert!(
                message.contains("no PNG image") || message.contains("not supported"),
                "unexpected clipboard error: {message}"
            );
        }
    }
    std::fs::remove_dir_all(dir).ok();
}
