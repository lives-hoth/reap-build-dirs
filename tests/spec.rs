//! The behavior spec for reap-build-dirs: one test per rule of what it deletes
//! and what it keeps.
//!
//! Runs against the built binary, or against any other implementation named by
//! `REAP_BIN` (for example the bash script), so every implementation is held to
//! the same spec. Tests that use command-line flags only run against the binary.
//!
//! An `#[ignore]`d test is decided behavior that is not built yet; its reason says
//! what is missing.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{Duration, SystemTime};

use filetime::FileTime;
use tempfile::TempDir;

/// The implementation under test.
fn bin() -> PathBuf {
    // A cargo build-dir shared between checkouts of this repo can hand this test
    // binary to another checkout, still pointing at that checkout's program. Cargo
    // runs tests from the package root, so compare it with the compiled-in one.
    let here = std::env::current_dir().unwrap().canonicalize().unwrap();
    let built_for = Path::new(env!("CARGO_MANIFEST_DIR")).canonicalize().unwrap();
    assert_eq!(here, built_for, "stale test binary built for another checkout; rerun after `cargo clean`");
    match std::env::var_os("REAP_BIN") {
        Some(p) => PathBuf::from(p),
        None => PathBuf::from(env!("CARGO_BIN_EXE_reap-build-dirs")),
    }
}

fn testing_binary() -> bool {
    std::env::var_os("REAP_BIN").is_none()
}

fn running_as_root() -> bool {
    let out = Command::new("id").arg("-u").output().expect("run id");
    String::from_utf8_lossy(&out.stdout).trim() == "0"
}

/// A throwaway tree of checkouts, temp dirs and build output.
struct Fixture {
    dir: TempDir,
}

impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().expect("create fixture dir");
        let f = Fixture { dir };
        fs::create_dir_all(f.path("git")).unwrap();
        fs::create_dir_all(f.path("tmp")).unwrap();
        f
    }

    fn root(&self) -> PathBuf {
        // Canonical, because the script reports canonical paths (macOS /var -> /private/var).
        self.dir.path().canonicalize().unwrap()
    }

    fn path(&self, rel: &str) -> PathBuf {
        self.root().join(rel)
    }

    fn exists(&self, rel: &str) -> bool {
        self.path(rel).exists()
    }

    fn git(&self, args: &[&str]) {
        let ok = Command::new("git")
            .args(["-c", "init.defaultBranch=main", "-c", "user.name=t", "-c", "user.email=t@t"])
            // Otherwise `commit` starts a detached `git maintenance`, whose lock files
            // can vanish while a test walks the tree to age it.
            .args(["-c", "maintenance.auto=false"])
            .args(args)
            .output()
            .expect("run git")
            .status
            .success();
        assert!(ok, "git {args:?} failed");
    }

    /// A main checkout at `git/<name>`.
    fn main_checkout(&self, name: &str) -> &Self {
        let p = self.path(&format!("git/{name}"));
        fs::create_dir_all(&p).unwrap();
        let p = p.to_str().unwrap().to_owned();
        self.git(&["-C", &p, "init", "-q"]);
        self.git(&["-C", &p, "commit", "-q", "--allow-empty", "-m", "init"]);
        self
    }

    /// A linked worktree of `git/<main>` at `git/<name>`.
    fn worktree(&self, main: &str, name: &str) -> &Self {
        let m = self.path(&format!("git/{main}"));
        let w = self.path(&format!("git/{name}"));
        self.git(&["-C", m.to_str().unwrap(), "worktree", "add", "-q", "--detach", w.to_str().unwrap()]);
        self
    }

    /// A cargo build dir: a marker file at its root plus `debug/deps`.
    fn cargo_dir(&self, rel: &str, marker: &str) -> &Self {
        let d = self.path(rel);
        fs::create_dir_all(d.join("debug/deps")).unwrap();
        fs::write(d.join(marker), "").unwrap();
        fs::write(d.join("debug/deps/libfoo.rlib"), "x").unwrap();
        self
    }

    /// A Terraform module dir with downloaded providers.
    fn tf_dir(&self, module: &str) -> &Self {
        let d = self.path(module).join(".terraform/providers");
        fs::create_dir_all(&d).unwrap();
        fs::write(d.join("provider-bin"), "x").unwrap();
        self
    }

    fn write(&self, rel: &str, contents: &str) -> &Self {
        let p = self.path(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, contents).unwrap();
        self
    }

    /// Set the mtime of everything under `rel` to `days` ago.
    fn age(&self, rel: &str, days: u64) -> &Self {
        self.age_hours(rel, days * 24)
    }

    fn age_hours(&self, rel: &str, hours: u64) -> &Self {
        let t = FileTime::from_system_time(SystemTime::now() - Duration::from_secs(hours * 3600));
        set_mtime_tree(&self.path(rel), t);
        self
    }

    fn run(&self, args: &[&str], env: &[(&str, &str)]) -> Run {
        let mut cmd = Command::new(bin());
        // Isolate from the caller's settings, and never let a run reach real dirs.
        for var in ["REAP_ROOTS", "REAP_TEMP_ROOTS", "REAP_AGE_DAYS", "REAP_TERRAFORM",
                    "REAP_SHARED_BUILD_DIR", "REAP_SHARED_CAP_GB", "DRY_RUN"] {
            cmd.env_remove(var);
        }
        cmd.env("HOME", self.root())
            .env("REAP_ROOTS", self.path("git"))
            .env("REAP_TEMP_ROOTS", self.path("tmp"))
            .envs(env.iter().copied())
            .args(args);
        Run(cmd.output().expect("run implementation"))
    }
}

fn set_mtime_tree(p: &Path, t: FileTime) {
    if p.is_dir() && !p.is_symlink() {
        for e in fs::read_dir(p).unwrap() {
            set_mtime_tree(&e.unwrap().path(), t);
        }
    }
    filetime::set_symlink_file_times(p, t, t).unwrap();
}

/// One reported deletion: `<date> <time> {deleted|would delete} <n> GiB  <path>  (<reason>)`.
#[derive(Debug, PartialEq)]
struct Reaped {
    path: String,
    reason: String,
}

struct Run(Output);

impl Run {
    fn code(&self) -> i32 {
        self.0.status.code().expect("exited normally")
    }
    fn stdout(&self) -> String {
        String::from_utf8_lossy(&self.0.stdout).into_owned()
    }
    fn reaped(&self) -> Vec<Reaped> {
        self.stdout().lines().filter_map(parse_reaped).collect()
    }
    fn reaped_paths(&self) -> Vec<String> {
        self.reaped().into_iter().map(|r| r.path).collect()
    }
}

fn parse_reaped(line: &str) -> Option<Reaped> {
    let rest = line.get(20..)?;
    let rest = rest.strip_prefix("would delete ").or_else(|| rest.strip_prefix("deleted "))?;
    let (_size, rest) = rest.split_once(" GiB  ")?;
    let (path, reason) = rest.rsplit_once("  (")?;
    Some(Reaped { path: path.to_owned(), reason: reason.strip_suffix(')')?.to_owned() })
}

// --- Cargo build dirs -------------------------------------------------------------

#[test]
fn r01_idle_cargo_dir_in_a_worktree_is_deleted() {
    let f = Fixture::new();
    f.main_checkout("repo").worktree("repo", "repo-review")
        .cargo_dir("git/repo-review/target", "CACHEDIR.TAG")
        .cargo_dir("git/repo-review/.cargo-build-pr1", ".rustc_info.json")
        .age("git", 10);
    let run = f.run(&[], &[]);
    assert_eq!(run.code(), 0);
    assert!(!f.exists("git/repo-review/target"));
    assert!(!f.exists("git/repo-review/.cargo-build-pr1"), "found by either marker, any name");
}

#[test]
fn r01_idle_cargo_dir_in_a_temp_root_is_deleted() {
    let f = Fixture::new();
    f.cargo_dir("tmp/job-build", "CACHEDIR.TAG").age("tmp", 10);
    f.run(&[], &[]);
    assert!(!f.exists("tmp/job-build"));
}

#[test]
fn r02_recently_written_cargo_dir_is_kept() {
    let f = Fixture::new();
    f.main_checkout("repo").worktree("repo", "repo-review")
        .cargo_dir("git/repo-review/busy", "CACHEDIR.TAG")
        .age("git", 10)
        .age("git/repo-review/busy/debug/deps/libfoo.rlib", 0);
    f.run(&[], &[]);
    assert!(f.exists("git/repo-review/busy"));
}

#[test]
#[ignore = "not built yet: delete only while holding cargo's build lock"]
fn r03_build_dir_locked_by_a_running_build_is_kept() {
    // Idle by mtime, but cargo holds its lock on it: a build has just started.
    unimplemented!("hold <dir>/debug/.cargo-lock with an flock, then assert the dir is kept");
}

#[test]
fn r04_main_checkout_top_level_target_is_kept() {
    let f = Fixture::new();
    f.main_checkout("repo").cargo_dir("git/repo/target", "CACHEDIR.TAG").age("git", 10);
    f.run(&[], &[]);
    assert!(f.exists("git/repo/target"));
}

#[test]
#[ignore = "not built yet: only the main checkout's top-level target/ is protected"]
fn r05_other_target_dirs_inside_the_main_checkout_are_deleted() {
    let f = Fixture::new();
    f.main_checkout("repo")
        .cargo_dir("git/repo/.review-copy/target", "CACHEDIR.TAG")
        .cargo_dir("git/repo/crates/x/target", "CACHEDIR.TAG")
        .age("git", 10);
    f.run(&[], &[]);
    assert!(!f.exists("git/repo/.review-copy/target"));
    assert!(!f.exists("git/repo/crates/x/target"));
}

#[test]
fn r06_custom_named_build_dir_inside_the_main_checkout_is_deleted() {
    let f = Fixture::new();
    f.main_checkout("repo").cargo_dir("git/repo/.cargo-build", "CACHEDIR.TAG").age("git", 10);
    f.run(&[], &[]);
    assert!(!f.exists("git/repo/.cargo-build"));
}

#[test]
fn r07_marker_without_deps_is_not_a_build_dir() {
    let f = Fixture::new();
    f.write("git/notes/CACHEDIR.TAG", "").age("git", 10);
    f.run(&[], &[]);
    assert!(f.exists("git/notes/CACHEDIR.TAG"));
}

#[test]
fn r07_crate_source_with_a_target_module_is_kept() {
    let f = Fixture::new();
    f.cargo_dir("git/vendor/registry/src/cc-1.0/src/target", "CACHEDIR.TAG").age("git", 10);
    f.run(&[], &[]);
    assert!(f.exists("git/vendor/registry/src/cc-1.0/src/target"));
}

#[test]
fn r07_temp_root_is_searched_only_four_levels_deep() {
    let f = Fixture::new();
    f.cargo_dir("tmp/a/b/c/d/e/deep", "CACHEDIR.TAG").age("tmp", 10);
    f.run(&[], &[]);
    assert!(f.exists("tmp/a/b/c/d/e/deep"));
}

#[test]
fn r07_dir_whose_age_cannot_be_checked_is_kept() {
    if running_as_root() {
        eprintln!("skipped: root can read everything, so this case can't be set up");
        return;
    }
    let f = Fixture::new();
    f.cargo_dir("tmp/unreadable", "CACHEDIR.TAG");
    fs::create_dir(f.path("tmp/unreadable/locked")).unwrap();
    f.age("tmp", 10);
    set_mode(&f.path("tmp/unreadable/locked"), 0o000);
    // Dry run: rm can't remove an unreadable folder either, so only the report
    // shows whether the implementation tried.
    let run = f.run(&["--dry-run"], &[("DRY_RUN", "1")]);
    set_mode(&f.path("tmp/unreadable/locked"), 0o755);
    assert!(run.reaped().is_empty(), "tried to delete: {:?}", run.reaped());
}

#[test]
fn r15_idle_per_workspace_build_dir_under_a_build_root_is_deleted() {
    // `build-dir = "{cargo-cache-home}/build/{workspace-path-hash}"` lays dirs out
    // as <root>/<2 hex>/<hash>, with no checkout to tie them to.
    let f = Fixture::new();
    f.cargo_dir("build/59/a9c965ec3ac137", ".rustc_info.json")
        .cargo_dir("build/b0/7114977524b2ca", "CACHEDIR.TAG")
        .age("build", 10)
        .age("build/b0/7114977524b2ca/debug/deps/libfoo.rlib", 0);
    let roots = format!("{}:{}", f.path("git").display(), f.path("build").display());
    f.run(&[], &[("REAP_ROOTS", &roots)]);
    assert!(!f.exists("build/59/a9c965ec3ac137"));
    assert!(f.exists("build/b0/7114977524b2ca"), "a recently used workspace's dir is kept");
}

// --- Terraform -----------------------------------------------------------------

#[test]
fn r08_idle_terraform_dir_in_a_worktree_is_deleted() {
    let f = Fixture::new();
    f.main_checkout("repo").worktree("repo", "repo-review").tf_dir("git/repo-review/infra").age("git", 10);
    f.run(&[], &[]);
    assert!(!f.exists("git/repo-review/infra/.terraform"));
}

#[test]
fn r09_terraform_dir_in_the_main_checkout_is_kept() {
    let f = Fixture::new();
    f.main_checkout("repo").tf_dir("git/repo/infra").age("git", 10);
    f.run(&[], &[]);
    assert!(f.exists("git/repo/infra/.terraform"));
}

#[test]
fn r09_terraform_dir_with_local_state_is_kept() {
    let f = Fixture::new();
    f.main_checkout("repo").worktree("repo", "repo-review")
        .tf_dir("git/repo-review/state-file").write("git/repo-review/state-file/terraform.tfstate", "{}")
        .tf_dir("git/repo-review/state-dir").write("git/repo-review/state-dir/terraform.tfstate.d/dev/terraform.tfstate", "{}")
        .age("git", 10);
    f.run(&[], &[]);
    assert!(f.exists("git/repo-review/state-file/.terraform"));
    assert!(f.exists("git/repo-review/state-dir/.terraform"));
}

#[test]
fn r09_terraform_dir_with_a_non_default_workspace_is_kept() {
    let f = Fixture::new();
    f.main_checkout("repo").worktree("repo", "repo-review")
        .tf_dir("git/repo-review/infra").write("git/repo-review/infra/.terraform/environment", "staging")
        .age("git", 10);
    f.run(&[], &[]);
    assert!(f.exists("git/repo-review/infra/.terraform"));
}

#[test]
fn r10_terraform_dir_outside_any_git_repo_is_kept() {
    let f = Fixture::new();
    f.tf_dir("git/loose").age("git", 10);
    f.run(&[], &[]);
    assert!(f.exists("git/loose/.terraform"));
}

#[test]
fn r10_terraform_can_be_switched_off() {
    let f = Fixture::new();
    f.main_checkout("repo").worktree("repo", "repo-review").tf_dir("git/repo-review/infra").age("git", 10);
    f.run(&[], &[("REAP_TERRAFORM", "0")]);
    assert!(f.exists("git/repo-review/infra/.terraform"));
}

// --- Shared build-dir ----------------------------------------------------------

#[test]
fn r11_shared_build_dir_under_the_cap_is_kept() {
    let f = Fixture::new();
    f.cargo_dir("shared", ".rustc_info.json").age("shared", 10);
    let shared = f.path("shared");
    f.run(&[], &[("REAP_SHARED_BUILD_DIR", shared.to_str().unwrap()), ("REAP_SHARED_CAP_GB", "1000")]);
    assert!(f.exists("shared"));
}

#[test]
fn r11_shared_build_dir_is_not_reaped_as_an_ordinary_build_dir() {
    // Under ~/git it would also match the cargo search; only the cap may remove it.
    let f = Fixture::new();
    f.cargo_dir("git/shared", ".rustc_info.json").age("git", 10);
    let shared = f.path("git/shared");
    f.run(&[], &[("REAP_SHARED_BUILD_DIR", shared.to_str().unwrap())]);
    assert!(f.exists("git/shared"));
}

#[test]
#[ignore = "not built yet: wipe over the cap unless cargo holds the dir's lock"]
fn r11_shared_build_dir_over_the_cap_is_wiped_unless_locked() {
    // Today this depends on whether any cargo process runs on the machine, so it
    // can't be tested deterministically.
    unimplemented!("wiped when unlocked; kept with a 'retry next run' line when locked");
}

// --- Settings ------------------------------------------------------------------

#[test]
fn r12_invalid_age_stops_before_deleting_anything() {
    for bad in ["3d", "three", "-1", "0"] {
        let f = Fixture::new();
        f.main_checkout("repo").worktree("repo", "repo-review")
            .cargo_dir("git/repo-review/target", "CACHEDIR.TAG").age("git", 10);
        let run = f.run(&[], &[("REAP_AGE_DAYS", bad)]);
        assert_eq!(run.code(), 2, "REAP_AGE_DAYS={bad}");
        assert!(f.exists("git/repo-review/target"), "REAP_AGE_DAYS={bad} deleted");
    }
}

#[test]
fn r12_invalid_shared_cap_stops_before_deleting_anything() {
    let f = Fixture::new();
    f.cargo_dir("tmp/job", "CACHEDIR.TAG").age("tmp", 10);
    let run = f.run(&[], &[("REAP_SHARED_CAP_GB", "lots")]);
    assert_eq!(run.code(), 2);
    assert!(f.exists("tmp/job"));
}

#[test]
fn r12_shared_cap_too_large_to_compute_stops_before_deleting_anything() {
    // In bash, cap × 1048576 KB wraps past 2^63 to a negative limit, so "never
    // wipe" turned into "always wipe".
    let f = Fixture::new();
    f.cargo_dir("shared", ".rustc_info.json").age("shared", 10);
    let shared = f.path("shared");
    let run = f.run(&[], &[("REAP_SHARED_BUILD_DIR", shared.to_str().unwrap()),
                            ("REAP_SHARED_CAP_GB", "9999999999999")]);
    assert_eq!(run.code(), 2, "{}", run.stdout());
    assert!(f.exists("shared"));
}

#[test]
fn r12_flag_overrides_environment_variable() {
    if !testing_binary() {
        return;
    }
    let f = Fixture::new();
    f.cargo_dir("tmp/job", "CACHEDIR.TAG").age("tmp", 3);
    // The variable alone would delete a 3-day-old dir; the flag says 5 days.
    f.run(&["--age-days", "5"], &[("REAP_AGE_DAYS", "1")]);
    assert!(f.exists("tmp/job"));
    f.run(&[], &[("REAP_AGE_DAYS", "1")]);
    assert!(!f.exists("tmp/job"));
}

#[test]
fn r12_invalid_flag_stops_before_deleting_anything() {
    if !testing_binary() {
        return;
    }
    let f = Fixture::new();
    f.cargo_dir("tmp/job", "CACHEDIR.TAG").age("tmp", 10);
    let run = f.run(&["--age-days", "0"], &[]);
    assert_eq!(run.code(), 2);
    assert!(f.exists("tmp/job"));
}

// --- Failures, dry run, report -------------------------------------------------

#[test]
fn r13_failed_delete_is_reported() {
    if running_as_root() {
        eprintln!("skipped: root can delete anything");
        return;
    }
    let f = Fixture::new();
    f.cargo_dir("tmp/stuck", "CACHEDIR.TAG").write("tmp/stuck/sealed/file", "x").age("tmp", 10);
    set_mode(&f.path("tmp/stuck/sealed"), 0o555);
    let run = f.run(&[], &[]);
    set_mode(&f.path("tmp/stuck/sealed"), 0o755);
    assert!(run.stdout().contains("FAILED"), "{}", run.stdout());
}

#[test]
fn r13_failed_cargo_delete_exits_nonzero_after_finishing() {
    if running_as_root() {
        eprintln!("skipped: root can delete anything");
        return;
    }
    let f = Fixture::new();
    f.cargo_dir("tmp/stuck", "CACHEDIR.TAG").write("tmp/stuck/sealed/file", "x")
        .cargo_dir("tmp/fine", "CACHEDIR.TAG")
        .age("tmp", 10);
    set_mode(&f.path("tmp/stuck/sealed"), 0o555);
    let run = f.run(&[], &[]);
    set_mode(&f.path("tmp/stuck/sealed"), 0o755);
    assert_eq!(run.code(), 1, "{}", run.stdout());
    assert!(!f.exists("tmp/fine"), "one failure must not stop the other deletes");
    assert!(run.stdout().lines().last().is_some_and(|l| l.contains(" done (free ")));
}

#[test]
fn r13_failed_terraform_delete_exits_nonzero() {
    if running_as_root() {
        eprintln!("skipped: root can delete anything");
        return;
    }
    let f = Fixture::new();
    f.main_checkout("repo").worktree("repo", "repo-review")
        .tf_dir("git/repo-review/infra").write("git/repo-review/infra/.terraform/sealed/file", "x")
        .age("git", 10);
    set_mode(&f.path("git/repo-review/infra/.terraform/sealed"), 0o555);
    let run = f.run(&[], &[]);
    set_mode(&f.path("git/repo-review/infra/.terraform/sealed"), 0o755);
    assert_eq!(run.code(), 1, "{}", run.stdout());
}

#[test]
fn r13_successful_run_exits_zero() {
    let f = Fixture::new();
    f.cargo_dir("tmp/fine", "CACHEDIR.TAG").age("tmp", 10);
    assert_eq!(f.run(&[], &[]).code(), 0);
    assert!(!f.exists("tmp/fine"));
}

#[test]
fn r14_dry_run_changes_nothing_but_reports_the_plan() {
    let f = Fixture::new();
    f.main_checkout("repo").worktree("repo", "repo-review")
        .cargo_dir("git/repo-review/target", "CACHEDIR.TAG")
        .tf_dir("git/repo-review/infra")
        .age("git", 10);
    for (args, env) in [(vec!["--dry-run"], vec![]), (vec![], vec![("DRY_RUN", "1")])] {
        let run = f.run(&args, &env);
        assert_eq!(run.code(), 0);
        assert!(f.exists("git/repo-review/target") && f.exists("git/repo-review/infra/.terraform"));
        let target = f.path("git/repo-review/target").to_str().unwrap().to_owned();
        let tf = f.path("git/repo-review/infra/.terraform").to_str().unwrap().to_owned();
        let mut paths = run.reaped_paths();
        paths.sort();
        assert_eq!(paths, vec![tf, target]);
    }
}

#[test]
fn report_lines_have_the_stable_format() {
    let f = Fixture::new();
    f.cargo_dir("tmp/job", "CACHEDIR.TAG").age("tmp", 10);
    let run = f.run(&[], &[]);
    let lines: Vec<String> = run.stdout().lines().map(str::to_owned).collect();
    assert!(lines.first().is_some_and(|l| l.contains(" start (free ")), "{lines:?}");
    assert!(lines.last().is_some_and(|l| l.contains(" done (free ")), "{lines:?}");
    for l in &lines {
        let ts = &l[..19];
        let shape = ts.char_indices().all(|(i, c)| match i {
            4 | 7 => c == '-',
            10 => c == ' ',
            13 | 16 => c == ':',
            _ => c.is_ascii_digit(),
        });
        assert!(shape && l.as_bytes()[19] == b' ', "timestamp: {l}");
    }
    assert_eq!(
        run.reaped(),
        vec![Reaped { path: f.path("tmp/job").to_str().unwrap().to_owned(), reason: "cargo, idle 3d+".into() }]
    );
}

// --- Environment the reaper can't assume ---------------------------------------

#[test]
fn r16_refuses_to_start_when_git_cannot_run() {
    // Without git a main checkout can't be recognized, so its target/ would lose
    // its protection. Covers git missing and git present but broken (as after a
    // macOS update breaks the command line tools).
    for broken_git in [None, Some("#!/bin/sh\necho 'xcrun: error: invalid active developer path' >&2\nexit 1\n")] {
        let f = Fixture::new();
        f.main_checkout("repo").cargo_dir("git/repo/target", "CACHEDIR.TAG")
            .cargo_dir("tmp/job", "CACHEDIR.TAG")
            .age("git", 10).age("tmp", 10);
        let path = path_without(&f, "git", broken_git);
        let run = f.run(&[], &[("PATH", &path)]);
        assert_eq!(run.code(), 2, "broken_git={}: {}", broken_git.is_some(), run.stdout());
        assert!(f.exists("git/repo/target") && f.exists("tmp/job"), "deleted without git");
    }
}

#[test]
fn r17_another_users_build_dir_is_skipped_silently() {
    let f = Fixture::new();
    f.cargo_dir("tmp/theirs", "CACHEDIR.TAG").cargo_dir("tmp/mine", "CACHEDIR.TAG").age("tmp", 10);
    if !as_superuser(&["chown", "-R", "65534", f.path("tmp/theirs").to_str().unwrap()]) {
        eprintln!("skipped: needs root or passwordless sudo to create another user's dir");
        return;
    }
    let run = f.run(&[], &[]);
    as_superuser(&["chown", "-R", &uid(), f.path("tmp/theirs").to_str().unwrap()]);
    assert_eq!(run.code(), 0, "{}", run.stdout());
    assert!(f.exists("tmp/theirs"));
    assert!(!f.exists("tmp/mine"));
    assert!(!run.stdout().contains("tmp/theirs"), "mentioned: {}", run.stdout());
}

#[test]
fn r17_another_users_terraform_dir_is_skipped_silently() {
    let f = Fixture::new();
    f.main_checkout("repo").worktree("repo", "repo-review")
        .tf_dir("git/repo-review/theirs").tf_dir("git/repo-review/mine")
        .age("git", 10);
    let theirs = f.path("git/repo-review/theirs/.terraform");
    if !as_superuser(&["chown", "-R", "65534", theirs.to_str().unwrap()]) {
        eprintln!("skipped: needs root or passwordless sudo to create another user's dir");
        return;
    }
    let run = f.run(&[], &[]);
    as_superuser(&["chown", "-R", &uid(), theirs.to_str().unwrap()]);
    assert_eq!(run.code(), 0, "{}", run.stdout());
    assert!(f.exists("git/repo-review/theirs/.terraform"));
    assert!(!f.exists("git/repo-review/mine/.terraform"));
}

#[test]
fn r18_idle_means_no_write_for_the_full_number_of_days() {
    // Same boundary on every OS: BSD and GNU find round -mtime differently.
    let f = Fixture::new();
    f.cargo_dir("tmp/h70", "CACHEDIR.TAG").cargo_dir("tmp/h74", "CACHEDIR.TAG")
        .age_hours("tmp/h70", 70).age_hours("tmp/h74", 74);
    f.run(&[], &[("REAP_AGE_DAYS", "3")]);
    assert!(f.exists("tmp/h70"), "deleted before 3 full days");
    assert!(!f.exists("tmp/h74"));
}

#[test]
fn r19_links_are_not_followed_out_of_a_root() {
    let f = Fixture::new();
    f.cargo_dir("outside/target", "CACHEDIR.TAG").age("outside", 10);
    std::os::unix::fs::symlink(f.path("outside"), f.path("git/link")).unwrap();
    f.run(&[], &[]);
    assert!(f.exists("outside/target"));
}

#[test]
fn r20_paths_with_spaces_are_handled() {
    let f = Fixture::new();
    f.cargo_dir("tmp/my build", "CACHEDIR.TAG").age("tmp", 10);
    let run = f.run(&[], &[]);
    assert!(!f.exists("tmp/my build"));
    assert_eq!(run.reaped_paths(), vec![f.path("tmp/my build").to_str().unwrap().to_owned()]);
}

#[test]
fn r21_nested_build_dirs() {
    // All idle: the outer dir goes, and the inner one is not reported separately.
    let f = Fixture::new();
    f.cargo_dir("tmp/outer", "CACHEDIR.TAG").cargo_dir("tmp/outer/inner", "CACHEDIR.TAG").age("tmp", 10);
    let run = f.run(&[], &[]);
    assert_eq!(run.code(), 0);
    assert_eq!(run.reaped_paths(), vec![f.path("tmp/outer").to_str().unwrap().to_owned()]);
    // Inner written recently: the whole outer tree counts as in use.
    let f = Fixture::new();
    f.cargo_dir("tmp/outer", "CACHEDIR.TAG").cargo_dir("tmp/outer/inner", "CACHEDIR.TAG")
        .age("tmp", 10).age("tmp/outer/inner/debug/deps/libfoo.rlib", 0);
    f.run(&[], &[]);
    assert!(f.exists("tmp/outer/inner"));
}

#[test]
fn r23_worktree_nested_inside_the_main_checkout_is_a_worktree() {
    // Review worktrees often live inside the main checkout (`repo/.worktrees/pr1`).
    // Being under the main checkout's path doesn't make their target/ the main one.
    let f = Fixture::new();
    f.main_checkout("repo").worktree("repo", "repo/.worktrees/pr1")
        .cargo_dir("git/repo/target", "CACHEDIR.TAG")
        .cargo_dir("git/repo/.worktrees/pr1/target", "CACHEDIR.TAG")
        .age("git", 10);
    let run = f.run(&[], &[]);
    assert_eq!(run.code(), 0, "{}", run.stdout());
    assert!(f.exists("git/repo/target"), "main checkout's target/ deleted");
    assert!(!f.exists("git/repo/.worktrees/pr1/target"), "nested worktree's target/ kept");
}

#[test]
#[cfg(target_os = "macos")]
fn r22_macos_user_temp_dir_is_searched_without_tmpdir() {
    // cron or a wrapper script may start the reaper without TMPDIR, and the per-user
    // temp dir's path differs on every Mac, so it must be found without either. With
    // no REAP_TEMP_ROOTS this searches the real temp dirs: dry run, and an `rm` that
    // always fails, so a broken dry run can't delete anything real.
    let out = Command::new("getconf").arg("DARWIN_USER_TEMP_DIR").output().unwrap();
    let user_tmp = String::from_utf8(out.stdout).unwrap();
    let dir = tempfile::tempdir_in(user_tmp.trim()).unwrap();
    let job = dir.path().canonicalize().unwrap().join("job");
    fs::create_dir_all(job.join("debug/deps")).unwrap();
    fs::write(job.join("CACHEDIR.TAG"), "").unwrap();
    set_mtime_tree(&job, FileTime::from_system_time(SystemTime::now() - Duration::from_secs(10 * 86400)));
    let git = dir.path().join("git");
    fs::create_dir_all(&git).unwrap();

    let no_rm = Fixture::new();
    let path = path_without(&no_rm, "rm", Some("#!/bin/sh\nexit 1\n"));
    let mut cmd = Command::new(bin());
    cmd.env("PATH", path);
    for var in ["REAP_TEMP_ROOTS", "TMPDIR", "REAP_AGE_DAYS", "REAP_TERRAFORM",
                "REAP_SHARED_BUILD_DIR", "REAP_SHARED_CAP_GB", "DRY_RUN"] {
        cmd.env_remove(var);
    }
    let run = Run(cmd.env("REAP_ROOTS", &git).arg("--dry-run").output().unwrap());
    assert_eq!(run.code(), 0, "{}", run.stdout());
    assert!(job.exists());
    assert!(run.reaped_paths().contains(&job.to_str().unwrap().to_owned()), "{}", run.stdout());
}

/// A PATH holding every command on the current PATH except `cmd`, plus, when
/// given, a `cmd` that runs this script instead.
fn path_without(f: &Fixture, cmd: &str, replacement: Option<&str>) -> String {
    use std::os::unix::fs::PermissionsExt;
    let bin = f.path("bin");
    fs::create_dir_all(&bin).unwrap();
    for dir in std::env::split_paths(&std::env::var_os("PATH").unwrap()) {
        let Ok(entries) = fs::read_dir(&dir) else { continue };
        for e in entries.flatten() {
            let name = e.file_name();
            if name == cmd || bin.join(&name).exists() {
                continue;
            }
            let _ = std::os::unix::fs::symlink(e.path(), bin.join(&name));
        }
    }
    if let Some(script) = replacement {
        fs::write(bin.join(cmd), script).unwrap();
        fs::set_permissions(bin.join(cmd), fs::Permissions::from_mode(0o755)).unwrap();
    }
    bin.to_str().unwrap().to_owned()
}

fn uid() -> String {
    let out = Command::new("id").arg("-u").output().expect("run id");
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

/// Run a command as root: directly when already root, else via `sudo -n`.
fn as_superuser(cmd: &[&str]) -> bool {
    let mut c = if running_as_root() {
        let mut c = Command::new(cmd[0]);
        c.args(&cmd[1..]);
        c
    } else {
        let mut c = Command::new("sudo");
        c.arg("-n").args(cmd);
        c
    };
    c.output().is_ok_and(|o| o.status.success())
}

fn set_mode(p: &Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(p, fs::Permissions::from_mode(mode)).unwrap();
}
