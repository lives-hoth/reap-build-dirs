//! Whether anything in a dir was written recently.

use std::num::NonZeroU32;
use std::path::Path;
use std::time::{Duration, SystemTime};

use walkdir::WalkDir;

pub enum Activity {
    Recent,
    Idle,
    /// Part of the dir couldn't be inspected. Never deleted: it may hide a recent write.
    Unknown,
}

/// How far below a dir to look for writes.
pub enum Depth {
    /// The dir and its direct entries.
    TopLevel,
    All,
}

/// Looks at `dir` and below it to `depth`, without following links. Idle means nothing
/// was written for the full `age` in days.
pub fn activity(dir: &Path, depth: Depth, age: NonZeroU32) -> Activity {
    let Some(cutoff) = SystemTime::now().checked_sub(Duration::from_secs(u64::from(age.get()) * 86_400)) else {
        return Activity::Recent; // older than the clock can express: nothing is that old
    };
    let max_depth = match depth {
        Depth::TopLevel => 1,
        Depth::All => usize::MAX,
    };
    for entry in WalkDir::new(dir).max_depth(max_depth) {
        let Ok(modified) = entry.map_err(drop).and_then(|e| e.metadata().map_err(drop))
            .and_then(|m| m.modified().map_err(drop))
        else {
            return Activity::Unknown;
        };
        if modified > cutoff {
            return Activity::Recent;
        }
    }
    Activity::Idle
}
