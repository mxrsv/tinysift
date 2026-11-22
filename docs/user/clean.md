# Cleaning caches

`sift clean` finds caches and leftovers you can remove, shows how much space each category
takes, lets you pick, and asks what to do before touching anything. It needs macOS. The
[desktop app](https://tinysift.app/docs) lists the same categories with its own review
and consent steps.

```bash
sift clean                              # picker → plan → action menu
sift clean --dry-run                    # show the plan and exit
sift clean --include-review             # also list categories worth a look first
sift clean --include-destructive        # also list the permanent ones
sift clean --review-paths               # untick individual paths before acting
sift clean --category node-modules      # only this category (repeatable)
sift clean --category trash --hard      # Hard delete preselected: empties the Trash
```

Run `--dry-run` first on a new machine.

## Categories

47 categories in three families. Each has a risk level that decides whether you see it by
default.

**Dev caches** — `cargo`, `npm`, `pnpm`, `yarn`, `uv`, `bun-cache`, `pip-cache`, `puppeteer`,
`huggingface`, `codex-runtimes`, `codex-releases`, `node-modules`, `python-caches`,
`rust-targets`, `build-folders`, `gradle-maven`, `jetbrains`, `vscode`, `ios-simulators`,
`simulator-devices`, `system-simulator-caches`, `simulator-runtimes`,
`simulator-runtime-images`, `android-sdk`, `go-cache`, `homebrew`, `docker`, `docker-volumes`,
`xcode-derived`, `xcode-archives`, `xcode-devicesupport`, `xcode-previews`.

**User storage** — `downloads-old`, `screenshots-old`, `mail-attachments`,
`streaming-caches`, `chat-caches`, `browser-caches`, `browser-offline-data`.

**System leftovers** — `user-logs`, `user-caches`, `trash`, `quarantine`, `crash-reports`,
`app-orphans`, `time-machine-local`, `font-quicklook-caches`.

| Risk        | Shown                        | Categories                                                                                      |
| ----------- | ---------------------------- | ----------------------------------------------------------------------------------------------- |
| Safe        | always, already ticked       | `user-logs`, `crash-reports`, `font-quicklook-caches`, `xcode-derived`, `xcode-previews`, `puppeteer` |
| Review      | with `--include-review`      | everything else                                                                                 |
| Destructive | with `--include-destructive` | `trash`, `time-machine-local`, `docker-volumes`                                                 |

Naming a category with `--category` shows it whatever its risk; the `--include-*` flags are
then ignored. `system-simulator-caches` is Safe but needs administrator rights, which `sift`
never asks for: plain `sift clean` leaves it out, and naming it lists it and then refuses it.
The [desktop app](https://tinysift.app/docs) only shows its size: macOS refuses
to let an app delete it, even after an administrator's password.

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

At the end `sift` prints how many paths it removed and lists any it could not.

Unticking a path keeps it, its contents, and any selected parent folder that would carry it
along. Those overlapping selections are retained and explained in the final report by the
[CLI executor](../../crates/tiny-core/src/clean/execute.rs).

## What is found

- `--idle-days N` (default 30, must be above 0) — `node-modules`, `rust-targets`,
  `build-folders` and the
  virtualenvs in `python-caches` are listed only for projects with no manifest edit, commit
  or uncommitted change in `N` days. Inside git the commit and the changes are the whole
  repository's: `ios/Pods` serves the app beside it, however long `ios/` went untouched. `downloads-old` lists files directly in `~/Downloads`
  neither modified nor opened for `N` days, `screenshots-old` lists screenshots not modified
  for `N` days, and `android-sdk` uses it for emulator system images.
- Project caches are searched under `~/Documents`, `~/Projects`, `~/Code`, `~/Developer` and
  `~/Workspace`. Nothing inside a `node_modules` folder is listed on its own: `python-caches`
  and `rust-targets` do not look inside one, and `node-modules` lists it whole
  ([`is_node_modules`](../../crates/tiny-core/src/clean/providers/mod.rs)).
- `build-folders` lists only folders a manifest beside them proves are generated: `.next` beside
  `package.json`, `Pods` beside a `Podfile`, `build` beside `build.gradle` or
  `build.gradle.kts`, and `build` or `dist` when `package.json` names it in `main`, `module`,
  `types`, `typings`, `browser`, `bin`, `exports` or `files`, or, beside a `package.json`,
  `tsconfig.json`'s `outDir` is `dist` ([`build_folders.rs`](../../crates/tiny-core/src/clean/providers/build_folders.rs)).
  A `build` or `dist` folder without such proof is never listed: many projects keep sources
  there.
- A `node_modules`, `target/` or build folder that holds a file git tracks is never listed or
  removed, and neither is one whose tracked files git cannot list (for example without the
  Command Line Tools).
- Some categories are skipped while their app is open, and `sift` tells you which app to quit:
  the Xcode categories (Xcode; `xcode-derived` also `xcodebuild`), `ios-simulators`, `simulator-devices` and
  `simulator-runtimes` (Xcode, Simulator or `xcodebuild`), `cargo` (cargo or rustc), `gradle-maven` (a Gradle daemon,
  client or wrapper, checked right before the move), `mail-attachments` (Mail), `vscode` (VS Code), `android-sdk`
  (Android Studio), `docker-volumes` (Docker Desktop), `bun-cache` (bun), `uv` (uv or uvx), `pnpm` (pnpm, including
  when Node runs it), `codex-runtimes` (ChatGPT), `codex-releases` (Codex) and
  `system-simulator-caches` (Xcode, Simulator, `xcodebuild`, `xctest`, `XCTRunner`,
  `simctl`, a booted simulator's `launchd_sim`, or any booted simulator).
- `user-caches` never offers a folder that keeps a tool's state beside its caches: `pypoetry` while it holds
  Poetry's `virtualenvs`, or `deno` while it holds Deno's `location_data` (localStorage and Deno KV)
  ([`user_caches.rs`](../../crates/tiny-core/src/clean/providers/user_caches.rs)). `gradle-maven` lists only
  Gradle's caches: Maven's `~/.m2/repository` also holds what `mvn install` put there, so it is not offered.
- `npm` offers only npm's package cache (`_cacache`), never `_npx`, where the packages `npx` runs live.
- `user-logs` lists every entry of `~/Library/Logs` except the `DiagnosticReports` folder: `crash-reports`
  lists the reports inside it, and the folder itself stays for macOS to save the next one
  ([`user_logs.rs`](../../crates/tiny-core/src/clean/providers/user_logs.rs)).
- `puppeteer` leaves out just the builds of a browser that is running (Chrome for Testing or
  `chrome-headless-shell`), without a message.
- `browser-caches`, `browser-offline-data`, `chat-caches`, `streaming-caches` and `user-caches` instead leave out
  just the folders of apps that are running, without a message. Quit the app and run again
  to see them.
- `browser-caches` lists every profile of Safari, Chrome, Arc, Brave, Edge, Vivaldi, Opera and Firefox: a
  Chromium profile's `Cache`, `Code Cache` and `Storage` under `~/Library/Caches`, and its `Cache`, `Code Cache`
  and `GPUCache`; Safari's `com.apple.Safari` cache; Firefox's `cache2`. Never cookies, logins, preferences,
  history, extensions, site storage or a service worker's scripts and registrations (`Service Worker/ScriptCache`,
  `Service Worker/Database`), which no category offers. A browser counts only when its profile list (`Local State`,
  Firefox's `profiles.ini`) can be read; otherwise `browser-caches` offers none of its folders. `user-caches`
  offers the rest of `~/Library/Caches` around what `browser-caches` offers: a vendor folder (`Google`,
  `BraveSoftware`, ...) whole when no folder in it is offered, else its other parts, such as a deleted profile's
  folder, so nothing is counted twice
  ([`browser_caches.rs`](../../crates/tiny-core/src/clean/providers/browser_caches.rs)).
- `browser-offline-data` (Offline site data) lists each Chromium profile's `Service Worker/CacheStorage`: copies
  sites keep for offline use. Sites download them again, but a page or file saved for offline reading goes with
  them, so it only moves to the Trash.

## Browser privacy in the desktop app

The desktop app's Browsers page also cleans each Chrome, Arc, Brave, Edge, Vivaldi, Opera and Firefox profile's
Privacy: cookies unused for 90 days whose site is not in the profile's history (trackers) or was not visited in
90 days, and history entries older than 90 days, with their icons, top sites and shortcuts. Cookies of a site you
visited in the last 90 days stay, so you stay signed in there. It runs only while the browser is quit;
TinySift never quits it for you. The change is on this Mac only: a browser's sync account keeps its own copy.
Safari history is never edited. Before anything changes, a backup of the browser's files goes to the Trash;
Undo puts them back until the profile is scanned again, as long as the browser has not changed them since, and
the backup stays in the Trash after that. The
[internals](../internals/clean.md#browsers) list what Privacy leaves in place. `sift clean` has no Privacy step.

## Scanning in the desktop app

The desktop app can scan some categories without the rest. A scan replaces the results of the
categories it covers and leaves every other category's results, and a plan already built from
them, as they were. A scan that is stopped or fails changes nothing. `sift clean` always scans
afresh. How the app presents this is in the [desktop guide](https://tinysift.app/docs).

## Your own files

`downloads-old`, `screenshots-old`, `mail-attachments` and `browser-offline-data` hold files nothing recreates; once
removed, only the Trash brings them back. Selecting one of these categories selects every
file it lists, including ones that look private such as keys or recovery codes. Read the
plan, and untick what you want to keep with `--review-paths`. The desktop app flags
private-looking downloads and never ticks them for you.

## Safety

- **Move to Trash is the default.** It goes through Finder, so Put Back works.
- **Hard delete is permanent** and asks you to confirm. `--hard` only preselects it in the
  menu. With `-y` there is no prompt, so `sift` also requires `TINY_CONFIRM_HARD=1`:
  `TINY_CONFIRM_HARD=1 sift clean --category cargo --hard -y`.
- **`-y` needs `--category`**, so an unattended run always names its scope.
- **The Trash can only be emptied**, with Hard delete. With `trash` selected, Move to Trash
  is refused and nothing is changed.
- **Some cleanup has no Trash.** `homebrew` runs `brew cleanup --prune=30` (old versions and
  downloads older than 30 days; never a formula, since `sift` turns off Homebrew's autoremove),
  and its size is what that command's dry run lists, measured on disk, leaving out
  `~/Library/Caches`, which `user-caches` already counts;
  `docker` prunes unused images and build cache, and its size is
  what Docker calls reclaimable, not the images' whole size (Docker's daemon must be running,
  so it is not skipped while Docker Desktop is open);
  `docker-volumes` prunes unused volumes, which hold container data such as databases. Both
  read and prune only an engine on this Mac: when `DOCKER_HOST` or the current Docker context
  points elsewhere (`ssh://`, `tcp://`), they report an error and prune nothing
  ([`docker.rs`](../../crates/tiny-core/src/clean/providers/docker.rs));
  `time-machine-local` deletes local snapshots; `simulator-devices` deletes, one by one with
  `xcrun simctl delete`, the simulators that Xcode reports unavailable; `simulator-runtimes` deletes runtimes no simulator uses (`simctl`
  finishes the removal in the background); `uv` runs `uv cache prune` and `pnpm` runs
  `pnpm store prune`, which remove only entries nothing references; `bun-cache` runs
  `bun pm cache rm` and `pip-cache` runs `pip3 cache purge`, which empty the whole cache
  (`pip-cache` only through a Homebrew or Command Line Tools `pip3`, and only when
  `~/Library/Caches/pip` exists; otherwise that folder is part of `user-caches`). These are permanent
  whichever action you choose, and `sift` warns you when you pick Move to Trash. These are
  Review for that reason, so plain `sift clean` leaves them unticked. `pnpm` used to move its
  store to the Trash, so a script with `--category pnpm` now prunes instead.
- **`simulator-runtime-images` is report only.** Without Xcode, `sift` lists the simulator
  runtimes it finds but refuses to remove them; use System Settings → General → Storage.
- **`app-orphans` is report only.** It lists Application Support folders that match no
  installed app, but a folder name does not prove the app is gone, so `sift` never moves
  them. Check them in Finder.
- **Symlinks are never followed**, so nothing outside the listed paths can be removed.
- **Always keep rules from the [desktop app](https://tinysift.app/docs)
  apply here too.** A kept path, or a folder that holds one, is reported as kept and left in
  place, for Move to Trash and Hard delete alike. If the saved rules cannot be read, nothing
  is cleaned.
- **App checks run again before cleanup.** For file-based categories, the
  [executor](../../crates/tiny-core/src/clean/execute.rs) checks each item's owning apps and
  provider guard immediately before acting. A running app or a failed check keeps the item;
  tool-backed categories are checked before their batch command.

> Move to Trash is still deletion if macOS empties the Trash automatically
> (System Settings → General → Storage). Turn that off or use `--dry-run` first.

A failed category scan, a failed running-app check or an unreadable candidate size is reported
on stderr and stops the CLI before cleanup; a missing optional tool is only reported as
unavailable. Naming an unavailable category explicitly, or one whose app is running, also
fails. A cleanup with failed or refused items exits nonzero after printing the per-item report. See [discovery](../../crates/tiny-core/src/clean/discover.rs)
and [CLI results](../../crates/tiny/src/render/clean/mod.rs).

The [executor](../../crates/tiny-core/src/clean/execute.rs) holds the kept-path store's lease
throughout the run and checks it again before each mutation. A changed or unreadable policy
stops further mutations. Tool resource IDs are accepted only by their owning provider;
arbitrary relative filesystem paths are refused.
