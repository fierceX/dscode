//! Linux sandbox backends: nsjail (preferred) and bubblewrap (fallback).

use crate::config::SandboxConfig;
use std::path::Path;
use std::process::Command;

/// Build a nsjail command line. Returns the full argv (including "nsjail").
pub fn try_nsjail(
    config: &SandboxConfig,
    exe: &Path,
    args: &[String],
) -> Result<Vec<String>, String> {
    // Check nsjail availability
    Command::new("nsjail")
        .arg("--version")
        .output()
        .map_err(|_| "nsjail binary not found in PATH".to_string())?;

    let mut cmd: Vec<String> = vec!["nsjail".into(), "--mode".into(), "execve".into()];

    // ── Essential paths (binary + system libraries) ──────────
    let cwd = std::env::current_dir().unwrap_or_default();

    if let Some(parent) = exe.parent() {
        let parent_str = parent.display().to_string();
        if !parent_str.is_empty() {
            cmd.push("--bindmount_ro".into());
            cmd.push(format!("{0}:{0}", parent_str));
        }
    }
    for d in ["/usr", "/lib", "/lib64", "/etc", "/run"] {
        cmd.push("--bindmount_ro".into());
        cmd.push(format!("{0}:{0}", d));
    }

    // ── Temp directory (tmpfs for Edit tool unified_diff_color, etc.) ──
    cmd.push("--tmpfs".into());
    cmd.push("/tmp".into());
    // If TMPDIR is set and differs from /tmp, also mount it
    if let Ok(tmpdir) = std::env::var("TMPDIR") {
        if !tmpdir.is_empty() && tmpdir != "/tmp" {
            cmd.push("--tmpfs".into());
            cmd.push(tmpdir);
        }
    }

    // ── User-configured bind mounts ──────────────────────────
    // Write dirs imply read access; skip them in the read-only list
    // to prevent ro-bind from shadowing writable bind mount on
    // systems where mount order behaves unexpectedly.
    let write_paths: Vec<String> = config
        .write_dirs
        .iter()
        .map(|d| resolve_dir(d, &cwd))
        .collect();
    for d in &config.read_dirs {
        let resolved = resolve_dir(d, &cwd);
        if write_paths.iter().any(|w| resolved.starts_with(w)) {
            continue;
        }
        cmd.push("--bindmount_ro".into());
        cmd.push(format!("{0}:{0}", resolved));
    }
    for d in &config.write_dirs {
        let resolved = resolve_dir(d, &cwd);
        cmd.push("--bindmount".into());
        cmd.push(format!("{0}:{0}", resolved));
    }

    // ── HOME directory (read-only for config access) ─────────
    if let Ok(ref home) = std::env::var("HOME") {
        if !home.is_empty() {
            cmd.push("--bindmount_ro".into());
            cmd.push(format!("{0}:{0}", home));
        }
    }

    // ── MINK_HOME / default ~/.mink (writable for session persistence) ──
    let mink_home = std::env::var("MINK_HOME").unwrap_or_else(|_| {
        std::env::var("HOME")
            .map(|h| format!("{}/.mink", h))
            .unwrap_or_else(|_| "/tmp/.mink".to_string())
    });
    if !mink_home.is_empty() && mink_home != "/" {
        let _ = std::fs::create_dir_all(&mink_home);
        cmd.push("--bindmount".into());
        cmd.push(format!("{}:{}", mink_home, mink_home));
    }

    // Working directory: first write dir, then first read dir
    let work_dir = config
        .write_dirs
        .first()
        .or(config.read_dirs.first())
        .map(|d| resolve_dir(d, &cwd))
        .unwrap_or_else(|| cwd.display().to_string());
    cmd.push("--cwd".into());
    cmd.push(work_dir);

    // ── Resource limits ──────────────────────────────────────
    cmd.push("--cgroup_mem_max".into());
    cmd.push((config.max_memory_mb * 1024 * 1024).to_string());
    cmd.push("--cgroup_pids_max".into());
    cmd.push(config.max_pids.to_string());
    cmd.push("--time_limit".into());
    cmd.push(config.timeout_secs.to_string());

    // ── Security hardening ───────────────────────────────────
    cmd.push("--disable_proc".into());

    if !config.allow_network {
        cmd.push("--iface_no_lo".into());
    }

    // ── Target binary ────────────────────────────────────────
    cmd.push("--".into());
    cmd.push(exe.display().to_string());
    // Skip argv[0] (the binary name itself), pass the rest
    if args.len() > 1 {
        cmd.extend(args[1..].iter().cloned());
    }

    Ok(cmd)
}

/// Build a bubblewrap command line. Returns the full argv (including "bwrap").
pub fn try_bwrap(
    config: &SandboxConfig,
    exe: &Path,
    args: &[String],
) -> Result<Vec<String>, String> {
    Command::new("bwrap")
        .arg("--version")
        .output()
        .map_err(|_| "bwrap binary not found in PATH".to_string())?;

    let cwd = std::env::current_dir().unwrap_or_default();

    let mut cmd: Vec<String> = vec!["bwrap".into()];

    // ── Minimal filesystem skeleton ──────────────────────────
    cmd.push("--dev".into());
    cmd.push("/dev".into());
    cmd.push("--proc".into());
    cmd.push("/proc".into());
    cmd.push("--tmpfs".into());
    cmd.push("/tmp".into());

    // ── Essential paths (binary + system libraries) ──────────
    // Without these the dynamically-linked binary won't even start.
    if let Some(parent) = exe.parent() {
        let parent_str = parent.display().to_string();
        if !parent_str.is_empty() {
            cmd.push("--ro-bind".into());
            cmd.push(parent_str.clone());
            cmd.push(parent_str);
        }
    }
    for d in ["/usr", "/lib", "/lib64", "/etc", "/run"] {
        cmd.push("--ro-bind".into());
        cmd.push(d.to_string());
        cmd.push(d.to_string());
    }

    // ── User-configured bind mounts ──────────────────────────
    // Write dirs imply read access; skip them in the read-only list
    // to prevent ro-bind from shadowing writable bind mount on
    // systems where mount order behaves unexpectedly.
    let write_paths: Vec<String> = config
        .write_dirs
        .iter()
        .map(|d| resolve_dir(d, &cwd))
        .collect();
    for d in &config.read_dirs {
        let resolved = resolve_dir(d, &cwd);
        if write_paths.iter().any(|w| resolved.starts_with(w)) {
            continue;
        }
        cmd.push("--ro-bind".into());
        cmd.push(resolved.clone());
        cmd.push(resolved);
    }
    for d in &config.write_dirs {
        let resolved = resolve_dir(d, &cwd);
        cmd.push("--bind".into());
        cmd.push(resolved.clone());
        cmd.push(resolved);
    }

    // ── HOME directory (read-only for config access) ─────────
    if let Ok(ref home) = std::env::var("HOME") {
        if !home.is_empty() {
            cmd.push("--ro-bind".into());
            cmd.push(home.to_string());
            cmd.push(home.to_string());
        }
    }

    // ── MINK_HOME / default ~/.mink (writable for session persistence) ──
    let mink_home = std::env::var("MINK_HOME").unwrap_or_else(|_| {
        std::env::var("HOME")
            .map(|h| format!("{}/.mink", h))
            .unwrap_or_else(|_| "/tmp/.mink".to_string())
    });
    if !mink_home.is_empty() && mink_home != "/" {
        let _ = std::fs::create_dir_all(&mink_home);
        cmd.push("--bind".into());
        cmd.push(mink_home.clone());
        cmd.push(mink_home);
    }

    // ── Namespace isolation ──────────────────────────────────
    cmd.push("--unshare-pid".into());
    cmd.push("--unshare-ipc".into());
    cmd.push("--unshare-uts".into());

    if !config.allow_network {
        cmd.push("--unshare-net".into());
    }

    // ── Working directory (same logic as nsjail) ──────────────
    let work_dir = config
        .write_dirs
        .first()
        .or(config.read_dirs.first())
        .map(|d| resolve_dir(d, &cwd))
        .unwrap_or_else(|| cwd.display().to_string());
    cmd.push("--chdir".into());
    cmd.push(work_dir);

    // ── Target binary ────────────────────────────────────────
    cmd.push("--".into());
    cmd.push(exe.display().to_string());
    if args.len() > 1 {
        cmd.extend(args[1..].iter().cloned());
    }

    Ok(cmd)
}

/// Resolve a directory path: absolute → as-is, relative → join with cwd.
fn resolve_dir(dir: &str, cwd: &Path) -> String {
    let p = Path::new(dir);
    if p.is_absolute() {
        p.display().to_string()
    } else {
        cwd.join(p).display().to_string()
    }
}
