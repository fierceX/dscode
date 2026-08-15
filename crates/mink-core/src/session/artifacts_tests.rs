use super::*;

fn temp_manager(name: &str) -> ArtifactManager {
    let dir = std::env::temp_dir().join(format!("mink-artifacts-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    ArtifactManager::new(dir)
}

#[test]
fn write_and_read_artifact() {
    let manager = temp_manager("write-read");
    let record = manager.write_text("Bash", "full output", "hello").unwrap();
    assert_eq!(record.id, "bash-0001");
    assert_eq!(manager.read_text(&record.id).unwrap(), "hello");
    let _ = std::fs::remove_dir_all(manager.root);
}

#[test]
fn bounded_read_preserves_utf8_boundary() {
    let manager = temp_manager("bounded-read");
    let record = manager
        .write_text("Bash", "unicode output", "abc中文def")
        .unwrap();
    let (content, truncated) = manager.read_text_prefix(&record.id, 5).unwrap();
    assert_eq!(content, "abc");
    assert!(truncated);
    let _ = std::fs::remove_dir_all(manager.root);
}

#[test]
fn resumed_manager_continues_ids_without_overwriting() {
    let manager = temp_manager("resume-counter");
    let root = manager.root.clone();
    let old = manager
        .write_text("Bash", "old output", "inherited")
        .unwrap();
    drop(manager);

    let resumed = ArtifactManager::new(root.clone());
    resumed.ensure().unwrap();
    let new = resumed
        .write_text("Bash", "new output", "continued")
        .unwrap();

    assert_eq!(old.id, "bash-0001");
    assert_eq!(new.id, "bash-0002");
    assert_eq!(resumed.read_text(&old.id).unwrap(), "inherited");
    assert_eq!(resumed.read_text(&new.id).unwrap(), "continued");
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn orphan_artifact_file_is_not_overwritten() {
    let manager = temp_manager("orphan-file");
    manager.ensure().unwrap();
    std::fs::write(manager.root.join("bash-0001.txt"), "orphaned content").unwrap();

    let record = manager
        .write_text("Bash", "new output", "new content")
        .unwrap();

    assert_eq!(record.id, "bash-0002");
    assert_eq!(
        std::fs::read_to_string(manager.root.join("bash-0001.txt")).unwrap(),
        "orphaned content"
    );
    let _ = std::fs::remove_dir_all(manager.root);
}

#[test]
fn rejects_path_traversal_id() {
    let manager = temp_manager("reject");
    assert!(manager.read_text("../secret").is_err());
    let _ = std::fs::remove_dir_all(manager.root);
}

#[test]
fn parses_artifact_url_id() {
    assert_eq!(
        artifact_id_from_url("artifact://bash-0001"),
        Some("bash-0001")
    );
    assert_eq!(
        artifact_id_from_url("artifact://bash-0001:1-20"),
        Some("bash-0001")
    );
    assert_eq!(artifact_id_from_url("file://x"), None);
}

#[test]
fn sanitizes_tool_name_for_id() {
    let manager = temp_manager("sanitize");
    let record = manager.write_text("Tool Name", "full output", "x").unwrap();
    assert_eq!(record.id, "toolname-0001");
    let _ = std::fs::remove_dir_all(manager.root);
}

#[test]
fn concurrent_spills_create_unique_bodies_and_complete_index() {
    let manager = std::sync::Arc::new(temp_manager("concurrent"));
    let root = manager.root.clone();
    let mut workers = Vec::new();
    for index in 0..32 {
        let manager = manager.clone();
        workers.push(std::thread::spawn(move || {
            manager
                .write_text("Read", "concurrent output", &format!("body-{index}"))
                .unwrap()
        }));
    }
    let records = workers
        .into_iter()
        .map(|worker| worker.join().unwrap())
        .collect::<Vec<_>>();
    let ids = records
        .iter()
        .map(|record| record.id.as_str())
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(ids.len(), 32);
    for record in &records {
        assert!(root.join(&record.path).is_file());
    }
    let index = std::fs::read_to_string(root.join("index.jsonl")).unwrap();
    assert_eq!(index.lines().filter(|line| !line.is_empty()).count(), 32);
    for line in index.lines() {
        serde_json::from_str::<ArtifactRecord>(line).unwrap();
    }
    let _ = std::fs::remove_dir_all(root);
}
