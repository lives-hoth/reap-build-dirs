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

/// git ran but answered with something other than "not a repository": what it said.
pub struct GitFailed(pub String);

pub fn checkout(dir: &Path) -> Result<Checkout, GitFailed> {
    // Not `--path-format=absolute`: git before 2.31 echoes it back as if it were a path.
    let out = Command::new("git")
        .arg("-C").arg(dir)
        .args(["rev-parse", "--absolute-git-dir", "--git-common-dir"])
        .env("LC_ALL", "C") // the "not a git repository" check reads git's message
        .output()
        .map_err(|e| GitFailed(e.to_string()))?;
    if !out.status.success() {
        let said = String::from_utf8_lossy(&out.stderr);
        return if said.contains("not a git repository") {
            Ok(Checkout::Outside)
        } else {
            Err(GitFailed(said.lines().next().unwrap_or("no message").to_owned()))
        };
    }
    let stdout = String::from_utf8_lossy(&out.stdout);
    let mut lines = stdout.lines();
    // The common dir may be relative to `dir`.
    let (Some(git_dir), Some(common)) = (lines.next(), lines.next()) else {
        return Err(GitFailed(format!("unexpected rev-parse output: {stdout:?}")));
    };
    match (Path::new(git_dir).canonicalize(), dir.join(common).canonicalize()) {
        (Ok(git_dir), Ok(common)) if git_dir == common => Ok(Checkout::Main),
        (Ok(_), Ok(_)) => Ok(Checkout::Linked),
        (Err(e), _) | (_, Err(e)) => Err(GitFailed(format!("can't resolve git dir: {e}"))),
    }
}
