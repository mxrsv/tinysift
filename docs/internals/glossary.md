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
  Crosses to Swift as a candidate (`FfiCleanCandidate`) with an opaque ID. Not: entry, file (an
  item is often a directory).
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
  unreadable time, or git that cannot answer, counts as not idle.
- **Dev search roots** — `~/Documents`, `~/Projects`, `~/Code`, `~/Developer`, `~/Workspace`:
  the folders the project-cache providers walk.
- **App gating** — Not acting while the owning app runs: per category (`quit_apps()`), per path
  in `discover()`, and per path again before each desktop move (`item_apps()`). See
  [clean.md](clean.md#selection-and-discovery).

## Desktop clean

- **Comes back** (`ComesBack`) — How a category's items return after Trash: `Rebuild` (with the
  command), `Redownload`, `AppRecreates`, `TrashOnly` or `NotRecoverable`, per id in
  `comes_back()`.
- **Report only** (`ReportOnly`) — A category the app shows but never moves: `Destructive`,
  `NotPerPathTrash` (cleanup is a tool command) or `UnreliableMatch`. Not: skipped, disabled.
- **Preview** — The app's reviewed selection, built in Rust from candidate IDs; single-use,
  expires after 15 minutes (`PREVIEW_TTL` in [`clean.rs`](../../crates/tiny-ffi/src/clean.rs)).
  Not: plan.
- **Checked execute** — The app's Move to Trash, revalidating each path right before it moves
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
