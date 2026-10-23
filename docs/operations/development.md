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

[CI](../../.github/workflows/ci.yml) keeps Rust tests/lint on macOS and Linux. Portable checks
do not establish that Finder, `lsof`, `tmutil` or other macOS tools work on another platform.
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
The client must consume these outputs together and regenerate them after an FFI change.

Rust tests cover the [session](../../crates/tiny-ffi/src/session.rs) and
[cleanup boundary](../../crates/tiny-ffi/src/clean.rs). Native UI/actor tests and packaging are
owned by the app repository, and remain necessary before shipping an app against a new core pin.
