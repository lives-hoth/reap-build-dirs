# reap-build-dirs

Keep many parallel checkouts of a Rust or Terraform repo from filling your disk.

If you (or your coding agents) review every PR in its own git worktree, each one
grows its own cargo build directory, often 5–25 GB, and its own `.terraform`
provider downloads, about 0.7 GB per module. Nothing ever deletes them, and a 1 TB
disk can fill in a week. `reap-build-dirs` runs daily and deletes build output that
nothing has written to for a few days. It never touches source code or git history,
and it always keeps your main checkout's build state.

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
| Cargo build dirs | under `REAP_ROOTS`, plus `/tmp` and `$TMPDIR` (4 levels deep) | nothing written for `REAP_AGE_DAYS` |
| `.terraform` dirs | in **linked git worktrees** under `REAP_ROOTS` | nothing written for `REAP_AGE_DAYS` |
| A shared cargo `build-dir` | `REAP_SHARED_BUILD_DIR`, if you set it | larger than `REAP_SHARED_CAP_GB`, and no cargo or rustc is running |

Everything it deletes is a cache. The next `cargo build` or `terraform init`
recreates it.

## What it never touches

- **Your main checkout's `target/` and `.terraform/`.** These hold the incremental
  state that makes your daily rebuilds fast. The main checkout is identified by
  asking git, not by path, so it works with any worktree layout.
- **Anything written in the last `REAP_AGE_DAYS` days**, including builds in progress.
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
| `REAP_TEMP_ROOTS` | `/tmp:$TMPDIR` | Colon-separated temp dirs, searched 4 levels deep |
| `REAP_AGE_DAYS` | `3` | Days without writes before a dir is deleted (a whole number, at least 1) |
| `REAP_TERRAFORM` | `1` | Set to `0` to leave `.terraform` dirs alone |
| `REAP_SHARED_BUILD_DIR` | unset | A cargo `build-dir` shared across workspaces |
| `REAP_SHARED_CAP_GB` | `100` | Size at which the shared build-dir is wiped |
| `DRY_RUN` | `0` | `1` reports without deleting, same as `--dry-run` |

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

**Share cargo's intermediate build output across worktrees.** Since Rust 1.96, a
global `build-dir` lets every worktree reuse one copy of compiled dependencies
instead of compiling its own:

```toml
# ~/.cargo/config.toml
[build]
build-dir = "/Users/you/.cargo/build-shared"
incremental = false   # throwaway trees never get to reuse incremental state
```

A shared build-dir never shrinks by itself, so point `REAP_SHARED_BUILD_DIR` at it
and let `reap-build-dirs` wipe it when it passes the cap. Two builds can't use the
same build-dir at once, so concurrent builds wait for each other. A repo's own
`.cargo/config.toml` overrides the global one, so a tree you work in every day can
opt back out:

```toml
# <your main checkout>/.cargo/config.toml
[build]
build-dir = "{workspace-root}/target"
incremental = true
```

That opt-out file is usually gitignored. If yours is committed, every worktree
inherits it and opts back out of sharing too.

## Testing

`tests/spec.rs` is the specification: one test per rule of what is deleted and what
is kept. Each test builds a throwaway tree of git worktrees, cargo build dirs and
Terraform modules in a temporary directory and checks what a run leaves behind.

```sh
cargo test                                        # the binary
REAP_BIN="$PWD/reap-build-dirs" cargo test        # the bash script, same spec
```

Ignored tests describe decided behavior that isn't implemented yet.

## License

MIT
