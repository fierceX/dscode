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

    // ── File-system bind mounts ──────────────────────────────
    let cwd = std::env::current_dir().unwrap_or_default();

    for d in &config.read_dirs {
        let resolved = resolve_dir(d, &cwd);
        cmd.push("--bindmount_ro".into());
        cmd.push(format!("{0}:{0}", resolved));
    }
    for d in &config.write_dirs {
        let resolved = resolve_dir(d, &cwd);
        cmd.push("--bindmount".into());
        cmd.push(format!("{0}:{0}", resolved));
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

    // ── Bind mounts ──────────────────────────────────────────
    for d in &config.read_dirs {
        let resolved = resolve_dir(d, &cwd);
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

    // ── Namespace isolation ──────────────────────────────────
    cmd.push("--unshare-pid".into());
    cmd.push("--unshare-ipc".into());
    cmd.push("--unshare-uts".into());

    if !config.allow_network {
        cmd.push("--unshare-net".into());
    }

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
