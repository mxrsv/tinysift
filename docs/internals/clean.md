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
never set process environment. Core repeats `idle_days > 0` (`validate_options`), because
the app reaches discovery without clap, and refuses a `build_output_safe_days` below
`BUILD_OUTPUT_SAFE_DAYS`, which only the app sets.

## The provider registry

A category exists only when its id appears in `all_providers_with`, `known_category_ids`,
`category_family` and `comes_back` in
[`providers/mod.rs`](../../crates/tiny-core/src/clean/providers/mod.rs), and in the `SECTIONS`
table test beside them. Tests require the providers in `known_category_ids` order, each with an
`inclusion_reason` and a `comes_back` answer; `category_family` panics on an unknown id.
`category_section` derives the desktop section from `comes_back` (Rebuilt, or Your files when
nothing recreates the items), except `node-modules`, `rust-targets` and `build-folders`, which are
Project build output. The `SECTIONS` test fails until a new id gets a deliberate section, so build output never
lands in Rebuilt just because a build restores it.

`clean_places(options)` (`FfiCleanPlace`: `id`, `label`) lists the categories `clean_discover` would
scan with the same options, from the same `select_providers_with`, so a count shown before a scan is
never hard-coded. It is the most a scan can return: a category whose tool or folder is absent
(`available()` false without a `required_tool`) is dropped from the scan result. The
`places` tests fail when the list drifts from the registry, the visibility filter or `category_section`.

Desktop eligibility is deny by default. `desktop_trash_paths()` is true only when cleanup is
exactly "move each listed path to Trash", and `Destructive` risk is always report only
(`desktop_report_only`). Two narrow exceptions each have their own provider flag and their own
sink: `desktop_empties_trash()` (only `trash`) and `desktop_tool()` (uv, pnpm, Bun, pip,
Homebrew, Docker, simulator devices and runtimes). `desktop_report_only` keeps its answer for them (`Destructive`, `NotPerPathTrash`): it
means "no per-path unprivileged route", and the flag decides what the app may do instead.
`needs_admin()` (system simulator caches) stays `NeedsAdministrator`, report-only
([why](#deletion-as-administrator)). The `DESKTOP_TRASH` table test fails
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

The project providers (`node-modules`, `rust-targets`, `python-caches`, `build-folders`) share one
walk of each dev root per scan ([`project_walk.rs`](../../crates/tiny-core/src/clean/providers/project_walk.rs),
kept in the `ScanContext`): each walker keeps its own rule of what it records and where it stops,
the walk goes into a folder while any walker wants it, and unreadable entries count only for the
walkers that read them. A rule changes there, never in a provider's own walk. A project's git
activity is asked once per scan too, so a `package.json` judged for `node_modules` is not asked
again for `.next` or `dist`.

`node-modules`, `rust-targets` and `build-folders` list build output inside projects.
`build-folders` offers `.next`, `Pods`, `build` and `dist` only beside the manifest that proves
them generated, and its `check_item` repeats that proof
([`build_folders.rs`](../../crates/tiny-core/src/clean/providers/build_folders.rs)). The CLI offers only projects
idle for `--idle-days`, as Review. The app sets `all_build_output` and lists every project, so the
Review default would hide most of the disk: an item is Safe when the project is in a git work tree
and its manifest, last commit and working tree are all older than `build_output_safe_days` (the
app's choice, 7 by default and never fewer) with every fact checked, and Review when it is active, outside git (`NotGitRepo`: a manifest's age
alone proves nothing) or git cannot tell. The category stays Review, so Swift preselects exactly
the Safe items by risk. Do not add a separate "preselected" flag that bypasses risk.

A folder that holds a file git tracks is never offered, and `check_item` repeats the test through
`refuse_tracked` right before acting, in the CLI executor too. When git cannot list the tracked
files (no Command Line Tools), the folder is not offered either, and `FfiCleanCategory.refused`
says why with git's first stderr line. A git repository nested inside `node_modules` is not
detected ([`project_activity.rs`](../../crates/tiny-core/src/clean/providers/project_activity.rs)).

A scan can be days old when Clean runs, and `npm install` keeps a folder's inode, so the
fingerprint does not notice a project back in use. Right before acting on build output offered
as Safe, or on a virtualenv, the desktop executor asks `recheck_idle`, which judges the project
again by the same rule (`refuse_if_used_since`) and skips it with "its project was used since the
scan" ([`checked_execute.rs`](../../crates/tiny-core/src/clean/checked_execute.rs)). One context
lasts the whole run, so a project is judged once however many of its folders go. Review build
output, an active project the user ticked on purpose, is not judged again. The scan itself still
has no age: sizes and the Review sheet's idle days are the scan's.

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

## A scan replaces only the categories it covers

`clean_discover` with category IDs scans those categories and, only when the scan completes,
replaces their candidates ([store](../../crates/tiny-ffi/src/clean/discovery_store.rs)). Every
other category keeps its candidates, IDs and results; an empty list scans by risk flags and
replaces everything, which is what a caller that always scans everything sees. The rest follows
from that, and each part is a trap if it slips:

- **Nothing is stored before the scan completes.** A cancelled or failed scan leaves every
  category and every preview as it was. A category the scan covered but did not report (its tool
  or folder is absent) is dropped, because the scan replaced it; one reported as running, failed
  or unavailable shows that state, not its earlier result.
- **Candidate IDs are `d{scan}-{n}` with a session-unique scan number**, so a replaced
  category's old IDs are refused ("scan again") and a kept category's IDs stay valid. Never
  derive an ID from the merged set's size.
- **Each candidate holds the options its scan used** (`idle_days`, `all_build_output`, ...).
  [`providers_scanned_with`](../../crates/tiny-ffi/src/clean/removal.rs) builds providers once
  per distinct options value and keeps only the categories scanned with it, so a category is never
  guarded by another scan's options. The categories counted are those of the run's items and of
  every candidate an item covers, which the executor gates too; a covered category left out is
  skipped as `UnknownCategory`. Options are compared without the category list, which no provider
  reads, so scans with equal settings share one build.
- **Derived values come from the merged set.** Each candidate keeps every shared block its walk
  saw (`shared_seen`); `shared` is recomputed from them after each merge, because a block one
  scan cannot complete another can. Covered and overlapping paths are computed at preview time
  over the merged set.
- **A preview is stale, not replaced, by a rescan.** `consume_preview` refuses it when a
  selected candidate is gone or the set of candidates under an item's path changed since the
  preview was built; a preview of untouched categories runs until its TTL. The operation gate is
  unchanged: a scan and a run never overlap.

## The desktop's checked removal

The app never calls `CleanProvider::execute`. `clean_discover` keeps each candidate's path,
fingerprint and discovery roots in the session under an opaque ID, so Swift never sends a path
back. `clean_preview_with_mode` (`clean_preview` is the same with `Trash`) excludes paths the
[saved rules](#desktop-persistent-protection) hold, drops paths another selected path covers,
excludes any that would carry an unselected Review candidate along (the explicit-selection
contract) and fixes how each item will be removed. A preview is single-use and expires.
[`execute_checked`](../../crates/tiny-core/src/clean/checked_execute.rs) revalidates each path, and
everything it covers, just before acting, through the
[`Trash`](../../crates/tiny-core/src/clean/finder_trash.rs) seam and the other sinks below.
Cancellation, and an Automation denial of a move (a Finder route, as uninstall's), stops the run. Scan and execute refuse to start when `HOME` is not
the account's home folder (`FfiError::UntrustedHome`).

**The delete mode is frozen in the preview.** `removal_for` in
[`trash_plan.rs`](../../crates/tiny-core/src/clean/trash_plan.rs) turns the mode, the item and
what it covers into a `Removal`; changing the mode means building a new preview. Merging never
widens: a cache folder that holds a path which only goes to the Trash goes to the Trash with it.

| `Removal` | Chosen when | Sink |
| --- | --- | --- |
| `MoveToTrash` | Trash mode; Your files always; a uv, pnpm, Bun or pip cache in Trash mode | [`NativeTrash`](../../crates/tiny-core/src/clean/native_trash.rs): `NSFileManager trashItemAtURL`, no Finder, no Put Back |
| `Delete` | Permanent mode and the item and all it covers are Rebuilt or Project build output | `SafeDelete` ([`fs_safe.rs`](../../crates/tiny-core/src/clean/fs_safe.rs)): no symlinks, no other volume |
| `RunTool(Prune)` | uv, pnpm, Bun, pip in Permanent mode, tool found | the category's tool |
| `RunTool(Request)` | simulator runtimes, both modes (no Trash route) | `simctl runtime delete` |
| `RunTool(Remove)` | Homebrew, Docker, simulator devices, both modes (no Trash route), tool found | `brew cleanup --prune=30`, `docker … prune --filter until=168h`, `simctl delete <udid>` |
| `EmptyTrash` | the `trash` row, both modes, only with `include_trash` | Finder |
| `DeleteAsAdministrator` | none at present: `needs_admin` categories are report-only on the desktop | [`elevated_delete`](#deletion-as-administrator) |

The ordinary permanent-delete [walker](../../crates/tiny-core/src/clean/fs_safe/remove_tree.rs)
opens each ancestor and directory relative to a pinned parent descriptor with `O_NOFOLLOW`.
It checks the opened directory's identity and device before reading its entries; child mutations
use single names with `unlinkat`, so replacing a directory with a symlink cannot redirect the
walk to its target. There is no pathname fallback. Clean's `Delete` is best effort
(`remove_recursive_one_device_each`, `Delete::delete_each`): an entry that fails (locked, denied,
replaced, on another device) stays, is recorded with its error, and its siblings are still removed;
the folders holding it then stay with `ENOTEMPTY`, which is not recorded as a second failure.
Failures at the item's ancestors or the item itself leave everything, and the walk starts only
from the device and inode discovery fingerprinted (`FileIdentity`, compared on the
descriptor-relative stat), so a root replaced after the last check is refused whole. A child gone
between its stat and its open counts as removed; one turned into a symlink or a file is refused as
changed. Uninstall and the CLI keep
the fail-fast form (`remove_recursive_*`, `Delete::delete`), which stops at the first error.
This is not an atomic filesystem transaction: a real directory substituted before the initial
identity read can still be selected; an opened directory renamed elsewhere remains the same
object and can still be traversed. The final identity check and `unlinkat` are separate calls,
so a last-moment replacement entry can be unlinked, but its symlink target is never followed.
Concurrent additions leave their folder with `ENOTEMPTY`, recorded as a failure.

`gated_items` rechecks the table at run time against the providers, so a plan can never delete what
only goes to the Trash, never delete a tool's store, and never give a `needs_admin` category any
other removal. `execution_order` runs the administrator batch first, then `Delete`, prunes and
requests, `EmptyTrash`, `MoveToTrash` and last `RunTool(Remove)`, so emptying the Trash never takes
what the same run moved there, and a folder moved in the run (App Caches' `Homebrew` in Trash mode)
moves whole before `brew cleanup` could empty part of it, which would leave its moved bytes
overcounted. Results stay in plan order. Nothing is retried another way: a failed move is not deleted, a
failed deletion is not moved or escalated. A failed `EmptyTrash` (a timeout included) skips every
`MoveToTrash` after it with `SafetyCheckFailed`, because Finder may still be emptying and would take
what the run moves in. A denied Empty Trash only fails its row: Finder never started, and moves need
no Automation, so they go on. The empty script runs under
`with timeout of 540 seconds`, below the runner's 600, since AppleScript's default two minutes would
report a large Trash as failed while Finder keeps going.

`clean_execute` keeps its old report and refuses a preview that removes anything for good
(`InvalidInput`, unspent). `clean_remove` runs any preview and reports `FfiRemovalOutcome` per
item. It is the only place that builds the real `OsascriptDelete`, and it panics under
`cfg!(test)`; tests call `remove_clean` with fakes.

**Freed, moved and requested are three numbers.** Bytes freed are measured, never discovery
sizes. A `Delete` counts them itself: the best-effort walker applies the
[`freed_size.rs`](../../crates/tiny-core/src/clean/freed_size.rs) rule to each regular file as it
unlinks it (its `fstatat` blocks and links, one descriptor-relative `getattrlistat` for APFS
sharing), so a hard-linked inode counts once its last link in the item goes and a clone family once
every member goes (`TreeRemoval::freed`); only a failed deletion walks what it left. Removals done
by another process (`DeleteAsAdministrator`, Finder's `EmptyTrash`, `RunTool(Prune)`,
`RunTool(Remove)`) cannot be counted from here, so right before each, among the slow checks, the
item is measured by the same rule and once more right after it
([`measure.rs`](../../crates/tiny-core/src/clean/checked_execute/measure.rs)); the item freed
`before - after` (saturating). The run's `bytes_freed` is the sum. A `RunTool(Remove)` item
that names no file (Homebrew's `<homebrew:cleanup>`, Docker's placeholders) is measured by its
provider instead (`CleanProvider::tool_freeable`): Homebrew sizes its dry run's paths again (the
scan's list while it is fresh, so neither measurement spawns `brew`),
Docker reads its kind's Reclaimable; one that cannot tell counts 0, as a ceiling. Items are measured one after
another, so a hard link or clone family split between two removed items counts with the second,
once. Bytes moved to the Trash free nothing until the Trash is emptied; bytes requested belong to a
tool that finishes in the background (simulator runtimes). Each result carries a `RemovalExtent`:
`Removed`, `PartlyRemoved` (a failed removal that freed bytes or removed an entry; its
`bytes_left` is what is still there), `NotRemoved`, `MovedToTrash` or `Requested`. A partly removed
item is still `DeleteFailed`, `Failed` or `ToolFailed`, and counts what was measured as freed.

**Every failure has one cause.** `ItemResult::failure` (FFI `cause` and `raw_error`) carries a
[`FailureCause`](../../crates/tiny-core/src/clean/failure_cause.rs) beside the raw error text, for
a skip, a failure and a partial removal (its first entry left). An `io::Error` maps by errno:
`ENOENT` already gone, `EBUSY`/`ETXTBSY` in use, `EROFS` protected by macOS, `ENOTEMPTY`, `ELOOP`,
`ENOTDIR` and a refused replacement changed since the scan. `EPERM`/`EACCES` look at the entry and
its folder again: a system flag on either (`restricted`, `schg`, `sappnd`, `sunlnk`) is protected by
macOS, a lock the user set (`uchg`) on either is other, another owner of either needs an
administrator, anything else is no permission (Full Disk Access). A denied Empty Trash is its own
cause, `AutomationDenied`, since only the Automation setting fixes it. Skip reasons map too (`AppRunning` in use, `Changed` changed since
the scan, `/System` protected); an item the user's saved rule keeps has no failure. The raw text is
for the run report only, never the screen.

The preview's selection total is the union of the selected items: `freed_size.rs` counts a hard
link or clone family only when it lies wholly inside an item, so an inode split between two items
counts in neither. Discovery therefore keeps those `SharedBlocks` (`retain_completable` drops what
no selection could complete), and `freed_together` adds back the blocks that items share only with
each other.

**Figures count only what surely goes.** The preview's `bytes_to_free` leaves out prunes: neither
`uv cache prune` (uv 0.8.16: only `--ci`) nor `pnpm store prune` (pnpm 12.18.0: only `-y`) has a dry
run, so a prune has no estimate, and its cache's size is `bytes_prune_up_to`, a ceiling the client
shows as "up to" and keeps out of its button; Bun and pip, which have no dry run either, and
Docker (`DesktopTool::Remove { sure: false }`: Reclaimable counts data its filtered prune keeps)
join it, and the FFI marks all of them `prunes`. A tool removal whose size is its dry run's
estimate (`Remove { sure: true }`: Homebrew, simulator devices sized on disk) counts in
`bytes_to_free` and in `found_bytes`. Requested removals and moves are never in it either.
The scan's progress figure (`found_bytes`) counts only categories whose `desktop_action` is
`MoveToTrash` without a tool, `EmptyTrash`, or `RemoveWithTool` with a sure size
(`discover_checked_counting`); the CLI still counts
every category. Docker's figure is `docker system df`'s Reclaimable column, never Size.

**Tool commands** need `tool_commands` in the options; without it every tool row stays
`ReportOnly{NotPerPathTrash}` (`FfiDesktopAction::RemoveWithTool` with it, for `Remove` tools), so an app that does not know the option cannot preselect uv into a
whole-cache Trash move. The command runs through the bounded `CommandRunner::output`
([`tool_command.rs`](../../crates/tiny-core/src/clean/providers/tool_command.rs)): absolute path,
stdin closed, no shell, never elevated, killed after ten minutes. The app's runner (`ToolRunner::new`)
starts each tool in its own process group and kills the whole group, because Corepack's pnpm runs the
real `pnpm-native` as a child that a plain kill would leave pruning; the CLI's runner keeps the
terminal's group so Ctrl-C reaches the tool. A prune is measured among the
slow checks like every removal that frees space, and once more afterwards. A prune walks the
cache twice. A tool that cannot be found keeps the row report only; the app never deletes a tool's store
by hand. `ToolLookup::app()` searches `PATH`, then the nvm `bin` folder for pnpm, then
`/opt/homebrew/bin` and `/usr/local/bin`, then per-user folders (uv in `~/.local/bin` and
`~/.cargo/bin`; pnpm in `~/Library/pnpm`, `~/.local/share/pnpm`, `~/.volta/bin`), because a
Finder-launched app has a minimal `PATH`. `pip-cache` names its `pip3` by absolute path instead
(Homebrew's, then the Command Line Tools'), never the `/usr/bin` shim. Every tool runs with `COREPACK_ENABLE_NETWORK=0` and
`COREPACK_ENABLE_DOWNLOAD_PROMPT=0` so a Corepack shim never downloads a package manager or waits
for an answer ([`runner.rs`](../../crates/tiny-core/src/runner.rs)).

**The Trash row** lists `~/.Trash` as one `Destructive` item. Its fingerprint also holds the
directory's mtime (`PathFingerprint::of_listing`), so a Trash that changed since discovery is
skipped instead of emptying files the user never saw.

**User data in cache-looking places (SF1).** `user-caches`, `npm` and `yarn` skip the entries of
[`never_cache.rs`](../../crates/tiny-core/src/clean/providers/never_cache.rs) (Spotify, Cypress,
Playwright, Camoufox, Buzz, llama.cpp, Kitty, Poetry virtualenvs, renv, pnpm `dlx`, `npx`, Yarn Berry), and a folder that holds
one is not offered whole (`npm` offers its other children). A category that names a place on purpose
(`puppeteer`, `huggingface`) is not a broad sweep and does not use the list.

**Re-check before a move (SF2).** `validate` skips an item whose owning app runs (`AppRunning`, cause
`InUse`) and one whose fingerprint differs (`Changed`, cause `ChangedSinceScan`): a file's size or
mtime, a folder's identity. A folder's contents are not compared, since caches churn between scan and
confirmation (only the Trash row also compares its listing), and no check asks whether a process holds
a file open.

**Placeholders.** A simulator runtime has no path: its item is `<simulator-runtime:UUID>` with
`Evidence::SimulatorRuntime`. Homebrew's cleanup (`<homebrew:cleanup>`) and Docker's kinds
(`<docker:images>`, `<docker:build-cache>`) are placeholders too, removed by `RunTool(Remove)`. Because every file-system check would resolve that relative name
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

`simulator-devices` lists the devices `simctl list devices -j` reports unavailable, as Review in
Rebuilt (such a device cannot boot; Xcode makes devices again for an installed runtime). The
desktop deletes each reviewed one with `xcrun simctl delete <udid>` after checking `simctl` still
lists it unavailable (one list per run, at most 30 s old; `xcrun --find` once per provider), never `simctl delete unavailable`, which would also take unticked devices.
It offers the tool only with a Devices folder, so a Mac without one spawns no `xcrun`.

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

**The desktop no longer routes anything here.** macOS denies the deletion even as root: the
kernel logs `System Policy: rm deny(1) file-write-unlink` for
`/Library/Developer/CoreSimulator/Caches`, because the platform policy asks TCC for Full Disk
Access and `do shell script … with administrator privileges` runs through
`Security.framework/authtrampoline`, which TCC treats as the responsible process instead of
TinySift. The app's own Full Disk Access therefore never applies, and the password dialog always
ends in `Operation not permitted`. [`desktop_action`](../../crates/tiny-ffi/src/clean.rs) reports
`needs_admin` categories as `ReportOnly { NeedsAdministrator }` instead; the route and its FFI
records stay until the app drops them.

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

List, preview and execute share the session gate with Clean and Storage; process reads and
signals ([`processes.rs`](../../crates/tiny-ffi/src/processes.rs)) stay outside it. Preview and execute also take the `UntrustedHome` check and the saved-rules store;
the list reads neither. Lock order is the gate, the
rules lease, then the short uninstall-state lock, which is never held across a probe, Trash call
or callback. Tests inject the inventory (`AppCatalog`), probe and Trash, so none reads
`/Applications`, calls `defaults`/`mdls` or Finder.

## Large Files

[`tiny-ffi/src/large_files.rs`](../../crates/tiny-ffi/src/large_files.rs) is the app's Large Files tab (Storage Explorer),
built on [`tiny-core/src/clean/large_files/`](../../crates/tiny-core/src/clean/large_files/mod.rs). It follows Clean's
consent contract and adds no second removal path: Swift names file, preview and move IDs, never a path to act on, and
every move goes through `Trash` (the app passes `NativeTrash`). Nothing here deletes for good.

| Call | Gate | What it does |
| --- | --- | --- |
| `large_files_scan(token, progress)` | takes it | Read-only. Replaces the earlier scan and any preview. Returns `FfiLargeFile` records (`id`, `path`, `name`, `group`, `reason`, `frees_bytes`, `frees_is_exact`, `matched_app_path`, and for group 5 `kind`, `app_name`, `app_bundle_id`, `app_running`, `preview_path`, `shared_bytes`, `last_changed`, and for group 1 `keeper_path`, `keeper_bytes`, `keep_rule`, `duration_ms`, `keeper_duration_ms`, `width`, `height`, `audio_tracks`) and `unreadable_folders`. |
| `large_files_preview(file_ids)` | none | Builds the one preview from IDs of the current scan. Unknown IDs and an empty list are refused; a file a saved "always keep" rule holds is returned in `held_ids` and left out. Unreadable rules fail it (`ProtectionUnavailable`). Replaces any earlier preview. |
| `large_files_move(preview_id, token, progress)` | takes it | Spends the preview, then moves. Returns one result per item (`FfiItemOutcome`, `FfiFailureCause`, `bytes_moved`, `trashed_path`) and a `move_id`. |
| `large_files_undo(move_id)` | takes it | Puts the move's files back. One outcome per file. |

**Preview and session.** A preview expires after `PREVIEW_TTL` (15 minutes, on two clocks), is spent before the first
move (a replay is `PreviewInvalid`, "already used"), is stale once a rescan replaces the list, and is dropped
(`ProtectionChanged`) when the saved rules differ from those it was authorised against. The lease on the saved rules is
taken before the preview is spent, so a busy or unreadable store fails without spending it. Only the preview's IDs
move; a move that is `Busy` leaves the preview usable. Lock order: session gate, saved-rules lease, then a short state
lock that never spans a Trash call or callback.

**Checks before each file** ([`move_run.rs`](../../crates/tiny-core/src/clean/large_files/move_run.rs)), in Clean's
order and with its pieces: cancellation, the rules unchanged, `/System`, the saved rules for the path, the path still
the file the scan saw (`PathFingerprint`: a file's size and mtime), not a symlink, on the home volume, inside a folder the
scan listed through real folders, and not a protected path; then the rules again and cancellation. A refusal is a skip
with its RM2 cause (`failure_cause.rs`) and the run goes on; cancellation, a denied Automation or a changed rule set
stops it and the rest are `NotAttempted`. The open-file check and a running-app check are not done (an installer has no
owning app; the open-file check is the plan's backlog item).

**What a move reports.** A move frees nothing by itself (FG3). `bytes_moved` is the file's private allocated bytes
measured right before the move (`private_allocated_bytes`: blocks shared with an APFS clone do not count, a hard link
counts once), i.e. what emptying the Trash frees; `bytes_moved_is_upper_bound` is set when the volume could not vouch
for every block. `frees_is_exact` on a record has the same meaning.

**Undo** ([`undo.rs`](../../crates/tiny-core/src/clean/large_files/undo.rs)) uses the Trash location the move
reported (`Trash::move_to_trash_reporting`; `NativeTrash` returns Foundation's resulting URL) and the file's
fingerprint from the move. Per item it refuses, and says why, when the location is not in a `.Trash`/`.Trashes`, the
item is gone from the Trash, something else is there now, the original folder is gone, or the original path is taken.
It renames with `renamex_np(RENAME_EXCL)`, so a file created between the check and the move is never replaced. Items
that went back return to the scan's list (same IDs); items that did not stay in the move and can be tried again. A
later move replaces what Undo takes. Emptying the Trash (the app asks Finder through the Trash row) ends Undo.

**Scan, step 1: Installers** ([`installers.rs`](../../crates/tiny-core/src/clean/large_files/installers.rs)). Direct
children of `~/Downloads` and `~/Desktop` ending in `.dmg`, `.pkg`, `.iso` or `.xip` (regular files, not symlinks) whose
private bytes are at least `INSTALLER_MIN_BYTES` (100 MB). A file is listed only when an app bundle directly inside
`/Applications` or `~/Applications` has the same normalised name
([`name_match.rs`](../../crates/tiny-core/src/clean/large_files/name_match.rs): lower case, separators ignored, version,
architecture and "installer" tokens cut after the first word). The reason is `<App> is installed in Applications`, or
`A newer <App> is installed in Applications` only when both versions parse and the installed
`CFBundleShortVersionString` (read with `plutil`) is higher. Anything else is not this group's. A file that frees
0 bytes is not offered. Tests inject the roots, the threshold and the version table (`InstallerRoots`,
`scan_installers_above`, `AppVersions`), so they never read the real home.

**Scan, step 2: Only copy on this Mac and Other large files**
([`discovery.rs`](../../crates/tiny-core/src/clean/large_files/discovery.rs)). One read-only walk of the home
(`scan_home`): folder listings and file metadata only, nothing hashed, cancellable, progress every 2000 entries. A file
is looked at only when its allocated blocks reach the smaller threshold; then its private bytes must be above 0 and reach
the group's threshold, and a hard link's twin counts once (a file whose private bytes are 0 goes to group 5 instead, see
step 3). A video (`mp4 mov mkv m4v avi webm`) of 500 MB or more is
`OnlyCopy` ("only_copy"), reason `Video · changed N days ago`; the spec fixes no threshold for this group, so it takes
group 4's. Any other file of 500 MB or more whose modification date is 90 days old is `OtherLarge` ("other_large"),
reason `<kind> · not changed for N days`; Spotlight's last-used date (`mdls`, asked only for a file that already passed
every other test) wins when newer, and then the reason says `not used for N days`. The kind comes from the extension
(`Disk image`, `Archive`, ... or `<EXT> file`). A file an installer row claimed is left out; an installer with no
installed app can land in group 4. Never listed (NL1), and never entered: symlinks (nothing outside the home is
reached), hidden names, `~/Library`, `~/Music/Music`, `~/Movies/TV`, app bundles and package libraries by extension
(`app photoslibrary musiclibrary tvlibrary fcpbundle imovielibrary ...`), VM bundles (`utm vmwarevm pvm sparsebundle`),
Smart Clean's build-output folder names (`node_modules target build DerivedData Pods ...`), a folder with a `.git` entry
(the home itself is exempt, so a dotfiles repo does not hide everything), and an iCloud placeholder (`SF_DATALESS`).
The walk is added to the move's scanned roots, so a group 3 or 4 file moves and undoes through the same checks.
Tests inject the limits, "now", the Spotlight table and the claimed set (`Limits`, `HomeWalk`, `LastUsed`).

**Scan, step 2b: Copies, the original stays**
([`copies.rs`](../../crates/tiny-core/src/clean/large_files/copies.rs),
[`video_facts.rs`](../../crates/tiny-core/src/clean/large_files/video_facts.rs)). `Group::Copies` ("copies") is
decided inside `scan_home`, after the walk: the walk keeps every video (`mp4 mov mkv m4v avi webm`) whose allocated
blocks reach `Limits::copy_min_bytes` (100 MB), and `find_pairs` groups them by stem and parent's parent folder (one
folder, or sibling folders such as `Raw/x.mp4` and `Backup_MKV/x.mkv`). Headers are read only for a group of two or more:
`matroska` for `.mkv`, `mp4` for `.mp4 .mov .m4v`, header only, never decoding. The facts are length, picture size of the
first video track and audio-track count; a file the parser cannot read, that panics (caught) or that lacks one fact
(an MKV without a Duration, an MP4 without `moov`) is never in a pair. A pair needs lengths under 500 ms apart and equal
size and track count. The file that stays: `.mp4`, else `.mov`, else the larger file (two of one kind: the larger, then
the path); `KeepRule` names which. The other file is the row (`copy: Some(CopyOf)`): it leaves groups 3 and 4, needs
100 MB of private bytes (a clone that frees nothing stays in group 5), and `reason` is words only
(`Same length and resolution as <file name>`); the app builds its sentence from the `FfiLargeFile` fields
`keeper_path`, `keeper_bytes`, `keep_rule`, `duration_ms`, `keeper_duration_ms`, `width`, `height`, `audio_tracks`. The
file that stays remains in whatever group it was in. Both halves need 100 MB, so a small proxy is not an original. Safety:
`PlannedMove::must_remain` makes the move skip (`Changed`) when the file that stays is gone or changed since the scan, and
`large_files_preview` refuses a request that holds a copy together with its keeper (`InvalidInput`).

**Scan, step 3: Remove these elsewhere**
([`elsewhere.rs`](../../crates/tiny-core/src/clean/large_files/elsewhere.rs)). `Group::Elsewhere` ("elsewhere") is
listed and never movable: `Group::movable()` is false and `large_files_preview` refuses its IDs (`InvalidInput`), alone
or among others, before anything is spent. It holds two kinds of row. (1) Items of a known app library (`LIBRARIES`, a
table of id, app name, bundle ID, process name, home-relative folder, bundle extension, kind): UTM's
`~/Library/Containers/com.utmapp.UTM/Data/Documents/*.utm` and Cap's
`~/Library/Application Support/so.cap.desktop/recordings/*.cap`, one row per bundle, any size. The name is the VM's
`Information › Name` from the XML `config.plist` (the bundle name when it is binary or lacks it) plus
`· UTM virtual machine`, or `Cap recording · 30 Oct, 5:10 PM` from the date in the bundle name (else `pretty_name` in
`recording-meta.json`, else the bundle name). It frees its private bytes (FG-L1); `last_changed` is the newest file date
inside; the preview is UTM's `screenshot.png` or Cap's `screenshots/display.jpg` (else another image there); the owning
app is the `.app` of that name in the Applications folders, and `app_running` is the app probe (`pgrep -x`) at scan time
(an unknown answer is false; task 11's move asks again). (2) FN1: every large file whose private bytes are 0. The
installer scan hands over installer-type files of 100 MB or more in Downloads and Desktop whatever their name, the home
walk any file of 500 MB or more, both before the age and name tests; a path found twice counts once. The reason comes
from `sharing_of`: if the file is an APFS clone and a file with the same clone ID sits inside a known library, it reads
`Frees nothing: UTM's Windows VM holds the same data` (and `app_name` is that app; reasons carry no formatted numbers, the app writes the size from `shared_bytes` in its own locale); if the clone's partner is
elsewhere, `Frees nothing: another copy on this Mac uses the same space`; if it is not a full clone,
`Frees nothing: its space is shared with other files`. `shared_bytes` is its allocated size and `frees_bytes` is 0. Rows
sort by what they free, or share. Tests use fixture libraries, `clonefile` pairs and an injected probe
(`ElsewhereRoots`, `AppProbe`).

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
internal rules, so cleanup never moves them or an ancestor that holds them. A tool item that
names no file (Homebrew's cleanup) is held at run time when a rule touches a path its tool
deletes or deletes inside ([`tool_touches`](../../crates/tiny-core/src/clean/providers/homebrew.rs):
the download cache and logs folders, and every old version the dry run lists): the run skips as
`Kept` and `brew` never starts. When the dry run fails, those paths cannot be told and the run
skips as `KeepUncertain`. The preview still lists it.

Swift never writes rules and never sends a path: it names a discovered candidate ID to
keep, or a rule ID to remove, with the revision it last read
([`clean_keep` and `clean_unkeep`](../../crates/tiny-ffi/src/clean/protection.rs)). The
[CLI executor](../../crates/tiny-core/src/clean/execute.rs) takes the same exclusive lease before
acting, reports every item the rules do not allow as kept, and refuses the whole run when they
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
