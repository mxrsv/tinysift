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

Use [Conventional Commits](https://www.conventionalcommits.org) with a scope. Enable the
[commit-msg check](.githooks/commit-msg) once per clone:

```bash
git config core.hooksPath .githooks
```

It checks the header form, its length, the blank line before the body and the language; the
rest of the list is for reviewers. [CI](.github/workflows/commit-messages.yml) runs the same
check on every commit pushed to `main` or proposed in a PR, and on the PR title and body.

- **Header:** `type(scope): description`, at most 72 characters. Write the description in the
  imperative mood (`add`, not `added` or `adds`), start it with a lower-case word and end it
  without a period.
- **Types:** `feat` (new behavior), `fix` (bug fix), `perf` (faster or lighter, same results),
  `refactor` (no behavior change), `test`, `docs`, `build` (Cargo, build scripts), `ci`
  (workflows), `chore` (repository upkeep).
- **Scopes** are kebab-case and name the area changed: `clean`, `uninstall`, `processes`,
  `storage`, `space-lens`, `ffi`, `core` for code; `docs`, `readme`, `release`, `agents`,
  `github`, `license` for repository files.
- **Body:** optional, after one blank line, wrapped at 72 characters. Explain what changed and
  why; the diff already shows how.
- **Breaking changes** to the CLI, the FFI records or the consent contract add `!` after the
  scope (`feat(ffi)!: …`) and a `BREAKING CHANGE:` footer.
- One commit is one complete, buildable piece of work. Do not mix unrelated changes.
- Everything committed is English: code, comments, docs, CLI copy, commit messages, PR text
  and review comments. The check rejects Vietnamese letters, so it also rejects accented words
  such as `café`.
- Messages git writes itself (`Merge …`, `Revert "…"`, `fixup!`, `squash!`, `amend!`) are exempt.

```text
fix(clean): stop moving into the Trash after a failed emptying

Finder may still be emptying after a timed-out or failed Empty Trash, and
would take what the run moves in. The empty script now runs under
'with timeout of 540 seconds' (below the runner's 600) so a large Trash is
not reported failed after AppleScript's two-minute default.
```

Early commits predate these rules and stay as they are: release tags and the app's pinned core
revision point at their hashes, so history is never rewritten.

## Pull requests

- One concern per PR, title in the same `type(scope): description` form.
- Fill in the [template](.github/pull_request_template.md): what changed and why, and how you
  tested it.
- CI must be green before merge.

## Releases

Maintainers publish the macOS app DMGs as GitHub Releases of this repository. The rules are in
the [release guide](docs/operations/release.md); the latest build is on the
[Releases page](https://github.com/mxrsv/tinysift/releases).
