# TinySift Core

The Rust engine, `tiny` command-line tool and UniFFI interface used by TinySift.
The [workspace](Cargo.toml) keeps all three crates together so a change to cleanup consent
or process identity can be tested across the engine and native interface in one revision.

## Repository names

This Rust workspace is the public GitHub repository `mxrsv/tinysift`, matching the TinySift
brand. Its [GitHub Releases](https://github.com/mxrsv/tinysift/releases) host the signed app
DMGs; the [release guide](docs/operations/release.md) holds the rules.
The private SwiftUI app and website belong to `mxrsv/tinycore`.
Local directories remain `tinysift-core` for Rust and `tiny-cli` for the app;
the [native builder](scripts/build-ffi.sh) does not depend on GitHub naming.
Commit and PR rules are in [CONTRIBUTING](CONTRIBUTING.md).

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

## License

This repository is licensed under [GPL-3.0-only](LICENSE). The code is open source; the
TinySift name and logo are not, see the [trademark policy](TRADEMARK.md). The source became
public on 2026-10-14 under the earlier MIT declaration, so copies obtained before the
relicense on 2026-10-17 remain under MIT. Third-party distribution notices for the released binaries are
still pending.
