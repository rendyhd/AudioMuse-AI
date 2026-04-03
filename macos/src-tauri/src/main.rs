// AudioMuse-AI macOS Tauri Application
// Entry point: manages lifecycle of all backend services and the WebView window.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod ports;
mod setup;
mod sidecar;

use std::sync::Mutex;
use tauri::{Emitter, Manager};
use tracing::{error, info};

/// Shared application state holding managed child processes.
pub struct AppState {
    pub sidecar: Mutex<Option<sidecar::SidecarManager>>,
}

fn main() {
    // Initialize structured logging
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,audiomuse_ai=debug".into()),
        )
        .init();

    info!("AudioMuse-AI v0.9.3 starting");

    tauri::Builder::default()
        .plugin(tauri_plugin_shell::init())
        .manage(AppState {
            sidecar: Mutex::new(None),
        })
        .setup(|app| {
            let app_handle = app.handle().clone();

            // Resolve data directory: ~/Library/Application Support/AudioMuse-AI/
            let data_dir = dirs::data_dir()
                .expect("Could not resolve Application Support directory")
                .join("AudioMuse-AI");

            info!("Data directory: {}", data_dir.display());

            // Create directory structure on first run
            setup::ensure_directory_structure(&data_dir)?;

            // Check if first-run model download is needed
            let models_dir = data_dir.join("models");
            if !models_dir.join("musicnn_embedding.onnx").exists() {
                info!("First run detected — models not found, triggering download");
                // Emit event to frontend to show download progress
                if let Some(window) = app.get_webview_window("main") {
                    let _ = window.emit("first-run-setup", "models-needed");
                }
                // Model download runs in background; setup.rs handles it
                let data_dir_clone = data_dir.clone();
                let handle_clone = app_handle.clone();
                std::thread::spawn(move || {
                    if let Err(e) = setup::download_models(&data_dir_clone, &handle_clone) {
                        error!("Model download failed: {}", e);
                    }
                });
            }

            // Allocate ports (check for conflicts)
            let ports = ports::allocate_ports()?;
            info!(
                "Allocated ports — Flask: {}, PostgreSQL: {}, Redis: {}",
                ports.flask, ports.postgres, ports.redis
            );

            // Start all backend services
            let mut manager = sidecar::SidecarManager::new(data_dir.clone(), ports.clone());
            match manager.start_all() {
                Ok(()) => info!("All backend services started successfully"),
                Err(e) => {
                    error!("Failed to start backend services: {}", e);
                    return Err(e.into());
                }
            }

            // Store manager in app state for shutdown
            let state = app_handle.state::<AppState>();
            *state.sidecar.lock().unwrap() = Some(manager);

            // Wait for Flask to become ready before loading the WebView
            let flask_url = format!("http://127.0.0.1:{}", ports.flask);
            info!("Waiting for Flask to become ready at {}", flask_url);
            wait_for_flask(&flask_url, 30);

            Ok(())
        })
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { .. } = event {
                info!("Window close requested — shutting down services");
                let state = window.state::<AppState>();
                if let Some(ref mut manager) = *state.sidecar.lock().unwrap() {
                    manager.stop_all();
                };
            }
        })
        .run(tauri::generate_context!())
        .expect("error while running AudioMuse-AI");
}

/// Block until Flask responds on its health endpoint, or timeout.
fn wait_for_flask(base_url: &str, timeout_secs: u64) {
    let start = std::time::Instant::now();
    let timeout = std::time::Duration::from_secs(timeout_secs);
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(2))
        .build()
        .unwrap();

    loop {
        if start.elapsed() > timeout {
            error!(
                "Flask did not become ready within {} seconds",
                timeout_secs
            );
            break;
        }
        match client.get(base_url).send() {
            Ok(resp) if resp.status().is_success() || resp.status().is_redirection() => {
                info!("Flask is ready (responded in {:?})", start.elapsed());
                return;
            }
            _ => {
                std::thread::sleep(std::time::Duration::from_millis(500));
            }
        }
    }
}
