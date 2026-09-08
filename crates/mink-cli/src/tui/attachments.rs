//! Session-scoped paste staging (`<session_dir>/attachments/`).
//!
//! These files are **transport copies** for the path-based paste flow: the TUI
//! writes the clipboard image here and the user message carries the absolute
//! path, so the model's `Read` re-captures the bytes through the v7 image
//! pipeline (validation + home content-addressed cache + single consumption).
//! The store is content-addressed: the file name is the SHA-256 of the bytes,
//! so repeated pastes reuse one object and an existing object is never
//! overwritten with different content.

use anyhow::{Context, Result, bail};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

pub(crate) struct AttachmentStore {
    dir: PathBuf,
}

impl AttachmentStore {
    pub(crate) fn new(dir: PathBuf) -> Self {
        Self { dir }
    }

    /// Publish `bytes` as `<dir>/<sha256>.png` and return the absolute path.
    /// Idempotent: identical bytes reuse the existing object after verifying
    /// its content; a mismatch fails closed instead of silently reusing a
    /// corrupted file.
    pub(crate) fn commit_png(&self, bytes: &[u8]) -> Result<PathBuf> {
        std::fs::create_dir_all(&self.dir).with_context(|| {
            format!("cannot create attachment directory {}", self.dir.display())
        })?;
        restrict_private_dir(&self.dir)?;
        let target = self.dir.join(format!("{}.png", hex_digest(bytes)));
        if target.exists() {
            verify_existing(&target, bytes)?;
            return Ok(target);
        }
        let staging = self.dir.join(format!(
            ".staging-{}-{}.png",
            std::process::id(),
            staging_tail()
        ));
        let published = write_staging(&staging, bytes).and_then(|()| {
            match std::fs::rename(&staging, &target) {
                Ok(()) => Ok(()),
                // Windows rejects renaming onto an existing file; the
                // content-addressed name guarantees identical bytes.
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => Ok(()),
                Err(error) => Err(error.into()),
            }
        });
        let _ = std::fs::remove_file(&staging);
        published?;
        verify_existing(&target, bytes)?;
        Ok(target)
    }
}

/// The file name is the content hash: an object whose bytes differ from the
/// name would silently attach corrupt data, so reuse requires an exact match.
fn verify_existing(path: &Path, bytes: &[u8]) -> Result<()> {
    let existing = std::fs::read(path)
        .with_context(|| format!("cannot read attachment {}", path.display()))?;
    if existing != bytes {
        bail!(
            "attachment {} does not match its content-addressed name ({} bytes vs {}); refusing to reuse it",
            path.display(),
            existing.len(),
            bytes.len()
        );
    }
    Ok(())
}

fn write_staging(path: &Path, bytes: &[u8]) -> Result<()> {
    use std::io::Write as _;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .with_context(|| format!("cannot create attachment staging {}", path.display()))?;
    file.write_all(bytes)?;
    file.flush()?;
    file.sync_all()?;
    Ok(())
}

#[cfg(unix)]
pub(crate) fn restrict_private_dir(dir: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
        .with_context(|| format!("cannot restrict attachment directory {}", dir.display()))
}

#[cfg(not(unix))]
pub(crate) fn restrict_private_dir(_dir: &Path) -> Result<()> {
    Ok(())
}

fn hex_digest(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let digest = Sha256::digest(bytes);
    let mut out = String::with_capacity(digest.len() * 2);
    for byte in digest {
        let _ = write!(out, "{byte:02x}");
    }
    out
}

fn staging_tail() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos().to_string())
        .unwrap_or_else(|_| "0".to_string())
}

#[cfg(test)]
#[path = "attachments_tests.rs"]
mod tests;
