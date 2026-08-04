// Build-time version provenance: embed the git commit hash (and dirty marker)
// into the binaries so `--version` can identify the exact source revision.
//
// Override with MINK_GIT_HASH (e.g. release pipelines pinning a specific
// commit); empty when git is unavailable so builds still succeed.

use std::path::PathBuf;
use std::process::Command;

fn main() {
    // `.git` lives at the workspace root; absolute paths keep rerun tracking
    // reliable regardless of the build script's working directory.
    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let git_dir = manifest.join("../../.git");
    println!("cargo:rerun-if-changed={}", git_dir.join("HEAD").display());
    println!(
        "cargo:rerun-if-changed={}",
        git_dir.join("refs/heads/main").display()
    );
    println!(
        "cargo:rerun-if-changed={}",
        git_dir.join("packed-refs").display()
    );
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
