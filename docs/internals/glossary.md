# Glossary

> For maintainers. Using the CLI? See [user guides](../user/).

Canonical engine, CLI and FFI vocabulary. Native presentation vocabulary belongs to its client.

## Product

- **TinySift** — The product and desktop app name in UI copy, docs and the landing page; the
  local build is `TinySift Dev.app`. Not: tiny, Tiny, Tinysift, Tiny Sift.
- **`tiny`** — The command-line tool, always in code formatting. Crate names, the bundle ID
  `com.mxrsv.tiny.dev`, `TINY_*` variables and the saved-data folders `~/.tiny-cli` and
  `~/Library/Application Support/Tiny` keep the old name; renaming them would orphan saved
  data. Not: TinySift as a command.

## Clean

- **Category** — One kind of removable data with a stable kebab-case id (`node-modules`,
  `trash`) accepted by `--category`. The user-facing unit of choice. Not: provider, type.
- **Provider** — The `CleanProvider` behind one category: id, label, risk, discovery, execution
  and desktop eligibility. One file may define several (`xcode.rs` has three). Not: category,
  plugin.
- **Family** — The CLI picker's top level: "Dev caches", "User storage", "System leftovers"
  (`Family::label`), assigned only by `category_family()`. Reaches Swift as
  `FfiCleanCategory.family`, unused by the Clean screen so far. Not: group, trust section.
- **Risk level** — `Safe`, `Review` or `Destructive`. Decides which categories run without
  `--include-*`; only `Safe` is preselected, in the CLI picker and in the app. Not: severity.
- **Item** (`CleanItem`) — One path a provider proposes to remove, with size, risk and evidence.
  Crosses to Swift as a candidate (`FfiCleanCandidate`) with an opaque ID. Its size is the bytes
  cleanup would free (see **Freed size**), not `du`. Not: entry, file (an item is often a
  directory).
- **Freed size** — What deleting an item returns to the volume: allocated blocks, a hard link or
  APFS clone family counted once and only when wholly inside the item, with `size_is_upper_bound`
  set when the volume cannot say
  ([`freed_size.rs`](../../crates/tiny-core/src/clean/freed_size.rs)). Not: `du`, Storage's
  Space Lens total, or the logical file length.
- **Evidence** (`Evidence`) — A checkable fact shown beside an item: modified or last-opened
  time, manifest and last commit, owning app, `Sensitive`. Empty means the category rule alone
  selected it. Not: reason (the category's `inclusion_reason`).
- **Group** (`CategoryGroup`) — All items one provider discovered, with their total size, as the
  CLI picker lists them. Not: family, trust section.
- **Plan** — The CLI's selected groups, printed path by path before the action menu. Nothing is
  removed before it is shown. Not: preview.
- **Clean action** (`CleanAction`) — The CLI menu choice: Trash, HardDelete, DryRun, Cancel.
  Not: exec action.
- **Exec action** (`ExecAction`) — What a provider's `execute` is asked to do: Trash, HardDelete
  or EmptyTrash, derived per provider by `map_action`. Not: clean action.
- **Excluded paths** — Paths unticked in the CLI's review-paths drill-down, filtered out just
  before execution.
- **Idle** — Unused for longer than `--idle-days`. For `node-modules`, `rust-targets` and
  `python-caches`: the manifest is older and, inside a git work tree, so is the last commit
  touching the project, with no uncommitted changes
  ([`project_activity.rs`](../../crates/tiny-core/src/clean/providers/project_activity.rs)).
  Elsewhere: the item's own modification time (Downloads also checks last opened). An
  unreadable time, or git that cannot answer, counts as not idle. The app lists build output for
  every project and uses a fixed 7 days (`BUILD_OUTPUT_SAFE_DAYS`): Safe only inside git with every
  fact checked, Review otherwise, including outside git (`NotGitRepo`).
- **Section** (`Section`, `FfiCleanSection`) — Where the app groups a category's movable items by
  what to expect afterwards: **Rebuilt automatically**, **Project build output** (`node-modules`,
  `rust-targets`, which a build or install recreates but which hold a project's work) or **Your
  files**. Derived by `category_section` from `comes_back`; report-only categories are grouped by
  their desktop action instead. Not: family, risk.
- **Dev search roots** — `~/Documents`, `~/Projects`, `~/Code`, `~/Developer`, `~/Workspace`:
  the folders the project-cache providers walk.
- **App gating** — Not acting while the owning app runs: per category (`quit_apps()`), per path
  in `discover()`, and per path again before each desktop move (`item_apps()`). See
  [clean.md](clean.md#selection-and-discovery).

## Desktop clean

- **Always keep** — A saved desktop rule: an exact path and a scope, `File` (that path) or
  `Directory` (also everything inside it). It also keeps a containing folder from moving. Not:
  a protected path (`protected_reason`: the root, the home folder and its ancestors, which no
  cleanup moves), and not used by the CLI
  ([rules](../../crates/tiny-core/src/clean/protection.rs)).
- **Kept path** — The path a rule names; the UI says "kept" (`Kept`, `HoldsKept`). Missing
  paths stay kept and apply again if they return; renames are not followed. Not: excluded
  paths (the CLI's per-run review choice).
- **Held** — A candidate a rule keeps in place: it is a kept path, lies inside a kept folder or
  holds one, or could not be compared with a rule. A held row cannot be selected, and Rust
  leaves it out of the preview and skips it at execution
  ([`FfiProtectionGuard`](../../crates/tiny-ffi/src/clean/protection.rs)). Not: Review risk,
  which selecting more can override.
- **Protection store** — The folder `~/Library/Application Support/Tiny/clean-protection/` with
  `rules.json`, plus the lock beside it. Unreadable or incomplete means cleanup is blocked, not
  that no rules exist ([contract](clean.md#the-store)).
- **Comes back** (`ComesBack`) — How a category's items return after cleanup: `Rebuild` (with the
  command), `Redownload`, `AppRecreates`, `TrashOnly` or `NotRecoverable`, per id in
  `comes_back()`. It also decides the **Section**.
- **Report only** (`ReportOnly`) — A category the app shows but does not act on per path, with one
  of five reasons: `Destructive` (permanent or Empty Trash), `NotPerPathTrash` (cleanup is a tool
  command), `UnreliableMatch`, `NeedsAdministrator` (no unprivileged route) or
  `XcodeNotInstalled` (only System Settings removes the simulator runtimes). It names why a
  path-by-path move is not offered; the **Trash row**, tool commands and **deletion as
  administrator** are separate routes that a provider flag opens. `NeedsAdministrator` is no longer
  sent over FFI, where such categories are `DeleteAsAdministrator`. Not: skipped, disabled.
- **`needs_admin`** — The provider flag for a category whose items root owns (the system simulator
  caches). The desktop deletes them only as administrator; the CLI leaves them out of its default
  selection and refuses them.
- **Preview** — The app's reviewed selection, built in Rust from candidate IDs; single-use,
  expires after 15 minutes (`PREVIEW_TTL` in [`clean.rs`](../../crates/tiny-ffi/src/clean.rs)).
  It freezes each item's **Removal** and the **Delete mode**. Not: plan. The uninstall flow has
  its own, built from an app ID (see **Uninstall preview**).
- **Delete mode** (`DeleteMode`, `FfiDeleteMode`) — The client's choice for Rebuilt automatically
  and Project build output: `Trash` (move them) or `Permanent` (delete them, or prune a tool's
  cache). Your files always go to the Trash. Fixed when the preview is built.
- **Removal** (`Removal`) — How the app removes one planned path: `MoveToTrash`, `Delete`,
  `EmptyTrash`, `RunTool` (a prune or a request) or `DeleteAsAdministrator`, chosen by
  `removal_for` ([routing](clean.md#the-desktops-checked-removal)). Not: clean action, exec action.
- **Trash row** — The `trash` category as the app offers it (`include_trash`): one `Destructive`
  item for `~/.Trash`, never preselected, emptied through Finder when explicitly selected.
- **Freed, moved, requested** — The three results the app reports apart: freed (`bytes_freed`:
  deleted, emptied or pruned), moved to the Trash (`bytes_moved_to_trash`: still on disk) and
  requested (`bytes_requested`: a tool finishes it in the background). Not: one total.
- **Deletion as administrator** — Root deleting `needs_admin` items through one `osascript`
  authentication dialog per Clean, with nothing installed
  ([contract](clean.md#deletion-as-administrator)). `FfiPreview.bytes_needing_admin` is the part of
  `bytes_to_free` that needs it, so a client knows the dialog will appear.
- **Authentication cancelled** (`FfiRemovalReport.authentication_cancelled`) — The user dismissed
  the administrator dialog: nothing changed and every item is `NotAttempted`.
- **Checked execute** — The app's removal, revalidating each path right before it changes
  ([`checked_execute.rs`](../../crates/tiny-core/src/clean/checked_execute.rs)). Not: execute
  (the CLI's provider path).

## Processes

- **Identity** — PID plus start time, also name and executable when known; a mismatch returns
  `IdentityChanged` ([`terminate.rs`](../../crates/tiny-core/src/processes/terminate.rs)). Not:
  PID alone.
- **Refusal** (`Refusal`) — Why TinySift will not signal a process: itself or its parent, PID 0 or
  1, another user, a protected name, or a port owner that started after `lsof` ran. Also gates
  the app's Swift-side Quit.
- **Busy** (`FfiError::Busy`) — The [session](../../crates/tiny-ffi/src/session.rs) gate is
  held: one scan or mutation at a time, rejected rather than queued. Not: locked.

## Uninstall

- **Leftover** — A `~/Library` path keyed by the app's bundle identifier. Not: cache (a leftover
  may be preferences or containers).
- **Blocked** — A plan that must not run: a system app, or a Homebrew cask without `--force`.
  One blocked plan stops the whole invocation.
- **Uninstall preview** — The app's reviewed uninstall of one installed app: core's plan frozen by
  `build_checked_plan`, with opaque item IDs; single-use, expires after 15 minutes, replaced by the
  next preview or listing ([`uninstall.rs`](../../crates/tiny-ffi/src/uninstall.rs)). Not: plan.
