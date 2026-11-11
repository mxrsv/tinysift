# Releases

> For maintainers. Users download the app from the
> [Releases page](https://github.com/mxrsv/tinysift/releases).

This public repository hosts the signed TinySift Mac app DMGs. The app source, build scripts and
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
