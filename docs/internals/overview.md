# Architecture overview

> For maintainers. Using the CLI? See [user guides](../user/).

The [workspace](../../Cargo.toml) keeps three responsibilities together: operations in
[tiny-core](../../crates/tiny-core/src/lib.rs), CLI parsing/presentation in
[tiny](../../crates/tiny/src/main.rs), and native consent/session enforcement in
[tiny-ffi](../../crates/tiny-ffi/src/lib.rs). Native UI is a separate client.

## Adding a command

Put the operation in `tiny-core`, with serde options in
[`options.rs`](../../crates/tiny-core/src/options.rs) when both front ends pass them. In `tiny`,
add a `Commands` variant with its own `*Opts` struct and `From` conversion in `cli.rs`, one match
arm in `main.rs`, and a `render/<command>` module whose `run` calls core and prints. Validate
flag combinations before any I/O, as
[`cli_validate.rs`](../../crates/tiny/src/render/clean/cli_validate.rs) does; core re-checks
what the app can send too. To reach the app, add a `#[uniffi::export]` method on `TinySession`
returning `Ffi*` records, claim the session gate (`begin()`) if it scans or mutates, regenerate
the bindings with [`build-ffi.sh`](../../scripts/build-ffi.sh), and consume the matching generated interface in the native client.

## Read-only versus destructive

`sys`, `scan`, `focus` and process listing never remove or signal anything; `scan` is read-only
by contract. `clean` and `uninstall` remove data and `processes quit` sends signals; each shows
what it will do, asks, then acts.

- **Move to Trash** asks Finder through `osascript` with the path as an argument, never
  interpolated, so Put Back works and quoting cannot break. The app, CLI clean and
  `uninstall` all go through [`FinderTrash`](../../crates/tiny-core/src/clean/finder_trash.rs),
  which refuses a symlink because Finder would trash its target.
- **Hard delete** exists only in the CLI. `clean` and `uninstall` remove through
  [`remove_recursive_safe`](../../crates/tiny-core/src/clean/fs_safe.rs), bottom-up and never
  following a symlink; tool-backed categories run their tool instead.
- **Confirmation** is a separate prompt for a permanent action chosen interactively. With `-y`
  there is none: `clean --hard -y` demands `TINY_CONFIRM_HARD=1`, `processes quit --force -y`
  demands `TINY_CONFIRM_FORCE=1` ([`processes.rs`](../../crates/tiny/src/render/processes.rs)),
  and `uninstall --hard -y` demands nothing
  ([`uninstall.rs`](../../crates/tiny/src/render/uninstall.rs)).
- **Signals** go only to a process whose identity still matches a fresh lookup, with no
  escalation or tree kill ([`terminate.rs`](../../crates/tiny-core/src/processes/terminate.rs)).

The clean pipeline, its registry and the desktop's checked Trash are in [clean.md](clean.md).

## Platform

`clean`, `uninstall` and port lookups assume macOS tools: Finder, `pgrep`, `mdls`, `tmutil`,
`xcrun`, `lsof`, `git`. Code the app reaches spawns them by absolute path or through
`ToolLookup::app`, because a Finder-launched app gets a minimal `PATH`
([`runner.rs`](../../crates/tiny-core/src/runner.rs)). There is no `cfg(target_os)` gating: the
workspace builds on Linux and fails at run time wherever it shells out to a macOS tool.
