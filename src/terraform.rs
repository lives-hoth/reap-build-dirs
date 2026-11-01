//! `.terraform` dirs hold downloaded providers and backend settings. With remote
//! state, `terraform init` restores them, so an idle one in a linked worktree can go.

use std::fs;
use std::io::ErrorKind;
use std::num::NonZeroU32;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use walkdir::WalkDir;

use crate::activity::{Activity, activity};
use crate::git::{self, Checkout};
use crate::reap::{Reap, Reason, Reaper};

pub fn reap_idle(roots: &[PathBuf], age: NonZeroU32, reaper: &mut Reaper) {
    for root in roots {
        for dir in find(root) {
            if let Ok(r) = verdict(dir, age) {
                reaper.reap(r);
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
    /// A main checkout's, outside any repo, or git couldn't say.
    NotLinked,
    Active,
}

fn verdict(dir: PathBuf, age: NonZeroU32) -> Result<Reap, Keep> {
    let module = dir.parent().expect("found under a root").to_owned();
    if !fs::symlink_metadata(&dir).is_ok_and(|m| m.uid() == rustix::process::geteuid().as_raw()) {
        return Err(Keep::OtherUser);
    }
    if may_exist(&module.join("terraform.tfstate")) || may_exist(&module.join("terraform.tfstate.d")) {
        return Err(Keep::LocalState);
    }
    match fs::read_to_string(dir.join("environment")) {
        Err(e) if e.kind() == ErrorKind::NotFound => {}
        Ok(name) if name.trim_end_matches('\n') == "default" => {}
        _ => return Err(Keep::Workspace),
    }
    if !matches!(git::checkout(&module), Ok(Checkout::Linked)) {
        return Err(Keep::NotLinked);
    }
    // Providers land deep inside .terraform; module edits show at its top level.
    if !matches!(activity(&dir, usize::MAX, age), Activity::Idle)
        || !matches!(activity(&module, 1, age), Activity::Idle)
    {
        return Err(Keep::Active);
    }
    Ok(Reap { dir, reason: Reason::TerraformIdle(age) })
}

/// False only when the path is certainly absent.
fn may_exist(p: &Path) -> bool {
    !matches!(fs::symlink_metadata(p), Err(e) if e.kind() == ErrorKind::NotFound)
}
