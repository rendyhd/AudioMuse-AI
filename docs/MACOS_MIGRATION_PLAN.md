# AudioMuse-AI: macOS Native App Migration Plan

## Context

AudioMuse-AI is a Dockerized 3-container music analysis platform. The goal is to transform it into a native macOS app that runs without Docker, ships as a signed DMG, and leverages Apple Silicon for ML inference. The app lives in a `macos/` folder within the existing monorepo, sharing the Python backend code.

**Current Docker architecture:**
- **Flask container** (`app.py`): gunicorn on port 8000, 17 blueprints, Jinja2+vanilla JS UI, Voyager HNSW index in memory
- **Worker container** (supervisord): `rq_worker.py` (default queue), `rq_worker_high_priority.py` (high queue), `rq_janitor.py` (registry cleanup)
- **PostgreSQL 15** + **Redis 7**: raw SQL via psycopg2 (no ORM), RQ job queue, pub/sub for index reloads

---

## Architecture Decision: Tauri v2 + Embedded Python

### Why Tauri v2

The UI is already a fully functional web app: 23 Jinja2 templates, vanilla JS, Plotly.js charts. Rewriting in SwiftUI would take months with zero functional benefit. Tauri v2 wraps the Flask UI in a native macOS WKWebView (~3MB shell vs Electron's ~150MB Chromium). Its Rust sidecar system manages all backend processes as children with lifecycle control.

### Service Translation Map

| Docker Service | macOS Equivalent | Managed By |
|---|---|---|
| Flask (gunicorn :8000) | Flask dev server on `127.0.0.1:8000` | Tauri sidecar (child process) |
| RQ Worker high (`rq_worker_high_priority.py`) | Python child process | Tauri sidecar |
| RQ Worker default (`rq_worker.py`) | Python child process | Tauri sidecar |
| RQ Janitor (`rq_janitor.py`) | Python child process | Tauri sidecar |
| PostgreSQL 15 | Bundled `postgres` binary | Tauri auto-start/stop |
| Redis 7 | Bundled `redis-server` binary | Tauri auto-start/stop |
| supervisord | Not needed | Tauri replaces it |

---

## Dependency Mapping

### System Libraries

| Docker (apt) | macOS Equivalent | Method |
|---|---|---|
| `libfftw3` | `fftw` | Homebrew or bundled dylib |
| `libsamplerate0` | `libsamplerate` | Homebrew or bundled dylib |
| `libsndfile1` | `libsndfile` | Homebrew or bundled dylib |
| `libopenblas-dev` | Apple Accelerate framework | Built-in (zero install) |
| `liblapack-dev` | Apple Accelerate framework | Built-in (zero install) |
| `libpq-dev` | `libpq` from `postgresql@15` | Bundled with postgres |
| `ffmpeg` | `ffmpeg` static binary | Bundled in app bundle |
| `supervisor` | N/A | Tauri manages processes |

### Python Environment

**Approach:** Bundled standalone Python 3.12 via [python-build-standalone](https://github.com/indygreg/python-build-standalone) (relocatable, no system Python dependency).

Location: `AudioMuse-AI.app/Contents/Resources/python/`

**Critical native-extension packages:**
- `numpy`, `scipy`, `scikit-learn` - Link against Accelerate automatically on macOS arm64
- `numba` - LLVM JIT, works natively on arm64 (must bundle LLVM)
- `psycopg2-binary` - Needs bundled `libpq` dylib
- `librosa` - Pure Python over numpy/scipy/numba
- `voyager==2.1.0` - C++ extension, may need arm64 compilation from source
- `onnxruntime==1.19.2` - CPU-only (see GPU section below)
- `transformers`, `sentencepiece` - Work natively on arm64

Use `delocate` (Python `auditwheel` equivalent) to fix dylib references for relocatability.

### ML Model Inference - GPU Strategy

**CRITICAL FINDING: CoreML EP is NOT viable for these models.**

The developer already tested CoreML and documented the results in `student_clap/data/clap_embedder.py:45-66` (teacher model; the student model used in production likely has similar issues):
```
CoreML is 2x SLOWER than CPU for this model due to poor operator coverage.
Only 24% of ops supported by CoreML GPU, context switching overhead too high.
Performance: ~325ms/segment (CPU) vs ~713ms/segment (CoreML)
```

**Strategy: Optimized CPU inference on Apple Silicon.**
- Apple's Accelerate framework (vecLib/BLAS/LAPACK) is used automatically by numpy/scipy/ONNX Runtime on arm64
- ONNX Runtime CPU EP with Apple's optimized math libraries already gets near-native performance on M-chips
- The existing `onnxruntime==1.19.2` CPU build works correctly on macOS arm64
- No provider selection code changes needed - CPU EP is the default fallback everywhere

**Files that do ONNX provider selection (8 production locations):**
1. `tasks/clap_analyzer.py:92-103` - `_load_audio_model()` - CUDA check, falls back to CPU
2. `tasks/clap_analyzer.py:192-203` - `_load_text_model()` - CUDA check, falls back to CPU
3. `tasks/mulan_analyzer.py:70-71` - `_load_mulan_models()` audio encoder - CUDA check, falls back to CPU
4. `tasks/mulan_analyzer.py:176-177` - `initialize_mulan_text_models()` - CUDA check, falls back to CPU
5. `tasks/analysis.py:384-397` - MusiCNN models in `analyze_track` - CUDA check, falls back to CPU
6. `tasks/analysis.py:814-825` - MusiCNN lazy loading in `analyze_album_task` - CUDA check, falls back to CPU
7. `tasks/analysis.py:862-873` - MusiCNN session recycling in `analyze_album_task` - CUDA check, falls back to CPU
8. `student_clap/data/clap_embedder.py:57-58` - Teacher model (training toolkit only, not production)

**All 8 locations already fall back to `CPUExecutionProvider` when CUDA is absent.** No code changes needed for macOS - the existing logic works as-is.

**`tasks/memory_utils.py`:** CUDA cleanup (`torch.cuda.empty_cache()`, `cupy` pool cleanup) is wrapped in `try/except ImportError` - already gracefully handles non-CUDA systems. No changes needed.

### ML Models (~1.5GB)

Downloaded on first launch, cached permanently:

| Model | Size | Source |
|---|---|---|
| `musicnn_embedding.onnx` | ~2MB | GitHub releases v4.0.0-model |
| `musicnn_prediction.onnx` | ~3MB | GitHub releases v4.0.0-model |
| `model_epoch_36.onnx` + `.data` | ~21MB | GitHub releases DCLAP v1 |
| `clap_text_model.onnx` | ~478MB | GitHub releases v4.0.0-model |
| HuggingFace tokenizers (RoBERTa) | ~985MB | GitHub releases v4.0.0-model |
| MuLan models (optional) | ~2.5GB | GitHub releases v3.0.0-model |

### PostgreSQL - Why Embedded PG is Required (NOT SQLite)

**Verified: 15+ PostgreSQL-specific features used throughout the codebase:**

| PG Feature | Usage Location | Why SQLite Can't Replace |
|---|---|---|
| `CREATE EXTENSION pg_trgm` | `app_helper.py:113` | GIN trigram indexes for fuzzy search |
| `CREATE EXTENSION unaccent` | `app_helper.py:112` | Accent-stripping for multilingual search |
| JSONB columns + operators (`->>`, `\|\|`) | `app_helper.py:125`, `app_setup.py:599` | Provider config, app settings |
| BYTEA columns | `app_helper.py:255-273` | Embeddings, serialized HNSW indexes |
| Advisory locks (`pg_advisory_lock`) | `app_helper.py:108,483` | Cross-process DDL serialization |
| PL/pgSQL stored functions | `app_helper.py:218-237` | `immutable_unaccent()`, `score_search_u_sync()` |
| Triggers | `app_helper.py:233-237` | Auto-maintain `search_u` column |
| GIN indexes | `app_helper.py:242` | `score_search_u_trgm` for text search |
| `ON CONFLICT ... DO UPDATE SET` with `EXCLUDED` | 13+ locations | Upsert pattern everywhere |
| `RETURNING id` | `app_helper.py:1844`, `app_setup.py:337` | Get inserted IDs |
| `ILIKE` | 6+ files (map, chat, MCP server, voyager) | Case-insensitive matching |
| `array_agg()`, `unnest()`, `string_to_array()` | `app_setup.py`, `app_chat.py`, `mcp_server.py` | Array operations |
| `REGEXP_REPLACE()` | `tasks/chat_manager.py:115` | Fuzzy matching |
| `information_schema` introspection | `app_helper.py` (14 locations) | Runtime schema migrations |
| `DictCursor`, `execute_values()` | Throughout (psycopg2-specific) | Dict-based row access, batch inserts |

**Solution:** Bundle `postgresql@15` binary with `pg_trgm` and `unaccent` extensions (both are part of PostgreSQL's contrib modules, always available).

### Redis - Verified Sufficient with Bundled Server

**Redis usage is lightweight (4 purposes):**

1. **RQ Job Queue** - `rq_queue_high` and `rq_queue_default` (standard list operations)
2. **Pub/Sub** - Channel `index-updates` for Voyager index reload signaling between worker->Flask (`app.py:680-745`, published from `tasks/analysis.py`, `tasks/cleaning.py`, `tasks/collection_manager.py`)
3. **Data Caching** - CLAP text embeddings cache (~50-100KB compressed NPZ blob, key: `clap:other_features_embeddings` in `tasks/clap_analyzer.py:733,766`)
4. **Distributed Locks** - Album batch locks via `SET NX EX` + Lua script for atomic release (`tasks/collection_manager.py:22-28,87,266`)

**No advanced Redis features used** (no sorted sets, streams, transactions, clustering). Total memory: ~5-50MB during operation. A bundled `redis-server` with `appendonly yes` for persistence is fully sufficient.

---

## Data & Storage Layout

```
~/Library/Application Support/AudioMuse-AI/
  config.env                    # User configuration (replaces Docker .env)
  models/                       # ONNX models (downloaded on first launch)
    musicnn_embedding.onnx
    musicnn_prediction.onnx
    model_epoch_36.onnx
    model_epoch_36.onnx.data
    clap_text_model.onnx
    hf_cache/                   # HuggingFace RoBERTa tokenizer
    mulan/                      # Optional MuLan models
  postgres/
    data/                       # PG_DATA directory
  redis/
    dump.rdb                    # Redis persistence
    appendonly.aof              # Append-only file
  temp_audio/                   # Temporary audio processing
  logs/                         # Application logs
    flask.log
    worker_default.log
    worker_high.log
    janitor.log
```

### Config Migration

`config.py` reads most values from `os.environ`, with one exception requiring a fix.

**Required change in `config.py:54`:** `TEMP_DIR` is hardcoded as `/app/temp_audio` and does NOT read from `os.environ`. This must be changed to:
```python
TEMP_DIR = os.environ.get("TEMP_DIR", "/app/temp_audio")
```
This is the **only modification needed** in the existing codebase. Without it, the macOS app will fail with permission errors trying to write to `/app/temp_audio`.

**macOS config flow:**
1. New `macos/config_macos.py` reads `config.env` from Application Support
2. Sets all env vars before the Python process imports `config.py`
3. Overrides: `TEMP_DIR` -> `~/Library/Application Support/AudioMuse-AI/temp_audio`, model paths -> Application Support, `POSTGRES_HOST=127.0.0.1`, `REDIS_URL=redis://127.0.0.1:6379/0`

---

## Networking

| Docker | macOS |
|---|---|
| `redis://redis:6379/0` | `redis://127.0.0.1:6379/0` |
| `POSTGRES_HOST=postgres` | `POSTGRES_HOST=127.0.0.1` |
| Port 8000 exposed to Docker host | `127.0.0.1:8000` (localhost only) |
| Docker internal network | All on localhost |
| Redis Pub/Sub `index-updates` | Same, on localhost Redis |

No reverse proxy or TLS. Tauri WebView loads `http://127.0.0.1:8000`.

**Port conflict handling:** On startup, check if ports 8000/5432/6379 are in use. If so, pick alternative ports and update config accordingly.

---

## New Files (in `macos/` directory)

```
macos/
  src-tauri/
    Cargo.toml                  # Rust deps (tauri v2, serde, reqwest)
    tauri.conf.json             # Window config, app metadata, permissions
    src/
      main.rs                   # App entry, window creation, lifecycle
      sidecar.rs                # Start/stop postgres, redis, python processes
      setup.rs                  # First-run model download with progress UI
      ports.rs                  # Port availability check and allocation
    icons/                      # App icons (.icns)
  scripts/
    build-python-env.sh         # Build relocatable Python 3.12 + all deps
    bundle-services.sh          # Bundle postgres, redis, ffmpeg binaries
    build-dmg.sh                # Final DMG packaging + signing
  config_macos.py               # Env var loader for macOS paths
  README.md                     # macOS-specific build instructions
```

**Files to modify in existing codebase: ONE.**
- `config.py:54` - Change `TEMP_DIR = "/app/temp_audio"` to `TEMP_DIR = os.environ.get("TEMP_DIR", "/app/temp_audio")` (hardcoded path, does not read from env)

All other existing Python code works as-is because:
- ONNX provider selection already falls back to CPU when CUDA absent (all 8 locations)
- `memory_utils.py` already handles missing torch/cupy gracefully (`try/except ImportError`)
- All other `config.py` values read from `os.environ` (set by `config_macos.py` before import)
- Workers detect `AUDIOMUSE_ROLE=worker` from env (set by Tauri sidecar, same as `rq_worker.py:13`)

---

## Build & Distribution

- **Build:** `cargo tauri build --target universal-apple-darwin` (arm64 + x86_64)
- **Signing:** Apple Developer ID certificate (not Mac App Store - bundled binaries violate sandboxing rules)
- **Notarization:** `xcrun notarytool` (automated in CI). Every bundled binary (postgres, redis, ffmpeg, python) must be individually signed before bundling.
- **Distribution:** DMG with drag-to-Applications. Models downloaded on first launch (keeps DMG ~500MB vs ~2GB).
- **Auto-updates:** Sparkle framework checking GitHub releases.

---

## Migration Phases

### Phase 1: Tauri Shell + Flask WebView
**Complexity: LOW | Effort: 1x**

Create `macos/src-tauri/` scaffold. Write `main.rs` that starts Flask as a child process on `127.0.0.1:8000` and opens a WebView. Test with system Python + manual pip install.

**Validate:** App window opens, all 23 pages navigate correctly.
**Risk:** Low - well-documented Tauri pattern.

### Phase 2: Embedded PostgreSQL + Redis
**Complexity: MEDIUM | Effort: 2x**

Write `sidecar.rs`: `start_postgres()` (run `initdb` if needed, start `postgres`), `start_redis()` (with AOF persistence), `stop_all()` (SIGTERM, wait, SIGKILL). Bundle PG 15 + Redis 7 binaries from Homebrew bottles. Create Application Support directory structure on first run. Add port conflict detection (`ports.rs`).

**Validate:** `psql` connects, `redis-cli ping` works, Flask `init_db()` creates all 15 tables with extensions (`pg_trgm`, `unaccent`).
**Risk:** Medium - PG data dir initialization needs care. Must ensure `pg_trgm` and `unaccent` extensions are available in bundled PG (they're in contrib, always included).

### Phase 3: Bundled Python Environment
**Complexity: HIGH | Effort: 4x** (riskiest phase)

Write `scripts/build-python-env.sh`: download python-build-standalone for arm64, create venv, pip install all `requirements/common.txt`, bundle ffmpeg, fix dylib paths with `delocate`. Update `main.rs` to use bundled Python.

**Validate:** `bundled-python -c "import flask, redis, psycopg2, librosa, onnxruntime, voyager, sklearn, numba, transformers"` succeeds.
**Risk:** HIGH - Native extensions need correct dylib linking. `voyager` may need source compilation. `numba` requires bundled LLVM. **Mitigation:** Test each import individually. Fall back to conda-forge arm64 packages if pip fails. Consider PyInstaller as alternative bundling approach.

### Phase 4: Worker Process Management
**Complexity: MEDIUM | Effort: 2x**

Extend `sidecar.rs` to manage 4 Python child processes (Flask + 3 workers). Startup order: PostgreSQL -> Redis -> Workers -> Flask. Shutdown: reverse. Auto-restart crashed workers (like supervisord `autorestart=true`). Pipe stdout/stderr to log files. Set `AUDIOMUSE_ROLE=worker` env var for worker processes (same as `rq_worker.py:13` does today).

**Validate:** Enqueue analysis job from UI, worker picks it up, completes, pub/sub triggers index reload in Flask.
**Risk:** Medium - process management is straightforward but needs robust SIGTERM/SIGKILL handling.

### Phase 5: First-Run Setup & Model Download
**Complexity: LOW | Effort: 1x**

Write `setup.rs`: detect first run (no models directory), show native progress dialog, download ~1.5GB of models from GitHub releases (same URLs as Dockerfile lines 37-62, 223-306), verify checksums. Integrate with existing setup wizard (`app_setup.py`) for media server config.

**Validate:** Delete Application Support folder, relaunch, models download, app starts, setup wizard works.
**Risk:** Low.

### Phase 6: DMG Packaging & Code Signing
**Complexity: MEDIUM | Effort: 2x**

Write `scripts/build-dmg.sh`: `cargo tauri build`, sign each binary individually (postgres, redis, ffmpeg, python, all .dylibs), notarize with `xcrun notarytool`, create DMG. Set up GitHub Actions CI for automated builds.

**Validate:** Install DMG on clean Mac, Gatekeeper accepts, app launches through full workflow.
**Risk:** Medium - signing bundled third-party binaries requires careful entitlements.

---

## Verification Plan

1. **Unit tests:** `pytest tests/` passes with bundled Python (set env vars via `config_macos.py`)
2. **Integration:** Configure LocalFiles provider -> test music folder -> analyze 5 songs -> verify embeddings in DB -> similarity search -> clustering
3. **Memory:** Analyze 50 songs, monitor RSS stays under 4GB (`PER_SONG_MODEL_RELOAD=true` controls model unloading)
4. **Clean install:** Fresh Mac, install DMG, complete setup wizard, full workflow
5. **Upgrade:** Install v1, create data, install v2 over it, verify data preserved in PG
6. **Port conflicts:** Start with PG already running on 5432, verify app detects and picks alternative port
