# TinySift Core

The Rust engine, `tiny` command-line tool and UniFFI interface used by TinySift.
The [workspace](Cargo.toml) keeps all three crates together so a change to cleanup consent
or process identity can be tested across the engine and native interface in one revision.

## Repository names

The planned public GitHub repository is `mxrsv/tinysift`, matching the TinySift brand.
It will contain this Rust workspace and host app binaries through GitHub Releases.
The private SwiftUI app and website belong to `mxrsv/tinycore`.
Local directories remain `tinysift-core` for Rust and `tiny-cli` for the app;
the [native builder](scripts/build-ffi.sh) does not depend on GitHub naming.
Publication has not happened yet; see [Publication](#publication).

## Build and check

Use Rust 1.91 or newer. From this checkout:

```bash
cargo install --path crates/tiny
cargo fmt --all --check
cargo test --workspace --all-targets --locked
cargo clippy --workspace --all-targets --locked -- -D warnings
```

macOS is required for Finder, port inspection and tool-backed cleanup. Portable tests also run
on Linux; see [development](docs/operations/development.md) for the limits of those checks.

## Commands

| Command | Guide |
| --- | --- |
| `tiny sys`, `tiny focus` | [Getting started](docs/user/getting-started.md) |
| `tiny scan` | [Scanning files](docs/user/scan.md) |
| `tiny clean` | [Cleaning caches](docs/user/clean.md) |
| `tiny uninstall` | [Uninstalling apps](docs/user/uninstall.md) |
| `tiny processes` | [Processes](docs/user/processes.md) |

## Native integration

The [FFI builder](scripts/build-ffi.sh) emits the static library and matching Swift bindings
to an explicit output directory. The app owns copying those generated bindings into its
Swift target. Neither the private app nor its website is part of this repository.

Read the [native safety contract](docs/internals/clean.md), [architecture](docs/internals/overview.md)
and [documentation index](docs/README.md) before extending the interface.

## Publication

This local extraction is not a published release. [Cargo metadata](Cargo.toml) retains the
existing MIT declaration; copyright ownership, the license file and third-party distribution
notices must be finalized before this repository or binaries are published.
