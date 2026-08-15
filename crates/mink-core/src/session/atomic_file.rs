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
    write_and_replace_with(file, temporary, path, content, || Ok(()))
}

fn write_and_replace_with(
    file: &mut File,
    temporary: &Path,
    path: &Path,
    content: &[u8],
    before_replace: impl FnOnce() -> Result<()>,
) -> Result<()> {
    file.write_all(content)?;
    file.flush()?;
    file.sync_all()?;
    before_replace()?;
    replace_existing(temporary, path)?;
    sync_parent(path)?;
    Ok(())
}

#[cfg(not(windows))]
fn replace_existing(temporary: &Path, path: &Path) -> Result<()> {
    std::fs::rename(temporary, path)?;
    Ok(())
}

#[cfg(windows)]
fn replace_existing(temporary: &Path, path: &Path) -> Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{
        MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW,
    };
    let source = temporary
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    let destination = path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    let result = unsafe {
        MoveFileExW(
            source.as_ptr(),
            destination.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    };
    if result == 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(())
}

fn sync_parent(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        let parent = path
            .parent()
            .ok_or_else(|| anyhow::anyhow!("state path has no parent: {}", path.display()))?;
        if let Err(error) = File::open(parent)?.sync_all() {
            let explicitly_unsupported = error
                .raw_os_error()
                .is_some_and(|code| code == libc::EINVAL || code == libc::ENOTSUP);
            if !explicitly_unsupported {
                return Err(error.into());
            }
        }
    }
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

#[cfg(test)]
#[path = "atomic_file_tests.rs"]
mod tests;
