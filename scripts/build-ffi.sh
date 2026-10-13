#!/bin/bash
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
if [ "$#" -ne 1 ] || [[ "$1" != /* ]] || [ "$1" = / ]; then
  printf 'Usage: %s /absolute/generated-output-directory\n' "$0" >&2
  exit 2
fi
GENERATED="$1"
NATIVE_TARGET="$(rustc -vV | sed -n 's/^host: //p')"
cd "$ROOT"
mkdir -p "$GENERATED"
cargo build -p tiny-ffi --release --locked --target-dir "$ROOT/target" --target "$NATIVE_TARGET"
# Keep bindgen and test-hooks out of the archive consumed by a native client.
cp "$ROOT/target/$NATIVE_TARGET/release/libtiny_ffi.a" "$GENERATED/libtiny_ffi.a"
cargo run -p tiny-ffi --features bindgen --bin uniffi-bindgen --locked \
  --target-dir "$ROOT/target" --target "$NATIVE_TARGET" -- \
  generate --library "$GENERATED/libtiny_ffi.a" --language swift --out-dir "$GENERATED"
cp "$GENERATED/tiny_ffiFFI.modulemap" "$GENERATED/module.modulemap"
printf 'Generated core bridge: %s\n' "$GENERATED"
