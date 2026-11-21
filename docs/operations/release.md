# Releases

> For maintainers. Users download the app from the
> [Releases page](https://github.com/mxrsv/tinysift/releases).

This public repository hosts the signed TinySift Mac app DMGs and the archives of the
[command-line tool](#the-command-line-tool). The app source, build scripts and
signing setup live in the private app repository; the rules below are what every release must
satisfy.

## Naming

| Item | Rule | Example |
| --- | --- | --- |
| Tag | `app-v<MAJOR>.<MINOR>.<PATCH>`, on the core commit pinned by the app (`core-revision`) | `app-v0.7.2` |
| Title | `TinySift app <version>` | `TinySift app 0.7.2` |
| Asset | exactly one `TinySift-<version>.dmg` | `TinySift-0.7.2.dmg` |
| Flag | a full release, not a pre-release, so the newest is GitHub's "Latest" | |

Versions follow [SemVer](https://semver.org). The app version is independent of the crate
versions in [Cargo.toml](../../Cargo.toml); never reuse or move a published tag.

## Checklist

1. The pinned core commit is pushed to `main` here and CI is green on it.
2. The app is built with its distribution script from a clean checkout at an exact core pin.
3. The DMG is Developer ID signed, notarized and stapled. Verify before upload:

   ```bash
   codesign --verify --deep --strict --verbose=2 TinySift-<version>.dmg
   xcrun stapler validate TinySift-<version>.dmg
   spctl --assess --type open --context context:primary-signature -v TinySift-<version>.dmg
   ```

4. A real install of the DMG on a Mac other than the build machine launches without a Gatekeeper
   warning; for an update release, an installed previous version updates to it in-app.
5. Create the release with the DMG attached, then publish the Sparkle appcast last. Never change
   the DMG bytes after signing or after publishing the checksum.

```bash
gh release create app-v<version> TinySift-<version>.dmg --repo mxrsv/tinysift \
  --target <pinned-core-commit> --title "TinySift app <version>" \
  --notes-file notes.md
```

6. Bump the Homebrew cask in [mxrsv/homebrew-tap](https://github.com/mxrsv/homebrew-tap): set
   `version` and `sha256` in `Casks/tinysift.rb` to the uploaded DMG, then commit and push.
   `brew livecheck --cask mxrsv/tap/tinysift` reports when the tap is behind the appcast.

## The command-line tool

The `tinysift` executable (typed as `sift`) is released from this repository on its own tags, apart
from the app.

| Item | Rule | Example |
| --- | --- | --- |
| Tag | `cli-v<MAJOR>.<MINOR>.<PATCH>`, equal to the workspace version in [Cargo.toml](../../Cargo.toml), on a commit of `main` with green CI | `cli-v0.1.0` |
| Title | `TinySift CLI <version>` | `TinySift CLI 0.1.0` |
| Asset | exactly one `tinysift-<version>-aarch64-apple-darwin.tar.gz` holding `tinysift`, `LICENSE` and `THIRD-PARTY-NOTICES.md` | |
| Flag | `--latest=false`, always: "Latest" belongs to the app, and the README badge and download links read it | |

1. Raise `version` under `[workspace.package]`, commit, push, and wait for CI on `main`.
2. Build the archive from a clean checkout of that commit with [release-cli.sh](../../scripts/release-cli.sh).
   It builds for Apple silicon, writes the notices from `Cargo.lock` with `cargo-about`
   ([accepted licences](../../about.toml)), signs with the Developer ID identity, notarizes and prints
   the SHA-256. It publishes nothing.

   ```bash
   scripts/release-cli.sh <version>
   ```

   `--no-notarize` is a rehearsal only. A bare executable cannot be stapled, so check the real
   archive the way a user meets it: download it in a browser on another Mac, unpack it and run
   `./tinysift --version`. Gatekeeper must not object.
3. Create the release. Never let it become "Latest":

   ```bash
   gh release create cli-v<version> target/dist/tinysift-<version>-aarch64-apple-darwin.tar.gz \
     --repo mxrsv/tinysift --target <commit> --title "TinySift CLI <version>" \
     --latest=false --notes-file notes.md
   gh release view --repo mxrsv/tinysift --json tagName   # must still print the newest app-v tag
   ```

4. Notes follow [Release notes](#release-notes) with the archive's hash on the `SHA-256:` line, and
   say once that `sift` and `tinysift` are the same tool.
5. Bump `Casks/tinysift-cli.rb` in [mxrsv/homebrew-tap](https://github.com/mxrsv/homebrew-tap): set
   `version` and `sha256` to the uploaded archive, then commit and push. It is a cask, not a
   formula: Homebrew treats a formula without a bottle as a source build and demands current
   Command Line Tools from every user. The cask links `tinysift`, and links `sift` only when no
   other `sift` is on the Mac. Check with `brew install --cask mxrsv/tap/tinysift-cli`.

Users get the new version with `brew upgrade --cask mxrsv/tap/tinysift-cli` once step 5 is pushed;
the CLI does not update itself. `brew livecheck --cask mxrsv/tap/tinysift-cli` reports when the tap
is behind the newest `cli-v` tag.

## Release notes

Write in English, in this order:

1. One sentence naming the release (`TinySift 0.7.2 for Mac fixes the high CPU use that 0.7.1 introduced.`).
2. User-visible changes as short bullets. No internal refactors.
3. Requirements: minimum macOS version and supported CPU architectures.
4. A **What the app sends** paragraph whenever network behavior changes.
5. A `SHA-256:` line with the hash of the uploaded DMG (`shasum -a 256 TinySift-<version>.dmg`).

## Problems after publishing

A bad release is superseded by a new patch version, not edited. Remove a published asset only if
it is unsafe, and say why in the next release's notes.
