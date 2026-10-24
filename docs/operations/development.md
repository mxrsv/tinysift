# Development

> For maintainers. Using the CLI? See [getting started](../user/getting-started.md).

Use Rust 1.91 or newer: the [lockfile](../../Cargo.lock) includes dependencies requiring it.
The [workspace](../../Cargo.toml) contains engine, CLI and FFI; update their contracts together.

## Verification

```bash
cargo fmt --all --check
cargo test --workspace --all-targets --locked
cargo clippy --workspace --all-targets --locked -- -D warnings
```

[CI](../../.github/workflows/ci.yml) runs these gates on macOS only, the one platform TinySift
ships on; nothing checks that the workspace builds or passes its tests elsewhere.
The [CLI smoke tests](../../crates/tiny/tests/) use dry runs and owned disposable processes;
tests must never delete real user data. A real home-directory discovery can take several minutes.

Deletion as administrator is tested without ever asking for it. The path checks run alone, and
the constant script runs through the real `/usr/bin/osascript` only with its administrator clause
removed, on temporary folders the tests create
([tests](../../crates/tiny-core/src/clean/elevated_delete/tests.rs)). `OsascriptDelete` is never
built in a test, and `clean_remove`, the only place that builds it, panics under `cfg!(test)`: run
the FFI path through `remove_clean` with fake sinks. The real dialog is checked by hand
before a release.

## Native clients

Run [build-ffi.sh](../../scripts/build-ffi.sh) with an absolute output directory:

```bash
scripts/build-ffi.sh /absolute/client/macos/Generated
```

The builder uses the native Rust host target and this repository's `target/`, ignoring
`CARGO_TARGET_DIR`/`CARGO_BUILD_TARGET` overrides. It first builds the static archive without
`bindgen` or `test-hooks`, then generates bindings from that archive. The output contains
`libtiny_ffi.a`, `tiny_ffi.swift`, `tiny_ffiFFI.h` and `module.modulemap`.
The client must consume these outputs together and regenerate them after an FFI change; the app
owns copying the generated bindings into its Swift target.

The cleanup records gained fields and options with defaults, but new enum cases break an exhaustive
Swift `switch`: `FfiReportOnlyReason::NeedsAdministrator` and `XcodeNotInstalled`,
`FfiEvidence::SimulatorRuntime`, and `FfiDesktopAction::EmptyTrash`, `RequestRemoval` and
`DeleteAsAdministrator`. Regenerate the bindings and handle every case when a client moves to a
new revision. `TinySession.clean_places` (`FfiCleanPlace`) is additive: it lists the categories a
scan with the same options can return, before any scan.

Rust tests cover the [session](../../crates/tiny-ffi/src/session.rs) and
[cleanup boundary](../../crates/tiny-ffi/src/clean.rs). Native UI/actor tests and packaging are
owned by the app repository, and remain necessary before shipping an app against a new core pin.
Read the [native safety contract](../internals/clean.md) and [architecture](../internals/overview.md)
before extending the interface.

## Repository names

This Rust workspace is the public GitHub repository `mxrsv/tinysift`, matching the TinySift
brand. Its [GitHub Releases](https://github.com/mxrsv/tinysift/releases) host the signed app
DMGs (all are beta pre-releases, so the sidebar shows only tags; open the Releases page);
the [release guide](release.md) holds the rules.
The private SwiftUI app and website belong to `mxrsv/tinycore`, so neither is part of this repository.
Local directories remain `tinysift-core` for Rust and `tiny-cli` for the app;
the [native builder](../../scripts/build-ffi.sh) does not depend on GitHub naming.
