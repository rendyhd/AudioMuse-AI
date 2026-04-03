// First-run setup: directory creation and ML model downloads.
// Models are downloaded from GitHub releases on first launch,
// matching the URLs used in the Dockerfile.

use sha2::{Digest, Sha256};
use std::fs;
use std::io::Write;
use std::path::Path;
use tauri::AppHandle;
use tracing::{error, info, warn};

/// Required directory structure under ~/Library/Application Support/AudioMuse-AI/
const SUBDIRS: &[&str] = &[
    "models",
    "models/hf_cache",
    "models/mulan",
    "postgres",
    "postgres/data",
    "redis",
    "temp_audio",
    "logs",
];

/// Create all required directories on first run.
pub fn ensure_directory_structure(data_dir: &Path) -> Result<(), String> {
    for subdir in SUBDIRS {
        let path = data_dir.join(subdir);
        if !path.exists() {
            fs::create_dir_all(&path)
                .map_err(|e| format!("Failed to create {}: {}", path.display(), e))?;
            info!("Created directory: {}", path.display());
        }
    }

    // Create default config.env if it doesn't exist
    let config_path = data_dir.join("config.env");
    if !config_path.exists() {
        let default_config = r#"# AudioMuse-AI Configuration
# See documentation for all available options.
#
# Media Server Configuration (uncomment and configure one):
# MEDIASERVER_TYPE=localfiles
# LOCALFILES_MUSIC_DIRECTORY=/path/to/your/music
#
# MEDIASERVER_TYPE=jellyfin
# JELLYFIN_URL=http://your-jellyfin:8096
# JELLYFIN_USER_ID=your-user-id
# JELLYFIN_TOKEN=your-api-token
#
# AI Provider (for Instant Playlist):
# GEMINI_API_KEY=your-key
# OPENAI_API_KEY=your-key
"#;
        fs::write(&config_path, default_config)
            .map_err(|e| format!("Failed to create config.env: {}", e))?;
        info!("Created default config.env");
    }

    Ok(())
}

/// Model definition: URL, filename, optional SHA-256 checksum.
struct ModelFile {
    url: &'static str,
    filename: &'static str,
    subdir: &'static str,
    sha256: Option<&'static str>,
}

/// Core models required for operation (downloaded from GitHub releases).
/// These match the Dockerfile download URLs.
const CORE_MODELS: &[ModelFile] = &[
    ModelFile {
        url: "https://github.com/rendyhd/AudioMuse-AI/releases/download/v4.0.0-model/musicnn_embedding.onnx",
        filename: "musicnn_embedding.onnx",
        subdir: "",
        sha256: None,
    },
    ModelFile {
        url: "https://github.com/rendyhd/AudioMuse-AI/releases/download/v4.0.0-model/musicnn_prediction.onnx",
        filename: "musicnn_prediction.onnx",
        subdir: "",
        sha256: None,
    },
    ModelFile {
        url: "https://github.com/rendyhd/AudioMuse-AI/releases/download/DCLAP.v1/model_epoch_36.onnx",
        filename: "model_epoch_36.onnx",
        subdir: "",
        sha256: None,
    },
    ModelFile {
        url: "https://github.com/rendyhd/AudioMuse-AI/releases/download/DCLAP.v1/model_epoch_36.onnx.data",
        filename: "model_epoch_36.onnx.data",
        subdir: "",
        sha256: None,
    },
    ModelFile {
        url: "https://github.com/rendyhd/AudioMuse-AI/releases/download/v4.0.0-model/clap_text_model.onnx",
        filename: "clap_text_model.onnx",
        subdir: "",
        sha256: None,
    },
];

/// Download all required models to the models directory.
/// Emits progress events to the Tauri frontend.
pub fn download_models(data_dir: &Path, app_handle: &AppHandle) -> Result<(), String> {
    let models_dir = data_dir.join("models");
    let total = CORE_MODELS.len();

    info!("Downloading {} model files", total);

    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(600))
        .build()
        .map_err(|e| format!("Failed to create HTTP client: {}", e))?;

    for (idx, model) in CORE_MODELS.iter().enumerate() {
        let target_dir = if model.subdir.is_empty() {
            models_dir.clone()
        } else {
            models_dir.join(model.subdir)
        };
        let target_path = target_dir.join(model.filename);

        // Skip if already downloaded and checksum matches
        if target_path.exists() {
            if let Some(expected_hash) = model.sha256 {
                if verify_sha256(&target_path, expected_hash) {
                    info!(
                        "[{}/{}] {} already exists and checksum matches, skipping",
                        idx + 1,
                        total,
                        model.filename
                    );
                    continue;
                }
                warn!(
                    "{} exists but checksum mismatch, re-downloading",
                    model.filename
                );
            } else {
                info!(
                    "[{}/{}] {} already exists, skipping",
                    idx + 1,
                    total,
                    model.filename
                );
                continue;
            }
        }

        info!(
            "[{}/{}] Downloading {}...",
            idx + 1,
            total,
            model.filename
        );

        // Emit progress event to frontend
        let progress = serde_json::json!({
            "current": idx + 1,
            "total": total,
            "filename": model.filename,
            "status": "downloading"
        });
        if let Some(window) = app_handle.get_webview_window("main") {
            let _ = window.emit("model-download-progress", &progress);
        }

        // Download with streaming to handle large files
        let response = client
            .get(model.url)
            .send()
            .map_err(|e| format!("Failed to download {}: {}", model.filename, e))?;

        if !response.status().is_success() {
            return Err(format!(
                "HTTP {} downloading {}",
                response.status(),
                model.filename
            ));
        }

        let bytes = response
            .bytes()
            .map_err(|e| format!("Failed to read response for {}: {}", model.filename, e))?;

        // Write to temporary file first, then rename (atomic on same filesystem)
        let tmp_path = target_path.with_extension("download");
        let mut file = fs::File::create(&tmp_path)
            .map_err(|e| format!("Failed to create {}: {}", tmp_path.display(), e))?;
        file.write_all(&bytes)
            .map_err(|e| format!("Failed to write {}: {}", model.filename, e))?;
        file.flush()
            .map_err(|e| format!("Failed to flush {}: {}", model.filename, e))?;

        // Verify checksum if provided
        if let Some(expected_hash) = model.sha256 {
            if !verify_sha256(&tmp_path, expected_hash) {
                let _ = fs::remove_file(&tmp_path);
                return Err(format!(
                    "Checksum mismatch for {} (download corrupted)",
                    model.filename
                ));
            }
        }

        fs::rename(&tmp_path, &target_path)
            .map_err(|e| format!("Failed to rename {}: {}", model.filename, e))?;

        info!(
            "[{}/{}] {} downloaded ({} bytes)",
            idx + 1,
            total,
            model.filename,
            bytes.len()
        );
    }

    // Emit completion event
    let done = serde_json::json!({
        "current": total,
        "total": total,
        "filename": "",
        "status": "complete"
    });
    if let Some(window) = app_handle.get_webview_window("main") {
        let _ = window.emit("model-download-progress", &done);
    }

    info!("All models downloaded successfully");
    Ok(())
}

/// Verify SHA-256 checksum of a file.
fn verify_sha256(path: &Path, expected: &str) -> bool {
    let Ok(bytes) = fs::read(path) else {
        return false;
    };
    let hash = Sha256::digest(&bytes);
    let actual = hex::encode(hash);
    actual == expected
}
