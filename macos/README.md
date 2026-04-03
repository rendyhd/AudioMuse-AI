# AudioMuse-AI — macOS Native App

Native macOS application wrapping AudioMuse-AI in a Tauri v2 shell. Replaces Docker with bundled PostgreSQL, Redis, and Python, all managed as child processes.

## Architecture

- **Tauri v2** WKWebView shell (~3MB) loads the existing Flask web UI
- **Rust sidecar** manages PostgreSQL 15, Redis 7, Flask, and RQ workers as child processes
- **Bundled Python 3.12** via [python-build-standalone](https://github.com/indygreg/python-build-standalone)
- ML inference uses **CPU EP** (Apple Accelerate for BLAS/LAPACK) — CoreML is not viable for these models

## Prerequisites

- macOS 13+ (Ventura or later)
- Apple Silicon (arm64) — Intel build possible but not tested
- [Rust toolchain](https://rustup.rs/) with `cargo install tauri-cli@^2`
- [Homebrew](https://brew.sh/) (for bundling PostgreSQL, Redis, ffmpeg)

## Build Steps

### 1. Bundle service binaries

```bash
./macos/scripts/bundle-services.sh
```

Downloads and bundles PostgreSQL 15, Redis 7, and ffmpeg from Homebrew into `macos/src-tauri/resources/`.

### 2. Build Python environment

```bash
./macos/scripts/build-python-env.sh
```

Downloads python-build-standalone, installs all pip dependencies, fixes dylib references with delocate. Output: `macos/src-tauri/resources/python/`.

### 3. Build the app

```bash
cd macos
cargo tauri build
```

For a signed + notarized DMG:

```bash
APP_SIGNING_IDENTITY="Developer ID Application: ..." \
NOTARIZE_APP=true \
APPLE_ID="you@example.com" \
APPLE_TEAM_ID="XXXXXXXXXX" \
APPLE_APP_PASSWORD="xxxx-xxxx-xxxx-xxxx" \
./macos/scripts/build-dmg.sh
```

### Development mode

```bash
cd macos
cargo tauri dev
```

For development, you can use system Python + Homebrew PostgreSQL/Redis instead of bundled binaries.

## Data Location

All user data lives in `~/Library/Application Support/AudioMuse-AI/`:

```
config.env          # User configuration (media server, API keys)
models/             # ONNX models (~1.5GB, downloaded on first launch)
postgres/data/      # PostgreSQL data directory
redis/              # Redis persistence (AOF + RDB)
temp_audio/         # Temporary audio processing files
logs/               # Service log files
```

## Port Allocation

Default ports: Flask 8000, PostgreSQL 5432, Redis 6379. If any port is occupied, the app automatically selects an alternative.

## Existing Codebase Changes

Only **one line** was modified in the existing codebase:

- `config.py:54` — Changed `TEMP_DIR = "/app/temp_audio"` to `TEMP_DIR = os.environ.get("TEMP_DIR", "/app/temp_audio")`

All other existing Python code works as-is because ONNX provider selection already falls back to CPU, and all config values read from `os.environ`.
