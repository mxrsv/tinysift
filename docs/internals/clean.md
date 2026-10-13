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
under an opaque ID, so Swift never sends a path back. `clean_preview` drops paths another
selected path covers and excludes any that would carry an unselected Review candidate along
(the explicit-selection contract);
a preview is single-use and expires. `clean_execute` consumes it, then
[`execute_checked`](../../crates/tiny-core/src/clean/checked_execute.rs) revalidates each path,
and everything it covers, just before moving it through the
[`Trash`](../../crates/tiny-core/src/clean/finder_trash.rs) seam. An Automation denial or
cancellation stops the run. Scan and execute refuse to start when `HOME` is not the account's
home folder (`FfiError::UntrustedHome`).

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
