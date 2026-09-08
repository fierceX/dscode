use super::*;

fn unique_dir(name: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    std::env::temp_dir().join(format!(
        "mink-attachments-{name}-{}-{nanos}",
        std::process::id()
    ))
}

#[test]
fn commit_png_publishes_content_addressed_file_and_dedups() {
    let dir = unique_dir("commit");
    let store = AttachmentStore::new(dir.clone());

    let first = store.commit_png(b"png-bytes").unwrap();
    let second = store.commit_png(b"png-bytes").unwrap();

    assert_eq!(first, second, "identical bytes must reuse one object");
    assert_eq!(first.file_name().unwrap().to_string_lossy().len(), 68);
    assert!(first.to_string_lossy().ends_with(".png"));
    assert_eq!(std::fs::read(&first).unwrap(), b"png-bytes");
    assert!(store.commit_png(b"other-bytes").unwrap() != first);
    std::fs::remove_dir_all(dir).ok();
}

#[test]
fn commit_png_fails_closed_on_corrupted_existing_object() {
    let dir = unique_dir("corrupt");
    let store = AttachmentStore::new(dir.clone());

    let path = store.commit_png(b"png-bytes").unwrap();
    // Different length.
    std::fs::write(&path, b"short").unwrap();
    let error = store.commit_png(b"png-bytes").unwrap_err().to_string();
    assert!(error.contains("refusing to reuse"), "{error}");

    // Same length, different content: the name is a content hash, so this
    // must fail closed too instead of silently attaching the wrong bytes.
    std::fs::write(&path, b"png-bytez").unwrap();
    let error = store.commit_png(b"png-bytes").unwrap_err().to_string();
    assert!(error.contains("refusing to reuse"), "{error}");
    std::fs::remove_dir_all(dir).ok();
}

#[cfg(unix)]
#[test]
fn commit_png_restricts_directory_permissions() {
    use std::os::unix::fs::PermissionsExt as _;

    let dir = unique_dir("perms");
    let store = AttachmentStore::new(dir.clone());

    store.commit_png(b"png-bytes").unwrap();

    let mode = std::fs::metadata(&dir).unwrap().permissions().mode();
    assert_eq!(mode & 0o777, 0o700);
    std::fs::remove_dir_all(dir).ok();
}
