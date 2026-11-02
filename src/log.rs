//! The run's report on stdout. Its line format is a stable contract:
//! `<date> <time> <message>`, with deletions as
//! `{deleted|would delete} <n> GiB  <path>  (<reason>)`.

use std::fmt::Display;
use std::path::Path;

use walkdir::WalkDir;

pub fn line(msg: impl Display) {
    println!("{} {msg}", jiff::Zoned::now().strftime("%F %T"));
}

pub fn gib(kb: u64) -> String {
    format!("{:.1}", kb as f64 / 1_048_576.0) // display only: no decision reads this value
}

/// Free space on the filesystem holding `dir`, as `df -Pk` reports it.
pub fn free_gib(dir: &Path) -> String {
    let kb = rustix::fs::statvfs(dir).map_or(0, |s| s.f_bavail.saturating_mul(s.f_frsize) / 1024);
    gib(kb)
}

/// Disk usage of `dir` in KB, as `du -sk` reports it. Unreadable parts count as empty.
pub fn size_kb(dir: &Path) -> u64 {
    use std::os::unix::fs::MetadataExt;
    WalkDir::new(dir)
        .into_iter()
        .filter_map(|e| e.ok()?.metadata().ok())
        .map(|m| m.blocks() / 2)
        .sum()
}
