# Contributing

Thanks for helping with TinySift Core. The code is [GPL-3.0-only](LICENSE); the TinySift
name and logo are covered by the [trademark policy](TRADEMARK.md).

## Before you open a PR

Run the same gates as [CI](.github/workflows/ci.yml):

```bash
cargo fmt --all --check
cargo test --workspace --all-targets --locked
cargo clippy --workspace --all-targets --locked -- -D warnings
```

Tests must never delete real user data: use mocks, temporary directories and `--dry-run`.
Read the [development guide](docs/operations/development.md) and the
[native safety contract](docs/internals/clean.md) before touching cleanup, uninstall or FFI code.

## Commits

Use [Conventional Commits](https://www.conventionalcommits.org) with a scope:
`type(scope): description`, imperative, lower-case, no trailing period, ≤ 72 characters.

- **Types:** `feat`, `fix`, `docs`, `test`, `refactor`, `perf`, `chore`.
- **Scopes** follow the code area: `clean`, `uninstall`, `processes`, `storage`, `space-lens`,
  `ffi`, `core`, `docs`, `license`.
- Breaking changes to the CLI, the FFI records or the consent contract add `!` after the scope
  (`feat(ffi)!: …`) and a `BREAKING CHANGE:` footer.
- One commit is one complete, buildable piece of work. Do not mix unrelated changes.
- Everything committed is English: code, comments, docs, CLI copy, commit messages, PR text
  and review comments.

## Pull requests

- One concern per PR, title in the same `type(scope): description` form.
- The body explains what changed and why, and lists how you tested it.
- CI must be green before merge.

## Releases

Maintainers publish the macOS app DMGs as GitHub Releases of this repository. The rules are in
the [release guide](docs/operations/release.md); the latest build is on the
[Releases page](https://github.com/mxrsv/tinysift/releases).
