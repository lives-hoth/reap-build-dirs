//! What git says about a dir: the only way to tell a main checkout from a worktree,
//! whatever the worktree layout.

use std::path::Path;
use std::process::Command;

/// The run refuses to start without git: a main checkout would look unprotected.
pub fn runnable() -> bool {
    Command::new("git").arg("--version").output().is_ok_and(|o| o.status.success())
}

pub enum Checkout {
    /// The repo's own working tree: its git dir is the common dir.
    Main,
    /// A `git worktree add` checkout: its git dir is under the common dir.
    Linked,
    Outside,
}

/// git ran but answered with something other than "not a repository".
pub struct GitFailed;

pub fn checkout(dir: &Path) -> Result<Checkout, GitFailed> {
    let out = Command::new("git")
        .arg("-C").arg(dir)
        .args(["rev-parse", "--path-format=absolute", "--git-dir", "--git-common-dir"])
        .env("LC_ALL", "C") // the "not a git repository" check reads git's message
        .output()
        .map_err(|_| GitFailed)?;
    if !out.status.success() {
        return if String::from_utf8_lossy(&out.stderr).contains("not a git repository") {
            Ok(Checkout::Outside)
        } else {
            Err(GitFailed)
        };
    }
    let stdout = String::from_utf8(out.stdout).map_err(|_| GitFailed)?;
    let mut lines = stdout.lines().map(|p| Path::new(p).canonicalize());
    match (lines.next(), lines.next()) {
        (Some(Ok(git_dir)), Some(Ok(common))) if git_dir == common => Ok(Checkout::Main),
        (Some(Ok(_)), Some(Ok(_))) => Ok(Checkout::Linked),
        _ => Err(GitFailed),
    }
}
