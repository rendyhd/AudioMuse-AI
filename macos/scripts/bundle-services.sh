#!/usr/bin/env bash
# bundle-services.sh
# Download and bundle PostgreSQL 15, Redis 7, and ffmpeg for the macOS app.
# Produces: macos/src-tauri/resources/{postgres,redis,ffmpeg}
#
# Prerequisites:
#   - macOS with Homebrew installed
#   - Apple Silicon (arm64)
#
# Usage: ./macos/scripts/bundle-services.sh

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
RESOURCES_DIR="$SCRIPT_DIR/../src-tauri/resources"

echo "=== AudioMuse-AI: Bundling Service Binaries ==="

mkdir -p "$RESOURCES_DIR"

# ── PostgreSQL 15 ─────────────────────────────────────────────────────

PG_DIR="$RESOURCES_DIR/postgres"
echo ""
echo ">>> Bundling PostgreSQL 15..."

if [ -d "$PG_DIR/bin" ]; then
    echo "PostgreSQL already bundled, skipping"
else
    # Install via Homebrew if not present
    if ! brew list postgresql@15 &>/dev/null; then
        echo "Installing postgresql@15 via Homebrew..."
        brew install postgresql@15
    fi

    PG_PREFIX="$(brew --prefix postgresql@15)"
    mkdir -p "$PG_DIR"

    # Copy essential binaries
    mkdir -p "$PG_DIR/bin"
    for bin in postgres initdb pg_ctl pg_isready psql; do
        cp "$PG_PREFIX/bin/$bin" "$PG_DIR/bin/"
    done

    # Copy shared libraries
    mkdir -p "$PG_DIR/lib"
    cp -R "$PG_PREFIX/lib/postgresql@15/"* "$PG_DIR/lib/" 2>/dev/null || \
        cp -R "$PG_PREFIX/lib/"*.dylib "$PG_DIR/lib/" 2>/dev/null || true

    # Copy extension modules (pg_trgm, unaccent are in contrib, always included)
    if [ -d "$PG_PREFIX/share/postgresql@15/extension" ]; then
        mkdir -p "$PG_DIR/share/extension"
        cp "$PG_PREFIX/share/postgresql@15/extension/"* "$PG_DIR/share/extension/"
    fi

    # Copy timezone and locale data
    if [ -d "$PG_PREFIX/share/postgresql@15" ]; then
        mkdir -p "$PG_DIR/share"
        cp -R "$PG_PREFIX/share/postgresql@15/timezonesets" "$PG_DIR/share/" 2>/dev/null || true
        cp -R "$PG_PREFIX/share/postgresql@15/timezone" "$PG_DIR/share/" 2>/dev/null || true
    fi

    echo "PostgreSQL binaries bundled to $PG_DIR"
fi

# ── Redis 7 ───────────────────────────────────────────────────────────

REDIS_DIR="$RESOURCES_DIR/redis"
echo ""
echo ">>> Bundling Redis 7..."

if [ -d "$REDIS_DIR/bin" ]; then
    echo "Redis already bundled, skipping"
else
    if ! brew list redis &>/dev/null; then
        echo "Installing redis via Homebrew..."
        brew install redis
    fi

    REDIS_PREFIX="$(brew --prefix redis)"
    mkdir -p "$REDIS_DIR/bin"
    cp "$REDIS_PREFIX/bin/redis-server" "$REDIS_DIR/bin/"
    cp "$REDIS_PREFIX/bin/redis-cli" "$REDIS_DIR/bin/"

    echo "Redis binaries bundled to $REDIS_DIR"
fi

# ── FFmpeg ────────────────────────────────────────────────────────────

FFMPEG_PATH="$RESOURCES_DIR/ffmpeg"
echo ""
echo ">>> Bundling ffmpeg..."

if [ -f "$FFMPEG_PATH" ]; then
    echo "ffmpeg already bundled, skipping"
else
    if ! brew list ffmpeg &>/dev/null; then
        echo "Installing ffmpeg via Homebrew..."
        brew install ffmpeg
    fi

    FFMPEG_PREFIX="$(brew --prefix ffmpeg)"
    cp "$FFMPEG_PREFIX/bin/ffmpeg" "$FFMPEG_PATH"

    echo "ffmpeg bundled to $FFMPEG_PATH"
fi

# ── Fix dylib references ─────────────────────────────────────────────

echo ""
echo ">>> Fixing dylib references for relocatability..."

# For each binary, update dylib references to use @executable_path/../lib/
fix_dylib_refs() {
    local binary="$1"
    local lib_dir="$2"

    # Get all non-system dylib dependencies
    otool -L "$binary" 2>/dev/null | tail -n +2 | while read -r line; do
        local dylib
        dylib=$(echo "$line" | awk '{print $1}')

        # Skip system frameworks and libraries
        case "$dylib" in
            /usr/lib/*|/System/*|@rpath/*|@executable_path/*|@loader_path/*)
                continue
                ;;
        esac

        local basename
        basename=$(basename "$dylib")

        # If the dylib exists in our lib dir, update the reference
        if [ -f "$lib_dir/$basename" ]; then
            install_name_tool -change "$dylib" "@executable_path/../lib/$basename" "$binary" 2>/dev/null || true
        else
            # Try to copy the dylib to our lib dir
            if [ -f "$dylib" ]; then
                cp "$dylib" "$lib_dir/" 2>/dev/null || true
                install_name_tool -change "$dylib" "@executable_path/../lib/$basename" "$binary" 2>/dev/null || true
            fi
        fi
    done
}

# Fix PostgreSQL binaries
if [ -d "$PG_DIR/bin" ]; then
    for bin in "$PG_DIR/bin/"*; do
        fix_dylib_refs "$bin" "$PG_DIR/lib"
    done
fi

# ── Summary ───────────────────────────────────────────────────────────

echo ""
echo "=== Service binaries bundled successfully ==="
echo ""
echo "PostgreSQL:"
"$PG_DIR/bin/postgres" --version 2>/dev/null || echo "  (binary present but may need libraries at runtime)"
echo ""
echo "Redis:"
"$REDIS_DIR/bin/redis-server" --version 2>/dev/null || echo "  (binary present)"
echo ""
echo "FFmpeg:"
"$FFMPEG_PATH" -version 2>&1 | head -1 || echo "  (binary present)"
echo ""
TOTAL_SIZE=$(du -sh "$RESOURCES_DIR" | cut -f1)
echo "Total resources size: $TOTAL_SIZE"
