# Rust type design — make illegal states unrepresentable

If a value encodes a decision, a dependency, or a domain structure, give it a type that
cannot express the wrong answer. A comment explaining which combinations are valid is a
type that has not been written yet.

For a tool that deletes directories, the wrong answer is nearly always the same one:
a state that should have meant "keep it" turned into "delete it". Every example below
is a defect this project actually had (in the bash version it is replacing) or a type
the Rust version uses so that it cannot happen again.

## Three outcomes wearing two

```rust
// ❌ "Is anything in this dir recent?" A failed check has to answer *something*,
//    and whichever it picks is a guess. The bash version picked "no": an unreadable
//    subfolder, or a typo that made `find -mtime` error, made every dir look idle,
//    and every dir was deleted.
fn is_recent(dir: &Path, age: Days) -> bool
```

```rust
// ✅ "Couldn't tell" is a different fact from "idle", and it decides the other way.
enum Activity {
    Recent,
    Idle,
    Unknown(io::Error),   // treated as Recent: never delete what can't be inspected
}
```

A state that only appears when something breaks is the state nobody has exercised.
Name it so a test can reach it.

## `Option<T>` when the `None` has a reason

The tell is mechanical — **if the doc comment above a `None` explains why the value is
absent, that explanation belongs in a variant name.**

```rust
// ❌ `None` means "not inside a git repo". It also means "git didn't run", because
//    both produce no answer. The bash version had exactly this: with git broken, a
//    main checkout looked like no checkout at all and lost its protection.
/// `None` when the dir is not inside a git repository.
fn checkout_kind(dir: &Path) -> Option<CheckoutKind>
```

```rust
// ✅ Being outside a repo is an answer. Git failing is an error: at startup it stops
//    the run; for one dir, that dir is kept and the failure is logged.
enum Checkout {
    Main,
    Linked,
    Outside,
}

fn checkout(dir: &Path) -> Result<Checkout, GitError>
```

Keep `Option` for what it means: a value genuinely absent, where absence carries no
further meaning — a setting the user didn't give, in which case the default applies.

## Bare integers carrying a domain meaning

```rust
// ❌ `0` compiles. "Idle for at least 0 days" is true of every dir, so 0 means
//    "delete everything", the most destructive value the setting can take.
age_days: u32
```

```rust
// ✅ Unrepresentable. Parsing fails at startup, before anything is deleted.
age_days: NonZeroU32
```

Apply this to every new scalar, including one added next to a field that was just
fixed, which is where it is easiest to miss.

## Arithmetic that can wrap

```bash
# ❌ bash: a cap meant as "never wipe" overflowed 64-bit arithmetic and came out
#    negative, so every shared build-dir was "over the cap" and was wiped.
[ "$shared_kb" -gt $((SHARED_CAP_GB * 1048576)) ]      # 9999999999999 GB -> -7.9e18 KB
```

```rust
// ✅ Widen losslessly, and make overflow an error rather than a wrapped number.
let cap_kb = u64::from(cap_gb).checked_mul(1 << 20).ok_or(ConfigError::CapTooLarge)?;
```

## `as` casts

Lossy by construction. On a correctness- or safety-relevant path prefer `From`,
`TryFrom`, or a type that cannot be out of range.

Where a cast is genuinely harmless, say *why* rather than letting the cast imply it.
Converting a size in KB to a fractional GiB for a log line is display-only; nothing is
decided by it:

```rust
let gib = kb as f64 / 1_048_576.0;   // display only: no decision reads this value
```

## Anonymous tuples

`(PathBuf, &str)` for "delete this dir, for this reason": what is the `&str`, a
reason or a label, and which strings are allowed? The reason is printed in every log
line, and that line format is a stable contract, so the set of reasons is closed:

```rust
struct Reap {
    dir: PathBuf,
    reason: Reason,
}

enum Reason {
    CargoIdle(Days),        // "cargo, idle 3d+"
    TerraformIdle(Days),    // "terraform, idle 3d+"
    SharedOverCap(Gigabytes),
}
```

## Rules of thumb

| If the value… | then… |
|---|---|
| encodes structure or a dependency | discriminated union |
| is required by design | not `Option` |
| has a `None` / `false` / `0` case with a **reason** | that reason is a variant name |
| must satisfy an invariant | make the invalid state unrepresentable, don't assert it |
| is a trait method every implementor must choose | required, never defaulted |
| can fail in a way that changes the decision | a variant that decides toward *keep* |

Reviews check new fields, parameters, and returns against this. Raising it on an
existing offender is welcome; rewriting unrelated code to match it in the same change
is not — prefer a small diff and a follow-up.
