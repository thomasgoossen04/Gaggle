//! Emits `GAGGLE_VERSION` = `2.0.<short-commit-hash>` for the running binary to
//! report. Falls back to `2.0.unknown` when git history is unavailable (e.g. a
//! build from a source tarball).

use std::process::Command;

fn main() {
    let hash = Command::new("git")
        .args(["rev-parse", "--short=7", "HEAD"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "unknown".into());

    println!("cargo:rustc-env=GAGGLE_VERSION=2.0.{hash}");
    println!("cargo:rerun-if-changed=../../.git/HEAD");
    println!("cargo:rerun-if-changed=../../.git/refs");
}
