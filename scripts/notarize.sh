#!/usr/bin/env bash
# Sign + notarize + staple the OnCue DMG.
#
# Required env vars (load from a .env file or your shell, never commit):
#   APPLE_ID            Apple Developer account email
#   APPLE_TEAM_ID       Team ID (10 chars)
#   APPLE_APP_PASSWORD  App-specific password (https://appleid.apple.com/account/manage)
#   SIGNING_IDENTITY    Full name, e.g. "Developer ID Application: Foo Bar (TEAMID)"
#
# Usage:
#   ./scripts/notarize.sh
#
# Prereq:
#   - tauri.conf.json bundle.macOS.signingIdentity matches $SIGNING_IDENTITY
#     (or set to "-" and override here via --sign during codesign)
#   - bun run tauri build --target aarch64-apple-darwin has succeeded
set -euo pipefail

: "${APPLE_ID:?APPLE_ID is required}"
: "${APPLE_TEAM_ID:?APPLE_TEAM_ID is required}"
: "${APPLE_APP_PASSWORD:?APPLE_APP_PASSWORD is required}"
: "${SIGNING_IDENTITY:?SIGNING_IDENTITY is required}"

BUNDLE_DIR="src-tauri/target/aarch64-apple-darwin/release/bundle"
DMG=$(find "$BUNDLE_DIR/dmg" -name "*.dmg" -maxdepth 1 | head -n1)

if [ -z "$DMG" ] || [ ! -f "$DMG" ]; then
    echo "DMG not found under $BUNDLE_DIR/dmg. Run 'bun run tauri build --target aarch64-apple-darwin' first." >&2
    exit 1
fi

echo "→ Submitting $DMG to Apple notary service…"
xcrun notarytool submit "$DMG" \
    --apple-id "$APPLE_ID" \
    --team-id "$APPLE_TEAM_ID" \
    --password "$APPLE_APP_PASSWORD" \
    --wait

echo "→ Stapling notarization ticket…"
xcrun stapler staple "$DMG"

echo "→ Verifying…"
xcrun stapler validate "$DMG"
spctl --assess --type install --verbose=4 "$DMG" || true

echo "✓ Done: $DMG"
