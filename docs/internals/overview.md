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
returning `Ffi*` records, claim the session gate (`begin()`) if it scans or mutates (a process
signal, [`process_terminate`](../../crates/tiny-ffi/src/processes.rs), does not), regenerate
the bindings with [`build-ffi.sh`](../../scripts/build-ffi.sh), and consume the matching generated interface in the native client.

## Read-only versus destructive

`sys`, `scan`, `focus` and process listing never remove or signal anything; `scan` is read-only
by contract. `clean` and `uninstall` remove data and `processes quit` sends signals; each shows
what it will do, asks, then acts.

- **Move to Trash** has two routes, both refusing a symlink. CLI clean and `uninstall` ask
  Finder through `osascript` with the path as an argument, never interpolated, so Put Back works
  and quoting cannot break ([`FinderTrash`](../../crates/tiny-core/src/clean/finder_trash.rs)).
  The app's Clean moves from its own process through `NSFileManager trashItemAtURL`
  ([`NativeTrash`](../../crates/tiny-core/src/clean/native_trash.rs)), so its Full Disk Access
  applies and no Automation permission is asked; Put Back does not know those items. Emptying
  the Trash always goes through Finder.
- **Permanent deletion** has two front ends. The CLI's Hard delete and `uninstall --hard` remove
  through [`remove_recursive_safe`](../../crates/tiny-core/src/clean/fs_safe.rs), bottom-up and never
  following a symlink; tool-backed categories run their tool instead. In the app, the client's
  delete mode decides: Permanent deletes Rebuilt automatically and Project build output through
  `SafeDelete` (never entering another volume) and prunes uv, pnpm, Bun and pip caches with their
  tool. Some removals are permanent in either mode: Finder's Empty Trash for the Trash row, a
  simulator runtime's `simctl` removal, and the tool removals of Homebrew, Docker and unavailable
  simulator devices ([routing](clean.md#the-desktops-checked-removal)). Root-owned simulator caches
  are report only: macOS denies their deletion even as administrator
  ([why](clean.md#deletion-as-administrator)).
- **Confirmation** is a separate prompt for a permanent action chosen interactively. With `-y`
  there is none: `clean --hard -y` demands `TINY_CONFIRM_HARD=1`, `processes quit --force -y`
  demands `TINY_CONFIRM_FORCE=1` ([`processes.rs`](../../crates/tiny/src/render/processes.rs)),
  and `uninstall --hard -y` demands nothing
  ([`uninstall.rs`](../../crates/tiny/src/render/uninstall.rs)).
- **Signals** go only to a process whose identity still matches a fresh lookup, with no
  escalation or tree kill ([`terminate.rs`](../../crates/tiny-core/src/processes/terminate.rs)).

The clean pipeline, its registry and the desktop's checked removal are in [clean.md](clean.md).

## Platform

Process sampling must remain independent of filesystem scan workers.
[`tiny-core` disables sysinfo's optional `multithread` feature](../../crates/tiny-core/Cargo.toml)
while retaining its other capabilities: otherwise process refresh queues on Rayon's
global pool behind any scan that uses it. Scans keep to bounded pools of their own:
[Storage scans](../../crates/tiny-core/src/space_lens.rs) and the desktop's
[`node_modules` sizing](../../crates/tiny-core/src/clean/providers/node_modules.rs) each use
four threads, since more threads did not walk faster and cost twice the kernel CPU.
The [sampler contention regression](../../crates/tiny-core/src/processes/snapshot/tests.rs)
holds the global pool busy in an isolated test process and requires a real sample to finish
before releasing the scan worker.

`clean`, `uninstall` and port lookups assume macOS tools: Finder, `pgrep`, `mdls`, `tmutil`,
`xcrun`, `lsof`, `git`. Code the app reaches spawns them by absolute path or through
`ToolLookup::app`, because a Finder-launched app gets a minimal `PATH`
([`runner.rs`](../../crates/tiny-core/src/runner.rs)). Direct macOS calls sit behind
`cfg(target_os = "macos")` with a fallback elsewhere: memory pressure
([`pressure.rs`](../../crates/tiny-core/src/processes/pressure.rs)), footprint and CPU time
([`footprint.rs`](../../crates/tiny-core/src/processes/footprint.rs)), the responsible process
([`responsible.rs`](../../crates/tiny-core/src/processes/responsible.rs)) and working directory
read `None`, used memory is sysinfo's figure with no split ([`sys.rs`](../../crates/tiny-core/src/sys.rs)),
APFS sharing reads unknown ([`freed_size.rs`](../../crates/tiny-core/src/clean/freed_size.rs)) and
the native Trash fails ([`native_trash.rs`](../../crates/tiny-core/src/clean/native_trash.rs)). On
other platforms the workspace also fails at run time wherever it shells out to a macOS tool, and
no CI checks it there.
