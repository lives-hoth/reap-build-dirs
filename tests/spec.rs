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

const DAY: u64 = 86_400;

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
        let t = FileTime::from_system_time(SystemTime::now() - Duration::from_secs(days * DAY));
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

fn set_mode(p: &Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(p, fs::Permissions::from_mode(mode)).unwrap();
}
