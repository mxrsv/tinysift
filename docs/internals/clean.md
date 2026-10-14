# Clean pipeline

> For maintainers. Using TinySift? See [docs/user/clean.md](../user/clean.md).

Providers, discovery and both execute paths live in
[`tiny-core/src/clean/`](../../crates/tiny-core/src/clean/mod.rs). The CLI's flow is
[`render/clean/mod.rs`](../../crates/tiny/src/render/clean/mod.rs); the app's (scan, preview,
checked Trash) is [`tiny-ffi/src/clean.rs`](../../crates/tiny-ffi/src/clean.rs). Terms are in the
[glossary](glossary.md).

## Validation first

[`cli_validate.rs`](../../crates/tiny/src/render/clean/cli_validate.rs) refuses the bad flag
combinations before any
filesystem access. `TINY_CONFIRM_HARD` is injected as a boolean (`validate_with_env`) so tests
never set process environment. Core repeats only `idle_days > 0` (`validate_options`), because
the app reaches discovery without clap.

## The provider registry

A category exists only when its id appears in `all_providers_with`, `known_category_ids`,
`category_family` and `comes_back` in
[`providers/mod.rs`](../../crates/tiny-core/src/clean/providers/mod.rs). Tests require the
providers in `known_category_ids` order, each with an `inclusion_reason` and a `comes_back`
answer; `category_family` panics on an unknown id.

Desktop eligibility is deny by default: `desktop_trash_paths()` is true only when cleanup is
exactly "move each listed path to Trash", and `Destructive` risk is always report only
(`desktop_report_only`). The `DESKTOP_TRASH` table test fails until a new id gets a deliberate
decision. A provider whose paths come from a tool's output sets `roots_from_tool_output()`, and
the app refuses its items outside the home folder. Shared helpers (`root_as_item`,
`execute_per_item`, `run_tool`, `is_idle`, ...) sit in the same file;
[`project_activity.rs`](../../crates/tiny-core/src/clean/providers/project_activity.rs) decides
whether a project is idle.

## Selection and discovery

[`discover.rs`](../../crates/tiny-core/src/clean/discover.rs) picks providers by risk unless
`--category` is given, which counts as consent and ignores the `--include-*` flags. The app
always scans Safe and Review, never Destructive
([native discovery](../../crates/tiny-ffi/src/clean.rs)).

`discover_checked` runs each provider in isolation and records found, app running, unavailable
(missing tool or failed probe) or failed. The CLI collapses that to its old report: unavailable
and failed categories vanish silently. The app keeps every outcome and also refuses `/`, the
home folder, its ancestors and tool-printed paths outside home
([`protected_reason`](../../crates/tiny-core/src/clean/trash_plan.rs)).

App gating has two mechanisms, both `pgrep -x` on a process name. `quit_apps()` (default
`requires_app_quit()`) skips the whole category while its app runs. `browser-caches`,
`chat-caches`, `streaming-caches` and `user-caches` instead drop only a running app's paths
inside `discover()` (`ScanContext::known_running`). Right before each move the app re-checks
`item_apps(path)` and `check_item(path)`. The [CLI executor](../../crates/tiny-core/src/clean/execute.rs)
also rechecks them with `RunnerProbe` before file mutations or tool-backed batches. A failed
probe still means "not running" during CLI discovery (`PgrepChecker`), but blocks execution;
to the app (`RunnerProbe`) it marks the category
unavailable or refuses the move, while per-path discovery keeps the path for that re-check
([`process.rs`](../../crates/tiny-core/src/clean/process.rs)).

## Mapping actions

The CLI acts through each provider's `execute`.
[`execute.rs`](../../crates/tiny-core/src/clean/execute.rs) maps the menu choice to one exec
action per provider with `map_action`:

| Menu choice | `trash` provider         | every other provider     |
| ----------- | ------------------------ | ------------------------ |
| Trash       | refused before execution | `ExecAction::Trash`      |
| Hard delete | `ExecAction::EmptyTrash` | `ExecAction::HardDelete` |

The trash provider rejects `Trash` and `HardDelete`; `execute_per_item` rejects `EmptyTrash`.
Both sides refuse so a wrong mapping fails loudly instead of deleting the wrong thing.

Excluded paths and overlapping selected ancestors/descendants leave each group before its
provider executes; retained overlaps are explained in the existing failure report. A failure on one path is
recorded in `ExecReport.failed` and the run continues; an error from a provider as a whole
aborts the remaining groups, after earlier groups were already acted on.

## The desktop's checked Trash

The app never calls `CleanProvider::execute`, so no tool command or hard delete runs from it.
`clean_discover` keeps each candidate's path, fingerprint and discovery roots in the session
under an opaque ID, so Swift never sends a path back. `clean_preview` excludes paths the
[saved rules](#desktop-persistent-protection) hold, drops paths another selected path covers and
excludes any that would carry an unselected Review candidate along
(the explicit-selection contract);
a preview is single-use and expires. `clean_execute` consumes it, then
[`execute_checked`](../../crates/tiny-core/src/clean/checked_execute.rs) revalidates each path,
and everything it covers, just before moving it through the
[`Trash`](../../crates/tiny-core/src/clean/finder_trash.rs) seam. An Automation denial or
cancellation stops the run. Scan and execute refuse to start when `HOME` is not the account's
home folder (`FfiError::UntrustedHome`).

The [path validator](../../crates/tiny-core/src/clean/checked_execute.rs) checks every parent
with `symlink_metadata`, including parents above a discovery root. A provider's root is not
a trusted boundary: a tool may return the candidate itself as its root. A path containing
any symlink parent is skipped, including system aliases such as `/var`; the executor never
canonicalizes it into a different path to move.

## Desktop persistent protection

Always keep is a desktop-only policy owned by Rust. A
[rule](../../crates/tiny-core/src/clean/protection.rs) is one absolute path plus a scope: `File`
covers exactly that path, `Directory` also everything inside it. A candidate is held when it
equals the rule path, lies inside a `Directory` rule, or is a strict ancestor of the rule path,
because moving it would carry the kept path along, found by discovery or not. Components are
compared one by one, never as string prefixes. Symlinks are never replaced by their targets
and renames are not tracked, so a rule for a path that is missing today applies again if that
path returns. Equivalent spellings on a volume that folds case or Unicode are decided from the
real directory entries; anything that cannot be decided (unreadable parent, several entries
for one inode) counts as held, not as free. Tiny's own store, lock and staging paths are
internal rules, so cleanup never moves them or an ancestor that holds them.

Swift never writes rules and never sends a path: it names a discovered candidate ID to
keep, or a rule ID to remove, with the revision it last read
([`clean_keep` and `clean_unkeep`](../../crates/tiny-ffi/src/clean/protection.rs)). The
[CLI executor](../../crates/tiny-core/src/clean/execute.rs) does not read the rules.

### The store

The [store](../../crates/tiny-core/src/clean/protection_store.rs) is
`~/Library/Application Support/Tiny/clean-protection/`, an owner-only directory holding
`rules.json` (versioned JSON: `version`, a `revision` that rises by one per commit, a
`next_id` so rule ids are never reused, and the rules) and an `initialized` marker. The
advisory lock is `clean-protection.lock` beside it, deliberately outside the directory that a
first save replaces. A held lock never blocks: after eight short attempts it is `Busy`.

- **Absent is first use.** A read of a missing store returns revision 0 and creates nothing.
- **Anything else fails closed.** A store directory without its marker or rules, malformed or
  unsupported JSON, invalid rules, a symlink or a file that is not owner-only is an error, never
  empty rules. Do not recover by writing an empty store.
- **Snapshots compare revision and content**, so an outside edit that keeps the revision is
  still a change.
- **The first save is staged.** Rules and marker are assembled in
  `clean-protection.staging`, synced and renamed into place; a failure before the rename
  commits nothing. Later saves replace `rules.json` through a synced temporary file and an
  atomic rename. After the rename the outcome may be uncertain (`CommitUncertain`): reload,
  never repeat the write.
- **Removing the last rule** writes a valid empty store; it does not delete the directory.

### Where it is enforced

1. [`clean_preview`](../../crates/tiny-ffi/src/clean.rs) reads the rules under the shared lock
   and removes held selections as typed exclusions before the PC-C3 check and before overlaps
   are merged, so a held parent leaves its independently selected children in place. A store
   that cannot be read fails the preview (`ProtectionUnavailable`). The preview remembers the
   snapshot it was authorized against; a rule write drops the stored preview.
2. [`clean_execute`](../../crates/tiny-ffi/src/clean.rs) takes the exclusive lease first, then
   consumes the preview only if the stored snapshot equals the one it was authorized against
   (else `ProtectionChanged` and the preview is dropped). A busy or unreadable store fails
   without spending the preview.
3. [`execute_checked`](../../crates/tiny-core/src/clean/checked_execute.rs) repeats the check
   before every move: cancellation, the snapshot still unchanged, `validate` (rules first, then
   provider eligibility, running-app probe, provider guards and the path checks), the snapshot
   unchanged again, the rules again, cancellation, then the Trash call. A changed or unreadable
   store stops the run and the rest is not attempted; a held path is skipped as `Kept`,
   `HoldsKept` or `KeepUncertain`. Only the planned path is evaluated: a rule inside it makes
   it `HoldsKept`, so what a merged item covers needs no second check.

Every desktop cleanup run holds the lease: `ExecContext.policy` is required, and the lock stays
until the report is built, so a rule write, a reload or another session's run meets `Busy`
meanwhile. Lock order is the session gate, the store lock, then the clean-state lock. A new
desktop path that moves files must go through `execute_checked` with a lease, or it bypasses
the rules.

### Session memory and Swift

The [session](../../crates/tiny-ffi/src/clean/protection.rs) remembers that it has seen a
committed store, across rescans and failed reads or writes. A store that later vanishes blocks
reads, previews, writes and execution until valid settings return. A session that never saw
the store cannot tell first use from the external removal of the whole directory, even when
the lock remains: execution can create the lock before the first rule is saved. This is not
tamper resistance, so never promise that deleting local settings keeps protection.

Native clients must apply the returned protection state only to the discovery it describes;
a failed rule read must never become an empty successful rule list. The
[FFI response](../../crates/tiny-ffi/src/clean/protection.rs) carries the revision and guards.

## Test seams

Tests never depend on what is installed or running on the machine:

- [`process.rs`](../../crates/tiny-core/src/clean/process.rs) — `ProcessChecker` and
  `AppProbe`, both implemented by `MockChecker`.
- [`runner.rs`](../../crates/tiny-core/src/runner.rs) — `CommandRunner`, with `MockRunner` for
  providers that shell out to `docker`, `git`, `tmutil`, `mdfind` and similar.
- `discover_with_checker`, `execute_with_providers`, `discover_checked` and `execute_checked`
  (with a fake `Trash`) take those seams; so do `discover_clean` and `execute_clean` in
  `tiny-ffi`. Native clients must inject fake cleanup engines in their UI tests.

[`clean_smoke.rs`](../../crates/tiny/tests/clean_smoke.rs) runs only `--dry-run` and validation
failures, so it passes on any machine without touching data.
