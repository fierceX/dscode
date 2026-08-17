use super::*;

fn temp_root(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("mink-cap-context-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

#[test]
fn context_file_snapshot_loads_project_and_user_files() {
    let root = temp_root("load");
    let home = root.join("home");
    let cwd = root.join("workspace");
    std::fs::create_dir_all(home.join(".mink")).unwrap();
    std::fs::create_dir_all(&cwd).unwrap();
    std::fs::write(home.join(".mink/AGENTS.md"), "global instructions").unwrap();
    std::fs::write(cwd.join("AGENTS.md"), "project instructions").unwrap();

    let snapshot = build_default_context_file_snapshot(&cwd, &home).unwrap();

    assert_eq!(snapshot.always_apply.len(), 2);
    assert!(
        snapshot
            .always_apply
            .iter()
            .any(|file| file.context_file.name == "project")
    );
    assert!(
        snapshot
            .always_apply
            .iter()
            .any(|file| file.context_file.name == "global")
    );
    assert_eq!(snapshot.always_apply[0].context_file.name, "global");
    assert_eq!(snapshot.always_apply[1].context_file.name, "project");
    let _ = std::fs::remove_dir_all(root);
}
