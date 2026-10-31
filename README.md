# reap-build-dirs

Keep many parallel checkouts of a Rust or Terraform repo from filling your disk.

If you (or your coding agents) review every PR in its own git worktree, each one
grows its own cargo build directory, often 5–25 GB, and its own `.terraform`
provider downloads, about 0.7 GB per module. Nothing ever deletes them, and a 1 TB
disk can fill in a week. `reap-build-dirs` runs daily and deletes build output that
nothing has written to for a few days. It never touches source code or git history,
and it always keeps your main checkout's `target/`.

```console
$ reap-build-dirs --dry-run
2026-10-29 19:46:37 start (free 270.5 GiB, age 3d, dry_run=1)
2026-10-29 19:46:52 would delete 12.4 GiB  /Users/you/git/app/.worktrees/pr-311/target  (cargo, idle 3d+)
2026-10-29 19:47:03 would delete 0.7 GiB  /Users/you/git/app.review-r4/terraform/.terraform  (terraform, idle 3d+)
...
2026-10-29 19:51:19 done (free 270.5 GiB)
```

## What it deletes

| What | Where | When |
|---|---|---|
| Cargo build dirs | under `REAP_ROOTS`, plus the temp dirs (4 levels deep) | nothing written for `REAP_AGE_DAYS` |
| `.terraform` dirs | in **linked git worktrees** under `REAP_ROOTS` | nothing written for `REAP_AGE_DAYS` |
| A shared cargo `build-dir` (not recommended, see below) | `REAP_SHARED_BUILD_DIR`, if you set it | larger than `REAP_SHARED_CAP_GB`, and no cargo or rustc is running |

Everything it deletes is a cache. The next `cargo build` or `terraform init`
recreates it.

## What it never touches

- **Your main checkout's `target/` and `.terraform/`.** These hold the incremental
  state that makes your daily rebuilds fast. The main checkout is identified by
  asking git, not by path, so it works with any worktree layout.
- **Anything written in the last `REAP_AGE_DAYS` days**, including builds in progress.
- **Other users' dirs.** Only dirs owned by the user running it are considered, so a
  shared `/tmp` doesn't produce a failed delete every day.
- **Anything at all if git can't run.** git is what tells a main checkout from a
  worktree, so without it the run stops before deleting anything.
- **Anything it can't fully inspect.** If checking a dir's age fails, for example
  because a subfolder is unreadable, the dir is kept. An invalid setting stops the
  run before anything is deleted.
- **Terraform modules with local state** (`terraform.tfstate` or `terraform.tfstate.d/`).
- **`.terraform` dirs with a non-default workspace selected.** Re-running
  `terraform init` would quietly switch back to `default`, and a later `apply` could
  target the wrong environment.
- **Crate source trees.** Build dirs are found by the marker files cargo writes
  (`CACHEDIR.TAG`, `.rustc_info.json`) and confirmed by a `deps/` folder, never by
  the name `target`. This also catches custom names such as `.cargo-build-pr42`.

## Install

Requires bash, git and standard find/du/df. Runs on macOS and Linux.

```sh
git clone https://github.com/lives-hoth/reap-build-dirs
install -m 755 reap-build-dirs/reap-build-dirs ~/.local/bin/
reap-build-dirs --dry-run      # see what it would delete
```

### Run it daily on macOS (launchd)

```sh
sed "s|__HOME__|$HOME|g" reap-build-dirs/launchd/reap-build-dirs.plist \
  > ~/Library/LaunchAgents/reap-build-dirs.plist
launchctl bootstrap gui/$(id -u) ~/Library/LaunchAgents/reap-build-dirs.plist
```

It runs at 04:30, or once on wake if the Mac was asleep. The log is
`~/Library/Logs/reap-build-dirs.log`. To change settings, add them under
`EnvironmentVariables` in the plist.

### Run it daily on Linux (systemd)

```sh
mkdir -p ~/.config/systemd/user
cp reap-build-dirs/systemd/reap-build-dirs.{service,timer} ~/.config/systemd/user/
systemctl --user daemon-reload
systemctl --user enable --now reap-build-dirs.timer
journalctl --user -u reap-build-dirs     # the log
```

## Configuration

| Variable | Default | Meaning |
|---|---|---|
| `REAP_ROOTS` | `$HOME/git` | Colon-separated dirs where your checkouts live |
| `REAP_TEMP_ROOTS` | `/tmp`, `$TMPDIR`, and on macOS the per-user temp dir | Colon-separated temp dirs, searched 4 levels deep |
| `REAP_AGE_DAYS` | `3` | Days without writes before a dir is deleted (a whole number, at least 1) |
| `REAP_TERRAFORM` | `1` | Set to `0` to leave `.terraform` dirs alone |
| `REAP_SHARED_BUILD_DIR` | unset | A cargo `build-dir` shared across workspaces |
| `REAP_SHARED_CAP_GB` | `100` | Size at which the shared build-dir is wiped |
| `DRY_RUN` | `0` | `1` reports without deleting, same as `--dry-run` |

## Exit status

| Code | Meaning |
|---|---|
| `0` | Every delete succeeded (or `--dry-run`) |
| `1` | At least one delete failed; the run still finished the rest. Failures are logged as `FAILED  <path>` |
| `2` | Invalid setting or argument, or git can't run; nothing was deleted |

A scheduler therefore shows a run with failed deletes as failed.

## Waste less in the first place

Cleaning up after the fact is only half of it. Two config changes cut how much gets
written to begin with.

**Turn on Terraform's plugin cache.** Without it, every `terraform init` in every
worktree downloads its own full copy of each provider. With it, providers are
downloaded once and linked into each `.terraform`, which then takes almost no space:

```sh
mkdir -p ~/.terraform.d/plugin-cache
echo 'plugin_cache_dir = "$HOME/.terraform.d/plugin-cache"' >> ~/.terraformrc
```

**Move cargo's intermediate build output out of your checkouts, one dir per
workspace.** Since Rust 1.96, a global `build-dir` keeps `deps/`, `incremental/`
and friends out of each worktree's `target/`, so they all sit under one root that
`reap-build-dirs` can sweep:

```toml
# ~/.cargo/config.toml
[build]
build-dir = "{cargo-cache-home}/build/{workspace-path-hash}"
incremental = false   # throwaway trees never get to reuse incremental state
```

Then add that root to the search: `REAP_ROOTS="$HOME/git:$HOME/.cargo/build"`. Each
workspace's dir is deleted once it has been idle for `REAP_AGE_DAYS`, like any other
build dir.

That includes your main checkout's dir: only a main checkout's own `target/` is
protected, and nothing tells which dir under `~/.cargo/build` belongs to it. To keep its
build state through a long break, build it in-tree again with a config file in that
checkout, kept out of git:

```toml
# <main checkout>/.cargo/config.toml
[build]
build-dir = "{workspace-root}/target"
```

Cargo also reads this file in every directory below it, so worktrees nested inside the
checkout (`repo/.worktrees/pr-42`) build into their own `target/` too. That is safe:
`{workspace-root}` differs per worktree, and their idle `target/` dirs are still deleted.

> **Don't share one build-dir across checkouts of the same repo.** A literal path
> (`build-dir = "/Users/you/.cargo/build-shared"`) looks like a way to compile
> dependencies once, but cargo identifies your own crates by name and version, not by
> location. Two checkouts of the same crate then reuse each other's build: `cargo
> build` reports nothing to do, and a worktree runs or tests *another* worktree's
> code. Include `{workspace-path-hash}` so each workspace gets its own dir.

If you already have a shared build-dir, `REAP_SHARED_BUILD_DIR` still wipes it once
it passes `REAP_SHARED_CAP_GB`.

## Testing

`tests/spec.rs` is the specification: one test per rule of what is deleted and what
is kept. Each test builds a throwaway tree of git worktrees, cargo build dirs and
Terraform modules in a temporary directory and checks what a run leaves behind.

```sh
cargo test                                        # the binary
REAP_BIN="$PWD/reap-build-dirs" cargo test        # the bash script, same spec
```

Ignored tests describe decided behavior that isn't implemented yet. The Rust code
follows [`docs/rust-type-design.md`](docs/rust-type-design.md); the migration order is
in [`ROADMAP.md`](ROADMAP.md).

## License

MIT
