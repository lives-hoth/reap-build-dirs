//! Deleting a dir, or reporting that it would be, and remembering failures.

use std::fmt;
use std::fs;
use std::num::NonZeroU32;
use std::path::PathBuf;

use crate::log;

/// A dir to delete, and why. The reaper deletes whatever it is handed, so build one
/// only where a rule has decided the dir goes.
pub struct Reap {
    pub dir: PathBuf,
    pub reason: Reason,
}

/// Printed in every deletion line, so the set is closed.
pub enum Reason {
    TerraformIdle(NonZeroU32),
}

impl fmt::Display for Reason {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Reason::TerraformIdle(days) => write!(f, "terraform, idle {days}d+"),
        }
    }
}

pub struct Reaper {
    pub dry_run: bool,
    pub failed: bool,
}

impl Reaper {
    /// A failed delete is logged and remembered; the run goes on with the rest.
    pub fn reap(&mut self, Reap { dir, reason }: Reap) {
        let size = log::gib(log::size_kb(&dir));
        if self.dry_run {
            log::line(format_args!("would delete {size} GiB  {}  ({reason})", dir.display()));
        } else if fs::remove_dir_all(&dir).is_ok() {
            log::line(format_args!("deleted {size} GiB  {}  ({reason})", dir.display()));
        } else {
            log::line(format_args!("FAILED  {}", dir.display()));
            self.failed = true;
        }
    }
}
