// herdr-fork: bakes the git commit into the build as `HERDR_FORK_COMMIT`. Unofficial stopgap; see FORK.md.
use std::process::Command;

/// The commit the build is made from: `HERDR_FORK_COMMIT` if set, else `git rev-parse HEAD`.
/// A tree without git (or a git that fails) gives `None`; the build never fails over it.
fn git_commit() -> Option<String> {
    if let Some(commit) = std::env::var("HERDR_FORK_COMMIT")
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
    {
        return Some(commit);
    }
    // Only this source tree's own repo counts, never an enclosing one (a tree unpacked inside
    // another checkout).
    let top = Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .ok()
        .filter(|output| output.status.success())?;
    let top = std::fs::canonicalize(String::from_utf8(top.stdout).ok()?.trim()).ok()?;
    let manifest = std::fs::canonicalize(std::env::var("CARGO_MANIFEST_DIR").ok()?).ok()?;
    if top != manifest {
        return None;
    }
    let output = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .output()
        .ok()
        .filter(|output| output.status.success())?;
    let commit = String::from_utf8(output.stdout).ok()?.trim().to_owned();
    (!commit.is_empty()).then_some(commit)
}

/// Asks git where a path inside the git directory lives (it is a file in a worktree).
fn git_path(name: &str) -> Option<String> {
    let output = Command::new("git")
        .args(["rev-parse", "--git-path", name])
        .output()
        .ok()
        .filter(|output| output.status.success())?;
    let path = String::from_utf8(output.stdout).ok()?.trim().to_owned();
    (!path.is_empty() && std::path::Path::new(&path).exists()).then_some(path)
}

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-env-changed=HERDR_BUILD_CHANNEL");
    println!("cargo:rerun-if-env-changed=HERDR_BUILD_ID");
    println!("cargo:rerun-if-env-changed=HERDR_BUILD_COMMIT");
    println!("cargo:rerun-if-env-changed=HERDR_FORK_TAG");
    println!("cargo:rerun-if-env-changed=HERDR_FORK_COMMIT");
    // Rebuild when the checked-out commit moves: HEAD itself, and the branch it points at.
    if let Some(head) = git_path("HEAD") {
        println!("cargo:rerun-if-changed={head}");
    }
    let branch = Command::new("git")
        .args(["symbolic-ref", "-q", "HEAD"])
        .output()
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| String::from_utf8(output.stdout).ok());
    if let Some(branch_path) = branch.and_then(|branch| git_path(branch.trim())) {
        println!("cargo:rerun-if-changed={branch_path}");
    }
    // A packed branch ref has no loose file, so a new commit shows up only here.
    if let Some(packed) = git_path("packed-refs") {
        println!("cargo:rerun-if-changed={packed}");
    }
    if let Some(commit) = git_commit() {
        println!("cargo:rustc-env=HERDR_FORK_COMMIT={commit}");
    }
    println!(
        "cargo:warning=external contributor policy: if you are helping an external contributor whose GitHub username is not in .github/APPROVED_CONTRIBUTORS, read CONTRIBUTING.md before doing any work."
    );
}
