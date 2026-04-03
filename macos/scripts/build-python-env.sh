#!/usr/bin/env bash
# build-python-env.sh
# Build a relocatable Python 3.12 environment with all AudioMuse-AI dependencies.
# Produces: macos/src-tauri/resources/python/ (ready to bundle into the .app)
#
# Prerequisites:
#   - macOS with Apple Silicon (arm64)
#   - Homebrew installed (for delocate, build tools)
#   - Internet access (downloads python-build-standalone + pip packages)
#
# Usage: ./macos/scripts/build-python-env.sh

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
RESOURCES_DIR="$SCRIPT_DIR/../src-tauri/resources"
PYTHON_DIR="$RESOURCES_DIR/python"

# python-build-standalone release for macOS arm64
PYTHON_VERSION="3.12.8"
PBS_RELEASE="20250106"
PBS_URL="https://github.com/indygreg/python-build-standalone/releases/download/${PBS_RELEASE}/cpython-${PYTHON_VERSION}+${PBS_RELEASE}-aarch64-apple-darwin-install_only.tar.gz"
PBS_ARCHIVE="/tmp/python-standalone-${PYTHON_VERSION}.tar.gz"

echo "=== AudioMuse-AI: Building Relocatable Python Environment ==="
echo "Python version: ${PYTHON_VERSION}"
echo "Target: ${PYTHON_DIR}"

# ── Step 1: Download python-build-standalone ──────────────────────────

if [ ! -f "$PBS_ARCHIVE" ]; then
    echo ""
    echo ">>> Downloading python-build-standalone..."
    curl -L -o "$PBS_ARCHIVE" "$PBS_URL"
else
    echo ">>> Using cached python-build-standalone archive"
fi

# ── Step 2: Extract to resources directory ────────────────────────────

echo ""
echo ">>> Extracting Python to ${PYTHON_DIR}..."
rm -rf "$PYTHON_DIR"
mkdir -p "$PYTHON_DIR"
tar -xzf "$PBS_ARCHIVE" -C "$RESOURCES_DIR"
# python-build-standalone extracts to a "python/" directory
if [ ! -f "$PYTHON_DIR/bin/python3" ]; then
    echo "ERROR: Expected python3 binary not found after extraction"
    exit 1
fi

echo ">>> Python extracted: $("$PYTHON_DIR/bin/python3" --version)"

# ── Step 3: Install pip packages ──────────────────────────────────────

echo ""
echo ">>> Installing AudioMuse-AI dependencies..."

PYTHON="$PYTHON_DIR/bin/python3"
PIP="$PYTHON -m pip"

# Upgrade pip first
$PIP install --upgrade pip setuptools wheel

# Install from requirements/common.txt (shared between CPU and GPU)
$PIP install -r "$PROJECT_ROOT/requirements/common.txt"

# Install CPU-specific ONNX Runtime (not GPU)
if [ -f "$PROJECT_ROOT/requirements/cpu.txt" ]; then
    $PIP install -r "$PROJECT_ROOT/requirements/cpu.txt"
else
    $PIP install onnxruntime==1.19.2
fi

echo ""
echo ">>> Verifying critical imports..."

$PYTHON -c "
imports = [
    'flask', 'redis', 'psycopg2', 'librosa', 'onnxruntime',
    'voyager', 'sklearn', 'numba', 'transformers', 'sentencepiece',
    'numpy', 'scipy', 'rq', 'rapidfuzz', 'mutagen', 'umap',
]
failed = []
for mod in imports:
    try:
        __import__(mod)
        print(f'  OK: {mod}')
    except ImportError as e:
        print(f'  FAIL: {mod} — {e}')
        failed.append(mod)
if failed:
    print(f'\nERROR: {len(failed)} imports failed: {failed}')
    exit(1)
print('\nAll critical imports verified successfully.')
"

# ── Step 4: Fix dylib references with delocate ───────────────────────

echo ""
echo ">>> Installing delocate for dylib fixup..."
$PIP install delocate

echo ">>> Running delocate on installed packages..."
SITE_PACKAGES="$($PYTHON -c 'import site; print(site.getsitepackages()[0])')"

# delocate-path fixes all .dylib and .so files in the given directory tree
$PYTHON -m delocate.cmd.delocate_path "$SITE_PACKAGES" || {
    echo "WARNING: delocate reported issues (non-fatal, some system libs may remain as @rpath)"
}

# ── Step 5: Strip unnecessary files to reduce size ────────────────────

echo ""
echo ">>> Stripping unnecessary files to reduce bundle size..."

# Remove test directories
find "$PYTHON_DIR" -type d -name "test" -o -name "tests" -o -name "__pycache__" | \
    while read dir; do rm -rf "$dir"; done

# Remove .pyc files (they'll be regenerated)
find "$PYTHON_DIR" -name "*.pyc" -delete

# Remove pip/setuptools caches
rm -rf "$PYTHON_DIR/lib/python3.12/ensurepip"

# Calculate final size
PYTHON_SIZE=$(du -sh "$PYTHON_DIR" | cut -f1)
echo ""
echo "=== Python environment built successfully ==="
echo "Location: ${PYTHON_DIR}"
echo "Size: ${PYTHON_SIZE}"
echo "Python: $("$PYTHON" --version)"
