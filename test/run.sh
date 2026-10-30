#!/usr/bin/env bash
# Builds a fixture tree in a temp dir, runs reap-build-dirs against it, and checks
# what was kept and what was deleted. No network, no real cargo or terraform.
set -eu

here=$(cd "$(dirname "$0")/.." && pwd)
bin="$here/reap-build-dirs"
fx=$(mktemp -d)
trap 'rm -rf "$fx"' EXIT
fx=$(cd "$fx" && pwd -P)

fails=0
ok()   { echo "ok    $1"; }
fail() { echo "FAIL  $1"; fails=$((fails + 1)); }
gone() { if [ ! -e "$fx/$1" ]; then ok "deletes $1"; else fail "should delete $1"; fi; }
kept() { if [ -e "$fx/$1" ]; then ok "keeps $1"; else fail "should keep $1"; fi; }

age() { find "$fx/$1" -exec touch -t 202001010000 {} +; }   # make a tree look idle

cargo_dir() { # $1 = path, $2 = marker file
  mkdir -p "$fx/$1/debug/deps"
  : > "$fx/$1/$2"
  : > "$fx/$1/debug/deps/libfoo.rlib"
}

tf_dir() { # $1 = module path
  mkdir -p "$fx/$1/.terraform/providers"
  : > "$fx/$1/.terraform/providers/provider-bin"
}

git_quiet() { git -c init.defaultBranch=main -c user.name=t -c user.email=t@t "$@" >/dev/null 2>&1; }

# A main checkout with a linked worktree.
mkdir -p "$fx/git/repo"
git_quiet -C "$fx/git/repo" init
git_quiet -C "$fx/git/repo" commit --allow-empty -m init
git_quiet -C "$fx/git/repo" worktree add "$fx/git/repo-review" --detach

cargo_dir git/repo/target CACHEDIR.TAG                          # main checkout: keep
cargo_dir git/repo-review/target CACHEDIR.TAG                   # idle worktree: delete
cargo_dir git/repo-review/.cargo-build-pr1 .rustc_info.json     # custom name, other marker
cargo_dir git/repo-review/busy-build CACHEDIR.TAG               # written recently: keep
mkdir -p "$fx/git/vendor/registry/src/cc-1.0/src/target/debug/deps"
: > "$fx/git/vendor/registry/src/cc-1.0/src/target/CACHEDIR.TAG" # crate source: keep
mkdir -p "$fx/git/repo-review/not-a-build"
: > "$fx/git/repo-review/not-a-build/CACHEDIR.TAG"              # marker without deps/: keep

tf_dir git/repo/infra                                           # main checkout: keep
tf_dir git/repo-review/infra                                    # idle worktree: delete
tf_dir git/repo-review/local-state
: > "$fx/git/repo-review/local-state/terraform.tfstate"         # local state: keep
tf_dir git/repo-review/workspace
echo staging > "$fx/git/repo-review/workspace/.terraform/environment"  # non-default: keep

cargo_dir tmp/job-build CACHEDIR.TAG                            # temp root: delete
cargo_dir tmp/a/b/c/d/e/too-deep CACHEDIR.TAG                   # beyond depth 4: keep

age git
age tmp
touch "$fx/git/repo-review/busy-build/debug/deps/libfoo.rlib"

run() { REAP_ROOTS="$fx/git" REAP_TEMP_ROOTS="$fx/tmp" "$bin" "$@"; }

echo "# dry run deletes nothing"
before=$(find "$fx" | wc -l)
run --dry-run >/dev/null
if [ "$(find "$fx" | wc -l)" = "$before" ]; then ok "dry run"; else fail "dry run changed files"; fi

echo "# real run"
run >/dev/null
kept git/repo/target
gone git/repo-review/target
gone git/repo-review/.cargo-build-pr1
kept git/repo-review/busy-build
kept git/vendor/registry/src/cc-1.0/src/target
kept git/repo-review/not-a-build
kept git/repo/infra/.terraform
gone git/repo-review/infra/.terraform
kept git/repo-review/local-state/.terraform
kept git/repo-review/workspace/.terraform
gone tmp/job-build
kept tmp/a/b/c/d/e/too-deep

echo "# invalid settings stop the run before anything is deleted"
cargo_dir git/repo-review/fresh-build CACHEDIR.TAG              # written just now
for bad in 3d three -1 0; do
  if REAP_AGE_DAYS="$bad" run >/dev/null 2>&1; then
    fail "REAP_AGE_DAYS='$bad' should exit nonzero"
  fi
done
if [ -e "$fx/git/repo-review/fresh-build" ]; then ok "rejects bad REAP_AGE_DAYS"; else fail "bad REAP_AGE_DAYS deleted a fresh dir"; fi
if REAP_SHARED_CAP_GB=lots run >/dev/null 2>&1; then fail "REAP_SHARED_CAP_GB=lots should exit nonzero"; else ok "rejects bad REAP_SHARED_CAP_GB"; fi

echo "# a dir whose age can't be checked is kept"
if [ "$(id -u)" = 0 ]; then
  echo "skip  (root can read everything, so this case can't be set up)"
else
  cargo_dir git/repo-review/unreadable CACHEDIR.TAG
  mkdir "$fx/git/repo-review/unreadable/locked"
  age git/repo-review/unreadable
  chmod 000 "$fx/git/repo-review/unreadable/locked"
  # Dry run: rm can't remove an unreadable folder either, so a real run would
  # keep the dir even if the script wrongly tried to delete it.
  run --dry-run > "$fx/out.log"
  chmod 755 "$fx/git/repo-review/unreadable/locked"
  if grep -q 'repo-review/unreadable' "$fx/out.log"; then
    fail "tried to delete a dir whose age couldn't be checked"
  else
    ok "keeps a dir whose age can't be checked"
  fi
fi

echo "# shared build-dir"
mkdir -p "$fx/shared/debug/deps"
: > "$fx/shared/.rustc_info.json"
REAP_SHARED_BUILD_DIR="$fx/shared" REAP_SHARED_CAP_GB=1000 run >/dev/null
kept shared
dd if=/dev/zero of="$fx/shared/debug/deps/big" bs=1024 count=2048 2>/dev/null
# A cap of 0 GB means any size is over the cap; the result depends on whether
# cargo happens to be running on this machine, so accept either outcome.
REAP_SHARED_BUILD_DIR="$fx/shared" REAP_SHARED_CAP_GB=0 run > "$fx/out.log"
if [ ! -e "$fx/shared" ] || grep -q 'cargo is building' "$fx/out.log"; then
  ok "wipes shared dir over cap (or defers while cargo runs)"
else
  fail "shared dir over cap was neither wiped nor deferred"
fi

if [ "$fails" != 0 ]; then echo "$fails failed"; exit 1; fi
echo "all passed"
