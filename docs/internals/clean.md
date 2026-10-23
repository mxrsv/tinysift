# Clean pipeline

> For maintainers. Using TinySift? See [docs/user/clean.md](../user/clean.md).

Providers, discovery and both execute paths live in
[`tiny-core/src/clean/`](../../crates/tiny-core/src/clean/mod.rs). The CLI's flow is
[`render/clean/mod.rs`](../../crates/tiny/src/render/clean/mod.rs); the app's (scan, preview,
checked removal) is [`tiny-ffi/src/clean.rs`](../../crates/tiny-ffi/src/clean.rs) and
[`clean/removal.rs`](../../crates/tiny-ffi/src/clean/removal.rs). Terms are in the
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
[`providers/mod.rs`](../../crates/tiny-core/src/clean/providers/mod.rs), and in the `SECTIONS`
table test beside them. Tests require the providers in `known_category_ids` order, each with an
`inclusion_reason` and a `comes_back` answer; `category_family` panics on an unknown id.
`category_section` derives the desktop section from `comes_back` (Rebuilt, or Your files when
nothing recreates the items), except `node-modules` and `rust-targets`, which are Project build
output. The `SECTIONS` test fails until a new id gets a deliberate section, so build output never
lands in Rebuilt just because a build restores it.

Desktop eligibility is deny by default. `desktop_trash_paths()` is true only when cleanup is
exactly "move each listed path to Trash", and `Destructive` risk is always report only
(`desktop_report_only`). Three narrow exceptions each have their own provider flag and their own
sink: `desktop_empties_trash()` (only `trash`), `desktop_tool()` (uv, pnpm, simulator runtimes)
and `needs_admin()` (system simulator caches). `desktop_report_only` keeps its answer for them
(`Destructive`, `NotPerPathTrash`, `NeedsAdministrator`): it means "no per-path unprivileged
route", and the flag decides what the app may do instead. The `DESKTOP_TRASH` table test fails
until a new id gets a deliberate decision. A provider whose paths come from a tool's output sets
`roots_from_tool_output()`, and the app refuses its items outside the home folder. Shared helpers (`root_as_item`,
`execute_per_item`, `run_tool`, `is_idle`, ...) sit in the same file;
[`project_activity.rs`](../../crates/tiny-core/src/clean/providers/project_activity.rs) decides
whether a project is idle.

## Selection and discovery

[`discover.rs`](../../crates/tiny-core/src/clean/discover.rs) picks providers by risk unless
`--category` is given, which counts as consent and ignores the `--include-*` flags. A category
that `needs_admin` is left out of the CLI's default selection and appears only when named, so it
is refused with a reason instead of listed and then failed. The app always scans Safe and
Review, and `Destructive` only for the Trash row when it sets `include_trash`
([native discovery](../../crates/tiny-ffi/src/clean.rs)).

`discover_checked` runs each provider in isolation and records found, app running, unavailable
(missing tool or failed probe) or failed. The CLI collapses that to its old report: unavailable
and failed categories vanish silently. The app keeps every outcome and also refuses `/`, the
home folder, its ancestors and tool-printed paths outside home
([`protected_reason`](../../crates/tiny-core/src/clean/trash_plan.rs)).

App gating has two mechanisms, both `pgrep -x` on a process name (`pnpm` adds a third check, below). `quit_apps()` (default
`requires_app_quit()`) skips the whole category while its app runs. `browser-caches`,
`chat-caches`, `streaming-caches` and `user-caches` instead drop only a running app's paths
inside `discover()` (`ScanContext::known_running`). Right before each move the app re-checks
`item_apps(path)` and `check_item(path)`. The [CLI executor](../../crates/tiny-core/src/clean/execute.rs)
also rechecks them with `RunnerProbe` before file mutations or tool-backed batches. A failed
probe still means "not running" during CLI discovery (`PgrepChecker`), but blocks execution;
to the app (`RunnerProbe`) it marks the category
unavailable or refuses the move, while per-path discovery keeps the path for that re-check
([`process.rs`](../../crates/tiny-core/src/clean/process.rs)).

`pnpm` runs as `node …/pnpm.cjs` under npm and Corepack installs, so a process name cannot show
it. Its `check_item` also asks `command_line_running`, a `pgrep -f` on the command line anchored
on a path component, so `pnpm-lock.yaml` and the store path do not match. `puppeteer` gates per
path: each build folder names its browser (`item_apps`), and an unrecognised path is gated on
every browser. `codex-releases` and `codex-runtimes` gate on the Codex processes and on ChatGPT
(which installs the runtime).

## Project build output

`node-modules` and `rust-targets` list build output inside projects. The CLI offers only projects
idle for `--idle-days`, as Review. The app sets `all_build_output` and lists every project, so the
Review default would hide most of the disk: an item is Safe when the project is in a git work tree
and its manifest, last commit and working tree are all older than `BUILD_OUTPUT_SAFE_DAYS` with
every fact checked, and Review when it is active, outside git (`NotGitRepo`: a manifest's age
alone proves nothing) or git cannot tell. The category stays Review, so Swift preselects exactly
the Safe items by risk. Do not add a separate "preselected" flag that bypasses risk.

A folder that holds a file git tracks is never offered, and `check_item` repeats the test through
`refuse_tracked` right before acting, in the CLI executor too. When git cannot list the tracked
files (no Command Line Tools), the folder is not offered either, and `FfiCleanCategory.refused`
says why with git's first stderr line. A git repository nested inside `node_modules` is not
detected ([`project_activity.rs`](../../crates/tiny-core/src/clean/providers/project_activity.rs)).

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

## The desktop's checked removal

The app never calls `CleanProvider::execute`. `clean_discover` keeps each candidate's path,
fingerprint and discovery roots in the session under an opaque ID, so Swift never sends a path
back. `clean_preview_with_mode` (`clean_preview` is the same with `Trash`) excludes paths the
[saved rules](#desktop-persistent-protection) hold, drops paths another selected path covers,
excludes any that would carry an unselected Review candidate along (the explicit-selection
contract) and fixes how each item will be removed. A preview is single-use and expires.
[`execute_checked`](../../crates/tiny-core/src/clean/checked_execute.rs) revalidates each path, and
everything it covers, just before acting, through the
[`Trash`](../../crates/tiny-core/src/clean/finder_trash.rs) seam and the other sinks below. An
Automation denial or cancellation stops the run. Scan and execute refuse to start when `HOME` is not
the account's home folder (`FfiError::UntrustedHome`).

**The delete mode is frozen in the preview.** `removal_for` in
[`trash_plan.rs`](../../crates/tiny-core/src/clean/trash_plan.rs) turns the mode, the item and
what it covers into a `Removal`; changing the mode means building a new preview. Merging never
widens: a cache folder that holds a path which only goes to the Trash goes to the Trash with it.

| `Removal` | Chosen when | Sink |
| --- | --- | --- |
| `MoveToTrash` | Trash mode; Your files always; a uv or pnpm cache in Trash mode | `FinderTrash` |
| `Delete` | Permanent mode and the item and all it covers are Rebuilt or Project build output | `SafeDelete` ([`fs_safe.rs`](../../crates/tiny-core/src/clean/fs_safe.rs)): no symlinks, no other volume |
| `RunTool(Prune)` | uv, pnpm in Permanent mode, tool found | the category's tool |
| `RunTool(Request)` | simulator runtimes, both modes (no Trash route) | `simctl runtime delete` |
| `EmptyTrash` | the `trash` row, both modes, only with `include_trash` | Finder |
| `DeleteAsAdministrator` | `needs_admin` categories, both modes | [`elevated_delete`](#deletion-as-administrator) |

`gated_items` rechecks the table at run time against the providers, so a plan can never delete what
only goes to the Trash, never delete a tool's store, and never give a `needs_admin` category any
other removal. `execution_order` runs the administrator batch first, then `Delete`, tool commands,
`EmptyTrash` and last `MoveToTrash`, so emptying the Trash never takes what the same run moved
there. Results stay in plan order. Nothing is retried another way: a failed move is not deleted, a
failed deletion is not moved or escalated. A failed `EmptyTrash` (a timeout included) skips every
`MoveToTrash` after it with `SafetyCheckFailed`, because Finder may still be emptying and would take
what the run moves in; an Automation denial stops the run as before. The empty script runs under
`with timeout of 540 seconds`, below the runner's 600, since AppleScript's default two minutes would
report a large Trash as failed while Finder keeps going.

`clean_execute` keeps its old report and refuses a preview that removes anything for good
(`InvalidInput`, unspent). `clean_remove` runs any preview and reports `FfiRemovalOutcome` per
item. It is the only place that builds the real `OsascriptDelete`, and it panics under
`cfg!(test)`; tests call `remove_clean` with fakes.

**Freed, moved and requested are three numbers.** Bytes freed are deleted items, the emptied Trash
and what each prune measured; bytes moved to the Trash free nothing until the Trash is emptied;
bytes requested belong to a tool that finishes in the background (simulator runtimes). A removal
counts as freed only when it ended `Deleted`, `TrashEmptied` or `Pruned`; a partial deletion is
`Failed` with `DeleteFailed` (`source_still_present`) and counts nothing, and a failed prune
(`ToolFailed`) counts nothing even if the tool pruned part of the cache. The selection total is the union of the
selected items: [`freed_size.rs`](../../crates/tiny-core/src/clean/freed_size.rs) counts a hard
link or clone family only when it lies wholly inside an item, so an inode split between two items
counts in neither. Discovery therefore keeps those `SharedBlocks` (`retain_completable` drops what
no selection could complete), and `freed_together` adds back the blocks that items share only with
each other. A prune frees at most its cache, so it makes the total an upper bound.

**Tool commands** need `tool_commands` in the options; without it every tool row stays
`ReportOnly{NotPerPathTrash}`, so an app that does not know the option cannot preselect uv into a
whole-cache Trash move. The command runs through the bounded `CommandRunner::output`
([`tool_command.rs`](../../crates/tiny-core/src/clean/providers/tool_command.rs)): absolute path,
stdin closed, no shell, never elevated, killed after ten minutes. The app's runner (`ToolRunner::new`)
starts each tool in its own process group and kills the whole group, because Corepack's pnpm runs the
real `pnpm-native` as a child that a plain kill would leave pruning; the CLI's runner keeps the
terminal's group so Ctrl-C reaches the tool. A prune is measured among the
slow checks, before the rules are read again, so those checks also cover the time the walk takes,
and once more afterwards. Freed bytes are `before - after` (saturating), outside the union. A
prune walks the cache twice. A tool that cannot be found keeps the row report only; the app never deletes a tool's store
by hand. `ToolLookup::app()` searches `PATH`, then the nvm `bin` folder for pnpm, then
`/opt/homebrew/bin` and `/usr/local/bin`, then per-user folders (uv in `~/.local/bin` and
`~/.cargo/bin`; pnpm in `~/Library/pnpm`, `~/.local/share/pnpm`, `~/.volta/bin`), because a
Finder-launched app has a minimal `PATH`. Every tool runs with `COREPACK_ENABLE_NETWORK=0` and
`COREPACK_ENABLE_DOWNLOAD_PROMPT=0` so a Corepack shim never downloads a package manager or waits
for an answer ([`runner.rs`](../../crates/tiny-core/src/runner.rs)).

**The Trash row** lists `~/.Trash` as one `Destructive` item. Its fingerprint also holds the
directory's mtime (`PathFingerprint::of_listing`), so a Trash that changed since discovery is
skipped instead of emptying files the user never saw.

**Placeholders.** A simulator runtime has no path: its item is `<simulator-runtime:UUID>` with
`Evidence::SimulatorRuntime`. Because every file-system check would resolve that relative name
against the working directory, `validate` skips only the file checks for it (rules, fingerprint,
volume, roots, protected paths) and still runs the app probe and `check_item`; only its tool acts
on it.

The [path validator](../../crates/tiny-core/src/clean/checked_execute.rs) checks every parent
with `symlink_metadata`, including parents above a discovery root. A provider's root is not
a trusted boundary: a tool may return the candidate itself as its root. A path containing
any symlink parent is skipped, including system aliases such as `/var`; the executor never
canonicalizes it into a different path to move.

## Simulators

`system-simulator-caches` is `/Library/Developer/CoreSimulator/Caches`, mostly `dyld/`: Safe,
`needs_admin`, refused while Xcode, Simulator, `xcodebuild`, `xctest`, `XCTRunner`, `simctl` or
`launchd_sim` (each booted device's init) runs (`SIMULATOR_ACTIVITY`, checked again by the root
shell after the dialog, which can stay open while a simulator boots headless) and, unless `xcrun
--find simctl` exits 72 (the only proof `simctl` is absent), while any simulator is booted. A
booted or unknown answer, including a failed lookup (a broken `DEVELOPER_DIR` exits 1, a hung
`xcrun` times out), refuses every entry with a reason rather than offering any. It never gates on
`simdiskimaged`, `CoreSimulatorService` or `SimLaunchHost`: they run without Xcode, so the caches
would never be offered.

`simulator-runtimes` joins `xcrun simctl runtime list -j` (keyed by image UUID) with
`list devices -j` (keyed by `runtimeIdentifier`) on `runtimeIdentifier`, never on display names:
the runtime list names an image by its full version ("iOS 26.4.1") while devices group under the
short one ("iOS 26.4"). An image is offered only when it is Ready, deletable, the only image for
its identifier and used by no device; `check_item` repeats the join before the delete. Deletion is
asynchronous and can stick in "Deleting", so it is reported as requested.

Without `simctl` (Xcode not installed), `simulator-runtime-images` reports the runtimes from
CoreSimulator's image registry and nothing more: only System Settings > General > Storage removes
them. It reads one key path at a time with `plutil -extract`, because `plutil -convert json`
rejects the registry's `<date>` and `<data>` values. The disk images lie under
`/System/Library/AssetsV2`, which TinySift only sizes.

Cleanup never touches `/System`. `inside_system` refuses it (case-insensitively, and any `..`) in
`execute_per_item`, the CLI per-item guard, `validate` and the administrator route's allowlist.

## Deletion as administrator

[`elevated_delete.rs`](../../crates/tiny-core/src/clean/elevated_delete.rs) documents the route;
this is why it is shaped that way. A helper would be a resident root process, which the spec rules
out, and a bundled binary run as root is unsafe because `/Applications/TinySift.app` is
user-owned. One `/usr/bin/osascript` call with a constant script shows one macOS dialog per Clean
and keeps nothing; the root shell runs only `/bin/sh`, `/usr/bin/pgrep` and `/bin/rm`.

- **The batch runs first.** After the same `prepare()` checks every item gets, one call takes all
  items that passed. Cancelling the dialog (AppleScript error -128) changes nothing: the run stops
  with `AuthenticationCancelled`, every item is `NotAttempted`, and no other sink runs (OC3).
- **The allowlist is checked twice.** Unprivileged, `check_system_cache_path` accepts only an
  absolute, UTF-8 path without NUL, outside `/System`, directly inside `CACHES_ROOT`, not a symlink,
  under real folders; one refusal refuses the whole batch before any dialog. The root shell
  receives bare names, never paths, and checks again: `cd -P` into the root and `pwd -P` equal to
  it, the name is not empty, `.`, `..` or containing `/`, and `[ -L ]` is false.
- **The root shell guards on running tools.** Exit 75 means a simulator tool is running (the name
  is on stderr) and becomes `Skipped(AppRunning)`; exit 70 means the guard or the root check could
  not run and becomes a failure. The fixed `PATH` and `env_clear` keep this process's environment
  out of the root shell. There is no timeout, because the dialog waits for the user, so the UI must
  read as waiting for authentication.
- **The outcome is what is left on disk.** After the call each path is `lstat`ed unprivileged:
  gone is `Deleted` and freed, present is `DeleteFailed`. The script's exit status is never trusted
  alone.
- **A failure never escalates.** A failed administrator deletion is not retried another way, and no
  other failure is ever retried as administrator.

The CLI never runs `sudo`: `execute` refuses a `needs_admin` category with
`needs administrator rights; the tiny CLI never runs sudo`.

## The desktop's checked uninstall

[`tiny-ffi/src/uninstall.rs`](../../crates/tiny-ffi/src/uninstall.rs) is the app's uninstall,
built like Clean's Trash path: Swift names IDs and never sends a path.

1. `uninstall_list_apps` lists the installed apps under opaque app IDs (name, bundle id, path for
   display, size, last-used days, and a `blocked_reason` for a system app or Homebrew cask). It
   replaces the previous listing and any preview.
2. `uninstall_preview(app_id)` reads the app's identity again, builds core's `Plan` with the fresh
   installed-app inventory (the O5 twin check), freezes it with `build_checked_plan` (trusted home,
   `/Applications`, the bundle's `CFBundleExecutable`) and stores it under a single-use preview ID
   that expires after 15 minutes. It reports the bundle id it used and whether leftovers were
   searched (`leftovers_searched`; with a missing or invalid id, O1, only the app is listed and
   `leftovers_skipped_reason` says why). Items carry opaque item IDs, the ownership class (app,
   cache, data, report-only or uncertain with its reason) and `selectable`. A path held by a
   saved "always keep" rule, by Tiny's own storage or by an undecidable rule check shows `held_by`
   and is not selectable, like Clean. The reply also says whether the app is running now
   (`Running`, `NotRunning` or `Unknown`). A new preview replaces the old one.
3. `uninstall_execute(preview_id, item_ids, token, progress)` takes the exclusive rules lease
   (`Busy` or `ProtectionUnavailable` spend nothing), then refuses with `InvalidInput`, still
   without spending the preview, for an unknown item ID, an empty selection, a report-only,
   uncertain or held item, or a selection without the app bundle (the desktop never removes
   leftovers alone). Rules that differ from the preview's snapshot give `ProtectionChanged` and
   drop the preview. It then consumes the preview, repeats the O5 check with a fresh inventory
   when a leftover is selected (another installed copy of the bundle id, or an unreadable
   inventory, refuses; a bundle-only run is not rechecked) and runs
   [`execute_checked_uninstall`](../../crates/tiny-core/src/uninstall/checked.rs) through the
   `Trash` seam. Only refusals after the preview is spent (`AppRunning`, `AppCheckFailed`,
   `ExecutableUnknown`, the twin check, `InconsistentPlan`) arrive as
   `FfiError::UninstallRefused` with a typed `FfiUninstallRefusal`; the client must build a new
   preview. Once moving starts the reply is a per-item report (`MovedToTrash`, `Failed` with
   `source_still_present`, `Skipped` with a reason, `NotAttempted`) plus an optional
   `FfiUninstallStop` (cancellation, Automation denial, rules changed or unreadable, the app
   started mid-run). The run-time `Kept` skip stays as defence in depth.

List, preview and execute share the session gate with Clean and Storage; process reads stay
outside it. Preview and execute also take the `UntrustedHome` check and the saved-rules store;
the list reads neither. Lock order is the gate, the
rules lease, then the short uninstall-state lock, which is never held across a probe, Trash call
or callback. Tests inject the inventory (`AppCatalog`), probe and Trash, so none reads
`/Applications`, calls `defaults`/`mdls` or Finder.

## Desktop persistent protection

Always keep is a policy owned by Rust; only the desktop app edits it. A
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
[CLI executor](../../crates/tiny-core/src/clean/execute.rs) loads the rules read-only before
acting, reports every item they do not allow as kept, and refuses the whole run when they
cannot be read.

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
desktop path that moves files must go through a checked executor with a lease, or it bypasses
the rules. There are two: `execute_checked`, and
[`execute_checked_uninstall`](../../crates/tiny-core/src/uninstall/checked.rs) for an app and its
leftovers. The second takes a plan frozen at discovery (fingerprint, ownership and expected
container per item) plus a set of item indexes, never a path. Report-only and uncertain items
refuse the whole run; the app must not be running (its `CFBundleExecutable`, unknown means
refuse); the bundle moves first, so a bundle that does not move leaves every leftover untouched.
Leftovers-only runs are allowed. The bundle too must be on the home volume (Finder may delete
permanently on a volume without a Trash). The plan must be built at discovery, because its
fingerprints are taken there; build it only from a core-built `Plan`, never from client input.
Every plan item keeps its index, so an unreadable leftover is skipped, not dropped. The per-item
order matches Clean's, and it shares the policy, protection and ancestor-walk helpers of
`checked_execute`, so a change to those applies to both.

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
  (with a fake `Trash`, `Delete` and `ElevatedDelete`) take those seams; so do `discover_clean`,
  `execute_clean` and `remove_clean` in `tiny-ffi`. `clean_remove` itself panics under
  `cfg!(test)`, so no test can reach the real dialog. Native clients must inject fake cleanup engines in their UI tests.

[`clean_smoke.rs`](../../crates/tiny/tests/clean_smoke.rs) runs only `--dry-run` and validation
failures, so it passes on any machine without touching data.
