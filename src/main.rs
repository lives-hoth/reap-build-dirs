//! Command-line front end for reap-build-dirs.
//!
//! The binary owns the report and the `.terraform` rules. Cargo build dirs and the
//! shared build-dir are still handled by the bash script, which is compiled into the
//! binary and run first with the parsed settings. Each setting can come from a flag
//! or from its `REAP_*` environment variable; a flag wins over the variable, and the
//! variable over the default.

mod activity;
mod git;
mod log;
mod reap;
mod terraform;

use std::collections::BTreeSet;
use std::num::NonZeroU32;
use std::path::PathBuf;
use std::process::{Command, ExitCode};

use clap::Parser;

use crate::reap::Reaper;

const SCRIPT: &str = include_str!("../reap-build-dirs");

/// Delete stale cargo and Terraform build output from worktrees and temp dirs.
#[derive(Parser, Debug)]
#[command(version, about)]
struct Args {
    /// Report what would be deleted without deleting anything
    // Boolish so `DRY_RUN=1` works; an unrecognised value is an error, never a real run.
    #[arg(short = 'n', long, env = "DRY_RUN",
          value_parser = clap::builder::BoolishValueParser::new())]
    dry_run: bool,

    /// Colon-separated dirs where your checkouts live [default: $HOME/git]
    #[arg(long, env = "REAP_ROOTS")]
    roots: Option<String>,

    /// Colon-separated temp dirs, searched 4 levels deep
    /// [default: /tmp, $TMPDIR and, on macOS, the per-user temp dir]
    #[arg(long, env = "REAP_TEMP_ROOTS")]
    temp_roots: Option<String>,

    /// Days without writes before a dir is deleted
    // NonZero: an age of 0 would make every dir idle (docs/rust-type-design.md).
    #[arg(long, env = "REAP_AGE_DAYS", default_value_t = NonZeroU32::new(3).unwrap())]
    age_days: NonZeroU32,

    /// Leave .terraform dirs alone [env: REAP_TERRAFORM=0]
    #[arg(long)]
    no_terraform: bool,

    /// `REAP_TERRAFORM=0` is the same as --no-terraform
    #[arg(long, env = "REAP_TERRAFORM", hide = true, default_value = "1",
          value_parser = zero_or_one, action = clap::ArgAction::Set)]
    terraform: bool,

    /// A cargo build-dir shared across workspaces, wiped whole past the cap
    #[arg(long, env = "REAP_SHARED_BUILD_DIR")]
    shared_build_dir: Option<String>,

    /// Size in GB at which the shared build-dir is wiped
    // Bounded to match the script, whose 64-bit arithmetic overflows past this.
    #[arg(long, env = "REAP_SHARED_CAP_GB", default_value_t = 100,
          value_parser = clap::value_parser!(u32).range(..=999_999_999))]
    shared_cap_gb: u32,
}

fn main() -> ExitCode {
    let args = Args::parse();
    if !git::runnable() {
        eprintln!("reap-build-dirs: git must be runnable to recognize main checkouts ('git --version' failed)");
        return ExitCode::from(2);
    }
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let free = || log::free_gib(home.as_deref().unwrap_or("/".as_ref()));
    // Empty means the default, as in the script.
    let roots = match args.roots.as_deref().filter(|r| !r.is_empty()) {
        Some(list) => existing_dirs(list),
        None => existing_dirs(&home.as_ref().map(|h| h.join("git").display().to_string()).unwrap_or_default()),
    };
    let mut reaper = Reaper { dry_run: args.dry_run, failed: false };
    log::line(format_args!("start (free {} GiB, age {}d, dry_run={})", free(), args.age_days, u8::from(args.dry_run)));

    match run_script(&args).status() {
        Ok(status) => match status.code() {
            Some(0) => {}
            Some(1) => reaper.failed = true,
            // The script refused to run: stop before anything else is deleted.
            Some(code) => return ExitCode::from(u8::try_from(code).unwrap_or(1)),
            None => return ExitCode::FAILURE,
        },
        Err(err) => {
            eprintln!("reap-build-dirs: could not run bash: {err}");
            return ExitCode::from(127);
        }
    }

    if args.terraform && !args.no_terraform {
        terraform::reap_idle(&roots, args.age_days, &mut reaper);
    }

    log::line(format_args!("done (free {} GiB)", free()));
    // Finish every delete first, then report failure so a scheduler shows it.
    if reaper.failed { ExitCode::FAILURE } else { ExitCode::SUCCESS }
}

/// The script, with exactly the parsed settings, so a flag overrides a conflicting
/// variable inherited from the environment. It leaves `.terraform` dirs and the
/// start and done lines to the binary.
fn run_script(args: &Args) -> Command {
    let mut script = Command::new("bash");
    script.arg("-c").arg(SCRIPT).arg("reap-build-dirs");
    if args.dry_run {
        script.arg("--dry-run");
    }
    script
        .env_remove("DRY_RUN")
        .env("REAP_FROM_BINARY", "1")
        .env("REAP_TERRAFORM", "0")
        .env("REAP_AGE_DAYS", args.age_days.to_string())
        .env("REAP_SHARED_CAP_GB", args.shared_cap_gb.to_string())
        .env("REAP_TEMP_ROOTS", args.temp_roots.clone().unwrap_or_else(default_temp_roots));
    for (name, value) in [
        ("REAP_ROOTS", &args.roots),
        ("REAP_SHARED_BUILD_DIR", &args.shared_build_dir),
    ] {
        match value {
            Some(v) => script.env(name, v),
            None => script.env_remove(name),
        };
    }
    script
}

/// `1` or `0`; empty means the default, as in the script.
fn zero_or_one(v: &str) -> Result<bool, String> {
    match v {
        "1" | "" => Ok(true),
        "0" => Ok(false),
        _ => Err(String::from("must be 0 or 1")),
    }
}

/// The dirs in a colon-separated list that exist, resolved and without duplicates.
fn existing_dirs(list: &str) -> Vec<PathBuf> {
    let dirs: BTreeSet<PathBuf> = list.split(':')
        .filter(|d| !d.is_empty())
        .filter_map(|d| std::fs::canonicalize(d).ok())
        .filter(|d| d.is_dir())
        .collect();
    dirs.into_iter().collect()
}

/// `/tmp`, `$TMPDIR`, and on macOS the per-user temp dir, found directly because cron
/// or a wrapper script may start the reaper without `TMPDIR`. Missing roots are skipped.
fn default_temp_roots() -> String {
    let mut roots = vec![String::from("/tmp")];
    roots.extend(std::env::var("TMPDIR").ok());
    if cfg!(target_os = "macos") {
        // If getconf fails this root is left out: fewer dirs searched, none deleted wrongly.
        if let Ok(out) = Command::new("getconf").arg("DARWIN_USER_TEMP_DIR").output()
            && out.status.success()
        {
            roots.push(String::from_utf8_lossy(&out.stdout).trim().to_owned());
        }
    }
    roots.join(":")
}
