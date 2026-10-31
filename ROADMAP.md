# Roadmap

`reap-build-dirs` is moving from a bash script to a Rust binary one piece at a time.
The binary currently runs the bash script for everything except parsing settings.
Each step replaces one piece, test-first: un-ignore or add its test in `tests/spec.rs`,
watch it fail, implement it in Rust, and keep the whole spec green against both the
binary and the script (`REAP_BIN`) until the script is gone.

`tests/spec.rs` is the specification. This file only orders the work.

## Done

- **Settings** — clap flags, each overridable by its `REAP_*` variable; a flag wins
  over the variable, the variable over the default. Invalid values stop the run
  before anything is deleted.

## Next

1. **Exit code** (`r13_failed_delete_exits_nonzero`) — exit 1 when any delete failed,
   so a scheduler shows the run as failed.
2. **Main checkout protection** (`r05_…`) — protect only the main checkout's
   top-level `target/`; other `target/` dirs inside it are deleted when idle.
3. **Terraform** — move `.terraform` discovery and its keep rules into Rust; run the
   script with `REAP_TERRAFORM=0`.
4. **Shared build-dir** (`r11_…_unless_locked`) — move the size-cap wipe into Rust,
   deciding "in use" by cargo's lock instead of looking for cargo processes.
5. **Cargo build dirs** (`r03_…`) — move discovery into Rust with
   [`walkdir`](https://docs.rs/walkdir): depth limits, pruning `.git`, `node_modules`,
   `deps` and `incremental`, no symlink following. Treat any walk error as "can't
   tell, keep it", never skip it. Delete only while holding cargo's build lock, so a
   build that starts mid-delete waits instead of failing. Before relying on the lock,
   test what a waiting build does when the dir it waited on is gone; keeping the lock
   files in place may be safer.
6. **Remove the script** — the binary owns every rule; drop `REAP_BIN` from CI.

## Not planned

- **The `ignore` crate** — it honors `.gitignore`, and build dirs are always ignored.
- **Parallel walking** — a daily background job doesn't need it.
