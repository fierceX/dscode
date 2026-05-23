//! macOS sandbox backend: sandbox-exec (built-in).
//!
//! File-system write restrictions work via sandbox-exec: deny-all + allow holes.
//! Read restrictions are handled at the application level (path checks in tools/*)
//! because a blanket ``(deny file-read* (subpath "/"))`` blocks critical system
//! paths that TUI mode requires for initialization but cannot be enumerated.

use crate::config::SandboxConfig;
use std::path::Path;

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
            lines.push(format!(
                "(allow file-write* (subpath \"{}\"))",
                resolved
            ));
        }

        // Always allow dscode session storage
        lines.push(format!(
            "(allow file-write* (subpath \"{}/.dscode\"))",
            home
        ));
        lines.push(format!(
            "(allow file-write* (subpath \"{}/.dscode\"))",
            cwd.display()
        ));

        // Always allow temp files (Edit tool diff, etc.)
        lines.push("(allow file-write* (subpath \"/tmp\"))".into());
        lines.push("(allow file-write* (subpath \"/private/tmp\"))".into());
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

fn resolve_dir(dir: &str, cwd: &Path) -> String {
    let p = Path::new(dir);
    if p.is_absolute() {
        p.display().to_string()
    } else {
        cwd.join(p).display().to_string()
    }
}
