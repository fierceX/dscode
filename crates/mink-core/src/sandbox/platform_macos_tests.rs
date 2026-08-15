use super::*;

#[test]
fn resolve_dir_removes_curdir_segments_for_relative_paths() {
    let cwd = Path::new("/tmp/mink-work");

    let resolved = resolve_dir("./qa_cache/./session", cwd);

    assert_eq!(resolved, "/tmp/mink-work/qa_cache/session");
    assert!(!resolved.contains("/./"));
}

#[test]
fn resolve_dir_removes_curdir_segments_for_absolute_paths() {
    let cwd = Path::new("/tmp/mink-work");

    let resolved = resolve_dir("/tmp/./qa_cache/./session", cwd);

    assert_eq!(resolved, "/tmp/qa_cache/session");
    assert!(!resolved.contains("/./"));
}

#[test]
fn resolve_dir_preserves_parent_segments() {
    let cwd = Path::new("/tmp/mink-work/current");

    let resolved = resolve_dir("../qa_cache/./session", cwd);

    assert_eq!(resolved, "/tmp/mink-work/current/../qa_cache/session");
}

#[test]
fn sandbox_profile_normalizes_write_dir_subpaths() {
    let config = SandboxConfig {
        write_dirs: vec!["./qa_cache/./session".into(), "/tmp/./absolute".into()],
        ..SandboxConfig::default()
    };
    let cwd = Path::new("/tmp/mink-work");

    let profile = build_sb_profile(&config, Path::new("/bin/echo"), cwd);

    assert!(profile.contains(r#"(allow file-write* (subpath "/tmp/mink-work/qa_cache/session"))"#));
    assert!(profile.contains(r#"(allow file-write* (subpath "/tmp/absolute"))"#));
    assert!(!profile.contains("/./"));
}

#[test]
fn sandbox_profile_allows_custom_mink_home_root() {
    let config = SandboxConfig {
        write_dirs: vec!["/tmp/workspace".into()],
        ..SandboxConfig::default()
    };
    let cwd = Path::new("/tmp/mink-work");

    let profile = build_sb_profile_with_env(
        &config,
        cwd,
        "/Users/alice",
        Some("/srv/mink-home/./tenant-a"),
    );

    assert!(
        profile.contains(r#"(allow file-write* (subpath "/srv/mink-home/tenant-a"))"#),
        "{profile}"
    );
    assert!(
        !profile.contains(r#"(allow file-write* (subpath "/Users/alice"))"#),
        "{profile}"
    );
}

#[test]
fn sandbox_profile_keeps_home_scope_narrow_when_mink_home_is_home() {
    let config = SandboxConfig {
        write_dirs: vec!["/tmp/workspace".into()],
        ..SandboxConfig::default()
    };
    let cwd = Path::new("/tmp/mink-work");

    let profile = build_sb_profile_with_env(&config, cwd, "/Users/alice", Some("/Users/alice"));

    assert!(
        profile.contains(r#"(allow file-write* (subpath "/Users/alice/.mink"))"#),
        "{profile}"
    );
    assert!(
        !profile.contains(r#"(allow file-write* (subpath "/Users/alice"))"#),
        "{profile}"
    );
}

#[test]
fn sandbox_profile_resolves_relative_mink_home_from_cwd() {
    let config = SandboxConfig {
        write_dirs: vec!["/tmp/workspace".into()],
        ..SandboxConfig::default()
    };
    let cwd = Path::new("/tmp/mink-work");

    let profile = build_sb_profile_with_env(&config, cwd, "/Users/alice", Some("./service-home"));

    assert!(
        profile.contains(r#"(allow file-write* (subpath "/tmp/mink-work/service-home"))"#),
        "{profile}"
    );
    assert!(!profile.contains("/./"));
}
