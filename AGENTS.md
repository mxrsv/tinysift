> Global standard: ~/.claude v2026-10-04.

# TinySift Core

Rust workspace containing [`tiny-core`](crates/tiny-core/src/lib.rs), the [`tiny` CLI](crates/tiny/src/main.rs)
and the [`tiny-ffi` UniFFI boundary](crates/tiny-ffi/src/lib.rs). Native presentation and the
website live in the separate private app repository. Start with [docs](docs/README.md).
This workspace's public GitHub repository is `mxrsv/tinysift`; its local directory remains
`tinysift-core`. The private app GitHub repository is `mxrsv/tinycore`, in the local `tiny-cli`
directory. See the [repository names](docs/operations/development.md#repository-names) before changing remotes or links.

## Common commands

| Command | Purpose |
| --- | --- |
| `cargo fmt --all --check` | Format |
| `cargo test --workspace --all-targets --locked` | Rust and CLI tests |
| `cargo clippy --workspace --all-targets --locked -- -D warnings` | Lint |
| `scripts/build-ffi.sh /absolute/output` | Static library and matching Swift bindings |
| `cargo run -p tiny -- clean --dry-run` | Read-only cleanup discovery |

## Conventions and boundaries

- Rust modules use `snake_case`; retain the feature layout under `crates/`. Rust unit tests
  stay with their modules; CLI smoke tests live in [`crates/tiny/tests/`](crates/tiny/tests/).
- All operations have one implementation in core. CLI parses/presents; FFI owns records,
  consent, preview lifetime and the session gate. See [overview](docs/internals/overview.md).
- Register cleanup categories in `all_providers_with`, `known_category_ids`, `category_family`,
  `comes_back` and the `SECTIONS` table test of `category_section` in
  [`providers/mod.rs`](crates/tiny-core/src/clean/providers/mod.rs). Eligibility is deny by
  default: the desktop moves paths only when `desktop_trash_paths()` is true, and acts
  otherwise only through `desktop_empties_trash()` (the Trash row), `desktop_tool()` (a tool
  command) or `needs_admin()` (deletion as administrator).
- In cleanup, discovered paths use `symlink_metadata`; deletion goes through
  [`fs_safe.rs`](crates/tiny-core/src/clean/fs_safe.rs), [`FinderTrash`](crates/tiny-core/src/clean/finder_trash.rs),
  a category's tool command ([`tool_command.rs`](crates/tiny-core/src/clean/providers/tool_command.rs))
  or [`elevated_delete.rs`](crates/tiny-core/src/clean/elevated_delete.rs). Never call
  `fs::remove_*` or `is_dir()` on discovered paths, and never run a bundled binary or a shell
  built from a path as root. Honor `quit_apps()` and
  `item_apps()` both during discovery and before execution; never bypass app guards to find more data.
- Preserve menu-level `CleanAction` versus executor-level `ExecAction`, including the
  Trash provider's `EmptyTrash` mapping ([actions](crates/tiny-core/src/clean/execute.rs)).
- Scan stays read-only. Review-risk paths require explicit consent; the FFI must preserve
  preview single-use/expiry, policy checks and the busy gate ([contract](docs/internals/clean.md)).
- Tests never delete real user data. Use mocks, test-created temporary directories and CLI
  `--dry-run`; never invoke real cleanup as a smoke check. See [development](docs/operations/development.md).
- Never enable `test-hooks` in a shipped static library. Generate bindings from the same
  archive/revision the client links, using [build-ffi.sh](scripts/build-ffi.sh).

## Commits and releases

Commit and PR rules are in [CONTRIBUTING](CONTRIBUTING.md); app DMG releases follow the
[release guide](docs/operations/release.md).

## Documentation

CLI guides live in `docs/user/`; contracts/traps in `docs/internals/`; maintainer commands in
`docs/operations/`. Keep a single [glossary](docs/internals/glossary.md). Specs/plans for the
cross-repository migration stay in the owning private app repository; do not duplicate them.
Every living behavior claim links to its implementation or canonical local guide.

## Language

Open-source standard: everything committed to the repository is English — docs, comments,
CLI copy, commit messages, PR titles/bodies, issues and review comments. Commits use
[Conventional Commits](https://www.conventionalcommits.org) with a scope, e.g.
`fix(core): skip symlinked app bundles`. Conversation with the maintainer may stay
Vietnamese; it never leaks into repository artifacts. Existing Vietnamese history is not
rewritten. Follow the shared global rules, except where this section overrides their language.
