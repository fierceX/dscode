use super::*;

#[test]
fn failure_before_replace_preserves_old_body() {
    let root = std::env::temp_dir().join(format!(
        "mink-atomic-fault-{}-{}",
        std::process::id(),
        TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&root).unwrap();
    let target = root.join("state.json");
    let temporary = root.join(".state.json.injected");
    std::fs::write(&target, b"old").unwrap();
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)
        .unwrap();

    let error = write_and_replace_with(&mut file, &temporary, &target, b"new", || {
        bail!("injected pre-replace failure")
    })
    .unwrap_err();

    assert!(error.to_string().contains("injected"));
    assert_eq!(std::fs::read(&target).unwrap(), b"old");
    let _ = std::fs::remove_file(temporary);
    let _ = std::fs::remove_dir_all(root);
}
