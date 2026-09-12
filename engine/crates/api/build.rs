//! Stamps the build with the commit it came from.
//!
//! This exists because three separate fixes were reported as not working when the engine
//! running was simply an older binary. "Which build am I talking to?" was unanswerable from
//! outside, so it is now answered by `/health` and shown in the UI.

#![allow(non_snake_case, non_upper_case_globals)]

use std::process::Command;

fn main() {
    // Re-run when HEAD moves, so the stamp cannot go stale while the sources look unchanged.
    println!("cargo:rerun-if-changed=../../../.git/HEAD");
    println!("cargo:rerun-if-changed=../../../.git/index");

    let strCommit = ReadGitCommit().unwrap_or_else(|| "unknown".to_string());
    println!("cargo:rustc-env=DVSIM_BUILD_COMMIT={strCommit}");

    let strBuiltAt = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|since| since.as_secs().to_string())
        .unwrap_or_else(|_| "0".to_string());
    println!("cargo:rustc-env=DVSIM_BUILT_AT_SECS={strBuiltAt}");
}

/// The short commit hash, with a `+` when the tree had uncommitted changes.
///
/// `None` rather than a guess when git is unavailable — a build from a tarball has no commit,
/// and inventing one would defeat the point.
fn ReadGitCommit() -> Option<String> {
    let output = Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }

    let strCommit = String::from_utf8(output.stdout).ok()?.trim().to_string();
    if strCommit.is_empty() {
        return None;
    }

    let bIsDirty = Command::new("git")
        .args(["status", "--porcelain"])
        .output()
        .map(|status| !status.stdout.is_empty())
        .unwrap_or(false);

    Some(if bIsDirty {
        format!("{strCommit}+")
    } else {
        strCommit
    })
}
