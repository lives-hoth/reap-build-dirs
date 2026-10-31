//! Command-line front end for reap-build-dirs.
//!
//! The deletion logic still lives in the bash script, which is compiled into the
//! binary and run with the parsed settings. Each setting can come from a flag or
//! from its `REAP_*` environment variable; a flag wins over the variable, and the
//! variable over the default.

use std::num::NonZeroU32;
use std::os::unix::process::CommandExt;
use std::process::{Command, ExitCode};

use clap::Parser;

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

    /// Leave .terraform dirs alone
    #[arg(long)]
    no_terraform: bool,

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

    let mut script = Command::new("bash");
    script.arg("-c").arg(SCRIPT).arg("reap-build-dirs");
    if args.dry_run {
        script.arg("--dry-run");
    }

    // Hand the script exactly the parsed settings, so a flag overrides a
    // conflicting variable inherited from the environment.
    script
        .env_remove("DRY_RUN")
        .env("REAP_AGE_DAYS", args.age_days.to_string())
        .env("REAP_SHARED_CAP_GB", args.shared_cap_gb.to_string())
        .env("REAP_TEMP_ROOTS", args.temp_roots.unwrap_or_else(default_temp_roots));
    for (name, value) in [
        ("REAP_ROOTS", &args.roots),
        ("REAP_SHARED_BUILD_DIR", &args.shared_build_dir),
    ] {
        match value {
            Some(v) => script.env(name, v),
            None => script.env_remove(name),
        };
    }
    if args.no_terraform {
        script.env("REAP_TERRAFORM", "0");
    }

    let err = script.exec();
    eprintln!("reap-build-dirs: could not run bash: {err}");
    ExitCode::from(127)
}

/// `/tmp`, `$TMPDIR`, and on macOS the per-user temp dir, found directly because a
/// scheduler may start the reaper without `TMPDIR`. Roots that don't exist are skipped.
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
