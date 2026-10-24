# Cleaning caches

`tiny clean` finds caches and leftovers you can remove, shows how much space each category
takes, lets you pick, and asks what to do before touching anything. It needs macOS. The
[desktop app](../../../tiny-cli/docs/user/desktop.md) lists the same categories with its own review
and consent steps.

```bash
tiny clean                              # picker → plan → action menu
tiny clean --dry-run                    # show the plan and exit
tiny clean --include-review             # also list categories worth a look first
tiny clean --include-destructive        # also list the permanent ones
tiny clean --review-paths               # untick individual paths before acting
tiny clean --category node-modules      # only this category (repeatable)
tiny clean --category trash --hard      # Hard delete preselected: empties the Trash
```

Run `--dry-run` first on a new machine.

## Categories

41 categories in three families. Each has a risk level that decides whether you see it by
default.

**Dev caches** — `cargo`, `npm`, `pnpm`, `yarn`, `uv`, `puppeteer`, `huggingface`,
`codex-runtimes`, `codex-releases`, `node-modules`, `python-caches`, `rust-targets`,
`gradle-maven`, `jetbrains`, `vscode`, `ios-simulators`, `simulator-devices`,
`system-simulator-caches`, `simulator-runtimes`, `simulator-runtime-images`, `android-sdk`,
`go-cache`, `docker`, `docker-volumes`, `xcode-derived`, `xcode-archives`,
`xcode-devicesupport`.

**User storage** — `downloads-old`, `screenshots-old`, `mail-attachments`,
`streaming-caches`, `chat-caches`, `browser-caches`.

**System leftovers** — `user-logs`, `user-caches`, `trash`, `quarantine`, `crash-reports`,
`app-orphans`, `time-machine-local`, `font-quicklook-caches`.

| Risk        | Shown                        | Categories                                                                                      |
| ----------- | ---------------------------- | ----------------------------------------------------------------------------------------------- |
| Safe        | always, already ticked       | `user-logs`, `crash-reports`, `font-quicklook-caches`, `xcode-derived`, `puppeteer`             |
| Review      | with `--include-review`      | everything else                                                                                 |
| Destructive | with `--include-destructive` | `trash`, `time-machine-local`, `docker-volumes`, `simulator-devices`                            |

Naming a category with `--category` shows it whatever its risk; the `--include-*` flags are
then ignored. `system-simulator-caches` is Safe but needs administrator rights, which `tiny`
never asks for: plain `tiny clean` leaves it out, and naming it lists it and then refuses it.
The [desktop app](../../../tiny-cli/docs/user/desktop.md) can delete it after one macOS
authentication.

## Choosing what to clean

1. **Pick categories.** Grouped by family, with Safe ones already ticked; Space toggles,
   Enter confirms.
2. **Read the plan.** The paths that would be removed, with sizes.
3. **Choose an action:**
   - **Move to Trash (recoverable)** — the default.
   - **Dry-run (no changes)** — stop here.
   - **Review paths (drill-down)** — untick individual paths, then come back to this menu.
   - **Hard delete (NOT recoverable)** — asks you to confirm once more.
   - **Cancel**.

At the end `tiny` prints how many paths it removed and lists any it could not.

Unticking a path keeps it, its contents, and any selected parent folder that would carry it
along. Those overlapping selections are retained and explained in the final report by the
[CLI executor](../../crates/tiny-core/src/clean/execute.rs).

## What is found

- `--idle-days N` (default 30, must be above 0) — `node-modules`, `rust-targets` and the
  virtualenvs in `python-caches` are listed only for projects with no manifest edit, commit
  or uncommitted change in `N` days. `downloads-old` lists files directly in `~/Downloads`
  neither modified nor opened for `N` days, `screenshots-old` lists screenshots not modified
  for `N` days, and `android-sdk` uses it for emulator system images.
- Project caches are searched under `~/Documents`, `~/Projects`, `~/Code`, `~/Developer` and
  `~/Workspace`. Nothing inside a `node_modules` folder is listed on its own: `python-caches`
  and `rust-targets` do not look inside one, and `node-modules` lists it whole
  ([`is_node_modules`](../../crates/tiny-core/src/clean/providers/mod.rs)).
- A `node_modules` or `target/` folder that holds a file git tracks is never listed or removed,
  and neither is one whose tracked files git cannot list (for example without the Command Line
  Tools).
- Some categories are skipped while their app is open, and `tiny` tells you which app to quit:
  the Xcode categories (Xcode), `ios-simulators`, `simulator-devices` and `simulator-runtimes`
  (Xcode or Simulator), `mail-attachments` (Mail), `vscode` (VS Code), `android-sdk` (Android Studio),
  `docker` and `docker-volumes` (Docker Desktop), `uv` (uv or uvx), `pnpm` (pnpm, including
  when Node runs it), `codex-runtimes` (ChatGPT), `codex-releases` (Codex) and
  `system-simulator-caches` (Xcode, Simulator, `xcodebuild`, `xctest`, `XCTRunner`,
  `simctl`, a booted simulator's `launchd_sim`, or any booted simulator).
- `puppeteer` leaves out just the builds of a browser that is running (Chrome for Testing or
  `chrome-headless-shell`), without a message.
- `browser-caches`, `chat-caches`, `streaming-caches` and `user-caches` instead leave out
  just the folders of apps that are running, without a message. Quit the app and run again
  to see them.

## Your own files

`downloads-old`, `screenshots-old` and `mail-attachments` hold files nothing recreates; once
removed, only the Trash brings them back. Selecting one of these categories selects every
file it lists, including ones that look private such as keys or recovery codes. Read the
plan, and untick what you want to keep with `--review-paths`. The desktop app flags
private-looking downloads and never ticks them for you.

## Safety

- **Move to Trash is the default.** It goes through Finder, so Put Back works.
- **Hard delete is permanent** and asks you to confirm. `--hard` only preselects it in the
  menu. With `-y` there is no prompt, so `tiny` also requires `TINY_CONFIRM_HARD=1`:
  `TINY_CONFIRM_HARD=1 tiny clean --category cargo --hard -y`.
- **`-y` needs `--category`**, so an unattended run always names its scope.
- **The Trash can only be emptied**, with Hard delete. With `trash` selected, Move to Trash
  is refused and nothing is changed.
- **Some cleanup has no Trash.** `docker` prunes unused images and build cache;
  `docker-volumes` prunes unused volumes, which hold container data such as databases;
  `time-machine-local` deletes local snapshots; `simulator-devices` deletes simulators that
  Xcode reports unavailable; `simulator-runtimes` deletes runtimes no simulator uses (`simctl`
  finishes the removal in the background); `uv` runs `uv cache prune` and `pnpm` runs
  `pnpm store prune`, which remove only entries nothing references. These are permanent
  whichever action you choose, and `tiny` warns you when you pick Move to Trash. `uv` and `pnpm`
  are Review for that reason, so plain `tiny clean` leaves them unticked. `pnpm` used to move its
  store to the Trash, so a script with `--category pnpm` now prunes instead.
- **`simulator-runtime-images` is report only.** Without Xcode, `tiny` lists the simulator
  runtimes it finds but refuses to remove them; use System Settings → General → Storage.
- **`app-orphans` is report only.** It lists Application Support folders that match no
  installed app, but a folder name does not prove the app is gone, so `tiny` never moves
  them. Check them in Finder.
- **Symlinks are never followed**, so nothing outside the listed paths can be removed.
- **Always keep rules from the [desktop app](../../../tiny-cli/docs/user/desktop.md#always-keep)
  apply here too.** A kept path, or a folder that holds one, is reported as kept and left in
  place, for Move to Trash and Hard delete alike. If the saved rules cannot be read, nothing
  is cleaned.
- **App checks run again before cleanup.** For file-based categories, the
  [executor](../../crates/tiny-core/src/clean/execute.rs) checks each item's owning apps and
  provider guard immediately before acting. A running app or a failed check keeps the item;
  tool-backed categories are checked before their batch command.

> Move to Trash is still deletion if macOS empties the Trash automatically
> (System Settings → General → Storage). Turn that off or use `--dry-run` first.
