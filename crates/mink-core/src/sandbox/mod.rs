//! Sandboxing via OS-native tools.
//!
//! On Linux, tries nsjail first, then bubblewrap.
//! On macOS, uses the built-in sandbox-exec.
//!
//! The core function is [`reexec_in_sandbox`] which replaces the current
//! process image with the same binary running inside a sandbox.
//! It sets `MINK_SANDBOXED=1` to prevent infinite re-exec loops.

use crate::config::SandboxConfig;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::Command;

#[cfg(target_os = "linux")]
mod platform_linux;
#[cfg(target_os = "macos")]
mod platform_macos;

/// Try to re-execute the current process inside a sandbox.
///
/// On success, this function does NOT return — the process is replaced.
/// On failure (sandbox tool not found, etc.), returns `Ok(())` so the
/// caller can fall back to running without sandbox.
///
/// `exe` is the path to the current binary.
/// `args` are the original command-line arguments (including argv[0]).
pub fn reexec_in_sandbox(config: &SandboxConfig, exe: &Path, args: &[String]) {
    // Prevent infinite re-exec loop
    if std::env::var("MINK_SANDBOXED").is_ok() {
        return;
    }

    if !config.is_active() {
        return;
    }

    // Set the guard *before* exec — if exec fails, the env var remains
    // but that's fine because the process continues and exits normally.
    // Safety: setting environment variable before process replacement is safe
    // in single-threaded context (we haven't spawned any threads yet).
    unsafe {
        std::env::set_var("MINK_SANDBOXED", "1");
    }

    let result = try_reexec(config, exe, args);

    // If we get here, sandbox exec failed — hard fail instead of silent fallback
    match result {
        Ok(()) => {
            // exec succeeded and replaced us, so this is unreachable.
            // But if it didn't replace us, it means exec failed silently.
            eprintln!("[mink] Fatal: sandbox exec returned unexpectedly");
        }
        Err(e) => {
            eprintln!("[mink] Fatal: sandbox unavailable ({}), exiting", e);
        }
    }
    std::process::exit(1);
}

fn try_reexec(config: &SandboxConfig, exe: &Path, args: &[String]) -> Result<(), String> {
    #[cfg(target_os = "linux")]
    {
        if config.backend == "nsjail" || config.backend == "auto" {
            match platform_linux::try_nsjail(config, exe, args) {
                Ok(cmd) => {
                    exec_cmd(&cmd)?;
                    return Err("nsjail exec returned unexpectedly".into());
                }
                Err(e) => {
                    if config.backend == "nsjail" {
                        return Err(e); // explicit backend request → hard error
                    }
                    // auto mode: fall through to bwrap
                    eprintln!("[mink] nsjail not available: {e}");
                }
            }
        }

        if config.backend == "bwrap" || config.backend == "auto" {
            match platform_linux::try_bwrap(config, exe, args) {
                Ok(cmd) => {
                    exec_cmd(&cmd)?;
                    return Err("bwrap exec returned unexpectedly".into());
                }
                Err(e) => {
                    return Err(format!("bwrap: {e}"));
                }
            }
        }

        Err("no Linux sandbox backend available (tried nsjail, bwrap)".into())
    }

    #[cfg(target_os = "macos")]
    {
        match platform_macos::try_sandbox_exec(config, exe, args) {
            Ok(cmd) => {
                exec_cmd(&cmd)?;
                Err("sandbox-exec returned unexpectedly".into())
            }
            Err(e) => Err(e),
        }
    }

    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        Err("sandbox not supported on this platform".into())
    }
}

/// Replace the current process with the given command.
/// Does NOT return on success.
fn exec_cmd(cmd: &[String]) -> Result<(), String> {
    let (prog, args) = cmd
        .split_first()
        .ok_or_else(|| "empty sandbox command".to_string())?;
    let err = Command::new(prog).args(args).exec();
    Err(format!("exec({prog}) failed: {err}"))
}
