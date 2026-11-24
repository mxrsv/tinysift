# The full-screen view

`sift` on its own opens a full-screen view with five tabs, so you can clean, uninstall, scan and
quit processes without typing another command. It uses the same engine and the same rules as
the commands; each section below links to the guide that holds them.

```bash
sift                  # open it
sift --icons nerd     # open it with Nerd Font icons, and keep them for later runs
```

## When it opens

It opens only when `sift` is typed with no command, or with `--icons` alone, and both its input
and its output are a terminal ([`entry.rs`](../../crates/tiny/src/tui/entry.rs)). Everything else
is unchanged: every command and flag works as before, and bare `sift` in a pipe or a script
still prints the help and exits with status 2.

The terminal must be at least 60×18, so a split pane works. A smaller one gets a one-line
message on stderr and exit status 2; use `sift <command>` there. If the window shrinks below that
while the view is open, it shows the minimum size until the window is large enough again; `q`
still quits. From 100 columns the highlighted row's detail is a pane on the right; below that it
is three lines under the list. Under 80 columns the bars get shorter before the names do, the
free-disk figure leaves the tab bar, and a key bar that does not fit ends in `? keys · q quit`:
`?` lists every key of the page.

`NO_COLOR` set to any non-empty value drops every colour
([`theme.rs`](../../crates/tiny/src/tui/theme.rs)).

## Tabs

- **Clean** — the cache categories `sift clean` lists, measured as the tab opens.
- **Uninstall** — the apps in `/Applications`, removed with the data they left in `~/Library`.
- **Scan** — large and old files in Downloads, Desktop and Documents. It only reads.
- **Processes** — every process, with Quit and Force Quit.
- **System** — memory, each disk and this Mac.

The view opens on Clean. The free space of the startup disk is at the right of the tab bar; it
is read again after a clean or an uninstall.

## Keys

The same everywhere ([`app.rs`](../../crates/tiny/src/tui/app.rs)):

| Key | Does |
| --- | --- |
| `tab`, `shift-tab` | next or previous tab |
| `1` to `5` | go to a tab |
| `↑` `↓`, `page up` `page down`, `home` `end` | move in a list |
| `esc` | step back: close a box, leave a list, stop a read or a run |
| `r` | read again |
| `?` | the keys of this screen |
| `q`, `ctrl-c` | quit |

The bar at the bottom shows the keys the screen takes. While a box is open, every key except
`ctrl-c` is the box's: `q` does not quit from inside it, and `tab` does nothing (only `←` `→`
move between its buttons).

While a clean or an uninstall runs, the tab keys and digits do nothing: one run at a time.

Quitting while a clean or an uninstall runs waits for the category or app in progress to
finish, as `esc` does, then closes the view; the bottom line says so. **A second `ctrl-c`**
(or SIGTERM) while it waits closes the view at once, with exit status 130 and one line on stderr
saying the category or app in progress may be partly done. Closing the terminal window never
cuts a run short: the run stops after the category or app in progress, then `sift` exits
([`event_loop.rs`](../../crates/tiny/src/tui/event_loop.rs)).

## Clean

The rules for what is listed and what each action removes are in [Cleaning caches](clean.md);
the view adds none ([`clean/engine.rs`](../../crates/tiny/src/tui/pages/clean/engine.rs)).

- **It opens with the Safe categories, ticked**: exactly what plain `sift clean` lists, with its
  defaults (for example `--idle-days 30`). `v` measures again and also lists the Review ones,
  unticked; `v` again returns to Safe only.
- **Measuring again keeps your choices** (`r`, `v`): a category keeps its tick and an unticked
  path stays unticked ([`listing.rs`](../../crates/tiny/src/tui/pages/clean/listing.rs)). Only a
  category listed for the first time starts as `sift clean` starts it.
- **The Destructive categories are not here.** Emptying the Trash, Time Machine local snapshots
  and Docker volumes still need `sift clean --include-destructive`.
- Ticking opens once every category is measured; `esc` stops measuring and lists nothing until
  `r`. A category whose app is open is listed without a size, marked `in use`, and cannot be
  ticked; quit the app and press `r`. Report-only categories cannot be ticked either.
- **If a category cannot be read, nothing can be cleaned** until `r` reads it again. When only
  a Review category failed, `v` back to Safe only also clears it.
- `space` ticks a category, `p` lists its paths. Unticking a path keeps it, any ticked path
  above it and any ticked path inside it, in every category, as `sift clean --review-paths`
  does.
- **The figure at the bottom is what `enter` cleans**: the scan size of every path of the ticked
  categories except the ones the engine keeps for your unticked paths. It is split into what
  goes to the Trash and what a tool removes for good whichever action you choose (for example
  `homebrew` or `docker`).
- `enter` opens the action box: Move to Trash, Dry-run, Review paths or Hard delete. **Hard
  delete asks again**, with Cancel focused: move to Delete for good with `←` or `→`, then press
  `enter`. It needs no `TINY_CONFIRM_HARD`; that is only for `-y` on the command.
- **A run stops only between categories.** `esc` (or `q`) lets the category in progress finish;
  the rest are listed as not run.
- **The result says how much is in the Trash and how much is gone for good**, plus anything the
  engine kept. Sizes are the scan sizes of the paths the engine reports removed. A removal a
  tool only accepted and finishes in the background is shown apart as `Requested`, not as
  cleaned. Space in the Trash returns only when the Trash is emptied. `enter` goes back to Clean
  and measures again.
- **If the engine stops inside a category**, that row says `stopped` with the engine's reason,
  and the category may be partly done; only the categories after it are `not run`.
- **The result is printed again after the view closes**, so it stays in the terminal's
  scrollback: every clean of the session added up, with what was kept and its size
  ([`exit_card.rs`](../../crates/tiny/src/tui/pages/clean/exit_card.rs)). A session with no
  clean prints nothing.

## Uninstall

What is listed, what is removed and what is refused are in
[Uninstalling apps](uninstall.md) ([`uninstall/engine.rs`](../../crates/tiny/src/tui/pages/uninstall/engine.rs)).

- The list is read when the tab first opens, with nothing ticked, least recently used first.
  `s` steps through last used, size and name; apps that tie are in name order. An app macOS
  has no last-use date for reads `unknown`.
- Apple apps and Homebrew casks are listed greyed with the reason and cannot be ticked. There is
  no `--force` here: remove a cask with `brew uninstall --cask` or `sift uninstall --force`.
- The highlighted app's leftovers are read as the cursor reaches it; the figure at the bottom is
  what Move to Trash or Hard delete removes for the ticked apps.
- **The view always removes the app and its `cache` and `data` entries**, as the command's menu
  does; `report-only` and `uncertain` entries, shown as `kept`, never move. There is no
  `--shallow` or `--leftovers-only` here.
- `enter` builds each ticked app's plan again and opens the action box: Move to Trash, Dry-run
  or Hard delete, which asks again with Cancel focused. **Both boxes split the total into
  `app`, `cache` and `data`**, and name the `data` as your settings and data in those apps,
  which every choice removes too ([`uninstall/boxes.rs`](../../crates/tiny/src/tui/pages/uninstall/boxes.rs)).
  If the engine refuses any ticked app at that point, nothing is removed.
- Apps are removed one by one; `esc` stops after the app in progress. The result names any app
  that was kept and why; an app that failed partway may be partly removed. `enter` reads the
  list again. Nothing is printed after the view closes.

## Scan

What is reported and how folders are walked are in [Scanning files](scan.md)
([`scan/engine.rs`](../../crates/tiny/src/tui/pages/scan/engine.rs)).

- Opening the tab reads nothing; `enter` starts the scan.
- It scans with the command's defaults only: Downloads, Desktop and Documents, 100 MB and up,
  untouched for 90 days. For other folders or thresholds, use `sift scan`.
- Both lists hold every file found, largest first; there is no `--limit`. `v` switches between
  Large and Old.
- `o` shows the highlighted file in Finder. Nothing else can be done to a file here. If Finder
  cannot show it, the detail says why.
- `esc` stops waiting for a scan, but the walk itself cannot be interrupted: it finishes in the
  background and its result is dropped. `r` scans again; while the stopped walk is still
  running, the new scan waits for it and starts when it ends.

## Processes

Reading the list and the quit rules are in [Processes](processes.md)
([`processes/mod.rs`](../../crates/tiny/src/tui/pages/processes/mod.rs)).

- The list is read when the tab first opens and again on `r`. **It never refreshes by itself**,
  so rows do not move under the cursor. It holds every process, not the top 20; `s` switches
  between CPU and memory order.
- The detail shows the owner, parent, children and listening ports of the highlighted process.
- `x` quits (SIGTERM) and `X` force quits (SIGKILL) the highlighted process, each after a
  confirmation with Cancel focused; `TINY_CONFIRM_FORCE` is not needed. A process `sift` refuses
  to quit opens no box: its detail says why.
- The outcome is shown at the bottom, for example still running or denied by macOS, and the
  list is read again.

## System

The same reading as `sift sys`, once when the tab first opens and again on `r`
([`system/engine.rs`](../../crates/tiny/src/tui/pages/system/engine.rs)). Each volume is listed
once: the startup volume's sibling volumes, which share its space, are one row.

## Icons

Out of the box the view draws Unicode symbols only. `sift --icons nerd` adds a Nerd Font
(Material Design) icon before each tab and each category; `sift --icons unicode` turns them off
([`icons.rs`](../../crates/tiny/src/tui/icons.rs)).

- The choice is remembered in `~/.tiny-cli/tui-settings.json`, so later runs of plain `sift` use
  it ([`settings.rs`](../../crates/tiny/src/tui/settings.rs)).
- Nerd icons need a [Nerd Font](https://www.nerdfonts.com) set as the terminal's font; without
  one they show as empty boxes.
- `--icons` is accepted only right after a bare `sift`. With a command (`sift clean --icons nerd`)
  or in a pipe it is an unknown flag.
- If the settings file cannot be read or is not valid, the view draws Unicode, never overwrites
  the file, and says so on stderr after it closes.
