use super::*;
use std::fs;
use std::sync::atomic::{AtomicUsize, Ordering};

static NEXT_TEMP_ID: AtomicUsize = AtomicUsize::new(0);

fn temp_dir(name: &str) -> PathBuf {
    let id = NEXT_TEMP_ID.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "mink-file-picker-{name}-{}-{id}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn current_dir_prefix_keeps_dot_slash_in_candidates() {
    let root = temp_dir("dot-slash");
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(root.join("src/main.rs"), "fn main() {}\n").unwrap();
    let policy = FilePickerPolicy::restricted_for_tests(root.clone(), vec![root.clone()], 3);

    let (_, candidates) = scan_candidates_for_query("./", &policy);

    assert!(candidates.iter().any(|item| item.path == "./src/"));
    assert!(!candidates.iter().any(|item| item.path == "./src/main.rs"));
    let _ = fs::remove_dir_all(root);
}

#[test]
fn current_dir_prefix_filters_current_level_candidates() {
    let root = temp_dir("dot-slash-filter");
    fs::create_dir_all(root.join("src/bin")).unwrap();
    fs::write(root.join("src/bin/main.rs"), "fn main() {}\n").unwrap();
    fs::write(root.join("src/lib.rs"), "pub fn lib() {}\n").unwrap();
    let policy = FilePickerPolicy::restricted_for_tests(root.clone(), vec![root.clone()], 3);

    let (_, candidates) = scan_candidates_for_query("./src", &policy);

    assert!(candidates.iter().any(|item| item.path == "./src/"));
    assert!(!candidates.iter().any(|item| item.path == "./src/lib.rs"));
    let _ = fs::remove_dir_all(root);
}

#[test]
fn directory_prefix_lists_only_direct_children() {
    let root = temp_dir("direct-children");
    fs::create_dir_all(root.join("src/bin")).unwrap();
    fs::write(root.join("src/bin/main.rs"), "fn main() {}\n").unwrap();
    fs::write(root.join("src/lib.rs"), "pub fn lib() {}\n").unwrap();
    let policy = FilePickerPolicy::restricted_for_tests(root.clone(), vec![root.clone()], 3);

    let (_, candidates) = scan_candidates_for_query("./src/", &policy);

    assert!(candidates.iter().any(|item| item.path == "./src/bin/"));
    assert!(candidates.iter().any(|item| item.path == "./src/lib.rs"));
    assert!(
        !candidates
            .iter()
            .any(|item| item.path == "./src/bin/main.rs")
    );
    let _ = fs::remove_dir_all(root);
}

#[test]
fn filtering_matches_leaf_and_sorts_directories_first() {
    let candidates = vec![
        FilePickCandidate {
            path: "src/test.rs".into(),
            is_dir: false,
        },
        FilePickCandidate {
            path: "src/tools/".into(),
            is_dir: true,
        },
        FilePickCandidate {
            path: "src/tui/".into(),
            is_dir: true,
        },
        FilePickCandidate {
            path: "src/runtime.rs".into(),
            is_dir: false,
        },
    ];

    let items = filter_candidates(&candidates, "src/t");

    let paths: Vec<&str> = items.iter().map(|item| item.path.as_str()).collect();
    assert_eq!(
        paths,
        vec!["src/tools/", "src/tui/", "src/test.rs", "src/runtime.rs"]
    );
}

#[test]
fn current_dir_prefix_hides_dot_entries_until_dot_is_typed() {
    let candidates = vec![
        FilePickCandidate {
            path: "./src/".into(),
            is_dir: true,
        },
        FilePickCandidate {
            path: "./.../".into(),
            is_dir: true,
        },
        FilePickCandidate {
            path: "./.github/".into(),
            is_dir: true,
        },
    ];

    let visible = filter_candidates(&candidates, "./");
    let visible_paths: Vec<&str> = visible.iter().map(|item| item.path.as_str()).collect();
    assert_eq!(visible_paths, vec!["./src/"]);

    let hidden = filter_candidates(&candidates, "./.");
    let hidden_paths: Vec<&str> = hidden.iter().map(|item| item.path.as_str()).collect();
    assert!(hidden_paths.contains(&"./.../"));
    assert!(hidden_paths.contains(&"./.github/"));
}

#[test]
fn dot_query_rescans_with_hidden_directories_enabled() {
    let root = temp_dir("hidden-scan");
    fs::create_dir_all(root.join("src")).unwrap();
    fs::create_dir_all(root.join(".git")).unwrap();
    fs::create_dir_all(root.join("...")).unwrap();
    let policy = FilePickerPolicy::restricted_for_tests(root.clone(), vec![root.clone()], 3);

    let visible = FilePickerState::open("./", "./".len(), &policy);
    let visible_paths: Vec<&str> = visible
        .items
        .iter()
        .map(|item| item.path.as_str())
        .collect();
    assert_eq!(visible_paths, vec!["./src/"]);

    let hidden = FilePickerState::open("./.", "./.".len(), &policy);
    let hidden_paths: Vec<&str> = hidden.items.iter().map(|item| item.path.as_str()).collect();
    assert!(hidden_paths.contains(&"./.../"));
    assert!(hidden_paths.contains(&"./.git/"));
    let _ = fs::remove_dir_all(root);
}
