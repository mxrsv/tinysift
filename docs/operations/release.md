# Releases

> For maintainers. Users download the app from the
> [Releases page](https://github.com/mxrsv/tinysift/releases).

This public repository hosts the signed TinySift Mac app DMGs. The app source, build scripts and
signing setup live in the private app repository; the rules below are what every release must
satisfy.

## Naming

| Item | Rule | Example |
| --- | --- | --- |
| Tag | `app-v<MAJOR>.<MINOR>.<PATCH>`, on the core commit pinned by the app (`core-revision`) | `app-v0.1.2` |
| Title | `TinySift app <version> (beta)` while the app is in beta | `TinySift app 0.1.2 (beta)` |
| Asset | exactly one `TinySift-<version>.dmg` | `TinySift-0.1.2.dmg` |
| Flag | mark as **pre-release** until the app leaves beta | |

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
  --target <pinned-core-commit> --prerelease --title "TinySift app <version> (beta)" \
  --notes-file notes.md
```

## Release notes

Write in English, in this order:

1. One sentence naming the release (`Third beta of the TinySift Mac app. Free to use.`).
2. User-visible changes as short bullets. No internal refactors.
3. Requirements: minimum macOS version and supported CPU architectures.
4. A **What the app sends** paragraph whenever network behavior changes.
5. A `SHA-256:` line with the hash of the uploaded DMG (`shasum -a 256 TinySift-<version>.dmg`).

## Problems after publishing

A bad release is superseded by a new patch version, not edited. Remove a published asset only if
it is unsafe, and say why in the next release's notes.
