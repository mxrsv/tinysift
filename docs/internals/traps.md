# Known traps

> For maintainers. Using TinySift? See [docs/user/](../user/).

Things that are not obvious from reading one file.

## Deletion

- **`uninstall --hard -y` asks nothing.** `clean` and `processes quit --force` demand an
  environment variable with `-y`; `uninstall` deletes permanently on the flags alone
  ([`uninstall.rs`](../../crates/tiny/src/render/uninstall.rs)).
- **An `uninstall` plan stops at its first failing path**, leaving that app partly removed;
  later apps still run ([`execute_plan`](../../crates/tiny-core/src/uninstall.rs)).
- **Move to Trash is not always recoverable.** With "Empty Trash automatically" on, Finder
  deletes it later; on a volume without a Trash it may delete at once, so the app skips paths
  off the home volume ([`checked_execute.rs`](../../crates/tiny-core/src/clean/checked_execute.rs)).
- **Some categories ignore Trash.** `docker`, `docker-volumes`, `time-machine-local` and
  `simulator-devices` run their tool for both CLI actions. The CLI's warning lists those ids by
  hand in [`render/clean/mod.rs`](../../crates/tiny/src/render/clean/mod.rs); add new
  tool-backed categories there.
- **`category_family` panics** on an unknown id. Register a new category in all four registry
  functions ([clean.md](clean.md#the-provider-registry)).

## Running locally

- **Real runs touch real data.** Only `--dry-run` and validation errors are side-effect free, so
  [`clean_smoke.rs`](../../crates/tiny/tests/clean_smoke.rs) is limited to those.
- **The dry-run smoke test is slow on a populated home folder**: it walks every
  category on the real disk ([smoke test](../../crates/tiny/tests/clean_smoke.rs)).
- **The first Trash action prompts for Automation access** to Finder. After a denial the CLI
  fails every path and carries on, while the app recognises error -1743 and stops with
  `AutomationDenied` ([`finder_trash.rs`](../../crates/tiny-core/src/clean/finder_trash.rs)).
  Grant access in System Settings → Privacy & Security → Automation.
- **The port smoke test is macOS-only and needs `/usr/sbin/lsof`.** It skips without
  `python3` ([`processes_smoke.rs`](../../crates/tiny/tests/processes_smoke.rs)); Linux
  [CI](../../.github/workflows/ci.yml) retains the platform-independent port fixture tests.


## Native integration

Bindings and the linked archive must come from the same core revision.
Use [build-ffi.sh](../../scripts/build-ffi.sh); its archive excludes bindgen and test hooks.
