// Build-time version provenance: embed the git commit hash (and dirty marker)
// into the binaries so `--version` can identify the exact source revision.
//
// Override with MINK_GIT_HASH (e.g. release pipelines pinning a specific
// commit); empty when git is unavailable so builds still succeed.

use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=.git/HEAD");
    println!("cargo:rerun-if-changed=.git/refs/heads/main");
    println!("cargo:rerun-if-changed=.git/packed-refs");
    println!("cargo:rerun-if-env-changed=MINK_GIT_HASH");

    let hash = match std::env::var("MINK_GIT_HASH") {
        Ok(value) if !value.trim().is_empty() => value.trim().to_string(),
        _ => git_short_hash().map(|hash| {
            if working_tree_dirty() {
                format!("{hash}-dirty")
            } else {
                hash
            }
        }).unwrap_or_default(),
    };

    println!("cargo:rustc-env=MINK_GIT_HASH={hash}");
}

fn git_short_hash() -> Option<String> {
    let output = Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let hash = String::from_utf8_lossy(&output.stdout).trim().to_string();
    (!hash.is_empty()).then_some(hash)
}

/// Dirty = any tracked file changed (untracked build artifacts excluded, so
/// `target/`, `cpython-wasi/`, etc. do not mark a clean checkout as dirty).
fn working_tree_dirty() -> bool {
    Command::new("git")
        .args(["status", "--porcelain", "--untracked-files=no"])
        .output()
        .ok()
        .is_some_and(|output| output.status.success() && !output.stdout.is_empty())
}
