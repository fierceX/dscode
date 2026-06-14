//! macOS sandbox backend: sandbox-exec (built-in).
//!
//! File-system write restrictions work via sandbox-exec: deny-all + allow holes.
//! Read restrictions are handled at the application level (path checks in tools/*)
//! because a blanket ``(deny file-read* (subpath "/"))`` blocks critical system
//! paths that TUI mode requires for initialization but cannot be enumerated.

use crate::config::SandboxConfig;
use std::path::{Component, Path, PathBuf};

/// Build a sandbox-exec command line. Returns the full argv.
pub fn try_sandbox_exec(
    config: &SandboxConfig,
    exe: &Path,
    args: &[String],
) -> Result<Vec<String>, String> {
    let cwd = std::env::current_dir().unwrap_or_default();
    let sb_profile = build_sb_profile(config, exe, &cwd);

    let mut cmd: Vec<String> = vec![
        "sandbox-exec".into(),
        "-p".into(),
        sb_profile,
        exe.display().to_string(),
    ];
    if args.len() > 1 {
        cmd.extend(args[1..].iter().cloned());
    }

    Ok(cmd)
}

/// Build a sandbox-exec profile (Scheme-like DSL).
///
/// Rule evaluation in sandbox-exec:
///   - ``(allow ...)`` rules add capabilities
///   - ``(deny ...)`` rules subtract capabilities
///   - Deny overrides allow regardless of position
///
/// Strategy:
///   1. ``(allow default)`` — let everything start normally (TUI, Mach, IOKit etc.)
///   2. ``(deny file-write* (subpath "/"))`` — deny all writes
///   3. ``(allow file-write* ...)`` — allow writes only to specified dirs
///   4. No blanket ``(deny file-read* ...)`` — reads blocked at app-level
fn build_sb_profile(config: &SandboxConfig, _exe: &Path, cwd: &Path) -> String {
    let home = std::env::var("HOME").unwrap_or_default();
    let mink_home = std::env::var("MINK_HOME").ok();

    build_sb_profile_with_env(config, cwd, &home, mink_home.as_deref())
}

fn build_sb_profile_with_env(
    config: &SandboxConfig,
    cwd: &Path,
    home: &str,
    mink_home: Option<&str>,
) -> String {
    let mut lines: Vec<String> = vec!["(version 1)".into()];

    // ═══ Step 1: Allow default — let the process initialize ═══
    lines.push("(allow default)".into());

    // ═══ Step 2: Write restrictions ═════════════════════════════
    if !config.write_dirs.is_empty() {
        // Deny all writes first (deny overrides allow regardless of order)
        lines.push("(deny file-write* (subpath \"/\"))".into());

        // Punch holes for user-specified write dirs
        for d in &config.write_dirs {
            let resolved = resolve_dir(d, cwd);
            lines.push(format!("(allow file-write* (subpath \"{}\"))", resolved));
        }

        // Always allow system temp directory (TMPDIR for Edit's unified_diff_color)
        let tmpdir = std::env::temp_dir();
        let tmpdir_str = tmpdir.display().to_string();
        lines.push(format!("(allow file-write* (subpath \"{tmpdir_str}\"))"));
        // Also allow /tmp and /private/tmp (common temp locations)
        lines.push("(allow file-write* (subpath \"/tmp\"))".into());
        lines.push("(allow file-write* (subpath \"/private/tmp\"))".into());

        // Always allow mink session storage. When MINK_HOME is a dedicated
        // service root, allow that root so Direct layout can write
        // MINK_HOME/<session_id>. If MINK_HOME is unset or equals HOME, keep
        // the narrower historical HOME/.mink permission.
        for dir in session_storage_write_dirs(home, mink_home, cwd) {
            lines.push(format!("(allow file-write* (subpath \"{dir}\"))"));
        }
    }

    // ═══ Read restrictions are NOT done here ═════════════════════
    // A blanket (deny file-read* (subpath "/")) would break TUI
    // initialization because macOS system paths are too numerous to
    // enumerate explicitly.
    //
    // Read restrictions are enforced at the application level via
    // path canonicalization + prefix checks in tools/file.rs.

    lines.join("\n")
}

fn session_storage_write_dirs(home: &str, mink_home: Option<&str>, cwd: &Path) -> Vec<String> {
    let mut dirs = Vec::new();
    let home = home.trim();
    let mink_home = mink_home.map(str::trim).filter(|value| !value.is_empty());

    match mink_home {
        Some(root) if root != "/" && root != home => {
            dirs.push(resolve_dir(root, cwd));
        }
        _ if !home.is_empty() && home != "/" => {
            dirs.push(
                normalize_sandbox_path(&Path::new(home).join(".mink"))
                    .display()
                    .to_string(),
            );
        }
        _ => {}
    }

    dirs.push(
        normalize_sandbox_path(&cwd.join(".mink"))
            .display()
            .to_string(),
    );
    dirs.sort();
    dirs.dedup();
    dirs
}

fn resolve_dir(dir: &str, cwd: &Path) -> String {
    let p = Path::new(dir);
    let abs = if p.is_absolute() {
        p.to_path_buf()
    } else {
        cwd.join(p)
    };
    normalize_sandbox_path(&abs).display().to_string()
}

fn normalize_sandbox_path(path: &Path) -> PathBuf {
    path.components()
        .filter(|component| !matches!(component, Component::CurDir))
        .collect()
}

#[cfg(test)]
mod tests {
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
        let mut config = SandboxConfig::default();
        config.write_dirs = vec!["./qa_cache/./session".into(), "/tmp/./absolute".into()];
        let cwd = Path::new("/tmp/mink-work");

        let profile = build_sb_profile(&config, Path::new("/bin/echo"), cwd);

        assert!(
            profile.contains(r#"(allow file-write* (subpath "/tmp/mink-work/qa_cache/session"))"#)
        );
        assert!(profile.contains(r#"(allow file-write* (subpath "/tmp/absolute"))"#));
        assert!(!profile.contains("/./"));
    }

    #[test]
    fn sandbox_profile_allows_custom_mink_home_root() {
        let mut config = SandboxConfig::default();
        config.write_dirs = vec!["/tmp/workspace".into()];
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
        let mut config = SandboxConfig::default();
        config.write_dirs = vec!["/tmp/workspace".into()];
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
        let mut config = SandboxConfig::default();
        config.write_dirs = vec!["/tmp/workspace".into()];
        let cwd = Path::new("/tmp/mink-work");

        let profile =
            build_sb_profile_with_env(&config, cwd, "/Users/alice", Some("./service-home"));

        assert!(
            profile.contains(r#"(allow file-write* (subpath "/tmp/mink-work/service-home"))"#),
            "{profile}"
        );
        assert!(!profile.contains("/./"));
    }
}
