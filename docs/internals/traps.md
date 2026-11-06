# Known traps

> For maintainers. Using TinySift? See [docs/user/](../user/).

Things that are not obvious from reading one file.

## Deletion

- **`uninstall --hard -y` asks nothing.** `clean` and `processes quit --force` demand an
  environment variable with `-y`; `uninstall` deletes the app and its cache leftovers permanently on the
  flags alone ([`uninstall.rs`](../../crates/tiny/src/render/uninstall.rs)).
- **An `uninstall` plan stops at its first failing path**, leaving that app partly removed;
  later apps still run ([`execute_plan`](../../crates/tiny-core/src/uninstall.rs)).
- **Move to Trash is not always recoverable.** With "Empty Trash automatically" on, Finder
  deletes it later; on a volume without a Trash it may delete at once, so the app skips paths
  off the home volume ([`checked_execute.rs`](../../crates/tiny-core/src/clean/checked_execute.rs)).
- **Some categories ignore Trash.** `docker`, `docker-volumes`, `pnpm`, `uv`, `bun-cache`,
  `pip-cache`, `homebrew`, `time-machine-local`, `simulator-devices` and `simulator-runtimes`
  run their tool for both CLI actions. `pnpm`
  used to move its store to the Trash and now prunes it, so `--category pnpm` is permanent. The
  CLI's warning lists those ids by hand in
  [`render/clean/mod.rs`](../../crates/tiny/src/render/clean/mod.rs); add new tool-backed
  categories there.
- **`category_family` panics** on an unknown id. Register a new category in every registry
  function and the `SECTIONS` test ([clean.md](clean.md#the-provider-registry)).
- **Finder's Empty Trash empties every volume's Trash**, but the Trash row counts, fingerprints and
  measures only `~/.Trash`, so the bytes it reports can be lower than what disappears, and it also
  takes anything put there while Finder waits for the Automation prompt
  ([clean.md](clean.md#the-desktops-checked-removal)).
- **Clean's deletion is best effort per entry; uninstall's is not.** Clean's `Delete` goes on past
  an entry that fails and ends `PartlyRemoved` with measured freed and left bytes
  ([`measure.rs`](../../crates/tiny-core/src/clean/checked_execute/measure.rs)). Uninstall and the
  CLI use the fail-fast [walker](../../crates/tiny-core/src/clean/fs_safe/remove_tree.rs) form,
  where earlier entries may already be gone when it stops.
- **The measuring walk opens a window, so identity is checked after it.** `prepare` repeats the
  fingerprint (the listing for the Trash row), home-volume and root checks after the walk
  (`same_file` in [`checked_execute.rs`](../../crates/tiny-core/src/clean/checked_execute.rs)), and
  the deletion pins its root to the fingerprinted device and inode. Without both, a folder replaced
  or a disk image mounted at the path during the walk would be deleted, and anything trashed during
  the walk of `~/.Trash` emptied. Finder's Empty Trash cannot be pinned, so its window is only from
  the check to Finder.
- **Freed bytes cost a walk before and after each removal.** A partly removed item, a pruned cache
  and the emptied Trash are walked twice; a fully deleted item only before, since a missing path
  measures 0 without a walk.
- **Permanent deletion rejects symlinked ancestors.** The
  [walker](../../crates/tiny-core/src/clean/fs_safe/remove_tree.rs) opens one component at a time;
  a path through `/var` on a Mac where it aliases `/private/var` is refused. Tests must canonicalize
  their own temporary fixture roots, never production cleanup targets. Pinned descriptors prevent
  symlink redirection, not concurrent renames or an atomic compare-and-unlink
  ([limits](clean.md#the-desktops-checked-removal)).
- **A folder the walk after a removal cannot read makes its freed bytes an upper bound**
  (`ItemResult::freed_is_upper_bound`, `Pruned.upper_bound`), since the size after is then too
  low; one the walk before could not read only understates them.
- **The app and the shell can run different pnpm.** An older Homebrew pnpm and a newer nvm or
  Corepack pnpm use different stores (`store/v10` against `store/v11` on the maintainer's Mac). The app's lookup puts nvm's pnpm before Homebrew's
  (`ToolLookup::app`); when a prune frees less than the sized store, check which pnpm resolved.
- **uv in Trash mode moves the whole cache folder** to the Trash, since a prune has no Trash
  route; only Permanent mode runs `uv cache prune`.
- **Homebrew's figure costs three dry runs.** `brew cleanup --prune=30 --dry-run` (about 5 s)
  runs at the scan and right before and after the cleanup, and the paths it lists are sized by
  our own walk, so figure and freed follow one rule
  ([`homebrew.rs`](../../crates/tiny-core/src/clean/providers/homebrew.rs)). Paths inside
  `~/Library/Caches` count in neither: `user-caches` lists Homebrew's download cache whole, so a
  cleanup without App Caches selected frees old downloads that no figure shows. `brew cleanup`
  would also autoremove formulae its dry run never sizes; `TOOL_ENV` in
  [`runner.rs`](../../crates/tiny-core/src/runner.rs) sets `HOMEBREW_NO_AUTOREMOVE` for every
  non-isolated `brew`.
- **Docker's freed bytes are Docker's, not the Mac's.** The prune is measured as its kind's
  Reclaimable before minus after (`docker system df`), and Reclaimable also counts data newer
  than the `until=168h` filter keeps, so the figure is only a ceiling. The images live in Docker
  Desktop's disk image, which macOS sees shrink only when Docker compacts it, so the free-space
  reading may not rise ([`docker.rs`](../../crates/tiny-core/src/clean/providers/docker.rs)).
- **A tool path with a `/` must exist.** `ToolLookup::resolve` returns such a path only when an
  executable file is there; before, any absolute path "resolved", so probing fixed locations
  always took the first ([`runner.rs`](../../crates/tiny-core/src/runner.rs)). `MockRunner::which`
  checks a set, not the disk, so only a real `ToolRunner` test catches this.
- **`/usr/bin/pip3` is a shim.** Without the Command Line Tools it asks to install them, so
  `pip-cache` runs only Homebrew's or the Command Line Tools' `pip3` by absolute path and spawns
  nothing unless `~/Library/Caches/pip` exists. Only with such a `pip3` does `pip-cache` list that
  folder and `user-caches` leave it out; without one (pyenv, conda, python.org) it stays a
  `user-caches` folder, so it is never counted twice or left uncleanable
  ([`purged_caches.rs`](../../crates/tiny-core/src/clean/providers/purged_caches.rs)).

## Cleanup sizes

- **Cleanup sizes are freed bytes, not `du` and not Space Lens.** On 2026-10-22 `du` showed about
  18 GiB for four cargo `target/` folders and deleting them freed 14 GiB, because of hard links and
  APFS clones ([`freed_size.rs`](../../crates/tiny-core/src/clean/freed_size.rs)). An inode split
  between two items counts in neither, so a selection total must be measured as a union.

## Administrator deletion

- **The paths must follow `--`.** [`OsascriptDelete`](../../crates/tiny-core/src/clean/elevated_delete.rs)
  passes entry names after `--`, so a name that starts with `-` is never an `osascript` option, and
  each reaches the root shell through `quoted form of`. Never splice a name into the script.
- **A root cache has no Trash and no unprivileged retry.** The folders were `root:admin` 755 when checked on
  2026-10-23, so the user cannot delete them; a cancelled or failed administrator deletion is final for that run.
- **EDR products may flag `osascript … with administrator privileges`**, and Apple may
  deprecate the route for apps. A run that fails at once with no dialog points at `env_clear` in
  `run_script`.

## Desktop protection

- **A rule write needs a prior read in the same session.** `clean_keep` and `clean_unkeep`
  fail with `ProtectionChanged` unless the stored rules equal what the last `clean_protection`
  or successful write returned, in revision and content; a write that fails after taking the
  lock clears that memory, so Swift reads again before the next one
  ([`change_rules`](../../crates/tiny-ffi/src/clean/protection.rs)). A new caller cannot pass a
  revision it got elsewhere.
- **A same-revision outside edit still counts.** Snapshots compare content as well as the
  revision, so a hand-edited `rules.json` invalidates previews and writes, and stops a running
  cleanup ([`ensure_unchanged`](../../crates/tiny-core/src/clean/protection_store.rs)).
- **Spelling depends on the volume.** Revalidate native case and Unicode matching on both
  case-sensitive and case-insensitive volumes; in-memory checks alone do not verify the
  host filesystem's behavior ([tests](../../crates/tiny-core/src/clean/protection.rs)).
- **The CLI ignores the rules.** Nothing in `tiny` or [`execute.rs`](../../crates/tiny-core/src/clean/execute.rs)
  reads the store, so `tiny clean` can still move a path the desktop keeps.
- **An unreadable store is not an empty one.** Do not "repair" it by writing empty rules: moving,
  saving and removing stay blocked until the original settings read again
  ([clean](clean.md#desktop-persistent-protection)).
- **Deleting the whole store looks like first use** to a session that never read it, and the
  lock can exist before the first rule is saved.

## Running locally

- **Real runs touch real data.** Only `--dry-run` and validation errors are side-effect free, so
  [`clean_smoke.rs`](../../crates/tiny/tests/clean_smoke.rs) is limited to those.
- **The dry-run smoke test is slow on a populated home folder**: it walks every
  category on the real disk ([smoke test](../../crates/tiny/tests/clean_smoke.rs)).
- **The first Finder action prompts for Automation access**: a CLI move, an uninstall move or the
  app's Empty Trash. After a denial the CLI fails every path and carries on; uninstall recognises
  error -1743 and stops with `AutomationDenied`; Clean only fails its Trash row
  ([`finder_trash.rs`](../../crates/tiny-core/src/clean/finder_trash.rs)), since its moves never
  ask Finder ([`native_trash.rs`](../../crates/tiny-core/src/clean/native_trash.rs)). Grant access
  in System Settings → Privacy & Security → Automation.
- **Clean's moves have no Put Back.** `NSFileManager trashItemAtURL` moves without telling Finder
  where an item came from; drag it back out of the Trash instead. The native-move test that touches
  the real Trash is `#[ignore]`d; run it by hand with `--ignored`.
- **The port smoke test is macOS-only and needs `/usr/sbin/lsof`.** It skips without
  `python3` ([`processes_smoke.rs`](../../crates/tiny/tests/processes_smoke.rs)); the
  platform-independent port fixture tests run everywhere.

## Code layout

- **The CLI's clean and scan modules are mostly shims**: files such as
  [`render/clean/types.rs`](../../crates/tiny/src/render/clean/types.rs) and
  [`render/scan/walker.rs`](../../crates/tiny/src/render/scan/walker.rs) only re-export
  `tiny-core`, where the logic belongs.

## Storage scan

- **Totals are deliberately not "everything under the path".** A multi-link file counts once per
  scan, and another volume is never entered, so a size can be smaller than `du`. On macOS `/`
  and `/System/Volumes/Data` share one `st_dev`; the device check alone would count the data
  volume twice, so [`space_lens.rs`](../../crates/tiny-core/src/space_lens.rs) skips that path
  from every root above it (`/`, `/System`, `/System/Volumes`), not only from `/`. Foreign
  mounts are counted in `other_volume_count`, never marked unreadable.
- **Three different "not fully measured" signals.** `unreadable` (a read failed) and
  `depth_limited` (a folder past the depth limit was not walked) both make `bytes` a lower
  bound and set `incomplete`; `omitted_*` only hides entries that are still in the totals.
  An entry that vanishes mid-scan is ignored, not reported.

## Native integration

Bindings and the linked archive must come from the same core revision.
Use [build-ffi.sh](../../scripts/build-ffi.sh); its archive excludes bindgen and test hooks.
