//! `.terraform` dirs hold downloaded providers and backend settings. With remote
//! state, `terraform init` restores them, so an idle one in a linked worktree can go.

use std::fs;
use std::io::ErrorKind;
use std::num::NonZeroU32;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use walkdir::WalkDir;

use crate::activity::{Activity, Depth, activity};
use crate::git::{self, Checkout, GitFailed};
use crate::log;
use crate::reap::{Reap, Reason, Reaper};

pub fn reap_idle(roots: &[PathBuf], age: NonZeroU32, reaper: &mut Reaper) {
    for root in roots {
        for dir in find(root) {
            match verdict(&dir, age) {
                Ok(r) => reaper.reap(r),
                // Kept. Logged, so a broken repo doesn't look like nothing to do.
                Err(Keep::GitFailed(why)) => log::line(format_args!("skip {}: git failed ({why})", dir.display())),
                Err(_) => {}
            }
        }
    }
}

/// Every `.terraform` dir under `root`, without following links. A part of the tree
/// that can't be read is left unsearched, so nothing in it is deleted.
fn find(root: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut walk = WalkDir::new(root).into_iter();
    while let Some(entry) = walk.next() {
        let Ok(entry) = entry else { continue };
        if !entry.file_type().is_dir() {
            continue;
        }
        match entry.file_name().to_str() {
            Some(".git" | "node_modules") => walk.skip_current_dir(),
            Some(".terraform") => {
                found.push(entry.into_path());
                walk.skip_current_dir();
            }
            _ => {}
        }
    }
    found
}

/// Why a `.terraform` dir is kept.
enum Keep {
    /// Theirs to clean up, and they'd see a failed delete every day.
    OtherUser,
    /// `terraform init` can't restore local state.
    LocalState,
    /// Re-init would quietly switch back to `default`, so a later apply could hit the
    /// wrong environment.
    Workspace,
    /// Its owner or workspace couldn't be read.
    Unreadable,
    /// The main checkout's build state is worth keeping.
    MainCheckout,
    OutsideRepo,
    /// git couldn't tell a worktree from a main checkout.
    GitFailed(String),
    Active,
}

fn verdict(dir: &Path, age: NonZeroU32) -> Result<Reap, Keep> {
    let module = dir.parent().expect("found under a root");
    match fs::symlink_metadata(dir) {
        Ok(m) if m.uid() == rustix::process::geteuid().as_raw() => {}
        Ok(_) => return Err(Keep::OtherUser),
        Err(_) => return Err(Keep::Unreadable),
    }
    if may_exist(&module.join("terraform.tfstate")) || may_exist(&module.join("terraform.tfstate.d")) {
        return Err(Keep::LocalState);
    }
    match fs::read_to_string(dir.join("environment")) {
        Err(e) if e.kind() == ErrorKind::NotFound => {}
        Ok(name) if name.trim_end_matches('\n') == "default" => {}
        Ok(_) => return Err(Keep::Workspace),
        Err(_) => return Err(Keep::Unreadable),
    }
    match git::checkout(module) {
        Ok(Checkout::Linked) => {}
        Ok(Checkout::Main) => return Err(Keep::MainCheckout),
        Ok(Checkout::Outside) => return Err(Keep::OutsideRepo),
        Err(GitFailed(why)) => return Err(Keep::GitFailed(why)),
    }
    // Providers land deep inside .terraform; module edits show at its top level.
    if !matches!(activity(dir, Depth::All, age), Activity::Idle)
        || !matches!(activity(module, Depth::TopLevel, age), Activity::Idle)
    {
        return Err(Keep::Active);
    }
    Ok(Reap { dir: dir.to_owned(), reason: Reason::TerraformIdle(age) })
}

/// False only when the path is certainly absent.
fn may_exist(p: &Path) -> bool {
    !matches!(fs::symlink_metadata(p), Err(e) if e.kind() == ErrorKind::NotFound)
}
