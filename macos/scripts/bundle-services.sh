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

    # Copy ICU libraries (required by PostgreSQL, not always pulled in automatically)
    ICU_PREFIX="$(brew --prefix icu4c@78 2>/dev/null || brew --prefix icu4c 2>/dev/null || true)"
    if [ -n "$ICU_PREFIX" ] && [ -d "$ICU_PREFIX/lib" ]; then
        for lib in libicudata libicuuc libicui18n; do
            for f in "$ICU_PREFIX/lib/${lib}"*.dylib; do
                [ -f "$f" ] && cp "$f" "$PG_DIR/lib/" 2>/dev/null || true
            done
        done
        echo "ICU libraries bundled from $ICU_PREFIX"
    fi

    # Copy all share data (postgres.bki, extensions, timezone, SQL files, etc.)
    if [ -d "$PG_PREFIX/share/postgresql@15" ]; then
        mkdir -p "$PG_DIR/share"
        cp -R "$PG_PREFIX/share/postgresql@15/"* "$PG_DIR/share/"
    fi

    # Copy PostgreSQL server modules ($libdir .so files - dict_snowball, pg_trgm, etc.)
    PG_CELLAR="$(brew --cellar postgresql@15)"
    PG_PKGLIB="$(find "$PG_CELLAR" -type d -name "postgresql" -path "*/lib/postgresql" | head -1)"
    if [ -n "$PG_PKGLIB" ] && [ -d "$PG_PKGLIB" ]; then
        mkdir -p "$PG_DIR/lib/postgresql"
        cp "$PG_PKGLIB/"*.so "$PG_DIR/lib/postgresql/"
        echo "PostgreSQL modules bundled: $(ls "$PG_DIR/lib/postgresql/"*.so | wc -l | tr -d ' ') .so files"
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
echo ">>> Bundling ffmpeg (static build)..."

if [ -f "$FFMPEG_PATH" ]; then
    echo "ffmpeg already bundled, skipping"
else
    if ! brew list ffmpeg &>/dev/null; then
        echo "Installing ffmpeg via Homebrew..."
        brew install ffmpeg
    fi

    FFMPEG_PREFIX="$(brew --prefix ffmpeg)"

    # Bundle ffmpeg into its own directory with all dylib dependencies
    FFMPEG_BUNDLE="$RESOURCES_DIR/ffmpeg"
    mkdir -p "$FFMPEG_BUNDLE/bin" "$FFMPEG_BUNDLE/lib"
    cp "$FFMPEG_PREFIX/bin/ffmpeg" "$FFMPEG_BUNDLE/bin/"

    echo "ffmpeg bundled to $FFMPEG_PATH"
fi

# ── Fix dylib references ─────────────────────────────────────────────

echo ""
echo ">>> Fixing dylib references for relocatability..."

# Recursively copy missing Homebrew dylibs and rewrite all references.
# Runs multiple passes until no new libraries are discovered.
fix_all_dylibs() {
    local lib_dir="$1"
    shift
    local -a bin_dirs=("$@")

    local changed=1
    local pass=0
    while [ "$changed" -eq 1 ]; do
        changed=0
        pass=$((pass + 1))
        echo "  Pass $pass: scanning for Homebrew dylib references..."

        # Scan all binaries and dylibs
        for f in "${bin_dirs[@]}"/* "$lib_dir"/*.dylib; do
            [ -f "$f" ] || continue
            otool -L "$f" 2>/dev/null | tail -n +2 | awk '{print $1}' | while read -r dep; do
                # Skip system and already-fixed references
                case "$dep" in
                    /usr/lib/*|/System/*|@rpath/*|@executable_path/*|@loader_path/*) continue ;;
                esac

                local base
                base=$(basename "$dep")

                # Copy the library if we don't have it yet
                if [ ! -f "$lib_dir/$base" ] && [ -f "$dep" ]; then
                    cp "$dep" "$lib_dir/"
                    chmod u+rw "$lib_dir/$base"
                    echo "    Copied: $base"
                    # Signal another pass is needed to resolve this lib's deps
                    echo "CHANGED" > /tmp/_dylib_changed
                fi
            done
        done

        # Check if new libs were added
        if [ -f /tmp/_dylib_changed ]; then
            rm -f /tmp/_dylib_changed
            changed=1
        fi
    done

    echo "  Rewriting all references to @loader_path / @executable_path..."

    # Now rewrite all references in all files
    for f in "${bin_dirs[@]}"/* "$lib_dir"/*.dylib; do
        [ -f "$f" ] || continue
        local is_bin=false
        for bd in "${bin_dirs[@]}"; do
            case "$f" in "$bd"/*) is_bin=true ;; esac
        done

        otool -L "$f" 2>/dev/null | tail -n +2 | awk '{print $1}' | while read -r dep; do
            case "$dep" in
                /usr/lib/*|/System/*|@rpath/*|@executable_path/*|@loader_path/*) continue ;;
            esac
            local base
            base=$(basename "$dep")
            if [ -f "$lib_dir/$base" ]; then
                if [ "$is_bin" = true ]; then
                    install_name_tool -change "$dep" "@executable_path/../lib/$base" "$f" 2>/dev/null || true
                else
                    install_name_tool -change "$dep" "@loader_path/$base" "$f" 2>/dev/null || true
                fi
            fi
        done

        # Fix the install name of dylibs themselves
        if [ "$is_bin" = false ]; then
            local self_name
            self_name=$(otool -D "$f" 2>/dev/null | tail -1)
            case "$self_name" in
                /usr/lib/*|/System/*|@rpath/*|@executable_path/*|@loader_path/*) ;;
                *)
                    if [ -n "$self_name" ]; then
                        install_name_tool -id "@loader_path/$(basename "$self_name")" "$f" 2>/dev/null || true
                    fi
                    ;;
            esac
        fi
    done
}

# Fix PostgreSQL
if [ -d "$PG_DIR/bin" ]; then
    fix_all_dylibs "$PG_DIR/lib" "$PG_DIR/bin"
fi

# Patch PostgreSQL compiled-in share directory path.
# Homebrew bakes /opt/homebrew/opt/postgresql@15/share/postgresql@15 into binaries.
# On machines without Homebrew this path doesn't exist, causing timezone lookup failures.
# We patch it to /tmp/.audiomuse_pg_share which the app creates as a symlink at runtime.
echo ""
echo ">>> Patching PostgreSQL share directory path in binaries..."
chmod -R u+rw "$RESOURCES_DIR"
PG_PREFIX_SHARE="$(brew --prefix postgresql@15)/share/postgresql@15"
PG_PREFIX_PKGLIB="$(brew --prefix postgresql@15)/lib/postgresql"
if [ -d "$PG_DIR/bin" ] && [ -n "$PG_PREFIX_SHARE" ]; then
    python3 -c "
import os, sys
pg_dir = '$PG_DIR'
patches = [
    (b'$PG_PREFIX_SHARE', b'/tmp/.audiomuse_pg_share'),
    (b'$PG_PREFIX_PKGLIB', b'/tmp/.audiomuse_pg_pkglib'),
]
for old, new in patches:
    if len(new) > len(old):
        print(f'ERROR: new path ({len(new)}) longer than old ({len(old)})', file=sys.stderr)
        sys.exit(1)
for name in ['postgres', 'initdb', 'pg_ctl', 'pg_isready', 'psql']:
    path = os.path.join(pg_dir, 'bin', name)
    if not os.path.exists(path): continue
    with open(path, 'rb') as f: data = f.read()
    total = 0
    for old, new in patches:
        count = data.count(old)
        padded = new + b'\x00' * (len(old) - len(new))
        data = data.replace(old, padded)
        total += count
    with open(path, 'wb') as f: f.write(data)
    print(f'  {name}: patched {total} refs')
"
fi

# Fix Redis
if [ -d "$REDIS_DIR/bin" ]; then
    mkdir -p "$REDIS_DIR/lib"
    fix_all_dylibs "$REDIS_DIR/lib" "$REDIS_DIR/bin"
fi

# Fix ffmpeg
FFMPEG_BUNDLE="$RESOURCES_DIR/ffmpeg"
if [ -d "$FFMPEG_BUNDLE/bin" ]; then
    fix_all_dylibs "$FFMPEG_BUNDLE/lib" "$FFMPEG_BUNDLE/bin"
fi

# ── Re-sign all binaries (install_name_tool invalidates signatures) ──

echo ""
echo ">>> Re-signing all binaries..."
chmod -R u+rw "$RESOURCES_DIR"
find "$RESOURCES_DIR" -type f \( -perm +111 -o -name "*.dylib" -o -name "*.so" \) -exec codesign --force --sign - {} \; 2>/dev/null || true
echo "Re-signing complete."

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
