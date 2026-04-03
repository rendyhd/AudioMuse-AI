#!/usr/bin/env bash
# build-dmg.sh
# Build, sign, notarize, and package AudioMuse-AI as a macOS DMG.
#
# Prerequisites:
#   - Rust toolchain with `cargo tauri` CLI installed
#   - Apple Developer ID certificate in Keychain
#   - Resources bundled via bundle-services.sh and build-python-env.sh
#   - APP_SIGNING_IDENTITY env var (e.g., "Developer ID Application: Your Name (TEAMID)")
#   - APPLE_ID, APPLE_TEAM_ID, APPLE_APP_PASSWORD env vars for notarization
#
# Usage: ./macos/scripts/build-dmg.sh

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
TAURI_DIR="$SCRIPT_DIR/../src-tauri"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"

# Signing identity (set via environment or use ad-hoc for development)
SIGNING_IDENTITY="${APP_SIGNING_IDENTITY:--}"
NOTARIZE="${NOTARIZE_APP:-false}"

echo "=== AudioMuse-AI: Building macOS Application ==="
echo "Signing identity: ${SIGNING_IDENTITY}"
echo "Notarize: ${NOTARIZE}"

# ── Step 1: Verify resources are bundled ──────────────────────────────

echo ""
echo ">>> Checking bundled resources..."

RESOURCES="$TAURI_DIR/resources"
MISSING=0

for required in python/bin/python3 postgres/bin/postgres redis/bin/redis-server ffmpeg; do
    if [ ! -f "$RESOURCES/$required" ]; then
        echo "ERROR: Missing $RESOURCES/$required"
        echo "  Run ./macos/scripts/build-python-env.sh and ./macos/scripts/bundle-services.sh first"
        MISSING=1
    fi
done

if [ "$MISSING" -eq 1 ]; then
    exit 1
fi

echo "All resources present."

# ── Step 2: Bundle Python source code ─────────────────────────────────

echo ""
echo ">>> Bundling AudioMuse-AI Python source..."

AUDIOMUSE_BUNDLE="$RESOURCES/audiomuse"
rm -rf "$AUDIOMUSE_BUNDLE"
mkdir -p "$AUDIOMUSE_BUNDLE"

# Copy Python source files (exclude non-essential directories)
rsync -a \
    --exclude='macos/' \
    --exclude='docs/' \
    --exclude='screenshot/' \
    --exclude='student_clap/' \
    --exclude='tests/' \
    --exclude='test/' \
    --exclude='deployment/' \
    --exclude='.git/' \
    --exclude='__pycache__/' \
    --exclude='*.pyc' \
    --exclude='.env' \
    --exclude='*.egg-info/' \
    "$PROJECT_ROOT/" "$AUDIOMUSE_BUNDLE/"

echo "Python source bundled."

# ── Step 3: Sign all bundled binaries ─────────────────────────────────

echo ""
echo ">>> Signing bundled binaries..."

sign_binary() {
    local binary="$1"
    if [ "$SIGNING_IDENTITY" = "-" ]; then
        # Ad-hoc signing for development
        codesign --force --sign - "$binary" 2>/dev/null || true
    else
        codesign --force --options runtime --timestamp \
            --sign "$SIGNING_IDENTITY" "$binary" 2>/dev/null || {
            echo "WARNING: Failed to sign $binary"
        }
    fi
}

# Sign all executables and dynamic libraries
find "$RESOURCES" -type f \( -perm +111 -o -name "*.dylib" -o -name "*.so" \) | while read -r binary; do
    sign_binary "$binary"
done

echo "Binary signing complete."

# ── Step 4: Build with Tauri ──────────────────────────────────────────

echo ""
echo ">>> Building Tauri application..."

cd "$TAURI_DIR/.."

# Build for the current architecture (arm64 on Apple Silicon)
# For universal binary, use: --target universal-apple-darwin
if [ "${UNIVERSAL_BUILD:-false}" = "true" ]; then
    echo "Building universal binary (arm64 + x86_64)..."
    cargo tauri build --target universal-apple-darwin
else
    echo "Building native binary..."
    cargo tauri build
fi

# ── Step 5: Notarize (if configured) ─────────────────────────────────

if [ "$NOTARIZE" = "true" ]; then
    echo ""
    echo ">>> Notarizing application..."

    # Find the built DMG
    DMG_PATH=$(find "$TAURI_DIR/target" -name "*.dmg" -newer "$TAURI_DIR/Cargo.toml" | head -1)

    if [ -z "$DMG_PATH" ]; then
        echo "ERROR: Could not find built DMG"
        exit 1
    fi

    echo "Submitting $DMG_PATH for notarization..."

    xcrun notarytool submit "$DMG_PATH" \
        --apple-id "${APPLE_ID}" \
        --team-id "${APPLE_TEAM_ID}" \
        --password "${APPLE_APP_PASSWORD}" \
        --wait

    echo "Stapling notarization ticket..."
    xcrun stapler staple "$DMG_PATH"

    echo "Notarization complete."
    echo "DMG: $DMG_PATH"
else
    echo ""
    echo ">>> Skipping notarization (set NOTARIZE_APP=true to enable)"

    DMG_PATH=$(find "$TAURI_DIR/target" -name "*.dmg" -newer "$TAURI_DIR/Cargo.toml" | head -1)
    if [ -n "$DMG_PATH" ]; then
        echo "DMG: $DMG_PATH"
    fi
fi

# ── Summary ───────────────────────────────────────────────────────────

echo ""
echo "=== Build complete ==="

if [ -n "${DMG_PATH:-}" ]; then
    DMG_SIZE=$(du -sh "$DMG_PATH" | cut -f1)
    echo "Output: $DMG_PATH ($DMG_SIZE)"
fi
