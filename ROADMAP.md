# Roadmap

`reap-build-dirs` is moving from a bash script to a Rust binary one piece at a time.
The binary owns settings, the report and the `.terraform` rules, and runs the bash
script for cargo build dirs and the shared build-dir.
Each step replaces one piece, test-first: un-ignore or add its test in `tests/spec.rs`,
watch it fail, implement it in Rust, and keep the whole spec green against both the
binary and the script (`REAP_BIN`) until the script is gone.

`tests/spec.rs` is the specification. This file only orders the work. Rust code
follows [`docs/rust-type-design.md`](docs/rust-type-design.md): every slice's types
should make the "delete it" answer impossible to reach by accident.

## Done

- **Settings** — clap flags, each overridable by its `REAP_*` variable; a flag wins
  over the variable, the variable over the default. Invalid values stop the run
  before anything is deleted.
- **Exit code** (`r13_…`) — exit 1 when any delete failed, after finishing the rest.
  The binary decides it from its own deletes and the script's exit code.
- **Environment it can't assume** (`r16_…` to `r21_…`) — refuse to start when git can't
  run; skip other users' dirs; pin the idle boundary, link handling, spaces and nested
  dirs so the Rust discovery has to reproduce them. Worktrees nested inside a main
  checkout are worktrees (`r23_…`).
- **Main checkout protection** (`r05_…`) — only the main checkout's top-level `target/`
  is kept; other build dirs inside it are deleted when idle.
- **Terraform** — discovery and keep rules are in Rust; the script runs with
  `REAP_TERRAFORM=0`. A write anywhere inside `.terraform` now keeps it (`r02_…`), not
  only at its top level.
- **macOS temp dir** (`r22_…`) — the per-user temp dir is found with
  `getconf DARWIN_USER_TEMP_DIR`, not only from `$TMPDIR`, which cron or a wrapper
  script may not set (launchd does).

## Next

1. **Shared build-dir** (`r11_…_unless_locked`) — move the size-cap wipe into Rust,
   deciding "in use" by cargo's lock instead of looking for cargo processes.
2. **Cargo build dirs** — move discovery into Rust with
   [`walkdir`](https://docs.rs/walkdir): depth limits, pruning `.git`, `node_modules`,
   `deps` and `incremental`, no symlink following. Treat any walk error as "can't
   tell, keep it", never skip it. Make `terraform::find` the one walk for both, with
   cargo's markers as its matcher, and share `git::checkout` (cached per worktree)
   for the main checkout's `target/`.
3. **Remove the script** — the binary owns every rule; drop `REAP_BIN` from CI.
4. **Locks and atomic deletes** (`r03_…`, `r24_…`, `r25_…`) — a dir can start being
   used between being judged idle and being deleted. Close that window for every
   kind of dir:
   - Keep a dir any process holds a BSD lock on, the systemd-tmpfiles convention,
     and hold that lock while deleting. Also take cargo's build lock (`r03`). Before
     relying on cargo's lock, test what a waiting build does when the dir it waited on
     is gone; keeping the lock files in place may be safer.
   - Rename a dir to `<name>.reap-<pid>` beside it before deleting, so a failed
     delete never leaves a half-deleted cache where tools read it. The next run
     deletes leftovers.

   This comes after the script is gone, so the spec doesn't need a bash version.

## Watch

- **Cargo's own build-output GC**
  ([rust-lang/cargo#13136](https://github.com/rust-lang/cargo/issues/13136)). Cargo
  1.88 cleans its download cache automatically, but not build output. If it starts
  cleaning `build-dir`, reaping `~/.cargo/build` becomes cargo's job.

## Not planned

- **The `ignore` crate** — it honors `.gitignore`, and build dirs are always ignored.
- **Parallel walking** — a daily background job doesn't need it.
- **Reads as activity (atime)** — unreliable on `noatime`/`relatime` mounts, so whether a
  dir was used would depend on the machine (`r18_reading_…`).
- **ctime as activity** — creating anything in a dir already updates that dir's mtime,
  which the walk sees. ctime would also make test fixtures impossible to age.
