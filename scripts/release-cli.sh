#!/usr/bin/env bash
# Builds the tinysift command-line tool as a signed, notarized archive. Publishes nothing:
# the tag, the GitHub release and the Homebrew formula are separate steps of
# docs/operations/release.md#the-command-line-tool.
#
#   scripts/release-cli.sh <version> [--no-notarize]
#
# <version> must equal the workspace version in Cargo.toml. --no-notarize is a rehearsal:
# the archive is signed but Gatekeeper will refuse it when it is downloaded in a browser.
#
# TINY_SIGN_IDENTITY   Developer ID Application identity (default: the first one in the keychain)
# TINY_NOTARY_PROFILE  notarytool keychain profile (default: tinysift-notary)
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TARGET="aarch64-apple-darwin"
BINARY="tinysift"
NOTICES="THIRD-PARTY-NOTICES.md"
NOTARY_PROFILE="${TINY_NOTARY_PROFILE:-tinysift-notary}"

fail() { printf 'release-cli: %s\n' "$1" >&2; exit 1; }

VERSION="${1:-}"
NOTARIZE=1
[ "${2:-}" = "--no-notarize" ] && NOTARIZE=0
[[ "$VERSION" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || fail "usage: scripts/release-cli.sh <MAJOR.MINOR.PATCH> [--no-notarize]"

cd "$ROOT"
WORKSPACE_VERSION="$(cargo metadata --no-deps --format-version 1 --locked |
  sed -n 's/.*"name":"tinysift","version":"\([^"]*\)".*/\1/p')"
[ "$WORKSPACE_VERSION" = "$VERSION" ] ||
  fail "Cargo.toml says $WORKSPACE_VERSION, not $VERSION: raise the workspace version first"
[ -z "$(git status --porcelain)" ] || fail "the checkout has uncommitted changes"
command -v cargo-about >/dev/null || fail "cargo-about is missing: cargo install cargo-about --locked"

IDENTITY="${TINY_SIGN_IDENTITY:-$(security find-identity -v -p codesigning |
  sed -n 's/.*"\(Developer ID Application:.*\)"/\1/p' | head -1)}"
[[ "$IDENTITY" == "Developer ID Application:"* ]] || fail "no Developer ID Application identity in the keychain"
if [ "$NOTARIZE" = 1 ]; then
  xcrun notarytool history --keychain-profile "$NOTARY_PROFILE" >/dev/null 2>&1 ||
    fail "no notary profile $NOTARY_PROFILE: store one with xcrun notarytool store-credentials"
fi

NAME="$BINARY-$VERSION-$TARGET"
OUT="$ROOT/target/dist"
STAGE="$OUT/$NAME"
rm -rf "$STAGE" "$OUT/$NAME.tar.gz" "$OUT/$NAME.zip"
mkdir -p "$STAGE"

cargo build -p "$BINARY" --release --locked --target "$TARGET"
cp "$ROOT/target/$TARGET/release/$BINARY" "$STAGE/$BINARY"
cp "$ROOT/LICENSE" "$STAGE/LICENSE"
cargo about generate --locked -m crates/tiny/Cargo.toml scripts/third-party-notices.hbs -o "$STAGE/$NOTICES"

codesign --force --sign "$IDENTITY" --timestamp --options runtime "$STAGE/$BINARY"
codesign --verify --strict --verbose=2 "$STAGE/$BINARY"

if [ "$NOTARIZE" = 1 ]; then
  # notarytool takes a zip, not a tar.gz. A bare executable cannot be stapled, so Gatekeeper
  # looks the ticket up online the first time the downloaded file runs.
  ditto -c -k --keepParent "$STAGE/$BINARY" "$OUT/$NAME.zip"
  xcrun notarytool submit "$OUT/$NAME.zip" --keychain-profile "$NOTARY_PROFILE" --wait
  rm "$OUT/$NAME.zip"
  # spctl --type execute rejects every bare executable ("does not seem to be an app"), notarized or
  # not, so ask codesign whether Apple holds a ticket for this exact signature.
  codesign --verify --strict -R="notarized" --check-notarization "$STAGE/$BINARY"
else
  printf 'release-cli: --no-notarize: signed but NOT notarized. Do not publish this archive.\n' >&2
fi

# The same bytes on every run of the same commit: fixed order, no owner names, no extended attributes.
COPYFILE_DISABLE=1 tar -czf "$OUT/$NAME.tar.gz" -C "$OUT" --uid 0 --gid 0 --uname root --gname wheel "$NAME"
"$STAGE/$BINARY" --version
printf 'Archive: %s\nSHA-256: %s\n' "$OUT/$NAME.tar.gz" "$(shasum -a 256 "$OUT/$NAME.tar.gz" | cut -d' ' -f1)"
