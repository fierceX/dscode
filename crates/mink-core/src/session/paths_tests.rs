use super::*;

#[test]
fn project_key_strips_leading_slash() {
    let key = project_key(std::path::Path::new("/Users/test/project"));
    assert!(!key.starts_with("--"));
}

#[test]
fn project_key_replaces_special_chars() {
    let key = project_key(std::path::Path::new("/tmp/my project!"));
    assert!(!key.contains('!'));
    assert!(!key.contains(' '));
}

#[test]
fn chrono_session_id_format() {
    let id = chrono_session_id();
    // Format: YYYYMMDD-HHmmss-XXXX
    let parts: Vec<_> = id.split('-').collect();
    assert_eq!(parts.len(), 3);
    assert_eq!(parts[0].len(), 8); // YYYYMMDD
    assert_eq!(parts[1].len(), 6); // HHmmss
    assert_eq!(parts[2].len(), 4); // XXXX
}

#[test]
fn project_key_different_dirs_different_keys() {
    let k1 = project_key(std::path::Path::new("/a/b"));
    let k2 = project_key(std::path::Path::new("/a/c"));
    assert_ne!(k1, k2);
}

#[test]
fn formerly_colliding_paths_have_distinct_project_keys() {
    assert_ne!(
        project_key(Path::new("/a/b-c")),
        project_key(Path::new("/a-b/c"))
    );
}

#[test]
fn nonexistent_paths_are_lexically_normalized_before_hashing() {
    assert_eq!(
        project_key(Path::new("/definitely-missing/a/../b")),
        project_key(Path::new("/definitely-missing/b"))
    );
}

#[test]
fn paths_for_layout_project_scoped_keeps_existing_shape() {
    let paths = paths_for_layout(
        Path::new("/home/mink"),
        Path::new("/work/project"),
        "sid",
        SessionLayout::ProjectScoped,
    );
    assert!(
        paths
            .session_dir
            .to_string_lossy()
            .contains("work-project--")
    );
}

#[test]
fn paths_for_layout_home_scoped_skips_project_key() {
    let paths = paths_for_layout(
        Path::new("/home/mink"),
        Path::new("/work/project"),
        "sid",
        SessionLayout::HomeScoped,
    );
    assert_eq!(
        paths.session_dir,
        PathBuf::from("/home/mink/.mink/sessions/sid")
    );
}

#[test]
fn paths_for_layout_direct_uses_home_as_session_root() {
    let paths = paths_for_layout(
        Path::new("/home/mink"),
        Path::new("/work/project"),
        "sid",
        SessionLayout::Direct,
    );
    assert_eq!(paths.session_dir, PathBuf::from("/home/mink/sid"));
}

#[test]
fn paths_for_layout_isolated_uses_home_as_session_dir() {
    let paths = paths_for_layout(
        Path::new("/home/mink/session-root"),
        Path::new("/work/project"),
        "sid",
        SessionLayout::Isolated,
    );
    assert_eq!(paths.session_id, "sid");
    assert_eq!(paths.base_dir, PathBuf::from("/home/mink/session-root"));
    assert_eq!(paths.session_dir, PathBuf::from("/home/mink/session-root"));
    assert_eq!(
        paths.conversation,
        PathBuf::from("/home/mink/session-root/conversation.jsonl")
    );
    assert_eq!(
        paths.todos,
        PathBuf::from("/home/mink/session-root/todos.json")
    );
}
