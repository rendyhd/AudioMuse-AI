"""
macOS configuration loader for AudioMuse-AI.

This module sets environment variables before config.py is imported,
translating Docker-style paths to macOS Application Support paths.
All services run on localhost instead of Docker internal hostnames.

Usage (from Tauri sidecar or direct invocation):
    import config_macos  # Sets env vars
    import config         # Now reads the correct macOS values
"""

import os
import sys
from pathlib import Path


def get_data_dir() -> Path:
    """Return the AudioMuse-AI data directory.

    Uses AUDIOMUSE_DATA_DIR env var if set (for testing/custom paths),
    otherwise defaults to ~/Library/Application Support/AudioMuse-AI/.
    """
    custom = os.environ.get("AUDIOMUSE_DATA_DIR")
    if custom:
        return Path(custom)
    return Path.home() / "Library" / "Application Support" / "AudioMuse-AI"


def setup_environment():
    """Set all environment variables for macOS operation.

    Must be called before importing config.py or any Flask/worker modules.
    Does NOT override variables already set in the environment (e.g., by Tauri).
    """
    data_dir = get_data_dir()

    # Ensure directory structure exists
    for subdir in [
        "models", "models/hf_cache", "models/mulan",
        "postgres", "postgres/data",
        "redis",
        "temp_audio",
        "logs",
    ]:
        (data_dir / subdir).mkdir(parents=True, exist_ok=True)

    # Mapping of env var -> macOS value (only set if not already defined)
    defaults = {
        # PostgreSQL (localhost, not Docker hostname)
        "POSTGRES_HOST": "127.0.0.1",
        "POSTGRES_PORT": "5432",
        "POSTGRES_DB": "audiomuse",
        "POSTGRES_USER": "audiomuse",
        "POSTGRES_PASSWORD": "audiomuse",

        # Redis (localhost, not Docker hostname)
        "REDIS_URL": "redis://127.0.0.1:6379/0",

        # Paths (Application Support, not /app/)
        "TEMP_DIR": str(data_dir / "temp_audio"),
        "MODEL_DIR": str(data_dir / "models"),
        "HF_HOME": str(data_dir / "models" / "hf_cache"),

        # Flask
        "FLASK_HOST": "127.0.0.1",
        "FLASK_PORT": "8000",
    }

    for key, value in defaults.items():
        if key not in os.environ:
            os.environ[key] = value

    # Load user config.env (additional media server settings, API keys, etc.)
    config_env = data_dir / "config.env"
    if config_env.exists():
        _load_env_file(config_env)

    # Add the project source to PYTHONPATH so imports work
    project_root = os.environ.get("PYTHONPATH")
    if not project_root:
        # Assume this file is at macos/config_macos.py, project root is parent
        project_root = str(Path(__file__).resolve().parent.parent)
        os.environ["PYTHONPATH"] = project_root
        if project_root not in sys.path:
            sys.path.insert(0, project_root)


def _load_env_file(path: Path):
    """Parse a simple .env file (KEY=VALUE, supports # comments and quotes)."""
    # Protected keys that are managed by Tauri/sidecar
    protected = {
        "POSTGRES_HOST", "POSTGRES_PORT", "REDIS_URL",
        "TEMP_DIR", "FLASK_HOST", "FLASK_PORT",
    }

    with open(path) as f:
        for line in f:
            line = line.strip()
            if not line or line.startswith("#"):
                continue
            if "=" not in line:
                continue
            key, _, value = line.partition("=")
            key = key.strip()
            value = value.strip().strip("\"'")

            # Don't override managed service settings
            if key in protected:
                continue

            # Only set if not already in environment
            if key not in os.environ:
                os.environ[key] = value


# Auto-setup when imported
setup_environment()
