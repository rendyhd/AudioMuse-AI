// Sidecar process manager for AudioMuse-AI.
// Manages PostgreSQL, Redis, Flask, and RQ worker child processes.
// Replaces Docker Compose + supervisord from the containerized deployment.

use crate::ports::AllocatedPorts;
use std::collections::HashMap;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::Duration;
use tracing::{error, info, warn};

/// Managed child process with metadata for restart logic.
struct ManagedProcess {
    child: Child,
    name: String,
    restart_count: u32,
}

impl ManagedProcess {
    fn new(child: Child, name: &str) -> Self {
        Self {
            child,
            name: name.to_string(),
            restart_count: 0,
        }
    }
}

/// Manages all backend service processes.
pub struct SidecarManager {
    data_dir: PathBuf,
    ports: AllocatedPorts,
    postgres_running: bool,
    redis: Option<ManagedProcess>,
    flask: Option<ManagedProcess>,
    worker_default: Option<ManagedProcess>,
    worker_high: Option<ManagedProcess>,
    janitor: Option<ManagedProcess>,
}

impl SidecarManager {
    pub fn new(data_dir: PathBuf, ports: AllocatedPorts) -> Self {
        Self {
            data_dir,
            ports,
            postgres_running: false,
            redis: None,
            flask: None,
            worker_default: None,
            worker_high: None,
            janitor: None,
        }
    }

    /// Resolve path to a bundled binary inside the app bundle's Resources.
    fn resource_bin(&self, name: &str) -> PathBuf {
        // In development, look relative to the Cargo project
        // In production, look inside the .app bundle Resources
        let dev_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("resources")
            .join(name);
        if dev_path.exists() {
            return dev_path;
        }
        // Production: inside .app/Contents/Resources/
        if let Ok(exe) = std::env::current_exe() {
            if let Some(resources) = exe.parent().and_then(|p| p.parent()).map(|p| p.join("Resources")) {
                let prod_path = resources.join(name);
                if prod_path.exists() {
                    return prod_path;
                }
            }
        }
        // Fallback: assume it's on PATH (development with Homebrew)
        PathBuf::from(name)
    }

    /// Resolve path to the bundled Python interpreter.
    fn python_bin(&self) -> PathBuf {
        self.resource_bin("python/bin/python3")
    }

    /// Path to the AudioMuse-AI Python source (the monorepo root).
    fn python_src(&self) -> PathBuf {
        // In development, the source is at ../../ relative to macos/src-tauri/
        let dev_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(|p| p.parent())
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| PathBuf::from("."));
        if dev_path.join("app.py").exists() {
            return dev_path;
        }
        // Production: bundled alongside the app
        self.resource_bin("audiomuse")
    }

    /// Build the base environment variables for all Python processes.
    fn base_env(&self) -> HashMap<String, String> {
        let mut env = HashMap::new();

        // PostgreSQL connection
        env.insert("POSTGRES_HOST".into(), "127.0.0.1".into());
        env.insert("POSTGRES_PORT".into(), self.ports.postgres.to_string());
        env.insert("POSTGRES_DB".into(), "audiomuse".into());
        env.insert("POSTGRES_USER".into(), "audiomuse".into());
        env.insert("POSTGRES_PASSWORD".into(), "audiomuse".into());

        // Redis connection
        env.insert(
            "REDIS_URL".into(),
            format!("redis://127.0.0.1:{}/0", self.ports.redis),
        );

        // Paths
        let temp_dir = self.data_dir.join("temp_audio");
        env.insert("TEMP_DIR".into(), temp_dir.to_string_lossy().into());
        let models_dir = self.data_dir.join("models");
        env.insert("MODEL_DIR".into(), models_dir.to_string_lossy().into());

        // Set individual model paths (defaults are /app/model/ which doesn't exist on macOS)
        env.insert("EMBEDDING_MODEL_PATH".into(), models_dir.join("musicnn_embedding.onnx").to_string_lossy().into());
        env.insert("PREDICTION_MODEL_PATH".into(), models_dir.join("musicnn_prediction.onnx").to_string_lossy().into());
        env.insert("CLAP_AUDIO_MODEL_PATH".into(), models_dir.join("model_epoch_36.onnx").to_string_lossy().into());
        env.insert("CLAP_TEXT_MODEL_PATH".into(), models_dir.join("clap_text_model.onnx").to_string_lossy().into());
        env.insert("MULAN_MODEL_DIR".into(), models_dir.join("mulan").to_string_lossy().into());
        env.insert(
            "HF_HOME".into(),
            self.data_dir
                .join("models")
                .join("hf_cache")
                .to_string_lossy()
                .into(),
        );

        // Flask
        env.insert("FLASK_HOST".into(), "127.0.0.1".into());
        env.insert("FLASK_PORT".into(), self.ports.flask.to_string());

        // macOS: prevent ObjC fork() crash in RQ worker child processes
        env.insert("OBJC_DISABLE_INITIALIZE_FORK_SAFETY".into(), "YES".into());

        // Add bundled ffmpeg to PATH so pydub can find it
        let ffmpeg_bin = self.resource_bin("ffmpeg/bin");
        if ffmpeg_bin.exists() {
            let current_path = std::env::var("PATH").unwrap_or_default();
            env.insert("PATH".into(), format!("{}:{}", ffmpeg_bin.to_string_lossy(), current_path));
            // Set DYLD_LIBRARY_PATH so ffmpeg can find its bundled dylibs
            let ffmpeg_lib = self.resource_bin("ffmpeg/lib");
            if ffmpeg_lib.exists() {
                env.insert("DYLD_LIBRARY_PATH".into(), ffmpeg_lib.to_string_lossy().into());
            }
        }

        // macOS: don't auto-create a default localfiles provider — let the setup wizard handle it
        env.entry("MEDIASERVER_TYPE".to_string()).or_insert_with(|| "".to_string());

        // Read user config.env if it exists
        let config_env_path = self.data_dir.join("config.env");
        if config_env_path.exists() {
            if let Ok(contents) = std::fs::read_to_string(&config_env_path) {
                for line in contents.lines() {
                    let line = line.trim();
                    if line.is_empty() || line.starts_with('#') {
                        continue;
                    }
                    if let Some((key, value)) = line.split_once('=') {
                        let key = key.trim();
                        let value = value.trim().trim_matches('"').trim_matches('\'');
                        // Don't override our managed service connection settings
                        if !matches!(
                            key,
                            "POSTGRES_HOST"
                                | "POSTGRES_PORT"
                                | "REDIS_URL"
                                | "TEMP_DIR"
                                | "FLASK_HOST"
                                | "FLASK_PORT"
                        ) {
                            env.insert(key.to_string(), value.to_string());
                        }
                    }
                }
            }
        }

        env
    }

    // ── PostgreSQL ───────────────────────────────────────────────────────

    /// Create symlink so PostgreSQL can find its share directory.
    /// The bundled postgres binaries have /tmp/.audiomuse_pg_share baked in
    /// as the share directory path (patched from the Homebrew default).
    fn ensure_pg_share_symlink(&self) {
        let pg_share = self.resource_bin("postgres/share");
        let symlink_path = std::path::PathBuf::from("/tmp/.audiomuse_pg_share");
        // Remove stale symlink and recreate
        let _ = std::fs::remove_file(&symlink_path);
        if pg_share.exists() {
            let _ = std::os::unix::fs::symlink(&pg_share, &symlink_path);
        }
    }

    /// Initialize PostgreSQL data directory if it doesn't exist.
    fn init_postgres(&self) -> Result<(), String> {
        let pg_data = self.data_dir.join("postgres").join("data");
        if pg_data.join("PG_VERSION").exists() {
            info!("PostgreSQL data directory already initialized");
            return Ok(());
        }

        info!("Initializing PostgreSQL data directory at {}", pg_data.display());
        std::fs::create_dir_all(&pg_data).map_err(|e| format!("Failed to create PG data dir: {}", e))?;

        let initdb = self.resource_bin("postgres/bin/initdb");
        let pg_share = self.resource_bin("postgres/share");
        let output = Command::new(&initdb)
            .args([
                "-D",
                &pg_data.to_string_lossy(),
                "-U",
                "audiomuse",
                "--encoding=UTF8",
                "--locale=C",
                "-L",
                &pg_share.to_string_lossy(),
            ])
            .env("LC_ALL", "C")
            .env("LC_CTYPE", "C")
            .env("TZ", "UTC")
            .env("PGSHAREDIR", pg_share.as_os_str())
            .output()
            .map_err(|e| format!("Failed to run initdb: {}", e))?;

        if !output.status.success() {
            return Err(format!(
                "initdb failed: {}",
                String::from_utf8_lossy(&output.stderr)
            ));
        }

        info!("PostgreSQL initialized successfully");
        Ok(())
    }

    /// Start the PostgreSQL server using pg_ctl (avoids "multithreaded" error).
    fn start_postgres(&mut self) -> Result<(), String> {
        self.ensure_pg_share_symlink();
        self.init_postgres()?;

        let pg_data = self.data_dir.join("postgres").join("data");
        let log_file = self.data_dir.join("logs").join("postgres.log");
        let pg_ctl = self.resource_bin("postgres/bin/pg_ctl");

        info!("Starting PostgreSQL on port {}", self.ports.postgres);

        let pg_options = format!(
            "-p {} -k '{}' -c logging_collector=on -c \"log_directory={}\" -c log_filename=postgres.log -c listen_addresses=127.0.0.1",
            self.ports.postgres,
            self.data_dir.join("postgres").to_string_lossy(),
            self.data_dir.join("logs").to_string_lossy(),
        );

        let pg_share = self.resource_bin("postgres/share");
        let output = Command::new(&pg_ctl)
            .args([
                "start",
                "-D",
                &pg_data.to_string_lossy(),
                "-l",
                &log_file.to_string_lossy(),
                "-o",
                &pg_options,
                "-w",
            ])
            .env("LC_ALL", "C")
            .env("LC_CTYPE", "C")
            .env("TZ", "UTC")
            .env("PGSHAREDIR", pg_share.as_os_str())
            .output()
            .map_err(|e| format!("Failed to start PostgreSQL via pg_ctl: {}", e))?;

        if !output.status.success() {
            return Err(format!(
                "pg_ctl start failed: {}",
                String::from_utf8_lossy(&output.stderr)
            ));
        }

        self.postgres_running = true;
        info!("PostgreSQL started via pg_ctl");

        // Wait for PostgreSQL to accept connections
        self.wait_for_postgres(15)?;

        // Create the database if it doesn't exist
        self.ensure_database()?;

        Ok(())
    }

    /// Wait for PostgreSQL to become ready.
    fn wait_for_postgres(&self, timeout_secs: u64) -> Result<(), String> {
        let pg_isready = self.resource_bin("postgres/bin/pg_isready");
        let start = std::time::Instant::now();
        let timeout = Duration::from_secs(timeout_secs);

        loop {
            if start.elapsed() > timeout {
                return Err("PostgreSQL did not become ready in time".into());
            }

            let status = Command::new(&pg_isready)
                .args([
                    "-h",
                    "127.0.0.1",
                    "-p",
                    &self.ports.postgres.to_string(),
                    "-U",
                    "audiomuse",
                ])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();

            if let Ok(s) = status {
                if s.success() {
                    info!("PostgreSQL is ready");
                    return Ok(());
                }
            }

            std::thread::sleep(Duration::from_millis(500));
        }
    }

    /// Create the `audiomuse` database if it doesn't exist.
    fn ensure_database(&self) -> Result<(), String> {
        let psql = self.resource_bin("postgres/bin/psql");

        // Check if database exists
        let output = Command::new(&psql)
            .args([
                "-h",
                "127.0.0.1",
                "-p",
                &self.ports.postgres.to_string(),
                "-U",
                "audiomuse",
                "-d",
                "postgres",
                "-tAc",
                "SELECT 1 FROM pg_database WHERE datname='audiomuse';",
            ])
            .output()
            .map_err(|e| format!("Failed to check database: {}", e))?;

        let result = String::from_utf8_lossy(&output.stdout);
        if result.trim() != "1" {
            info!("Creating 'audiomuse' database");
            let create_output = Command::new(&psql)
                .args([
                    "-h",
                    "127.0.0.1",
                    "-p",
                    &self.ports.postgres.to_string(),
                    "-U",
                    "audiomuse",
                    "-d",
                    "postgres",
                    "-c",
                    "CREATE DATABASE audiomuse;",
                ])
                .output()
                .map_err(|e| format!("Failed to create database: {}", e))?;

            if !create_output.status.success() {
                return Err(format!(
                    "Failed to create database: {}",
                    String::from_utf8_lossy(&create_output.stderr)
                ));
            }
        }

        Ok(())
    }

    // ── Redis ────────────────────────────────────────────────────────────

    /// Start the Redis server with AOF persistence.
    fn start_redis(&mut self) -> Result<(), String> {
        let redis_dir = self.data_dir.join("redis");
        std::fs::create_dir_all(&redis_dir)
            .map_err(|e| format!("Failed to create Redis dir: {}", e))?;

        let redis_bin = self.resource_bin("redis/bin/redis-server");
        let log_file = self.data_dir.join("logs").join("redis.log");

        info!("Starting Redis on port {}", self.ports.redis);

        let child = Command::new(&redis_bin)
            .args([
                "--port",
                &self.ports.redis.to_string(),
                "--bind",
                "127.0.0.1",
                "--dir",
                &redis_dir.to_string_lossy(),
                "--appendonly",
                "yes",
                "--appendfilename",
                "appendonly.aof",
                "--dbfilename",
                "dump.rdb",
                "--logfile",
                &log_file.to_string_lossy(),
                "--daemonize",
                "no",
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| format!("Failed to start Redis: {}", e))?;

        self.redis = Some(ManagedProcess::new(child, "Redis"));

        // Wait briefly for Redis to start
        std::thread::sleep(Duration::from_millis(500));
        info!("Redis started");
        Ok(())
    }

    // ── Python Processes (Flask + Workers) ───────────────────────────────

    /// Start a Python process with the base environment.
    fn start_python_process(
        &self,
        script: &str,
        name: &str,
        extra_env: &[(&str, &str)],
    ) -> Result<ManagedProcess, String> {
        let python = self.python_bin();
        let src = self.python_src();
        let script_path = src.join(script);
        let log_file = self.data_dir.join("logs").join(format!("{}.log", name));

        info!("Starting {} ({})", name, script);

        let mut env = self.base_env();
        env.insert("PYTHONPATH".into(), src.to_string_lossy().into());
        for (key, value) in extra_env {
            env.insert(key.to_string(), value.to_string());
        }

        let log_out = std::fs::File::create(&log_file)
            .map(Stdio::from)
            .unwrap_or(Stdio::null());
        let log_err = std::fs::File::create(
            self.data_dir
                .join("logs")
                .join(format!("{}_err.log", name)),
        )
        .map(Stdio::from)
        .unwrap_or(Stdio::null());

        let child = Command::new(&python)
            .arg(&script_path)
            .envs(&env)
            .current_dir(&src)
            .stdout(log_out)
            .stderr(log_err)
            .spawn()
            .map_err(|e| format!("Failed to start {}: {}", name, e))?;

        Ok(ManagedProcess::new(child, name))
    }

    /// Start Flask development server.
    fn start_flask(&mut self) -> Result<(), String> {
        let process = self.start_python_process(
            "app.py",
            "flask",
            &[
                ("FLASK_RUN_HOST", "127.0.0.1"),
                ("FLASK_RUN_PORT", &self.ports.flask.to_string()),
            ],
        )?;
        self.flask = Some(process);
        Ok(())
    }

    /// Start the default-queue RQ worker.
    fn start_worker_default(&mut self) -> Result<(), String> {
        let process = self.start_python_process(
            "rq_worker.py",
            "worker_default",
            &[("AUDIOMUSE_ROLE", "worker")],
        )?;
        self.worker_default = Some(process);
        Ok(())
    }

    /// Start the high-priority RQ worker.
    fn start_worker_high(&mut self) -> Result<(), String> {
        let process = self.start_python_process(
            "rq_worker_high_priority.py",
            "worker_high",
            &[("AUDIOMUSE_ROLE", "worker")],
        )?;
        self.worker_high = Some(process);
        Ok(())
    }

    /// Start the RQ janitor process.
    fn start_janitor(&mut self) -> Result<(), String> {
        let process = self.start_python_process("rq_janitor.py", "janitor", &[])?;
        self.janitor = Some(process);
        Ok(())
    }

    // ── Lifecycle ────────────────────────────────────────────────────────

    /// Start all services in the correct order:
    /// PostgreSQL -> Redis -> Workers -> Flask
    pub fn start_all(&mut self) -> Result<(), String> {
        info!("Starting all backend services");

        self.start_postgres()?;
        self.start_redis()?;

        // Start workers before Flask so they're ready for jobs
        self.start_worker_default()?;
        self.start_worker_high()?;
        self.start_janitor()?;

        // Flask last — it will init the DB tables on startup
        self.start_flask()?;

        info!("All services started");
        Ok(())
    }

    /// Gracefully stop all services in reverse order.
    /// Sends SIGTERM first, waits, then SIGKILL if needed.
    pub fn stop_all(&mut self) {
        info!("Stopping all backend services");

        // Stop in reverse startup order
        stop_process(&mut self.flask, "Flask");
        stop_process(&mut self.janitor, "Janitor");
        stop_process(&mut self.worker_high, "Worker (high)");
        stop_process(&mut self.worker_default, "Worker (default)");
        stop_process(&mut self.redis, "Redis");
        self.stop_postgres();

        info!("All services stopped");
    }

    /// Stop PostgreSQL using pg_ctl for clean shutdown.
    fn stop_postgres(&mut self) {
        if self.postgres_running {
            info!("Stopping PostgreSQL");
            let pg_ctl = self.resource_bin("postgres/bin/pg_ctl");
            let pg_data = self.data_dir.join("postgres").join("data");

            let result = Command::new(&pg_ctl)
                .args([
                    "stop",
                    "-D",
                    &pg_data.to_string_lossy(),
                    "-m",
                    "fast",
                    "-w",
                ])
                .output();

            match result {
                Ok(output) if output.status.success() => {
                    info!("PostgreSQL stopped cleanly via pg_ctl");
                }
                _ => {
                    warn!("pg_ctl stop failed");
                }
            }

            self.postgres_running = false;
        }
    }

    /// Check worker health and restart crashed processes.
    /// Call this periodically (e.g., every 10 seconds from a Tauri timer).
    pub fn health_check(&mut self) {
        self.maybe_restart_field("worker_default", "rq_worker.py");
        self.maybe_restart_field("worker_high", "rq_worker_high_priority.py");
        self.maybe_restart_field("janitor", "rq_janitor.py");
    }

    /// Restart a process field if it has exited unexpectedly.
    fn maybe_restart_field(&mut self, name: &str, script: &str) {
        let field = match name {
            "worker_default" => &mut self.worker_default,
            "worker_high" => &mut self.worker_high,
            "janitor" => &mut self.janitor,
            _ => return,
        };

        let should_restart = if let Some(ref mut proc) = field {
            match proc.child.try_wait() {
                Ok(Some(status)) => {
                    if proc.restart_count < 10 {
                        warn!(
                            "{} exited with status {:?}, restarting (attempt {})",
                            name,
                            status,
                            proc.restart_count + 1
                        );
                        Some(proc.restart_count + 1)
                    } else {
                        error!("{} exceeded max restarts (10), giving up", name);
                        None
                    }
                }
                Ok(None) => None, // Still running
                Err(e) => {
                    error!("Error checking {} status: {}", name, e);
                    None
                }
            }
        } else {
            None
        };

        if let Some(restart_count) = should_restart {
            let extra_env: Vec<(&str, &str)> = if name.starts_with("worker") {
                vec![("AUDIOMUSE_ROLE", "worker")]
            } else {
                vec![]
            };
            match self.start_python_process(script, name, &extra_env) {
                Ok(mut new_proc) => {
                    new_proc.restart_count = restart_count;
                    let field = match name {
                        "worker_default" => &mut self.worker_default,
                        "worker_high" => &mut self.worker_high,
                        "janitor" => &mut self.janitor,
                        _ => return,
                    };
                    *field = Some(new_proc);
                }
                Err(e) => error!("Failed to restart {}: {}", name, e),
            }
        }
    }
}

/// Stop a single managed process gracefully (free function to avoid borrow issues).
fn stop_process(process: &mut Option<ManagedProcess>, label: &str) {
    if let Some(ref mut proc) = process {
        info!("Stopping {}", label);

        // Send SIGTERM via nix
        let pid = nix::unistd::Pid::from_raw(proc.child.id() as i32);
        let _ = nix::sys::signal::kill(pid, nix::sys::signal::Signal::SIGTERM);

        // Wait up to 5 seconds for graceful shutdown
        let start = std::time::Instant::now();
        loop {
            match proc.child.try_wait() {
                Ok(Some(_)) => {
                    info!("{} stopped gracefully", label);
                    *process = None;
                    return;
                }
                Ok(None) => {
                    if start.elapsed() > Duration::from_secs(5) {
                        warn!("{} did not stop in time, sending SIGKILL", label);
                        let _ = proc.child.kill();
                        let _ = proc.child.wait();
                        *process = None;
                        return;
                    }
                    std::thread::sleep(Duration::from_millis(200));
                }
                Err(e) => {
                    error!("Error waiting for {}: {}", label, e);
                    return;
                }
            }
        }
    }
}

impl Drop for SidecarManager {
    fn drop(&mut self) {
        self.stop_all();
    }
}
