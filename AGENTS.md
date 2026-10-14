> Global standard: ~/.claude v2026-10-04.

# TinySift Core

Rust workspace containing [`tiny-core`](crates/tiny-core/src/lib.rs), the [`tiny` CLI](crates/tiny/src/main.rs)
and the [`tiny-ffi` UniFFI boundary](crates/tiny-ffi/src/lib.rs). Native presentation and the
website live in the separate private app repository. Start with [docs](docs/README.md).
This workspace's planned public GitHub name is `mxrsv/tinysift`; its local directory remains
`tinysift-core`. The private app GitHub repository is `mxrsv/tinycore`, in the local `tiny-cli`
directory. See the [repository names](README.md#repository-names) before changing remotes or links.

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
- Register cleanup categories in `all_providers_with`, `known_category_ids`, `category_family`
  and `comes_back` in [`providers/mod.rs`](crates/tiny-core/src/clean/providers/mod.rs); the
  desktop may move paths only when `desktop_trash_paths()` is true.
- In cleanup, discovered paths use `symlink_metadata`; deletion goes through
  [`fs_safe.rs`](crates/tiny-core/src/clean/fs_safe.rs) or
  [`FinderTrash`](crates/tiny-core/src/clean/finder_trash.rs). Never call `fs::remove_*` or `is_dir()` on discovered paths. Honor `quit_apps()` and
  `item_apps()` both during discovery and before execution; never bypass app guards to find more data.
- Preserve menu-level `CleanAction` versus executor-level `ExecAction`, including the
  Trash provider's `EmptyTrash` mapping ([actions](crates/tiny-core/src/clean/execute.rs)).
- Scan stays read-only. Review-risk paths require explicit consent; the FFI must preserve
  preview single-use/expiry, policy checks and the busy gate ([contract](docs/internals/clean.md)).
- Tests never delete real user data. Use mocks, test-created temporary directories and CLI
  `--dry-run`; never invoke real cleanup as a smoke check. See [development](docs/operations/development.md).
- Never enable `test-hooks` in a shipped static library. Generate bindings from the same
  archive/revision the client links, using [build-ffi.sh](scripts/build-ffi.sh).

## Documentation

CLI guides live in `docs/user/`; contracts/traps in `docs/internals/`; maintainer commands in
`docs/operations/`. Keep a single [glossary](docs/internals/glossary.md). Specs/plans for the
cross-repository migration stay in the owning private app repository; do not duplicate them.
Every living behavior claim links to its implementation or canonical local guide.

## Language

Docs, comments and CLI copy are English. Commit messages use conventional scope prefixes
with Vietnamese descriptions; PR titles/bodies are Vietnamese. Follow the shared global rules.
