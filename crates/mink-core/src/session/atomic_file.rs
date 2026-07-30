use anyhow::{Result, bail};
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

pub(crate) fn atomic_replace(path: &Path, content: &[u8]) -> Result<()> {
    ensure_parent(path)?;
    let parent = path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("state path has no parent: {}", path.display()))?;
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| anyhow::anyhow!("invalid state path: {}", path.display()))?;
    let mut last_collision = None;
    for _ in 0..16 {
        let sequence = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let temporary = parent.join(format!(
            ".{file_name}.tmp-{}-{sequence}",
            std::process::id()
        ));
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
        {
            Ok(mut file) => {
                let result = write_and_replace(&mut file, &temporary, path, content);
                if result.is_err() {
                    let _ = std::fs::remove_file(&temporary);
                }
                return result;
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                last_collision = Some(error);
            }
            Err(error) => return Err(error.into()),
        }
    }
    Err(last_collision
        .map(anyhow::Error::from)
        .unwrap_or_else(|| anyhow::anyhow!("cannot allocate temporary state file")))
}

fn write_and_replace(file: &mut File, temporary: &Path, path: &Path, content: &[u8]) -> Result<()> {
    file.write_all(content)?;
    file.flush()?;
    file.sync_all()?;
    std::fs::rename(temporary, path)?;
    Ok(())
}

fn ensure_parent(path: &Path) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("state path has no parent: {}", path.display()))?;
    if parent.as_os_str().is_empty() {
        bail!("state path has an empty parent: {}", path.display());
    }
    std::fs::create_dir_all(parent)?;
    Ok(())
}
